//! Control-channel protocol of WO Mic.
//!
//! The control channel is a plain TCP stream (or an RFCOMM stream for the
//! Bluetooth transport). Every message is length prefixed:
//!
//! ```text
//! +--------+------------------+-------------------+
//! | tag:1  | length: u32 (BE) | payload: length   |
//! +--------+------------------+-------------------+
//! ```
//!
//! All integers are in network byte order; this was verified against the
//! disassembly of the three command senders in `WOMicEngine.dll`:
//!
//! * `FUN_00404eb0` – `e` (connect), payload `04 04 00 00 00 00`
//! * `FUN_00405000` – `f` (version check), payload `02 02` + transport `u32 BE`
//! * `FUN_00405120` – `g` (start streaming), empty payload
//!
//! The answers are read back as `[tag:1][length:u32 BE][payload]`; the
//! original client only looks at the first bytes of the payload and treats a
//! wrong tag as a version mismatch.

use std::io::{Read, Write};

use crate::error::{Error, Result};
use crate::settings::TransportType;

/// Tag of the "connect" command.
pub const TAG_CONNECT: u8 = 0x65;
/// Tag of the "version check" command.
pub const TAG_VERSION: u8 = 0x66;
/// Tag of the "start streaming" command.
pub const TAG_START: u8 = 0x67;

/// Payload of the connect command, verbatim as sent by the original client.
pub const CONNECT_PAYLOAD: [u8; 6] = [0x04, 0x04, 0x00, 0x00, 0x00, 0x00];
/// First two bytes of the version-check payload.
pub const VERSION_PAYLOAD_PREFIX: [u8; 2] = [0x02, 0x02];

/// Builds a length prefixed control message.
pub fn encode_message(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut message = Vec::with_capacity(5 + payload.len());
    message.push(tag);
    message.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    message.extend_from_slice(payload);
    message
}

/// The connect command with its fixed payload.
pub fn connect_message() -> Vec<u8> {
    encode_message(TAG_CONNECT, &CONNECT_PAYLOAD)
}

/// The version-check command for the given transport.
pub fn version_message(transport: TransportType) -> Vec<u8> {
    let mut payload = Vec::with_capacity(6);
    payload.extend_from_slice(&VERSION_PAYLOAD_PREFIX);
    payload.extend_from_slice(&(transport.as_i32() as u32).to_be_bytes());
    encode_message(TAG_VERSION, &payload)
}

/// The start-streaming command.
pub fn start_message() -> Vec<u8> {
    encode_message(TAG_START, &[])
}

/// A decoded answer of the control channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    /// Message tag.
    pub tag: u8,
    /// Length the server announced for the payload.
    pub length: u32,
    /// Payload bytes.
    pub payload: Vec<u8>,
}

/// Result of the connect command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectReply {
    /// Zero when the server accepted the connection.
    pub status: u8,
    /// Media framing the server wants the client to use.
    pub framing: u8,
}

/// High level client for the WO Mic control channel.
#[derive(Debug)]
pub struct ControlClient<S> {
    stream: S,
}

impl<S: Read + Write> ControlClient<S> {
    /// Wraps a stream.
    pub fn new(stream: S) -> Self {
        ControlClient { stream }
    }

    /// Gives access to the underlying stream.
    pub fn get_ref(&self) -> &S {
        &self.stream
    }

    /// Gives mutable access to the underlying stream.
    pub fn get_mut(&mut self) -> &mut S {
        &mut self.stream
    }

    /// Sends the connect command and reads the answer.
    ///
    /// The second byte of the answer is the media framing the server wants to
    /// use; the original client stores it next to the connection state
    /// (`FUN_00403ea0`, controller offset `+0x44`).
    pub fn connect(&mut self) -> Result<ConnectReply> {
        let reply = self.exchange(TAG_CONNECT, &connect_message(), 2)?;
        let status = reply.payload[0];
        if status != 0 {
            return Err(Error::Negotiation);
        }
        Ok(ConnectReply { status, framing: reply.payload[1] })
    }

    /// Sends the version-check command.
    ///
    /// A non zero status or a wrong tag is reported as a version mismatch,
    /// which is the code the original client shows for `-7`.
    pub fn check_version(&mut self, transport: TransportType) -> Result<()> {
        let reply = self.exchange(TAG_VERSION, &version_message(transport), 1)?;
        if reply.payload[0] != 0 {
            return Err(Error::Mismatch);
        }
        Ok(())
    }

    /// Sends the start-streaming command.
    pub fn start(&mut self) -> Result<()> {
        let reply = self.exchange(TAG_START, &start_message(), 1)?;
        if reply.payload[0] != 0 {
            return Err(Error::MediaStart);
        }
        Ok(())
    }

