//! Modbus TCP server (slave / device simulator). Holds coils, discrete
//! inputs, holding and input registers that can be edited in a table,
//! answers any number of clients and can change values automatically.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::app::section;
use crate::modbus::{Format, exception_text, format_regs};
use crate::sessions::{GREEN, Session, Shared, big_button, status_line};
use crate::terminal::{Dir, TermSettings, Terminal, View};

const SIZE: usize = 65536;

// ------------------------------------------------------------------ data store

pub struct Store {
    pub coils: Vec<bool>,
    pub discrete: Vec<bool>,
    pub holding: Vec<u16>,
    pub input: Vec<u16>,
}

impl Store {
    pub fn new() -> Self {
        Self { coils: vec![false; SIZE], discrete: vec![false; SIZE], holding: vec![0; SIZE], input: vec![0; SIZE] }
    }
}

fn exception(fc: u8, code: u8) -> Vec<u8> {
    vec![fc | 0x80, code]
}

fn pack_bits(bits: &[bool]) -> Vec<u8> {
    let mut out = vec![0u8; bits.len().div_ceil(8)];
    for (i, b) in bits.iter().enumerate() {
        if *b {
            out[i / 8] |= 1 << (i % 8);
        }
    }
    out
}

/// Answer one request PDU. Returns the response PDU and a short description.
pub fn handle_pdu(store: &mut Store, pdu: &[u8]) -> (Vec<u8>, String) {
    let Some(&fc) = pdu.first() else { return (exception(0, 3), "leere Anfrage".into()) };
    let word = |i: usize| -> Option<u16> { Some(u16::from_be_bytes([*pdu.get(i)?, *pdu.get(i + 1)?])) };
    let (Some(addr), Some(arg)) = (word(1), word(3)) else {
        return (exception(fc, 3), format!("FC {fc}: Anfrage zu kurz"));
    };
    let a = addr as usize;
    match fc {
        1 | 2 => {
            let qty = arg as usize;
            if !(1..=2000).contains(&qty) {
                return (exception(fc, 3), format!("FC {fc:02}: ungültige Anzahl {qty}"));
            }
            if a + qty > SIZE {
                return (exception(fc, 2), format!("FC {fc:02}: Adressbereich überschritten"));
            }
            let src = if fc == 1 { &store.coils } else { &store.discrete };
            let bytes = pack_bits(&src[a..a + qty]);
            let mut r = vec![fc, bytes.len() as u8];
            r.extend_from_slice(&bytes);
            let what = if fc == 1 { "Coils" } else { "Eingänge" };
            (r, format!("{what} lesen ab {addr}, {qty} Stück"))
        }
        3 | 4 => {
            let qty = arg as usize;
            if !(1..=125).contains(&qty) {
                return (exception(fc, 3), format!("FC {fc:02}: ungültige Anzahl {qty}"));
            }
            if a + qty > SIZE {
                return (exception(fc, 2), format!("FC {fc:02}: Adressbereich überschritten"));
            }
            let src = if fc == 3 { &store.holding } else { &store.input };
            let mut r = vec![fc, (qty * 2) as u8];
            for v in &src[a..a + qty] {
                r.extend_from_slice(&v.to_be_bytes());
            }
            let what = if fc == 3 { "Holding-Register" } else { "Input-Register" };
            (r, format!("{what} lesen ab {addr}, {qty} Stück"))
        }
        5 => {
            let on = match arg {
                0xFF00 => true,
                0x0000 => false,
                _ => return (exception(fc, 3), "Coil schreiben: Wert muss FF00 oder 0000 sein".into()),
            };
            store.coils[a] = on;
            (pdu[..5].to_vec(), format!("Coil {addr} = {}", if on { "an" } else { "aus" }))
        }
        6 => {
            store.holding[a] = arg;
            (pdu[..5].to_vec(), format!("Register {addr} = {arg}"))
        }
        15 | 16 => {
            let qty = arg as usize;
            let count = pdu.get(5).copied().unwrap_or(0) as usize;
            let data = pdu.get(6..6 + count);
            let max = if fc == 15 { 1968 } else { 123 };
            let need = if fc == 15 { qty.div_ceil(8) } else { qty * 2 };
            if !(1..=max).contains(&qty) || count != need || data.is_none() {
                return (exception(fc, 3), format!("FC {fc}: ungültige Anzahl oder Länge"));
            }
            if a + qty > SIZE {
                return (exception(fc, 2), format!("FC {fc}: Adressbereich überschritten"));
            }
            let data = data.unwrap();
            if fc == 15 {
                for i in 0..qty {
                    store.coils[a + i] = data[i / 8] & (1 << (i % 8)) != 0;
                }
            } else {
                for i in 0..qty {
                    store.holding[a + i] = u16::from_be_bytes([data[i * 2], data[i * 2 + 1]]);
                }
            }
            let mut r = vec![fc];
            r.extend_from_slice(&pdu[1..5]);
            let what = if fc == 15 { "Coils" } else { "Register" };
            (r, format!("{what} schreiben ab {addr}, {qty} Stück"))
        }
        _ => (exception(fc, 1), format!("Funktion {fc} wird nicht unterstützt")),
    }
}

