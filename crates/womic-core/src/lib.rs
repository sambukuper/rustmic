//! Core of the WO Mic client, re-implemented from the behaviour of the
//! original Windows client (`WOMicClient.exe` + `WOMicEngine.dll`).
//!
//! The crate is deliberately free of third-party dependencies so that it can
//! be reused from any front-end and unit-tested without a network stack.
//!
//! Every protocol detail implemented here was recovered from the shipped
//! binary; see `docs/ANALYSIS.md` in the repository root for the evidence
//! (function addresses, disassembly excerpts) behind each constant.

pub mod error;
pub mod jitter;
pub mod media;
pub mod protocol;
pub mod settings;
pub mod stats;

pub use error::{Error, Result};
pub use jitter::JitterBuffer;
pub use media::{Framing, MediaFrame};
pub use protocol::{ControlClient, Reply};
pub use settings::{Settings, TransportType};
pub use stats::Statistics;

/// Sample rate used by the WO Mic server for the Opus stream.
pub const SAMPLE_RATE: u32 = 48_000;
/// Number of channels of the WO Mic server stream (mono).
pub const CHANNELS: u32 = 1;
/// Duration of one Opus frame in milliseconds.
pub const FRAME_DURATION_MS: u32 = 20;
/// Number of PCM samples in one 20 ms Opus frame at 48 kHz.
pub const FRAME_SAMPLES: usize = (SAMPLE_RATE as usize / 1000) * FRAME_DURATION_MS as usize;
/// Size of the decoded PCM frame in bytes (mono, signed 16 bit).
pub const FRAME_BYTES: usize = FRAME_SAMPLES * 2;
