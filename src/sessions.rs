//! Connection sessions. Every connection type (serial, TCP client,
//! TCP server, UDP, Modbus) can have several named instances, shown as
//! tabs that can be opened, renamed, duplicated and closed.

use eframe::egui::{self, Color32, CornerRadius, RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::app::section;
use crate::link::{self, Cmd, Event, Flow, Link, ModemLines, Parity, SerialConfig};
use crate::terminal::{Dir, SendMode, SendRequest, TermSettings, Terminal};

pub const GREEN: Color32 = Color32::from_rgb(30, 160, 70);
pub const ORANGE: Color32 = Color32::from_rgb(220, 150, 0);
pub const ACCENT: Color32 = Color32::from_rgb(0, 130, 160);
pub const RED: Color32 = Color32::from_rgb(200, 70, 60);

/// State shared by all sessions (e.g. the list of serial ports).
pub struct Shared {
    pub ports: Vec<String>,
}

pub trait Session {
    fn name(&self) -> &str;
    fn set_name(&mut self, n: String);
    fn is_open(&self) -> bool;
    fn is_busy(&self) -> bool {
        false
    }
    /// Called every frame for every session, visible or not.
    fn pump(&mut self, ctx: &egui::Context);
    fn ui(&mut self, ui: &mut Ui, shared: &mut Shared);
    fn stop(&mut self);
    fn duplicate(&self) -> Self
    where
        Self: Sized;
    fn insert_byte(&mut self, _b: u8) {}
    fn set_send_mode(&mut self, _m: SendMode) {}
    /// Short text for the status bar when open, e.g. "COM3".
    fn summary(&self) -> String {
        self.name().to_string()
    }
}

// ------------------------------------------------------------------ group of sessions

enum Act {
    Select(usize),
    Close(usize),
    Duplicate(usize),
    Rename(usize),
    New,
}

pub struct Group<T> {
    pub items: Vec<T>,
    pub active: usize,
    rename: Option<usize>,
    rename_buf: String,
    rename_focus: bool,
}

impl<T: Session> Group<T> {
    pub fn new(items: Vec<T>, active: usize) -> Self {
        let active = active.min(items.len().saturating_sub(1));
        Self { items, active, rename: None, rename_buf: String::new(), rename_focus: false }
    }

    pub fn open_count(&self) -> usize {
        self.items.iter().filter(|s| s.is_open()).count()
    }

    pub fn any_busy(&self) -> bool {
        self.items.iter().any(|s| s.is_busy())
    }

    pub fn pump(&mut self, ctx: &egui::Context) {
        for s in &mut self.items {
            s.pump(ctx);
        }
    }

    pub fn stop_all(&mut self) {
        for s in &mut self.items {
            s.stop();
        }
    }

    pub fn current(&mut self) -> Option<&mut T> {
        self.items.get_mut(self.active)
    }

    fn next_name(&self, prefix: &str) -> String {
        (1..).map(|n| format!("{prefix} {n}")).find(|c| !self.items.iter().any(|s| s.name() == c)).unwrap()
    }

    /// Session tab bar + the active session.
    pub fn ui(&mut self, ui: &mut Ui, prefix: &str, shared: &mut Shared, make: impl Fn(String) -> T) {
        let mut act = None;
        let dark = ui.visuals().dark_mode;
        egui::Panel::top("session_bar")
            .frame(
                egui::Frame::side_top_panel(ui.style())
                    .inner_margin(egui::Margin { left: 10, right: 10, top: 6, bottom: 0 }),
            )
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    for (i, s) in self.items.iter().enumerate() {
                        let sel = i == self.active;
                        let fill = if sel {
                            ui.visuals().panel_fill
                        } else if dark {
                            Color32::from_gray(40)
                        } else {
                            Color32::from_gray(228)
                        };
                        let fill = if sel && !dark { Color32::from_gray(250) } else { fill };
                        egui::Frame::new()
                            .fill(fill)
                            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                            .corner_radius(CornerRadius { nw: 6, ne: 6, sw: 0, se: 0 })
                            .inner_margin(egui::Margin { left: 8, right: 4, top: 4, bottom: 4 })
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let col = if s.is_open() { GREEN } else { Color32::from_gray(160) };
                                    led(ui, col);
                                    if self.rename == Some(i) {
                                        let r = ui.add(egui::TextEdit::singleline(&mut self.rename_buf).desired_width(130.0));
                                        if self.rename_focus {
                                            r.request_focus();
                                            self.rename_focus = false;
                                        }
                                        if r.lost_focus() {
                                            act = Some(Act::Rename(i));
                                        }
                                    } else {
                                        let mut text = RichText::new(s.name());
                                        if sel {
                                            text = text.strong();
                                        }
                                        if s.is_busy() {
                                            text = RichText::new(format!("{} 🔁", s.name()));
                                        }
                                        let r = ui
                                            .add(egui::Label::new(text).sense(egui::Sense::click()).selectable(false))
                                            .on_hover_text("Klick: anzeigen · Doppelklick: umbenennen · Rechtsklick: mehr");
                                        if r.clicked() {
                                            act = Some(Act::Select(i));
                                        }
                                        if r.double_clicked() {
                                            self.rename = Some(i);
                                            self.rename_buf = s.name().to_string();
                                            self.rename_focus = true;
                                        }
                                        r.context_menu(|ui| {
                                            if ui.button("✏ Umbenennen").clicked() {
                                                self.rename = Some(i);
                                                self.rename_buf = s.name().to_string();
                                                self.rename_focus = true;
                                                ui.close();
                                            }
                                            if ui.button("🗐 Duplizieren").clicked() {
                                                act = Some(Act::Duplicate(i));
                                                ui.close();
                                            }
                                            if ui.button("✖ Schließen").clicked() {
                                                act = Some(Act::Close(i));
                                                ui.close();
                                            }
                                        });
                                    }
                                    if ui
                                        .add(egui::Button::new(RichText::new("✖").size(11.0)).frame(false))
                                        .on_hover_text("Sitzung schließen (Verbindung wird getrennt)")
                                        .clicked()
                                    {
                                        act = Some(Act::Close(i));
                                    }
                                });
                            });
                    }
                    if ui.button("➕ Neu").on_hover_text("Neue Sitzung öffnen").clicked() {
                        act = Some(Act::New);
                    }
                });
            });

        match act {
            Some(Act::Select(i)) => self.active = i,
            Some(Act::Close(i)) => {
                self.items[i].stop();
                self.items.remove(i);
                if self.active >= self.items.len() {
                    self.active = self.items.len().saturating_sub(1);
                } else if self.active > i {
                    self.active -= 1;
                }
                self.rename = None;
            }
            Some(Act::Duplicate(i)) => {
                let mut d = self.items[i].duplicate();
                d.set_name(format!("{} (Kopie)", self.items[i].name()));
                self.items.insert(i + 1, d);
                self.active = i + 1;
            }
            Some(Act::Rename(i)) => {
                let n = self.rename_buf.trim().to_string();
                if !n.is_empty() {
                    self.items[i].set_name(n);
                }
                self.rename = None;
            }
            Some(Act::New) => {
                let n = self.next_name(prefix);
                self.items.push(make(n));
                self.active = self.items.len() - 1;
            }
            None => {}
        }

        if self.items.is_empty() {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    ui.label(RichText::new("Keine Sitzung offen.").size(16.0).weak());
                    ui.add_space(8.0);
                    if ui.button(RichText::new("➕ Neue Sitzung").size(15.0)).clicked() {
                        let n = self.next_name(prefix);
                        self.items.push(make(n));
                        self.active = 0;
                    }
                });
            });
            return;
        }
        let i = self.active;
        ui.push_id(("session", i), |ui| self.items[i].ui(ui, shared));
    }
}

