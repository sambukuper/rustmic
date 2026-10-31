//! TCP transport, used for the control channel of the Wi-Fi transports and
//! for both channels of the USB transport (where the TCP connections are
//! tunnelled through `adb forward`).

use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use rustmic_core::error::{Error, Result};
use rustmic_core::media::{Framing, MediaFrame};

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
        enable_keepalive(&stream);
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
        rustmic_core::media::read_frame(&mut self.stream, framing)
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

/// Enables short TCP keepalives on the control stream.
///
/// After the handshake the control channel sits idle while the media
/// arrives on its own channel. Routers and firewalls forget such idle
/// mappings after a few minutes and the session dies with them. Keepalive
/// probes keep the mapping alive without sending anything on the wire,
/// so the protocol never sees them.
pub(crate) fn enable_keepalive(stream: &TcpStream) {
    #[cfg(target_os = "windows")]
    unsafe {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::{
            setsockopt, tcp_keepalive, WSAIoctl, SIO_KEEPALIVE_VALS, SOCKET_ERROR, SOL_SOCKET,
            SO_KEEPALIVE,
        };
        let socket = stream.as_raw_socket() as _;
        let one: i32 = 1;
        if setsockopt(
            socket,
            SOL_SOCKET,
            SO_KEEPALIVE,
            &one as *const i32 as *const u8,
            4,
        ) == SOCKET_ERROR
        {
            return;
        }
        // Probe after 15 idle seconds, then every 3 seconds, so the
        // mapping is refreshed long before a router forgets it.
        let params = tcp_keepalive { onoff: 1, keepalivetime: 15_000, keepaliveinterval: 3_000 };
        let mut returned: u32 = 0;
        WSAIoctl(
            socket,
            SIO_KEEPALIVE_VALS,
            &params as *const tcp_keepalive as *const core::ffi::c_void,
            std::mem::size_of::<tcp_keepalive>() as u32,
            std::ptr::null_mut(),
            0,
            &mut returned,
            std::ptr::null_mut(),
            None,
        );
    }
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::io::AsRawFd;
        let fd = stream.as_raw_fd();
        let one: libc::c_int = 1;
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_KEEPALIVE,
            &one as *const libc::c_int as *const libc::c_void,
            std::mem::size_of_val(&one) as u32,
        );
        let idle: libc::c_int = 15;
        let interval: libc::c_int = 3;
        let probes: libc::c_int = 4;
        for (option, value) in
            [(libc::TCP_KEEPIDLE, idle), (libc::TCP_KEEPINTVL, interval), (libc::TCP_KEEPCNT, probes)]
        {
            libc::setsockopt(
                fd,
                libc::IPPROTO_TCP,
                option,
                &value as *const libc::c_int as *const libc::c_void,
                std::mem::size_of_val(&value) as u32,
            );
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux")))]
    {
        let _ = stream;
    }
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
