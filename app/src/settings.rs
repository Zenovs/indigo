//! Einstellungen als JSON im Config-Verzeichnis des Nutzers
//! ($XDG_CONFIG_HOME/indigo/settings.json, sonst ~/.config/indigo/settings.json).

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Serialize, Deserialize, Clone)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub collapsed: bool,
    pub interval_ms: u64,
    /// None = noch nie entschieden -> beim ersten Start aktivieren
    pub autostart: Option<bool>,
    /// beim Start die neueste GitHub-Release-Version holen
    pub autoupdate: bool,
    /// über allen Fenstern liegen; false = normales Fenster, das andere
    /// Fenster verdecken dürfen
    pub always_on_top: bool,
    /// Felder neuerer Versionen unverändert mitschreiben, statt sie beim
    /// nächsten Speichern zu verlieren
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            x: None,
            y: None,
            collapsed: false,
            interval_ms: 1500,
            autostart: None,
            autoupdate: true,
            always_on_top: true,
            extra: serde_json::Map::new(),
        }
    }
}

pub struct SettingsStore {
    path: PathBuf,
    inner: Mutex<Settings>,
    last_write: Mutex<Instant>,
}

impl SettingsStore {
    pub fn load() -> Self {
        let path = config_dir().join("settings.json");
        let settings = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
        Self {
            path,
            inner: Mutex::new(settings),
            last_write: Mutex::new(Instant::now() - Duration::from_secs(60)),
        }
    }

    pub fn get(&self) -> Settings {
        self.inner.lock().unwrap().clone()
    }

    /// Ändern und sofort schreiben.
    pub fn update(&self, f: impl FnOnce(&mut Settings)) {
        let snapshot = {
            let mut guard = self.inner.lock().unwrap();
            f(&mut guard);
            guard.clone()
        };
        self.write(&snapshot);
        *self.last_write.lock().unwrap() = Instant::now();
    }

    /// Ändern, aber höchstens alle 500 ms schreiben (für Move-Events beim
    /// Ziehen). Der letzte Stand wird spätestens beim Beenden geschrieben.
    pub fn update_throttled(&self, f: impl FnOnce(&mut Settings)) {
        let snapshot = {
            let mut guard = self.inner.lock().unwrap();
            f(&mut guard);
            guard.clone()
        };
        let mut last = self.last_write.lock().unwrap();
        if last.elapsed() >= Duration::from_millis(500) {
            self.write(&snapshot);
            *last = Instant::now();
        }
    }

    /// Aktuellen Stand ungedrosselt schreiben (beim Beenden).
    pub fn flush(&self) {
        let snapshot = self.inner.lock().unwrap().clone();
        self.write(&snapshot);
    }

    fn write(&self, settings: &Settings) {
        use std::io::Write;
        if let Some(dir) = self.path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let Ok(json) = serde_json::to_string_pretty(settings) else {
            return;
        };
        // atomar: erst tempdatei im selben verzeichnis, dann rename —
        // ein absturz mitten im schreiben hinterlässt so nie ein kaputtes json
        let tmp = self.path.with_extension("json.tmp");
        let ok = fs::File::create(&tmp)
            .and_then(|mut f| {
                f.write_all(json.as_bytes())?;
                f.sync_all()
            })
            .is_ok();
        if ok {
            let _ = fs::rename(&tmp, &self.path);
        } else {
            let _ = fs::remove_file(&tmp);
        }
    }
}

fn config_dir() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            home.join(".config")
        });
    base.join("indigo")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vordergrund_einstellung() {
        // settings.json von v0.2 kennt das feld nicht: weiter immer oben
        let alt: Settings = serde_json::from_str(r#"{"x":10,"y":20,"autoupdate":false}"#).unwrap();
        assert!(alt.always_on_top);
        assert_eq!((alt.x, alt.y, alt.autoupdate), (Some(10), Some(20), false));

        let aus: Settings = serde_json::from_str(r#"{"alwaysOnTop":false}"#).unwrap();
        assert!(!aus.always_on_top);
        let json = serde_json::to_string(&aus).unwrap();
        assert!(json.contains(r#""alwaysOnTop":false"#), "{json}");
    }

    #[test]
    fn fremde_felder_bleiben_erhalten() {
        // ein späteres indigo hat neue felder geschrieben
        let neu = r#"{"x":5,"alwaysOnTop":false,"zukunft":{"a":[1,2]},"modus":"x"}"#;
        let s: Settings = serde_json::from_str(neu).unwrap();
        assert_eq!((s.x, s.always_on_top), (Some(5), false));
        let wieder: serde_json::Value = serde_json::to_value(&s).unwrap();
        assert_eq!(wieder["zukunft"], serde_json::json!({"a": [1, 2]}));
        assert_eq!(wieder["modus"], "x");
        // bekannte felder landen nicht doppelt im rest
        assert!(!s.extra.contains_key("alwaysOnTop") && !s.extra.contains_key("x"));
    }
}