// ------------------------------------------------------------------ worker

enum SrvCmd {
    Kick(String),
    Close,
}

enum SrvEvent {
    Opened(String),
    Closed,
    Error(String),
    ClientUp(String),
    ClientDown(String),
    Frame { peer: String, rx: bool, data: Vec<u8> },
    Request { peer: String, text: String, ok: bool },
}

struct SrvLink {
    tx: Sender<SrvCmd>,
    rx: Receiver<SrvEvent>,
}

impl Drop for SrvLink {
    fn drop(&mut self) {
        let _ = self.tx.send(SrvCmd::Close);
    }
}

struct Client {
    name: String,
    stream: TcpStream,
    buf: Vec<u8>,
}

fn spawn(ctx: &egui::Context, bind: String, port: u16, unit_only: Option<u8>, store: Arc<Mutex<Store>>, traffic: bool) -> SrvLink {
    let (cmd_tx, cmd_rx) = channel();
    let (ev_tx, ev_rx) = channel();
    let ctx = ctx.clone();
    thread::spawn(move || {
        let out = |e: SrvEvent| {
            let _ = ev_tx.send(e);
            ctx.request_repaint();
        };
        let listener = match TcpListener::bind((bind.trim(), port)) {
            Ok(l) => l,
            Err(e) => {
                let hint = if port < 1024 && !cfg!(windows) { " (Ports unter 1024 brauchen Administratorrechte)" } else { "" };
                out(SrvEvent::Error(format!("Port {port} kann nicht geöffnet werden: {e}{hint}")));
                out(SrvEvent::Closed);
                return;
            }
        };
        let _ = listener.set_nonblocking(true);
        let local = listener.local_addr().map(|a| a.to_string()).unwrap_or_default();
        out(SrvEvent::Opened(local));
        let mut clients: Vec<Client> = Vec::new();
        let mut tmp = [0u8; 2048];
        'outer: loop {
            let mut busy = false;
            match cmd_rx.try_recv() {
                Ok(SrvCmd::Close) | Err(TryRecvError::Disconnected) => break 'outer,
                Ok(SrvCmd::Kick(name)) => {
                    clients.retain(|c| {
                        if c.name == name {
                            let _ = c.stream.shutdown(std::net::Shutdown::Both);
                            false
                        } else {
                            true
                        }
                    });
                    out(SrvEvent::ClientDown(name));
                }
                Err(TryRecvError::Empty) => {}
            }
            while let Ok((s, a)) = listener.accept() {
                let _ = s.set_nonblocking(true);
                let _ = s.set_nodelay(true);
                out(SrvEvent::ClientUp(a.to_string()));
                clients.push(Client { name: a.to_string(), stream: s, buf: Vec::new() });
                busy = true;
            }
            let mut gone = Vec::new();
            for (ci, c) in clients.iter_mut().enumerate() {
                match c.stream.read(&mut tmp) {
                    Ok(0) => {
                        gone.push(ci);
                        continue;
                    }
                    Ok(n) => {
                        c.buf.extend_from_slice(&tmp[..n]);
                        busy = true;
                    }
                    Err(e) if e.kind() == ErrorKind::WouldBlock => {}
                    Err(_) => {
                        gone.push(ci);
                        continue;
                    }
                }
                // handle every complete frame in the buffer
                while c.buf.len() >= 7 {
                    let len = u16::from_be_bytes([c.buf[4], c.buf[5]]) as usize;
                    if !(2..=254).contains(&len) {
                        c.buf.clear(); // garbage, resync
                        break;
                    }
                    if c.buf.len() < 6 + len {
                        break;
                    }
                    let frame: Vec<u8> = c.buf.drain(..6 + len).collect();
                    if traffic {
                        out(SrvEvent::Frame { peer: c.name.clone(), rx: true, data: frame.clone() });
                    }
                    let unit = frame[6];
                    if let Some(u) = unit_only {
                        if unit != u {
                            out(SrvEvent::Request { peer: c.name.clone(), text: format!("Geräteadresse {unit} ignoriert"), ok: false });
                            continue;
                        }
                    }
                    let (resp, text) = {
                        let mut st = store.lock().unwrap();
                        handle_pdu(&mut st, &frame[7..])
                    };
                    let ok = resp[0] & 0x80 == 0;
                    let text = if ok { text } else { format!("{text} → Ausnahme {:02}: {}", resp[1], exception_text(resp[1])) };
                    let mut answer = frame[..4].to_vec();
                    answer.extend_from_slice(&((resp.len() + 1) as u16).to_be_bytes());
                    answer.push(unit);
                    answer.extend_from_slice(&resp);
                    let _ = c.stream.set_nonblocking(false);
                    let _ = c.stream.write_all(&answer);
                    let _ = c.stream.set_nonblocking(true);
                    if traffic {
                        out(SrvEvent::Frame { peer: c.name.clone(), rx: false, data: answer });
                    }
                    out(SrvEvent::Request { peer: c.name.clone(), text, ok });
                }
            }
            for i in gone.into_iter().rev() {
                let c = clients.remove(i);
                out(SrvEvent::ClientDown(c.name));
            }
            if !busy {
                thread::sleep(Duration::from_millis(5));
            }
        }
        for c in &clients {
            let _ = c.stream.shutdown(std::net::Shutdown::Both);
        }
        out(SrvEvent::Closed);
    });
    SrvLink { tx: cmd_tx, rx: ev_rx }
}

