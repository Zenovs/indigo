//! Auto-Update beim Start: ein Hintergrund-Thread wartet kurz, fragt dann das
//! neueste GitHub-Release (Zenovs/indigo) ab und vergleicht dessen Version mit
//! der eigenen. Ist das Release strikt neuer, werden Binärdatei und SHA256SUMS
//! heruntergeladen, die Prüfsumme verifiziert und installiert: in-place per
//! atomarem rename, wenn das Verzeichnis des laufenden Binaries beschreibbar
//! ist, sonst nach $XDG_DATA_HOME/indigo/bin/indigo (der Autostart-Eintrag
//! wird dann dorthin umgebogen). Das Ergebnis geht als UpdateEvent über einen
//! glib-Kanal an den Hauptthread. Der Check ist über die Einstellung
//! `autoupdate` abschaltbar — das prüft der Aufrufer vor spawn_check, nicht
//! dieses Modul. Jeder Fehler (auch offline) wird nur nach stderr geloggt und
//! bricht den Check still ab.

use crate::autostart;
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Ergebnis eines erfolgreichen Updates.
pub enum UpdateEvent {
    /// Binary wurde in-place ersetzt; enthält den Pfad des neuen Binaries.
    /// current_exe() ist nach dem rename unbrauchbar — /proc/self/exe
    /// liefert dann "<pfad> (deleted)".
    ReadyRestart(std::path::PathBuf),
    /// Binary wurde an diesem Pfad installiert, weil das Verzeichnis des
    /// laufenden Binaries nicht beschreibbar ist.
    InstalledAt(std::path::PathBuf),
}

const RELEASE_URL: &str = "https://api.github.com/repos/Zenovs/indigo/releases/latest";
const BINARY_ASSET: &str = "indigo-x86_64-linux";
const SUMS_ASSET: &str = "SHA256SUMS";

/// Startet den Update-Check in einem eigenen Thread und kehrt sofort zurück.
pub fn spawn_check(notify: glib::Sender<UpdateEvent>) {
    std::thread::spawn(move || {
        // erst die UI starten lassen
        std::thread::sleep(Duration::from_secs(3));
        match check_and_install() {
            Ok(Some(event)) => {
                if notify.send(event).is_err() {
                    eprintln!("indigo-update: ui-kanal geschlossen");
                }
            }
            Ok(None) => {}
            Err(e) => eprintln!("indigo-update: {e}"),
        }
    });
}

/// Kompletter Ablauf; Ok(None) heisst: schon aktuell, nichts zu tun.
fn check_and_install() -> Result<Option<UpdateEvent>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(6))
        .timeout_read(Duration::from_secs(6))
        .build();

    let release: serde_json::Value = agent
        .get(RELEASE_URL)
        .set("User-Agent", "indigo-updater")
        .call()
        .map_err(|e| format!("release-abfrage fehlgeschlagen: {e}"))?
        .into_json()
        .map_err(|e| format!("release-antwort unlesbar: {e}"))?;

    let tag = release["tag_name"].as_str().ok_or("release ohne tag_name")?;
    let remote =
        parse_version(tag).ok_or_else(|| format!("unverständliches release-tag {tag:?}"))?;
    let local = parse_version(env!("CARGO_PKG_VERSION"))
        .ok_or("eigene version unverständlich")?;
    let marker = data_dir().join("letztes-update-tag");
    if remote <= local {
        let _ = fs::remove_file(&marker);
        return Ok(None);
    }
    if fs::read_to_string(&marker).map(|s| s.trim() == tag).unwrap_or(false) {
        return Err(format!(
            "release {tag} wurde bereits installiert, version ist aber weiterhin {} — \
             vermutlich passt das release nicht zum tag; überspringe",
            env!("CARGO_PKG_VERSION")
        ));
    }

    let assets = release["assets"].as_array().ok_or("release ohne assets")?;
    let binary_url = asset_url(assets, BINARY_ASSET)
        .ok_or_else(|| format!("asset {BINARY_ASSET} fehlt im release"))?;
    let sums_url = asset_url(assets, SUMS_ASSET)
        .ok_or_else(|| format!("asset {SUMS_ASSET} fehlt im release"))?;

    let dir = data_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("{} nicht anlegbar: {e}", dir.display()))?;
    let pid = std::process::id();
    let download = dir.join(format!("indigo.download.{pid}"));
    let sums_file = dir.join(format!("{SUMS_ASSET}.{pid}"));
    fetch_to_file(&agent, &binary_url, &download)?;
    fetch_to_file(&agent, &sums_url, &sums_file)?;

    let sums = fs::read_to_string(&sums_file)
        .map_err(|e| format!("{SUMS_ASSET} unlesbar: {e}"))?;
    let expected = find_sum(&sums, BINARY_ASSET)
        .ok_or_else(|| format!("keine prüfsumme für {BINARY_ASSET} in {SUMS_ASSET}"))?;
    let actual = sha256_hex(&download)?;
    if actual != expected {
        let _ = fs::remove_file(&download);
        let _ = fs::remove_file(&sums_file);
        return Err(format!(
            "prüfsummen-mismatch für {tag}: erwartet {expected}, berechnet {actual}"
        ));
    }

    let event = install(&download)?;
    let _ = fs::remove_file(&download);
    let _ = fs::remove_file(&sums_file);
    let _ = fs::write(&marker, tag);
    Ok(Some(event))
}

