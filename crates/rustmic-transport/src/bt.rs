//! Bluetooth transport (RFCOMM).
//!
//! The original client pairs with the phone, asks SDP for the Serial Port
//! Profile service `{00001101-0000-1000-8000-005F9B34FB}` on the chosen
//! device (`FUN_00402760` in `WOMicEngine.dll`), reads the RFCOMM channel
//! out of the protocol descriptor list (attribute 4, `FUN_00402570` and
//! `FUN_004024f0`) and connects a raw RFCOMM socket:
//!
//! ```text
//! socket(AF_BTH /*0x20*/, SOCK_STREAM, BTPROTO_RFCOMM /*3*/)
//! connect(sock, SOCKADDR_BTH { family, btAddr, port = channel }, 30)
//! ```
//!
//! (`FUN_00402350`.) One RFCOMM connection carries the control channel and
//! the media channel; the media reader reads frames from the same stream as
//! the control commands (`FUN_00404730` reads through the controller stream
//! at offset `+0x18`), using the framing the server announced.
//!
//! This implementation resolves the channel the same way when it can and
//! otherwise scans channels 1..=30, which reaches the same server because
//! the WO Mic service is the only SPP listener on the phone.

use std::io::{Read, Write};

use rustmic_core::error::{Error, Result};
use rustmic_core::media::{Framing, MediaFrame};

use crate::MediaChannel;

/// Service class UUID of the Serial Port Profile, as sent by the original
/// client in its SDP query (`FUN_00402760`, stack GUID at `-0xfe8`):
/// `{00001101-0000-1000-8000-005F9B34FB}` in Windows GUID memory order.
pub const SPP_SERVICE_CLASS: [u8; 16] = [
    0x01, 0x11, 0x00, 0x00, // data1, little endian
    0x00, 0x00, // data2
    0x00, 0x10, // data3
    0x80, 0x00, 0x00, 0x80, 0x5f, 0x9b, 0x34, 0xfb, // data4
];

/// Channels scanned when the SDP lookup did not run.
pub const CHANNEL_SCAN: std::ops::RangeInclusive<u8> = 1..=30;

/// A Bluetooth device seen by the operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtDeviceInfo {
    /// Human readable device name.
    pub name: String,
    /// Address in the `AA:BB:CC:DD:EE:FF` form.
    pub address: String,
}

/// An open RFCOMM connection. One stream carries control and media traffic.
#[derive(Debug)]
pub struct BtSocket {
    inner: BtSocketInner,
}

impl BtSocket {
    /// Shuts the connection down, waking a blocked reader.
    pub fn shutdown(&self) {
        self.inner.shutdown();
    }
}

/// The Bluetooth transport: an open socket plus its address bookkeeping.
#[derive(Debug)]
pub struct BluetoothTransport {
    socket: BtSocket,
}

impl BluetoothTransport {
    /// Connects to the WO Mic service on `address` (any common punctuation
    /// form, or `.` separated as stored in `settings.dat`).
    ///
    /// With `channel` the client dials that RFCOMM channel directly;
    /// otherwise it scans the usual SPP channels.
    pub fn connect(address: &str, channel: Option<u8>) -> Result<BluetoothTransport> {
        let addr = parse_bdaddr(address)?;
        let socket = BtSocketInner::open()?;
        let failure = match channel {
            Some(channel) => socket.connect_channel(addr, channel).err(),
            None => {
                let mut failure = None;
                for candidate in CHANNEL_SCAN {
                    match socket.connect_channel(addr, candidate) {
                        Ok(()) => {
                            failure = None;
                            break;
                        }
                        Err(error) => failure = Some(error),
                    }
                }
                failure
            }
        };
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(BluetoothTransport { socket: BtSocket { inner: socket } })
    }

    /// Lists the paired devices the operating system knows about.
    pub fn paired_devices() -> Result<Vec<BtDeviceInfo>> {
        BtSocketInner::paired_devices()
    }

