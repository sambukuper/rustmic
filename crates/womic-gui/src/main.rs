//! Desktop client in the spirit of the original WO Mic client window:
//! a Connection menu, connection fields, and a status bar that shows the
//! state on the left and the transport on the right, like the shipped
//! client's `Disconnected | Wi-Fi` bar. The connection itself runs on a
//! worker thread through the same pipeline the command line client uses.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use eframe::egui;

use womic_app::session::{self, ConnectionPlan, PipelineOptions};
use womic_app::sink::{NullSink, SampleSink, SpeakerSink};
use womic_core::error::Error;
use womic_core::settings::{Settings, TransportType};
use womic_core::stats::Statistics;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([500.0, 357.0])
            .with_min_inner_size([420.0, 300.0]),
        ..Default::default()
    };
    eframe::run_native(
        "WO Mic Client (rust-mic)",
        options,
        Box::new(|_cc| Ok(Box::new(WomicApp::new()))),
    )
}

/// What the status bar shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum State {
    Disconnected,
    Connecting,
    Connected,
    Failed(String),
}

struct WomicApp {
    // connection form
    transport: TransportType,
    host: String,
    soft_ap: String,
    control_port: u16,
    media_port: u16,
    bt_address: String,
    adb_path: String,
    buffer_size: u32,
    play_audio: bool,
    reconnect: bool,
    auto_connect: bool,

    // runtime
    state: State,
    stats: Arc<Statistics>,
    stop: Arc<AtomicBool>,
    disconnecting: bool,
    result_rx: Option<mpsc::Receiver<womic_core::Result<()>>>,
    worker: Option<JoinHandle<()>>,
    about_open: bool,
    settings_path: Option<PathBuf>,
}

impl WomicApp {
    fn new() -> WomicApp {
        let discovered = Settings::discover(exe_directory().as_deref());
        let settings = discovered.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        let mut app = WomicApp {
            transport: settings.transport_type,
            host: settings.wifi_server_addr.clone(),
            soft_ap: settings.soft_ap_addr.clone(),
            control_port: settings.control_port,
            media_port: settings.media_port,
            bt_address: settings.bt_address.clone(),
            adb_path: String::new(),
            buffer_size: settings.buffer_size,
            play_audio: settings.play_locally,
            reconnect: true,
            auto_connect: settings.auto_connect,
            state: State::Disconnected,
            stats: Arc::new(Statistics::new()),
            stop: Arc::new(AtomicBool::new(false)),
            disconnecting: false,
            result_rx: None,
            worker: None,
            about_open: false,
            settings_path: discovered.map(|(path, _)| path),
        };
        if app.auto_connect {
            // The original client had the same flag in settings.dat.
            app.connect_now();
        }
        app
    }

    fn settings(&self) -> Settings {
        Settings {
            transport_type: self.transport,
            wifi_server_addr: self.host.clone(),
            soft_ap_addr: self.soft_ap.clone(),
            play_locally: self.play_audio,
            control_port: self.control_port,
            media_port: self.media_port,
            buffer_size: self.buffer_size,
            auto_connect: self.auto_connect,
            window: womic_core::settings::WindowState::default(),
            bt_address: self.bt_address.clone(),
        }
    }

    fn save_settings(&self) {
        let path = self
            .settings_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("settings.dat"));
        let _ = self.settings().save(path);
    }

    fn busy(&self) -> bool {
        matches!(self.state, State::Connecting | State::Connected)
    }

    fn connect_now(&mut self) {
        if self.busy() {
            return;
        }
        self.save_settings();

        let plan = ConnectionPlan {
            transport: self.transport,
            host: match self.transport {
                TransportType::WifiDirect => self.soft_ap.clone(),
                _ => self.host.clone(),
            },
            control_port: self.control_port,
            media_port: self.media_port,
            bt_address: self.bt_address.clone(),
            bt_channel: None,
            adb_path: if self.adb_path.is_empty() { None } else { Some(self.adb_path.clone()) },
        };
        let options = PipelineOptions {
            buffer_size: self.buffer_size as usize,
            reconnect: self.reconnect,
            reconnect_delay: Duration::from_secs(3),
            stop: self.stop.clone(),
        };

        let (tx, rx) = mpsc::channel();
        let stats = self.stats.clone();
        let play = self.play_audio;
        self.worker = Some(std::thread::spawn(move || {
            let mut sink: Box<dyn SampleSink> = if play {
                SpeakerSink::open(None).map(|s| Box::new(s) as Box<dyn SampleSink>).unwrap_or_else(|_| Box::new(NullSink))
            } else {
                Box::new(NullSink)
            };
            let _ = tx.send(session::run(&plan, &options, sink.as_mut(), &stats));
        }));
        self.result_rx = Some(rx);
        self.state = State::Connecting;
    }

    fn disconnect_now(&mut self) {
        if !self.busy() {
            return;
        }
        self.disconnecting = true;
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Collects what the worker reported and derives the visible state.
    fn poll_worker(&mut self) {
        let finished = match &self.result_rx {
            Some(rx) => rx.try_recv().ok(),
            None => None,
        };
        if let Some(result) = finished {
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
            self.result_rx = None;
            self.state = if self.disconnecting {
                State::Disconnected
            } else {
                match result {
                    Ok(()) => State::Disconnected,
                    Err(error) => State::Failed(error.to_string()),
                }
            };
            self.disconnecting = false;
        } else if self.stats.packets_received.load(Ordering::Relaxed) > 0 {
            self.state = State::Connected;
        }
    }

    fn status_text(&self) -> String {
        match &self.state {
            State::Disconnected => "Disconnected".to_string(),
            State::Connecting => "Connecting...".to_string(),
            State::Connected => {
                let s = self.stats.snapshot();
                format!(
                    "Connected: {} packets, {} ms average latency",
                    s.packets_received,
                    s.average_latency.as_millis()
                )
            }
            State::Failed(error) => format!("Failed: {error}"),
        }
    }

    fn connection_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Connection", |ui| {
            let connected = self.busy();
            if ui.add_enabled(!connected, egui::Button::new("Connect")).clicked() {
                ui.close_menu();
                self.connect_now();
            }
            if ui.add_enabled(connected, egui::Button::new("Disconnect")).clicked() {
                ui.close_menu();
                self.disconnect_now();
            }
            ui.separator();
            if ui.button("Exit").clicked() {
                ui.close_menu();
                self.disconnect_now();
                self.save_settings();
                ctx_of(ui).send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }

    fn options_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Options", |ui| {
            ui.checkbox(&mut self.play_audio, "Play audio on this computer");
            ui.checkbox(&mut self.reconnect, "Reconnect after a broken stream");
            ui.checkbox(&mut self.auto_connect, "Connect when the client starts");
            ui.separator();
            if ui.button("Save settings now").clicked() {
                ui.close_menu();
                self.save_settings();
            }
        });
    }

    fn help_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("Help", |ui| {
            if ui.button("About").clicked() {
                ui.close_menu();
                self.about_open = true;
            }
        });
    }
}

