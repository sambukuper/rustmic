//! Library surface of the WO Mic client, shared by the `rustmic` binary and
//! the integration tests.

pub mod codec;
pub mod session;
pub mod sink;

#[cfg(target_os = "linux")]
pub mod virtual_mic;