// ------------------------------------------------------------------ session

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Area {
    Coils,
    Discrete,
    Holding,
    Input,
}

impl Area {
    fn label(self) -> &'static str {
        match self {
            Area::Coils => "Coils (01/05/15)",
            Area::Discrete => "Diskrete Eingänge (02)",
            Area::Holding => "Holding-Register (03/06/16)",
            Area::Input => "Input-Register (04)",
        }
    }
    fn is_bits(self) -> bool {
        matches!(self, Area::Coils | Area::Discrete)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Sim {
    Off,
    Count,
    Random,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModbusServerSaved {
    pub name: String,
    pub bind: String,
    pub port: u16,
    pub unit_only: bool,
    pub unit: u8,
    pub area: Area,
    pub view_start: u16,
    pub view_count: u16,
    pub format: Format,
    pub log_traffic: bool,
    pub sim: Sim,
    pub sim_ms: u64,
    /// Only non-zero values are stored.
    pub coils: Vec<u16>,
    pub discrete: Vec<u16>,
    pub holding: Vec<(u16, u16)>,
    pub input: Vec<(u16, u16)>,
}

impl Default for ModbusServerSaved {
    fn default() -> Self {
        Self {
            name: "Modbus-Server 1".into(),
            bind: "0.0.0.0".into(),
            port: 502,
            unit_only: false,
            unit: 1,
            area: Area::Holding,
            view_start: 0,
            view_count: 50,
            format: Format::Unsigned,
            log_traffic: true,
            sim: Sim::Off,
            sim_ms: 1000,
            coils: Vec::new(),
            discrete: Vec::new(),
            holding: Vec::new(),
            input: Vec::new(),
        }
    }
}

pub struct ModbusServerSession {
    s: ModbusServerSaved,
    store: Arc<Mutex<Store>>,
    link: Option<SrvLink>,
    open: bool,
    starting: bool,
    local: String,
    clients: Vec<String>,
    requests: u64,
    errors: u64,
    last_sim: Instant,
    rng: u64,
    log: Terminal,
}

impl ModbusServerSession {
    pub fn new(name: String, port: u16) -> Self {
        Self::from_saved(ModbusServerSaved { name, port, ..Default::default() })
    }

    pub fn from_saved(s: ModbusServerSaved) -> Self {
        let mut st = Store::new();
        for a in &s.coils {
            st.coils[*a as usize] = true;
        }
        for a in &s.discrete {
            st.discrete[*a as usize] = true;
        }
        for (a, v) in &s.holding {
            st.holding[*a as usize] = *v;
        }
        for (a, v) in &s.input {
            st.input[*a as usize] = *v;
        }
        let mut log = Terminal::new(TermSettings { view: View::Hex, ..TermSettings::default() });
        log.info("Bereit. Port wählen und „Starten“ klicken. Werte in der Tabelle können jederzeit geändert werden.");
        Self {
            s,
            store: Arc::new(Mutex::new(st)),
            link: None,
            open: false,
            starting: false,
            local: String::new(),
            clients: Vec::new(),
            requests: 0,
            errors: 0,
            last_sim: Instant::now(),
            rng: 0x2545_F491_4F6C_DD1D,
            log,
        }
    }

    pub fn saved(&self) -> ModbusServerSaved {
        let mut s = self.s.clone();
        let st = self.store.lock().unwrap();
        s.coils = (0..SIZE).filter(|&i| st.coils[i]).map(|i| i as u16).collect();
        s.discrete = (0..SIZE).filter(|&i| st.discrete[i]).map(|i| i as u16).collect();
        s.holding = (0..SIZE).filter(|&i| st.holding[i] != 0).map(|i| (i as u16, st.holding[i])).collect();
        s.input = (0..SIZE).filter(|&i| st.input[i] != 0).map(|i| (i as u16, st.input[i])).collect();
        s
    }

    pub fn port(&self) -> u16 {
        self.s.port
    }

    fn rand(&mut self) -> u64 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        self.rng
    }

    fn view_range(&self) -> std::ops::Range<usize> {
        let a = self.s.view_start as usize;
        a..(a + self.s.view_count as usize).min(SIZE)
    }

    fn simulate(&mut self) {
        let range = self.view_range();
        let area = self.s.area;
        let sim = self.s.sim;
        let randoms: Vec<u64> = range.clone().map(|_| self.rand()).collect();
        let mut st = self.store.lock().unwrap();
        for (k, i) in range.enumerate() {
            match (area, sim) {
                (_, Sim::Off) => {}
                (Area::Coils, Sim::Count) => st.coils[i] = !st.coils[i],
                (Area::Discrete, Sim::Count) => st.discrete[i] = !st.discrete[i],
                (Area::Holding, Sim::Count) => st.holding[i] = st.holding[i].wrapping_add(1),
                (Area::Input, Sim::Count) => st.input[i] = st.input[i].wrapping_add(1),
                (Area::Coils, Sim::Random) => st.coils[i] = randoms[k] & 1 == 1,
                (Area::Discrete, Sim::Random) => st.discrete[i] = randoms[k] & 1 == 1,
                (Area::Holding, Sim::Random) => st.holding[i] = randoms[k] as u16,
                (Area::Input, Sim::Random) => st.input[i] = randoms[k] as u16,
            }
        }
    }

    fn settings_ui(&mut self, ui: &mut Ui) {
        let active = self.link.is_some();
        section(ui, "Server");
        ui.add_enabled_ui(!active, |ui| {
            egui::Grid::new("mbs_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Lokale IP");
                ui.add(egui::TextEdit::singleline(&mut self.s.bind).desired_width(150.0))
                    .on_hover_text("0.0.0.0 = auf allen Netzwerkkarten");
                ui.end_row();
                ui.label("Port");
                ui.add(egui::DragValue::new(&mut self.s.port).range(1..=65535)).on_hover_text("Standard für Modbus TCP: 502");
                ui.end_row();
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.s.unit_only, "nur Geräteadresse");
                ui.add_enabled(self.s.unit_only, egui::DragValue::new(&mut self.s.unit).range(0..=255));
            })
            .response
            .on_hover_text("Ohne Haken beantwortet der Server jede Geräteadresse (Unit-ID).");
            ui.checkbox(&mut self.s.log_traffic, "Datenverkehr als Hex protokollieren");
        });
        ui.add_space(6.0);
        if big_button(ui, active, "Starten", "Stoppen").clicked() {
            if active {
                self.link = None;
                self.open = false;
                self.starting = false;
                self.clients.clear();
                self.log.info("Server gestoppt");
            } else {
                let unit = self.s.unit_only.then_some(self.s.unit);
                self.link = Some(spawn(ui.ctx(), self.s.bind.clone(), self.s.port, unit, self.store.clone(), self.s.log_traffic));
                self.starting = true;
            }
        }
        status_line(ui, self.open, self.starting, "lauscht", "gestoppt");
        if self.open {
            ui.label(RichText::new(format!("Erreichbar unter {}", self.local)).small().weak());
        }

        section(ui, &format!("Clients ({})", self.clients.len()));
        if self.clients.is_empty() {
            ui.label(RichText::new("Noch kein Client verbunden.").weak().italics());
        }
        let mut kick = None;
        for c in &self.clients {
            ui.horizontal(|ui| {
                ui.label(RichText::new("●").color(GREEN));
                ui.label(c);
                if ui.small_button("✖").on_hover_text("Verbindung trennen").clicked() {
                    kick = Some(c.clone());
                }
            });
        }
        if let (Some(k), Some(l)) = (kick, &self.link) {
            let _ = l.tx.send(SrvCmd::Kick(k));
        }

        section(ui, "Statistik");
        ui.label(format!("Anfragen: {}", self.requests));
        ui.label(format!("davon Fehler: {}", self.errors));
        if ui.small_button("Zähler zurücksetzen").clicked() {
            self.requests = 0;
            self.errors = 0;
        }
    }

    fn table_ui(&mut self, ui: &mut Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Bereich").strong());
            egui::ComboBox::from_id_salt("mbs_area").selected_text(self.s.area.label()).width(210.0).show_ui(ui, |ui| {
                for a in [Area::Coils, Area::Discrete, Area::Holding, Area::Input] {
                    ui.selectable_value(&mut self.s.area, a, a.label());
                }
            });
            ui.label("ab Adresse");
            ui.add(egui::DragValue::new(&mut self.s.view_start).range(0..=65535));
            ui.label("Anzahl");
            ui.add(egui::DragValue::new(&mut self.s.view_count).range(1..=2000));
            if !self.s.area.is_bits() {
                ui.label("Anzeige");
                egui::ComboBox::from_id_salt("mbs_format").selected_text(self.s.format.label()).width(220.0).show_ui(ui, |ui| {
                    for f in Format::ALL {
                        ui.selectable_value(&mut self.s.format, f, f.label());
                    }
                });
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Simulation").strong());
            ui.selectable_value(&mut self.s.sim, Sim::Off, "aus");
            ui.selectable_value(&mut self.s.sim, Sim::Count, if self.s.area.is_bits() { "umschalten" } else { "hochzählen" });
            ui.selectable_value(&mut self.s.sim, Sim::Random, "Zufall");
            ui.label("alle");
            ui.add(egui::DragValue::new(&mut self.s.sim_ms).range(50..=3_600_000).suffix(" ms").speed(10.0));
            ui.label(RichText::new("(gilt für die angezeigten Adressen)").small().weak());
            ui.separator();
            if ui.button("Alle auf 0").on_hover_text("Angezeigte Adressen zurücksetzen").clicked() {
                let r = self.view_range();
                let mut st = self.store.lock().unwrap();
                match self.s.area {
                    Area::Coils => st.coils[r].fill(false),
                    Area::Discrete => st.discrete[r].fill(false),
                    Area::Holding => st.holding[r].fill(0),
                    Area::Input => st.input[r].fill(0),
                }
            }
        });
        ui.add_space(4.0);

        let range = self.view_range();
        let area = self.s.area;
        let format = self.s.format;
        let dark = ui.visuals().dark_mode;
        let store = self.store.clone();
        egui::Frame::new()
            .fill(if dark { Color32::from_gray(18) } else { Color32::WHITE })
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
            .corner_radius(4.0)
            .inner_margin(6.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                let mut st = store.lock().unwrap();
                let row_h = ui.spacing().interact_size.y + 4.0;
                ui.horizontal(|ui| {
                    ui.add_sized([80.0, 18.0], egui::Label::new(RichText::new("Adresse").strong()));
                    ui.add_sized([140.0, 18.0], egui::Label::new(RichText::new("Wert").strong()));
                    if !area.is_bits() {
                        ui.add_sized([90.0, 18.0], egui::Label::new(RichText::new("Hex").strong()));
                        ui.label(RichText::new("Anzeige").strong());
                    }
                });
                ui.separator();
                let shown: Vec<(u16, String, String)> = if area.is_bits() {
                    Vec::new()
                } else {
                    let regs = if area == Area::Holding { &st.holding[range.clone()] } else { &st.input[range.clone()] };
                    format_regs(range.start as u16, regs, format)
                };
                egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, range.len(), |ui, rows| {
                    for k in rows {
                        let i = range.start + k;
                        ui.horizontal(|ui| {
                            ui.add_sized([80.0, 18.0], egui::Label::new(RichText::new(i.to_string()).monospace()));
                            match area {
                                Area::Coils | Area::Discrete => {
                                    let v = if area == Area::Coils { &mut st.coils[i] } else { &mut st.discrete[i] };
                                    let label = if *v { "1 (an)" } else { "0 (aus)" };
                                    ui.checkbox(v, RichText::new(label).color(if *v { GREEN } else { Color32::GRAY }));
                                }
                                Area::Holding | Area::Input => {
                                    let v = if area == Area::Holding { &mut st.holding[i] } else { &mut st.input[i] };
                                    ui.add_sized([140.0, 20.0], egui::DragValue::new(v).range(0..=65535));
                                    ui.add_sized([90.0, 18.0], egui::Label::new(RichText::new(format!("{:04X}", *v)).monospace().weak()));
                                    let disp = shown.iter().find(|r| r.0 as usize == i).map(|r| r.1.clone()).unwrap_or_default();
                                    ui.monospace(disp);
                                }
                            }
                        });
                    }
                });
            });
    }
}

