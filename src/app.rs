use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::ascii::AsciiTable;
use crate::link;
use crate::modbus::{ModbusSaved, ModbusSession};
use crate::project;
use crate::rest::{RestClient, RestSettings};
use crate::sessions::{
    Group, SerialSaved, SerialSession, Session, Shared, TcpClientSaved, TcpClientSession, TcpServerSaved,
    TcpServerSession, UdpSaved, UdpSession,
};
use crate::terminal::SendMode;
use crate::tools::Tools;

pub const APP_NAME: &str = "PortWerk";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Tab {
    Serial,
    TcpClient,
    TcpServer,
    Udp,
    Modbus,
    Rest,
    Tools,
    About,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::Serial => "🔌 Seriell",
            Tab::TcpClient => "🔗 TCP-Client",
            Tab::TcpServer => "🗄 TCP-Server",
            Tab::Udp => "📡 UDP",
            Tab::Modbus => "🏭 Modbus",
            Tab::Rest => "🌐 REST",
            Tab::Tools => "🧮 Werkzeuge",
            Tab::About => "ℹ Info",
        }
    }
    const ALL: [Tab; 8] =
        [Tab::Serial, Tab::TcpClient, Tab::TcpServer, Tab::Udp, Tab::Modbus, Tab::Rest, Tab::Tools, Tab::About];
}

/// Everything that is remembered between starts and stored in project files.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Saved {
    pub tab: Tab,
    pub dark: bool,
    pub serial_sessions: Vec<SerialSaved>,
    pub serial_active: usize,
    pub tcpc_sessions: Vec<TcpClientSaved>,
    pub tcpc_active: usize,
    pub tcps_sessions: Vec<TcpServerSaved>,
    pub tcps_active: usize,
    pub udp_sessions: Vec<UdpSaved>,
    pub udp_active: usize,
    pub modbus_sessions: Vec<ModbusSaved>,
    pub modbus_active: usize,
    pub rest: RestSettings,
}

impl Default for Saved {
    fn default() -> Self {
        Self {
            tab: Tab::Serial,
            dark: false,
            serial_sessions: vec![SerialSaved::default()],
            serial_active: 0,
            tcpc_sessions: vec![TcpClientSaved::default()],
            tcpc_active: 0,
            tcps_sessions: vec![TcpServerSaved::default()],
            tcps_active: 0,
            udp_sessions: vec![UdpSaved::default()],
            udp_active: 0,
            modbus_sessions: vec![ModbusSaved::default()],
            modbus_active: 0,
            rest: RestSettings::default(),
        }
    }
}

/// Only kept in the app's own storage, not in project files.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct AppState {
    project: Saved,
    project_path: Option<PathBuf>,
}

pub struct PortWerkApp {
    tab: Tab,
    dark: bool,
    shared: Shared,
    serial: Group<SerialSession>,
    tcpc: Group<TcpClientSession>,
    tcps: Group<TcpServerSession>,
    udp: Group<UdpSession>,
    modbus: Group<ModbusSession>,
    rest: RestClient,
    tools: Tools,
    ascii: AsciiTable,
    project_path: Option<PathBuf>,
    title: String,
    notice: Option<(String, bool, Instant)>,
    // screenshot mode
    shot: Option<PathBuf>,
    frames: u32,
    shot_requested: bool,
}