// ------------------------------------------------------------------ one connection + terminal

pub struct Conn {
    pub term: Terminal,
    pub link: Option<Link>,
    pub open: bool,
    pub starting: bool,
    needs_target: bool,
}

impl Conn {
    pub fn new(s: TermSettings, hello: &str) -> Self {
        let mut term = Terminal::new(s);
        term.info(hello);
        Self { term, link: None, open: false, starting: false, needs_target: false }
    }

    pub fn start(&mut self, link: Link) {
        self.link = Some(link);
        self.starting = true;
    }

    pub fn stop(&mut self) {
        if let Some(l) = &self.link {
            l.send(Cmd::Close);
        }
    }

    pub fn active(&self) -> bool {
        self.link.is_some()
    }

    /// Handle worker events and run timers/loops/scripts.
    fn pump(&mut self, ctx: &egui::Context, to: Option<String>, other: impl FnMut(&Event)) {
        self.pump_events(other);
        let reqs = self.term.tick(ctx, self.open);
        self.send(to, reqs);
    }

    fn pump_events(&mut self, mut other: impl FnMut(&Event)) {
        let Some(link) = &self.link else { return };
        let events = link.poll();
        let mut closed = false;
        for e in events {
            match e {
                Event::Info(m) => self.term.info(m),
                Event::Error(m) => self.term.error(m),
                Event::Opened => {
                    self.open = true;
                    self.starting = false;
                }
                Event::Closed => closed = true,
                Event::Rx { peer, data } => {
                    other(&Event::Rx { peer: peer.clone(), data: data.clone() });
                    if self.term.s.echo {
                        link.send(Cmd::Send { to: peer.clone(), data: data.clone() });
                    }
                    self.term.feed_script(&data);
                    self.term.push(Dir::Rx, peer, data);
                }
                Event::Tx { peer, data } => self.term.push(Dir::Tx, peer, data),
                ev => other(&ev),
            }
        }
        if closed {
            self.open = false;
            self.starting = false;
            self.link = None;
            other(&Event::Closed);
        }
    }

