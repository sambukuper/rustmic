//! Media channel framing.
//!
//! The WO Mic server uses one of three framings on the media channel. Which
//! one is used is announced by the server in the second byte of the answer to
//! the connect command (`FUN_00404eb0` stores it at controller offset
//! `+0x44`).
//!
//! All integers are big endian. The layouts were recovered from the three
//! frame readers in `WOMicEngine.dll`:
//!
//! | id | transport | layout |
//! |----|-----------|--------|
//! | 2  | USB       | `[hdr0][hdr1][seq:u16][len:u16][payload]` |
//! | 3  | Bluetooth | `[hdr0][hdr1][seq:u16][len:u16][payload]` |
//! | 4  | Wi-Fi     | `[0x04][0x00][len:u16][seq:u16][ts:u32][flag:u8][payload]` |
//!
//! For framing 4 the length field counts everything that follows it, so the
//! payload is `len - 7` bytes long.

use std::io::{Read, Write};

use crate::error::{Error, Result};

/// Framing used on the media channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Framing {
    /// USB transport, `FUN_00404db0`.
    Usb = 2,
    /// Bluetooth transport, `FUN_00404c90`.
    Bluetooth = 3,
    /// Wi-Fi transport, `FUN_00404af0`.
    Wifi = 4,
}

impl Framing {
    /// Maps the framing id announced by the server.
    pub fn from_id(id: u8) -> Result<Framing> {
        match id {
            2 => Ok(Framing::Usb),
            3 => Ok(Framing::Bluetooth),
            4 => Ok(Framing::Wifi),
            other => Err(Error::Protocol(format!("unknown media framing {other}"))),
        }
    }

    /// Numeric id of this framing.
    pub fn id(self) -> u8 {
        self as u8
    }

    /// True when the framing carries a timestamp and a flag byte.
    pub fn has_timestamp(self) -> bool {
        matches!(self, Framing::Wifi)
    }
}

/// One decoded media frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFrame {
    /// Sequence number of the frame.
    pub seq: u16,
    /// Server timestamp in milliseconds, only present for framing 4.
    pub timestamp: u32,
    /// Flag byte of the frame, only present for framing 4.
    pub flags: u8,
    /// Encoded Opus payload.
    pub payload: Vec<u8>,
}

/// Largest payload the client accepts, guarding against a corrupt length.
pub const MAX_PAYLOAD: usize = 4096;

/// Reads one frame from a byte oriented media stream.
///
/// This covers the USB and Bluetooth transports, which deliver the media
/// channel as a reliable byte stream (TCP respectively RFCOMM).
pub fn read_frame<R: Read>(reader: &mut R, framing: Framing) -> Result<MediaFrame> {
    match framing {
        Framing::Usb | Framing::Bluetooth => read_stream_frame(reader),
        Framing::Wifi => read_wifi_frame(reader),
    }
}

/// Reads the `[hdr0][hdr1][seq:u16][len:u16][payload]` framing used by the
/// USB and Bluetooth transports.
fn read_stream_frame<R: Read>(reader: &mut R) -> Result<MediaFrame> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header)?;
    let seq = u16::from_be_bytes([header[2], header[3]]);

    let mut length_bytes = [0u8; 2];
    reader.read_exact(&mut length_bytes)?;
    let length = u16::from_be_bytes(length_bytes) as usize;
    if length > MAX_PAYLOAD {
        return Err(Error::Protocol(format!("media frame of {length} bytes is too large")));
    }

    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload)?;

    Ok(MediaFrame { seq, timestamp: 0, flags: 0, payload })
}

/// Reads the Wi-Fi framing.
///
/// The Wi-Fi transport runs over UDP, so the whole frame arrives in one
/// datagram. The reader therefore consumes exactly one datagram worth of
/// bytes; the caller is expected to feed it a datagram buffer.
fn read_wifi_frame<R: Read>(reader: &mut R) -> Result<MediaFrame> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header)?;
    if header[0] != 0x04 || header[1] != 0x00 {
        return Err(Error::Protocol(format!(
            "unexpected wifi media header {:02x}{:02x}",
            header[0], header[1]
        )));
    }
    let length = u16::from_be_bytes([header[2], header[3]]) as usize;
    if length < 7 {
        return Err(Error::Protocol(format!("wifi media length {length} is too small")));
    }
    if length > MAX_PAYLOAD + 7 {
        return Err(Error::Protocol(format!("media frame of {length} bytes is too large")));
    }

    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;

    let seq = u16::from_be_bytes([body[0], body[1]]);
    let timestamp = u32::from_be_bytes([body[2], body[3], body[4], body[5]]);
    let flags = body[6];
    let payload = body[7..].to_vec();

    Ok(MediaFrame { seq, timestamp, flags, payload })
}

