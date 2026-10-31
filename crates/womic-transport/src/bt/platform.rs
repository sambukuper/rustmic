//! Platform specific RFCOMM sockets behind [`crate::bt::BtSocket`].
//!
//! Both implementations expose the same operations: opening a raw RFCOMM
//! socket, connecting it to a device address and channel, listing paired
//! devices, and duplicating an open socket so control and media can hold
//! separate handles to the same byte stream.

use std::time::Duration;

/// Timeout used while waiting for data on an open connection.
pub const READ_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(windows)]
mod imp {
    //! Windows: Winsock Bluetooth sockets and the `bthprops` device
    //! enumeration the original client used (`FUN_00402210`).
    use std::io;
    use std::sync::Once;

    use womic_core::error::{Error, Result};

    use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
    use windows_sys::Win32::Networking::WinSock::{
        closesocket, connect, recv, send, setsockopt, shutdown, socket, WSAGetLastError,
        WSAStartup, INVALID_SOCKET, SOCK_STREAM, SOL_SOCKET, SO_RCVTIMEO, SOCKET, WSADATA,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    /// `AF_BTH`, from `ws2bth.h`.
    const AF_BTH: u16 = 32;
    /// `BTPROTO_RFCOMM`, from `ws2bth.h`.
    const BTPROTO_RFCOMM: i32 = 3;
    /// `SD_BOTH`, from `winsock2.h`.
    const SD_BOTH: i32 = 2;

    /// `SOCKADDR_BTH` from `ws2bth.h`: family, 8 byte address, service
    /// class GUID, port; byte packed, 30 bytes. The GUID stays zero for a
    /// raw channel connect, exactly like the original client's `connect`
    /// call (`FUN_00402350`).
    #[repr(C, packed(1))]
    #[derive(Clone, Copy)]
    struct SockAddrBth {
        address_family: u16,
        bt_addr: u64,
        service_class_id: [u8; 16],
        port: u32,
    }

    pub struct BtSocketInner {
        socket: SOCKET,
    }

    // A socket handle is just an integer; moving it between threads is fine.
    unsafe impl Send for BtSocketInner {}

    impl Drop for BtSocketInner {
        fn drop(&mut self) {
            unsafe {
                closesocket(self.socket);
            }
        }
    }

    impl std::fmt::Debug for BtSocketInner {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("BtSocketInner").field("socket", &self.socket).finish()
        }
    }

    impl BtSocketInner {
        /// Opens a raw RFCOMM socket, starting Winsock once per process.
        pub fn open() -> Result<BtSocketInner> {
            static STARTUP: Once = Once::new();
            STARTUP.call_once(|| unsafe {
                let mut data: WSADATA = std::mem::zeroed();
                WSAStartup(0x202, &mut data);
            });
            let raw = unsafe { socket(AF_BTH as i32, SOCK_STREAM, BTPROTO_RFCOMM) };
            if raw == INVALID_SOCKET {
                return Err(Error::DeviceNotFound(last_error_message()));
            }
            Ok(BtSocketInner { socket: raw })
        }

        /// Connects to `addr` on `channel`.
        pub fn connect_channel(&self, addr: u64, channel: u8) -> Result<()> {
            let address = SockAddrBth {
                address_family: AF_BTH,
                bt_addr: addr,
                service_class_id: [0; 16],
                port: channel as u32,
            };
            let result = unsafe {
                connect(
                    self.socket,
                    &address as *const SockAddrBth as *const _,
                    std::mem::size_of::<SockAddrBth>() as i32,
                )
            };
            if result != 0 {
                return Err(Error::DeviceNotFound(format!(
                    "channel {channel}: {}",
                    last_error_message()
                )));
            }
            self.apply_read_timeout();
            Ok(())
        }

        /// Duplicates the socket handle for the media half.
        pub fn duplicate(&self) -> Result<BtSocketInner> {
            let mut duplicate: HANDLE = std::ptr::null_mut();
            let ok = unsafe {
                DuplicateHandle(
                    GetCurrentProcess(),
                    self.socket as HANDLE,
                    GetCurrentProcess(),
                    &mut duplicate,
                    0,
                    0,
                    DUPLICATE_SAME_ACCESS,
                )
            };
            if ok == 0 || duplicate.is_null() {
                return Err(Error::DeviceNotFound("could not duplicate the socket".into()));
            }
            Ok(BtSocketInner { socket: duplicate as SOCKET })
        }

        /// Lists paired (remembered or authenticated) devices, the same
        /// filter the original client's discovery used (`FUN_00402210`).
        pub fn paired_devices() -> Result<Vec<super::super::BtDeviceInfo>> {
            let mut devices = Vec::new();
            unsafe {
                let mut search: BluetoothDeviceSearchParams = std::mem::zeroed();
                search.dw_size = std::mem::size_of::<BluetoothDeviceSearchParams>() as u32;
                search.f_return_remembered = 1;

                let mut info: BluetoothDeviceInfo = std::mem::zeroed();
                info.dw_size = std::mem::size_of::<BluetoothDeviceInfo>() as u32;

                let found = BluetoothFindFirstDevice(&search, &mut info);
                if found.is_null() {
                    // No radio or no paired devices: an empty list is the
                    // honest answer, the caller prints it.
                    return Ok(devices);
                }
                loop {
                    devices.push(device_info_to_device(&info));
                    if BluetoothFindNextDevice(found, &mut info) == 0 {
                        break;
                    }
                }
                BluetoothFindDeviceClose(found);
            }
            Ok(devices)
        }

