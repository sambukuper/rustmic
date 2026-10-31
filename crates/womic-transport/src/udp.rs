//! UDP transport for the media channel of the Wi-Fi transports.
//!
//! The WO Mic server streams the audio as UDP datagrams. Every datagram
//! carries one complete media frame using the framing the server announced
//! during the handshake.

use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use womic_core::error::{Error, Result};
use womic_core::media::{Framing, MediaFrame};

use crate::MediaChannel;

/// Largest datagram the client accepts.
pub const MAX_DATAGRAM: usize = 4096;

/// The UDP socket the media channel arrives on.
#[derive(Debug)]
pub struct UdpTransport {
    socket: UdpSocket,
}

impl UdpTransport {
    /// Binds the media socket to the given port on every interface.
    ///
    /// The server sends the audio to the address it saw on the control
    /// connection, using the same port number on both sides.
    pub fn bind(port: u16) -> Result<UdpTransport> {
        let socket = UdpSocket::bind(("0.0.0.0", port)).map_err(|e| {
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("could not bind the media port {port}: {e}"),
            ))
        })?;
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
        Ok(UdpTransport { socket })
    }

    /// Wraps an already bound socket.
    pub fn from_socket(socket: UdpSocket) -> UdpTransport {
        UdpTransport { socket }
    }

    /// The port the socket is bound to.
    pub fn local_port(&self) -> u16 {
        self.socket.local_addr().map(|a| a.port()).unwrap_or(0)
    }
}

impl MediaChannel for UdpTransport {
    fn read_frame(&mut self, framing: Framing) -> Result<MediaFrame> {
        let mut buffer = vec![0u8; MAX_DATAGRAM];
        let (length, _peer) = self.socket.recv_from(&mut buffer)?;
        buffer.truncate(length);
        womic_core::media::parse_datagram(&buffer, framing)
    }

    fn local_endpoint(&self) -> String {
        self.socket
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    }
}

/// Convenience helper for tests and diagnostics.
pub fn local_address(port: u16) -> Result<SocketAddr> {
    Ok(UdpSocket::bind(("0.0.0.0", port))?.local_addr()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_arrives_as_a_datagram() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_port = server.local_addr().unwrap().port();
        let client = UdpTransport::bind(0).unwrap();
        let client_port = client.local_port();
        assert_ne!(client_port, 0);

        let payload = vec![1u8, 2, 3];
        let mut datagram = Vec::new();
        datagram.extend_from_slice(&[0x04, 0x00]);
        datagram.extend_from_slice(&((7 + payload.len()) as u16).to_be_bytes());
        datagram.extend_from_slice(&9u16.to_be_bytes());
        datagram.extend_from_slice(&1234u32.to_be_bytes());
        datagram.push(0x07);
        datagram.extend_from_slice(&payload);
        server.send_to(&datagram, ("127.0.0.1", client_port)).unwrap();

        let mut channel = client;
        let frame = channel.read_frame(Framing::Wifi).unwrap();
        assert_eq!(frame.seq, 9);
        assert_eq!(frame.timestamp, 1234);
        assert_eq!(frame.flags, 0x07);
        assert_eq!(frame.payload, payload);
        let _ = server_port;
    }
}
