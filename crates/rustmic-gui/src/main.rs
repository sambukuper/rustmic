//! Desktop client in the spirit of the original WO Mic client window.
//!
//! The menu layout, dialog texts and the transport tips come from the
//! resources of the shipped `WOMicEngine.dll` (menu 109, string table 7,
//! the Connect/Ports/Buffer/Statistics/About dialog templates). The
//! dialogs are embedded in the client area of the main window, and the
//! connection runs on a worker thread through the same pipeline the
//! command line client uses.

// A windowed application: no console window on release builds.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use eframe::egui;

use rustmic_app::session::{self, ConnectionPlan, PipelineOptions};
use rustmic_app::sink::{NullSink, SampleSink, SpeakerSink};
use rustmic_core::settings::{Settings, TransportType};
use rustmic_core::stats::Statistics;
use rustmic_transport::bt::BtDeviceInfo;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([500.0, 357.0])
            .with_min_inner_size([420.0, 300.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Rust Mic Client",
        options,
        Box::new(|_cc| Ok(Box::new(RustMicApp::new()))),
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

/// The dialog currently embedded in the client area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialog {
    Connect,
    Ports,
    Buffer,
    Statistics,
    About,
}

/// The tips texts of the original client, string table 7 of
/// `WOMicEngine.dll`.
fn tips(transport: TransportType) -> &'static str {
    match transport {
        TransportType::Bluetooth => {
            "Tips:\n1. Turn on Bluetooth on phone and PC. \n2. Pair phone and PC if you have not.\n3. Select Bluetooth as transport on phone and start the server.\n4. Select phone as target device in above drop box."
        }
        TransportType::Usb => {
            "Tips:\n1. Connect phone and PC by USB cable. \n2. Install correct USB driver for your phone if not yet. \nNOTE: Your phone should appear as Android device in Device Manager.\n3. Select USB as transport on phone and start the server."
        }
        TransportType::Wifi => {
            "Tips:\n1. Connect phone and PC to same Wi-Fi AP (hot spot). \n2. Select Wi-Fi as transport on phone and start the server. It shows phone IP address at panel top.\n3. Enter that IP address into above edit box."
        }
        TransportType::WifiDirect => {
            "NOTE: The following steps disconnect your PC from existing Internet connection through Wi-Fi\n\nTips:\n1. Enable Soft AP (or portable Wi-Fi hotspot) on your phone. \n2. Connect your PC Wi-Fi to this Soft AP.\n3. Select Wi-Fi Direct as transport on phone and start the server.\n4. Use '192.168.43.1' as soft AP address unless you know otherwise.\n\nYou can refer to http://www.wolicheng.com/womic/faq.html on how to find Soft AP address."
        }
    }
}

struct RustMicApp {
    // connection settings, mirrored into settings.dat
    transport: TransportType,
    host: String,
    soft_ap: String,
    control_port: u16,
    media_port: u16,
    bt_address: String,
    buffer_size: u32,
    play_in_speaker: bool,
    reconnect: bool,
    auto_connect: bool,

    // runtime
    state: State,
    stats: Arc<Statistics>,
    stop: Arc<AtomicBool>,
    disconnecting: bool,
    result_rx: Option<mpsc::Receiver<rustmic_core::Result<()>>>,
    worker: Option<JoinHandle<()>>,

    // dialog state
    dialog: Option<Dialog>,
    connect_transport: TransportType,
    connect_host: String,
    connect_soft_ap: String,
    connect_bt_address: String,
    bt_devices: Vec<BtDeviceInfo>,

    settings_path: Option<PathBuf>,
}

