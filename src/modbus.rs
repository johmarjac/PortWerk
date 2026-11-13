//! Modbus master (client) for RTU over a serial port and for Modbus TCP.
//! Each session reads and writes coils and registers, can poll cyclically
//! and shows every frame in a traffic log.

use std::io::{ErrorKind, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, RichText, Ui};
use serde::{Deserialize, Serialize};

use crate::app::section;
use crate::codec;
use crate::link::SerialConfig;
use crate::sessions::{GREEN, RED, Session, Shared, big_button, serial_settings_ui, status_line};
use crate::terminal::{Dir, TermSettings, Terminal, View};

// ------------------------------------------------------------------ protocol

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Func {
    ReadCoils,
    ReadDiscrete,
    ReadHolding,
    ReadInput,
    WriteCoil,
    WriteRegister,
    WriteCoils,
    WriteRegisters,
}

impl Func {
    pub fn code(self) -> u8 {
        match self {
            Func::ReadCoils => 1,
            Func::ReadDiscrete => 2,
            Func::ReadHolding => 3,
            Func::ReadInput => 4,
            Func::WriteCoil => 5,
            Func::WriteRegister => 6,
            Func::WriteCoils => 15,
            Func::WriteRegisters => 16,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Func::ReadCoils => "01 Coils lesen",
            Func::ReadDiscrete => "02 Eingänge lesen (diskret)",
            Func::ReadHolding => "03 Holding-Register lesen",
            Func::ReadInput => "04 Input-Register lesen",
            Func::WriteCoil => "05 Einzelne Coil schreiben",
            Func::WriteRegister => "06 Einzelnes Register schreiben",
            Func::WriteCoils => "15 Mehrere Coils schreiben",
            Func::WriteRegisters => "16 Mehrere Register schreiben",
        }
    }
    pub fn is_bits(self) -> bool {
        matches!(self, Func::ReadCoils | Func::ReadDiscrete | Func::WriteCoil | Func::WriteCoils)
    }
    const READS: [Func; 4] = [Func::ReadCoils, Func::ReadDiscrete, Func::ReadHolding, Func::ReadInput];
    const WRITES: [Func; 4] = [Func::WriteCoil, Func::WriteRegister, Func::WriteCoils, Func::WriteRegisters];
}

#[derive(Debug, Clone)]
pub struct Request {
    pub unit: u8,
    pub func: Func,
    pub addr: u16,
    pub qty: u16,
    /// Write values (coils: 0 / 1).
    pub values: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    Bits(Vec<bool>),
    Regs(Vec<u16>),
    Written,
}

pub fn exception_text(code: u8) -> &'static str {
    match code {
        1 => "Funktion wird nicht unterstützt",
        2 => "Ungültige Datenadresse",
        3 => "Ungültiger Datenwert",
        4 => "Fehler im Gerät",
        5 => "Bestätigt, Bearbeitung dauert noch",
        6 => "Gerät ist beschäftigt",
        8 => "Speicher-Paritätsfehler",
        10 => "Gateway-Pfad nicht verfügbar",
        11 => "Gateway: Zielgerät antwortet nicht",
        _ => "Unbekannte Ausnahme",
    }
}

pub fn build_pdu(r: &Request) -> Result<Vec<u8>, String> {
    let mut p = vec![r.func.code()];
    p.extend_from_slice(&r.addr.to_be_bytes());
    match r.func {
        Func::ReadCoils | Func::ReadDiscrete => {
            if !(1..=2000).contains(&r.qty) {
                return Err("Anzahl muss zwischen 1 und 2000 liegen".into());
            }
            p.extend_from_slice(&r.qty.to_be_bytes());
        }
        Func::ReadHolding | Func::ReadInput => {
            if !(1..=125).contains(&r.qty) {
                return Err("Anzahl muss zwischen 1 und 125 liegen".into());
            }
            p.extend_from_slice(&r.qty.to_be_bytes());
        }
        Func::WriteCoil => {
            let v = *r.values.first().ok_or("Kein Wert angegeben")?;
            p.extend_from_slice(if v != 0 { &[0xFF, 0x00] } else { &[0x00, 0x00] });
        }
        Func::WriteRegister => {
            let v = *r.values.first().ok_or("Kein Wert angegeben")?;
            p.extend_from_slice(&v.to_be_bytes());
        }
        Func::WriteCoils => {
            let n = r.values.len();
            if !(1..=1968).contains(&n) {
                return Err("1 bis 1968 Werte angeben".into());
            }
            p.extend_from_slice(&(n as u16).to_be_bytes());
            let bytes = n.div_ceil(8);
            p.push(bytes as u8);
            let mut packed = vec![0u8; bytes];
            for (i, v) in r.values.iter().enumerate() {
                if *v != 0 {
                    packed[i / 8] |= 1 << (i % 8);
                }
            }
            p.extend_from_slice(&packed);
        }
        Func::WriteRegisters => {
            let n = r.values.len();
            if !(1..=123).contains(&n) {
                return Err("1 bis 123 Werte angeben".into());
            }
            p.extend_from_slice(&(n as u16).to_be_bytes());
            p.push((n * 2) as u8);
            for v in &r.values {
                p.extend_from_slice(&v.to_be_bytes());
            }
        }
    }
    Ok(p)
}

