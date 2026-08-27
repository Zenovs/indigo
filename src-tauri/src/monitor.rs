//! Systemwerte auslesen. Jede Quelle ist einzeln gekapselt und fällt einzeln
//! aus: ein fehlender Sensor liefert None (Frontend: "n/a"), nie einen Absturz.

use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use nvml_wrapper::enum_wrappers::device::TemperatureSensor;
use nvml_wrapper::Nvml;
use sysinfo::{CpuRefreshKind, Disks, Networks, RefreshKind, System};

/// (MemTotal, MemAvailable) in Bytes aus /proc/meminfo.
fn read_meminfo() -> Option<(u64, u64)> {
    let content = fs::read_to_string("/proc/meminfo").ok()?;
    let mut total = None;
    let mut available = None;
    for line in content.lines() {
        let parse = |rest: &str| -> Option<u64> {
            rest.trim().trim_end_matches("kB").trim().parse::<u64>().ok()
        };
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total = parse(rest).map(|kb| kb * 1024);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available = parse(rest).map(|kb| kb * 1024);
        }
        if total.is_some() && available.is_some() {
            break;
        }
    }
    Some((total?, available?))
}

#[derive(Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    pub cpu: Option<f32>,
    pub ram: Option<f32>,
    /// belegter/gesamter Arbeitsspeicher in Bytes (MemTotal - MemAvailable)
    pub ram_used: Option<u64>,
    pub ram_total: Option<u64>,
    pub disk: Option<f32>,
    pub gpu: Option<f32>,
    pub temp_cpu: Option<f32>,
    pub temp_gpu: Option<f32>,
    /// mb/s, None beim ersten Tick (noch kein Delta)
    pub net_up: Option<f64>,
    pub net_down: Option<f64>,
    pub pwr: Option<f64>,
    pub ip: Option<String>,
    /// GPU-Lüfter in % (NVML, nur lesbar)
    pub gpu_fan: Option<f32>,
    /// Mainboard-Lüfter (hwmon), leer wenn kein Chip geladen ist
    pub fans: Vec<crate::fans::FanStat>,
}

impl Stats {
    /// Signatur der *anzeige-gerundeten* Werte. Ist sie unverändert, würde
    /// das Frontend pixelgleich rendern — der Tick kann übersprungen werden.
    pub fn display_signature(&self) -> String {
        fn r0(v: Option<f32>) -> i64 {
            v.map(|x| x.round() as i64).unwrap_or(i64::MIN)
        }
        fn net(v: Option<f64>) -> i64 {
            // unter 10 eine nachkommastelle, darüber ganzzahlig
            v.map(|x| {
                if x < 10.0 {
                    (x * 10.0).round() as i64
                } else {
                    (x.round() as i64) * 10
                }
            })
            .unwrap_or(i64::MIN)
        }
        // ram_used auf 0.1-gib-auflösung, wie die anzeige
        let ram_used_dgib = self
            .ram_used
            .map(|b| (b as f64 / (1024.0 * 1024.0 * 1024.0) * 10.0).round() as i64)
            .unwrap_or(i64::MIN);
        let mut sig = format!(
            "{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}",
            r0(self.cpu),
            r0(self.ram),
            ram_used_dgib,
            r0(self.disk),
            r0(self.gpu),
            r0(self.temp_cpu),
            r0(self.temp_gpu),
            net(self.net_up),
            net(self.net_down),
            self.pwr.map(|x| x.round() as i64).unwrap_or(i64::MIN),
            self.ip.as_deref().unwrap_or("-"),
            r0(self.gpu_fan),
        );
        for fan in &self.fans {
            sig.push_str(&format!(
                "|{}:{}:{}:{:?}",
                fan.id,
                fan.rpm.map(|r| r as i64).unwrap_or(-1),
                fan.pct.map(|p| p.round() as i64).unwrap_or(-1),
                fan.auto_mode
            ));
        }
        sig
    }
}