    fn send(&mut self, to: Option<String>, reqs: Vec<SendRequest>) {
        if reqs.is_empty() {
            return;
        }
        if self.needs_target && to.is_none() {
            self.term.error("Noch kein Absender bekannt – erst Daten empfangen oder ein Ziel eintragen");
            return;
        }
        if let Some(l) = &self.link {
            for r in reqs {
                l.send(Cmd::Send { to: to.clone(), data: r.data });
            }
        }
    }

    /// Right settings panel + bottom send area + log in the middle.
    fn frame(&mut self, ui: &mut Ui, settings: impl FnOnce(&mut Ui, &mut Conn)) -> Vec<SendRequest> {
        egui::Panel::right("cfg")
            .resizable(true)
            .default_size(270.0)
            .min_size(230.0)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(10.0))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| settings(ui, self));
            });
        let connected = self.open;
        let mut reqs = Vec::new();
        egui::Panel::bottom("send")
            .resizable(false)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(10, 8)))
            .show(ui, |ui| {
                reqs = self.term.send_ui(ui, connected);
            });
        egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(10.0)).show(ui, |ui| {
            self.term.toolbar_ui(ui);
            ui.add_space(2.0);
            self.term.log_ui(ui);
        });
        reqs
    }
}

// ------------------------------------------------------------------ serial

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SerialSaved {
    pub name: String,
    pub cfg: SerialConfig,
    pub dtr: bool,
    pub rts: bool,
    pub term: TermSettings,
}

impl Default for SerialSaved {
    fn default() -> Self {
        Self { name: "Seriell 1".into(), cfg: SerialConfig::default(), dtr: true, rts: true, term: TermSettings::default() }
    }
}

pub struct SerialSession {
    name: String,
    cfg: SerialConfig,
    dtr: bool,
    rts: bool,
    brk: bool,
    lines: ModemLines,
    conn: Conn,
}

impl SerialSession {
    pub fn new(name: String, ports: &[String]) -> Self {
        Self::from_saved(SerialSaved { name, ..Default::default() }, ports)
    }

    pub fn from_saved(s: SerialSaved, ports: &[String]) -> Self {
        let mut cfg = s.cfg;
        if cfg.port.is_empty() {
            cfg.port = ports.first().cloned().unwrap_or_else(|| default_port_name().into());
        }
        Self {
            name: s.name,
            cfg,
            dtr: s.dtr,
            rts: s.rts,
            brk: false,
            lines: ModemLines::default(),
            conn: Conn::new(s.term, "Bereit. Schnittstelle wählen und „Öffnen“ klicken."),
        }
    }

    pub fn saved(&self) -> SerialSaved {
        SerialSaved {
            name: self.name.clone(),
            cfg: self.cfg.clone(),
            dtr: self.dtr,
            rts: self.rts,
            term: self.conn.term.s.clone(),
        }
    }
}

