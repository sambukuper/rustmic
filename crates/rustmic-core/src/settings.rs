//! Reader and writer for the `settings.dat` file of the original client.
//!
//! The file is a plain text key/value list. Every record of the shipped file
//! (`settings.dat`, 203 bytes) looks like `key:value\r\n`:
//!
//! ```text
//! transportType:3
//! wifiServerAddr:192.168.1.178
//! softApAddr:192.168.43.1
//! playLocally:1
//! controlPort:1488
//! mediaPort:49152
//! bufferSize:3
//! autoConnect:0
//! window:712,403,500,357,1972
//! btAddress:0.0.0.0.0.0
//! ```
//!
//! The same key names and the same decimal encodings are used by the Rust
//! client, so a configuration written by either implementation can be read by
//! the other.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// Default TCP port of the WO Mic control channel.
pub const DEFAULT_CONTROL_PORT: u16 = 1488;
/// Default UDP port of the WO Mic media channel (Wi-Fi transport).
pub const DEFAULT_MEDIA_PORT: u16 = 49152;
/// Default number of packets kept in the jitter buffer.
pub const DEFAULT_BUFFER_SIZE: u32 = 3;

/// Transport used to reach the WO Mic server.
///
/// The numeric values are the ones the client puts on the wire in the `f`
/// (version) command; they are written big endian as a 32 bit integer
/// (see `FUN_00405000` in `WOMicEngine.dll`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportType {
    /// Bluetooth RFCOMM transport.
    Bluetooth = 1,
    /// USB transport, tunnelled through `adb forward`.
    Usb = 2,
    /// Wi-Fi transport (client and server in the same LAN).
    Wifi = 3,
    /// Wi-Fi Direct transport (Android in hotspot mode).
    WifiDirect = 4,
}

impl TransportType {
    /// Maps the numeric value stored in `settings.dat` to a transport.
    pub fn from_i32(value: i32) -> Result<Self> {
        match value {
            1 => Ok(TransportType::Bluetooth),
            2 => Ok(TransportType::Usb),
            3 => Ok(TransportType::Wifi),
            4 => Ok(TransportType::WifiDirect),
            other => Err(Error::Config(format!("unknown transport type {other}"))),
        }
    }

    /// Numeric value as stored in `settings.dat` and sent on the wire.
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    /// Short human readable name, used on the command line.
    pub fn short_name(self) -> &'static str {
        match self {
            TransportType::Bluetooth => "bluetooth",
            TransportType::Usb => "usb",
            TransportType::Wifi => "wifi",
            TransportType::WifiDirect => "wifi-direct",
        }
    }

    /// Parses the name accepted on the command line.
    pub fn from_name(name: &str) -> Result<Self> {
        match name.to_ascii_lowercase().as_str() {
            "bluetooth" | "bt" => Ok(TransportType::Bluetooth),
            "usb" => Ok(TransportType::Usb),
            "wifi" => Ok(TransportType::Wifi),
            "wifi-direct" | "wifidirect" | "softap" => Ok(TransportType::WifiDirect),
            other => Err(Error::Config(format!("unknown transport '{other}'"))),
        }
    }
}

impl fmt::Display for TransportType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.short_name())
    }
}

/// Position and size of the main window of the original client.
///
/// The original stores five comma separated integers. The first four are the
/// usual `left,top,width,height`; the fifth is an internal state value that
/// the client uses to remember the last window state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowState {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    pub state: i32,
}

impl Default for WindowState {
    fn default() -> Self {
        WindowState { left: 712, top: 403, width: 500, height: 357, state: 1972 }
    }
}

