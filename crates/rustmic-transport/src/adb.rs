//! USB transport.
//!
//! The WO Mic server runs on the Android phone. Over USB the client sets up
//! one port forwarding with `adb forward` and talks to it over localhost.
//! The control channel and the media channel share that single TCP
//! connection: the media reader reads its frames from the same stream the
//! control commands run on (`FUN_00404730` in `WOMicEngine.dll` reads
//! through the controller stream), using the framing the server announced.
//!
//! ```text
//! adb forward tcp:<control port> tcp:<control port>
//! ```
//!
//! The media channel of the USB transport uses the framing with the id 2.

use std::path::{Path, PathBuf};
use std::process::Command;

use rustmic_core::error::{Error, Result};

use crate::tcp::{TcpMedia, TcpTransport};

/// The channels of an open USB transport.
pub struct AdbTransport {
    /// The control channel.
    pub control: TcpTransport,
    /// The media channel, reading from the same connection.
    pub media: TcpMedia,
}

impl AdbTransport {
    /// Sets up the port forwarding and opens the shared connection.
    pub fn connect(adb_path: Option<&str>, port: u16) -> Result<AdbTransport> {
        let adb = Adb::locate(adb_path)?;

        adb.forward(port)?;

        let control = TcpTransport::connect("127.0.0.1", port)?;
        let media = control
            .stream()
            .try_clone()
            .map_err(|e| {
                Error::Io(std::io::Error::new(
                    e.kind(),
                    format!("could not duplicate the USB connection: {e}"),
                ))
            })?;

        Ok(AdbTransport { control, media: TcpMedia::new(media) })
    }
}

/// The `adb` executable.
pub struct Adb {
    program: PathBuf,
}

impl Adb {
    /// Uses the given program or searches the usual places.
    pub fn locate(explicit: Option<&str>) -> Result<Adb> {
        if let Some(path) = explicit {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Ok(Adb { program: path });
            }
            return Err(Error::Config(format!("{} is not an adb executable", path.display())));
        }

        if let Ok(path) = std::env::var("RUSTMIC_ADB") {
            let path = PathBuf::from(path);
            if path.is_file() {
                return Ok(Adb { program: path });
            }
        }

        if let Ok(current) = std::env::current_exe() {
            if let Some(directory) = current.parent() {
                let candidate = directory.join("adb.exe");
                if candidate.is_file() {
                    return Ok(Adb { program: candidate });
                }
                let candidate = directory.join("adb");
                if candidate.is_file() {
                    return Ok(Adb { program: candidate });
                }
            }
        }

        for candidate in ["adb.exe", "adb"] {
            if let Some(found) = which(candidate) {
                return Ok(Adb { program: found });
            }
        }

        Err(Error::Config(
            "adb was not found; put it in the path or point RUSTMIC_ADB at it".to_string(),
        ))
    }

    /// The path of the executable.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// Forwards a port of the device to the same port of the host.
    pub fn forward(&self, port: u16) -> Result<()> {
        let output = Command::new(&self.program)
            .args(["forward", &format!("tcp:{port}"), &format!("tcp:{port}")])
            .output()
            .map_err(|e| {
                Error::Io(std::io::Error::new(
                    e.kind(),
                    format!("could not run {}: {e}", self.program.display()),
                ))
            })?;

        if !output.status.success() {
            return Err(Error::Io(std::io::Error::other(format!(
                "adb forward failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))));
        }
        Ok(())
    }

    /// Lists the attached devices.
    pub fn devices(&self) -> Result<Vec<String>> {
        let output = Command::new(&self.program).args(["devices"]).output().map_err(|e| {
            Error::Io(std::io::Error::new(
                e.kind(),
                format!("could not run {}: {e}", self.program.display()),
            ))
        })?;
        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text
            .lines()
            .skip(1)
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.ends_with("unauthorized") || line.contains("offline") {
                    None
                } else {
                    Some(line.split_whitespace().next().unwrap_or_default().to_string())
                }
            })
            .collect())
    }
}

/// Searches the path for an executable.
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        let candidate = directory.join(program);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_path_must_exist() {
        assert!(Adb::locate(Some("definitely-not-here/adb")).is_err());
    }

    #[test]
    fn the_path_is_searched() {
        // The test only asserts that the lookup either finds an adb or
        // reports a clear error.
        match Adb::locate(None) {
            Ok(adb) => assert!(adb.program().is_file()),
            Err(error) => assert!(error.to_string().contains("adb was not found")),
        }
    }
}
