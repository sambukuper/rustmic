//! Transports that carry the WO Mic control and media channels.
//!
//! The original client supports four transports. They were recovered from
//! the transport factory `FUN_004048a0` in `WOMicEngine.dll`:
//!
//! | id | transport | control channel | media channel |
//! |----|-----------|-----------------|---------------|
//! | 1  | Bluetooth | RFCOMM (shared) | RFCOMM (shared) |
//! | 2  | USB       | TCP via `adb` (shared) | TCP via `adb` (shared) |
//! | 3  | Wi-Fi     | TCP             | UDP           |
//! | 4  | Wi-Fi Direct | TCP          | UDP           |
//!
//! For Bluetooth and USB one connection carries both channels; the media
//! reader reads frames from the same byte stream the control commands run
//! on (`FUN_00404730`). For the Wi-Fi transports the control channel is a
//! TCP stream and the media channel arrives as UDP datagrams. The framing
//! of the media channel differs per transport and is announced by the
//! server, see [`womic_core::media`].

pub mod adb;
pub mod bt;
pub mod tcp;
pub mod udp;

use womic_core::error::Result;
use womic_core::media::{Framing, MediaFrame};

pub use adb::AdbTransport;
pub use bt::{BluetoothTransport, BtSocket};
pub use tcp::TcpTransport;
pub use udp::UdpTransport;

/// The bidirectional control channel.
pub trait ControlChannel: std::io::Read + std::io::Write + Send {}

impl<T: std::io::Read + std::io::Write + Send> ControlChannel for T {}

/// The read only media channel.
pub trait MediaChannel: Send {
    /// Reads the next media frame.
    fn read_frame(&mut self, framing: Framing) -> Result<MediaFrame>;

    /// Description of the local endpoint, used for diagnostics.
    fn local_endpoint(&self) -> String {
        "unknown".to_string()
    }
}

/// Everything that is needed to talk to a WO Mic server.
pub struct Session {
    /// The control channel.
    pub control: Box<dyn ControlChannel>,
    /// The media channel.
    pub media: Box<dyn MediaChannel>,
}

/// Opens the control and media channels for the given transport.
pub fn open(
    transport: womic_core::settings::TransportType,
    host: &str,
    control_port: u16,
    media_port: u16,
    bt_address: &str,
    bt_channel: Option<u8>,
    adb_path: Option<&str>,
) -> Result<Session> {
    match transport {
        womic_core::settings::TransportType::Wifi | womic_core::settings::TransportType::WifiDirect => {
            let session = TcpTransport::connect(host, control_port)?;
            let media = UdpTransport::bind(media_port)?;
            Ok(Session { control: Box::new(session), media: Box::new(media) })
        }
        womic_core::settings::TransportType::Usb => {
            let session = AdbTransport::connect(adb_path, control_port)?;
            Ok(Session { control: Box::new(session.control), media: Box::new(session.media) })
        }
        womic_core::settings::TransportType::Bluetooth => {
            let session = BluetoothTransport::connect(bt_address, bt_channel)?;
            let media = session.media_channel()?;
            Ok(Session { control: Box::new(session), media: Box::new(media) })
        }
    }
}