    /// Duplicates the connection for the media channel.
    ///
    /// Both halves read the same RFCOMM byte stream, exactly like the
    /// original client that hands one `BtDeviceStream` to control and media.
    pub fn media_channel(&self) -> Result<MediaHalf> {
        let inner = self.socket.inner.duplicate()?;
        Ok(MediaHalf { socket: BtSocket { inner } })
    }

    /// The control half of the connection.
    pub fn control_half(&self) -> &BtSocket {
        &self.socket
    }

    /// Address of the local radio, for diagnostics.
    pub fn local_endpoint(&self) -> String {
        "bluetooth".to_string()
    }
}

impl std::io::Read for BluetoothTransport {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.socket.read(buffer)
    }
}

impl std::io::Write for BluetoothTransport {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.socket.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The read only media half of a Bluetooth connection.
pub struct MediaHalf {
    socket: BtSocket,
}

impl MediaChannel for MediaHalf {
    fn read_frame(&mut self, framing: Framing) -> Result<MediaFrame> {
        rustmic_core::media::read_frame(&mut self.socket, framing)
    }
}

/// Parses a Bluetooth address in `AA:BB:CC:DD:EE:FF`, `AA-BB-..`, `AABB..`
/// or the `settings.dat` form `AA.BB.CC.DD.EE.FF`.
///
/// Returns the address as the little endian 64 bit integer the platform
/// APIs expect (the last octet is the most significant byte).
pub fn parse_bdaddr(text: &str) -> Result<u64> {
    let cleaned: String = text
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    if cleaned.len() != 12 || !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(Error::Config(format!("'{text}' is not a Bluetooth address")));
    }
    let mut bytes = [0u8; 6];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&cleaned[index * 2..index * 2 + 2], 16)
            .map_err(|_| Error::Config(format!("'{text}' is not a Bluetooth address")))?;
    }
    let mut address = 0u64;
    for byte in bytes {
        address = (address >> 8) | ((byte as u64) << 40);
    }
    Ok(address)
}

/// Renders an address integer back into the `AA:BB:CC:DD:EE:FF` form.
pub fn format_bdaddr(address: u64) -> String {
    let octets = [
        (address & 0xff) as u8,
        ((address >> 8) & 0xff) as u8,
        ((address >> 16) & 0xff) as u8,
        ((address >> 24) & 0xff) as u8,
        ((address >> 32) & 0xff) as u8,
        ((address >> 40) & 0xff) as u8,
    ];
    octets.map(|o| format!("{o:02X}")).join(":")
}

#[cfg(windows)]
mod platform;

#[cfg(unix)]
mod platform;

#[cfg(windows)]
use platform::BtSocketInner;

#[cfg(unix)]
use platform::BtSocketInner;

impl Read for BtSocket {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl Write for BtSocket {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_address_spelling() {
        for spelling in [
            "AA:BB:CC:DD:EE:FF",
            "aa-bb-cc-dd-ee-ff",
            "aabbccddeeff",
            "AA.BB.CC.DD.EE.FF",
        ] {
            assert_eq!(parse_bdaddr(spelling).unwrap(), 0xFFEEDDCCBBAA);
        }
    }

    #[test]
    fn rejects_broken_addresses() {
        assert!(parse_bdaddr("not-an-address").is_err());
        assert!(parse_bdaddr("AA:BB:CC:DD:EE").is_err());
        assert!(parse_bdaddr("00.0.0.0.0.0").is_err());
    }

    #[test]
    fn formats_the_address_back() {
        assert_eq!(format_bdaddr(0xFFEEDDCCBBAA), "AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn the_spp_uuid_matches_the_original_query() {
        // {00001101-0000-1000-8000-005F9B34FB}, Windows GUID memory order
        assert_eq!(
            SPP_SERVICE_CLASS,
            [0x01, 0x11, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x80, 0x00, 0x00, 0x80, 0x5f,
             0x9b, 0x34, 0xfb]
        );
    }
}