pub fn rtu_frame(unit: u8, pdu: &[u8]) -> Vec<u8> {
    let mut f = vec![unit];
    f.extend_from_slice(pdu);
    let crc = codec::crc16_modbus(&f);
    f.push((crc & 0xFF) as u8);
    f.push((crc >> 8) as u8);
    f
}

pub fn tcp_frame(tid: u16, unit: u8, pdu: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(7 + pdu.len());
    f.extend_from_slice(&tid.to_be_bytes());
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&((pdu.len() + 1) as u16).to_be_bytes());
    f.push(unit);
    f.extend_from_slice(pdu);
    f
}

/// Expected total length of an RTU reply once enough bytes are known.
fn rtu_expected_len(b: &[u8]) -> Option<usize> {
    if b.len() < 2 {
        return None;
    }
    let fc = b[1];
    if fc & 0x80 != 0 {
        return Some(5);
    }
    match fc {
        1..=4 => b.get(2).map(|n| 3 + *n as usize + 2),
        5 | 6 | 15 | 16 => Some(8),
        _ => Some(b.len()),
    }
}

pub fn parse_pdu(req: &Request, pdu: &[u8]) -> Result<Reply, String> {
    let fc = *pdu.first().ok_or("Leere Antwort")?;
    if fc == req.func.code() | 0x80 {
        let code = pdu.get(1).copied().unwrap_or(0);
        return Err(format!("Ausnahme {code:02}: {}", exception_text(code)));
    }
    if fc != req.func.code() {
        return Err(format!("Unerwarteter Funktionscode {fc}"));
    }
    match req.func {
        Func::ReadCoils | Func::ReadDiscrete => {
            let n = *pdu.get(1).ok_or("Antwort zu kurz")? as usize;
            let data = pdu.get(2..2 + n).ok_or("Antwort zu kurz")?;
            Ok(Reply::Bits((0..req.qty as usize).map(|i| data.get(i / 8).is_some_and(|b| b & (1 << (i % 8)) != 0)).collect()))
        }
        Func::ReadHolding | Func::ReadInput => {
            let n = *pdu.get(1).ok_or("Antwort zu kurz")? as usize;
            let data = pdu.get(2..2 + n).ok_or("Antwort zu kurz")?;
            Ok(Reply::Regs(data.chunks(2).filter(|c| c.len() == 2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect()))
        }
        _ => Ok(Reply::Written),
    }
}

// ------------------------------------------------------------------ worker

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Transport {
    Rtu,
    Tcp,
}

enum Target {
    Rtu(SerialConfig),
    Tcp(String, u16),
}

enum MbCmd {
    Req(Request),
    Close,
}

enum MbEvent {
    Opened(String),
    Closed,
    Error(String),
    Frame { tx: bool, data: Vec<u8> },
    Done { req: Request, res: Result<Reply, String>, ms: u128 },
}

struct MbLink {
    tx: Sender<MbCmd>,
    rx: Receiver<MbEvent>,
}

impl Drop for MbLink {
    fn drop(&mut self) {
        let _ = self.tx.send(MbCmd::Close);
    }
}

enum Port {
    Serial(Box<dyn serialport::SerialPort>),
    Tcp(TcpStream),
}

fn open_port(t: &Target) -> Result<(Port, String), String> {
    match t {
        Target::Rtu(cfg) => {
            let p = serialport::new(cfg.port.trim(), cfg.baud)
                .data_bits(match cfg.data_bits {
                    7 => serialport::DataBits::Seven,
                    _ => serialport::DataBits::Eight,
                })
                .parity(match cfg.parity {
                    crate::link::Parity::None => serialport::Parity::None,
                    crate::link::Parity::Odd => serialport::Parity::Odd,
                    crate::link::Parity::Even => serialport::Parity::Even,
                })
                .stop_bits(if cfg.stop_bits == 2 { serialport::StopBits::Two } else { serialport::StopBits::One })
                .timeout(Duration::from_millis(10))
                .open()
                .map_err(|e| format!("{} konnte nicht geöffnet werden: {e}", cfg.port))?;
            Ok((Port::Serial(p), format!("{} geöffnet ({} Baud)", cfg.port, cfg.baud)))
        }
        Target::Tcp(host, port) => {
            let addr = (host.trim(), *port)
                .to_socket_addrs()
                .map_err(|e| format!("Adresse '{host}' nicht auflösbar: {e}"))?
                .next()
                .ok_or("Adresse nicht auflösbar")?;
            let s = TcpStream::connect_timeout(&addr, Duration::from_secs(5))
                .map_err(|e| format!("Verbindung zu {addr} fehlgeschlagen: {e}"))?;
            let _ = s.set_nodelay(true);
            Ok((Port::Tcp(s), format!("Verbunden mit {addr}")))
        }
    }
}

/// Send one request and read its reply. Returns (tx frame, rx frame, result).
fn transact(port: &mut Port, tid: u16, req: &Request, timeout: Duration, out: &impl Fn(MbEvent)) -> Result<Reply, String> {
    let pdu = build_pdu(req)?;
    let deadline = Instant::now() + timeout;
    match port {
        Port::Serial(p) => {
            let _ = p.clear(serialport::ClearBuffer::Input);
            let frame = rtu_frame(req.unit, &pdu);
            p.write_all(&frame).map_err(|e| format!("Senden fehlgeschlagen: {e}"))?;
            let _ = p.flush();
            out(MbEvent::Frame { tx: true, data: frame });
            let mut buf = Vec::new();
            let mut tmp = [0u8; 256];
            loop {
                if let Some(n) = rtu_expected_len(&buf) {
                    if buf.len() >= n {
                        buf.truncate(n);
                        break;
                    }
                }
                if Instant::now() > deadline {
                    if !buf.is_empty() {
                        out(MbEvent::Frame { tx: false, data: buf });
                        return Err("Unvollständige Antwort (Zeitüberschreitung)".into());
                    }
                    return Err("Keine Antwort (Zeitüberschreitung)".into());
                }
                match p.read(&mut tmp) {
                    Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    Err(e) if e.kind() == ErrorKind::TimedOut => {}
                    Err(e) => return Err(format!("Lesefehler: {e}")),
                }
            }
            out(MbEvent::Frame { tx: false, data: buf.clone() });
            let n = buf.len();
            if n < 4 {
                return Err("Antwort zu kurz".into());
            }
            let crc = codec::crc16_modbus(&buf[..n - 2]);
            if buf[n - 2] != (crc & 0xFF) as u8 || buf[n - 1] != (crc >> 8) as u8 {
                return Err("CRC-Fehler in der Antwort".into());
            }
            if buf[0] != req.unit {
                return Err(format!("Antwort von falscher Geräteadresse {}", buf[0]));
            }
            parse_pdu(req, &buf[1..n - 2])
        }
        Port::Tcp(s) => {
            let frame = tcp_frame(tid, req.unit, &pdu);
            s.write_all(&frame).map_err(|e| format!("Senden fehlgeschlagen: {e}"))?;
            out(MbEvent::Frame { tx: true, data: frame });
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err("Keine Antwort (Zeitüberschreitung)".into());
                }
                let _ = s.set_read_timeout(Some(left));
                let mut head = [0u8; 7];
                read_exact(s, &mut head)?;
                let len = u16::from_be_bytes([head[4], head[5]]) as usize;
                if len < 2 || len > 260 {
                    return Err("Ungültiger Modbus-TCP-Kopf".into());
                }
                let mut rest = vec![0u8; len - 1];
                read_exact(s, &mut rest)?;
                let mut all = head.to_vec();
                all.extend_from_slice(&rest);
                out(MbEvent::Frame { tx: false, data: all });
                if u16::from_be_bytes([head[0], head[1]]) != tid {
                    continue; // late answer to an earlier request
                }
                return parse_pdu(req, &rest);
            }
        }
    }
}

