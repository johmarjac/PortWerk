use std::path::PathBuf;

use eframe::egui::{self, Color32, RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::ascii::AsciiTable;
use crate::link::{self, Cmd, Event, Flow, Link, ModemLines, Parity, SerialConfig};
use crate::terminal::{Dir, SendMode, SendRequest, TermSettings, Terminal};
use crate::rest::{RestClient, RestSettings};
use crate::tools::Tools;

pub const APP_NAME: &str = "PortWerk";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Tab {
    Serial,
    TcpClient,
    TcpServer,
    Udp,
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
            Tab::Rest => "🌐 REST",
            Tab::Tools => "🧮 Werkzeuge",
            Tab::About => "ℹ Info",
        }
    }
}

/// Everything that is remembered between starts.
#[derive(Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    tab: Tab,
    dark: bool,
    serial: SerialConfig,
    dtr: bool,
    rts: bool,
    serial_term: TermSettings,
    tcpc_host: String,
    tcpc_port: u16,
    tcpc_term: TermSettings,
    tcps_bind: String,
    tcps_port: u16,
    tcps_term: TermSettings,
    udp_bind: String,
    udp_port: u16,
    udp_remote_host: String,
    udp_remote_port: u16,
    udp_broadcast: bool,
    udp_reply: bool,
    udp_term: TermSettings,
    rest: RestSettings,
}

impl Default for Saved {
    fn default() -> Self {
        Self {
            tab: Tab::Serial,
            dark: false,
            serial: SerialConfig::default(),
            dtr: true,
            rts: true,
            serial_term: TermSettings::default(),
            tcpc_host: "127.0.0.1".into(),
            tcpc_port: 23,
            tcpc_term: TermSettings::default(),
            tcps_bind: "0.0.0.0".into(),
            tcps_port: 2000,
            tcps_term: TermSettings::default(),
            udp_bind: "0.0.0.0".into(),
            udp_port: 2001,
            udp_remote_host: "127.0.0.1".into(),
            udp_remote_port: 2001,
            udp_broadcast: false,
            udp_reply: false,
            udp_term: TermSettings::default(),
            rest: RestSettings::default(),
        }
    }
}

/// One connection with its terminal.
struct Conn {
    term: Terminal,
    link: Option<Link>,
    open: bool,
    starting: bool,
    needs_target: bool,
}

impl Conn {
    fn new(s: TermSettings, hello: &str) -> Self {
        let mut term = Terminal::new(s);
        term.info(hello);
        Self { term, link: None, open: false, starting: false, needs_target: false }
    }

    fn start(&mut self, link: Link) {
        self.link = Some(link);
        self.starting = true;
    }

    fn stop(&mut self) {
        if let Some(l) = &self.link {
            l.send(Cmd::Close);
        }
    }

    fn active(&self) -> bool {
        self.link.is_some()
    }

    /// Handle worker events and run timers/loops/scripts.
    /// Events the terminal can't show are handed to `other`.
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
}

pub struct PortWerkApp {
    tab: Tab,
    dark: bool,
    // serial
    serial_cfg: SerialConfig,
    serial_ports: Vec<String>,
    dtr: bool,
    rts: bool,
    brk: bool,
    lines: ModemLines,
    serial: Conn,
    // tcp client
    tcpc_host: String,
    tcpc_port: u16,
    tcpc: Conn,
    // tcp server
    tcps_bind: String,
    tcps_port: u16,
    tcps_clients: Vec<String>,
    tcps_target: Option<String>,
    tcps: Conn,
    // udp
    udp_bind: String,
    udp_port: u16,
    udp_remote_host: String,
    udp_remote_port: u16,
    udp_broadcast: bool,
    udp_reply: bool,
    udp_last_sender: Option<String>,
    udp: Conn,
    rest: RestClient,
    tools: Tools,
    ascii: AsciiTable,
    // screenshot mode
    shot: Option<PathBuf>,
    frames: u32,
    shot_requested: bool,
}

