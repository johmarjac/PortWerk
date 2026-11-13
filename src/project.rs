//! Project files (*.portwerk): all sessions and settings in one JSON file
//! that can be passed on to colleagues, plus the Windows file association.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::app::Saved;

pub const EXT: &str = "portwerk";
const FORMAT: &str = "portwerk-projekt";

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    format: String,
    version: u32,
    app_version: String,
    project: Saved,
}

pub fn save(path: &Path, project: &Saved) -> Result<(), String> {
    let f = ProjectFile {
        format: FORMAT.into(),
        version: 1,
        app_version: env!("CARGO_PKG_VERSION").into(),
        project: project.clone(),
    };
    let text = serde_json::to_string_pretty(&f).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("Projekt konnte nicht gespeichert werden: {e}"))
}

pub fn load(path: &Path) -> Result<Saved, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("Projekt konnte nicht gelesen werden: {e}"))?;
    let f: ProjectFile = serde_json::from_str(&text).map_err(|e| format!("Keine gültige PortWerk-Projektdatei: {e}"))?;
    if f.format != FORMAT {
        return Err("Keine PortWerk-Projektdatei".into());
    }
    Ok(f.project)
}

pub fn display_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

/// Registers *.portwerk for the current user so a double click opens PortWerk.
/// (The MSI installer does the same for all users.)
#[cfg(windows)]
pub fn register_extension() -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.display().to_string();
    let base = r"HKCU\Software\Classes";
    let entries: [(String, String); 4] = [
        (format!(r"{base}\.{EXT}"), "PortWerk.Project".into()),
        (format!(r"{base}\PortWerk.Project"), "PortWerk-Projekt".into()),
        (format!(r"{base}\PortWerk.Project\DefaultIcon"), format!("\"{exe}\",0")),
        (format!(r"{base}\PortWerk.Project\shell\open\command"), format!("\"{exe}\" \"%1\"")),
    ];
    for (key, value) in entries {
        let out = std::process::Command::new("reg")
            .args(["add", &key, "/ve", "/d", &value, "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("Registry konnte nicht geändert werden: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "Registry konnte nicht geändert werden: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    Ok(format!(".{EXT}-Dateien öffnen jetzt mit {exe}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_roundtrip() {
        let mut p = Saved::default();
        p.tcpc_sessions[0].host = "10.0.0.7".into();
        p.modbus_sessions[0].unit = 17;
        let path = std::env::temp_dir().join("portwerk_test.portwerk");
        save(&path, &p).unwrap();
        let q = load(&path).unwrap();
        assert_eq!(q.tcpc_sessions[0].host, "10.0.0.7");
        assert_eq!(q.modbus_sessions[0].unit, 17);
        let _ = std::fs::remove_file(path);
    }
}