fn read_exact(s: &mut TcpStream, buf: &mut [u8]) -> Result<(), String> {
    s.read_exact(buf).map_err(|e| match e.kind() {
        ErrorKind::WouldBlock | ErrorKind::TimedOut => "Keine Antwort (Zeitüberschreitung)".to_string(),
        ErrorKind::UnexpectedEof => "Verbindung von der Gegenstelle geschlossen".to_string(),
        _ => format!("Lesefehler: {e}"),
    })
}

fn spawn(ctx: &egui::Context, target: Target, timeout_ms: u64) -> MbLink {
    let (cmd_tx, cmd_rx) = channel();
    let (ev_tx, ev_rx) = channel();
    let ctx = ctx.clone();
    thread::spawn(move || {
        let out = move |e: MbEvent| {
            let _ = ev_tx.send(e);
            ctx.request_repaint();
        };
        let (mut port, msg) = match open_port(&target) {
            Ok(p) => p,
            Err(e) => {
                out(MbEvent::Error(e));
                out(MbEvent::Closed);
                return;
            }
        };
        out(MbEvent::Opened(msg));
        let mut tid: u16 = 0;
        loop {
            match cmd_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(MbCmd::Req(req)) => {
                    tid = tid.wrapping_add(1);
                    let start = Instant::now();
                    let res = transact(&mut port, tid, &req, Duration::from_millis(timeout_ms), &out);
                    let dead = matches!(&res, Err(e) if e.starts_with("Verbindung") || e.starts_with("Senden"));
                    out(MbEvent::Done { req, res, ms: start.elapsed().as_millis() });
                    if dead {
                        break;
                    }
                }
                Ok(MbCmd::Close) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        if let Port::Tcp(s) = &port {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        out(MbEvent::Closed);
    });
    MbLink { tx: cmd_tx, rx: ev_rx }
}

// ------------------------------------------------------------------ values

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Format {
    Unsigned,
    Signed,
    Hex,
    Binary,
    U32Be,
    U32Le,
    I32Be,
    I32Le,
    F32Be,
    F32Le,
    Ascii,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Unsigned => "16 Bit ohne Vorzeichen",
            Format::Signed => "16 Bit mit Vorzeichen",
            Format::Hex => "Hex",
            Format::Binary => "Binär",
            Format::U32Be => "32 Bit ohne Vorzeichen (AB CD)",
            Format::U32Le => "32 Bit ohne Vorzeichen (CD AB)",
            Format::I32Be => "32 Bit mit Vorzeichen (AB CD)",
            Format::I32Le => "32 Bit mit Vorzeichen (CD AB)",
            Format::F32Be => "Float 32 (AB CD)",
            Format::F32Le => "Float 32 (CD AB)",
            Format::Ascii => "Text (2 Zeichen je Register)",
        }
    }
    pub fn wide(self) -> bool {
        matches!(self, Format::U32Be | Format::U32Le | Format::I32Be | Format::I32Le | Format::F32Be | Format::F32Le)
    }
    pub const ALL: [Format; 11] = [
        Format::Unsigned,
        Format::Signed,
        Format::Hex,
        Format::Binary,
        Format::U32Be,
        Format::U32Le,
        Format::I32Be,
        Format::I32Le,
        Format::F32Be,
        Format::F32Le,
        Format::Ascii,
    ];
}