/// Complete content of `settings.dat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Transport used for the next connection.
    pub transport_type: TransportType,
    /// Address of the WO Mic server in the local network.
    pub wifi_server_addr: String,
    /// Address of the Android device when it runs as a Wi-Fi hotspot.
    pub soft_ap_addr: String,
    /// Play the received audio on this computer.
    pub play_locally: bool,
    /// TCP port of the control channel.
    pub control_port: u16,
    /// UDP port of the media channel.
    pub media_port: u16,
    /// Number of packets kept in the jitter buffer.
    pub buffer_size: u32,
    /// Connect automatically when the client starts.
    pub auto_connect: bool,
    /// Remembered window geometry.
    pub window: WindowState,
    /// Bluetooth address of the server, `0.0.0.0.0.0` when unset.
    pub bt_address: String,
    /// Audio output device substring for this client, empty for the
    /// default device. An extension the original client ignores.
    pub audio_device: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            transport_type: TransportType::Wifi,
            wifi_server_addr: "192.168.1.178".to_string(),
            soft_ap_addr: "192.168.43.1".to_string(),
            play_locally: true,
            control_port: DEFAULT_CONTROL_PORT,
            media_port: DEFAULT_MEDIA_PORT,
            buffer_size: DEFAULT_BUFFER_SIZE,
            auto_connect: false,
            window: WindowState::default(),
            bt_address: "0.0.0.0.0.0".to_string(),
            audio_device: String::new(),
        }
    }
}

impl Settings {
    /// Parses the textual representation of a `settings.dat` file.
    ///
    /// Unknown keys are ignored and malformed values fall back to the
    /// default, which mirrors the tolerant behaviour of the original client.
    pub fn parse(text: &str) -> Settings {
        let mut settings = Settings::default();
        for line in text.split('\n') {
            let line = line.trim_end_matches('\r');
            let Some((key, value)) = line.split_once(':') else { continue };
            let key = key.trim();
            let value = value.trim();
            match key {
                "transportType" => {
                    if let Ok(v) = value.parse::<i32>() {
                        if let Ok(t) = TransportType::from_i32(v) {
                            settings.transport_type = t;
                        }
                    }
                }
                "wifiServerAddr" => settings.wifi_server_addr = value.to_string(),
                "softApAddr" => settings.soft_ap_addr = value.to_string(),
                "playLocally" => settings.play_locally = value != "0",
                "controlPort" => {
                    if let Ok(v) = value.parse::<u16>() {
                        settings.control_port = v;
                    }
                }
                "mediaPort" => {
                    if let Ok(v) = value.parse::<u16>() {
                        settings.media_port = v;
                    }
                }
                "bufferSize" => {
                    if let Ok(v) = value.parse::<u32>() {
                        settings.buffer_size = v;
                    }
                }
                "autoConnect" => settings.auto_connect = value != "0",
                "window" => settings.window = parse_window(value),
                "btAddress" => settings.bt_address = value.to_string(),
                "audioDevice" => settings.audio_device = value.to_string(),
                _ => {}
            }
        }
        settings
    }

    /// Renders the settings in the format of the original `settings.dat`.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("transportType:{}\r\n", self.transport_type.as_i32()));
        out.push_str(&format!("wifiServerAddr:{}\r\n", self.wifi_server_addr));
        out.push_str(&format!("softApAddr:{}\r\n", self.soft_ap_addr));
        out.push_str(&format!("playLocally:{}\r\n", self.play_locally as u8));
        out.push_str(&format!("controlPort:{}\r\n", self.control_port));
        out.push_str(&format!("mediaPort:{}\r\n", self.media_port));
        out.push_str(&format!("bufferSize:{}\r\n", self.buffer_size));
        out.push_str(&format!("autoConnect:{}\r\n", self.auto_connect as u8));
        out.push_str(&format!(
            "window:{},{},{},{},{}\r\n",
            self.window.left,
            self.window.top,
            self.window.width,
            self.window.height,
            self.window.state
        ));
        out.push_str(&format!("btAddress:{}\r\n", self.bt_address));
        out
    }

    /// Reads a `settings.dat` file.
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Settings> {
        let text = fs::read_to_string(path.as_ref())?;
        Ok(Settings::parse(&text))
    }

    /// Writes a `settings.dat` file.
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        fs::write(path.as_ref(), self.to_text())?;
        Ok(())
    }

    /// Looks for `settings.dat` next to the executable and in the current
    /// working directory, mirroring the lookup of the original client.
    pub fn discover(exe_dir: Option<&Path>) -> Option<(PathBuf, Settings)> {
        let mut candidates = Vec::new();
        if let Some(dir) = exe_dir {
            candidates.push(dir.join("settings.dat"));
        }
        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd.join("settings.dat"));
        }
        for candidate in candidates {
            if let Ok(settings) = Settings::load(&candidate) {
                return Some((candidate, settings));
            }
        }
        None
    }

    /// Address of the server for the currently selected transport.
    ///
    /// For the Wi-Fi Direct transport the Android device acts as the access
    /// point, so the soft AP address is used instead of the LAN address.
    pub fn server_address(&self) -> &str {
        match self.transport_type {
            TransportType::WifiDirect => &self.soft_ap_addr,
            _ => &self.wifi_server_addr,
        }
    }
}

