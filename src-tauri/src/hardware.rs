use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

#[cfg(windows)]
const VENDOR_NVIDIA: u32 = 0x10DE;
#[cfg(windows)]
const VENDOR_AMD: u32 = 0x1002;
#[cfg(windows)]
const VENDOR_INTEL: u32 = 0x8086;
#[cfg(windows)]
const VENDOR_MICROSOFT: u32 = 0x1414;
#[cfg(windows)]
const MAX_ADAPTERS: u32 = 16;
#[cfg(windows)]
const NVIDIA_SMI_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

static GPU_CACHE: Mutex<Option<Option<GpuInfo>>> = Mutex::new(None);
static GPU_DETECT: Mutex<()> = Mutex::new(());

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
    pub gpu: Option<GpuInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuInfo {
    pub vendor: GpuVendor,
    pub name: String,
    pub compute_cap: Option<String>,
    pub driver_version: Option<String>,
}

impl GpuInfo {
    pub fn cc(&self) -> Option<(u32, u32)> {
        self.compute_cap.as_deref().and_then(parse_compute_cap)
    }

    pub fn driver_major(&self) -> Option<u32> {
        self.driver_version.as_deref().and_then(parse_driver_major)
    }

    pub fn fingerprint(&self) -> String {
        format!(
            "{:?}|{}|{}",
            self.vendor,
            self.name.trim(),
            self.driver_version.as_deref().unwrap_or("").trim()
        )
    }
}

struct CudaArch {
    major: u32,
    minor: u32,
    exact: bool,
}

const WHISPER_CUDA_ARCHS: [CudaArch; 5] = [
    CudaArch { major: 6, minor: 1, exact: false },
    CudaArch { major: 7, minor: 5, exact: false },
    CudaArch { major: 8, minor: 6, exact: false },
    CudaArch { major: 8, minor: 9, exact: false },
    CudaArch { major: 12, minor: 0, exact: true },
];

fn arch_covers(archs: &[CudaArch], cc: (u32, u32)) -> bool {
    archs.iter().any(|arch| {
        if arch.exact {
            cc == (arch.major, arch.minor)
        } else {
            cc.0 == arch.major && cc.1 >= arch.minor
        }
    })
}

pub fn whisper_cuda_supported(cc: (u32, u32)) -> bool {
    arch_covers(&WHISPER_CUDA_ARCHS, cc)
}

pub fn parse_compute_cap(text: &str) -> Option<(u32, u32)> {
    let (major, minor) = text.trim().split_once('.')?;
    Some((major.trim().parse().ok()?, minor.trim().parse().ok()?))
}

pub fn parse_driver_major(text: &str) -> Option<u32> {
    let text = text.trim();
    let major = text.split('.').next()?.trim();
    if major.is_empty() || !major.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    major.parse().ok()
}

#[cfg(any(windows, test))]
fn smi_value(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() || value.starts_with('[') || value.eq_ignore_ascii_case("n/a") {
        None
    } else {
        Some(value.to_string())
    }
}

#[cfg(any(windows, test))]
fn parse_nvidia_smi(text: &str, with_cc: bool) -> Vec<GpuInfo> {
    let fields = if with_cc { 3 } else { 2 };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts: Vec<&str> = line.rsplitn(fields, ',').collect();
        if parts.len() != fields {
            continue;
        }
        parts.reverse();
        let name = parts[0].trim();
        if name.is_empty() {
            continue;
        }
        let compute_cap = if with_cc {
            smi_value(parts[1]).filter(|cc| parse_compute_cap(cc).is_some())
        } else {
            None
        };
        let driver_version = smi_value(parts[fields - 1]).filter(|v| parse_driver_major(v).is_some());
        out.push(GpuInfo {
            vendor: GpuVendor::Nvidia,
            name: name.to_string(),
            compute_cap,
            driver_version,
        });
    }
    out
}

#[cfg(any(windows, test))]
fn pick_nvidia(rows: &[GpuInfo]) -> Option<&GpuInfo> {
    rows.iter().min_by_key(|row| row.cc().unwrap_or((0, 0)))
}