impl RustMicApp {
    fn new() -> RustMicApp {
        let discovered = Settings::discover(exe_directory().as_deref());
        let settings = discovered.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        let mut app = RustMicApp {
            transport: settings.transport_type,
            host: settings.wifi_server_addr.clone(),
            soft_ap: settings.soft_ap_addr.clone(),
            control_port: settings.control_port,
            media_port: settings.media_port,
            bt_address: settings.bt_address.clone(),
            buffer_size: settings.buffer_size.min(10).max(1),
            play_in_speaker: settings.play_locally,
            reconnect: settings.auto_connect,
            auto_connect: settings.auto_connect,
            state: State::Disconnected,
            stats: Arc::new(Statistics::new()),
            stop: Arc::new(AtomicBool::new(false)),
            disconnecting: false,
            result_rx: None,
            worker: None,
            dialog: None,
            connect_transport: settings.transport_type,
            connect_host: settings.wifi_server_addr.clone(),
            connect_soft_ap: settings.soft_ap_addr.clone(),
            connect_bt_address: settings.bt_address.clone(),
            bt_devices: Vec::new(),
            settings_path: discovered.map(|(path, _)| path),
        };
        if app.auto_connect {
            // The original client starts a connection on the same flag.
            app.start_connection();
        }
        if let Some(which) = std::env::var_os("RUSTMIC_TEST_OPEN") {
            // Test hook: open one dialog right away so a scripted
            // screenshot can verify the layout.
            match which.to_string_lossy().as_ref() {
                "connect" => app.open_connect_dialog(),
                "ports" => app.dialog = Some(Dialog::Ports),
                "buffer" => app.dialog = Some(Dialog::Buffer),
                "stats" => app.dialog = Some(Dialog::Statistics),
                "about" => app.dialog = Some(Dialog::About),
                _ => {}
            }
        }
        app
    }