pub struct Sampler {
    sys: System,
    disks: Disks,
    net: NetSampler,
    gpu: Option<Nvml>,
    cpu_temp: Option<PathBuf>,
    rapl: Option<Rapl>,
    tick: u64,
    /// letzte langsame GPU-Werte (temp, power, fan) — nur jeden 2. Tick
    /// frisch abgefragt, um NVML-ioctl-Kosten zu halbieren
    gpu_slow: (Option<f32>, Option<f64>, Option<f32>),
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            sys: System::new_with_specifics(
                RefreshKind::nothing().with_cpu(CpuRefreshKind::nothing().with_cpu_usage()),
            ),
            disks: Disks::new_with_refreshed_list(),
            net: NetSampler::new(),
            gpu: Nvml::init().ok(),
            cpu_temp: find_cpu_temp_sensor(),
            rapl: Rapl::new(),
            tick: 0,
            gpu_slow: (None, None, None),
        }
    }

    pub fn sample(&mut self) -> Stats {
        let timing = std::env::var_os("INDIGO_TIMING").is_some();
        let mut t = std::time::Instant::now();
        let mut lap = |name: &str| {
            if timing {
                eprintln!("  {name}: {:?}", t.elapsed());
            }
            t = std::time::Instant::now();
        };
        let mut stats = Stats::default();

        self.sys.refresh_cpu_usage();
        lap("cpu");
        stats.cpu = Some(self.sys.global_cpu_usage());
        // ram direkt aus /proc/meminfo: used = MemTotal - MemAvailable
        if let Some((total, available)) = read_meminfo() {
            let used = total.saturating_sub(available);
            stats.ram = Some(used as f32 / total as f32 * 100.0);
            stats.ram_used = Some(used);
            stats.ram_total = Some(total);
        }

        stats.disk = self.sample_disk();
        lap("disk");
        stats.temp_cpu = self.sample_cpu_temp();
        lap("temp");

        let (net_up, net_down, ip) = self.net.sample();
        stats.net_up = net_up;
        stats.net_down = net_down;
        stats.ip = ip;
        lap("net");

        self.tick = self.tick.wrapping_add(1);
        let refresh_slow = self.tick % 2 == 1;
        let (gpu_util, gpu_temp, gpu_power, gpu_fan) = self.sample_gpu(refresh_slow);
        stats.gpu = gpu_util;
        if refresh_slow {
            self.gpu_slow = (gpu_temp, gpu_power, gpu_fan);
        }
        stats.temp_gpu = self.gpu_slow.0;
        stats.gpu_fan = self.gpu_slow.2;
        let gpu_power = self.gpu_slow.1;
        lap("gpu");

        let rapl_power = self.rapl.as_mut().and_then(Rapl::sample);
        lap("rapl");
        stats.pwr = match (gpu_power, rapl_power) {
            (None, None) => None,
            (g, r) => Some(g.unwrap_or(0.0) + r.unwrap_or(0.0)),
        };

        stats
    }

    fn sample_disk(&mut self) -> Option<f32> {
        self.disks.refresh(true);
        let root = self
            .disks
            .iter()
            .find(|d| d.mount_point() == Path::new("/"))?;
        let total = root.total_space();
        if total == 0 {
            return None;
        }
        let used = total.saturating_sub(root.available_space());
        Some(used as f32 / total as f32 * 100.0)
    }

    fn sample_cpu_temp(&self) -> Option<f32> {
        let raw = fs::read_to_string(self.cpu_temp.as_ref()?).ok()?;
        let millideg: f32 = raw.trim().parse().ok()?;
        Some(millideg / 1000.0)
    }

    fn sample_gpu(
        &self,
        refresh_slow: bool,
    ) -> (Option<f32>, Option<f32>, Option<f64>, Option<f32>) {
        let Some(nvml) = &self.gpu else {
            return (None, None, None, None);
        };
        let Ok(device) = nvml.device_by_index(0) else {
            return (None, None, None, None);
        };
        let util = device.utilization_rates().ok().map(|u| u.gpu as f32);
        if !refresh_slow {
            return (util, None, None, None);
        }
        let temp = device
            .temperature(TemperatureSensor::Gpu)
            .ok()
            .map(|t| t as f32);
        let power = device.power_usage().ok().map(|mw| mw as f64 / 1000.0);
        let fan = device.fan_speed(0).ok().map(|p| p as f32);
        (util, temp, power, fan)
    }
}

/// Netz-Durchsatz als Delta der Zähler des Default-Route-Interfaces,
/// dazu dessen IPv4-Adresse.
struct NetSampler {
    networks: Networks,
    last: Instant,
    primed: bool,
}

impl NetSampler {
    fn new() -> Self {
        Self {
            networks: Networks::new_with_refreshed_list(),
            last: Instant::now(),
            primed: false,
        }
    }

    fn sample(&mut self) -> (Option<f64>, Option<f64>, Option<String>) {
        self.networks.refresh(true);
        let dt = self.last.elapsed().as_secs_f64();
        self.last = Instant::now();

        let Some(iface) = default_route_interface() else {
            return (None, None, None);
        };
        let Some(data) = self
            .networks
            .iter()
            .find(|(name, _)| **name == iface)
            .map(|(_, data)| data)
        else {
            return (None, None, None);
        };

        let ip = data
            .ip_networks()
            .iter()
            .find(|net| net.addr.is_ipv4())
            .map(|net| net.addr.to_string());

        if !self.primed || dt <= 0.0 {
            self.primed = true;
            return (None, None, ip);
        }

        let up = data.transmitted() as f64 / dt / 1_000_000.0;
        let down = data.received() as f64 / dt / 1_000_000.0;
        (Some(up), Some(down), ip)
    }
}

