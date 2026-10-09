//! Lüfter über hwmon: Drehzahl lesen (fanN_input) und PWM steuern
//! (pwmN / pwmN_enable). Die Kanäle werden bei jedem Tick neu erkannt,
//! damit ein nachträglich geladener Treiber (modprobe nct6775) ohne
//! Neustart auftaucht. Regelt der Kernel einen Lüfter selbst über eine
//! Thermal-Zone (pwm-fan am Lüfteranschluss von Raspberry Pi 5 und CM5,
//! gpio-fan am Pi 4), wird er nur angezeigt.

use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FanStat {
    pub id: String,
    pub label: String,
    pub rpm: Option<u32>,
    /// aktueller PWM-Wert in %, None wenn der Kanal kein PWM hat
    pub pct: Option<f32>,
    /// true = Automatik der Firmware, false = manuell; None = nicht steuerbar
    pub auto_mode: Option<bool>,
    /// der Kernel regelt den Lüfter über eine Thermal-Zone: anzeigen
    /// (immer auto), aber nicht steuern
    pub kernel_governed: bool,
}

struct Channel {
    id: String,
    label: String,
    fan_input: Option<PathBuf>,
    pwm: Option<PathBuf>,
    pwm_enable: Option<PathBuf>,
    kernel_governed: bool,
}

pub struct FanControl {
    inner: Mutex<Inner>,
}

struct Inner {
    channels: Vec<Channel>,
    /// pwm_enable-Wert bei der ersten Sichtung eines Kanals,
    /// zum Wiederherstellen der Automatik
    orig_enable: HashMap<String, u8>,
    reads: u64,
}

impl FanControl {
    pub fn new() -> Self {
        let mut inner = Inner {
            channels: Vec::new(),
            orig_enable: HashMap::new(),
            reads: 0,
        };
        inner.rescan();
        Self {
            inner: Mutex::new(inner),
        }
    }

    pub fn read(&self) -> Vec<FanStat> {
        let mut inner = self.inner.lock().unwrap();
        // neu erkennen nur alle 8 ticks (oder solange nichts da ist) —
        // ein nachgeladener treiber taucht so binnen ~12 s auf
        if inner.reads % 8 == 0 || inner.channels.is_empty() {
            inner.rescan();
        }
        inner.reads += 1;
        inner
            .channels
            .iter()
            .map(|ch| {
                let auto_mode = match (&ch.pwm, &ch.pwm_enable) {
                    // pwm_enable heisst dort etwas anderes; geregelt
                    // wird über die thermal-zone, also immer automatisch
                    (Some(_), _) if ch.kernel_governed => Some(true),
                    (Some(_), Some(en)) => read_u8(en).map(|v| v != 1),
                    _ => None,
                };
                FanStat {
                    id: ch.id.clone(),
                    label: ch.label.clone(),
                    rpm: ch.fan_input.as_deref().and_then(read_u32),
                    pct: ch
                        .pwm
                        .as_deref()
                        .and_then(read_u32)
                        .map(|raw| raw as f32 / 255.0 * 100.0),
                    auto_mode,
                    kernel_governed: ch.kernel_governed,
                }
            })
            .collect()
    }

    /// Manuellen PWM-Wert setzen (0–100 %).
    pub fn set_manual(&self, id: &str, pct: f32) -> Result<(), String> {
        let inner = self.inner.lock().unwrap();
        let ch = inner.channel(id)?;
        if ch.kernel_governed {
            return Err(format!("{id} wird vom kernel geregelt"));
        }
        let pwm = ch.pwm.as_ref().ok_or("kanal hat kein pwm")?;
        let raw = (pct.clamp(0.0, 100.0) / 100.0 * 255.0).round() as u8;
        if let Some(enable) = &ch.pwm_enable {
            write_sysfs(enable, "1")?;
        }
        write_sysfs(pwm, &raw.to_string())
    }

    /// Firmware-Automatik wiederherstellen.
    pub fn set_auto(&self, id: &str) -> Result<(), String> {
        let inner = self.inner.lock().unwrap();
        let ch = inner.channel(id)?;
        if ch.kernel_governed {
            return Err(format!("{id} wird vom kernel geregelt"));
        }
        let enable = ch.pwm_enable.as_ref().ok_or("kanal hat kein pwm_enable")?;
        // ursprünglichen Modus wiederherstellen; war der schon "manuell",
        // auf Thermal Cruise (2) als generischen Automatikmodus gehen
        let target = match inner.orig_enable.get(id) {
            Some(v) if *v != 1 => *v,
            _ => 2,
        };
        write_sysfs(enable, &target.to_string())
    }
}

