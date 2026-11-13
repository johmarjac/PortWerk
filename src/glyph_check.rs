//! Test: every character used in the user interface exists in the built-in
//! fonts (otherwise it shows up as an empty box).

#[cfg(test)]
mod tests {
    use eframe::egui::epaint::text::{FontDefinitions, Fonts, TextOptions};
    use eframe::egui::FontFamily;

    const SOURCES: &[(&str, &str)] = &[
        ("app.rs", include_str!("app.rs")),
        ("ascii.rs", include_str!("ascii.rs")),
        ("modbus.rs", include_str!("modbus.rs")),
        ("modbus_server.rs", include_str!("modbus_server.rs")),
        ("mqtt.rs", include_str!("mqtt.rs")),
        ("rest.rs", include_str!("rest.rs")),
        ("script.rs", include_str!("script.rs")),
        ("sessions.rs", include_str!("sessions.rs")),
        ("terminal.rs", include_str!("terminal.rs")),
        ("terminal/send_ui.rs", include_str!("terminal/send_ui.rs")),
        ("tools.rs", include_str!("tools.rs")),
    ];

    #[test]
    fn all_ui_characters_exist_in_fonts() {
        let mut fonts = Fonts::new(TextOptions::default(), FontDefinitions::default());
        let mut view = fonts.with_pixels_per_point(1.0);
        let prop: std::collections::BTreeSet<char> = view.characters(&FontFamily::Proportional).keys().copied().collect();
        let mono: std::collections::BTreeSet<char> = view.characters(&FontFamily::Monospace).keys().copied().collect();
        assert!(prop.contains(&'A') && prop.len() > 500, "Schriftliste leer?");
        let mut missing = Vec::new();
        for (file, src) in SOURCES {
            for (n, line) in src.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for c in line.chars().filter(|c| !c.is_ascii()) {
                    if !prop.contains(&c) && !mono.contains(&c) {
                        missing.push(format!("{file}:{} '{c}' (U+{:04X})", n + 1, c as u32));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "Zeichen fehlen in der Schrift:\n{}", missing.join("\n"));
    }
}
