//! Environment repairs for connection drops, reachable from the Fix menu.
//!
//! Three kinds of repairs live here: rules that keep the firewall from
//! blocking the client, power management switches that stop Windows and
//! Linux from suspending the adapters or the machine mid stream, and the
//! sleep inhibitor the connection thread holds while streaming. All of
//! the elevated ones ask for permission through the platform's own
//! prompt (UAC on Windows, pkexec on Linux).

use std::process::Command;

/// One repair the Fix dialog offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repair {
    /// Firewall rules for the client executable (Windows) or the control
    /// and media ports (Linux).
    Firewall,
    /// Stops Windows from suspending USB devices, the usual killer of the
    /// USB transport on laptops.
    UsbSuspend,
    /// Stops Windows from powering down network adapters.
    AdapterPower,
    /// Turns off Wi-Fi power saving on Linux (`iw dev ... set power_save
    /// off`), a common cause of drops on battery power.
    WifiPowerSave,
    /// Clears the kernel's USB autosuspend on Linux.
    UsbAutosuspend,
}

impl Repair {
    /// The label shown on the button.
    pub fn label(self) -> &'static str {
        match self {
            Repair::Firewall => "Add firewall rules",
            Repair::UsbSuspend => "Disable USB selective suspend",
            Repair::AdapterPower => "Keep network adapters awake",
            Repair::WifiPowerSave => "Turn off Wi-Fi power saving",
            Repair::UsbAutosuspend => "Clear USB autosuspend",
        }
    }

    /// Whether this repair applies to the running platform.
    pub fn available(self) -> bool {
        match self {
            Repair::Firewall | Repair::UsbSuspend | Repair::AdapterPower => {
                cfg!(target_os = "windows")
            }
            Repair::WifiPowerSave | Repair::UsbAutosuspend => cfg!(target_os = "linux"),
        }
    }
}

/// Builds the cmd script body for a Windows repair. Public for tests.
pub fn windows_script(repair: Repair) -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    match repair {
        Repair::Firewall => Some(format!(
            "netsh advfirewall firewall delete rule name=\"Rust Mic\" >nul 2>&1\r\n\
             netsh advfirewall firewall add rule name=\"Rust Mic\" dir=in action=allow \
             program=\"{}\" enable=yes\r\n",
            exe.display()
        )),
        Repair::UsbSuspend => Some(
            "powercfg /setacvalueindex scheme_current sub_usb USBSELECTIVESUSPEND 0\r\n\
             powercfg /setdcvalueindex scheme_current sub_usb USBSELECTIVESUSPEND 0\r\n\
             powercfg /setactive scheme_current\r\n"
                .to_string(),
        ),
        Repair::AdapterPower => Some(
            "powershell -NoProfile -Command \"Get-NetAdapter -Physical | \
             Set-NetAdapterPowerManagement -AllowComputerToTurnOffDevice Disabled\"\r\n"
                .to_string(),
        ),
        _ => None,
    }
}

/// Builds the shell command for a Linux repair. Public for tests.
pub fn linux_command(repair: Repair) -> Option<String> {
    match repair {
        Repair::Firewall => {
            Some("ufw allow 8125/tcp; ufw allow 1488/tcp; ufw allow 49152/udp; echo firewall rules added".to_string())
        }
        Repair::WifiPowerSave => Some(
            "for d in /sys/class/net/*; do \
                 [ -d \"$d/wireless\" ] && iw dev \"$(basename \"$d\")\" set power_save off; \
             done; echo wi-fi power saving off"
                .to_string(),
        ),
        Repair::UsbAutosuspend => Some(
            "for f in /sys/bus/usb/devices/*/power/control; do echo on > \"$f\"; done; \
             echo usb autosuspend cleared"
                .to_string(),
        ),
        _ => None,
    }
}

/// Runs a repair, asking for elevation through the platform prompt, and
/// returns the combined output for the dialog.
pub fn run(repair: Repair) -> Result<String, String> {
    if cfg!(target_os = "windows") {
        run_windows(repair)
    } else if cfg!(target_os = "linux") {
        run_linux(repair)
    } else {
        Err("this platform is not supported".to_string())
    }
}