#[cfg(windows)]
struct Adapter {
    vendor: GpuVendor,
    name: String,
    memory: u64,
}

#[cfg(windows)]
fn vendor_from_id(id: u32) -> GpuVendor {
    match id {
        VENDOR_NVIDIA => GpuVendor::Nvidia,
        VENDOR_AMD => GpuVendor::Amd,
        VENDOR_INTEL => GpuVendor::Intel,
        _ => GpuVendor::Other,
    }
}

#[cfg(windows)]
fn vendor_rank(vendor: GpuVendor) -> u8 {
    match vendor {
        GpuVendor::Nvidia => 3,
        GpuVendor::Amd => 2,
        GpuVendor::Intel => 1,
        GpuVendor::Other => 0,
    }
}

#[cfg(windows)]
fn pick_adapter(adapters: &[Adapter]) -> Option<&Adapter> {
    adapters
        .iter()
        .max_by_key(|adapter| (vendor_rank(adapter.vendor), adapter.memory))
}

#[cfg(windows)]
fn dxgi_adapters() -> Vec<Adapter> {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
    };

    let factory = match unsafe { CreateDXGIFactory1::<IDXGIFactory1>() } {
        Ok(factory) => factory,
        Err(err) => {
            tracing::warn!("graphics adapters could not be listed: {err}");
            return Vec::new();
        }
    };
    let mut adapters = Vec::new();
    for index in 0..MAX_ADAPTERS {
        let adapter = match unsafe { factory.EnumAdapters1(index) } {
            Ok(adapter) => adapter,
            Err(_) => break,
        };
        let desc = match unsafe { adapter.GetDesc1() } {
            Ok(desc) => desc,
            Err(err) => {
                tracing::debug!("graphics adapter {index} description unavailable: {err}");
                continue;
            }
        };
        let software = desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0;
        if software || desc.VendorId == VENDOR_MICROSOFT {
            continue;
        }
        let len = desc
            .Description
            .iter()
            .position(|c| *c == 0)
            .unwrap_or(desc.Description.len());
        let name = String::from_utf16_lossy(&desc.Description[..len])
            .trim()
            .to_string();
        adapters.push(Adapter {
            vendor: vendor_from_id(desc.VendorId),
            name,
            memory: desc.DedicatedVideoMemory as u64,
        });
    }
    adapters
}

#[cfg(windows)]
fn nvidia_smi_paths() -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    if let Some(root) = std::env::var_os("SystemRoot") {
        paths.push(std::path::PathBuf::from(root).join("System32").join("nvidia-smi.exe"));
    }
    if let Some(program_files) = std::env::var_os("ProgramFiles") {
        paths.push(
            std::path::PathBuf::from(program_files)
                .join("NVIDIA Corporation")
                .join("NVSMI")
                .join("nvidia-smi.exe"),
        );
    }
    let mut found: Vec<std::path::PathBuf> = paths.into_iter().filter(|p| p.is_file()).collect();
    if found.is_empty() {
        found.push(std::path::PathBuf::from("nvidia-smi"));
    }
    found
}

#[cfg(windows)]
fn nvidia_smi() -> Vec<GpuInfo> {
    for exe in nvidia_smi_paths() {
        for (query, with_cc) in [
            ("name,compute_cap,driver_version", true),
            ("name,driver_version", false),
        ] {
            let mut command = std::process::Command::new(&exe);
            command
                .arg(format!("--query-gpu={query}"))
                .arg("--format=csv,noheader");
            match crate::sidecar::run_capture(command, NVIDIA_SMI_TIMEOUT) {
                Ok(text) => {
                    let rows = parse_nvidia_smi(&text, with_cc);
                    if !rows.is_empty() {
                        return rows;
                    }
                    tracing::info!("nvidia-smi ({}) returned no usable rows for {query}", exe.display());
                }
                Err(err) => {
                    tracing::info!("nvidia-smi ({}) query {query} failed: {err}", exe.display());
                }
            }
        }
    }
    Vec::new()
}

