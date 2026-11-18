//! Generic Linux hardware probe: /proc, /sys, statvfs, and nvidia-smi / lspci
//! when available. Nothing here requires root.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const GB: f64 = 1024.0 * 1024.0 * 1024.0;

#[derive(Clone, Debug)]
pub struct Gpu {
    pub vendor: String,
    pub name: String,
    /// Dedicated VRAM in bytes; `None` for integrated GPUs (shared memory).
    pub vram: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct SystemInfo {
    pub os: String,
    pub kernel: String,
    pub cpu: String,
    pub threads: usize,
    pub ram_total: u64,
    pub ram_available: u64,
    pub gpus: Vec<Gpu>,
    pub models_dir: PathBuf,
    pub disk_free: u64,
    pub disk_total: u64,
}

impl SystemInfo {
    pub fn probe() -> Self {
        let (ram_total, ram_available) = meminfo();
        let models_dir = models_dir();
        let (disk_free, disk_total) = disk_space(&models_dir);
        Self {
            os: os_release(),
            kernel: read_trim("/proc/sys/kernel/osrelease"),
            cpu: cpu_model(),
            threads: std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
            ram_total,
            ram_available,
            gpus: gpus(),
            models_dir,
            disk_free,
            disk_total,
        }
    }

    /// Total dedicated VRAM across GPUs (Ollama can split a model across them).
    pub fn vram_total(&self) -> u64 {
        self.gpus.iter().filter_map(|g| g.vram).sum()
    }
}

fn read_trim(path: &str) -> String {
    fs::read_to_string(path).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn os_release() -> String {
    let text = fs::read_to_string("/etc/os-release").unwrap_or_default();
    text.lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim_matches('"').to_string())
        .unwrap_or_else(|| "Linux".into())
}

fn cpu_model() -> String {
    let text = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    text.lines()
        .find(|l| l.starts_with("model name") || l.starts_with("Model")) // x86 / ARM
        .and_then(|l| l.split_once(':'))
        .map(|(_, v)| v.trim().to_string())
        .unwrap_or_else(|| "Unknown CPU".into())
}

/// (total, available) in bytes from /proc/meminfo.
fn meminfo() -> (u64, u64) {
    let text = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let get = |key: &str| {
        text.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    (get("MemTotal:"), get("MemAvailable:"))
}

/// Where Ollama stores models: $OLLAMA_MODELS, then the per-user directory,
/// then the one used by the official systemd service (`ollama` user).
fn models_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("OLLAMA_MODELS").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into());
    let user = home.join(".ollama/models");
    let service = PathBuf::from("/usr/share/ollama/.ollama/models");
    if !user.exists() && service.exists() {
        return service;
    }
    user
}

/// (free, total) bytes on the filesystem holding `path` (or its nearest existing parent).
fn disk_space(path: &Path) -> (u64, u64) {
    let mut p = path.to_path_buf();
    while !p.exists() {
        if !p.pop() {
            return (0, 0);
        }
    }
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()) else {
        return (0, 0);
    };
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return (0, 0);
    }
    let frsize = st.f_frsize as u64;
    (st.f_bavail as u64 * frsize, st.f_blocks as u64 * frsize)
}