impl Session for SerialSession {
    fn name(&self) -> &str {
        &self.name
    }
    fn set_name(&mut self, n: String) {
        self.name = n;
    }
    fn is_open(&self) -> bool {
        self.conn.open
    }
    fn is_busy(&self) -> bool {
        self.conn.term.busy()
    }
    fn stop(&mut self) {
        self.conn.stop();
    }
    fn duplicate(&self) -> Self {
        Self::from_saved(self.saved(), &[])
    }
    fn insert_byte(&mut self, b: u8) {
        self.conn.term.insert_byte(b);
    }
    fn set_send_mode(&mut self, m: SendMode) {
        self.conn.term.s.mode = m;
    }
    fn summary(&self) -> String {
        format!("{} ({})", self.name, self.cfg.port)
    }

    fn pump(&mut self, ctx: &egui::Context) {
        let lines = &mut self.lines;
        self.conn.pump(ctx, None, |e| {
            if let Event::Lines(l) = e {
                *lines = *l;
            } else if let Event::Closed = e {
                *lines = ModemLines::default();
            }
        });
    }

    fn ui(&mut self, ui: &mut Ui, shared: &mut Shared) {
        let cfg = &mut self.cfg;
        let (dtr, rts, brk, lines) = (&mut self.dtr, &mut self.rts, &mut self.brk, self.lines);
        let reqs = self.conn.frame(ui, |ui, conn| {
            section(ui, "Schnittstelle");
            serial_settings_ui(ui, "serial", cfg, &mut shared.ports, conn.active());
            ui.add_space(6.0);
            if big_button(ui, conn.active(), "Öffnen", "Schließen").clicked() {
                if conn.active() {
                    conn.stop();
                } else if cfg.port.trim().is_empty() {
                    conn.term.error("Bitte zuerst einen Port wählen");
                } else {
                    let mut c = cfg.clone();
                    c.port = c.port.trim().to_string();
                    conn.start(link::open_serial(ui.ctx(), c, *dtr, *rts));
                }
            }
            status_line(ui, conn.open, conn.starting, "offen", "geschlossen");

            section(ui, "Steuerleitungen");
            ui.horizontal(|ui| {
                if ui.toggle_value(dtr, "DTR").on_hover_text("Data Terminal Ready setzen").changed() {
                    if let Some(l) = &conn.link {
                        l.send(Cmd::SetDtr(*dtr));
                    }
                }
                if ui.toggle_value(rts, "RTS").on_hover_text("Request To Send setzen").changed() {
                    if let Some(l) = &conn.link {
                        l.send(Cmd::SetRts(*rts));
                    }
                }
                if ui
                    .add_enabled(conn.open, egui::Button::new("BREAK").selected(*brk))
                    .on_hover_text("Break-Signal ein/aus")
                    .clicked()
                {
                    *brk = !*brk;
                    if let Some(l) = &conn.link {
                        l.send(Cmd::SetBreak(*brk));
                    }
                }
            });
            ui.horizontal(|ui| {
                lamp(ui, "CTS", lines.cts, conn.open);
                lamp(ui, "DSR", lines.dsr, conn.open);
                lamp(ui, "RI", lines.ri, conn.open);
                lamp(ui, "CD", lines.cd, conn.open);
            });
        });
        self.conn.send(None, reqs);
    }
}