/// Interface der Default-Route aus /proc/net/route (Destination 00000000).
fn default_route_interface() -> Option<String> {
    let content = fs::read_to_string("/proc/net/route").ok()?;
    for line in content.lines().skip(1) {
        let mut cols = line.split_whitespace();
        let iface = cols.next()?;
        if cols.next() == Some("00000000") {
            return Some(iface.to_string());
        }
    }
    None
}

/// CPU-Temperatursensor unter /sys/class/hwmon suchen. Bevorzugt bekannte
/// CPU-Chips und deren Package-/Tctl-Kanal, sonst deren temp1_input.
fn find_cpu_temp_sensor() -> Option<PathBuf> {
    const CHIPS: [&str; 4] = ["coretemp", "k10temp", "zenpower", "cpu_thermal"];
    const LABELS: [&str; 3] = ["Package id 0", "Tctl", "Tdie"];

    let hwmons = fs::read_dir("/sys/class/hwmon").ok()?;
    for entry in hwmons.flatten() {
        let dir = entry.path();
        let Ok(name) = fs::read_to_string(dir.join("name")) else {
            continue;
        };
        if !CHIPS.contains(&name.trim()) {
            continue;
        }
        // passenden Kanal über das Label suchen
        if let Ok(files) = fs::read_dir(&dir) {
            for file in files.flatten() {
                let fname = file.file_name().to_string_lossy().into_owned();
                let Some(stem) = fname.strip_suffix("_label") else {
                    continue;
                };
                let Ok(label) = fs::read_to_string(file.path()) else {
                    continue;
                };
                if LABELS.contains(&label.trim()) {
                    let input = dir.join(format!("{stem}_input"));
                    if input.exists() {
                        return Some(input);
                    }
                }
            }
        }
        let fallback = dir.join("temp1_input");
        if fallback.exists() {
            return Some(fallback);
        }
    }
    None
}

/// Package-Leistung über RAPL (energy_uj-Delta). Auf vielen Systemen ist
/// energy_uj nur für root lesbar — dann bleibt rapl None.
struct Rapl {
    packages: Vec<RaplPackage>,
    last: Instant,
}

struct RaplPackage {
    energy_path: PathBuf,
    max_range_uj: u64,
    last_uj: u64,
}

impl Rapl {
    fn new() -> Option<Self> {
        let entries = fs::read_dir("/sys/class/powercap").ok()?;
        let mut packages = Vec::new();
        for entry in entries.flatten() {
            let dir = entry.path();
            let dirname = entry.file_name().to_string_lossy().into_owned();
            // nur Top-Level-Domains (intel-rapl:0), keine Subzonen (intel-rapl:0:0)
            if !dirname.starts_with("intel-rapl:") || dirname.matches(':').count() != 1 {
                continue;
            }
            let Ok(name) = fs::read_to_string(dir.join("name")) else {
                continue;
            };
            if !name.trim().starts_with("package") {
                continue;
            }
            let energy_path = dir.join("energy_uj");
            let Some(last_uj) = read_u64(&energy_path) else {
                continue; // nicht lesbar (Rechte) -> Paket auslassen
            };
            let max_range_uj =
                read_u64(&dir.join("max_energy_range_uj")).unwrap_or(u64::MAX);
            packages.push(RaplPackage {
                energy_path,
                max_range_uj,
                last_uj,
            });
        }
        if packages.is_empty() {
            return None;
        }
        Some(Self {
            packages,
            last: Instant::now(),
        })
    }

    fn sample(&mut self) -> Option<f64> {
        let dt = self.last.elapsed().as_secs_f64();
        self.last = Instant::now();
        if dt <= 0.0 {
            return None;
        }
        let mut total_uj: u64 = 0;
        let mut any = false;
        for pkg in &mut self.packages {
            let Some(now_uj) = read_u64(&pkg.energy_path) else {
                continue;
            };
            let delta = if now_uj >= pkg.last_uj {
                now_uj - pkg.last_uj
            } else {
                // Zähler-Überlauf
                pkg.max_range_uj.saturating_sub(pkg.last_uj) + now_uj
            };
            pkg.last_uj = now_uj;
            total_uj += delta;
            any = true;
        }
        if !any {
            return None;
        }
        Some(total_uj as f64 / dt / 1_000_000.0)
    }
}

fn read_u64(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}
