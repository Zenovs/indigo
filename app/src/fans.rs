//! Lüfter über hwmon: Drehzahl lesen (fanN_input) und PWM steuern
//! (pwmN / pwmN_enable). Die Kanäle werden bei jedem Tick neu erkannt,
//! damit ein nachträglich geladener Treiber (modprobe nct6775) ohne
//! Neustart auftaucht.

use serde::Serialize;
use std::collections::HashMap;
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
}

struct Channel {
    id: String,
    label: String,
    fan_input: Option<PathBuf>,
    pwm: Option<PathBuf>,
    pwm_enable: Option<PathBuf>,
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
                }
            })
            .collect()
    }

    /// Manuellen PWM-Wert setzen (0–100 %).
    pub fn set_manual(&self, id: &str, pct: f32) -> Result<(), String> {
        let inner = self.inner.lock().unwrap();
        let ch = inner.channel(id)?;
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

fn read_u32(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn read_u8(path: &Path) -> Option<u8> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn write_sysfs(path: &Path, value: &str) -> Result<(), String> {
    fs::write(path, value).map_err(|e| format!("{}: {e}", path.display()))
}