#[cfg(windows)]
fn detect_gpu() -> Option<GpuInfo> {
    let adapters = dxgi_adapters();
    let best = pick_adapter(&adapters)?;
    let mut info = GpuInfo {
        vendor: best.vendor,
        name: best.name.clone(),
        compute_cap: None,
        driver_version: None,
    };
    if best.vendor == GpuVendor::Nvidia {
        let rows = nvidia_smi();
        if let Some(row) = pick_nvidia(&rows) {
            info = row.clone();
        }
    }
    Some(info)
}

#[cfg(not(windows))]
fn detect_gpu() -> Option<GpuInfo> {
    None
}

fn log_gpu(gpu: Option<&GpuInfo>) {
    match gpu {
        Some(gpu) => {
            let whisper = match gpu.cc() {
                Some(cc) if whisper_cuda_supported(cc) => "covered",
                Some(_) => "not covered",
                None => "unknown",
            };
            tracing::info!(
                "graphics card: {} ({:?}, compute capability {}, driver {}, whisper CUDA kernels {whisper})",
                gpu.name,
                gpu.vendor,
                gpu.compute_cap.as_deref().unwrap_or("unknown"),
                gpu.driver_version.as_deref().unwrap_or("unknown")
            );
        }
        None => tracing::info!("no hardware graphics card detected"),
    }
}

pub fn cached_gpu() -> Option<GpuInfo> {
    GPU_CACHE.lock().clone().flatten()
}

fn gpu_blocking(refresh: bool) -> Option<GpuInfo> {
    let _serial = GPU_DETECT.lock();
    if !refresh {
        if let Some(cached) = GPU_CACHE.lock().clone() {
            return cached;
        }
    }
    let detected = detect_gpu();
    log_gpu(detected.as_ref());
    *GPU_CACHE.lock() = Some(detected.clone());
    detected
}

pub async fn gpu(refresh: bool) -> Option<GpuInfo> {
    match tokio::task::spawn_blocking(move || gpu_blocking(refresh)).await {
        Ok(gpu) => gpu,
        Err(err) => {
            tracing::warn!("graphics card detection failed: {err}");
            cached_gpu()
        }
    }
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

pub fn detect(card: Option<GpuInfo>) -> HardwareInfo {
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
        gpu: card,
    }
}

