//! Autostart über eine .desktop-Datei in ~/.config/autostart — der
//! Standardmechanismus der Freedesktop-Desktops, ohne Zusatzabhängigkeit.

use std::fs;
use std::path::PathBuf;

fn desktop_file() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            home.join(".config")
        });
    base.join("autostart").join("indigo.desktop")
}

pub fn is_enabled() -> bool {
    desktop_file().exists()
}

/// Binary-Pfad aus der Exec-Zeile des bestehenden Eintrags.
pub fn exec_path() -> Option<std::path::PathBuf> {
    let content = fs::read_to_string(desktop_file()).ok()?;
    content
        .lines()
        .find_map(|l| l.strip_prefix("Exec="))
        .map(|p| PathBuf::from(p.trim()))
}

pub fn enable() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    enable_with_exec(&exe)
}

/// Autostart-Eintrag auf einen bestimmten Binary-Pfad zeigen lassen
/// (der Updater nutzt das, wenn er nicht in-place ersetzen kann).
pub fn enable_with_exec(exe: &std::path::Path) -> Result<(), String> {
    let path = desktop_file();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=indigo\n\
         Comment=resource monitor widget\n\
         Exec={}\n\
         X-GNOME-Autostart-enabled=true\n",
        exe.display()
    );
    fs::write(&path, content).map_err(|e| e.to_string())
}

pub fn disable() -> Result<(), String> {
    let path = desktop_file();
    if path.exists() {
        fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}