fn parse_window(value: &str) -> WindowState {
    let mut window = WindowState::default();
    let mut parts = value.split(',');
    let mut next = |slot: &mut i32| {
        if let Some(v) = parts.next() {
            if let Ok(parsed) = v.trim().parse::<i32>() {
                *slot = parsed;
            }
        }
    };
    next(&mut window.left);
    next(&mut window.top);
    next(&mut window.width);
    next(&mut window.height);
    next(&mut window.state);
    window
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHIPPED: &str = concat!(
        "transportType:3\r\n",
        "wifiServerAddr:192.168.1.178\r\n",
        "softApAddr:192.168.43.1\r\n",
        "playLocally:1\r\n",
        "controlPort:1488\r\n",
        "mediaPort:49152\r\n",
        "bufferSize:3\r\n",
        "autoConnect:0\r\n",
        "window:712,403,500,357,1972\r\n",
        "btAddress:0.0.0.0.0.0\r\n",
    );

    #[test]
    fn parses_the_shipped_settings_file() {
        let settings = Settings::parse(SHIPPED);
        assert_eq!(settings.transport_type, TransportType::Wifi);
        assert_eq!(settings.wifi_server_addr, "192.168.1.178");
        assert_eq!(settings.soft_ap_addr, "192.168.43.1");
        assert!(settings.play_locally);
        assert_eq!(settings.control_port, 1488);
        assert_eq!(settings.media_port, 49152);
        assert_eq!(settings.buffer_size, 3);
        assert!(!settings.auto_connect);
        assert_eq!(settings.window.left, 712);
        assert_eq!(settings.window.top, 403);
        assert_eq!(settings.window.width, 500);
        assert_eq!(settings.window.height, 357);
        assert_eq!(settings.window.state, 1972);
        assert_eq!(settings.bt_address, "0.0.0.0.0.0");
    }

    #[test]
    fn round_trips_the_shipped_settings_file() {
        let settings = Settings::parse(SHIPPED);
        assert_eq!(settings.to_text(), SHIPPED);
        assert_eq!(Settings::parse(&settings.to_text()), settings);
    }

    #[test]
    fn ignores_unknown_and_broken_records() {
        let settings = Settings::parse("transportType:99\r\nnonsense\r\ncontrolPort:abc\r\n");
        assert_eq!(settings.transport_type, TransportType::Wifi);
        assert_eq!(settings.control_port, DEFAULT_CONTROL_PORT);
    }

    #[test]
    fn maps_transport_numbers() {
        assert_eq!(TransportType::from_i32(1).unwrap(), TransportType::Bluetooth);
        assert_eq!(TransportType::from_i32(2).unwrap(), TransportType::Usb);
        assert_eq!(TransportType::from_i32(3).unwrap(), TransportType::Wifi);
        assert_eq!(TransportType::from_i32(4).unwrap(), TransportType::WifiDirect);
        assert!(TransportType::from_i32(5).is_err());
    }

    #[test]
    fn wifi_direct_uses_the_soft_ap_address() {
        let mut settings = Settings::default();
        settings.transport_type = TransportType::WifiDirect;
        assert_eq!(settings.server_address(), "192.168.43.1");
        settings.transport_type = TransportType::Wifi;
        assert_eq!(settings.server_address(), "192.168.1.178");
    }
}
