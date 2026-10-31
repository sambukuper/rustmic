//! Desktop client in the spirit of the original WO Mic client window.
//!
//! The menu layout, dialog texts and the transport tips come from the
//! resources of the shipped `WOMicEngine.dll` (menu 109, string table 7,
//! the Connect/Ports/Buffer/Statistics/About dialog templates), so the
//! window looks and reads like the original. The connection runs on a
//! worker thread through the same pipeline the command line client uses.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

use eframe::egui;

use womic_app::session::{self, ConnectionPlan, PipelineOptions};
use womic_app::sink::{NullSink, SampleSink, SpeakerSink};
use womic_core::settings::{Settings, TransportType};
use womic_core::stats::Statistics;
use womic_transport::bt::BtDeviceInfo;

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

struct WomicApp {
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
    result_rx: Option<mpsc::Receiver<womic_core::Result<()>>>,
    worker: Option<JoinHandle<()>>,

    // dialogs
    connect_open: bool,
    connect_transport: TransportType,
    connect_host: String,
    connect_soft_ap: String,
    connect_bt_address: String,
    bt_devices: Vec<BtDeviceInfo>,
    ports_open: bool,
    buffer_open: bool,
    stats_open: bool,
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
            connect_open: false,
            connect_transport: settings.transport_type,
            connect_host: settings.wifi_server_addr.clone(),
            connect_soft_ap: settings.soft_ap_addr.clone(),
            connect_bt_address: settings.bt_address.clone(),
            bt_devices: Vec::new(),
            ports_open: false,
            buffer_open: false,
            stats_open: false,
            about_open: false,
            settings_path: discovered.map(|(path, _)| path),
        };
        if app.auto_connect {
            // The original client starts a connection on the same flag.
            app.start_connection();
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
        self.bt_devices = womic_transport::BluetoothTransport::paired_devices().unwrap_or_default();
    }
}