#[tauri::command]
pub async fn hardware_info() -> HardwareInfo {
    match tokio::task::spawn_blocking(|| detect(gpu_blocking(false))).await {
        Ok(info) => info,
        Err(err) => {
            tracing::warn!("hardware detection failed: {err}");
            detect(cached_gpu())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvidia_smi_csv_is_parsed() {
        let rows = parse_nvidia_smi("NVIDIA GeForce RTX 5070, 12.0, 610.88\r\n", true);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].vendor, GpuVendor::Nvidia);
        assert_eq!(rows[0].name, "NVIDIA GeForce RTX 5070");
        assert_eq!(rows[0].compute_cap.as_deref(), Some("12.0"));
        assert_eq!(rows[0].driver_version.as_deref(), Some("610.88"));
        assert_eq!(rows[0].cc(), Some((12, 0)));
        assert_eq!(rows[0].driver_major(), Some(610));
    }

    #[test]
    fn nvidia_smi_handles_missing_fields_and_noise() {
        let text = "\nQuadro, Special K1, [N/A], 470.14\n\nbroken line\nTesla P4, 6.1, [Not Supported]\n";
        let rows = parse_nvidia_smi(text, true);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "Quadro, Special K1");
        assert_eq!(rows[0].compute_cap, None);
        assert_eq!(rows[0].driver_major(), Some(470));
        assert_eq!(rows[1].name, "Tesla P4");
        assert_eq!(rows[1].cc(), Some((6, 1)));
        assert_eq!(rows[1].driver_version, None);
        let fallback = parse_nvidia_smi("NVIDIA GeForce GTX 1060 6GB, 566.36\n", false);
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].compute_cap, None);
        assert_eq!(fallback[0].driver_major(), Some(566));
        assert!(parse_nvidia_smi("NVIDIA-SMI has failed because it couldn't communicate with the NVIDIA driver.", true).is_empty());
    }

    #[test]
    fn nvidia_pick_prefers_lowest_compute_capability() {
        let rows = parse_nvidia_smi(
            "NVIDIA GeForce RTX 5070, 12.0, 610.88\nNVIDIA GeForce GTX 1080, 6.1, 610.88\n",
            true,
        );
        assert_eq!(pick_nvidia(&rows).map(|r| r.name.as_str()), Some("NVIDIA GeForce GTX 1080"));
        let unknown = parse_nvidia_smi("NVIDIA GeForce RTX 4090, 8.9, 580.10\nOld Card, [N/A], 580.10\n", true);
        assert_eq!(pick_nvidia(&unknown).map(|r| r.name.as_str()), Some("Old Card"));
        assert!(pick_nvidia(&[]).is_none());
    }

    #[test]
    fn compute_capability_and_driver_parsing() {
        assert_eq!(parse_compute_cap("8.6"), Some((8, 6)));
        assert_eq!(parse_compute_cap(" 12.0 "), Some((12, 0)));
        assert_eq!(parse_compute_cap("12"), None);
        assert_eq!(parse_compute_cap("[N/A]"), None);
        assert_eq!(parse_driver_major("610.88"), Some(610));
        assert_eq!(parse_driver_major("551.61"), Some(551));
        assert_eq!(parse_driver_major("[N/A]"), None);
        assert_eq!(parse_driver_major(""), None);
    }

    #[test]
    fn whisper_cuda_table_covers_exactly_the_compiled_archs() {
        for cc in [(6, 1), (6, 2), (7, 5), (7, 6), (8, 6), (8, 7), (8, 9), (12, 0)] {
            assert!(whisper_cuda_supported(cc), "{cc:?} should be covered");
        }
        for cc in [
            (5, 0),
            (5, 2),
            (6, 0),
            (7, 0),
            (7, 2),
            (7, 4),
            (8, 0),
            (8, 5),
            (9, 0),
            (10, 0),
            (10, 1),
            (11, 0),
            (12, 1),
            (12, 6),
            (13, 0),
        ] {
            assert!(!whisper_cuda_supported(cc), "{cc:?} should not be covered");
        }
    }

    #[test]
    fn gpu_fingerprint_changes_with_driver() {
        let mut gpu = GpuInfo {
            vendor: GpuVendor::Nvidia,
            name: "NVIDIA GeForce RTX 5070".to_string(),
            compute_cap: Some("12.0".to_string()),
            driver_version: Some("610.88".to_string()),
        };
        let first = gpu.fingerprint();
        assert_eq!(first, gpu.clone().fingerprint());
        gpu.driver_version = Some("612.01".to_string());
        assert_ne!(first, gpu.fingerprint());
    }

    #[cfg(windows)]
    #[test]
    fn adapter_pick_prefers_discrete_vendor_then_memory() {
        let adapters = vec![
            Adapter { vendor: GpuVendor::Intel, name: "Intel UHD".to_string(), memory: 128 },
            Adapter { vendor: GpuVendor::Nvidia, name: "RTX 4060 Laptop".to_string(), memory: 8192 },
            Adapter { vendor: GpuVendor::Amd, name: "Radeon".to_string(), memory: 16384 },
        ];
        assert_eq!(pick_adapter(&adapters).map(|a| a.name.as_str()), Some("RTX 4060 Laptop"));
        let amd_intel = vec![
            Adapter { vendor: GpuVendor::Amd, name: "Radeon 780M".to_string(), memory: 512 },
            Adapter { vendor: GpuVendor::Amd, name: "Radeon RX 7800".to_string(), memory: 16384 },
            Adapter { vendor: GpuVendor::Intel, name: "Arc".to_string(), memory: 32768 },
        ];
        assert_eq!(pick_adapter(&amd_intel).map(|a| a.name.as_str()), Some("Radeon RX 7800"));
        assert!(pick_adapter(&[]).is_none());
        assert_eq!(vendor_from_id(0x10DE), GpuVendor::Nvidia);
        assert_eq!(vendor_from_id(0x1002), GpuVendor::Amd);
        assert_eq!(vendor_from_id(0x8086), GpuVendor::Intel);
        assert_eq!(vendor_from_id(0x15AD), GpuVendor::Other);
    }
}
