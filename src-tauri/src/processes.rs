//! Top-Prozesse nach CPU, RAM oder GPU. Wird nur auf Anfrage gerechnet
//! (Dropdown offen), damit der Leerlaufverbrauch des Widgets klein bleibt.
//!
//! RAM wird bewusst ohne sysinfo erhoben: Iteration ausschliesslich über
//! /proc/[pid] (nie task/ — Threads teilen sich den Adressraum und würden
//! denselben Speicher mehrfach zählen), Wert ist Pss aus smaps_rollup
//! (anteilig für geteilte Seiten), Fallback VmRSS aus status.

use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use nvml_wrapper::enums::device::UsedGpuMemory;
use nvml_wrapper::Nvml;
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TopEntry {
    pub name: String,
    pub value: f64,
    /// "pct" oder "bytes" — bestimmt die Formatierung im Frontend
    pub unit: &'static str,
    /// Anzahl zusammengefasster Prozesse (nur bei ram gesetzt)
    pub count: Option<u32>,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TopList {
    pub entries: Vec<TopEntry>,
    /// gesetzt, wenn die Messung den Sanity-Check verletzt
    pub warning: Option<String>,
}

enum NvmlState {
    Untried,
    Failed,
    Ready(Nvml),
}

pub struct TopProcs {
    sys: Mutex<(System, Instant)>,
    nvml: Mutex<NvmlState>,
}

impl TopProcs {
    pub fn new() -> Self {
        Self {
            sys: Mutex::new((System::new(), Instant::now() - Duration::from_secs(60))),
            nvml: Mutex::new(NvmlState::Untried),
        }
    }

    pub fn top(&self, kind: &str) -> Result<TopList, String> {
        match kind {
            "cpu" => Ok(TopList {
                entries: self.top_cpu(),
                warning: None,
            }),
            "ram" => Ok(top_ram()),
            "gpu" => Ok(TopList {
                entries: self.top_gpu(),
                warning: None,
            }),
            other => Err(format!("unbekannte art: {other}")),
        }
    }

    /// CPU über sysinfo (Prozent-Deltas), Threads ausgefiltert und
    /// nach Name aggregiert (ein Browser = viele Prozesse).
    fn top_cpu(&self) -> Vec<TopEntry> {
        let mut guard = self.sys.lock().unwrap();
        let (sys, last) = &mut *guard;
        let kind = ProcessRefreshKind::nothing().with_cpu();
        sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        // CPU-Prozent ist ein Delta: nach längerer Pause ist der erste
        // Messwert leer, darum kurz warten und noch einmal lesen
        if last.elapsed() > Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(220));
            sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
        }
        *last = Instant::now();

        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1) as f64;

        let mut agg: HashMap<String, f64> = HashMap::new();
        for proc in sys.processes().values() {
            // Tasks (Threads) überspringen: ihre CPU-Zeit steckt bereits
            // in der Prozesssumme und ihre comm-Namen sind keine Programme
            if proc.thread_kind().is_some() {
                continue;
            }
            let name = proc.name().to_string_lossy().into_owned();
            *agg.entry(name).or_default() += proc.cpu_usage() as f64;
        }

        let mut list: Vec<TopEntry> = agg
            .into_iter()
            .map(|(name, cpu)| TopEntry {
                name,
                value: cpu / cores, // % der gesamtkapazität, wie die cpu-zeile
                unit: "pct",
                count: None,
            })
            .collect();
        list.sort_by(|a, b| b.value.total_cmp(&a.value));
        list.truncate(10);
        list
    }

    /// GPU: Prozesse nach belegtem GPU-Speicher (stabilster Messwert über NVML).
    fn top_gpu(&self) -> Vec<TopEntry> {
        let mut guard = self.nvml.lock().unwrap();
        if matches!(*guard, NvmlState::Untried) {
            *guard = match Nvml::init() {
                Ok(n) => NvmlState::Ready(n),
                Err(_) => NvmlState::Failed,
            };
        }
        let NvmlState::Ready(nvml) = &*guard else {
            return Vec::new();
        };
        let Ok(device) = nvml.device_by_index(0) else {
            return Vec::new();
        };

        let mut by_pid: HashMap<u32, u64> = HashMap::new();
        let procs = device
            .running_graphics_processes()
            .into_iter()
            .flatten()
            .chain(device.running_compute_processes().into_iter().flatten());
        for p in procs {
            let bytes = match p.used_gpu_memory {
                UsedGpuMemory::Used(b) => b,
                UsedGpuMemory::Unavailable => 0,
            };
            by_pid.insert(p.pid, bytes); // insert dedupliziert grafik+compute
        }

        let mut agg: HashMap<String, u64> = HashMap::new();
        for (pid, bytes) in by_pid {
            let name = process_name(Path::new(&format!("/proc/{pid}")), &pid.to_string());
            *agg.entry(name).or_default() += bytes;
        }

        let mut list: Vec<TopEntry> = agg
            .into_iter()
            .map(|(name, bytes)| TopEntry {
                name,
                value: bytes as f64,
                unit: "bytes",
                count: None,
            })
            .collect();
        list.sort_by(|a, b| b.value.total_cmp(&a.value));
        list.truncate(10);
        list
    }
}