/// Port, baud rate, data bits, parity, stop bits and handshake.
pub fn serial_settings_ui(ui: &mut Ui, id: &str, cfg: &mut SerialConfig, ports: &mut Vec<String>, locked: bool) {
    ui.add_enabled_ui(!locked, |ui| {
        egui::Grid::new(format!("{id}_grid")).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
            ui.label("Port");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(format!("{id}_port"))
                    .selected_text(if cfg.port.is_empty() { "–" } else { cfg.port.as_str() })
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        if ports.is_empty() {
                            ui.label(RichText::new("Keine Ports gefunden").weak());
                        }
                        for p in ports.iter() {
                            ui.selectable_value(&mut cfg.port, p.clone(), p);
                        }
                    });
                if ui.button("🔄").on_hover_text("Portliste neu einlesen").clicked() {
                    *ports = link::list_serial_ports();
                }
            });
            ui.end_row();
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut cfg.port).desired_width(140.0))
                .on_hover_text("Portname kann auch direkt eingegeben werden");
            ui.end_row();
            ui.label("Baudrate");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(format!("{id}_baud")).selected_text(cfg.baud.to_string()).width(80.0).show_ui(
                    ui,
                    |ui| {
                        for b in [
                            300, 600, 1200, 2400, 4800, 9600, 14400, 19200, 28800, 38400, 57600, 115200, 230400, 460800,
                            921600,
                        ] {
                            ui.selectable_value(&mut cfg.baud, b, b.to_string());
                        }
                    },
                );
                ui.add(egui::DragValue::new(&mut cfg.baud).range(50..=10_000_000).speed(100.0))
                    .on_hover_text("Eigene Baudrate");
            });
            ui.end_row();
            ui.label("Datenbits");
            ui.horizontal(|ui| {
                for d in [5u8, 6, 7, 8] {
                    ui.selectable_value(&mut cfg.data_bits, d, d.to_string());
                }
            });
            ui.end_row();
            ui.label("Parität");
            egui::ComboBox::from_id_salt(format!("{id}_parity"))
                .selected_text(parity_label(cfg.parity))
                .width(110.0)
                .show_ui(ui, |ui| {
                    for p in [Parity::None, Parity::Odd, Parity::Even] {
                        ui.selectable_value(&mut cfg.parity, p, parity_label(p));
                    }
                });
            ui.end_row();
            ui.label("Stoppbits");
            ui.horizontal(|ui| {
                ui.selectable_value(&mut cfg.stop_bits, 1, "1");
                ui.selectable_value(&mut cfg.stop_bits, 2, "2");
            });
            ui.end_row();
            ui.label("Handshake");
            egui::ComboBox::from_id_salt(format!("{id}_flow")).selected_text(flow_label(cfg.flow)).width(110.0).show_ui(
                ui,
                |ui| {
                    for f in [Flow::None, Flow::Hardware, Flow::Software] {
                        ui.selectable_value(&mut cfg.flow, f, flow_label(f));
                    }
                },
            );
            ui.end_row();
        });
    });
}

// ------------------------------------------------------------------ TCP client

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TcpClientSaved {
    pub name: String,
    pub host: String,
    pub port: u16,
    pub term: TermSettings,
}

impl Default for TcpClientSaved {
    fn default() -> Self {
        Self { name: "Client 1".into(), host: "127.0.0.1".into(), port: 23, term: TermSettings::default() }
    }
}

pub struct TcpClientSession {
    name: String,
    host: String,
    port: u16,
    conn: Conn,
}

impl TcpClientSession {
    pub fn new(name: String) -> Self {
        Self::from_saved(TcpClientSaved { name, ..Default::default() })
    }
    pub fn from_saved(s: TcpClientSaved) -> Self {
        Self {
            name: s.name,
            host: s.host,
            port: s.port,
            conn: Conn::new(s.term, "Bereit. Adresse und Port eingeben und „Verbinden“ klicken."),
        }
    }
    pub fn saved(&self) -> TcpClientSaved {
        TcpClientSaved { name: self.name.clone(), host: self.host.clone(), port: self.port, term: self.conn.term.s.clone() }
    }
}

impl Session for TcpClientSession {
    fn name(&self) -> &str {
        &self.name
    }
    fn set_name(&mut self, n: String) {
        self.name = n;
    }
    fn is_open(&self) -> bool {
        self.conn.open
    }
    fn is_busy(&self) -> bool {
        self.conn.term.busy()
    }
    fn stop(&mut self) {
        self.conn.stop();
    }
    fn duplicate(&self) -> Self {
        Self::from_saved(self.saved())
    }
    fn insert_byte(&mut self, b: u8) {
        self.conn.term.insert_byte(b);
    }
    fn set_send_mode(&mut self, m: SendMode) {
        self.conn.term.s.mode = m;
    }
    fn summary(&self) -> String {
        format!("{} ({}:{})", self.name, self.host, self.port)
    }
    fn pump(&mut self, ctx: &egui::Context) {
        self.conn.pump(ctx, None, |_| {});
    }