impl PortWerkApp {
    pub fn new(cc: &eframe::CreationContext<'_>, shot: Option<PathBuf>) -> Self {
        let saved: Saved = if shot.is_some() {
            Saved::default()
        } else {
            cc.storage.and_then(|s| eframe::get_value(s, eframe::APP_KEY)).unwrap_or_default()
        };
        let ports = link::list_serial_ports();
        let mut serial_cfg = saved.serial.clone();
        if serial_cfg.port.is_empty() {
            serial_cfg.port = ports.first().cloned().unwrap_or_else(|| default_port_name().into());
        }
        let app = Self {
            tab: saved.tab,
            dark: saved.dark,
            serial_cfg,
            serial_ports: ports,
            dtr: saved.dtr,
            rts: saved.rts,
            brk: false,
            lines: ModemLines::default(),
            serial: Conn::new(saved.serial_term, "Bereit. Schnittstelle wählen und „Öffnen“ klicken."),
            tcpc_host: saved.tcpc_host,
            tcpc_port: saved.tcpc_port,
            tcpc: Conn::new(saved.tcpc_term, "Bereit. Adresse und Port eingeben und „Verbinden“ klicken."),
            tcps_bind: saved.tcps_bind,
            tcps_port: saved.tcps_port,
            tcps_clients: Vec::new(),
            tcps_target: None,
            tcps: Conn::new(saved.tcps_term, "Bereit. Port wählen und „Starten“ klicken."),
            udp_bind: saved.udp_bind,
            udp_port: saved.udp_port,
            udp_remote_host: saved.udp_remote_host,
            udp_remote_port: saved.udp_remote_port,
            udp_broadcast: saved.udp_broadcast,
            udp_reply: saved.udp_reply,
            udp_last_sender: None,
            udp: Conn { needs_target: true, ..Conn::new(saved.udp_term, "Bereit. Lokalen Port öffnen, dann an Ziel senden.") },
            rest: RestClient::new(saved.rest),
            tools: Tools::default(),
            ascii: AsciiTable::default(),
            shot,
            frames: 0,
            shot_requested: false,
        };
        app.apply_theme(&cc.egui_ctx);
        app
    }