    /// Writes a command and reads the matching answer.
    fn exchange(&mut self, expected_tag: u8, message: &[u8], expected_len: usize) -> Result<Reply> {
        self.stream.write_all(message)?;
        self.stream.flush()?;

        let mut tag = [0u8; 1];
        self.stream.read_exact(&mut tag)?;
        // The original client looks at the tag before anything else and
        // reports a wrong one as a version mismatch; do the same instead of
        // waiting for the payload.
        if tag[0] != expected_tag {
            return Err(Error::Mismatch);
        }
        let mut length_bytes = [0u8; 4];
        self.stream.read_exact(&mut length_bytes)?;
        let length = u32::from_be_bytes(length_bytes) as usize;

        // The original client reads a fixed number of bytes and ignores the
        // announced length. We consume the whole announced payload and top it
        // up to the number of bytes the client looks at, so a server that
        // sends a longer answer cannot desynchronise the stream.
        let mut payload = vec![0u8; length];
        self.stream.read_exact(&mut payload)?;
        while payload.len() < expected_len {
            let mut extra = [0u8; 1];
            self.stream.read_exact(&mut extra)?;
            payload.push(extra[0]);
        }

        Ok(Reply { tag: tag[0], length: length as u32, payload })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read, Write};

    /// A test stream with independent read and write directions, like a
    /// socket: writes are collected, reads consume the reply bytes.
    struct MockStream {
        replies: Cursor<Vec<u8>>,
        _written: Vec<u8>,
    }

    impl MockStream {
        fn new(replies: Vec<u8>) -> MockStream {
            MockStream { replies: Cursor::new(replies), _written: Vec::new() }
        }
    }

    impl Read for MockStream {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.replies.read(buffer)
        }
    }

    impl Write for MockStream {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self._written.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn connect_message_matches_the_original_bytes() {
        assert_eq!(connect_message(), vec![0x65, 0, 0, 0, 6, 0x04, 0x04, 0, 0, 0, 0]);
    }

    #[test]
    fn version_message_carries_the_transport_big_endian() {
        assert_eq!(
            version_message(TransportType::Wifi),
            vec![0x66, 0, 0, 0, 6, 0x02, 0x02, 0, 0, 0, 3]
        );
        assert_eq!(
            version_message(TransportType::Bluetooth),
            vec![0x66, 0, 0, 0, 6, 0x02, 0x02, 0, 0, 0, 1]
        );
    }

    #[test]
    fn start_message_has_an_empty_payload() {
        assert_eq!(start_message(), vec![0x67, 0, 0, 0, 0]);
    }

    #[test]
    fn connect_reads_status_and_framing() {
        // server answers 65 00 00 00 02 00 04
        let mut stream = MockStream::new(vec![0x65, 0, 0, 0, 2, 0x00, 0x04]);
        let mut client = ControlClient::new(&mut stream);
        let reply = client.connect().unwrap();
        assert_eq!(reply.status, 0);
        assert_eq!(reply.framing, 4);
    }

    #[test]
    fn connect_reports_a_non_zero_status_as_negotiation_error() {
        let mut stream = MockStream::new(vec![0x65, 0, 0, 0, 2, 0x01, 0x04]);
        let mut client = ControlClient::new(&mut stream);
        assert!(matches!(client.connect(), Err(Error::Negotiation)));
    }

    #[test]
    fn a_wrong_tag_is_a_mismatch() {
        let mut stream = MockStream::new(vec![0x66, 0, 0, 0, 1, 0x00]);
        let mut client = ControlClient::new(&mut stream);
        assert!(matches!(client.connect(), Err(Error::Mismatch)));
    }

    #[test]
    fn version_and_start_are_acknowledged() {
        let mut stream =
            MockStream::new(vec![0x66, 0, 0, 0, 1, 0x00, 0x67, 0, 0, 0, 1, 0x00]);
        let mut client = ControlClient::new(&mut stream);
        client.check_version(TransportType::Wifi).unwrap();
        client.start().unwrap();
    }

    #[test]
    fn a_longer_answer_is_consumed_completely() {
        // three payload bytes announced, the client only looks at the first
        let mut stream = MockStream::new(vec![0x66, 0, 0, 0, 3, 0x00, 0xAA, 0xBB]);
        let mut client = ControlClient::new(&mut stream);
        client.check_version(TransportType::Wifi).unwrap();
        // the reply stream must be fully consumed, no leftovers
        assert_eq!(stream.replies.position(), 8);
    }
}