fn run_windows(repair: Repair) -> Result<String, String> {
    let Some(body) = windows_script(repair) else {
        return Err("this repair only exists on Linux".to_string());
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let temp = std::env::temp_dir();
    let script = temp.join(format!("rustmic-fix-{stamp}.cmd"));
    let output_path = temp.join(format!("rustmic-fix-{stamp}.out"));
    std::fs::write(&script, format!("@echo off\r\n{body}\r\n"))
        .map_err(|e| format!("could not write the script: {e}"))?;

    // The elevated cmd writes its output next to the script; PowerShell
    // only carries the UAC prompt.
    let command = format!(
        "Start-Process -FilePath cmd.exe -Verb RunAs -Wait -ArgumentList '/c','{}'",
        script.display()
    );
    let launched = Command::new("powershell")
        .args(["-NoProfile", "-Command", &command])
        .output()
        .map_err(|e| format!("could not start PowerShell: {e}"))?;
    let result = if launched.status.success() {
        match std::fs::read_to_string(&output_path) {
            Ok(text) => Ok(text.trim().to_string()),
            Err(_) => Ok("(no output; if the UAC prompt was cancelled, nothing ran)".to_string()),
        }
    } else {
        Err(format!(
            "elevation was cancelled or failed: {}",
            String::from_utf8_lossy(&launched.stderr).trim()
        ))
    };
    let _ = std::fs::remove_file(&script);
    let _ = std::fs::remove_file(&output_path);
    result
}

fn run_linux(repair: Repair) -> Result<String, String> {
    let Some(command) = linux_command(repair) else {
        return Err("this repair only exists on Windows".to_string());
    };
    let output = Command::new("pkexec")
        .args(["sh", "-c", &command])
        .output()
        .map_err(|e| {
            format!("could not start pkexec (is polkit installed?): {e}")
        })?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() {
        Ok(text.trim().to_string())
    } else {
        Err(text.trim().to_string())
    }
}

/// Holds the system awake while streaming.
///
/// Windows: `SetThreadExecutionState`, the same call the original client
/// made (visible in its imports). Linux: a `systemd-inhibit` child that
/// holds a sleep inhibitor for as long as it lives.
pub struct KeepAwake {
    #[cfg(target_os = "linux")]
    child: Option<std::process::Child>,
    #[cfg(not(target_os = "linux"))]
    _private: (),
}

impl KeepAwake {
    pub fn new() -> KeepAwake {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows_sys::Win32::System::Power::{
                SetThreadExecutionState, ES_CONTINUOUS, ES_SYSTEM_REQUIRED,
            };
            SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED);
        }
        #[cfg(target_os = "linux")]
        let child = Command::new("systemd-inhibit")
            .args(["--what=sleep:idle", "sleep", "infinity"])
            .spawn()
            .ok();
        KeepAwake {
            #[cfg(target_os = "linux")]
            child,
            #[cfg(not(target_os = "linux"))]
            _private: (),
        }
    }
}

impl Default for KeepAwake {
    fn default() -> Self {
        KeepAwake::new()
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        #[cfg(target_os = "windows")]
        unsafe {
            use windows_sys::Win32::System::Power::{SetThreadExecutionState, ES_CONTINUOUS};
            SetThreadExecutionState(ES_CONTINUOUS);
        }
        #[cfg(target_os = "linux")]
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "windows")]
    #[test]
    fn the_firewall_script_names_the_running_executable() {
        let script = windows_script(Repair::Firewall).unwrap();
        assert!(script.contains("netsh advfirewall firewall add rule"));
        assert!(script.contains("Rust Mic"));
        assert!(script.to_ascii_lowercase().contains(".exe"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn the_usb_script_touches_both_power_sources() {
        let script = windows_script(Repair::UsbSuspend).unwrap();
        assert!(script.contains("setacvalueindex"));
        assert!(script.contains("setdcvalueindex"));
        assert!(script.contains("setactive scheme_current"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_wifi_command_targets_wireless_devices_only() {
        let command = linux_command(Repair::WifiPowerSave).unwrap();
        assert!(command.contains("/wireless"));
        assert!(command.contains("set power_save off"));
    }

    #[test]
    fn availability_follows_the_platform() {
        let firewall_available = Repair::Firewall.available();
        let wifi_available = Repair::WifiPowerSave.available();
        assert_ne!(firewall_available, wifi_available);
    }
}