fn gpus() -> Vec<Gpu> {
    let mut out = Vec::new();

    // NVIDIA: the proprietary driver does not expose VRAM in sysfs, ask nvidia-smi.
    let nvidia_ok = Command::new("nvidia-smi")
        .args(["--query-gpu=name,memory.total", "--format=csv,noheader,nounits"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            for line in String::from_utf8_lossy(&o.stdout).lines() {
                if let Some((name, mib)) = line.rsplit_once(',') {
                    out.push(Gpu {
                        vendor: "NVIDIA".into(),
                        name: name.trim().to_string(),
                        vram: mib.trim().parse::<u64>().ok().map(|m| m * 1024 * 1024),
                    });
                }
            }
        })
        .is_some();

    // Everything else (and NVIDIA without nvidia-smi): walk /sys/class/drm.
    let Ok(entries) = fs::read_dir("/sys/class/drm") else { return out };
    let mut cards: Vec<_> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("card") && n[4..].chars().all(|c| c.is_ascii_digit()))
        .collect();
    cards.sort();
    for card in cards {
        let dev = format!("/sys/class/drm/{card}/device");
        let vendor_id = read_trim(&format!("{dev}/vendor"));
        let vendor = match vendor_id.as_str() {
            "0x10de" if nvidia_ok => continue,
            "0x10de" => "NVIDIA",
            "0x1002" => "AMD",
            "0x8086" => "Intel",
            "" => continue,
            _ => "Other",
        };
        // amdgpu exposes dedicated VRAM; tiny values mean an APU carve-out
        let vram = read_trim(&format!("{dev}/mem_info_vram_total"))
            .parse::<u64>()
            .ok()
            .filter(|&b| b >= 1024 * 1024 * 1024);
        let name = pci_name(&dev).unwrap_or_else(|| format!("{vendor} GPU"));
        out.push(Gpu { vendor: vendor.into(), name, vram });
    }
    out
}

/// Human-readable device name via lspci, if installed.
fn pci_name(dev: &str) -> Option<String> {
    let uevent = fs::read_to_string(format!("{dev}/uevent")).ok()?;
    let slot = uevent.lines().find_map(|l| l.strip_prefix("PCI_SLOT_NAME="))?;
    let out = Command::new("lspci").args(["-mm", "-s", slot]).output().ok()?;
    // format: slot "class" "vendor" "device" ...
    let line = String::from_utf8_lossy(&out.stdout).to_string();
    let parts: Vec<&str> = line.split('"').collect();
    let (vendor, device) = (parts.get(3)?, parts.get(5)?);
    Some(format!("{vendor} {device}"))
}

/// How well a model of `size` bytes fits this machine.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fit {
    /// Fits entirely in GPU memory: fast.
    Gpu,
    /// Fits in system RAM (maybe partially offloaded): works, slower.
    Cpu,
    /// Not enough memory.
    TooBig,
    /// Not enough free disk space to download it.
    NoDisk,
}

impl Fit {
    pub fn assess(size: u64, sys: &SystemInfo) -> Fit {
        // weights + KV cache / runtime overhead
        let need = size as f64 * 1.2 + 0.5 * GB;
        if sys.disk_free > 0 && (sys.disk_free as f64) < size as f64 * 1.05 {
            return Fit::NoDisk;
        }
        if sys.vram_total() as f64 >= need {
            return Fit::Gpu;
        }
        // keep ~2 GB for the OS and desktop
        if sys.ram_total as f64 + sys.vram_total() as f64 >= need + 2.0 * GB {
            return Fit::Cpu;
        }
        Fit::TooBig
    }

    pub fn label(self) -> &'static str {
        match self {
            Fit::Gpu => "Recommended · fits in GPU",
            Fit::Cpu => "OK · runs on CPU, slower",
            Fit::TooBig => "Not recommended · not enough memory",
            Fit::NoDisk => "Not enough disk space",
        }
    }
}

pub fn fmt_bytes(b: u64) -> String {
    let g = b as f64 / GB;
    if g >= 1.0 { format!("{g:.1} GB") } else { format!("{:.0} MB", b as f64 / 1024.0 / 1024.0) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_basic_hardware() {
        let s = SystemInfo::probe();
        println!("{s:#?}");
        assert!(s.ram_total > 0);
        assert!(s.threads > 0);
    }

    #[test]
    fn fit_thresholds() {
        let mut s = SystemInfo { ram_total: 16 * GB as u64, disk_free: 100 * GB as u64, ..Default::default() };
        assert_eq!(Fit::assess(4 * GB as u64, &s), Fit::Cpu);
        assert_eq!(Fit::assess(40 * GB as u64, &s), Fit::TooBig);
        s.gpus.push(Gpu { vendor: "NVIDIA".into(), name: "test".into(), vram: Some(12 * GB as u64) });
        assert_eq!(Fit::assess(4 * GB as u64, &s), Fit::Gpu);
        s.disk_free = GB as u64;
        assert_eq!(Fit::assess(4 * GB as u64, &s), Fit::NoDisk);
    }
}