/// One display row: (address, formatted value, raw hex).
pub fn format_regs(start: u16, regs: &[u16], f: Format) -> Vec<(u16, String, String)> {
    let mut rows = Vec::new();
    if f.wide() {
        for (i, pair) in regs.chunks(2).enumerate() {
            let addr = start.wrapping_add((i * 2) as u16);
            if pair.len() < 2 {
                rows.push((addr, "–".into(), format!("{:04X}", pair[0])));
                continue;
            }
            let (hi, lo) = match f {
                Format::U32Le | Format::I32Le | Format::F32Le => (pair[1], pair[0]),
                _ => (pair[0], pair[1]),
            };
            let v = (hi as u32) << 16 | lo as u32;
            let s = match f {
                Format::U32Be | Format::U32Le => v.to_string(),
                Format::I32Be | Format::I32Le => (v as i32).to_string(),
                _ => {
                    let x = f32::from_bits(v);
                    format!("{x}")
                }
            };
            rows.push((addr, s, format!("{:04X} {:04X}", pair[0], pair[1])));
        }
    } else {
        for (i, r) in regs.iter().enumerate() {
            let addr = start.wrapping_add(i as u16);
            let s = match f {
                Format::Signed => (*r as i16).to_string(),
                Format::Hex => format!("0x{r:04X}"),
                Format::Binary => format!("{:08b} {:08b}", r >> 8, r & 0xFF),
                Format::Ascii => codec::to_text(&r.to_be_bytes(), true),
                _ => r.to_string(),
            };
            rows.push((addr, s, format!("{r:04X}")));
        }
    }
    rows
}

/// Parse "1, 2 0x10 -5" (registers) or "1 0 an aus" (coils).
pub fn parse_values(s: &str, bits: bool) -> Result<Vec<u16>, String> {
    s.split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .map(|t| {
            let l = t.to_lowercase();
            if bits {
                return match l.as_str() {
                    "1" | "an" | "ein" | "on" | "true" => Ok(1),
                    "0" | "aus" | "off" | "false" => Ok(0),
                    _ => Err(format!("'{t}' ist kein Coil-Wert (1/0, an/aus)")),
                };
            }
            let v: i64 = if let Some(h) = l.strip_prefix("0x") {
                i64::from_str_radix(h, 16).map_err(|_| format!("'{t}' ist keine Hex-Zahl"))?
            } else if let Some(b) = l.strip_prefix("0b") {
                i64::from_str_radix(b, 2).map_err(|_| format!("'{t}' ist keine Binärzahl"))?
            } else {
                l.parse().map_err(|_| format!("'{t}' ist keine Zahl"))?
            };
            if !(-32768..=65535).contains(&v) {
                return Err(format!("{t} passt nicht in ein Register (−32768 … 65535)"));
            }
            Ok(v as u16)
        })
        .collect()
}

// ------------------------------------------------------------------ session

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ModbusSaved {
    pub name: String,
    pub transport: Transport,
    pub serial: SerialConfig,
    pub host: String,
    pub port: u16,
    pub unit: u8,
    pub timeout_ms: u64,
    pub read_func: Func,
    pub read_addr: u16,
    pub read_qty: u16,
    pub poll: bool,
    pub poll_ms: u64,
    pub write_func: Func,
    pub write_addr: u16,
    pub write_values: String,
    pub format: Format,
}

impl Default for ModbusSaved {
    fn default() -> Self {
        Self {
            name: "Modbus 1".into(),
            transport: Transport::Tcp,
            serial: SerialConfig { baud: 19200, parity: crate::link::Parity::Even, ..SerialConfig::default() },
            host: "127.0.0.1".into(),
            port: 502,
            unit: 1,
            timeout_ms: 1000,
            read_func: Func::ReadHolding,
            read_addr: 0,
            read_qty: 10,
            poll: false,
            poll_ms: 1000,
            write_func: Func::WriteRegister,
            write_addr: 0,
            write_values: "0".into(),
            format: Format::Unsigned,
        }
    }
}