impl Session for ModbusServerSession {
    fn name(&self) -> &str {
        &self.s.name
    }
    fn set_name(&mut self, n: String) {
        self.s.name = n;
    }
    fn is_open(&self) -> bool {
        self.open
    }
    fn is_busy(&self) -> bool {
        self.s.sim != Sim::Off
    }
    fn stop(&mut self) {
        self.link = None;
        self.open = false;
    }
    fn duplicate(&self) -> Self {
        let mut s = self.saved();
        s.port = s.port.saturating_add(1);
        Self::from_saved(s)
    }
    fn summary(&self) -> String {
        format!("{} (:{}, {} Clients)", self.s.name, self.s.port, self.clients.len())
    }

    fn pump(&mut self, ctx: &egui::Context) {
        if self.s.sim != Sim::Off {
            if self.last_sim.elapsed() >= Duration::from_millis(self.s.sim_ms) {
                self.last_sim = Instant::now();
                self.simulate();
            }
            ctx.request_repaint_after(Duration::from_millis(self.s.sim_ms.min(200)));
        }
        let Some(link) = &self.link else { return };
        let events: Vec<SrvEvent> = link.rx.try_iter().collect();
        let mut closed = false;
        for e in events {
            match e {
                SrvEvent::Opened(a) => {
                    self.open = true;
                    self.starting = false;
                    self.log.info(format!("Modbus-TCP-Server wartet auf {a}"));
                    self.local = a;
                }
                SrvEvent::Closed => closed = true,
                SrvEvent::Error(m) => self.log.error(m),
                SrvEvent::ClientUp(c) => {
                    self.log.info(format!("Client verbunden: {c}"));
                    self.clients.push(c);
                }
                SrvEvent::ClientDown(c) => {
                    self.log.info(format!("Client getrennt: {c}"));
                    self.clients.retain(|x| *x != c);
                }
                SrvEvent::Frame { peer, rx, data } => self.log.push(if rx { Dir::Rx } else { Dir::Tx }, Some(peer), data),
                SrvEvent::Request { peer, text, ok } => {
                    self.requests += 1;
                    if ok {
                        self.log.info(format!("{peer}: {text}"));
                    } else {
                        self.errors += 1;
                        self.log.error(format!("{peer}: {text}"));
                    }
                }
            }
        }
        if closed {
            self.link = None;
            self.open = false;
            self.starting = false;
            self.clients.clear();
        }
    }

