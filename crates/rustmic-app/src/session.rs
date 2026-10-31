//! The connection pipeline: handshake, media pump and reconnects.
//!
//! The sequence mirrors the master start routine of the original client
//! (`FUN_00403ea0` in `WOMicEngine.dll`): open the transport, run the
//! connect command, run the version check, then start the stream and pump
//! frames from the media channel through the jitter buffer and the Opus
//! decoder into the output sink. A broken stream is an "aborted" code,
//! which the original client followed with a reconnect; this client does
//! the same while `reconnect` is enabled.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustmic_core::error::{Error, Result};
use rustmic_core::jitter::JitterBuffer;
use rustmic_core::media::Framing;
use rustmic_core::protocol::ControlClient;
use rustmic_core::settings::TransportType;
use rustmic_core::stats::Statistics;

use crate::codec::{VoiceDecoder, FRAME_SAMPLES};
use crate::sink::SampleSink;

/// Everything needed to reach the server.
#[derive(Debug, Clone)]
pub struct ConnectionPlan {
    /// Transport to dial with.
    pub transport: TransportType,
    /// Server address for the Wi-Fi transports.
    pub host: String,
    /// TCP port of the control channel.
    pub control_port: u16,
    /// UDP port of the media channel.
    pub media_port: u16,
    /// Bluetooth address of the server.
    pub bt_address: String,
    /// Explicit RFCOMM channel; scanned when unset.
    pub bt_channel: Option<u8>,
    /// Path of the `adb` executable for the USB transport.
    pub adb_path: Option<String>,
}

/// Tunables of the pipeline.
#[derive(Debug, Clone)]
pub struct PipelineOptions {
    /// Number of packets kept in the jitter buffer (`bufferSize`).
    pub buffer_size: usize,
    /// Reconnect after a broken stream, like the original client.
    pub reconnect: bool,
    /// Pause between reconnect attempts.
    pub reconnect_delay: Duration,
    /// Set to true from the outside to stop the run.
    pub stop: Arc<AtomicBool>,
}

/// Header overhead of the largest media framing, for the byte counter.
const FRAMING_OVERHEAD: usize = 12;

/// Runs the client until the stream ends or `stop` is set.
///
/// Returns the error of the last connection when reconnects are disabled
/// or the error is not retryable; otherwise loops until the stop flag is
/// set, which reports a clean end.
pub fn run(
    plan: &ConnectionPlan,
    options: &PipelineOptions,
    sink: &mut dyn SampleSink,
    stats: &Statistics,
) -> Result<()> {
    loop {
        if options.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        match stream(plan, options, sink, stats) {
            Err(Error::Aborted | Error::NoAudioData) if options.reconnect => {
                stats.on_reconnect();
                eprintln!(
                    "the stream ended ({}), reconnecting in {}s",
                    Error::Aborted,
                    options.reconnect_delay.as_secs().max(1)
                );
                if !sleep_interruptible(&options.stop, options.reconnect_delay) {
                    return Ok(());
                }
            }
            other => return other,
        }
    }
}

/// One connection: handshake, media pump, clean end.
fn stream(
    plan: &ConnectionPlan,
    options: &PipelineOptions,
    sink: &mut dyn SampleSink,
    stats: &Statistics,
) -> Result<()> {
    let mut session = rustmic_transport::open(
        plan.transport,
        &plan.host,
        plan.control_port,
        plan.media_port,
        &plan.bt_address,
        plan.bt_channel,
        plan.adb_path.as_deref(),
    )?;

    let mut control = ControlClient::new(&mut *session.control);
    let reply = control.connect()?;
    let framing = Framing::from_id(reply.framing)?;
    control.check_version(plan.transport)?;
    control.start()?;

    pump(&mut session, framing, options, sink, stats)
}

/// Pumps media frames of an already negotiated session into the sink.
///
/// Public so the integration tests can drive an opened session against a
/// mock server.
pub fn pump(
    session: &mut rustmic_transport::Session,
    framing: Framing,
    options: &PipelineOptions,
    sink: &mut dyn SampleSink,
    stats: &Statistics,
) -> Result<()> {
    let mut jitter = JitterBuffer::new(options.buffer_size);
    let mut decoder = VoiceDecoder::new()?;
    let mut out = [0i16; FRAME_SAMPLES];
    let mut push_times: VecDeque<Instant> = VecDeque::new();
    let mut jitter_seen = jitter.stats();

    while !options.stop.load(Ordering::Relaxed) {
        let frame = match session.media.read_frame(framing) {
            Ok(frame) => frame,
            Err(Error::Io(error))
                if matches!(error.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) =>
            {
                return Err(Error::NoAudioData);
            }
            Err(error) => return Err(error),
        };

        let now = Instant::now();
        stats.on_packet(frame.payload.len() + FRAMING_OVERHEAD, Duration::ZERO);
        if !jitter.push(frame) {
            // The jitter buffer counted the duplicate or late packet.
            continue;
        }
        push_times.push_back(now);

        while let Some(next) = jitter.pop() {
            let waited = push_times.pop_front().map(|t| t.elapsed()).unwrap_or_default();
            stats.on_latency(waited);
            match decoder.decode(&next.payload, &mut out) {
                Ok(samples) => sink.write(&out[..samples])?,
                Err(_) => {
                    // Keep the timeline running: conceal the lost packet
                    // and count the failure, like the original decoder
                    // pipeline does for late or broken packets.
                    stats.on_decode_error();
                    if decoder.decode(&[], &mut out).unwrap_or(0) > 0 {
                        sink.write(&out)?;
                    }
                }
            }
            stats.on_played();
        }

        let current = jitter.stats();
        stats.on_dropped(current.dropped - jitter_seen.dropped);
        stats.on_lost(current.lost - jitter_seen.lost);
        jitter_seen = current;
    }

    Ok(())
}

/// Waits for the delay; false as soon as the stop flag was set.
fn sleep_interruptible(stop: &AtomicBool, delay: Duration) -> bool {
    let deadline = Instant::now() + delay;
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !stop.load(Ordering::Relaxed)
}