/// RAM: eigener /proc-Scan, aggregiert nach Prozessname.
fn top_ram() -> TopList {
    let mut agg: HashMap<String, (u64, u32)> = HashMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return TopList {
            entries: Vec::new(),
            warning: Some("kein zugriff auf /proc".into()),
        };
    };
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let Some(pid) = fname.to_str() else { continue };
        if !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let base = entry.path();
        // Pss (anteilig) bevorzugt, sonst VmRSS; Kernel-Threads haben
        // keins von beidem und fallen so automatisch raus. Prozesse, die
        // während des Scans verschwinden, werden still übersprungen.
        let Some(bytes) = read_pss(&base).or_else(|| read_vmrss(&base)) else {
            continue;
        };
        if bytes == 0 {
            continue;
        }
        let name = process_name(&base, pid);
        let slot = agg.entry(name).or_default();
        slot.0 += bytes;
        slot.1 += 1;
    }

    let total_bytes: u64 = agg.values().map(|(b, _)| *b).sum();
    let mem_total = read_meminfo_total().unwrap_or(u64::MAX);
    let warning = if total_bytes > mem_total {
        let msg = format!(
            "messung inkonsistent: prozesse {:.1} gb > ram {:.1} gb",
            total_bytes as f64 / GIB,
            mem_total as f64 / GIB
        );
        eprintln!("indigo: {msg}");
        Some(msg)
    } else {
        None
    };

    let mut list: Vec<TopEntry> = agg
        .into_iter()
        .map(|(name, (bytes, count))| TopEntry {
            name,
            value: bytes as f64,
            unit: "bytes",
            count: Some(count),
        })
        .collect();
    list.sort_by(|a, b| b.value.total_cmp(&a.value));
    list.truncate(10);
    TopList {
        entries: list,
        warning,
    }
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Summe der Pss-Zeilen aus smaps_rollup, in Bytes.
fn read_pss(base: &Path) -> Option<u64> {
    let content = fs::read_to_string(base.join("smaps_rollup")).ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("Pss:") {
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

/// VmRSS aus /proc/[pid]/status, in Bytes.
fn read_vmrss(base: &Path) -> Option<u64> {
    let content = fs::read_to_string(base.join("status")).ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

fn read_meminfo_total() -> Option<u64> {
    let content = fs::read_to_string("/proc/meminfo").ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: u64 = rest.trim().trim_end_matches("kB").trim().parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

/// Prozessname aus comm, bei leerem Wert Basename des ersten
/// cmdline-Arguments, sonst "pid N".
fn process_name(base: &Path, pid: &str) -> String {
    if let Ok(comm) = fs::read_to_string(base.join("comm")) {
        let comm = comm.trim();
        if !comm.is_empty() {
            return comm.to_string();
        }
    }
    if let Ok(cmdline) = fs::read(base.join("cmdline")) {
        if let Some(first) = cmdline.split(|b| *b == 0).next() {
            if !first.is_empty() {
                let arg = String::from_utf8_lossy(first);
                if let Some(basename) = arg.rsplit('/').next() {
                    if !basename.is_empty() {
                        return basename.to_string();
                    }
                }
            }
        }
    }
    format!("pid {pid}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_liste_ist_plausibel() {
        let result = top_ram();
        assert!(result.warning.is_none(), "sanity-check verletzt: {:?}", result.warning);
        assert!(!result.entries.is_empty());
        let thread_namen = [
            "ThreadPoolForeg",
            "V8Worker",
            "tokio-rt-worker",
            "Chrome_ChildIOT",
            "DedicatedWorker",
            "PerfettoTrace",
        ];
        for entry in &result.entries {
            assert!(
                !thread_namen.contains(&entry.name.as_str()),
                "thread-name in liste: {}",
                entry.name
            );
        }
        let sum: f64 = result.entries.iter().map(|e| e.value).sum();
        let total = read_meminfo_total().unwrap() as f64;
        assert!(sum < total, "top-10-summe {sum} >= memtotal {total}");
        for entry in &result.entries {
            println!(
                "{:20} {:8.1} mb ({})",
                entry.name,
                entry.value / 1024.0 / 1024.0,
                entry.count.unwrap_or(0)
            );
        }
    }
}