enum Table {
    Bits(u16, Vec<bool>),
    Regs(u16, Vec<u16>),
}

pub struct ModbusSession {
    s: ModbusSaved,
    link: Option<MbLink>,
    open: bool,
    starting: bool,
    in_flight: bool,
    last_poll: Instant,
    table: Option<Table>,
    /// Read function the table came from (to know what a write changes).
    table_func: Option<Func>,
    /// Read the table range again after a write, to show what the device really stored.
    reread: bool,
    last_result: Option<(bool, String)>,
    ok: u64,
    errors: u64,
    log: Terminal,
}

impl ModbusSession {
    pub fn new(name: String) -> Self {
        Self::from_saved(ModbusSaved { name, ..Default::default() })
    }

    pub fn from_saved(mut s: ModbusSaved) -> Self {
        if s.serial.port.is_empty() {
            s.serial.port = crate::link::list_serial_ports()
                .into_iter()
                .next()
                .unwrap_or_else(|| crate::sessions::default_port_name().into());
        }
        let mut log = Terminal::new(TermSettings { view: View::Hex, ..TermSettings::default() });
        log.info("Bereit. Verbindung einstellen und „Verbinden“ klicken.");
        Self {
            s,
            link: None,
            open: false,
            starting: false,
            in_flight: false,
            last_poll: Instant::now(),
            table: None,
            table_func: None,
            reread: false,
            last_result: None,
            ok: 0,
            errors: 0,
            log,
        }
    }

    pub fn saved(&self) -> ModbusSaved {
        self.s.clone()
    }

    /// Show a successful write in the table right away and confirm it by reading again.
    fn apply_write(&mut self, req: &Request) {
        let target = match req.func {
            Func::WriteCoil | Func::WriteCoils => Func::ReadCoils,
            Func::WriteRegister | Func::WriteRegisters => Func::ReadHolding,
            _ => return,
        };
        if self.table_func != Some(target) {
            return;
        }
        let mut touched = false;
        match &mut self.table {
            Some(Table::Bits(start, bits)) => {
                for (k, v) in req.values.iter().enumerate() {
                    let i = (req.addr as usize + k).wrapping_sub(*start as usize);
                    if let Some(b) = bits.get_mut(i) {
                        *b = *v != 0;
                        touched = true;
                    }
                }
            }
            Some(Table::Regs(start, regs)) => {
                for (k, v) in req.values.iter().enumerate() {
                    let i = (req.addr as usize + k).wrapping_sub(*start as usize);
                    if let Some(r) = regs.get_mut(i) {
                        *r = *v;
                        touched = true;
                    }
                }
            }
            None => {}
        }
        if touched {
            self.reread = true;
        }
    }

    fn request(&mut self, req: Request) {
        if let Some(l) = &self.link {
            if l.tx.send(MbCmd::Req(req)).is_ok() {
                self.in_flight = true;
            }
        }
    }

    fn read_request(&self) -> Request {
        Request { unit: self.s.unit, func: self.s.read_func, addr: self.s.read_addr, qty: self.s.read_qty, values: Vec::new() }
    }

    fn connect(&mut self, ctx: &egui::Context) {
        let target = match self.s.transport {
            Transport::Rtu => Target::Rtu(self.s.serial.clone()),
            Transport::Tcp => Target::Tcp(self.s.host.clone(), self.s.port),
        };
        self.link = Some(spawn(ctx, target, self.s.timeout_ms));
        self.starting = true;
    }