impl PortWerkApp {
    pub fn new(cc: &eframe::CreationContext<'_>, shot: Option<PathBuf>, open: Option<PathBuf>) -> Self {
        let state: AppState = if shot.is_some() {
            AppState::default()
        } else {
            cc.storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)).unwrap_or_default()
        };
        let ports = link::list_serial_ports();
        let mut app = Self {
            tab: Tab::Serial,
            dark: state.project.dark,
            shared: Shared { ports },
            serial: Group::new(Vec::new(), 0),
            tcpc: Group::new(Vec::new(), 0),
            tcps: Group::new(Vec::new(), 0),
            udp: Group::new(Vec::new(), 0),
            modbus: Group::new(Vec::new(), 0),
            rest: RestClient::new(RestSettings::default()),
            tools: Tools::default(),
            ascii: AsciiTable::default(),
            project_path: state.project_path,
            title: String::new(),
            notice: None,
            shot,
            frames: 0,
            shot_requested: false,
        };
        app.apply(state.project);
        if let Some(p) = open {
            app.open_project(&p);
        }
        app.apply_theme(&cc.egui_ctx);
        app
    }

    /// Replace all sessions and settings.
    fn apply(&mut self, s: Saved) {
        self.stop_all();
        let ports = self.shared.ports.clone();
        self.tab = s.tab;
        self.serial = Group::new(s.serial_sessions.into_iter().map(|x| SerialSession::from_saved(x, &ports)).collect(), s.serial_active);
        self.tcpc = Group::new(s.tcpc_sessions.into_iter().map(TcpClientSession::from_saved).collect(), s.tcpc_active);
        self.tcps = Group::new(s.tcps_sessions.into_iter().map(TcpServerSession::from_saved).collect(), s.tcps_active);
        self.udp = Group::new(s.udp_sessions.into_iter().map(UdpSession::from_saved).collect(), s.udp_active);
        self.modbus = Group::new(s.modbus_sessions.into_iter().map(ModbusSession::from_saved).collect(), s.modbus_active);
        self.rest = RestClient::new(s.rest);
    }

    fn snapshot(&self) -> Saved {
        Saved {
            tab: self.tab,
            dark: self.dark,
            serial_sessions: self.serial.items.iter().map(|s| s.saved()).collect(),
            serial_active: self.serial.active,
            tcpc_sessions: self.tcpc.items.iter().map(|s| s.saved()).collect(),
            tcpc_active: self.tcpc.active,
            tcps_sessions: self.tcps.items.iter().map(|s| s.saved()).collect(),
            tcps_active: self.tcps.active,
            udp_sessions: self.udp.items.iter().map(|s| s.saved()).collect(),
            udp_active: self.udp.active,
            modbus_sessions: self.modbus.items.iter().map(|s| s.saved()).collect(),
            modbus_active: self.modbus.active,
            rest: self.rest.s.clone(),
        }
    }

    fn stop_all(&mut self) {
        self.serial.stop_all();
        self.tcpc.stop_all();
        self.tcps.stop_all();
        self.udp.stop_all();
        self.modbus.stop_all();
    }

    fn notify(&mut self, msg: impl Into<String>, ok: bool) {
        self.notice = Some((msg.into(), ok, Instant::now()));
    }

    // ------------------------------------------------------------ projects

    fn open_project(&mut self, path: &Path) {
        match project::load(path) {
            Ok(mut s) => {
                s.dark = self.dark;
                self.apply(s);
                self.project_path = Some(path.to_path_buf());
                self.notify(format!("Projekt geladen: {}", project::display_name(path)), true);
            }
            Err(e) => self.notify(e, false),
        }
    }

    fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("PortWerk-Projekt", &[project::EXT]).pick_file() {
            self.open_project(&p);
        }
    }

    fn save_project(&mut self, ask: bool) {
        let path = match (&self.project_path, ask) {
            (Some(p), false) => Some(p.clone()),
            _ => rfd::FileDialog::new()
                .add_filter("PortWerk-Projekt", &[project::EXT])
                .set_file_name(format!("projekt.{}", project::EXT))
                .save_file()
                .map(|p| if p.extension().is_none() { p.with_extension(project::EXT) } else { p }),
        };
        let Some(path) = path else { return };
        match project::save(&path, &self.snapshot()) {
            Ok(()) => {
                self.notify(format!("Gespeichert: {}", project::display_name(&path)), true);
                self.project_path = Some(path);
            }
            Err(e) => self.notify(e, false),
        }
    }

    fn new_project(&mut self) {
        let dark = self.dark;
        self.apply(Saved { dark, ..Saved::default() });
        self.project_path = None;
        self.notify("Neues Projekt", true);
    }

    fn project_menu(&mut self, ui: &mut Ui) {
        ui.menu_button("📁 Projekt", |ui| {
            if ui.button("🗋 Neu").clicked() {
                self.new_project();
                ui.close();
            }
            if ui.add(egui::Button::new("📂 Öffnen …").shortcut_text("Strg+O")).clicked() {
                ui.close();
                self.open_dialog();
            }
            if ui.add(egui::Button::new("💾 Speichern").shortcut_text("Strg+S")).clicked() {
                ui.close();
                self.save_project(false);
            }
            if ui.button("💾 Speichern unter …").clicked() {
                ui.close();
                self.save_project(true);
            }
            ui.separator();
            ui.label(
                RichText::new("Ein Projekt enthält alle Sitzungen, Sendezeilen,\nSkripte, Modbus- und REST-Einstellungen.")
                    .small()
                    .weak(),
            );
            #[cfg(windows)]
            {
                ui.separator();
                if ui
                    .button(format!("📎 .{}-Dateien mit PortWerk öffnen", project::EXT))
                    .on_hover_text("Dateiendung für Ihr Benutzerkonto registrieren (für die portable Version; der Installer erledigt das selbst)")
                    .clicked()
                {
                    ui.close();
                    match project::register_extension() {
                        Ok(m) => self.notify(m, true),
                        Err(e) => self.notify(e, false),
                    }
                }
            }
        });
    }

    // ------------------------------------------------------------ misc

    pub fn apply_start_args(&mut self, tab: Option<&str>, mode: Option<&str>, ascii: bool) {
        if let Some(t) = tab {
            self.tab = match t {
                "tcpc" => Tab::TcpClient,
                "tcps" => Tab::TcpServer,
                "udp" => Tab::Udp,
                "modbus" => Tab::Modbus,
                "rest" => Tab::Rest,
                "tools" => Tab::Tools,
                "info" => Tab::About,
                _ => Tab::Serial,
            };
        }
        if let Some(m) = mode {
            let m = match m {
                "loop" => SendMode::Loop,
                "script" => SendMode::Script,
                _ => SendMode::Lines,
            };
            self.serial.items.iter_mut().for_each(|s| s.set_send_mode(m));
            self.tcpc.items.iter_mut().for_each(|s| s.set_send_mode(m));
            self.tcps.items.iter_mut().for_each(|s| s.set_send_mode(m));
            self.udp.items.iter_mut().for_each(|s| s.set_send_mode(m));
        }
        self.ascii.open |= ascii;
    }

    fn apply_theme(&self, ctx: &egui::Context) {
        let mut v = if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        let accent = Color32::from_rgb(0, 120, 150);
        v.selection.bg_fill = if self.dark { Color32::from_rgb(0, 95, 120) } else { Color32::from_rgb(180, 225, 235) };
        v.selection.stroke.color = if self.dark { Color32::WHITE } else { accent };
        v.hyperlink_color = accent;
        ctx.set_visuals(v);
        ctx.global_style_mut(|s| {
            s.spacing.item_spacing = egui::vec2(8.0, 6.0);
            s.spacing.button_padding = egui::vec2(8.0, 3.0);
        });
    }

    fn tab_state(&self, t: Tab) -> (usize, bool) {
        match t {
            Tab::Serial => (self.serial.open_count(), self.serial.any_busy()),
            Tab::TcpClient => (self.tcpc.open_count(), self.tcpc.any_busy()),
            Tab::TcpServer => (self.tcps.open_count(), self.tcps.any_busy()),
            Tab::Udp => (self.udp.open_count(), self.udp.any_busy()),
            Tab::Modbus => (self.modbus.open_count(), self.modbus.any_busy()),
            Tab::Rest => (0, self.rest.busy()),
            _ => (0, false),
        }
    }

    fn about_tab(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(24.0)).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading(RichText::new(format!("{APP_NAME} {APP_VERSION}")).size(26.0).strong());
                ui.label("Werkzeug für Entwickler: serielle Schnittstellen, TCP, UDP, Modbus und REST in einem Fenster.");
                ui.add_space(12.0);
                section(ui, "Was es kann");
                for t in [
                    "Mehrere Sitzungen je Verbindungsart als Reiter: öffnen, benennen (Doppelklick), duplizieren, schließen – alle laufen gleichzeitig",
                    "Serielle Ports öffnen (Baudrate, Datenbits, Parität, Stoppbits, Handshake), DTR/RTS/BREAK schalten, CTS/DSR/RI/CD beobachten",
                    "Als TCP-Client mit Geräten oder Diensten verbinden",
                    "Als TCP-Server mehrere Clients annehmen, an alle oder einzelne senden, Clients trennen",
                    "UDP-Pakete senden und empfangen, auch Broadcast und Antwort an den letzten Absender",
                    "Modbus RTU und TCP: Coils, Eingänge und Register lesen und schreiben, zyklisch abfragen, Werte als 16/32 Bit, Float oder Text, Datenverkehr als Hex",
                    "REST-Client: GET/POST/PUT/PATCH/DELETE, Parameter, Header, JSON/Text/Formular-Body, Anmeldung, Variablen wie {{baseUrl}}, Verlauf, cURL-Export",
                    "Projektdateien (.portwerk): alle Sitzungen und Einstellungen speichern und an Kollegen weitergeben – Doppelklick öffnet PortWerk",
                    "Anzeige als Text, Hex, Hex + Text oder Dezimal, mit Uhrzeit und Steuerzeichen",
                    "Beliebig viele Sendezeilen, als Text mit $XX-/#DDD-Escapes oder als Hex, mit automatischer Wiederholung",
                    "Schleife: mehrere Zeilen nacheinander mit eigenen Pausen senden, x-mal oder endlos",
                    "Skripte (Lua) für Protokolle: senden, warten, auf Antworten reagieren, Prüfsummen, HTTP-Anfragen",
                    "ASCII-Tabelle (Dezimal, Hex, Binär, Zeichen) – ein Klick fügt das Zeichen in die Sendezeile ein",
                    "Echo-Modus, Pause, Fensterinhalt speichern und laufende Mitschrift in eine Datei",
                    "Werkzeuge: Umrechner Text/Hex/Dezimal/Binär, Prüfsummen (Summe, XOR, CRC-8, CRC-16 Modbus/CCITT, CRC-32), Zahlenumrechner",
                ] {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("•");
                        ui.label(t);
                    });
                }
                ui.add_space(8.0);
                section(ui, "Escape-Zeichen in Text-Sendezeilen");
                egui::Grid::new("esc").num_columns(2).striped(true).show(ui, |ui| {
                    for (a, b) in [
                        ("$0D", "ein Byte als Hex (hier 13 = CR)"),
                        ("#013", "ein Byte als Dezimalzahl"),
                        ("\\r \\n \\t \\0", "CR, LF, Tab, Null"),
                        ("$$  ##", "das Zeichen $ bzw. # selbst"),
                    ] {
                        ui.monospace(a);
                        ui.label(b);
                        ui.end_row();
                    }
                });
            });
        });
    }
}

