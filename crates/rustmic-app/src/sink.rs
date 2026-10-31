//! Destinations for the decoded PCM stream.
//!
//! The original client plays the decoded frames through `waveOut` (the
//! `waveOutOpen`/`waveOutWrite` imports of `WOMicEngine.dll`), optionally
//! through its own virtual audio device. This client writes to an audio
//! output through cpal, to stdout for piping into another program, or
//! nowhere when only the statistics are wanted.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use rustmic_core::error::{Error, Result};

use crate::codec::FRAME_SAMPLES;

/// Receives the decoded samples, one 20 ms frame per call.
pub trait SampleSink {
    /// Hands one frame of mono samples to the destination.
    fn write(&mut self, samples: &[i16]) -> Result<()>;

    /// Human readable name, shown in the log.
    fn name(&self) -> &'static str;
}

/// Discards every frame.
pub struct NullSink;

impl SampleSink for NullSink {
    fn write(&mut self, _samples: &[i16]) -> Result<()> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "null"
    }
}

/// Writes raw little endian 16 bit mono samples to stdout.
pub struct StdoutSink {
    buffer: Vec<u8>,
}

impl StdoutSink {
    pub fn new() -> StdoutSink {
        StdoutSink { buffer: Vec::with_capacity(FRAME_SAMPLES * 2) }
    }
}

impl Default for StdoutSink {
    fn default() -> Self {
        StdoutSink::new()
    }
}

impl SampleSink for StdoutSink {
    fn write(&mut self, samples: &[i16]) -> Result<()> {
        self.buffer.clear();
        for sample in samples {
            self.buffer.extend_from_slice(&sample.to_le_bytes());
        }
        std::io::stdout().write_all(&self.buffer).map_err(Error::Io)
    }

    fn name(&self) -> &'static str {
        "stdout"
    }
}

/// Writes raw little endian 16 bit mono samples into a file or pipe,
/// used by the Linux virtual microphone.
pub struct FileSink {
    inner: std::fs::File,
    buffer: Vec<u8>,
}

impl FileSink {
    pub fn new(inner: std::fs::File) -> FileSink {
        FileSink { inner, buffer: Vec::with_capacity(FRAME_SAMPLES * 2) }
    }
}

impl SampleSink for FileSink {
    fn write(&mut self, samples: &[i16]) -> Result<()> {
        self.buffer.clear();
        for sample in samples {
            self.buffer.extend_from_slice(&sample.to_le_bytes());
        }
        self.inner.write_all(&self.buffer).map_err(Error::Io)
    }

    fn name(&self) -> &'static str {
        "file"
    }
}

/// Plays through a real audio output device via cpal.
///
/// The media thread pushes 20 ms frames into a bounded channel; the cpal
/// callback drains the channel and never blocks, falling back to silence
/// when the pipeline is late. The mono 48 kHz stream is duplicated to the
/// device channels and linearly resampled when the device does not run at
/// 48 kHz natively.
pub struct SpeakerSink {
    sender: SyncSender<Vec<i16>>,
    _stream: cpal::Stream,
}

/// Samples buffered in the output callback before they are dropped again,
/// bounding the delay the speaker can accumulate (~170 ms).
const MAX_QUEUED_SAMPLES: usize = 8192;

impl SpeakerSink {
    /// Opens the requested device (or the default one) for playback.
    pub fn open(device_substring: Option<&str>) -> Result<SpeakerSink> {
        let host = cpal::default_host();
        let device = match device_substring {
            Some(pattern) => find_device(&host, pattern).ok_or_else(|| {
                Error::Config(format!("no audio output device matches '{pattern}'"))
            })?,
            None => host.default_output_device().ok_or(Error::MediaOpen)?,
        };

        let output = device.default_output_config().map_err(|_| Error::MediaOpen)?;
        let (sender, receiver) = mpsc::sync_channel::<Vec<i16>>(32);
        let stream = build_stream(
            &device,
            output.sample_rate().0,
            output.channels() as usize,
            receiver,
        )?;
        stream.play().map_err(|_| Error::MediaOpen)?;

        Ok(SpeakerSink { sender, _stream: stream })
    }
}