impl eframe::App for WomicApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker();
        ctx.request_repaint_after(Duration::from_millis(250));

        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                self.connection_menu(ui);
                self.options_menu(ui);
                self.help_menu(ui);
            });
        });

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let text = self.status_text();
                let color = match self.state {
                    State::Connected => egui::Color32::from_rgb(0x2e, 0x8b, 0x57),
                    State::Failed(_) => egui::Color32::from_rgb(0xb0, 0x30, 0x30),
                    _ => ui.style().visuals.text_color(),
                };
                ui.label(egui::RichText::new(text).color(color));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(self.transport.short_name());
                });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            egui::Grid::new("connection_form")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Transport");
                    egui::ComboBox::from_id_salt("transport")
                        .selected_text(self.transport.short_name())
                        .width(160.0)
                        .show_ui(ui, |ui| {
                            for candidate in [
                                TransportType::Wifi,
                                TransportType::WifiDirect,
                                TransportType::Usb,
                                TransportType::Bluetooth,
                            ] {
                                ui.selectable_value(
                                    &mut self.transport,
                                    candidate,
                                    candidate.short_name(),
                                );
                            }
                        });
                    match self.transport {
                        TransportType::Wifi | TransportType::WifiDirect => {
                            let (label, value) = if self.transport == TransportType::Wifi {
                                ("Server address", &mut self.host)
                            } else {
                                ("Hotspot address", &mut self.soft_ap)
                            };
                            ui.label(label);
                            ui.add(egui::TextEdit::singleline(value).desired_width(220.0));
                            ui.label("Media port (UDP)");
                            ui.add(egui::DragValue::new(&mut self.media_port).range(1..=65535));
                        }
                        TransportType::Usb => {
                            ui.label("adb path (empty = search)");
                            ui.add(egui::TextEdit::singleline(&mut self.adb_path).desired_width(220.0));
                        }
                        TransportType::Bluetooth => {
                            ui.label("Device address");
                            ui.add(egui::TextEdit::singleline(&mut self.bt_address).desired_width(220.0));
                        }
                    }
                    ui.label("Control port (TCP)");
                    ui.add(egui::DragValue::new(&mut self.control_port).range(1..=65535));
                    ui.label("Jitter buffer (packets)");
                    ui.add(egui::DragValue::new(&mut self.buffer_size).range(1..=20));
                    ui.end_row();
                });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if self.busy() {
                    if ui.button("Disconnect").clicked() {
                        self.disconnect_now();
                    }
                } else if ui.add(egui::Button::new(egui::RichText::new("Connect").strong())).clicked() {
                    self.connect_now();
                }
            });

            if matches!(self.state, State::Connected) {
                ui.add_space(12.0);
                ui.separator();
                let s = self.stats.snapshot();
                egui::Grid::new("stats").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                    ui.label("Uptime");
                    ui.label(format!("{} s", s.uptime.as_secs()));
                    ui.label("Packets received / played");
                    ui.label(format!("{} / {}", s.packets_received, s.packets_played));
                    ui.label("Dropped / lost");
                    ui.label(format!("{} / {}", s.packets_dropped, s.packets_lost));
                    ui.label("Decode errors");
                    ui.label(format!("{}", s.decode_errors));
                    ui.label("Latency (average / max)");
                    ui.label(format!(
                        "{} ms / {} ms",
                        s.average_latency.as_millis(),
                        s.max_latency.as_millis()
                    ));
                    ui.label("Bytes received");
                    ui.label(format!("{} KiB", s.bytes_received / 1024));
                    ui.end_row();
                });
            }
        });

        if self.about_open {
            egui::Window::new("About")
                .open(&mut self.about_open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("rust-mic, a WO Mic client rewritten in Rust.");
                    ui.label("Protocol recovered from the shipped Windows client;");
                    ui.label("see docs/ANALYSIS.md for the evidence.");
                    ui.add_space(4.0);
                    ui.label("GPL-3.0");
                });
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.disconnect_now();
        self.save_settings();
    }
}

/// The egui context, reachable from inside a menu callback.
fn ctx_of(ui: &mut egui::Ui) -> &egui::Context {
    ui.ctx()
}

fn exe_directory() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}

/// Silence the unused-variant warning for errors the GUI maps only through
/// their Display form.
#[allow(dead_code)]
fn _error_shape(error: &Error) -> i32 {
    error.code()
}