impl eframe::App for PortWerkApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.serial.pump(&ctx);
        self.tcpc.pump(&ctx);
        self.tcps.pump(&ctx);
        self.udp.pump(&ctx);
        self.modbus.pump(&ctx);

        // shortcuts
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::S)) {
            self.save_project(false);
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::O)) {
            self.open_dialog();
        }
        // a project file dropped onto the window
        let dropped: Option<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).find(|p| !p.as_os_str().is_empty()));
        if let Some(p) = dropped {
            if p.extension().is_some_and(|e| e.eq_ignore_ascii_case(project::EXT)) {
                self.open_project(&p);
            }
        }

        // window title
        let title = match &self.project_path {
            Some(p) => format!("{} – {APP_NAME} {APP_VERSION}", project::display_name(p)),
            None => format!("{APP_NAME} {APP_VERSION}"),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        // ---- top bar
        egui::Panel::top("tabs")
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(10, 6)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(APP_NAME).strong().size(18.0).color(Color32::from_rgb(0, 130, 160)));
                    ui.add_space(4.0);
                    self.project_menu(ui);
                    ui.add_space(8.0);
                    for t in Tab::ALL {
                        let (open, busy) = self.tab_state(t);
                        let mut label = t.label().to_string();
                        if open > 0 {
                            label.push_str(&format!(" ({open})"));
                        }
                        if busy {
                            label.push_str(" 🔁");
                        }
                        let mut text = RichText::new(label).size(14.5);
                        if open > 0 {
                            text = text.color(Color32::from_rgb(30, 160, 70));
                        }
                        if ui.add(egui::Button::selectable(self.tab == t, text)).clicked() {
                            self.tab = t;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let icon = if self.dark { "☀" } else { "🌙" };
                        if ui.button(icon).on_hover_text("Hell / Dunkel").clicked() {
                            self.dark = !self.dark;
                            self.apply_theme(&ctx);
                        }
                        ui.toggle_value(&mut self.ascii.open, "🔤 ASCII")
                            .on_hover_text("ASCII-Tabelle mit Dezimal-, Hex- und Zeichenwerten einblenden");
                    });
                });
            });

        // ---- status bar
        egui::Panel::bottom("status")
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(10, 4)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    status_group(ui, "Seriell", &self.serial);
                    status_group(ui, "TCP-Client", &self.tcpc);
                    status_group(ui, "TCP-Server", &self.tcps);
                    status_group(ui, "UDP", &self.udp);
                    status_group(ui, "Modbus", &self.modbus);
                    if let Some((msg, ok, at)) = &self.notice {
                        if at.elapsed() < Duration::from_secs(8) {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let col = if *ok { Color32::from_rgb(30, 150, 70) } else { Color32::from_rgb(200, 60, 50) };
                                ui.label(RichText::new(msg).color(col));
                            });
                            ctx.request_repaint_after(Duration::from_millis(500));
                        }
                    }
                });
            });

        let shared = &mut self.shared;
        match self.tab {
            Tab::Serial => self.serial.ui(ui, "Seriell", shared, |n| SerialSession::new(n, &shared_ports())),
            Tab::TcpClient => self.tcpc.ui(ui, "Client", shared, TcpClientSession::new),
            Tab::TcpServer => {
                let next = self.tcps.items.iter().map(|s| s.port()).max().map_or(2000, |p| p.saturating_add(1));
                self.tcps.ui(ui, "Server", shared, move |n| TcpServerSession::new(n, next))
            }
            Tab::Udp => {
                let next = 2001 + self.udp.items.len() as u16;
                self.udp.ui(ui, "UDP", shared, move |n| UdpSession::new(n, next))
            }
            Tab::Modbus => self.modbus.ui(ui, "Modbus", shared, ModbusSession::new),
            Tab::Rest => self.rest.ui(ui),
            Tab::Tools => self.tools.ui(ui),
            Tab::About => self.about_tab(ui),
        }

        if let Some(b) = self.ascii.show(&ctx) {
            match self.tab {
                Tab::Serial => self.serial.current().map(|s| s.insert_byte(b)),
                Tab::TcpClient => self.tcpc.current().map(|s| s.insert_byte(b)),
                Tab::TcpServer => self.tcps.current().map(|s| s.insert_byte(b)),
                Tab::Udp => self.udp.current().map(|s| s.insert_byte(b)),
                Tab::Tools => Some(self.tools.insert_byte(b)),
                _ => None,
            };
        }

        // ---- screenshot mode
        if let Some(path) = self.shot.clone() {
            self.frames += 1;
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
            ctx.request_repaint();
            if self.frames >= 120 && !self.shot_requested {
                self.shot_requested = true;
                ctx.request_screenshot(move |img| {
                    let [w, h] = img.size;
                    let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
                    let code = match image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                        Some(buf) => match buf.save(&path) {
                            Ok(()) => 0,
                            Err(e) => {
                                eprintln!("Screenshot konnte nicht gespeichert werden: {e}");
                                1
                            }
                        },
                        None => 1,
                    };
                    std::process::exit(code);
                });
            }
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if self.shot.is_none() {
            let state = AppState { project: self.snapshot(), project_path: self.project_path.clone() };
            eframe::set_value(storage, eframe::APP_KEY, &state);
        }
    }

    fn on_exit(&mut self) {
        self.stop_all();
    }
}

fn shared_ports() -> Vec<String> {
    link::list_serial_ports()
}

fn status_group<T: Session>(ui: &mut Ui, label: &str, g: &Group<T>) {
    let open = g.open_count();
    let total = g.items.len();
    let col = if open > 0 { Color32::from_rgb(30, 160, 70) } else { Color32::from_gray(150) };
    let r = ui
        .horizontal(|ui| {
            ui.label(RichText::new("●").color(col));
            ui.label(RichText::new(format!("{label} {open}/{total}")).size(12.5));
        })
        .response;
    if open > 0 {
        let list: Vec<String> = g.items.iter().filter(|s| s.is_open()).map(|s| s.summary()).collect();
        r.on_hover_text(list.join("\n"));
    }
    ui.add_space(10.0);
}

pub fn section(ui: &mut Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong().size(14.0));
    ui.separator();
}
