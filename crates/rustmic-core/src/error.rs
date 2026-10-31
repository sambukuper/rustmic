//! Error taxonomy of the client.
//!
//! The numeric codes mirror the ones the original client reports in its
//! message boxes. They were recovered from the master start routine
//! (`FUN_00403ea0` in `WOMicEngine.dll`) and from the media reader loops
//! (`FUN_00404580` for Wi-Fi, `FUN_004042d0` for USB/Bluetooth).

use std::fmt;
use std::io;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum Error {
    /// Underlying I/O failure.
    Io(io::Error),
    /// The peer sent something that does not match the recovered protocol.
    Protocol(String),
    /// The server refused the connection or the transport is unusable.
    Negotiation,
    /// Client and server versions do not match (original code -7).
    Mismatch,
    /// The media stream could not be started (original code -8).
    MediaStart,
    /// The media stream could not be opened (original code -12).
    MediaOpen,
    /// No audio arrived on the media channel (original code -21).
    NoAudioData,
    /// The stream was aborted and may be retried (original code -20).
    Aborted,
    /// The requested Bluetooth device could not be found (original code -5).
    DeviceNotFound(String),
    /// The transport is not implemented for the current platform.
    Unsupported(String),
    /// Invalid configuration.
    Config(String),
}

impl Error {
    /// Numeric code as reported by the original Windows client.
    pub fn code(&self) -> i32 {
        match self {
            Error::Io(_) => -1,
            Error::Protocol(_) => -4,
            Error::DeviceNotFound(_) => -5,
            Error::Negotiation => -6,
            Error::Mismatch => -7,
            Error::MediaStart => -8,
            Error::MediaOpen => -12,
            Error::Aborted => -20,
            Error::NoAudioData => -21,
            Error::Unsupported(_) => -9,
            Error::Config(_) => -2,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "I/O error: {e}"),
            Error::Protocol(m) => write!(f, "protocol error: {m}"),
            Error::Negotiation => write!(f, "the server refused the connection"),
            Error::Mismatch => write!(f, "client and server versions do not match"),
            Error::MediaStart => write!(f, "the media stream could not be started"),
            Error::MediaOpen => write!(f, "the media stream could not be opened"),
            Error::NoAudioData => write!(f, "no audio data received"),
            Error::Aborted => write!(f, "the stream was aborted"),
            Error::DeviceNotFound(d) => write!(f, "device not found: {d}"),
            Error::Unsupported(m) => write!(f, "unsupported: {m}"),
            Error::Config(m) => write!(f, "configuration error: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        // A closed socket is the normal way a WO Mic server terminates a
        // session; map it onto the "aborted" code so the front-end can offer
        // an automatic reconnect just like the original client did.
        match e.kind() {
            io::ErrorKind::UnexpectedEof => Error::Aborted,
            io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe => Error::Aborted,
            io::ErrorKind::TimedOut => Error::NoAudioData,
            _ => Error::Io(e),
        }
    }
}
