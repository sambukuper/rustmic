//! Opus decoding of the media payload.
//!
//! The original client carries a statically linked libopus behind its
//! `OpusVoiceDecoder` class and feeds it one 20 ms packet at a time
//! (`FUN_004042d0` in `WOMicEngine.dll`); the decoder is created with the
//! sample rate and channel count that the start routine stores on the
//! controller (48000 Hz, one channel, `FUN_00403ea0` offsets `+0x48` and
//! `+0x4c`). The wrapper below keeps the same shape: one frame in, one
//! 1920 byte PCM frame out.

use rustmic_core::error::{Error, Result};

/// One decoded 20 ms frame, 48 kHz mono signed 16 bit.
pub const FRAME_SAMPLES: usize = rustmic_core::FRAME_SAMPLES;

/// Decodes Opus packets into PCM frames.
pub struct VoiceDecoder {
    decoder: opus::Decoder,
}

impl VoiceDecoder {
    /// Creates the decoder with the format of the WO Mic server stream.
    pub fn new() -> Result<VoiceDecoder> {
        let decoder = opus::Decoder::new(rustmic_core::SAMPLE_RATE, opus::Channels::Mono)
            .map_err(|_| Error::MediaOpen)?;
        Ok(VoiceDecoder { decoder })
    }

    /// Decodes one packet into `out`, returning the number of samples.
    ///
    /// An empty payload asks the decoder to conceal a lost packet, which
    /// keeps the timeline running across gaps in the media channel.
    pub fn decode(&mut self, payload: &[u8], out: &mut [i16]) -> Result<usize> {
        self.decoder
            .decode(payload, out, false)
            .map_err(|e| Error::Protocol(format!("Opus decode failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoder() -> opus::Encoder {
        opus::Encoder::new(rustmic_core::SAMPLE_RATE, opus::Channels::Mono, opus::Application::Voip)
            .unwrap()
    }

    #[test]
    fn a_silence_packet_round_trips_through_the_frame_size() {
        let mut encoder = encoder();
        let mut packet = vec![0u8; 1024];
        let written = encoder
            .encode(&[0i16; FRAME_SAMPLES], &mut packet)
            .unwrap();
        packet.truncate(written);

        let mut decoder = VoiceDecoder::new().unwrap();
        let mut out = [0i16; FRAME_SAMPLES];
        let samples = decoder.decode(&packet, &mut out).unwrap();
        assert_eq!(samples, FRAME_SAMPLES);
        assert!(out.iter().all(|&s| s.abs() < 32));
    }

    #[test]
    fn an_empty_packet_conceals_the_loss() {
        let mut decoder = VoiceDecoder::new().unwrap();
        let mut out = [0i16; FRAME_SAMPLES];
        let samples = decoder.decode(&[], &mut out).unwrap();
        assert_eq!(samples, FRAME_SAMPLES);
    }

    #[test]
    fn garbage_is_a_protocol_error() {
        let mut decoder = VoiceDecoder::new().unwrap();
        let mut out = [0i16; FRAME_SAMPLES];
        assert!(decoder.decode(&[0xFF, 0xFF, 0xFF, 0xFF], &mut out).is_err());
    }
}