        /// Applies [`super::READ_TIMEOUT`] to the socket.
        fn apply_read_timeout(&self) {
            let milliseconds: u32 = super::READ_TIMEOUT.as_millis() as u32;
            unsafe {
                setsockopt(
                    self.socket,
                    SOL_SOCKET as i32,
                    SO_RCVTIMEO as i32,
                    &milliseconds as *const u32 as *const u8,
                    4,
                );
            }
        }

        /// Shuts the connection down, waking a blocked reader.
        pub fn shutdown(&self) {
            unsafe {
                shutdown(self.socket, SD_BOTH);
            }
        }

        pub fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
            let received = unsafe {
                recv(self.socket, buffer.as_mut_ptr(), buffer.len() as i32, 0)
            };
            if received < 0 {
                Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }))
            } else {
                Ok(received as usize)
            }
        }

        pub fn write(&self, buffer: &[u8]) -> io::Result<usize> {
            let sent = unsafe {
                send(self.socket, buffer.as_ptr(), buffer.len() as i32, 0)
            };
            if sent < 0 {
                Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }))
            } else {
                Ok(sent as usize)
            }
        }
    }

    impl io::Read for BtSocketInner {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            BtSocketInner::read(self, buffer)
        }
    }

    impl io::Write for BtSocketInner {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            BtSocketInner::write(self, buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn last_error_message() -> String {
        let code = unsafe { WSAGetLastError() };
        match code {
            10048 => "address already in use".into(),
            10060 => "connection timed out".into(),
            10061 => "connection refused".into(),
            other => format!("winsock error {other}"),
        }
    }

    // --- bthprops.cpl FFI -------------------------------------------------
    //
    // The Bluetooth structures are declared by hand instead of through
    // windows-sys so the layout is visible next to the call sites. The
    // sizes match the API: 560 bytes for `BLUETOOTH_DEVICE_INFO`, which is
    // also the value the original client writes (`FUN_00402210`).

    #[repr(C)]
    struct BluetoothDeviceSearchParams {
        dw_size: u32,
        f_return_authenticated: i32,
        f_return_remembered: i32,
        f_return_unknown: i32,
        f_return_connected: i32,
        f_issue_inquiry: i32,
        c_timeout_multiplier: u8,
        _padding: [u8; 7],
        h_radio: HANDLE,
    }

    #[repr(C)]
    struct BluetoothDeviceInfo {
        dw_size: u32,
        _padding: u32,
        address: u64,
        ul_class_of_device: u32,
        f_connected: i32,
        f_remembered: i32,
        f_authenticated: i32,
        st_last_seen: SystemTime,
        st_last_used: SystemTime,
        sz_name: [u16; 248],
    }

    #[repr(C)]
    struct SystemTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        milliseconds: u16,
    }

    #[link(name = "bthprops")]
    extern "system" {
        fn BluetoothFindFirstDevice(
            params: *const BluetoothDeviceSearchParams,
            info: *mut BluetoothDeviceInfo,
        ) -> *mut core::ffi::c_void;
        fn BluetoothFindNextDevice(
            handle: *mut core::ffi::c_void,
            info: *mut BluetoothDeviceInfo,
        ) -> i32;
        fn BluetoothFindDeviceClose(handle: *mut core::ffi::c_void) -> i32;
    }

    fn device_info_to_device(info: &BluetoothDeviceInfo) -> super::super::BtDeviceInfo {
        let name_length =
            info.sz_name.iter().position(|&c| c == 0).unwrap_or(info.sz_name.len());
        let name: String = info.sz_name[..name_length]
            .iter()
            .map(|&c| char::from_u32(c as u32).unwrap_or('?'))
            .collect();
        super::super::BtDeviceInfo {
            name,
            address: super::super::format_bdaddr(info.address),
        }
    }
}

#[cfg(windows)]
pub use imp::BtSocketInner;

#[cfg(unix)]
mod imp {
    //! Linux: BlueZ RFCOMM sockets through raw syscalls; the paired device
    //! list comes from `bluetoothctl`, which every BlueZ install ships.
    use std::io;

    use womic_core::error::{Error, Result};

    /// `AF_BLUETOOTH`, from `bluetooth.h`.
    const AF_BLUETOOTH: libc::c_int = 31;
    /// `BTPROTO_RFCOMM`, from `bluetooth.h`.
    const BTPROTO_RFCOMM: libc::c_int = 3;
    /// `SO_RCVTIMEO`, from `asm-generic/socket.h`.
    const SO_RCVTIMEO: libc::c_int = 20;
    /// `SHUT_RDWR`, from `socket.h`.
    const SHUT_RDWR: libc::c_int = 2;

