//! The send area below the log: single lines, a loop of lines, or a script.

use std::time::Instant;

use eframe::egui::{self, Color32, RichText};

use super::{LineEnd, Seq, SendMode, SendRequest, SendRow, Terminal};
use crate::script;

const ESCAPE_HELP: &str = "Text-Zeilen: $0D = Byte als Hex, #13 = Byte als Dezimalzahl,\n\
\\r \\n \\t = Steuerzeichen, $$ und ## = das Zeichen selbst.\n\
HEX-Zeilen: Bytes als Hex, z. B. 01 03 0A FF.\n\
Enter im Feld sendet die Zeile. Die ASCII-Tabelle (oben) fügt Zeichen\n\
in die zuletzt angeklickte Zeile ein.";

impl Terminal {
    /// Draws the send area. Returns what a click asked to send.
    pub fn send_ui(&mut self, ui: &mut egui::Ui, connected: bool) -> Vec<SendRequest> {
        let mut out = Vec::new();

        ui.horizontal(|ui| {
            ui.label(RichText::new("Senden").strong());
            for (m, label, tip) in [
                (SendMode::Lines, "✉ Zeilen", "Einzelne Zeilen per Klick oder Timer senden"),
                (SendMode::Loop, "🔁 Schleife", "Mehrere Zeilen nacheinander mit Pausen senden, beliebig oft"),
                (SendMode::Script, "📜 Skript", "Ein Skript für komplexere Abläufe und Protokolle"),
            ] {
                ui.selectable_value(&mut self.s.mode, m, label).on_hover_text(tip);
            }
            ui.separator();
            if self.s.mode != SendMode::Script {
                egui::ComboBox::from_id_salt(ui.scope_id().with("lineend"))
                    .selected_text(self.s.line_end.label())
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for v in [LineEnd::None, LineEnd::Cr, LineEnd::Lf, LineEnd::CrLf] {
                            ui.selectable_value(&mut self.s.line_end, v, v.label());
                        }
                    })
                    .response
                    .on_hover_text("Wird an Text-Zeilen automatisch angehängt");
                ui.label(RichText::new("ℹ Hilfe").weak()).on_hover_text(ESCAPE_HELP);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("➕ Zeile").clicked() {
                        self.s.rows.push(SendRow::default());
                    }
                });
            }
        });

        match self.s.mode {
            SendMode::Lines => self.lines_ui(ui, connected, &mut out),
            SendMode::Loop => self.loop_ui(ui, connected),
            SendMode::Script => self.script_ui(ui),
        }

        ui.label(
            RichText::new(format!("Empfangen: {} Byte   Gesendet: {} Byte", self.rx_bytes, self.tx_bytes))
                .small()
                .weak(),
        );
        out
    }

    fn lines_ui(&mut self, ui: &mut egui::Ui, connected: bool, out: &mut Vec<SendRequest>) {
        let mut remove = None;
        let mut errors = Vec::new();
        let line_end = self.s.line_end;
        let n = self.s.rows.len();
        self.last_repeat.resize(n, Instant::now());
        for i in 0..n {
            let row = &mut self.s.rows[i];
            ui.horizontal(|ui| {
                let w = (ui.available_width() - 300.0).max(120.0);
                let te = ui.add(
                    egui::TextEdit::singleline(&mut row.text)
                        .font(egui::TextStyle::Monospace)
                        .hint_text(if row.hex { "z. B. 01 02 0A" } else { "Text, z. B. Hallo$0D$0A" })
                        .desired_width(w),
                );
                if te.has_focus() || te.clicked() {
                    self.focus_row = i;
                }
                let enter = te.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                ui.checkbox(&mut row.hex, "HEX").on_hover_text("Eingabe als Hex-Bytes");
                let send = ui.add_enabled(connected, egui::Button::new("Senden")).clicked() || (enter && connected);
                ui.checkbox(&mut row.repeat, "alle").on_hover_text("Automatisch wiederholt senden");
                ui.add(egui::DragValue::new(&mut row.interval_ms).range(10..=3_600_000).suffix(" ms").speed(10.0));
                if n > 1 && ui.small_button("✖").on_hover_text("Zeile entfernen").clicked() {
                    remove = Some(i);
                }
                if send {
                    self.last_repeat[i] = Instant::now();
                    match Terminal::encode_row(row, line_end) {
                        Ok(d) => out.push(SendRequest { data: d }),
                        Err(e) => errors.push(e),
                    }
                }
            });
        }
        if let Some(i) = remove {
            self.s.rows.remove(i);
            self.last_repeat.remove(i);
            self.focus_row = self.focus_row.min(self.s.rows.len().saturating_sub(1));
        }
        for e in errors {
            self.error(e);
        }
    }

    fn loop_ui(&mut self, ui: &mut egui::Ui, connected: bool) {
        let running = self.seq.is_some();
        let current = self.seq.as_ref().map(|s| s.idx);
        let seq_rows: Vec<usize> = (0..self.s.rows.len()).filter(|&i| self.s.rows[i].in_seq).collect();
        let mut remove = None;
        let n = self.s.rows.len();
        for i in 0..n {
            let row = &mut self.s.rows[i];
            ui.horizontal(|ui| {
                let is_next = running && current.and_then(|c| seq_rows.get(c)) == Some(&i);
                ui.label(RichText::new(if is_next { "▶" } else { " " }).color(Color32::from_rgb(30, 160, 70)).monospace());
                ui.checkbox(&mut row.in_seq, "").on_hover_text("Diese Zeile gehört zur Schleife");
                let w = (ui.available_width() - 270.0).max(120.0);
                let te = ui.add(
                    egui::TextEdit::singleline(&mut row.text)
                        .font(egui::TextStyle::Monospace)
                        .hint_text(if row.hex { "z. B. 01 02 0A" } else { "Text" })
                        .desired_width(w),
                );
                if te.has_focus() || te.clicked() {
                    self.focus_row = i;
                }
                ui.checkbox(&mut row.hex, "HEX");
                ui.label("dann");
                ui.add(egui::DragValue::new(&mut row.pause_ms).range(0..=3_600_000).suffix(" ms").speed(10.0))
                    .on_hover_text("Pause nach dieser Zeile");
                if n > 1 && ui.small_button("✖").on_hover_text("Zeile entfernen").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            if !running {
                self.s.rows.remove(i);
                self.focus_row = self.focus_row.min(self.s.rows.len().saturating_sub(1));
            }
        }
        ui.horizontal(|ui| {
            ui.label("Durchläufe");
            ui.add(egui::DragValue::new(&mut self.s.seq_loops).range(0..=1_000_000))
                .on_hover_text("0 = endlos, bis Stopp");
            if self.s.seq_loops == 0 {
                ui.label(RichText::new("(endlos)").weak());
            }
            let label = if running { "⏹ Stopp" } else { "▶ Schleife starten" };
            let fill = if running { Color32::from_rgb(200, 70, 60) } else { Color32::from_rgb(0, 130, 160) };
            if ui
                .add_enabled(
                    connected || running,
                    egui::Button::new(RichText::new(label).color(Color32::WHITE).strong()).fill(fill),
                )
                .clicked()
            {
                if running {
                    self.seq = None;
                    self.info("Schleife gestoppt");
                } else {
                    self.seq = Some(Seq { idx: 0, round: 0, next_at: Instant::now() });
                    self.info("Schleife gestartet");
                }
            }
            if let Some(s) = &self.seq {
                let total = if self.s.seq_loops == 0 { "∞".to_string() } else { self.s.seq_loops.to_string() };
                ui.label(RichText::new(format!("Durchlauf {} / {total}", s.round + 1)).weak());
            }
            if !connected && !running {
                ui.label(RichText::new("erst verbinden").weak().italics());
            }
        });
    }

    fn script_ui(&mut self, ui: &mut egui::Ui) {
        let running = self.script.is_some();
        ui.horizontal(|ui| {
            let label = if running { "⏹ Stopp" } else { "▶ Skript starten" };
            let fill = if running { Color32::from_rgb(200, 70, 60) } else { Color32::from_rgb(0, 130, 160) };
            if ui.add(egui::Button::new(RichText::new(label).color(Color32::WHITE).strong()).fill(fill)).clicked() {
                if let Some(s) = &self.script {
                    s.stop();
                } else {
                    self.start_script = true;
                }
            }
            ui.add_enabled_ui(!running, |ui| {
                egui::ComboBox::from_id_salt(ui.scope_id().with("examples"))
                    .selected_text("Beispiel laden …")
                    .width(170.0)
                    .show_ui(ui, |ui| {
                        for (name, src) in script::EXAMPLES {
                            if ui.selectable_label(false, *name).clicked() {
                                self.s.script = src.to_string();
                            }
                        }
                    });
                if ui.button("📂 Öffnen").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("Skript", &["lua", "txt"]).pick_file() {
                        match std::fs::read_to_string(&p) {
                            Ok(s) => self.s.script = s,
                            Err(e) => self.error(format!("Skript nicht lesbar: {e}")),
                        }
                    }
                }
            });
            if ui.button("💾 Speichern").clicked() {
                if let Some(p) = rfd::FileDialog::new()
                    .add_filter("Skript", &["lua", "txt"])
                    .set_file_name("skript.lua")
                    .save_file()
                {
                    if let Err(e) = std::fs::write(&p, &self.s.script) {
                        self.error(format!("Skript nicht gespeichert: {e}"));
                    }
                }
            }
            ui.label(RichText::new("ℹ Befehle").weak()).on_hover_text(RichText::new(script::HELP).monospace());
            if running {
                ui.spinner();
                ui.label(RichText::new("läuft …").color(Color32::from_rgb(30, 160, 70)));
            }
        });
        egui::ScrollArea::vertical().max_height(170.0).id_salt(ui.scope_id().with("script_scroll")).show(ui, |ui| {
            ui.add_enabled(
                !running,
                egui::TextEdit::multiline(&mut self.s.script)
                    .code_editor()
                    .desired_rows(8)
                    .desired_width(f32::INFINITY),
            );
        });
    }
}
