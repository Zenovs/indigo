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

pub fn enable() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
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
