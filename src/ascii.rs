//! The ASCII table window: decimal, hex, character and name.
//! A click hands the byte back so it can be inserted into a send line.

use eframe::egui::{self, RichText};

const CONTROL: [&str; 32] = [
    "NUL", "SOH", "STX", "ETX", "EOT", "ENQ", "ACK", "BEL", "BS", "TAB", "LF", "VT", "FF", "CR", "SO", "SI", "DLE",
    "DC1/XON", "DC2", "DC3/XOFF", "DC4", "NAK", "SYN", "ETB", "CAN", "EM", "SUB", "ESC", "FS", "GS", "RS", "US",
];

pub struct AsciiTable {
    pub open: bool,
    extended: bool,
    filter: String,
}

impl Default for AsciiTable {
    fn default() -> Self {
        Self { open: false, extended: false, filter: String::new() }
    }
}

pub fn char_name(b: u8) -> String {
    match b {
        0..=31 => CONTROL[b as usize].to_string(),
        32 => "Leerzeichen".into(),
        127 => "DEL".into(),
        128..=159 => "–".into(),
        _ => char::from_u32(b as u32).map(|c| c.to_string()).unwrap_or_default(),
    }
}

impl AsciiTable {
    /// Shows the window when open; returns a clicked byte.
    pub fn show(&mut self, ctx: &egui::Context) -> Option<u8> {
        let mut picked = None;
        let mut open = self.open;
        egui::Window::new("ASCII-Tabelle")
            .open(&mut open)
            .default_size([430.0, 520.0])
            .default_pos([460.0, 70.0])
            .resizable(true)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label("🔍");
                    ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Suchen: A, 0x41, 65, CR …").desired_width(170.0));
                    ui.checkbox(&mut self.extended, "128–255");
                });
                ui.label(
                    RichText::new("Klick auf eine Zeile fügt das Zeichen in die zuletzt benutzte Sendezeile ein.")
                        .small()
                        .weak(),
                );
                ui.separator();
                let f = self.filter.trim().to_lowercase();
                let max: u16 = if self.extended { 255 } else { 127 };
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    egui::Grid::new("ascii_grid").num_columns(5).striped(true).spacing([14.0, 3.0]).show(ui, |ui| {
                        for h in ["Dez", "Hex", "Bin", "Zeichen", ""] {
                            ui.label(RichText::new(h).strong());
                        }
                        ui.end_row();
                        for v in 0..=max {
                            let b = v as u8;
                            let name = char_name(b);
                            if !f.is_empty() {
                                let hex = format!("{b:02x}");
                                let hit = name.to_lowercase() == f
                                    || name.to_lowercase().contains(&f) && f.len() > 1
                                    || v.to_string() == f
                                    || hex == f.trim_start_matches("0x").trim_start_matches('$')
                                    || (f.chars().count() == 1 && name == self.filter.trim());
                                if !hit {
                                    continue;
                                }
                            }
                            let mut clicked = false;
                            clicked |= ui.add(egui::Button::new(RichText::new(v.to_string()).monospace()).frame(false)).clicked();
                            clicked |= ui.add(egui::Button::new(RichText::new(format!("{b:02X}")).monospace()).frame(false)).clicked();
                            clicked |= ui.add(egui::Button::new(RichText::new(format!("{b:08b}")).monospace().weak()).frame(false)).clicked();
                            let shown = if b < 32 || b == 127 { RichText::new(name).italics() } else { RichText::new(name).monospace().strong() };
                            clicked |= ui.add(egui::Button::new(shown).frame(false)).clicked();
                            if ui.small_button("einfügen").clicked() {
                                clicked = true;
                            }
                            if clicked {
                                picked = Some(b);
                            }
                            ui.end_row();
                        }
                    });
                });
            });
        self.open = open;
        picked
    }
}