    fn settings_ui(&mut self, ui: &mut Ui, shared: &mut Shared) {
        let active = self.link.is_some();
        section(ui, "Verbindung");
        ui.add_enabled_ui(!active, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.s.transport, Transport::Tcp, "Modbus TCP");
                ui.selectable_value(&mut self.s.transport, Transport::Rtu, "Modbus RTU (seriell)");
            });
            ui.add_space(4.0);
            match self.s.transport {
                Transport::Tcp => {
                    egui::Grid::new("mb_tcp").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                        ui.label("Adresse");
                        ui.add(egui::TextEdit::singleline(&mut self.s.host).desired_width(150.0).hint_text("IP oder Hostname"));
                        ui.end_row();
                        ui.label("Port");
                        ui.add(egui::DragValue::new(&mut self.s.port).range(1..=65535));
                        ui.end_row();
                    });
                }
                Transport::Rtu => serial_settings_ui(ui, "mb_serial", &mut self.s.serial, &mut shared.ports, false),
            }
            ui.horizontal(|ui| {
                ui.label("Zeitlimit");
                ui.add(egui::DragValue::new(&mut self.s.timeout_ms).range(20..=60_000).suffix(" ms").speed(10.0));
            });
        });
        ui.add_space(6.0);
        if big_button(ui, active, "Verbinden", "Trennen").clicked() {
            if active {
                self.link = None;
                self.open = false;
                self.starting = false;
                self.in_flight = false;
                self.log.info("Getrennt");
            } else {
                self.connect(ui.ctx());
            }
        }
        status_line(ui, self.open, self.starting, "verbunden", "getrennt");

        section(ui, "Gerät");
        ui.horizontal(|ui| {
            ui.label("Geräteadresse");
            ui.add(egui::DragValue::new(&mut self.s.unit).range(0..=255))
                .on_hover_text("Slave-/Unit-ID, meist 1. Bei Modbus TCP oft 1 oder 255.");
        });

        section(ui, "Statistik");
        ui.label(format!("Erfolgreich: {}", self.ok));
        ui.label(format!("Fehler: {}", self.errors));
        if let Some((good, msg)) = &self.last_result {
            ui.label(RichText::new(msg).color(if *good { GREEN } else { RED }).small());
        }
        if ui.small_button("Zähler zurücksetzen").clicked() {
            self.ok = 0;
            self.errors = 0;
        }
    }

    fn main_ui(&mut self, ui: &mut Ui) {
        let connected = self.open;
        // ---- read
        egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Lesen").strong());
                egui::ComboBox::from_id_salt("mb_read_func")
                    .selected_text(self.s.read_func.label())
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for f in Func::READS {
                            ui.selectable_value(&mut self.s.read_func, f, f.label());
                        }
                    });
                ui.label("ab Adresse");
                ui.add(egui::DragValue::new(&mut self.s.read_addr).range(0..=65535))
                    .on_hover_text("Protokolladresse, beginnt bei 0 (Register 40001 = Adresse 0)");
                ui.label("Anzahl");
                let max = if self.s.read_func.is_bits() { 2000 } else { 125 };
                ui.add(egui::DragValue::new(&mut self.s.read_qty).range(1..=max));
                if ui.add_enabled(connected && !self.in_flight, egui::Button::new("▶ Lesen")).clicked() {
                    let r = self.read_request();
                    self.request(r);
                }
                ui.separator();
                ui.checkbox(&mut self.s.poll, "zyklisch alle").on_hover_text("Automatisch immer wieder lesen");
                ui.add(egui::DragValue::new(&mut self.s.poll_ms).range(50..=3_600_000).suffix(" ms").speed(10.0));
            });
        });
        ui.add_space(4.0);
        // ---- write
        egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Schreiben").strong());
                egui::ComboBox::from_id_salt("mb_write_func")
                    .selected_text(self.s.write_func.label())
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for f in Func::WRITES {
                            ui.selectable_value(&mut self.s.write_func, f, f.label());
                        }
                    });
                ui.label("Adresse");
                ui.add(egui::DragValue::new(&mut self.s.write_addr).range(0..=65535));
                ui.label("Werte");
                let hint = if self.s.write_func.is_bits() { "1 0 1 oder an aus" } else { "z. B. 100, 0x1F, -5" };
                ui.add(egui::TextEdit::singleline(&mut self.s.write_values).hint_text(hint).desired_width(200.0))
                    .on_hover_text("Mehrere Werte mit Komma oder Leerzeichen trennen. Bei Funktion 05/06 zählt nur der erste.");
                if ui.add_enabled(connected && !self.in_flight, egui::Button::new("✏ Schreiben")).clicked() {
                    match parse_values(&self.s.write_values, self.s.write_func.is_bits()) {
                        Ok(values) if !values.is_empty() => {
                            let req = Request {
                                unit: self.s.unit,
                                func: self.s.write_func,
                                addr: self.s.write_addr,
                                qty: values.len() as u16,
                                values,
                            };
                            self.request(req);
                        }
                        Ok(_) => self.log.error("Bitte mindestens einen Wert eingeben"),
                        Err(e) => self.log.error(e),
                    }
                }
            });
        });
        ui.add_space(6.0);

        // ---- results
        ui.horizontal(|ui| {
            ui.label(RichText::new("Werte").strong().size(14.0));
            if matches!(self.table, Some(Table::Regs(..))) {
                ui.label("Anzeige");
                egui::ComboBox::from_id_salt("mb_format").selected_text(self.s.format.label()).width(230.0).show_ui(
                    ui,
                    |ui| {
                        for f in Format::ALL {
                            ui.selectable_value(&mut self.s.format, f, f.label());
                        }
                    },
                );
            }
            if self.in_flight {
                ui.spinner();
            }
        });
        let mut write_coil = None;
        let mut edit_reg = None;
        let dark = ui.visuals().dark_mode;
        egui::Frame::new()
            .fill(if dark { Color32::from_gray(18) } else { Color32::WHITE })
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
            .corner_radius(4.0)
            .inner_margin(6.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match &self.table {
                    None => {
                        ui.label(RichText::new("Noch keine Werte gelesen.").weak().italics());
                    }
                    Some(Table::Bits(start, bits)) => {
                        let is_coils = self.table_func == Some(Func::ReadCoils);
                        egui::Grid::new("mb_bits").num_columns(3).striped(true).spacing([24.0, 4.0]).show(ui, |ui| {
                            ui.label(RichText::new("Adresse").strong());
                            ui.label(RichText::new("Wert").strong());
                            ui.label("");
                            ui.end_row();
                            for (i, b) in bits.iter().enumerate() {
                                let a = start.wrapping_add(i as u16);
                                ui.monospace(a.to_string());
                                ui.label(RichText::new(if *b { "● 1 (an)" } else { "○ 0 (aus)" }).color(if *b { GREEN } else { Color32::GRAY }));
                                if is_coils && ui.small_button("umschalten").clicked() {
                                    write_coil = Some((a, !*b));
                                }
                                ui.end_row();
                            }
                        });
                    }
                    Some(Table::Regs(start, regs)) => {
                        let rows = format_regs(*start, regs, self.s.format);
                        let is_holding = self.table_func == Some(Func::ReadHolding);
                        egui::Grid::new("mb_regs").num_columns(4).striped(true).spacing([24.0, 4.0]).show(ui, |ui| {
                            ui.label(RichText::new("Adresse").strong());
                            ui.label(RichText::new("Wert").strong());
                            ui.label(RichText::new("Roh (Hex)").strong());
                            ui.label("");
                            ui.end_row();
                            for (a, v, raw) in rows {
                                ui.monospace(a.to_string());
                                ui.monospace(RichText::new(v).strong());
                                ui.monospace(RichText::new(raw).weak());
                                if is_holding && ui.small_button("✏").on_hover_text("Dieses Register schreiben").clicked() {
                                    edit_reg = Some(a);
                                }
                                ui.end_row();
                            }
                        });
                    }
                });
            });
        if let Some((a, v)) = write_coil {
            // the worker handles requests in order, so this is queued behind a running read
            if connected {
                self.request(Request { unit: self.s.unit, func: Func::WriteCoil, addr: a, qty: 1, values: vec![v as u16] });
            }
        }
        if let Some(a) = edit_reg {
            self.s.write_func = Func::WriteRegister;
            self.s.write_addr = a;
            if let Some(Table::Regs(start, regs)) = &self.table {
                if let Some(v) = regs.get(a.wrapping_sub(*start) as usize) {
                    self.s.write_values = v.to_string();
                }
            }
        }
    }
}