    fn ui(&mut self, ui: &mut Ui, _shared: &mut Shared) {
        egui::Panel::right("mbs_cfg")
            .resizable(true)
            .default_size(270.0)
            .min_size(230.0)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(10.0))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.settings_ui(ui));
            });
        egui::Panel::bottom("mbs_log")
            .resizable(true)
            .default_size(170.0)
            .min_size(80.0)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(10, 6)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Anfragen & Datenverkehr").strong());
                    ui.checkbox(&mut self.log.s.timestamps, "Zeit");
                    ui.checkbox(&mut self.log.paused, "Pause");
                    if ui.small_button("🗑 Leeren").clicked() {
                        self.log.clear();
                    }
                });
                self.log.log_ui(ui);
            });
        egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(10.0)).show(ui, |ui| {
            self.table_ui(ui);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modbus::{Func, Reply, Request, build_pdu, parse_pdu, tcp_frame};

    fn call(s: &mut TcpStream, tid: u16, req: &Request) -> Result<Reply, String> {
        s.write_all(&tcp_frame(tid, req.unit, &build_pdu(req).unwrap())).unwrap();
        let mut head = [0u8; 7];
        s.read_exact(&mut head).unwrap();
        assert_eq!(u16::from_be_bytes([head[0], head[1]]), tid);
        let len = u16::from_be_bytes([head[4], head[5]]) as usize;
        let mut rest = vec![0u8; len - 1];
        s.read_exact(&mut rest).unwrap();
        parse_pdu(req, &rest)
    }

    #[test]
    fn server_answers_reads_and_writes() {
        let store = Arc::new(Mutex::new(Store::new()));
        store.lock().unwrap().input[5] = 777;
        let ctx = egui::Context::default();
        let link = spawn(&ctx, "127.0.0.1".into(), 0, None, store.clone(), true);
        let addr = loop {
            match link.rx.recv_timeout(Duration::from_secs(3)).unwrap() {
                SrvEvent::Opened(a) => break a,
                SrvEvent::Error(e) => panic!("{e}"),
                _ => {}
            }
        };
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let w = Request { unit: 1, func: Func::WriteRegisters, addr: 10, qty: 3, values: vec![1, 2, 3] };
        assert_eq!(call(&mut s, 1, &w).unwrap(), Reply::Written);
        let r = Request { unit: 1, func: Func::ReadHolding, addr: 10, qty: 3, values: vec![] };
        assert_eq!(call(&mut s, 2, &r).unwrap(), Reply::Regs(vec![1, 2, 3]));
        let r = Request { unit: 1, func: Func::ReadInput, addr: 5, qty: 1, values: vec![] };
        assert_eq!(call(&mut s, 3, &r).unwrap(), Reply::Regs(vec![777]));
        let c = Request { unit: 1, func: Func::WriteCoil, addr: 3, qty: 1, values: vec![1] };
        assert_eq!(call(&mut s, 4, &c).unwrap(), Reply::Written);
        let r = Request { unit: 1, func: Func::ReadCoils, addr: 0, qty: 5, values: vec![] };
        assert_eq!(call(&mut s, 5, &r).unwrap(), Reply::Bits(vec![false, false, false, true, false]));
        let bad = Request { unit: 1, func: Func::ReadHolding, addr: 65535, qty: 2, values: vec![] };
        assert!(call(&mut s, 6, &bad).unwrap_err().contains("Datenadresse"));
        assert_eq!(store.lock().unwrap().holding[11], 2);
    }

    #[test]
    fn saved_values_survive() {
        let a = ModbusServerSession::new("x".into(), 502);
        a.store.lock().unwrap().holding[100] = 42;
        a.store.lock().unwrap().coils[7] = true;
        let b = ModbusServerSession::from_saved(a.saved());
        assert_eq!(b.store.lock().unwrap().holding[100], 42);
        assert!(b.store.lock().unwrap().coils[7]);
    }
}
