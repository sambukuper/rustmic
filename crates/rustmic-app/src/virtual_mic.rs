//! The Linux virtual microphone: a PulseAudio / PipeWire source fed by
//! the client stream.
//!
//! This is the Linux counterpart of the WO Mic Device driver on Windows:
//! a `module-pipe-source` named `rustmic` shows up as a microphone in
//! every application, and the client writes the decoded PCM into the
//! pipe backing it. Requires `pactl` (present on every PulseAudio and
//! PipeWire desktop).

use std::fs::OpenOptions;
use std::path::PathBuf;
use std::process::Command;

use rustmic_core::error::{Error, Result};
use rustmic_core::stats::Statistics;

use crate::session::PipelineOptions;
use crate::sink::FileSink;

const FIFO_PATH: &str = "/tmp/rustmic-mic.pcm";
const SOURCE_NAME: &str = "rustmic";

/// Loads the pipe source module and returns its module index.
fn load_source(fifo: &str) -> Result<u32> {
    let output = Command::new("pactl")
        .args([
            "load-module",
            "module-pipe-source",
            &format!("source_name={SOURCE_NAME}"),
            &format!("file={fifo}"),
            "format=s16le",
            "rate=48000",
            "channels=1",
            "source_properties=device.description=Rust Mic (virtual microphone)",
        ])
        .output()
        .map_err(|e| {
            Error::Config(format!(
                "could not start pactl (is PulseAudio or PipeWire installed?): {e}"
            ))
        })?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(Error::Config(format!("pactl load-module failed: {}", text.trim())));
    }
    let index = text
        .trim()
        .split_whitespace()
        .find_map(|token| token.parse::<u32>().ok())
        .ok_or_else(|| Error::Config(format!("pactl printed no module index: {}", text.trim())))?;
    Ok(index)
}

fn unload_source(index: u32) {
    let _ = Command::new("pactl").args(["unload-module", &index.to_string()]).output();
}

/// Sets up the virtual microphone, runs the connection with the decoded
/// stream written into it, and tears the module down afterwards.
pub fn run(
    plan: &crate::session::ConnectionPlan,
    options: &PipelineOptions,
    stats: &Statistics,
) -> Result<()> {
    let fifo = PathBuf::from(FIFO_PATH);
    let _ = std::fs::remove_file(&fifo);

    let path = std::ffi::CString::new(FIFO_PATH).expect("fifo path");
    let created = unsafe { libc::mkfifo(path.as_ptr(), 0o660) };
    if created != 0 {
        return Err(Error::Io(std::io::Error::last_os_error()));
    }

    let index = load_source(FIFO_PATH)?;
    eprintln!(
        "virtual microphone '{SOURCE_NAME}' is active; applications can record from it now"
    );

    let result = (|| -> Result<()> {
        // The pipe source holds the read end open, so this returns
        // immediately; when an application stops recording the writes
        // simply block, which slows the pipeline down without breaking it.
        let file = OpenOptions::new().write(true).open(&fifo)?;
        let mut sink = FileSink::new(file);
        crate::session::run(plan, options, &mut sink, stats)
    })();

    unload_source(index);
    let _ = std::fs::remove_file(&fifo);
    eprintln!("virtual microphone unloaded");
    result
}