    fn settings(&self) -> Settings {
        Settings {
            transport_type: self.transport,
            wifi_server_addr: self.host.clone(),
            soft_ap_addr: self.soft_ap.clone(),
            play_locally: self.play_in_speaker,
            control_port: self.control_port,
            media_port: self.media_port,
            buffer_size: self.buffer_size,
            auto_connect: self.auto_connect,
            window: rustmic_core::settings::WindowState::default(),
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

    /// Applies the Connect dialog and spawns the worker.
    fn start_connection(&mut self) {
        if self.busy() {
            return;
        }
        self.transport = self.connect_transport;
        self.host = self.connect_host.clone();
        self.soft_ap = self.connect_soft_ap.clone();
        self.bt_address = self.connect_bt_address.clone();
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
            adb_path: None,
        };
        let options = PipelineOptions {
            buffer_size: self.buffer_size as usize,
            reconnect: self.reconnect,
            reconnect_delay: Duration::from_secs(3),
            stop: self.stop.clone(),
        };

        let (tx, rx) = mpsc::channel();
        let stats = self.stats.clone();
        let play = self.play_in_speaker;
        self.worker = Some(std::thread::spawn(move || {
            let mut sink: Box<dyn SampleSink> = if play {
                SpeakerSink::open(None)
                    .map(|s| Box::new(s) as Box<dyn SampleSink>)
                    .unwrap_or_else(|_| Box::new(NullSink))
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

    /// Loads the paired device list for the Bluetooth drop box, the way the
    /// original dialog fills its combo when the transport is selected.
    fn load_bt_devices(&mut self) {
        self.bt_devices = rustmic_transport::BluetoothTransport::paired_devices().unwrap_or_default();
    }

    /// Opens a dialog with the values the connection would use right now.
    fn open_connect_dialog(&mut self) {
        self.connect_transport = self.transport;
        self.connect_host = self.host.clone();
        self.connect_soft_ap = self.soft_ap.clone();
        self.connect_bt_address = self.bt_address.clone();
        if self.connect_transport == TransportType::Bluetooth {
            self.load_bt_devices();
        }
        self.dialog = Some(Dialog::Connect);
    }
}

impl eframe::App for RustMicApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker();
        ctx.request_repaint_after(Duration::from_millis(250));

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.dialog = None;
        }

        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("Connection", |ui| {
                    let connected = self.busy();
                    if ui.add_enabled(!connected, egui::Button::new("Connect...")).clicked() {
                        ui.close_menu();
                        self.open_connect_dialog();
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
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("Options", |ui| {
                    ui.checkbox(&mut self.play_in_speaker, "Play in speaker");
                    ui.checkbox(&mut self.reconnect, "Auto reconnect");
                    ui.separator();
                    if ui.button("Ports...").clicked() {
                        ui.close_menu();
                        self.dialog = Some(Dialog::Ports);
                    }
                    if ui.button("Buffer size...").clicked() {
                        ui.close_menu();
                        self.dialog = Some(Dialog::Buffer);
                    }
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("Statistics").clicked() {
                        ui.close_menu();
                        self.dialog = Some(Dialog::Statistics);
                    }
                    if ui.button("About ...").clicked() {
                        ui.close_menu();
                        self.dialog = Some(Dialog::About);
                    }
                });
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

        egui::CentralPanel::default().show(ctx, |ui| match self.dialog {
            None => {
                if matches!(self.state, State::Connected) {
                    ui.add_space(8.0);
                    ui.label(self.status_text());
                }
            }
            Some(Dialog::Connect) => {
                let close = dialog_frame(ui, "Connect", |ui| self.connect_dialog(ui));
                if close {
                    self.dialog = None;
                }
            }
            Some(Dialog::Ports) => {
                let close = dialog_frame(ui, "Change ports", |ui| self.ports_dialog(ui));
                if close {
                    self.dialog = None;
                }
            }
            Some(Dialog::Buffer) => {
                let close = dialog_frame(ui, "Change buffer size", |ui| self.buffer_dialog(ui));
                if close {
                    self.dialog = None;
                }
            }
            Some(Dialog::Statistics) => {
                let close = dialog_frame(ui, "Statistics", |ui| self.statistics_dialog(ui));
                if close {
                    self.dialog = None;
                }
            }
            Some(Dialog::About) => {
                let close = dialog_frame(ui, "About", |ui| self.about_dialog(ui));
                if close {
                    self.dialog = None;
                }
            }
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.disconnect_now();
        self.save_settings();
    }
}

/// A dialog embedded in the client area: title and close button on top,
/// content below, like the client area sized dialogs of the original.
/// Returns true when the close button was clicked.
fn dialog_frame(
    ui: &mut egui::Ui,
    title: &str,
    content: impl FnOnce(&mut egui::Ui),
) -> bool {
    let mut close = false;
    ui.add_space(6.0);
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_min_width(ui.available_width() - 12.0);
        ui.horizontal(|ui| {
            ui.strong(title);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("X").clicked() {
                    close = true;
                }
            });
        });
        ui.separator();
        content(ui);
    });
    close
}

impl RustMicApp {
    /// The Connect dialog: transport radios stacked in the left group, per
    /// transport details and the original tips in the right group, OK and
    /// Cancel centered at the bottom, like the original dialog.
    fn connect_dialog(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_min_width(130.0);
                        ui.set_min_height(150.0);
                        ui.strong("Transport type");
                        ui.add_space(6.0);
                        for candidate in [
                            TransportType::Bluetooth,
                            TransportType::Usb,
                            TransportType::Wifi,
                            TransportType::WifiDirect,
                        ] {
                            if ui
                                .radio(
                                    &self.connect_transport == &candidate,
                                    transport_label(candidate),
                                )
                                .clicked()
                            {
                                self.connect_transport = candidate;
                                if candidate == TransportType::Bluetooth
                                    && self.bt_devices.is_empty()
                                {
                                    self.load_bt_devices();
                                }
                            }
                        }
                    });
                });

                ui.vertical(|ui| {
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_min_width(ui.available_width() - 16.0);
                        ui.set_min_height(150.0);
                        ui.strong("Details");
                        ui.add_space(6.0);
                        match self.connect_transport {
                            TransportType::Bluetooth => {
                                ui.label("Target device:");
                                let selected = self
                                    .bt_devices
                                    .iter()
                                    .find(|d| d.address == self.connect_bt_address)
                                    .map(|d| format!("{} ({})", d.name, d.address))
                                    .unwrap_or_else(|| "Select a paired phone...".to_string());
                                egui::ComboBox::from_id_salt("bt_device")
                                    .selected_text(selected)
                                    .width(240.0)
                                    .show_ui(ui, |ui| {
                                        for device in &self.bt_devices {
                                            let text =
                                                format!("{} ({})", device.name, device.address);
                                            ui.selectable_value(
                                                &mut self.connect_bt_address,
                                                device.address.clone(),
                                                text,
                                            );
                                        }
                                    });
                                if self.bt_devices.is_empty() {
                                    ui.label("No paired devices found.");
                                }
                            }
                            TransportType::Wifi => {
                                ui.label("Server IP address:");
                                ui.add(egui::TextEdit::singleline(&mut self.connect_host)
                                    .desired_width(160.0));
                            }
                            TransportType::WifiDirect => {
                                ui.label("Soft AP address:");
                                ui.add(egui::TextEdit::singleline(&mut self.connect_soft_ap)
                                    .desired_width(160.0));
                            }
                            TransportType::Usb => {}
                        }
                        ui.add_space(12.0);
                        ui.label(egui::RichText::new("Tips:").small().strong());
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(tips(self.connect_transport)).small(),
                            )
                            .wrap(),
                        );
                    });
                });
            });

            // Pin the buttons to the bottom of the client area.
            let left = ui.available_height() - 46.0;
            if left > 0.0 {
                ui.add_space(left);
            }
            match dialog_buttons(ui) {
                Some(true) => {
                    self.dialog = None;
                    self.start_connection();
                }
                Some(false) => self.dialog = None,
                None => {}
            }
        });
    }

    fn ports_dialog(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        egui::Grid::new("ports_grid").num_columns(2).spacing([8.0, 8.0]).show(ui, |ui| {
            ui.label("Control port:");
            ui.add(egui::DragValue::new(&mut self.control_port).range(1..=65535));
            ui.label("Media port:");
            ui.add(egui::DragValue::new(&mut self.media_port).range(1..=65535));
            ui.end_row();
        });
        ui.add_space(8.0);
        match dialog_buttons(ui) {
            Some(true) => {
                self.save_settings();
                self.dialog = None;
            }
            Some(false) => self.dialog = None,
            None => {}
        }
    }

    fn buffer_dialog(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label("Buffer size (1 - 10):");
        ui.add(egui::DragValue::new(&mut self.buffer_size).range(1..=10));
        ui.add_space(8.0);
        match dialog_buttons(ui) {
            Some(true) => {
                self.save_settings();
                self.dialog = None;
            }
            Some(false) => self.dialog = None,
            None => {}
        }
    }

    fn statistics_dialog(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        let s = self.stats.snapshot();
        egui::Grid::new("stats_grid")
            .num_columns(2)
            .spacing([24.0, 5.0])
            .show(ui, |ui| {
                for (label, value) in [
                    ("Packets received", format!("{}", s.packets_received)),
                    ("Packets played", format!("{}", s.packets_played)),
                    ("Packets dropped", format!("{}", s.packets_dropped)),
                    ("Packets lost", format!("{}", s.packets_lost)),
                    ("Decode errors", format!("{}", s.decode_errors)),
                    ("Bytes received", format!("{} KiB", s.bytes_received / 1024)),
                    ("Average latency", format!("{} ms", s.average_latency.as_millis())),
                    ("Maximum latency", format!("{} ms", s.max_latency.as_millis())),
                    ("Reconnects", format!("{}", s.reconnects)),
                    ("Uptime", format!("{} s", s.uptime.as_secs())),
                ] {
                    ui.label(label);
                    ui.label(value);
                    ui.end_row();
                }
            });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Copy to clipboard").clicked() {
                ui.output_mut(|o| o.copied_text = s.to_string());
            }
            if ui.button("Cancel").clicked() {
                self.dialog = None;
            }
        });
    }

    fn about_dialog(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label("Rust Mic Client, a WO Mic client rewritten in Rust");
        ui.label("Version: 0.1.0");
        ui.label("Protocol recovered from the shipped Windows client;");
        ui.label("see docs/ANALYSIS.md for the evidence.");
        ui.add_space(4.0);
        ui.hyperlink_to("http://www.wolicheng.com/womic", "http://www.wolicheng.com/womic");
        ui.add_space(8.0);
        if dialog_buttons(ui).is_some() {
            self.dialog = None;
        }
    }
}

/// OK / Cancel centered at the bottom of a dialog. Some(true) means OK,
/// Some(false) means Cancel, nothing was clicked yet otherwise.
fn dialog_buttons(ui: &mut egui::Ui) -> Option<bool> {
    let mut clicked = None;
    ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new("OK").min_size([80.0, 0.0].into())).clicked() {
                clicked = Some(true);
            }
            if ui
                .add(egui::Button::new("Cancel").min_size([80.0, 0.0].into()))
                .clicked()
            {
                clicked = Some(false);
            }
        });
    });
    clicked
}

fn transport_label(transport: TransportType) -> &'static str {
    match transport {
        TransportType::Bluetooth => "Bluetooth",
        TransportType::Usb => "USB",
        TransportType::Wifi => "Wi-Fi",
        TransportType::WifiDirect => "Wi-Fi Direct",
    }
}

fn exe_directory() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}