impl Inner {
    fn rescan(&mut self) {
        let mut channels = Vec::new();
        // thermal-zonen nur lesen, wenn es einen passenden lüfter gibt
        let mut bound: Option<HashSet<String>> = None;
        if let Ok(entries) = fs::read_dir("/sys/class/hwmon") {
            let mut dirs: Vec<_> = entries.flatten().map(|e| e.path()).collect();
            dirs.sort();
            for dir in dirs {
                let chip = fs::read_to_string(dir.join("name"))
                    .map(|s| s.trim().to_string())
                    .unwrap_or_default();
                // GPU-Lüfter kommt über NVML, nicht doppelt anzeigen
                if chip.starts_with("nvidia") {
                    continue;
                }
                for idx in 1..=8u32 {
                    let fan_input = dir.join(format!("fan{idx}_input"));
                    let pwm = dir.join(format!("pwm{idx}"));
                    let pwm_enable = dir.join(format!("pwm{idx}_enable"));
                    let has_fan = fan_input.exists();
                    let has_pwm = pwm.exists();
                    if !has_fan && !has_pwm {
                        continue;
                    }
                    let id = format!("{chip}:{idx}");
                    if !self.orig_enable.contains_key(&id) {
                        if let Some(v) = read_u8(&pwm_enable) {
                            self.orig_enable.insert(id.clone(), v);
                        }
                    }
                    channels.push(Channel {
                        id,
                        label: format!("fan{idx}"),
                        fan_input: has_fan.then_some(fan_input),
                        pwm: has_pwm.then_some(pwm),
                        pwm_enable: pwm_enable.exists().then_some(pwm_enable),
                        kernel_governed: cooling_type(&chip).is_some_and(|ty| {
                            bound
                                .get_or_insert_with(|| {
                                    bound_cooling_types(Path::new("/sys/class/thermal"))
                                })
                                .contains(ty)
                        }),
                    });
                }
            }
        }
        self.channels = channels;
    }

    fn channel(&self, id: &str) -> Result<&Channel, String> {
        self.channels
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| format!("unbekannter kanal: {id}"))
    }
}

/// Kühlstufen-Typ, unter dem der Treiber dieses hwmon-Chips an einer
/// Thermal-Zone hängt. Nur Treiber, deren PWM-Wert der Kernel bei jedem
/// Trip-Wechsel selbst setzt — ein manueller Wert hielte dort nicht. (emc2305
/// fehlt bewusst: dort wirkt ein manueller Wert als Untergrenze.)
fn cooling_type(chip: &str) -> Option<&'static str> {
    match chip {
        "pwmfan" => Some("pwm-fan"),
        "gpio_fan" => Some("gpio-fan"),
        _ => None,
    }
}

/// Typen aller Kühlstufen, die an einer Thermal-Zone hängen. (cdevN
/// verlinkt auf das cooling_device; das hat keinen Rückverweis auf das
/// hwmon-Gerät, darum der Typname.)
fn bound_cooling_types(thermal: &Path) -> HashSet<String> {
    let mut types = HashSet::new();
    let Ok(zones) = fs::read_dir(thermal) else {
        return types;
    };
    for zone in zones.flatten() {
        if !zone.file_name().to_string_lossy().starts_with("thermal_zone") {
            continue;
        }
        let Ok(entries) = fs::read_dir(zone.path()) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            // cdev0, cdev1 … (nicht cdev0_trip_point, cdev0_weight)
            let is_cdev = name.len() > 4
                && name.starts_with("cdev")
                && name[4..].bytes().all(|b| b.is_ascii_digit());
            if is_cdev {
                if let Ok(t) = fs::read_to_string(e.path().join("type")) {
                    types.insert(t.trim().to_string());
                }
            }
        }
    }
    types
}

fn read_u32(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_u8(path: &Path) -> Option<u8> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn write_sysfs(path: &Path, value: &str) -> Result<(), String> {
    fs::write(path, value).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kuehlstufen_an_thermal_zonen() {
        let root = std::env::temp_dir().join(format!("indigo-thermal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let typ = |dir: &str, ty: &str| {
            let dir = root.join(dir);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("type"), format!("{ty}\n")).unwrap();
        };
        let gebunden = |ty: &str| bound_cooling_types(&root).contains(ty);
        assert!(!gebunden("pwm-fan"), "fehlendes verzeichnis");

        // zone mit pcie-drossel; der pwm-fan existiert, ist aber nicht gebunden
        typ("thermal_zone0/cdev0", "PCIe_Port_Link_Speed_0001:00:00.0");
        fs::write(root.join("thermal_zone0/cdev0_trip_point"), "0\n").unwrap();
        typ("cooling_device2", "pwm-fan");
        assert!(!gebunden("pwm-fan"));
        assert!(gebunden("PCIe_Port_Link_Speed_0001:00:00.0"));

        // wie auf dem raspberry pi 5: pwm-fan an den aktiven trips der cpu-zone
        typ("thermal_zone0/cdev1", "pwm-fan");
        assert!(gebunden("pwm-fan") && !gebunden("gpio-fan"));
        // pi 4 mit gehäuselüfter aus raspi-config
        typ("thermal_zone1/cdev0", "gpio-fan");
        assert!(gebunden("gpio-fan"));

        assert_eq!(cooling_type("pwmfan"), Some("pwm-fan"));
        assert_eq!(cooling_type("gpio_fan"), Some("gpio-fan"));
        assert_eq!(cooling_type("nct6798"), None);
        assert_eq!(cooling_type("emc2305"), None);

        let _ = fs::remove_dir_all(&root);
    }
}