    fn ui(&mut self, ui: &mut Ui, _shared: &mut Shared) {
        let host = &mut self.host;
        let port = &mut self.port;
        let reqs = self.conn.frame(ui, |ui, conn| {
            section(ui, "Gegenstelle");
            ui.add_enabled_ui(!conn.active(), |ui| {
                egui::Grid::new("tcpc_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Adresse");
                    ui.add(egui::TextEdit::singleline(host).desired_width(150.0).hint_text("IP oder Hostname"));
                    ui.end_row();
                    ui.label("Port");
                    ui.add(egui::DragValue::new(port).range(1..=65535));
                    ui.end_row();
                });
            });
            ui.add_space(6.0);
            if big_button(ui, conn.active(), "Verbinden", "Trennen").clicked() {
                if conn.active() {
                    conn.stop();
                } else {
                    conn.start(link::open_tcp_client(ui.ctx(), host.clone(), *port));
                }
            }
            status_line(ui, conn.open, conn.starting, "verbunden", "getrennt");
            section(ui, "Hinweis");
            ui.label(
                RichText::new(
                    "Verbindet sich als Client mit einem TCP-Server, z. B. einem Gerät, einem Telnet-Dienst oder einer Sitzung im TCP-Server-Reiter.",
                )
                .weak(),
            );
        });
        self.conn.send(None, reqs);
    }
}

// ------------------------------------------------------------------ TCP server

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TcpServerSaved {
    pub name: String,
    pub bind: String,
    pub port: u16,
    pub term: TermSettings,
}

impl Default for TcpServerSaved {
    fn default() -> Self {
        Self { name: "Server 1".into(), bind: "0.0.0.0".into(), port: 2000, term: TermSettings::default() }
    }
}

pub struct TcpServerSession {
    name: String,
    bind: String,
    port: u16,
    clients: Vec<String>,
    target: Option<String>,
    conn: Conn,
}

impl TcpServerSession {
    pub fn new(name: String, port: u16) -> Self {
        Self::from_saved(TcpServerSaved { name, port, ..Default::default() })
    }
    pub fn from_saved(s: TcpServerSaved) -> Self {
        Self {
            name: s.name,
            bind: s.bind,
            port: s.port,
            clients: Vec::new(),
            target: None,
            conn: Conn::new(s.term, "Bereit. Port wählen und „Starten“ klicken."),
        }
    }
    pub fn saved(&self) -> TcpServerSaved {
        TcpServerSaved { name: self.name.clone(), bind: self.bind.clone(), port: self.port, term: self.conn.term.s.clone() }
    }
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Session for TcpServerSession {
    fn name(&self) -> &str {
        &self.name
    }
    fn set_name(&mut self, n: String) {
        self.name = n;
    }
    fn is_open(&self) -> bool {
        self.conn.open
    }
    fn is_busy(&self) -> bool {
        self.conn.term.busy()
    }
    fn stop(&mut self) {
        self.conn.stop();
    }
    fn duplicate(&self) -> Self {
        let mut s = self.saved();
        s.port = s.port.saturating_add(1);
        Self::from_saved(s)
    }
    fn insert_byte(&mut self, b: u8) {
        self.conn.term.insert_byte(b);
    }
    fn set_send_mode(&mut self, m: SendMode) {
        self.conn.term.s.mode = m;
    }
    fn summary(&self) -> String {
        format!("{} (:{}, {} Clients)", self.name, self.port, self.clients.len())
    }

    fn pump(&mut self, ctx: &egui::Context) {
        let clients = &mut self.clients;
        let target = &mut self.target;
        let to = target.clone();
        self.conn.pump(ctx, to, |e| match e {
            Event::ClientUp(c) => clients.push(c.clone()),
            Event::ClientDown(c) => {
                clients.retain(|x| x != c);
                if target.as_ref() == Some(c) {
                    *target = None;
                }
            }
            Event::Closed => {
                clients.clear();
                *target = None;
            }
            _ => {}
        });
    }

    fn ui(&mut self, ui: &mut Ui, _shared: &mut Shared) {
        let bind = &mut self.bind;
        let port = &mut self.port;
        let clients = &self.clients;
        let target = &mut self.target;
        let reqs = self.conn.frame(ui, |ui, conn| {
            section(ui, "Server");
            ui.add_enabled_ui(!conn.active(), |ui| {
                egui::Grid::new("tcps_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Lokale IP");
                    ui.add(egui::TextEdit::singleline(bind).desired_width(150.0))
                        .on_hover_text("0.0.0.0 = auf allen Netzwerkkarten");
                    ui.end_row();
                    ui.label("Port");
                    ui.add(egui::DragValue::new(port).range(1..=65535));
                    ui.end_row();
                });
            });
            ui.add_space(6.0);
            if big_button(ui, conn.active(), "Starten", "Stoppen").clicked() {
                if conn.active() {
                    conn.stop();
                } else {
                    conn.start(link::open_tcp_server(ui.ctx(), bind.clone(), *port));
                }
            }
            status_line(ui, conn.open, conn.starting, "lauscht", "gestoppt");
            section(ui, &format!("Clients ({})", clients.len()));
            if clients.is_empty() {
                ui.label(RichText::new("Noch kein Client verbunden.").weak().italics());
            } else {
                ui.label(RichText::new("Senden an:").small());
                ui.radio_value(target, None, "Alle Clients");
                let mut kick = None;
                for c in clients {
                    ui.horizontal(|ui| {
                        ui.radio_value(target, Some(c.clone()), c);
                        if ui.small_button("✖").on_hover_text("Verbindung trennen").clicked() {
                            kick = Some(c.clone());
                        }
                    });
                }
                if let (Some(k), Some(l)) = (kick, &conn.link) {
                    l.send(Cmd::Kick(k));
                }
            }
        });
        let to = self.target.clone();
        self.conn.send(to, reqs);
    }
}