impl Session for ModbusSession {
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
        self.s.poll && self.open
    }
    fn stop(&mut self) {
        self.link = None;
        self.open = false;
    }
    fn duplicate(&self) -> Self {
        Self::from_saved(self.saved())
    }
    fn summary(&self) -> String {
        match self.s.transport {
            Transport::Tcp => format!("{} ({}:{})", self.s.name, self.s.host, self.s.port),
            Transport::Rtu => format!("{} ({})", self.s.name, self.s.serial.port),
        }
    }

    fn pump(&mut self, ctx: &egui::Context) {
        let Some(link) = &self.link else { return };
        let events: Vec<MbEvent> = link.rx.try_iter().collect();
        let mut closed = false;
        for e in events {
            match e {
                MbEvent::Opened(m) => {
                    self.open = true;
                    self.starting = false;
                    self.log.info(m);
                }
                MbEvent::Closed => closed = true,
                MbEvent::Error(m) => self.log.error(m),
                MbEvent::Frame { tx, data } => self.log.push(if tx { Dir::Tx } else { Dir::Rx }, None, data),
                MbEvent::Done { req, res, ms } => {
                    self.in_flight = false;
                    match res {
                        Ok(reply) => {
                            self.ok += 1;
                            self.last_result = Some((true, format!("{} – OK in {ms} ms", req.func.label())));
                            match reply {
                                Reply::Bits(b) => {
                                    self.table = Some(Table::Bits(req.addr, b));
                                    self.table_func = Some(req.func);
                                }
                                Reply::Regs(r) => {
                                    self.table = Some(Table::Regs(req.addr, r));
                                    self.table_func = Some(req.func);
                                }
                                Reply::Written => {
                                    self.log.info(format!("Geschrieben: {} ab Adresse {}", req.func.label(), req.addr));
                                    self.apply_write(&req);
                                }
                            }
                        }
                        Err(e) => {
                            self.errors += 1;
                            self.last_result = Some((false, e.clone()));
                            self.log.error(e);
                        }
                    }
                }
            }
        }
        if closed {
            self.link = None;
            self.open = false;
            self.starting = false;
            self.in_flight = false;
            self.log.info("Verbindung beendet");
            return;
        }
        if self.reread && self.open && !self.in_flight {
            self.reread = false;
            if let (Some(func), Some(t)) = (self.table_func, &self.table) {
                let (addr, qty) = match t {
                    Table::Bits(a, b) => (*a, b.len() as u16),
                    Table::Regs(a, r) => (*a, r.len() as u16),
                };
                if qty > 0 {
                    self.request(Request { unit: self.s.unit, func, addr, qty, values: Vec::new() });
                }
            }
        }
        if self.s.poll && self.open {
            if !self.in_flight && self.last_poll.elapsed() >= Duration::from_millis(self.s.poll_ms) {
                self.last_poll = Instant::now();
                let r = self.read_request();
                self.request(r);
            }
            ctx.request_repaint_after(Duration::from_millis(self.s.poll_ms.min(100)));
        }
    }

    fn ui(&mut self, ui: &mut Ui, shared: &mut Shared) {
        egui::Panel::right("mb_cfg")
            .resizable(true)
            .default_size(280.0)
            .min_size(240.0)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(10.0))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| self.settings_ui(ui, shared));
            });
        egui::Panel::bottom("mb_log")
            .resizable(true)
            .default_size(170.0)
            .min_size(80.0)
            .frame(egui::Frame::side_top_panel(ui.style()).inner_margin(egui::Margin::symmetric(10, 6)))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Datenverkehr").strong());
                    ui.checkbox(&mut self.log.s.timestamps, "Zeit");
                    if ui.small_button("🗑 Leeren").clicked() {
                        self.log.clear();
                    }
                });
                self.log.log_ui(ui);
            });
        egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(10.0)).show(ui, |ui| {
            self.main_ui(ui);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn rtu_frame_has_known_crc() {
        let req = Request { unit: 1, func: Func::ReadHolding, addr: 0, qty: 10, values: vec![] };
        let f = rtu_frame(1, &build_pdu(&req).unwrap());
        assert_eq!(f, vec![0x01, 0x03, 0x00, 0x00, 0x00, 0x0A, 0xC5, 0xCD]);
    }

    #[test]
    fn write_coils_are_packed() {
        let req = Request { unit: 1, func: Func::WriteCoils, addr: 19, qty: 10, values: vec![1, 0, 1, 1, 0, 0, 1, 1, 1, 0] };
        assert_eq!(build_pdu(&req).unwrap(), vec![0x0F, 0x00, 0x13, 0x00, 0x0A, 0x02, 0xCD, 0x01]);
    }

    #[test]
    fn exception_is_reported() {
        let req = Request { unit: 1, func: Func::ReadHolding, addr: 0, qty: 1, values: vec![] };
        let e = parse_pdu(&req, &[0x83, 0x02]).unwrap_err();
        assert!(e.contains("Ungültige Datenadresse"));
    }

    #[test]
    fn float_and_word_order() {
        let rows = format_regs(0, &[0x4148, 0x0000], Format::F32Be);
        assert_eq!(rows[0].1, "12.5");
        let rows = format_regs(0, &[0x0000, 0x4148], Format::F32Le);
        assert_eq!(rows[0].1, "12.5");
        assert_eq!(parse_values("1, 0x10 -1", false).unwrap(), vec![1, 16, 0xFFFF]);
    }

    #[test]
    fn modbus_tcp_against_tiny_server() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        thread::spawn(move || {
            let (mut s, _) = l.accept().unwrap();
            let mut head = [0u8; 12];
            s.read_exact(&mut head).unwrap(); // MBAP + fc 03 + addr + qty
            let qty = u16::from_be_bytes([head[10], head[11]]);
            let mut resp = vec![head[0], head[1], 0, 0, 0, (3 + qty * 2) as u8, head[6], 3, (qty * 2) as u8];
            for i in 0..qty {
                resp.extend_from_slice(&(100 + i).to_be_bytes());
            }
            s.write_all(&resp).unwrap();
            let mut b = [0u8; 1];
            let _ = s.read(&mut b);
        });
        let (mut p, _) = open_port(&Target::Tcp("127.0.0.1".into(), port)).unwrap();
        let req = Request { unit: 1, func: Func::ReadHolding, addr: 0, qty: 3, values: vec![] };
        let r = transact(&mut p, 7, &req, Duration::from_secs(2), &|_| {}).unwrap();
        assert_eq!(r, Reply::Regs(vec![100, 101, 102]));
    }
}