    pub fn apply_start_args(&mut self, tab: Option<&str>, mode: Option<&str>, ascii: bool) {
        if let Some(t) = tab {
            self.tab = match t {
                "tcpc" => Tab::TcpClient,
                "tcps" => Tab::TcpServer,
                "udp" => Tab::Udp,
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
            for c in [&mut self.serial, &mut self.tcpc, &mut self.tcps, &mut self.udp] {
                c.term.s.mode = m;
            }
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

    fn snapshot(&self) -> Saved {
        Saved {
            tab: self.tab,
            dark: self.dark,
            serial: self.serial_cfg.clone(),
            dtr: self.dtr,
            rts: self.rts,
            serial_term: self.serial.term.s.clone(),
            tcpc_host: self.tcpc_host.clone(),
            tcpc_port: self.tcpc_port,
            tcpc_term: self.tcpc.term.s.clone(),
            tcps_bind: self.tcps_bind.clone(),
            tcps_port: self.tcps_port,
            tcps_term: self.tcps.term.s.clone(),
            udp_bind: self.udp_bind.clone(),
            udp_port: self.udp_port,
            udp_remote_host: self.udp_remote_host.clone(),
            udp_remote_port: self.udp_remote_port,
            udp_broadcast: self.udp_broadcast,
            udp_reply: self.udp_reply,
            udp_term: self.udp.term.s.clone(),
            rest: self.rest.s.clone(),
        }
    }

    fn udp_target(&self) -> Option<String> {
        if self.udp_reply {
            self.udp_last_sender.clone()
        } else {
            Some(format!("{}:{}", self.udp_remote_host.trim(), self.udp_remote_port))
        }
    }

    fn pump_all(&mut self, ctx: &egui::Context) {
        let lines = &mut self.lines;
        self.serial.pump(ctx, None, |e| {
            if let Event::Lines(l) = e {
                *lines = *l;
            } else if let Event::Closed = e {
                *lines = ModemLines::default();
            }
        });
        self.tcpc.pump(ctx, None, |_| {});
        let clients = &mut self.tcps_clients;
        let target = &mut self.tcps_target;
        let to = target.clone();
        self.tcps.pump(ctx, to, |e| match e {
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
        let to = self.udp_target();
        let last = &mut self.udp_last_sender;
        self.udp.pump(ctx, to, |e| {
            if let Event::Rx { peer: Some(p), .. } = e {
                *last = Some(p.clone());
            }
        });
    }

    // ------------------------------------------------------------ layout helpers

    /// Right settings panel + bottom send area + log in the middle.
    fn conn_frame(
        ui: &mut Ui,
        id: &str,
        conn: &mut Conn,
        settings: impl FnOnce(&mut Ui, &mut Conn),
    ) -> Vec<SendRequest> {
        egui::Panel::right(format!("{id}_cfg"))
            .resizable(true)
            .default_size(270.0)
            .min_size(230.0)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(10.0))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| settings(ui, conn));
            });
        let connected = conn.open;
        let mut reqs = Vec::new();
        egui::Panel::bottom(format!("{id}_send"))
            .resizable(false)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(10, 8)))
            .show(ui, |ui| {
                reqs = conn.term.send_ui(ui, connected);
            });
        egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(10.0)).show(ui, |ui| {
            conn.term.toolbar_ui(ui);
            ui.add_space(2.0);
            conn.term.log_ui(ui);
        });
        reqs
    }

    // ------------------------------------------------------------ tabs

    fn serial_tab(&mut self, ui: &mut Ui) {
        let cfg = &mut self.serial_cfg;
        let ports = &mut self.serial_ports;
        let (dtr, rts, brk, lines) = (&mut self.dtr, &mut self.rts, &mut self.brk, self.lines);
        let reqs = Self::conn_frame(ui, "serial", &mut self.serial, |ui, conn| {
            section(ui, "Schnittstelle");
            let locked = conn.active();
            ui.add_enabled_ui(!locked, |ui| {
                egui::Grid::new("serial_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Port");
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt("port")
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
                        egui::ComboBox::from_id_salt("baud").selected_text(cfg.baud.to_string()).width(80.0).show_ui(
                            ui,
                            |ui| {
                                for b in [
                                    300, 600, 1200, 2400, 4800, 9600, 14400, 19200, 28800, 38400, 57600, 115200,
                                    230400, 460800, 921600,
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
                    egui::ComboBox::from_id_salt("parity")
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
                    egui::ComboBox::from_id_salt("flow").selected_text(flow_label(cfg.flow)).width(110.0).show_ui(
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
            status_line(ui, conn, "offen", "geschlossen");

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
                if ui.add_enabled(conn.open, egui::Button::new("BREAK").selected(*brk))
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
        self.serial.send(None, reqs);
    }

    fn tcp_client_tab(&mut self, ui: &mut Ui) {
        let host = &mut self.tcpc_host;
        let port = &mut self.tcpc_port;
        let reqs = Self::conn_frame(ui, "tcpc", &mut self.tcpc, |ui, conn| {
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
            status_line(ui, conn, "verbunden", "getrennt");
            section(ui, "Hinweis");
            ui.label(RichText::new("Verbindet sich als Client mit einem TCP-Server, z. B. einem Gerät, einem Telnet-Dienst oder dem eigenen TCP-Server-Reiter.").weak());
        });
        self.tcpc.send(None, reqs);
    }

    fn tcp_server_tab(&mut self, ui: &mut Ui) {
        let bind = &mut self.tcps_bind;
        let port = &mut self.tcps_port;
        let clients = &self.tcps_clients;
        let target = &mut self.tcps_target;
        let reqs = Self::conn_frame(ui, "tcps", &mut self.tcps, |ui, conn| {
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
            status_line(ui, conn, "lauscht", "gestoppt");
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
        let to = self.tcps_target.clone();
        self.tcps.send(to, reqs);
    }

    fn udp_tab(&mut self, ui: &mut Ui) {
        let bind = &mut self.udp_bind;
        let port = &mut self.udp_port;
        let rhost = &mut self.udp_remote_host;
        let rport = &mut self.udp_remote_port;
        let bcast = &mut self.udp_broadcast;
        let reply = &mut self.udp_reply;
        let last = &self.udp_last_sender;
        let reqs = Self::conn_frame(ui, "udp", &mut self.udp, |ui, conn| {
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
            status_line(ui, conn, "offen", "geschlossen");
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
            ui.label(
                RichText::new(format!("Letzter Absender: {}", last.as_deref().unwrap_or("–")))
                    .small()
                    .weak(),
            );
        });
        let to = self.udp_target();
        self.udp.send(to, reqs);
    }

    fn about_tab(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(24.0)).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading(RichText::new(format!("{APP_NAME} {APP_VERSION}")).size(26.0).strong());
                ui.label("Werkzeug für Entwickler: serielle Schnittstellen, TCP-Client, TCP-Server und UDP in einem Fenster.");
                ui.add_space(12.0);
                section(ui, "Was es kann");
                for t in [
                    "Serielle Ports öffnen (Baudrate, Datenbits, Parität, Stoppbits, Handshake), DTR/RTS/BREAK schalten, CTS/DSR/RI/CD beobachten",
                    "Als TCP-Client mit Geräten oder Diensten verbinden",
                    "Als TCP-Server mehrere Clients annehmen, an alle oder einzelne senden, Clients trennen",
                    "UDP-Pakete senden und empfangen, auch Broadcast und Antwort an den letzten Absender",
                    "REST-Client: GET/POST/PUT/PATCH/DELETE, Parameter, Header, JSON/Text/Formular-Body, Anmeldung (Token, Benutzer, API-Schlüssel), Variablen wie {{baseUrl}}, Verlauf, cURL-Export",
                    "Anzeige als Text, Hex, Hex + Text oder Dezimal, mit Uhrzeit und Steuerzeichen",
                    "Beliebig viele Sendezeilen, als Text mit $XX-/#DDD-Escapes oder als Hex, mit automatischer Wiederholung",
                    "Schleife: mehrere Zeilen nacheinander mit eigenen Pausen senden, x-mal oder endlos",
                    "Skripte (Lua) für Protokolle: senden, warten, auf Antworten reagieren, Prüfsummen berechnen (mit Beispielen)",
                    "ASCII-Tabelle (Dezimal, Hex, Binär, Zeichen) – ein Klick fügt das Zeichen in die Sendezeile ein",
                    "Echo-Modus, Pause, Fensterinhalt speichern und laufende Mitschrift in eine Datei",
                    "Werkzeuge: Umrechner Text/Hex/Dezimal/Binär, Prüfsummen (Summe, XOR, CRC-8, CRC-16 Modbus/CCITT, CRC-32), Zahlenumrechner",
                    "Alle Einstellungen werden beim Beenden gespeichert",
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
        self.pump_all(&ctx);

        // ---- top bar with tabs
        egui::Panel::top("tabs")
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(10, 6)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(APP_NAME).strong().size(18.0).color(Color32::from_rgb(0, 130, 160)));
                    ui.add_space(12.0);
                    for t in [Tab::Serial, Tab::TcpClient, Tab::TcpServer, Tab::Udp, Tab::Rest, Tab::Tools, Tab::About] {
                        let conn = match t {
                            Tab::Serial => Some(&self.serial),
                            Tab::TcpClient => Some(&self.tcpc),
                            Tab::TcpServer => Some(&self.tcps),
                            Tab::Udp => Some(&self.udp),
                            _ => None,
                        };
                        let active = conn.is_some_and(|c| c.open);
                        let busy = conn.is_some_and(|c| c.term.busy()) || (t == Tab::Rest && self.rest.busy());
                        let label = if busy { format!("{} 🔁", t.label()) } else { t.label().to_string() };
                        let mut text = RichText::new(label).size(14.5);
                        if active {
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
                        ui.toggle_value(&mut self.ascii.open, "🔤 ASCII-Tabelle")
                            .on_hover_text("Tabelle mit Dezimal-, Hex- und Zeichenwerten einblenden");
                    });
                });
            });

        // ---- status bar
        egui::Panel::bottom("status")
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(10, 4)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let ser = if self.serial.open { format!("Seriell {}", self.serial_cfg.port) } else { "Seriell".into() };
                    dot(ui, &ser, self.serial.open);
                    dot(ui, "TCP-Client", self.tcpc.open);
                    let srv = if self.tcps.open {
                        format!("TCP-Server :{} ({} Clients)", self.tcps_port, self.tcps_clients.len())
                    } else {
                        "TCP-Server".into()
                    };
                    dot(ui, &srv, self.tcps.open);
                    let udp = if self.udp.open { format!("UDP :{}", self.udp_port) } else { "UDP".into() };
                    dot(ui, &udp, self.udp.open);
                });
            });

        match self.tab {
            Tab::Serial => self.serial_tab(ui),
            Tab::TcpClient => self.tcp_client_tab(ui),
            Tab::TcpServer => self.tcp_server_tab(ui),
            Tab::Udp => self.udp_tab(ui),
            Tab::Rest => self.rest.ui(ui),
            Tab::Tools => self.tools.ui(ui),
            Tab::About => self.about_tab(ui),
        }

        if let Some(b) = self.ascii.show(&ctx) {
            match self.tab {
                Tab::Serial => self.serial.term.insert_byte(b),
                Tab::TcpClient => self.tcpc.term.insert_byte(b),
                Tab::TcpServer => self.tcps.term.insert_byte(b),
                Tab::Udp => self.udp.term.insert_byte(b),
                Tab::Tools => self.tools.insert_byte(b),
                Tab::Rest | Tab::About => {}
            }
        }

        // ---- screenshot mode
        if let Some(path) = self.shot.clone() {
            self.frames += 1;
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
            ctx.request_repaint();
            if self.frames >= 120 && !self.shot_requested {
                self.shot_requested = true;
                let c2 = ctx.clone();
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
                    let _ = c2;
                    std::process::exit(code);
                });
            }
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if self.shot.is_none() {
            eframe::set_value(storage, eframe::APP_KEY, &self.snapshot());
        }
    }

    fn on_exit(&mut self) {
        self.serial.stop();
        self.tcpc.stop();
        self.tcps.stop();
        self.udp.stop();
    }
}