impl eframe::App for WomicApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker();
        ctx.request_repaint_after(Duration::from_millis(250));

        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                ui.menu_button("&Connection", |ui| {
                    let connected = self.busy();
                    if ui.add_enabled(!connected, egui::Button::new("&Connect...")).clicked() {
                        ui.close_menu();
                        self.connect_open = true;
                        if self.connect_transport == TransportType::Bluetooth {
                            self.load_bt_devices();
                        }
                    }
                    if ui.add_enabled(connected, egui::Button::new("&Disconnect")).clicked() {
                        ui.close_menu();
                        self.disconnect_now();
                    }
                    ui.separator();
                    if ui.button("E&xit").clicked() {
                        ui.close_menu();
                        self.disconnect_now();
                        self.save_settings();
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("&Options", |ui| {
                    ui.checkbox(&mut self.play_in_speaker, "&Play in speaker");
                    ui.checkbox(&mut self.reconnect, "&Auto reconnect");
                    ui.separator();
                    if ui.button("&Ports...").clicked() {
                        ui.close_menu();
                        self.ports_open = true;
                    }
                    if ui.button("&Buffer size...").clicked() {
                        ui.close_menu();
                        self.buffer_open = true;
                    }
                });
                ui.menu_button("&Help", |ui| {
                    if ui.button("&Statistics").clicked() {
                        ui.close_menu();
                        self.stats_open = true;
                    }
                    if ui.button("&About ...").clicked() {
                        ui.close_menu();
                        self.about_open = true;
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

        // The original client area is empty; everything happens through the
        // Connect dialog.
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            if matches!(self.state, State::Connected) {
                ui.label(self.status_text());
            }
        });

        self.show_connect_dialog(ctx);
        self.show_ports_dialog(ctx);
        self.show_buffer_dialog(ctx);
        self.show_stats_dialog(ctx);
        self.show_about_dialog(ctx);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.disconnect_now();
        self.save_settings();
    }
}

impl WomicApp {
    fn show_connect_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.connect_open;
        egui::Window::new("Connect")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Transport type group, radios like the original dialog.
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_min_width(110.0);
                        ui.label("Transport type");
                        for candidate in [
                            TransportType::Bluetooth,
                            TransportType::Usb,
                            TransportType::Wifi,
                            TransportType::WifiDirect,
                        ] {
                            if ui
                                .radio(&self.connect_transport == &candidate, transport_label(candidate))
                                .clicked()
                            {
                                self.connect_transport = candidate;
                                if candidate == TransportType::Bluetooth && self.bt_devices.is_empty() {
                                    self.load_bt_devices();
                                }
                            }
                        }
                    });

                    ui.vertical(|ui| {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.set_min_width(300.0);
                            ui.label("Details");
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
                                        .width(280.0)
                                        .show_ui(ui, |ui| {
                                            for device in &self.bt_devices {
                                                let text = format!("{} ({})", device.name, device.address);
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
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.connect_host)
                                            .desired_width(160.0),
                                    );
                                }
                                TransportType::WifiDirect => {
                                    ui.label("Soft AP address:");
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.connect_soft_ap)
                                            .desired_width(160.0),
                                    );
                                }
                                TransportType::Usb => {}
                            }
                            ui.add_space(4.0);
                            ui.set_min_height(96.0);
                            ui.small(tips(self.connect_transport));
                        });
                    });
                });

                ui.add_space(8.0);
                ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                    ui.horizontal(|ui| {
                        if ui.add(egui::Button::new("OK").min_size([80.0, 0.0].into())).clicked() {
                            self.connect_open = false;
                            self.start_connection();
                        }
                        if ui.add(egui::Button::new("Cancel").min_size([80.0, 0.0].into())).clicked() {
                            self.connect_open = false;
                        }
                    });
                });
            });
        self.connect_open = open;
    }

    fn show_ports_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.ports_open;
        egui::Window::new("Change ports")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("ports_grid").num_columns(2).spacing([8.0, 8.0]).show(ui, |ui| {
                    ui.label("Control port:");
                    ui.add(egui::DragValue::new(&mut self.control_port).range(1..=65535));
                    ui.label("Media port:");
                    ui.add(egui::DragValue::new(&mut self.media_port).range(1..=65535));
                    ui.end_row();
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        self.ports_open = false;
                        self.save_settings();
                    }
                    if ui.button("Cancel").clicked() {
                        self.ports_open = false;
                    }
                });
            });
        self.ports_open = open;
    }

    fn show_buffer_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.buffer_open;
        egui::Window::new("Change buffer size")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Buffer size (1 - 10):");
                ui.add(egui::DragValue::new(&mut self.buffer_size).range(1..=10));
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("OK").clicked() {
                        self.buffer_open = false;
                        self.save_settings();
                    }
                    if ui.button("Cancel").clicked() {
                        self.buffer_open = false;
                    }
                });
            });
        self.buffer_open = open;
    }

    fn show_stats_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.stats_open;
        let snapshot_text = self.stats.snapshot().to_string();
        egui::Window::new("Statistics")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                let s = self.stats.snapshot();
                egui::Grid::new("stats_grid").num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
                    ui.label("Packets received");
                    ui.label(format!("{}", s.packets_received));
                    ui.label("Packets played");
                    ui.label(format!("{}", s.packets_played));
                    ui.label("Packets dropped");
                    ui.label(format!("{}", s.packets_dropped));
                    ui.label("Packets lost");
                    ui.label(format!("{}", s.packets_lost));
                    ui.label("Decode errors");
                    ui.label(format!("{}", s.decode_errors));
                    ui.label("Bytes received");
                    ui.label(format!("{} KiB", s.bytes_received / 1024));
                    ui.label("Average latency");
                    ui.label(format!("{} ms", s.average_latency.as_millis()));
                    ui.label("Maximum latency");
                    ui.label(format!("{} ms", s.max_latency.as_millis()));
                    ui.label("Reconnects");
                    ui.label(format!("{}", s.reconnects));
                    ui.label("Uptime");
                    ui.label(format!("{} s", s.uptime.as_secs()));
                    ui.end_row();
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Copy to clipboard").clicked() {
                        ui.output_mut(|o| o.copied_text = snapshot_text.clone());
                    }
                    if ui.button("Cancel").clicked() {
                        self.stats_open = false;
                    }
                });
            });
        self.stats_open = open;
    }

    fn show_about_dialog(&mut self, ctx: &egui::Context) {
        let mut open = self.about_open;
        egui::Window::new("About WO Mic Client")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("rust-mic, a WO Mic client rewritten in Rust");
                ui.label("Version: 0.1.0");
                ui.label("Protocol recovered from the shipped Windows client;");
                ui.label("see docs/ANALYSIS.md for the evidence.");
                ui.add_space(4.0);
                ui.hyperlink_to("http://www.wolicheng.com/womic", "http://www.wolicheng.com/womic");
            });
        self.about_open = open;
    }
}

fn transport_label(transport: TransportType) -> &'static str {
    match transport {
        TransportType::Bluetooth => "&Bluetooth",
        TransportType::Usb => "&USB",
        TransportType::Wifi => "&Wi-Fi",
        TransportType::WifiDirect => "&Wi-Fi Direct",
    }
}

fn exe_directory() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}
