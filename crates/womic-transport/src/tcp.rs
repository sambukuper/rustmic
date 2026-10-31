//! TCP transport, used for the control channel of the Wi-Fi transports and
//! for both channels of the USB transport (where the TCP connections are
//! tunnelled through `adb forward`).

use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use womic_core::error::{Error, Result};
use womic_core::media::{Framing, MediaFrame};

use crate::MediaChannel;

/// Timeout used for the control connection.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// A TCP connection to the WO Mic server.
#[derive(Debug)]
pub struct TcpTransport {
    stream: TcpStream,
}

impl TcpTransport {
    /// Connects to `host:port`.
    pub fn connect(host: &str, port: u16) -> Result<TcpTransport> {
        let address = resolve(host, port)?;
        let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
            .map_err(|e| Error::Io(std::io::Error::new(e.kind(), format!("{address}: {e}"))))?;
        stream.set_nodelay(true).ok();
        Ok(TcpTransport { stream })
    }

    /// The address of the peer.
    pub fn peer_address(&self) -> Result<SocketAddr> {
        Ok(self.stream.peer_addr()?)
    }

    /// The underlying stream, for duplicating the connection.
    pub fn stream(&self) -> &TcpStream {
        &self.stream
    }

    /// Closes the connection in both directions.
    pub fn close(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// A media channel that is delivered over a TCP stream.
#[derive(Debug)]
pub struct TcpMedia {
    stream: TcpStream,
}

impl TcpMedia {
    /// Wraps an established TCP connection.
    pub fn new(stream: TcpStream) -> TcpMedia {
        TcpMedia { stream }
    }
}

impl MediaChannel for TcpMedia {
    fn read_frame(&mut self, framing: Framing) -> Result<MediaFrame> {
        womic_core::media::read_frame(&mut self.stream, framing)
    }

    fn local_endpoint(&self) -> String {
        self.stream
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    }
}

/// Resolves a host name and a port into a socket address.
pub fn resolve(host: &str, port: u16) -> Result<SocketAddr> {
    for address in (host, port).to_socket_addrs()? {
        return Ok(address);
    }
    Err(Error::Config(format!("could not resolve {host}:{port}")))
}

impl std::io::Read for TcpTransport {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.stream.read(buffer)
    }
}

impl std::io::Write for TcpTransport {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_localhost() {
        let address = resolve("127.0.0.1", 1488).unwrap();
        assert_eq!(address.port(), 1488);
    }

    #[test]
    fn an_unknown_host_is_an_error() {
        assert!(resolve("host.invalid.", 1488).is_err());
    }
}