/// Ersetzt das laufende Binary in-place, wenn dessen Verzeichnis beschreibbar
/// ist, sonst Installation unter $XDG_DATA_HOME/indigo/bin/indigo mit
/// umgebogenem Autostart-Eintrag.
fn install(download: &Path) -> Result<UpdateEvent, String> {
    let exe = std::env::current_exe().map_err(|e| format!("eigener pfad unbekannt: {e}"))?;
    if exe.parent().map_or(false, dir_writable) {
        // atomarer tausch im selben verzeichnis (gleiches dateisystem)
        let mut staged = exe.clone().into_os_string();
        staged.push(".new");
        let staged = PathBuf::from(staged);
        stage(download, &staged)?;
        fs::rename(&staged, &exe)
            .map_err(|e| format!("austausch von {} fehlgeschlagen: {e}", exe.display()))?;
        Ok(UpdateEvent::ReadyRestart(exe))
    } else {
        let bin_dir = data_dir().join("bin");
        fs::create_dir_all(&bin_dir)
            .map_err(|e| format!("{} nicht anlegbar: {e}", bin_dir.display()))?;
        let target = bin_dir.join("indigo");
        let staged = bin_dir.join("indigo.new");
        stage(download, &staged)?;
        fs::rename(&staged, &target)
            .map_err(|e| format!("austausch von {} fehlgeschlagen: {e}", target.display()))?;
        // nur umbiegen, wenn autostart überhaupt aktiv ist — sonst würde
        // das update einen vom nutzer deaktivierten autostart reaktivieren
        if autostart::is_enabled() {
            if let Err(e) = autostart::enable_with_exec(&target) {
                eprintln!("indigo-update: autostart-eintrag nicht aktualisiert: {e}");
            }
        }
        Ok(UpdateEvent::InstalledAt(target))
    }
}

/// Kopiert den Download an den Zielpfad und setzt Ausführrechte.
fn stage(download: &Path, staged: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::copy(download, staged)
        .map_err(|e| format!("kopieren nach {} fehlgeschlagen: {e}", staged.display()))?;
    fs::set_permissions(staged, fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("rechte für {} nicht setzbar: {e}", staged.display()))
}