/// Parses one complete frame from a buffer that holds a whole frame.
///
/// The Wi-Fi transport delivers the media as UDP datagrams, so the frame
/// boundaries are given by the datagram. The buffer may contain trailing
/// bytes, which are ignored.
pub fn parse_datagram(buffer: &[u8], framing: Framing) -> Result<MediaFrame> {
    let mut cursor = std::io::Cursor::new(buffer);
    let frame = read_frame(&mut cursor, framing)?;
    Ok(frame)
}

/// Writes a frame, used by the tests and by the loopback diagnostics.
pub fn write_frame<W: Write>(writer: &mut W, framing: Framing, frame: &MediaFrame) -> Result<()> {
    match framing {
        Framing::Usb | Framing::Bluetooth => {
            writer.write_all(&[0x00, 0x00])?;
            writer.write_all(&frame.seq.to_be_bytes())?;
            writer.write_all(&(frame.payload.len() as u16).to_be_bytes())?;
            writer.write_all(&frame.payload)?;
        }
        Framing::Wifi => {
            let length = 7 + frame.payload.len();
            writer.write_all(&[0x04, 0x00])?;
            writer.write_all(&(length as u16).to_be_bytes())?;
            writer.write_all(&frame.seq.to_be_bytes())?;
            writer.write_all(&frame.timestamp.to_be_bytes())?;
            writer.write_all(&[frame.flags])?;
            writer.write_all(&frame.payload)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn usb_frames_are_read_big_endian() {
        // 00 00 | seq 0x1234 | len 0x0003 | payload
        let bytes = [0x00, 0x00, 0x12, 0x34, 0x00, 0x03, 0xAA, 0xBB, 0xCC];
        let mut cursor = Cursor::new(bytes);
        let frame = read_frame(&mut cursor, Framing::Usb).unwrap();
        assert_eq!(frame.seq, 0x1234);
        assert_eq!(frame.payload, vec![0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn bluetooth_frames_use_the_same_layout() {
        let bytes = [0x00, 0x00, 0x00, 0x07, 0x00, 0x01, 0x5A];
        let mut cursor = Cursor::new(bytes);
        let frame = read_frame(&mut cursor, Framing::Bluetooth).unwrap();
        assert_eq!(frame.seq, 7);
        assert_eq!(frame.payload, vec![0x5A]);
    }

    #[test]
    fn wifi_frames_carry_a_timestamp_and_a_flag() {
        // 04 00 | len 0x0008 | seq 0x0001 | ts 0x0000dead | flag 0x03 | one payload byte
        let bytes = [0x04, 0x00, 0x00, 0x08, 0x00, 0x01, 0x00, 0x00, 0xDE, 0xAD, 0x03, 0x42];
        let mut cursor = Cursor::new(bytes);
        let frame = read_frame(&mut cursor, Framing::Wifi).unwrap();
        assert_eq!(frame.seq, 1);
        assert_eq!(frame.timestamp, 0xDEAD);
        assert_eq!(frame.flags, 3);
        assert_eq!(frame.payload, vec![0x42]);
    }

    #[test]
    fn a_wrong_wifi_header_is_rejected() {
        let bytes = [0x05, 0x00, 0x00, 0x08, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut cursor = Cursor::new(bytes);
        assert!(matches!(read_frame(&mut cursor, Framing::Wifi), Err(Error::Protocol(_))));
    }

    #[test]
    fn an_oversized_frame_is_rejected() {
        let bytes = [0x00, 0x00, 0x00, 0x00, 0xFF, 0xFF];
        let mut cursor = Cursor::new(bytes);
        assert!(matches!(read_frame(&mut cursor, Framing::Usb), Err(Error::Protocol(_))));
    }

    #[test]
    fn frames_round_trip_through_the_writer() {
        for framing in [Framing::Usb, Framing::Bluetooth, Framing::Wifi] {
            let frame = MediaFrame {
                seq: 42,
                timestamp: 1234,
                flags: 1,
                payload: vec![1, 2, 3, 4],
            };
            let mut buffer = Vec::new();
            write_frame(&mut buffer, framing, &frame).unwrap();
            let mut cursor = Cursor::new(buffer);
            let decoded = read_frame(&mut cursor, framing).unwrap();
            assert_eq!(decoded.seq, frame.seq);
            assert_eq!(decoded.payload, frame.payload);
            if framing.has_timestamp() {
                assert_eq!(decoded.timestamp, frame.timestamp);
                assert_eq!(decoded.flags, frame.flags);
            }
        }
    }

    #[test]
    fn framing_ids_are_mapped_both_ways() {
        assert_eq!(Framing::from_id(2).unwrap(), Framing::Usb);
        assert_eq!(Framing::from_id(3).unwrap(), Framing::Bluetooth);
        assert_eq!(Framing::from_id(4).unwrap(), Framing::Wifi);
        assert!(Framing::from_id(5).is_err());
        assert_eq!(Framing::Wifi.id(), 4);
    }
}