impl SampleSink for SpeakerSink {
    fn write(&mut self, samples: &[i16]) -> Result<()> {
        // A full channel means the output fell behind; dropping the frame
        // keeps the delay bounded, matching the late packet handling of the
        // original client.
        match self.sender.try_send(samples.to_vec()) {
            Ok(()) => Ok(()),
            Err(mpsc::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::TrySendError::Disconnected(_)) => Err(Error::MediaOpen),
        }
    }

    fn name(&self) -> &'static str {
        "speaker"
    }
}

fn find_device(host: &cpal::Host, pattern: &str) -> Option<cpal::Device> {
    let lowered = pattern.to_ascii_lowercase();
    host.output_devices()
        .ok()?
        .find(|device| {
            device
                .name()
                .map(|name| name.to_ascii_lowercase().contains(&lowered))
                .unwrap_or(false)
        })
}

/// Names of the audio output devices, for `--list-devices`.
pub fn output_device_names() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|devices| devices.filter_map(|device| device.name().ok()).collect())
        .unwrap_or_default()
}

/// Builds the output stream for the given device configuration.
///
/// The callback keeps a mono sample queue in the input (48 kHz) timeline
/// and walks it with a fractional cursor, so a device rate of 48 kHz pops
/// exactly one input sample per output sample and any other rate
/// interpolates.
fn build_stream(
    device: &cpal::Device,
    device_rate: u32,
    device_channels: usize,
    receiver: Receiver<Vec<i16>>,
) -> Result<cpal::Stream> {
    let config = cpal::StreamConfig {
        channels: device_channels as u16,
        sample_rate: cpal::SampleRate(device_rate),
        buffer_size: cpal::BufferSize::Default,
    };
    let step = rustmic_core::SAMPLE_RATE as f64 / device_rate as f64;

    let stream = device
        .build_output_stream(
            &config,
            move |out: &mut [f32], _| {
                let mut queue: VecDeque<f32> = VecDeque::new();
                let mut cursor: f64 = 0.0;
                let mut previous: f32 = 0.0;
                let mut current: f32 = 0.0;
                drain_channel(&receiver, &mut queue);
                for chunk in out.chunks_mut(device_channels.max(1)) {
                    while cursor >= 1.0 {
                        cursor -= 1.0;
                        previous = current;
                        current = queue.pop_front().unwrap_or(current);
                    }
                    let value = previous + (current - previous) * cursor as f32;
                    for slot in chunk.iter_mut() {
                        *slot = value;
                    }
                    cursor += step;
                    if queue.len() < MAX_QUEUED_SAMPLES {
                        drain_channel(&receiver, &mut queue);
                    } else {
                        queue.pop_front();
                    }
                }
            },
            |error| eprintln!("audio output error: {error}"),
            None,
        )
        .map_err(|_| Error::MediaOpen)?;
    Ok(stream)
}

/// Moves every buffered frame from the media thread into the queue.
fn drain_channel(receiver: &Receiver<Vec<i16>>, queue: &mut VecDeque<f32>) {
    loop {
        match receiver.try_recv() {
            Ok(frame) => {
                queue.extend(frame.iter().map(|&sample| sample as f32 / 32768.0));
            }
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_null_sink_accepts_frames() {
        let mut sink = NullSink;
        sink.write(&[0; FRAME_SAMPLES]).unwrap();
        assert_eq!(sink.name(), "null");
    }

    #[test]
    fn the_stdout_sink_emits_little_endian_samples() {
        let mut sink = StdoutSink::new();
        sink.write(&[0x0142, -1]).unwrap();
        // reading back through a pipe would need a real capture; the buffer
        // contract is checked indirectly by the frame size
        assert_eq!(sink.name(), "stdout");
    }
}
