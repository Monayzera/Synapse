use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HwTier {
    Weak,
    Modest,
    Capable,
}

#[derive(Debug, Clone, Serialize)]
pub struct HardwareInfo {
    pub total_ram_mb: u64,
    pub logical_cores: u32,
    pub build_gpu: bool,
    pub os: String,
    pub tier: HwTier,
}

fn logical_cores() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4)
}

fn build_gpu() -> bool {
    cfg!(feature = "cuda") || cfg!(feature = "metal")
}

#[cfg(windows)]
fn total_ram_mb() -> u64 {
    use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe {
        if GlobalMemoryStatusEx(&mut status).is_ok() {
            status.ullTotalPhys / (1024 * 1024)
        } else {
            0
        }
    }
}

#[cfg(target_os = "macos")]
fn total_ram_mb() -> u64 {
    std::process::Command::new("sysctl")
        .arg("-n")
        .arg("hw.memsize")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
        .map(|bytes| bytes / (1024 * 1024))
        .unwrap_or(0)
}

#[cfg(not(any(windows, target_os = "macos")))]
fn total_ram_mb() -> u64 {
    0
}

fn derive_tier(ram_mb: u64, cores: u32, build_gpu: bool) -> HwTier {
    if build_gpu {
        return HwTier::Capable;
    }
    if ram_mb == 0 {
        return if cores <= 4 { HwTier::Weak } else { HwTier::Modest };
    }
    if ram_mb <= 8192 {
        HwTier::Weak
    } else if ram_mb <= 16384 || cores <= 4 {
        HwTier::Modest
    } else {
        HwTier::Capable
    }
}

pub fn detect() -> HardwareInfo {
    let total_ram_mb = total_ram_mb();
    let logical_cores = logical_cores();
    let gpu = build_gpu();
    let tier = derive_tier(total_ram_mb, logical_cores, gpu);
    HardwareInfo {
        total_ram_mb,
        logical_cores,
        build_gpu: gpu,
        os: std::env::consts::OS.to_string(),
        tier,
    }
}

#[tauri::command]
pub fn hardware_info() -> HardwareInfo {
    detect()
}