// ------------------------------------------------------------------ UDP

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct UdpSaved {
    pub name: String,
    pub bind: String,
    pub port: u16,
    pub remote_host: String,
    pub remote_port: u16,
    pub broadcast: bool,
    pub reply: bool,
    pub term: TermSettings,
}

impl Default for UdpSaved {
    fn default() -> Self {
        Self {
            name: "UDP 1".into(),
            bind: "0.0.0.0".into(),
            port: 2001,
            remote_host: "127.0.0.1".into(),
            remote_port: 2001,
            broadcast: false,
            reply: false,
            term: TermSettings::default(),
        }
    }
}

pub struct UdpSession {
    name: String,
    bind: String,
    port: u16,
    remote_host: String,
    remote_port: u16,
    broadcast: bool,
    reply: bool,
    last_sender: Option<String>,
    conn: Conn,
}

impl UdpSession {
    pub fn new(name: String, port: u16) -> Self {
        Self::from_saved(UdpSaved { name, port, ..Default::default() })
    }
    pub fn from_saved(s: UdpSaved) -> Self {
        let mut conn = Conn::new(s.term, "Bereit. Lokalen Port öffnen, dann an Ziel senden.");
        conn.needs_target = true;
        Self {
            name: s.name,
            bind: s.bind,
            port: s.port,
            remote_host: s.remote_host,
            remote_port: s.remote_port,
            broadcast: s.broadcast,
            reply: s.reply,
            last_sender: None,
            conn,
        }
    }
    pub fn saved(&self) -> UdpSaved {
        UdpSaved {
            name: self.name.clone(),
            bind: self.bind.clone(),
            port: self.port,
            remote_host: self.remote_host.clone(),
            remote_port: self.remote_port,
            broadcast: self.broadcast,
            reply: self.reply,
            term: self.conn.term.s.clone(),
        }
    }
    fn target(&self) -> Option<String> {
        if self.reply {
            self.last_sender.clone()
        } else {
            Some(format!("{}:{}", self.remote_host.trim(), self.remote_port))
        }
    }
}

impl Session for UdpSession {
    fn name(&self) -> &str {
        &self.name
    }
    fn set_name(&mut self, n: String) {
        self.name = n;
    }
    fn is_open(&self) -> bool {
        self.conn.open
    }
    fn is_busy(&self) -> bool {
        self.conn.term.busy()
    }
    fn stop(&mut self) {
        self.conn.stop();
    }
    fn duplicate(&self) -> Self {
        let mut s = self.saved();
        s.port = s.port.saturating_add(1);
        Self::from_saved(s)
    }
    fn insert_byte(&mut self, b: u8) {
        self.conn.term.insert_byte(b);
    }
    fn set_send_mode(&mut self, m: SendMode) {
        self.conn.term.s.mode = m;
    }
    fn summary(&self) -> String {
        format!("{} (:{})", self.name, self.port)
    }
    fn pump(&mut self, ctx: &egui::Context) {
        let to = self.target();
        let last = &mut self.last_sender;
        self.conn.pump(ctx, to, |e| {
            if let Event::Rx { peer: Some(p), .. } = e {
                *last = Some(p.clone());
            }
        });
    }

