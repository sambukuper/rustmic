//! Command line WO Mic client.
//!
//! Usage mirrors the settings of the original Windows client: the values
//! come from a `settings.dat` file (the same format the shipped client
//! reads) and every value can be overridden on the command line. Exit
//! codes are the numeric codes of the original client, so scripts can
//! tell a version mismatch (-7) from a refused connection (-6).

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;

use womic_app::session;
use womic_app::sink;
use womic_core::error::Error;
use womic_core::settings::{Settings, TransportType};
use womic_core::stats::Statistics;

use session::{ConnectionPlan, PipelineOptions};
use sink::{NullSink, SampleSink, SpeakerSink, StdoutSink};

/// Connects to a WO Mic server and plays the microphone stream of the
/// phone on this computer.
#[derive(Debug, Parser)]
#[command(name = "womic", version, about)]
struct Args {
    /// Transport: wifi, wifi-direct, usb or bluetooth.
    #[arg(long)]
    transport: Option<String>,

    /// Address of the WO Mic server (Wi-Fi transports).
    #[arg(long)]
    host: Option<String>,

    /// TCP port of the control channel.
    #[arg(long)]
    control_port: Option<u16>,

    /// UDP port of the media channel.
    #[arg(long)]
    media_port: Option<u16>,

    /// Bluetooth address of the server (bluetooth transport).
    #[arg(long)]
    bt_address: Option<String>,

    /// RFCOMM channel to dial directly instead of scanning 1..=30.
    #[arg(long)]
    bt_channel: Option<u8>,

    /// Path of the adb executable (usb transport).
    #[arg(long)]
    adb: Option<String>,

    /// Jitter buffer depth in packets (original bufferSize).
    #[arg(long)]
    buffer: Option<u32>,

    /// Read the original settings.dat from this path.
    #[arg(long)]
    settings: Option<PathBuf>,

    /// Write decoded PCM (16 bit little endian mono 48 kHz) to stdout
    /// instead of playing it.
    #[arg(long)]
    stdout: bool,

    /// Substring of the audio output device to play on.
    #[arg(long)]
    device: Option<String>,

    /// Do not play anything; useful to watch the statistics only.
    #[arg(long)]
    no_play: bool,

    /// Reconnect after a broken stream; 0 disables reconnecting.
    #[arg(long, default_value_t = 3)]
    reconnect: u64,

    /// Print statistics every N seconds; 0 disables printing.
    #[arg(long, default_value_t = 10)]
    stats_interval: u64,

    /// List the audio output devices and exit.
    #[arg(long)]
    list_devices: bool,

    /// List the paired Bluetooth devices and exit.
    #[arg(long)]
    list_bt_devices: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();

    if args.list_devices {
        for device in sink::output_device_names() {
            println!("{device}");
        }
        return ExitCode::SUCCESS;
    }
    if args.list_bt_devices {
        return match womic_transport::BluetoothTransport::paired_devices() {
            Ok(devices) => {
                for device in devices {
                    println!("{:18} {}", device.address, device.name);
                }
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(error_code(&error))
            }
        };
    }

    let settings = match &args.settings {
        Some(path) => match Settings::load(path) {
            Ok(settings) => settings,
            Err(error) => {
                eprintln!("could not read {}: {error}", path.display());
                return ExitCode::from(error_code(&error));
            }
        },
        None => Settings::discover(exe_directory().as_deref())
            .map(|(_, settings)| settings)
            .unwrap_or_default(),
    };

    let transport = match &args.transport {
        Some(name) => match TransportType::from_name(name) {
            Ok(transport) => transport,
            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(error_code(&error));
            }
        },
        None => settings.transport_type,
    };

    let plan = ConnectionPlan {
        transport,
        host: args.host.clone().unwrap_or_else(|| settings.server_address().to_string()),
        control_port: args.control_port.unwrap_or(settings.control_port),
        media_port: args.media_port.unwrap_or(settings.media_port),
        bt_address: args.bt_address.clone().unwrap_or_else(|| settings.bt_address.clone()),
        bt_channel: args.bt_channel,
        adb_path: args.adb.clone(),
    };

    let options = PipelineOptions {
        buffer_size: args.buffer.map(|n| n as usize).unwrap_or(settings.buffer_size as usize),
        reconnect: args.reconnect > 0,
        reconnect_delay: Duration::from_secs(args.reconnect.max(1)),
        stop: Arc::new(AtomicBool::new(false)),
    };

    if args.bt_address.is_some() {
        eprintln!(
            "warning: --bt-address overrides settings.dat; the phone must be paired"
        );
    }

    let mut sink: Box<dyn SampleSink> = if args.stdout {
        Box::new(StdoutSink::new())
    } else if args.no_play {
        Box::new(NullSink)
    } else {
        match SpeakerSink::open(args.device.as_deref()) {
            Ok(sink) => Box::new(sink),
            Err(error) => {
                eprintln!("could not open the audio output: {error}");
                return ExitCode::from(error_code(&error));
            }
        }
    };

    let stop = options.stop.clone();
    ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed))
        .expect("could not install the Ctrl+C handler");

    let stats = Arc::new(Statistics::new());
    let stats_stop = options.stop.clone();
    let thread_stats = stats.clone();
    let stats_thread = std::thread::spawn(move || {
        if args.stats_interval == 0 {
            return;
        }
        let interval = Duration::from_secs(args.stats_interval.max(1));
        loop {
            for _ in 0..interval.as_millis() / 100 {
                if stats_stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            eprintln!("{}", thread_stats.snapshot());
        }
    });

    eprintln!(
        "connecting over {} to {}:{} (output: {})",
        plan.transport,
        if plan.transport == TransportType::Bluetooth { plan.bt_address.as_str() } else { plan.host.as_str() },
        plan.control_port,
        sink.name()
    );

    let outcome = session::run(&plan, &options, sink.as_mut(), &stats);
    options.stop.store(true, Ordering::Relaxed);
    let _ = stats_thread.join();
    print_final(&stats);

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::from(error_code(&error))
        }
    }
}

fn print_final(stats: &Statistics) {
    eprintln!("final: {}", stats.snapshot());
}

fn exe_directory() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(Path::to_path_buf)
}

fn error_code(error: &Error) -> u8 {
    let code = error.code();
    let positive = if code < 0 { -code } else { code };
    (positive & 0xff) as u8
}
