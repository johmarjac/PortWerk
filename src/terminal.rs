//! The shared terminal: a received/sent data log with view options and
//! a set of send lines. Every connection tab owns one.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, FontId, RichText, text::LayoutJob, TextFormat};
use serde::{Deserialize, Serialize};

use crate::codec;
use crate::script::{self, ScriptMsg, ScriptRun};

mod send_ui;

const MAX_ENTRIES: usize = 4000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dir {
    Rx,
    Tx,
    Info,
    Error,
}

struct Entry {
    time: String,
    dir: Dir,
    peer: Option<String>,
    data: Vec<u8>,
    last: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum View {
    Text,
    Hex,
    Both,
    Dec,
}

impl View {
    fn label(self) -> &'static str {
        match self {
            View::Text => "Text",
            View::Hex => "Hex",
            View::Both => "Hex + Text",
            View::Dec => "Dezimal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum LineEnd {
    None,
    Cr,
    Lf,
    CrLf,
}

impl LineEnd {
    fn label(self) -> &'static str {
        match self {
            LineEnd::None => "kein Zeilenende",
            LineEnd::Cr => "+ CR",
            LineEnd::Lf => "+ LF",
            LineEnd::CrLf => "+ CR LF",
        }
    }
    fn bytes(self) -> &'static [u8] {
        match self {
            LineEnd::None => b"",
            LineEnd::Cr => b"\r",
            LineEnd::Lf => b"\n",
            LineEnd::CrLf => b"\r\n",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SendRow {
    pub text: String,
    pub hex: bool,
    /// Single mode: repeat this line on its own timer.
    pub repeat: bool,
    pub interval_ms: u64,
    /// Loop mode: take part in the sequence, then pause this long.
    pub in_seq: bool,
    pub pause_ms: u64,
}

impl Default for SendRow {
    fn default() -> Self {
        Self { text: String::new(), hex: false, repeat: false, interval_ms: 1000, in_seq: true, pause_ms: 500 }
    }
}

impl SendRow {
    fn new(text: &str, hex: bool) -> Self {
        Self { text: text.into(), hex, ..Default::default() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum SendMode {
    Lines,
    Loop,
    Script,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TermSettings {
    pub view: View,
    pub show_special: bool,
    pub timestamps: bool,
    pub show_tx: bool,
    pub show_peer: bool,
    pub echo: bool,
    pub line_end: LineEnd,
    pub rows: Vec<SendRow>,
    pub mode: SendMode,
    /// 0 = endless
    pub seq_loops: u32,
    pub script: String,
}

impl Default for TermSettings {
    fn default() -> Self {
        Self {
            view: View::Text,
            show_special: false,
            timestamps: true,
            show_tx: true,
            show_peer: true,
            echo: false,
            line_end: LineEnd::None,
            rows: vec![SendRow::new("Hallo$0D$0A", false), SendRow::new("01 03 00 00 00 0A", true), SendRow::new("", false)],
            mode: SendMode::Lines,
            seq_loops: 0,
            script: script::EXAMPLES[0].1.to_string(),
        }
    }
}

struct Seq {
    idx: usize,
    round: u32,
    next_at: Instant,
}

pub struct Terminal {
    pub s: TermSettings,
    entries: VecDeque<Entry>,
    pub paused: bool,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    log_file: Option<(String, File)>,
    last_repeat: Vec<Instant>,
    seq: Option<Seq>,
    script: Option<ScriptRun>,
    focus_row: usize,
    start_script: bool,
}

/// Bytes the send area wants to send.
pub struct SendRequest {
    pub data: Vec<u8>,
}

impl Terminal {
    pub fn new(mut s: TermSettings) -> Self {
        // scripts saved by an earlier version were Rhai (// comments); start fresh with Lua
        if s.script.trim_start().starts_with("//") {
            s.script = script::EXAMPLES[0].1.to_string();
        }
        Self {
            s,
            entries: VecDeque::new(),
            paused: false,
            rx_bytes: 0,
            tx_bytes: 0,
            log_file: None,
            last_repeat: Vec::new(),
            seq: None,
            script: None,
            focus_row: 0,
            start_script: false,
        }
    }

    pub fn busy(&self) -> bool {
        self.seq.is_some() || self.script.is_some() || self.s.rows.iter().any(|r| r.repeat)
    }

    /// Received data is also handed to a running script.
    pub fn feed_script(&self, data: &[u8]) {
        if let Some(s) = &self.script {
            s.feed(data);
        }
    }

    /// Insert one byte into the send line that last had focus
    /// (used by the ASCII table).
    pub fn insert_byte(&mut self, b: u8) {
        if self.s.rows.is_empty() {
            self.s.rows.push(SendRow::default());
        }
        let i = self.focus_row.min(self.s.rows.len() - 1);
        let row = &mut self.s.rows[i];
        if row.hex {
            if !row.text.is_empty() && !row.text.ends_with(' ') {
                row.text.push(' ');
            }
            row.text.push_str(&format!("{b:02X}"));
        } else {
            match b {
                b'$' => row.text.push_str("$$"),
                b'#' => row.text.push_str("##"),
                0x20..=0x7E => row.text.push(b as char),
                _ => row.text.push_str(&format!("${b:02X}")),
            }
        }
    }

    fn encode_row(row: &SendRow, line_end: LineEnd) -> Result<Vec<u8>, String> {
        let mut d = if row.hex { codec::parse_hex(&row.text)? } else { codec::parse_text(&row.text)? };
        if !row.hex {
            d.extend_from_slice(line_end.bytes());
        }
        if d.is_empty() {
            return Err("Nichts zu senden – Zeile ist leer".into());
        }
        Ok(d)
    }

    /// Timers, loops and scripts. Call every frame, visible or not.
    pub fn tick(&mut self, ctx: &egui::Context, connected: bool) -> Vec<SendRequest> {
        let mut out = Vec::new();
        let now = Instant::now();
        self.last_repeat.resize(self.s.rows.len(), now);
        let line_end = self.s.line_end;

        // per-line repeat (single mode only)
        if self.s.mode == SendMode::Lines && connected {
            let mut errs = Vec::new();
            for (i, row) in self.s.rows.iter_mut().enumerate() {
                if row.repeat {
                    if now.duration_since(self.last_repeat[i]) >= Duration::from_millis(row.interval_ms) {
                        self.last_repeat[i] = now;
                        match Self::encode_row(row, line_end) {
                            Ok(d) => out.push(SendRequest { data: d }),
                            Err(e) => {
                                row.repeat = false;
                                errs.push(e);
                            }
                        }
                    }
                    ctx.request_repaint_after(Duration::from_millis(row.interval_ms.min(100)));
                }
            }
            for e in errs {
                self.error(e);
            }
        }

        // loop / sequence
        if let Some(mut seq) = self.seq.take() {
            let mut keep = true;
            if !connected {
                keep = false;
                self.info("Schleife beendet (Verbindung geschlossen)");
            } else if now >= seq.next_at {
                let rows: Vec<usize> = (0..self.s.rows.len()).filter(|&i| self.s.rows[i].in_seq).collect();
                if rows.is_empty() {
                    keep = false;
                    self.error("Keine Zeile für die Schleife ausgewählt");
                } else {
                    let ri = rows[seq.idx.min(rows.len() - 1)];
                    let row = &self.s.rows[ri];
                    let pause = row.pause_ms;
                    match Self::encode_row(row, line_end) {
                        Ok(d) => {
                            out.push(SendRequest { data: d });
                            seq.next_at = now + Duration::from_millis(pause);
                            seq.idx += 1;
                            if seq.idx >= rows.len() {
                                seq.idx = 0;
                                seq.round += 1;
                                if self.s.seq_loops > 0 && seq.round >= self.s.seq_loops {
                                    keep = false;
                                    self.info(format!("Schleife fertig ({} Durchläufe)", seq.round));
                                }
                            }
                        }
                        Err(e) => {
                            keep = false;
                            self.error(format!("Schleife gestoppt – Zeile {}: {e}", ri + 1));
                        }
                    }
                }
            }
            if keep {
                ctx.request_repaint_after(seq.next_at.saturating_duration_since(now).min(Duration::from_millis(100)));
                self.seq = Some(seq);
            }
        }

        // script
        if self.start_script {
            self.start_script = false;
            self.script = Some(script::run(ctx.clone(), self.s.script.clone()));
        }
        if let Some(run) = &self.script {
            let mut done = false;
            let mut msgs = Vec::new();
            for m in run.poll() {
                match m {
                    ScriptMsg::Send(d) => {
                        if connected {
                            out.push(SendRequest { data: d });
                        } else {
                            msgs.push((Dir::Error, "Skript: nicht verbunden – nichts gesendet".to_string()));
                        }
                    }
                    ScriptMsg::Log(s) => msgs.push((Dir::Info, format!("Skript: {s}"))),
                    ScriptMsg::Error(s) => msgs.push((Dir::Error, s)),
                    ScriptMsg::Done => done = true,
                }
            }
            for (d, m) in msgs {
                self.push(d, None, m.into_bytes());
            }
            if done {
                self.script = None;
            }
        }
        out
    }

    pub fn push(&mut self, dir: Dir, peer: Option<String>, data: Vec<u8>) {
        match dir {
            Dir::Rx => self.rx_bytes += data.len() as u64,
            Dir::Tx => self.tx_bytes += data.len() as u64,
            _ => {}
        }
        let now_s = chrono::Local::now().format("%H:%M:%S%.3f").to_string();
        if let Some((_, f)) = self.log_file.as_mut() {
            let tag = match dir {
                Dir::Rx => "RX",
                Dir::Tx => "TX",
                Dir::Info => "--",
                Dir::Error => "!!",
            };
            let peer_s = peer.as_deref().map(|p| format!(" {p}")).unwrap_or_default();
            let body = match dir {
                Dir::Rx | Dir::Tx => format!("{}  |{}|", codec::to_hex(&data), codec::to_text(&data, true)),
                _ => String::from_utf8_lossy(&data).into_owned(),
            };
            let _ = writeln!(f, "{now_s} {tag}{peer_s} {body}");
        }
        if self.paused && matches!(dir, Dir::Rx | Dir::Tx) {
            return;
        }
        // merge quickly arriving pieces of the same stream into one entry
        if let Some(last) = self.entries.back_mut() {
            if last.dir == dir
                && dir == Dir::Rx
                && last.peer == peer
                && last.last.elapsed() < Duration::from_millis(30)
                && last.data.len() < 4096
            {
                last.data.extend_from_slice(&data);
                last.last = Instant::now();
                return;
            }
        }
        self.entries.push_back(Entry { time: now_s, dir, peer, data, last: Instant::now() });
        while self.entries.len() > MAX_ENTRIES {
            self.entries.pop_front();
        }
    }

    pub fn info(&mut self, msg: impl Into<String>) {
        self.push(Dir::Info, None, msg.into().into_bytes());
    }

    pub fn error(&mut self, msg: impl Into<String>) {
        self.push(Dir::Error, None, msg.into().into_bytes());
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.rx_bytes = 0;
        self.tx_bytes = 0;
    }

    fn export_text(&self) -> String {
        let mut s = String::new();
        for e in &self.entries {
            let tag = match e.dir {
                Dir::Rx => "RX",
                Dir::Tx => "TX",
                Dir::Info => "--",
                Dir::Error => "!!",
            };
            let peer = e.peer.as_deref().map(|p| format!(" {p}")).unwrap_or_default();
            let body = match e.dir {
                Dir::Rx | Dir::Tx => format!("{}  |{}|", codec::to_hex(&e.data), codec::to_text(&e.data, true)),
                _ => String::from_utf8_lossy(&e.data).into_owned(),
            };
            s.push_str(&format!("{} {tag}{peer} {body}\n", e.time));
        }
        s
    }

    // ------------------------------------------------------------- log UI

    pub fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt(ui.scope_id().with("view"))
                .selected_text(self.s.view.label())
                .width(100.0)
                .show_ui(ui, |ui| {
                    for v in [View::Text, View::Hex, View::Both, View::Dec] {
                        ui.selectable_value(&mut self.s.view, v, v.label());
                    }
                });
            ui.checkbox(&mut self.s.timestamps, "Zeit")
                .on_hover_text("Uhrzeit vor jedem Eintrag anzeigen");
            ui.checkbox(&mut self.s.show_special, "Steuerzeichen")
                .on_hover_text("Nicht druckbare Zeichen als {0D} anzeigen");
            ui.checkbox(&mut self.s.show_tx, "Gesendetes")
                .on_hover_text("Auch gesendete Daten im Fenster zeigen");
            ui.checkbox(&mut self.s.echo, "Echo")
                .on_hover_text("Empfangene Daten sofort an den Absender zurückschicken");
            ui.separator();
            let pause_label = if self.paused { "▶ Weiter" } else { "⏸ Pause" };
            ui.toggle_value(&mut self.paused, pause_label)
                .on_hover_text("Anzeige anhalten (Mitschrift in Datei läuft weiter)");
            if ui.button("🗑 Leeren").clicked() {
                self.clear();
            }
            if ui.button("💾 Speichern").on_hover_text("Inhalt des Fensters als Textdatei speichern").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Text", &["txt", "log"])
                    .set_file_name("mitschnitt.txt")
                    .save_file()
                {
                    match std::fs::write(&path, self.export_text()) {
                        Ok(()) => self.info(format!("Gespeichert: {}", path.display())),
                        Err(e) => self.error(format!("Speichern fehlgeschlagen: {e}")),
                    }
                }
            }
            let logging = self.log_file.is_some();
            let label = if logging { "⏺ Mitschrift läuft" } else { "⏺ Mitschreiben" };
            let btn = ui.add(egui::Button::new(label).selected(logging));
            let btn = match &self.log_file {
                Some((p, _)) => btn.on_hover_text(format!("Schreibt nach {p}. Klick beendet.")),
                None => btn.on_hover_text("Alles laufend in eine Datei mitschreiben"),
            };
            if btn.clicked() {
                if logging {
                    let (p, _) = self.log_file.take().unwrap();
                    self.info(format!("Mitschrift beendet: {p}"));
                } else if let Some(path) = rfd::FileDialog::new()
                    .add_filter("Log", &["log", "txt"])
                    .set_file_name("mitschrift.log")
                    .save_file()
                {
                    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                        Ok(f) => {
                            let p = path.display().to_string();
                            self.log_file = Some((p.clone(), f));
                            self.info(format!("Mitschrift nach {p}"));
                        }
                        Err(e) => self.error(format!("Datei nicht beschreibbar: {e}")),
                    }
                }
            }
        });
    }

    pub fn log_ui(&mut self, ui: &mut egui::Ui) {
        let dark = ui.visuals().dark_mode;
        let font = FontId::monospace(13.0);
        let small = FontId::monospace(11.5);
        let c_rx = ui.visuals().text_color();
        let c_tx = if dark { Color32::from_rgb(110, 170, 255) } else { Color32::from_rgb(20, 80, 190) };
        let c_info = if dark { Color32::from_gray(140) } else { Color32::from_gray(110) };
        let c_err = if dark { Color32::from_rgb(255, 110, 100) } else { Color32::from_rgb(190, 30, 20) };
        let c_meta = if dark { Color32::from_gray(120) } else { Color32::from_gray(130) };

        let mut job = LayoutJob::default();
        let mut prev: Option<(Dir, Option<String>, bool)> = None; // dir, peer, ended with newline
        for e in &self.entries {
            if e.dir == Dir::Tx && !self.s.show_tx {
                continue;
            }
            let color = match e.dir {
                Dir::Rx => c_rx,
                Dir::Tx => c_tx,
                Dir::Info => c_info,
                Dir::Error => c_err,
            };
            let is_data = matches!(e.dir, Dir::Rx | Dir::Tx);
            let continuous = is_data
                && !self.s.timestamps
                && self.s.view == View::Text
                && prev.as_ref().is_some_and(|(d, p, nl)| *d == e.dir && *p == e.peer && !*nl);
            if !continuous {
                if prev.is_some() && !job.text.ends_with('\n') {
                    job.append("\n", 0.0, TextFormat::simple(font.clone(), color));
                }
                if self.s.timestamps {
                    job.append(&format!("{} ", e.time), 0.0, TextFormat::simple(small.clone(), c_meta));
                }
                let tag = match e.dir {
                    Dir::Rx => "◀ ",
                    Dir::Tx => "▶ ",
                    Dir::Info => "• ",
                    Dir::Error => "⚠ ",
                };
                job.append(tag, 0.0, TextFormat::simple(font.clone(), color));
                if self.s.show_peer {
                    if let Some(p) = &e.peer {
                        job.append(&format!("[{p}] "), 0.0, TextFormat::simple(small.clone(), c_meta));
                    }
                }
            }
            let body = if is_data {
                match self.s.view {
                    View::Text => codec::to_text(&e.data, self.s.show_special),
                    View::Hex => codec::to_hex(&e.data),
                    View::Dec => codec::to_dec(&e.data),
                    View::Both => format!("{}   {}", codec::to_hex(&e.data), codec::to_text(&e.data, true)),
                }
            } else {
                String::from_utf8_lossy(&e.data).into_owned()
            };
            job.append(&body, 0.0, TextFormat::simple(font.clone(), color));
            let nl = body.ends_with('\n');
            prev = Some((e.dir, e.peer.clone(), nl));
        }
        job.wrap.max_width = ui.available_width();

        egui::Frame::new()
            .fill(if dark { Color32::from_gray(18) } else { Color32::WHITE })
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
            .inner_margin(6.0)
            .corner_radius(4.0)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                egui::ScrollArea::both()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.entries.is_empty() {
                            ui.label(RichText::new("Noch keine Daten.").color(c_info).italics());
                        } else {
                            ui.add(egui::Label::new(job).selectable(true));
                        }
                    });
            });
    }
}