/// Prüft per Testdatei, ob der Nutzer in dieses Verzeichnis schreiben darf.
fn dir_writable(dir: &Path) -> bool {
    let probe = dir.join(".indigo-update-probe");
    match fs::write(&probe, b"") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// browser_download_url des Assets mit exakt diesem Namen.
fn asset_url(assets: &[serde_json::Value], name: &str) -> Option<String> {
    assets.iter().find_map(|asset| {
        if asset["name"].as_str() == Some(name) {
            asset["browser_download_url"].as_str().map(str::to_owned)
        } else {
            None
        }
    })
}

/// Lädt eine URL binär in eine Datei.
fn fetch_to_file(agent: &ureq::Agent, url: &str, dest: &Path) -> Result<(), String> {
    let response = agent
        .get(url)
        .set("User-Agent", "indigo-updater")
        .call()
        .map_err(|e| format!("download {url} fehlgeschlagen: {e}"))?;
    let mut reader = response.into_reader();
    let mut file = fs::File::create(dest)
        .map_err(|e| format!("{} nicht anlegbar: {e}", dest.display()))?;
    io::copy(&mut reader, &mut file)
        .map_err(|e| format!("download {url} abgebrochen: {e}"))?;
    Ok(())
}

/// Hex-codierter SHA256 einer Datei.
fn sha256_hex(path: &Path) -> Result<String, String> {
    use std::fmt::Write as _;
    let mut file =
        fs::File::open(path).map_err(|e| format!("{} unlesbar: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)
        .map_err(|e| format!("{} unlesbar: {e}", path.display()))?;
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

/// Sucht in einer SHA256SUMS-Datei ("<hex>  <name>" pro Zeile, Format wie
/// sha256sum) die Prüfsumme des Eintrags, dessen Name auf `name` endet
/// (toleriert Binärmodus-Stern und Pfadpräfixe), kleingeschrieben.
fn find_sum(sums: &str, name: &str) -> Option<String> {
    for line in sums.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hash), Some(entry)) = (parts.next(), parts.next()) else {
            continue;
        };
        if entry.ends_with(name) {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// Zerlegt "X.Y.Z" (führendes v/V erlaubt) in ein vergleichbares Triple;
/// Prerelease-Suffixe oder abweichende Formate ergeben None.
fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim();
    let s = s
        .strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s);
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// $XDG_DATA_HOME/indigo, sonst ~/.local/share/indigo.
fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            home.join(".local").join("share")
        });
    base.join("indigo")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versionsvergleich() {
        // gültige formate
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v0.2.0"), Some((0, 2, 0)));
        assert_eq!(parse_version("V10.20.30"), Some((10, 20, 30)));
        assert_eq!(parse_version(" v1.0.0 "), Some((1, 0, 0)));

        // fremde formate -> None
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("1.2.3-beta.1"), None);
        assert_eq!(parse_version("1.2.x"), None);
        assert_eq!(parse_version("latest"), None);
        assert_eq!(parse_version(""), None);

        // ordnung: strikt neuer / gleich / älter
        let local = parse_version("0.2.0").unwrap();
        assert!(parse_version("0.2.1").unwrap() > local);
        assert!(parse_version("0.3.0").unwrap() > local);
        assert!(parse_version("1.0.0").unwrap() > local);
        assert!(parse_version("v0.2.0").unwrap() <= local);
        assert!(parse_version("0.1.9").unwrap() <= local);
        assert!(parse_version("0.10.0").unwrap() > parse_version("0.9.9").unwrap());
    }

    #[test]
    fn sha256sums_zeilen() {
        let sums = "0123abcd  indigo-x86_64-linux\n\
                    deadbeef  anderes-asset\n";
        assert_eq!(
            find_sum(sums, "indigo-x86_64-linux").as_deref(),
            Some("0123abcd")
        );
        assert_eq!(find_sum(sums, "anderes-asset").as_deref(), Some("deadbeef"));
        assert_eq!(find_sum(sums, "fehlt"), None);

        // binärmodus-stern, pfadpräfix, grossschreibung, leere zeilen
        let messy = "\nABCDEF01 *indigo-x86_64-linux\n\n1234  ./dist/anderes\n";
        assert_eq!(
            find_sum(messy, "indigo-x86_64-linux").as_deref(),
            Some("abcdef01")
        );
        assert_eq!(find_sum("", "indigo-x86_64-linux"), None);
    }
}
