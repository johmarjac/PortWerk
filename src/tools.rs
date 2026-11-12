//! Tools tab: data converter, checksums, number converter.

use eframe::egui::{self, RichText, Ui};

use crate::app::section;
use crate::codec;

pub struct Tools {
    input: String,
    input_hex: bool,
    number: String,
}

impl Default for Tools {
    fn default() -> Self {
        Self { input: "123456789".into(), input_hex: false, number: "255".into() }
    }
}

fn copy_row(ui: &mut Ui, name: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(170.0, 18.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_min_width(170.0);
                ui.label(name);
            },
        );
        if ui.small_button("📋").on_hover_text("Kopieren").clicked() {
            ui.ctx().copy_text(value.to_string());
        }
        ui.add(egui::Label::new(RichText::new(value).monospace()).wrap());
    });
}

/// Rows of name / copy / value inside a light frame.
fn table(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

fn parse_number(s: &str) -> Result<u64, String> {
    let t = s.trim().replace('_', "").replace(' ', "");
    let lower = t.to_lowercase();
    let r = if let Some(h) = lower.strip_prefix("0x") {
        u64::from_str_radix(h, 16)
    } else if let Some(h) = lower.strip_prefix('$') {
        u64::from_str_radix(h, 16)
    } else if let Some(h) = lower.strip_suffix('h') {
        u64::from_str_radix(h, 16)
    } else if let Some(b) = lower.strip_prefix("0b") {
        u64::from_str_radix(b, 2)
    } else if let Some(o) = lower.strip_prefix("0o") {
        u64::from_str_radix(o, 8)
    } else {
        lower.parse::<u64>()
    };
    r.map_err(|_| "Keine gültige Zahl (Beispiele: 255, 0xFF, FFh, 0b1010)".to_string())
}

impl Tools {
    pub fn insert_byte(&mut self, b: u8) {
        if self.input_hex {
            if !self.input.is_empty() && !self.input.ends_with(' ') {
                self.input.push(' ');
            }
            self.input.push_str(&format!("{b:02X}"));
        } else {
            match b {
                b'$' => self.input.push_str("$$"),
                b'#' => self.input.push_str("##"),
                0x20..=0x7E => self.input.push(b as char),
                _ => self.input.push_str(&format!("${b:02X}")),
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        egui::CentralPanel::default().frame(egui::Frame::central_panel(ui.style()).inner_margin(16.0)).show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                section(ui, "Daten umrechnen & Prüfsummen");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.input_hex, false, "Eingabe als Text");
                    ui.selectable_value(&mut self.input_hex, true, "Eingabe als Hex");
                    ui.label(RichText::new("ℹ").weak())
                        .on_hover_text("Text: $0D, #13, \\r \\n wie in den Sendezeilen. Hex: 01 02 FF");
                });
                ui.add(
                    egui::TextEdit::multiline(&mut self.input)
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                let data = if self.input_hex { codec::parse_hex(&self.input) } else { codec::parse_text(&self.input) };
                match data {
                    Err(e) => {
                        ui.colored_label(egui::Color32::from_rgb(200, 50, 40), e);
                    }
                    Ok(d) => {
                        ui.add_space(4.0);
                        ui.columns(2, |cols| {
                            table(&mut cols[0], |ui| {
                                    copy_row(ui, "Länge", &format!("{} Byte", d.len()));
                                    copy_row(ui, "Hex", &codec::to_hex(&d));
                                    copy_row(ui, "Dezimal", &codec::to_dec(&d));
                                    copy_row(ui, "Binär", &codec::to_bin(&d));
                                    copy_row(ui, "Text", &codec::to_text(&d, true));
                                    let esc: String = d
                                        .iter()
                                        .map(|&b| match b {
                                            b'$' => "$$".to_string(),
                                            b'#' => "##".to_string(),
                                            0x20..=0x7E => (b as char).to_string(),
                                            _ => format!("${b:02X}"),
                                        })
                                        .collect();
                                    copy_row(ui, "Sendezeile", &esc);
                            });
                            table(&mut cols[1], |ui| {
                                    let s8 = codec::sum8(&d);
                                    copy_row(ui, "Summe (8 Bit)", &format!("{s8:02X}"));
                                    copy_row(ui, "Summe, 2er-Komplement", &format!("{:02X}", s8.wrapping_neg()));
                                    copy_row(ui, "XOR (8 Bit)", &format!("{:02X}", codec::xor8(&d)));
                                    copy_row(ui, "CRC-8", &format!("{:02X}", codec::crc8(&d)));
                                    let m = codec::crc16_modbus(&d);
                                    copy_row(
                                        ui,
                                        "CRC-16 Modbus",
                                        &format!("{m:04X}  (gesendet: {:02X} {:02X})", m & 0xFF, m >> 8),
                                    );
                                    copy_row(ui, "CRC-16 CCITT", &format!("{:04X}", codec::crc16_ccitt(&d)));
                                    copy_row(ui, "CRC-32", &format!("{:08X}", codec::crc32(&d)));
                            });
                        });
                    }
                }

                ui.add_space(14.0);
                section(ui, "Zahlen umrechnen");
                ui.horizontal(|ui| {
                    ui.label("Zahl");
                    ui.add(egui::TextEdit::singleline(&mut self.number).font(egui::TextStyle::Monospace).desired_width(200.0));
                    ui.label(RichText::new("z. B. 255, 0xFF, FFh, 0b1010").weak());
                });
                match parse_number(&self.number) {
                    Ok(n) => {
                        table(ui, |ui| {
                            copy_row(ui, "Dezimal", &n.to_string());
                            copy_row(ui, "Hex", &format!("{n:X}"));
                            copy_row(ui, "Binär", &format!("{n:b}"));
                            copy_row(ui, "Oktal", &format!("{n:o}"));
                            let be: Vec<u8> = n.to_be_bytes().into_iter().skip_while(|b| *b == 0).collect();
                            let be = if be.is_empty() { vec![0] } else { be };
                            let le: Vec<u8> = be.iter().rev().copied().collect();
                            copy_row(ui, "Bytes (Big Endian)", &codec::to_hex(&be));
                            copy_row(ui, "Bytes (Little Endian)", &codec::to_hex(&le));
                            if n <= 0xFF {
                                let c = n as u8;
                                copy_row(ui, "Zeichen", &crate::ascii::char_name(c));
                            }
                        });
                    }
                    Err(e) => {
                        ui.colored_label(egui::Color32::from_rgb(200, 50, 40), e);
                    }
                }
            });
        });
    }
}