    fn ui(&mut self, ui: &mut Ui, _shared: &mut Shared) {
        let bind = &mut self.bind;
        let port = &mut self.port;
        let rhost = &mut self.remote_host;
        let rport = &mut self.remote_port;
        let bcast = &mut self.broadcast;
        let reply = &mut self.reply;
        let last = &self.last_sender;
        let reqs = self.conn.frame(ui, |ui, conn| {
            section(ui, "Lokal");
            ui.add_enabled_ui(!conn.active(), |ui| {
                egui::Grid::new("udp_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Lokale IP");
                    ui.add(egui::TextEdit::singleline(bind).desired_width(150.0));
                    ui.end_row();
                    ui.label("Port");
                    ui.add(egui::DragValue::new(port).range(0..=65535)).on_hover_text("0 = beliebiger freier Port");
                    ui.end_row();
                });
                ui.checkbox(bcast, "Broadcast erlauben");
            });
            ui.add_space(6.0);
            if big_button(ui, conn.active(), "Öffnen", "Schließen").clicked() {
                if conn.active() {
                    conn.stop();
                } else {
                    conn.start(link::open_udp(ui.ctx(), bind.clone(), *port, *bcast));
                }
            }
            status_line(ui, conn.open, conn.starting, "offen", "geschlossen");
            section(ui, "Ziel");
            egui::Grid::new("udp_dst").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Adresse");
                ui.add_enabled(
                    !*reply,
                    egui::TextEdit::singleline(rhost).desired_width(150.0).hint_text("IP, Name oder Broadcast"),
                );
                ui.end_row();
                ui.label("Port");
                ui.add_enabled(!*reply, egui::DragValue::new(rport).range(1..=65535));
                ui.end_row();
            });
            ui.checkbox(reply, "An letzten Absender antworten");
            ui.label(RichText::new(format!("Letzter Absender: {}", last.as_deref().unwrap_or("–"))).small().weak());
        });
        let to = self.target();
        self.conn.send(to, reqs);
    }
}

// ------------------------------------------------------------------ small widgets

pub fn default_port_name() -> &'static str {
    if cfg!(windows) { "COM1" } else { "/dev/ttyUSB0" }
}

pub fn parity_label(p: Parity) -> &'static str {
    match p {
        Parity::None => "keine",
        Parity::Odd => "ungerade",
        Parity::Even => "gerade",
    }
}

pub fn flow_label(f: Flow) -> &'static str {
    match f {
        Flow::None => "kein",
        Flow::Hardware => "RTS/CTS",
        Flow::Software => "XON/XOFF",
    }
}

pub fn big_button(ui: &mut Ui, active: bool, open: &str, close: &str) -> egui::Response {
    let (text, fill) = if active { (close, RED) } else { (open, ACCENT) };
    ui.add_sized(
        [ui.available_width().min(240.0), 30.0],
        egui::Button::new(RichText::new(text).color(Color32::WHITE).strong().size(15.0)).fill(fill),
    )
}

pub fn status_line(ui: &mut Ui, open: bool, starting: bool, on: &str, off: &str) {
    let (txt, col) = if open {
        (on, GREEN)
    } else if starting {
        ("wird geöffnet …", ORANGE)
    } else {
        (off, Color32::GRAY)
    };
    ui.horizontal(|ui| {
        led(ui, col);
        ui.label(RichText::new(txt).color(col));
    });
}

fn lamp(ui: &mut Ui, name: &str, on: bool, enabled: bool) {
    let col = if !enabled {
        Color32::from_gray(150)
    } else if on {
        Color32::from_rgb(40, 190, 80)
    } else {
        RED
    };
    led(ui, col);
    ui.label(RichText::new(name).small()).on_hover_text(match name {
        "CTS" => "Clear To Send",
        "DSR" => "Data Set Ready",
        "RI" => "Ring Indicator",
        _ => "Carrier Detect",
    });
}

/// A round status light (drawn, so it looks the same with every font).
pub fn led(ui: &mut Ui, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 4.5, color);
}