    /// `struct sockaddr_rc` from `rfcomm.h`: family, 6 byte address,
    /// channel; byte packed, 9 bytes.
    #[repr(C, packed(1))]
    #[derive(Clone, Copy)]
    struct SockAddrRc {
        rc_family: u16,
        rc_bdaddr: [u8; 6],
        rc_channel: u8,
    }

    pub struct BtSocketInner {
        fd: libc::c_int,
    }

    unsafe impl Send for BtSocketInner {}

    impl Drop for BtSocketInner {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.fd);
            }
        }
    }

    impl std::fmt::Debug for BtSocketInner {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("BtSocketInner").field("fd", &self.fd).finish()
        }
    }

    impl BtSocketInner {
        pub fn open() -> Result<BtSocketInner> {
            let fd = unsafe { libc::socket(AF_BLUETOOTH, libc::SOCK_STREAM, BTPROTO_RFCOMM) };
            if fd < 0 {
                return Err(Error::DeviceNotFound(
                    "could not open a Bluetooth socket (is BlueZ running?)".into(),
                ));
            }
            Ok(BtSocketInner { fd })
        }

        pub fn connect_channel(&self, addr: u64, channel: u8) -> Result<()> {
            let address = SockAddrRc {
                rc_family: AF_BLUETOOTH as u16,
                rc_bdaddr: address_bytes(addr),
                rc_channel: channel,
            };
            let result = unsafe {
                libc::connect(
                    self.fd,
                    &address as *const SockAddrRc as *const libc::sockaddr,
                    std::mem::size_of::<SockAddrRc>() as u32,
                )
            };
            if result != 0 {
                return Err(Error::DeviceNotFound(format!(
                    "channel {channel}: {}",
                    io::Error::last_os_error()
                )));
            }
            self.apply_read_timeout();
            Ok(())
        }

        pub fn duplicate(&self) -> Result<BtSocketInner> {
            let fd = unsafe { libc::dup(self.fd) };
            if fd < 0 {
                return Err(Error::DeviceNotFound("could not duplicate the socket".into()));
            }
            Ok(BtSocketInner { fd })
        }

        pub fn paired_devices() -> Result<Vec<super::super::BtDeviceInfo>> {
            let output = std::process::Command::new("bluetoothctl").args(["devices"]).output();
            match output {
                Ok(output) if output.status.success() => {
                    let text = String::from_utf8_lossy(&output.stdout);
                    Ok(text
                        .lines()
                        .filter_map(|line| line.strip_prefix("Device "))
                        .filter_map(|rest| rest.split_once(' '))
                        .map(|(address, name)| super::super::BtDeviceInfo {
                            address: address.to_string(),
                            name: name.trim().to_string(),
                        })
                        .collect())
                }
                Ok(output) => Err(Error::DeviceNotFound(format!(
                    "bluetoothctl failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ))),
                Err(error) => Err(Error::DeviceNotFound(format!(
                    "bluetoothctl is not available: {error}"
                ))),
            }
        }

        fn apply_read_timeout(&self) {
            let timeout = libc::timeval {
                tv_sec: super::READ_TIMEOUT.as_secs() as libc::time_t,
                tv_usec: super::READ_TIMEOUT.subsec_micros() as libc::suseconds_t,
            };
            unsafe {
                libc::setsockopt(
                    self.fd,
                    libc::SOL_SOCKET,
                    SO_RCVTIMEO,
                    &timeout as *const libc::timeval as *const libc::c_void,
                    std::mem::size_of::<libc::timeval>() as u32,
                );
            }
        }

        pub fn shutdown(&self) {
            unsafe {
                libc::shutdown(self.fd, SHUT_RDWR);
            }
        }

        pub fn read(&self, buffer: &mut [u8]) -> io::Result<usize> {
            let received = unsafe {
                libc::read(self.fd, buffer.as_mut_ptr() as *mut libc::c_void, buffer.len())
            };
            if received < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(received as usize)
            }
        }

        pub fn write(&self, buffer: &[u8]) -> io::Result<usize> {
            let sent = unsafe {
                libc::send(
                    self.fd,
                    buffer.as_ptr() as *const libc::c_void,
                    buffer.len(),
                    libc::MSG_NOSIGNAL,
                )
            };
            if sent < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(sent as usize)
            }
        }
    }

    impl io::Read for BtSocketInner {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            BtSocketInner::read(self, buffer)
        }
    }

    impl io::Write for BtSocketInner {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            BtSocketInner::write(self, buffer)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Splits the little endian address integer into the byte order the
    /// kernel expects (`b[0]` is the least significant octet).
    fn address_bytes(addr: u64) -> [u8; 6] {
        [
            (addr & 0xff) as u8,
            ((addr >> 8) & 0xff) as u8,
            ((addr >> 16) & 0xff) as u8,
            ((addr >> 24) & 0xff) as u8,
            ((addr >> 32) & 0xff) as u8,
            ((addr >> 40) & 0xff) as u8,
        ]
    }
}

#[cfg(unix)]
pub use imp::BtSocketInner;