#[cfg(test)]
mod write_tests {
    use super::*;

    #[test]
    fn coil_write_updates_table_and_rereads() {
        let mut m = ModbusSession::new("t".into());
        m.table = Some(Table::Bits(0, vec![false; 8]));
        m.table_func = Some(Func::ReadCoils);
        m.apply_write(&Request { unit: 1, func: Func::WriteCoil, addr: 3, qty: 1, values: vec![1] });
        match &m.table {
            Some(Table::Bits(_, b)) => assert!(b[3]),
            _ => panic!(),
        }
        assert!(m.reread);
    }

    #[test]
    fn register_write_outside_table_is_ignored() {
        let mut m = ModbusSession::new("t".into());
        m.table = Some(Table::Regs(10, vec![0; 5]));
        m.table_func = Some(Func::ReadHolding);
        m.apply_write(&Request { unit: 1, func: Func::WriteRegisters, addr: 13, qty: 3, values: vec![7, 8, 9] });
        match &m.table {
            Some(Table::Regs(_, r)) => assert_eq!(r, &vec![0, 0, 0, 7, 8]),
            _ => panic!(),
        }
        // input registers are not changed by a holding write
        m.table_func = Some(Func::ReadInput);
        m.reread = false;
        m.apply_write(&Request { unit: 1, func: Func::WriteRegister, addr: 10, qty: 1, values: vec![5] });
        assert!(!m.reread);
    }
}