// ---------------------------------------------------------------- small widgets

fn default_port_name() -> &'static str {
    if cfg!(windows) { "COM1" } else { "/dev/ttyUSB0" }
}

fn parity_label(p: Parity) -> &'static str {
    match p {
        Parity::None => "keine",
        Parity::Odd => "ungerade",
        Parity::Even => "gerade",
    }
}

fn flow_label(f: Flow) -> &'static str {
    match f {
        Flow::None => "kein",
        Flow::Hardware => "RTS/CTS",
        Flow::Software => "XON/XOFF",
    }
}

pub fn section(ui: &mut Ui, title: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).strong().size(14.0));
    ui.separator();
}

fn big_button(ui: &mut Ui, active: bool, open: &str, close: &str) -> egui::Response {
    let (text, fill) = if active {
        (close, Color32::from_rgb(200, 70, 60))
    } else {
        (open, Color32::from_rgb(0, 130, 160))
    };
    ui.add_sized(
        [ui.available_width().min(240.0), 30.0],
        egui::Button::new(RichText::new(text).color(Color32::WHITE).strong().size(15.0)).fill(fill),
    )
}

fn status_line(ui: &mut Ui, conn: &Conn, on: &str, off: &str) {
    let (txt, col) = if conn.open {
        (on, Color32::from_rgb(30, 160, 70))
    } else if conn.starting {
        ("wird geöffnet …", Color32::from_rgb(220, 150, 0))
    } else {
        (off, Color32::GRAY)
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new("●").color(col));
        ui.label(RichText::new(txt).color(col));
    });
}

fn lamp(ui: &mut Ui, name: &str, on: bool, enabled: bool) {
    let col = if !enabled {
        Color32::from_gray(150)
    } else if on {
        Color32::from_rgb(40, 190, 80)
    } else {
        Color32::from_rgb(200, 70, 60)
    };
    ui.label(RichText::new("●").color(col));
    ui.label(RichText::new(name).small()).on_hover_text(match name {
        "CTS" => "Clear To Send",
        "DSR" => "Data Set Ready",
        "RI" => "Ring Indicator",
        _ => "Carrier Detect",
    });
}

fn dot(ui: &mut Ui, label: &str, on: bool) {
    let col = if on { Color32::from_rgb(30, 160, 70) } else { Color32::from_gray(150) };
    ui.label(RichText::new("●").color(col));
    ui.label(RichText::new(label).size(12.5));
    ui.add_space(10.0);
}
