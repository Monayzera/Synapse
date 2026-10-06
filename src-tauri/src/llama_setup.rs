use crate::config::{LlmBackend, Settings};
use crate::error::{AppError, AppResult};
use crate::hardware::{self, GpuInfo};
use crate::models;
use crate::services::{self, SidecarOutcome};
use crate::state::{LlmState, SharedState};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

#[cfg(windows)]
const BIN_NAME: &str = "llama-server.exe";
#[cfg(not(windows))]
const BIN_NAME: &str = "llama-server";

const BACKEND_FILE: &str = "backend.json";
const SETUP_DIR: &str = "bin.setup";
const DOWNLOADS_DIR: &str = "downloads";
const STAGE_DIR: &str = "stage";
const OLD_DIR_PREFIX: &str = "bin.old-";
const INFERRED_TAG: &str = "unknown";
const SETUP_RUNNING: &str = "Setup already in progress.";
const SETUP_ABORTED: &str = "The local AI setup stopped unexpectedly. Please try again.";
const SERVER_NOT_READY: &str = "The local server did not respond in time. Please try again.";
const DEFAULT_GPU_LAYERS: i32 = 99;
#[cfg(windows)]
const LIST_DEVICES_TIMEOUT: Duration = Duration::from_secs(30);
const SWAP_BUDGET: Duration = Duration::from_secs(10);
const SETTLE_TIMEOUT: Duration = Duration::from_secs(90);
const BINARY_START: f64 = 2.0;
const BINARY_END: f64 = 30.0;
const MODEL_END: f64 = 95.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupMode {
    Full,
    Repair,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendDevice {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendInfo {
    pub variant: String,
    pub tag: String,
    #[serde(default)]
    pub devices: Vec<BackendDevice>,
    #[serde(default)]
    pub gpu_name: Option<String>,
    #[serde(default)]
    pub failed_gpu: Option<String>,
    #[serde(default)]
    pub gpu_fingerprint: Option<String>,
}

impl BackendInfo {
    fn device_prefix(&self) -> Option<&'static str> {
        if self.variant.starts_with("cuda") {
            Some("CUDA")
        } else if self.variant == "vulkan" {
            Some("Vulkan")
        } else {
            None
        }
    }

    pub fn gpu_ready(&self) -> bool {
        if self.variant == "metal" {
            return true;
        }
        match self.device_prefix() {
            Some(prefix) => self.devices.iter().any(|device| device.id.starts_with(prefix)),
            None => false,
        }
    }

    fn gpu_device_name(&self) -> Option<String> {
        let prefix = self.device_prefix()?;
        self.devices
            .iter()
            .find(|device| device.id.starts_with(prefix))
            .map(|device| device.name.clone())
    }
}

#[derive(Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Stage {
    ResolveRelease,
    DownloadBinary,
    Unzip,
    DownloadModel,
    ConfigureStart,
}

#[derive(Serialize, Clone)]
struct Progress {
    stage: Stage,
    pct: f64,
    overall_pct: f64,
    message: String,
    done: bool,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct Asset {
    name: String,
    url: String,
    size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Want {
    Cuda(u32),
    Vulkan,
    Cpu,
}

#[derive(Debug, Clone)]
struct Package {
    want: Want,
    variant: String,
    files: Vec<Asset>,
}

impl Package {
    fn gpu(&self) -> bool {
        self.want != Want::Cpu
    }

    fn title(&self) -> String {
        match self.want {
            Want::Cuda(_) => format!("CUDA {}", self.variant.trim_start_matches("cuda-")),
            Want::Vulkan => "Vulkan".to_string(),
            Want::Cpu => "CPU".to_string(),
        }
    }
}

type Step<T> = Result<T, (Stage, AppError)>;

fn at(stage: Stage) -> impl FnOnce(AppError) -> (Stage, AppError) {
    move |err| (stage, err)
}

fn emit(app: &AppHandle, stage: Stage, pct: f64, overall: f64, message: &str) {
    let _ = app.emit(
        "llama-setup-progress",
        Progress {
            stage,
            pct: (pct * 10.0).round() / 10.0,
            overall_pct: (overall.clamp(0.0, 100.0) * 10.0).round() / 10.0,
            message: message.to_string(),
            done: false,
            error: None,
        },
    );
}

fn emit_done(app: &AppHandle, message: &str) {
    let _ = app.emit(
        "llama-setup-progress",
        Progress {
            stage: Stage::ConfigureStart,
            pct: 100.0,
            overall_pct: 100.0,
            message: message.to_string(),
            done: true,
            error: None,
        },
    );
}

fn emit_error(app: &AppHandle, stage: Stage, message: &str) {
    let _ = app.emit(
        "llama-setup-progress",
        Progress {
            stage,
            pct: 0.0,
            overall_pct: 0.0,
            message: message.to_string(),
            done: false,
            error: Some(message.to_string()),
        },
    );
}

struct Meter<'a> {
    app: &'a AppHandle,
    overall: f64,
}

impl Meter<'_> {
    fn report(&mut self, stage: Stage, pct: f64, overall: f64, message: &str) {
        if overall > self.overall {
            self.overall = overall;
        }
        emit(self.app, stage, pct.clamp(0.0, 100.0), self.overall, message);
    }
}

struct SetupGuard {
    app: AppHandle,
    state: SharedState,
    finished: bool,
    reported: bool,
}

impl SetupGuard {
    fn finish(&mut self) -> bool {
        self.finished = true;
        self.state.finish_llama_setup()
    }
}

impl Drop for SetupGuard {
    fn drop(&mut self) {
        if self.reported {
            return;
        }
        tracing::error!("local AI setup stopped before reporting its result");
        if !self.finished {
            self.state.finish_llama_setup();
        }
        self.state.set_local_ai_error(Some(SETUP_ABORTED.to_string()));
        emit_error(&self.app, Stage::ConfigureStart, SETUP_ABORTED);
        let state = self.state.clone();
        tauri::async_runtime::spawn(async move {
            services::restart_sidecar(&state).await;
        });
    }
}

pub fn launch(app: AppHandle, state: SharedState, mode: SetupMode) -> Result<(), String> {
    if !state.begin_llama_setup() {
        tracing::warn!("local AI setup ({mode:?}) not started: another setup is running");
        return Err(SETUP_RUNNING.to_string());
    }
    tracing::info!("local AI setup started ({mode:?})");
    state.set_local_ai_error(None);
    tauri::async_runtime::spawn(async move {
        let mut guard = SetupGuard {
            app: app.clone(),
            state: state.clone(),
            finished: false,
            reported: false,
        };
        let worker = tauri::async_runtime::spawn(run_owned(app.clone(), state.clone(), mode));
        let (result, stopped) = match worker.await {
            Ok(done) => done,
            Err(err) => {
                tracing::error!("local AI setup task failed: {err}");
                (
                    Err((
                        Stage::ConfigureStart,
                        AppError::Other(format!("setup task failed: {err}")),
                    )),
                    true,
                )
            }
        };
        let pending = guard.finish();
        let outcome = if stopped || pending || result.is_ok() {
            let first = services::restart_sidecar(&state).await;
            Some(settle(&state, first).await)
        } else {
            None
        };
        state.refresh_local_ai_files();
        match result {
            Ok(()) => match outcome {
                Some(SidecarOutcome::Ready) => {
                    tracing::info!("local AI setup finished; server ready");
                    emit_done(&app, "AI correction ready to use.");
                }
                Some(SidecarOutcome::Off) | Some(SidecarOutcome::Deferred) | None => {
                    tracing::info!("local AI setup finished; server not started ({outcome:?})");
                    emit_done(&app, "Local AI installed.");
                }
                Some(other) => {
                    let message = state
                        .local_ai_error
                        .read()
                        .clone()
                        .unwrap_or_else(|| SERVER_NOT_READY.to_string());
                    tracing::warn!(
                        "local AI setup finished but the server is not ready ({other:?}): {message}"
                    );
                    state.set_local_ai_error(Some(message.clone()));
                    emit_error(&app, Stage::ConfigureStart, &message);
                }
            },
            Err((stage, err)) => {
                tracing::warn!("llama auto-setup failed: {err}");
                let message = err.to_string();
                state.set_local_ai_error(Some(message.clone()));
                emit_error(&app, stage, &message);
            }
        }
        guard.reported = true;
        detect_repairable(&state, false).await;
    });
    Ok(())
}

async fn settle(state: &SharedState, outcome: SidecarOutcome) -> SidecarOutcome {
    if outcome != SidecarOutcome::Superseded {
        return outcome;
    }
    let deadline = tokio::time::Instant::now() + SETTLE_TIMEOUT;
    loop {
        match state.local_llm_state() {
            LlmState::Ready => return SidecarOutcome::Ready,
            LlmState::Off => return SidecarOutcome::Off,
            LlmState::Failed => return SidecarOutcome::Failed,
            LlmState::Starting => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return SidecarOutcome::Failed;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn run_owned(app: AppHandle, state: SharedState, mode: SetupMode) -> (Step<()>, bool) {
    let _files = state.llama_files.lock().await;
    let mut stopped = false;
    let result = run(&app, &state, mode, &mut stopped).await;
    (result, stopped)
}

async fn run(
    app: &AppHandle,
    state: &SharedState,
    mode: SetupMode,
    stopped: &mut bool,
) -> Step<()> {
    let mut meter = Meter { app, overall: 0.0 };
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .tcp_keepalive(Duration::from_secs(30))
        .user_agent("Synapse")
        .build()
        .map_err(|e| (Stage::ResolveRelease, AppError::Download(e.to_string())))?;

    meter.report(Stage::ResolveRelease, 0.0, 1.0, "Detecting the graphics card...");
    let gpu = hardware::gpu(mode == SetupMode::Repair).await;
    let plan = plan_for(gpu.as_ref());
    tracing::info!("local AI build plan: {plan:?}");

    meter.report(Stage::ResolveRelease, 50.0, 1.5, "Searching for the latest llama-server...");
    let (tag, assets) = resolve_assets(&client, &plan)
        .await
        .map_err(at(Stage::ResolveRelease))?;
    let candidates: Vec<Package> = plan
        .iter()
        .filter_map(|want| package_for(&assets, *want))
        .collect();
    if candidates.is_empty() {
        return Err((
            Stage::ResolveRelease,
            AppError::Download("llama-server build not found for this system".to_string()),
        ));
    }
    tracing::info!(
        "llama.cpp {tag}: candidate builds {:?}",
        candidates.iter().map(|p| p.variant.as_str()).collect::<Vec<_>>()
    );

    let bin_dir = state.bin_dir.clone();
    let root_dir = setup_root(&bin_dir);
    let downloads = root_dir.join(DOWNLOADS_DIR);
    let stage_dir = root_dir.join(STAGE_DIR);
    let wanted: HashSet<String> = candidates
        .iter()
        .flat_map(|package| package.files.iter().map(|asset| asset.name.clone()))
        .collect();
    let stale = bin_dir.clone();
    blocking(move || {
        cleanup_stale(&stale, Some(&wanted));
        Ok(())
    })
    .await
    .map_err(at(Stage::DownloadBinary))?;

    let (staged, info) = install_candidates(
        &mut meter,
        &client,
        &candidates,
        &downloads,
        &stage_dir,
        gpu.as_ref(),
        &tag,
    )
    .await?;

    let model_file = if mode == SetupMode::Full {
        Some(ensure_model(&mut meter, &client, state).await?)
    } else {
        None
    };

    meter.report(Stage::ConfigureStart, 0.0, 96.0, "Configuring and starting the local server...");
    configure_settings(state, model_file.as_deref(), info.gpu_ready())
        .map_err(at(Stage::ConfigureStart))?;

    *stopped = true;
    let generation = state.stop_sidecar();
    let settings = state.settings_snapshot();
    if services::llm_wanted(&settings) && settings.llm_backend == LlmBackend::Local {
        state.publish_llm_state(generation, LlmState::Starting, false);
    }
    swap_in(&staged, &bin_dir)
        .await
        .map_err(at(Stage::ConfigureStart))?;
    state.set_gpu_fallback(None);
    state.set_llama_backend(Some(info));
    match tokio::fs::remove_dir_all(&root_dir).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!("setup files in {} not removed: {err}", root_dir.display()),
    }
    Ok(())
}

async fn install_candidates(
    meter: &mut Meter<'_>,
    client: &reqwest::Client,
    candidates: &[Package],
    downloads: &Path,
    stage_dir: &Path,
    gpu: Option<&GpuInfo>,
    tag: &str,
) -> Step<(PathBuf, BackendInfo)> {
    let mut gpu_failed = false;
    for (index, package) in candidates.iter().enumerate() {
        let start = meter.overall.max(BINARY_START);
        let span = (start, start + (BINARY_END - start) * 0.8);
        let root = stage_package(meter, client, package, downloads, stage_dir, span).await?;
        meter.report(
            Stage::Unzip,
            90.0,
            start + (BINARY_END - start) * 0.9,
            &format!("Checking llama-server ({})...", package.title()),
        );
        let validation = validate(&root).await;
        let info = match judge(package, validation, tag, gpu, gpu_failed) {
            Judged::Accept(info) => info,
            Judged::Next { no_device } => {
                gpu_failed |= no_device;
                announce_next(meter, candidates, index, package);
                continue;
            }
            Judged::Reject(err) => {
                return Err((
                    Stage::Unzip,
                    AppError::Download(format!("llama-server does not run on this computer: {err}")),
                ))
            }
        };
        let record = info.clone();
        let target = root.clone();
        blocking(move || write_backend(&target, &record))
            .await
            .map_err(at(Stage::Unzip))?;
        tracing::info!("llama-server {} accepted ({tag})", info.variant);
        meter.report(Stage::Unzip, 100.0, BINARY_END, "llama-server installed.");
        return Ok((root, info));
    }
    Err((
        Stage::Unzip,
        AppError::Download("no llama-server build could run on this computer".to_string()),
    ))
}

#[derive(Debug, PartialEq, Eq)]
enum Judged {
    Accept(BackendInfo),
    Next { no_device: bool },
    Reject(String),
}

fn judge(
    package: &Package,
    validation: Result<Vec<BackendDevice>, String>,
    tag: &str,
    gpu: Option<&GpuInfo>,
    gpu_failed: bool,
) -> Judged {
    let devices = match validation {
        Ok(devices) => devices,
        Err(err) if package.gpu() => {
            tracing::warn!("llama-server {} could not be checked: {err}", package.variant);
            return Judged::Next { no_device: false };
        }
        Err(err) => return Judged::Reject(err),
    };
    let mut info = BackendInfo {
        variant: package.variant.clone(),
        tag: tag.to_string(),
        devices,
        gpu_name: None,
        failed_gpu: None,
        gpu_fingerprint: gpu.map(GpuInfo::fingerprint),
    };
    if package.gpu() && !info.gpu_ready() {
        tracing::warn!(
            "llama-server {} lists no usable graphics device ({:?})",
            package.variant,
            info.devices
        );
        return Judged::Next { no_device: true };
    }
    info.gpu_name = info.gpu_device_name();
    if !package.gpu() && gpu_failed {
        info.failed_gpu = gpu.map(GpuInfo::fingerprint);
    }
    Judged::Accept(info)
}

fn announce_next(meter: &mut Meter<'_>, candidates: &[Package], index: usize, failed: &Package) {
    if let Some(next) = candidates.get(index + 1) {
        let overall = meter.overall;
        meter.report(
            Stage::DownloadBinary,
            0.0,
            overall,
            &format!(
                "The graphics card did not work with {}; trying {}...",
                failed.title(),
                next.title()
            ),
        );
    }
}

async fn stage_package(
    meter: &mut Meter<'_>,
    client: &reqwest::Client,
    package: &Package,
    downloads: &Path,
    stage_dir: &Path,
    span: (f64, f64),
) -> Step<PathBuf> {
    let (archives, reused) = fetch_package(meter, client, package, downloads, span)
        .await
        .map_err(at(Stage::DownloadBinary))?;
    meter.report(
        Stage::Unzip,
        0.0,
        span.1,
        &format!("Extracting llama-server ({})...", package.title()),
    );
    match extract_staged(archives.clone(), stage_dir.to_path_buf()).await {
        Ok(root) => Ok(root),
        Err(err) if reused => {
            tracing::warn!("cached llama-server package unusable ({err}); downloading it again");
            discard(&archives).await;
            let (fresh, _) = fetch_package(meter, client, package, downloads, span)
                .await
                .map_err(at(Stage::DownloadBinary))?;
            match extract_staged(fresh.clone(), stage_dir.to_path_buf()).await {
                Ok(root) => Ok(root),
                Err(err) => {
                    discard(&fresh).await;
                    Err((Stage::Unzip, err))
                }
            }
        }
        Err(err) => {
            discard(&archives).await;
            Err((Stage::Unzip, err))
        }
    }
}

async fn discard(paths: &[PathBuf]) {
    for path in paths {
        match tokio::fs::remove_file(path).await {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!("{} not removed: {err}", path.display()),
        }
    }
}

fn reusable(path: &Path, size: u64) -> bool {
    size > 0
        && std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.len() == size)
            .unwrap_or(false)
}

fn file_message(asset: &Asset, package: &Package) -> String {
    if asset.name.to_ascii_lowercase().starts_with("cudart-") {
        format!(
            "Downloading the CUDA runtime ({})...",
            package.variant.trim_start_matches("cuda-")
        )
    } else if package.gpu() {
        format!("Downloading llama-server ({})...", package.title())
    } else {
        "Downloading llama-server...".to_string()
    }
}

async fn fetch_package(
    meter: &mut Meter<'_>,
    client: &reqwest::Client,
    package: &Package,
    downloads: &Path,
    span: (f64, f64),
) -> AppResult<(Vec<PathBuf>, bool)> {
    tokio::fs::create_dir_all(downloads)
        .await
        .map_err(|e| AppError::Download(format!("{}: {e}", downloads.display())))?;
    let known = package.files.iter().all(|asset| asset.size > 0);
    let total_known: u64 = package.files.iter().map(|asset| asset.size).sum();
    let count = package.files.len().max(1) as f64;
    let mut before: u64 = 0;
    let mut reused = false;
    let mut paths = Vec::new();
    for (index, asset) in package.files.iter().enumerate() {
        let message = file_message(asset, package);
        let dest = downloads.join(&asset.name);
        if reusable(&dest, asset.size) {
            tracing::info!("reusing downloaded {}", asset.name);
            reused = true;
            before = before.saturating_add(asset.size);
            let fraction = if known && total_known > 0 {
                before as f64 / total_known as f64
            } else {
                (index as f64 + 1.0) / count
            };
            meter.report(
                Stage::DownloadBinary,
                fraction * 100.0,
                span.0 + (span.1 - span.0) * fraction,
                &message,
            );
            paths.push(dest);
            continue;
        }
        meter.report(
            Stage::DownloadBinary,
            if known && total_known > 0 {
                before as f64 / total_known as f64 * 100.0
            } else {
                index as f64 / count * 100.0
            },
            span.0,
            &message,
        );
        let mut last = 0u64;
        let base = before;
        let expected = asset.size;
        models::download_to_file(client, &asset.url, &dest, asset.size, |done, total| {
            if total > 0 && (done.saturating_sub(last) >= 4_000_000 || done == total) {
                last = done;
                let fraction = if known && total_known > 0 {
                    (base + done.min(expected)) as f64 / total_known as f64
                } else {
                    (index as f64 + (done as f64 / total as f64).min(1.0)) / count
                };
                let fraction = fraction.clamp(0.0, 1.0);
                meter.report(
                    Stage::DownloadBinary,
                    fraction * 100.0,
                    span.0 + (span.1 - span.0) * fraction,
                    &message,
                );
            }
        })
        .await?;
        before = before.saturating_add(asset.size);
        paths.push(dest);
    }
    Ok((paths, reused))
}

async fn extract_staged(archives: Vec<PathBuf>, stage: PathBuf) -> AppResult<PathBuf> {
    blocking(move || extract_package(&archives, &stage)).await
}

async fn blocking<T, F>(work: F) -> AppResult<T>
where
    F: FnOnce() -> AppResult<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

fn extract_package(archives: &[PathBuf], stage: &Path) -> AppResult<PathBuf> {
    match std::fs::remove_dir_all(stage) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(AppError::Io(format!(
                "could not clear {}: {err}",
                stage.display()
            )))
        }
    }
    std::fs::create_dir_all(stage)?;
    for archive in archives {
        let lower = archive
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if lower.ends_with(".zip") {
            extract_zip(archive, stage)?;
        } else {
            extract_targz(archive, stage)?;
        }
    }
    let exe = find_binary(stage, BIN_NAME)
        .ok_or_else(|| AppError::Download("llama-server not found in the package".to_string()))?;
    let root = exe
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| AppError::Download("invalid package structure".to_string()))?;
    if root != stage {
        for entry in std::fs::read_dir(stage)? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                std::fs::rename(entry.path(), root.join(entry.file_name()))?;
            }
        }
    }
    set_executable(&root.join(BIN_NAME));
    Ok(root)
}

#[cfg(windows)]
async fn validate(root: &Path) -> Result<Vec<BackendDevice>, String> {
    let dir = root.to_path_buf();
    match tokio::task::spawn_blocking(move || list_devices(&dir)).await {
        Ok(result) => result,
        Err(err) => Err(format!("check task failed: {err}")),
    }
}

#[cfg(not(windows))]
async fn validate(_root: &Path) -> Result<Vec<BackendDevice>, String> {
    Ok(Vec::new())
}

#[cfg(windows)]
fn list_devices(dir: &Path) -> Result<Vec<BackendDevice>, String> {
    let mut command = std::process::Command::new(dir.join(BIN_NAME));
    command.arg("--list-devices");
    let mut path = std::ffi::OsString::from(dir.as_os_str());
    path.push(";");
    if let Some(current) = std::env::var_os("PATH") {
        path.push(current);
    }
    command.env("PATH", path);
    let text = crate::sidecar::run_capture(command, LIST_DEVICES_TIMEOUT)?;
    if !text.contains("Available devices") {
        let snippet: String = text.trim().chars().take(240).collect();
        return Err(format!("unexpected output: {snippet}"));
    }
    let devices = parse_devices(&text);
    tracing::info!(
        "llama-server in {} lists {} device(s): {:?}",
        dir.display(),
        devices.len(),
        devices
    );
    Ok(devices)
}

fn parse_devices(text: &str) -> Vec<BackendDevice> {
    const KINDS: [&str; 4] = ["CUDA", "Vulkan", "ROCm", "SYCL"];
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((id, rest)) = line.trim().split_once(':') else {
            continue;
        };
        let id = id.trim();
        let known = KINDS.iter().any(|kind| {
            id.strip_prefix(kind)
                .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        });
        if !known {
            continue;
        }
        let mut name = rest.trim();
        if name.ends_with(')') {
            if let Some(open) = name.rfind('(') {
                if name[open..].contains("MiB") {
                    name = name[..open].trim_end();
                }
            }
        }
        if name.is_empty() {
            continue;
        }
        out.push(BackendDevice {
            id: id.to_string(),
            name: name.to_string(),
        });
    }
    out
}

fn write_backend(dir: &Path, info: &BackendInfo) -> AppResult<()> {
    let bytes = serde_json::to_vec_pretty(info)?;
    crate::atomic_io::write_durable(&dir.join(BACKEND_FILE), &bytes)?;
    Ok(())
}

pub fn read_backend(bin_dir: &Path) -> Option<BackendInfo> {
    let path = bin_dir.join(BACKEND_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<BackendInfo>(&text) {
            Ok(info) => Some(info),
            Err(err) => {
                tracing::warn!(
                    "{} unreadable ({err}); local AI build treated as unknown",
                    path.display()
                );
                None
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!("{} could not be read: {err}", path.display());
            None
        }
    }
}

fn setup_root(bin_dir: &Path) -> PathBuf {
    bin_dir.with_file_name(SETUP_DIR)
}

fn safe_asset_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

fn remove_quietly(path: &Path) {
    match std::fs::remove_dir_all(path) {
        Ok(()) => tracing::info!("removed leftover {}", path.display()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!("leftover {} not removed: {err}", path.display()),
    }
}

fn prune_downloads(dir: &Path, keep: Option<&HashSet<String>>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
        Err(err) => {
            tracing::debug!("{} not listed: {err}", dir.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            remove_quietly(&path);
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if !stale_download(&name, keep) {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => tracing::info!("removed stale download {}", path.display()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => tracing::warn!("stale download {} not removed: {err}", path.display()),
        }
    }
}

fn stale_download(name: &str, keep: Option<&HashSet<String>>) -> bool {
    name.to_ascii_lowercase().ends_with(".part") || keep.is_some_and(|keep| !keep.contains(name))
}

pub async fn clean_leftovers(state: &SharedState) {
    let Ok(_files) = state.llama_files.try_lock() else {
        tracing::info!("local AI setup running; leftover cleanup left to it");
        return;
    };
    let bin_dir = state.bin_dir.clone();
    let cleanup = blocking(move || {
        cleanup_stale(&bin_dir, None);
        Ok(())
    });
    if let Err(err) = cleanup.await {
        tracing::warn!("local AI leftovers not cleaned: {err}");
    }
}

fn cleanup_stale(bin_dir: &Path, keep: Option<&HashSet<String>>) {
    let root = setup_root(bin_dir);
    remove_quietly(&root.join(STAGE_DIR));
    prune_downloads(&root.join(DOWNLOADS_DIR), keep);
    let Some(parent) = bin_dir.parent() else {
        return;
    };
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(err) => {
            tracing::debug!("{} not listed: {err}", parent.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(OLD_DIR_PREFIX) && entry.path().is_dir() {
            remove_quietly(&entry.path());
        }
    }
}

fn is_sharing_violation(err: &std::io::Error) -> bool {
    cfg!(windows) && matches!(err.raw_os_error(), Some(32) | Some(5))
}

async fn rename_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let deadline = tokio::time::Instant::now() + SWAP_BUDGET;
    let mut delay = Duration::from_millis(100);
    loop {
        match tokio::fs::rename(from, to).await {
            Ok(()) => return Ok(()),
            Err(err)
                if is_sharing_violation(&err)
                    && tokio::time::Instant::now() + delay <= deadline =>
            {
                tracing::info!(
                    "{} is busy ({err}); retrying in {} ms",
                    from.display(),
                    delay.as_millis()
                );
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_millis(1600));
            }
            Err(err) => return Err(err),
        }
    }
}

async fn remove_dir_patiently(dir: PathBuf) {
    let mut delay = Duration::from_secs(1);
    for attempt in 1..=5u32 {
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => {
                tracing::info!("removed {}", dir.display());
                return;
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return,
            Err(err) if attempt == 5 => {
                tracing::warn!(
                    "{} could not be removed ({err}); the next setup will retry",
                    dir.display()
                );
                return;
            }
            Err(err) => {
                tracing::debug!("{} not removed yet ({err})", dir.display());
                tokio::time::sleep(delay).await;
                delay *= 2;
            }
        }
    }
}

async fn swap_in(staged: &Path, bin_dir: &Path) -> AppResult<()> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let backup = bin_dir.with_file_name(format!("{OLD_DIR_PREFIX}{stamp}"));
    let moved = match rename_retry(bin_dir, &backup).await {
        Ok(()) => true,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => {
            return Err(AppError::Io(format!(
                "the current local AI files are in use and could not be replaced ({err})"
            )))
        }
    };
    if let Err(err) = rename_retry(staged, bin_dir).await {
        if moved {
            if let Err(restore) = rename_retry(&backup, bin_dir).await {
                tracing::error!(
                    "previous local AI files could not be restored from {}: {restore}",
                    backup.display()
                );
            }
        }
        return Err(AppError::Io(format!(
            "the new local AI files could not be installed ({err})"
        )));
    }
    tracing::info!("local AI files installed into {}", bin_dir.display());
    if moved {
        tauri::async_runtime::spawn(async move {
            remove_dir_patiently(backup).await;
        });
    }
    Ok(())
}

async fn ensure_model(
    meter: &mut Meter<'_>,
    client: &reqwest::Client,
    state: &SharedState,
) -> Step<String> {
    let gemma = models::find("gemma-3-4b-it").ok_or_else(|| {
        (
            Stage::DownloadModel,
            AppError::Model("unknown correction model".to_string()),
        )
    })?;
    if models::is_present(&state.models_dir, &gemma) {
        meter.report(Stage::DownloadModel, 100.0, MODEL_END, "AI correction model already present.");
        return Ok(gemma.filename);
    }
    let dest = models::model_path(&state.models_dir, &gemma.filename);
    let start = meter.overall.max(BINARY_END);
    let message = "Downloading the AI correction model (Gemma 3 4B)...";
    meter.report(Stage::DownloadModel, 0.0, start, message);
    let mut last = 0u64;
    models::download_to_file(client, &gemma.url, &dest, gemma.size_bytes, |done, total| {
        if total > 0 && (done.saturating_sub(last) >= 8_000_000 || done == total) {
            last = done;
            let pct = (done as f64 / total as f64 * 100.0).clamp(0.0, 100.0);
            meter.report(
                Stage::DownloadModel,
                pct,
                start + (MODEL_END - start) * pct / 100.0,
                message,
            );
        }
    })
    .await
    .map_err(at(Stage::DownloadModel))?;
    Ok(gemma.filename)
}

pub async fn startup(state: &SharedState) -> bool {
    let gpu = hardware::gpu(false).await;
    refresh_backend(state, gpu.as_ref()).await;
    let raised = raise_gpu_layers(state);
    update_repairable(state, gpu.as_ref());
    raised
}

async fn refresh_backend(state: &SharedState, gpu: Option<&GpuInfo>) {
    if !cfg!(windows) {
        return;
    }
    let exe = state.sidecar_binary();
    if !exe.is_file() {
        return;
    }
    let Some(dir) = exe.parent().map(Path::to_path_buf) else {
        return;
    };
    let current = state.llama_backend.read().clone();
    let recorded = current.clone().filter(|_| dir == state.bin_dir);
    if recorded
        .as_ref()
        .is_some_and(|info| !backend_stale(info, gpu))
    {
        return;
    }
    let devices = match validate(&dir).await {
        Ok(devices) => devices,
        Err(err) => {
            tracing::warn!(
                "llama-server in {} could not be checked at startup: {err}",
                dir.display()
            );
            return;
        }
    };
    let info = match recorded {
        Some(info) => refreshed_backend(info, devices),
        None => inferred_backend(devices),
    };
    tracing::info!(
        "llama-server in {} checked at startup: build {} ({}), devices {:?}",
        dir.display(),
        info.variant,
        info.tag,
        info.devices
    );
    if !state.adopt_llama_backend(current.as_ref(), Some(info)) {
        tracing::info!("local AI build changed during the startup check; result discarded");
    }
}

fn backend_stale(info: &BackendInfo, gpu: Option<&GpuInfo>) -> bool {
    info.gpu_fingerprint != gpu.map(GpuInfo::fingerprint)
}

fn refreshed_backend(mut info: BackendInfo, devices: Vec<BackendDevice>) -> BackendInfo {
    info.devices = devices;
    info.gpu_name = info.gpu_device_name();
    info
}

fn inferred_backend(devices: Vec<BackendDevice>) -> BackendInfo {
    let listed = |prefix: &str| devices.iter().any(|device| device.id.starts_with(prefix));
    let variant = if listed("CUDA") {
        "cuda"
    } else if listed("Vulkan") {
        "vulkan"
    } else {
        "cpu"
    };
    let mut info = BackendInfo {
        variant: variant.to_string(),
        tag: INFERRED_TAG.to_string(),
        devices,
        gpu_name: None,
        failed_gpu: None,
        gpu_fingerprint: None,
    };
    info.gpu_name = info.gpu_device_name();
    info
}

fn raise_gpu_layers(state: &SharedState) -> bool {
    let gpu_ready = state
        .llama_backend
        .read()
        .as_ref()
        .is_some_and(BackendInfo::gpu_ready);
    let layers = state.settings.read().llm_gpu_layers;
    if !gpu_ready || layers > 0 {
        return false;
    }
    let changed = state.mutate_settings(|settings| {
        if settings.llm_gpu_layers <= 0 {
            settings.llm_gpu_layers = DEFAULT_GPU_LAYERS;
        }
        Ok(())
    });
    match changed {
        Ok((old, new)) if old.llm_gpu_layers != new.llm_gpu_layers => {
            tracing::info!(
                "graphics card available; local AI GPU layers raised from {} to {}",
                old.llm_gpu_layers,
                new.llm_gpu_layers
            );
            state.emit_settings_changed();
            true
        }
        Ok(_) => false,
        Err(err) => {
            tracing::warn!("local AI GPU layers not raised: {err}");
            false
        }
    }
}

pub async fn detect_repairable(state: &SharedState, refresh: bool) {
    let gpu = hardware::gpu(refresh).await;
    update_repairable(state, gpu.as_ref());
}

fn update_repairable(state: &SharedState, gpu: Option<&GpuInfo>) {
    let installed = state.sidecar_binary().exists();
    let backend = state.llama_backend.read().clone();
    let value = cfg!(windows) && repairable_for(gpu, installed, backend.as_ref());
    if state.local_ai_repairable.swap(value, Ordering::AcqRel) != value {
        tracing::info!("local AI repair available: {value}");
    }
    state.emit_local_ai_status();
}

fn repairable_for(gpu: Option<&GpuInfo>, installed: bool, backend: Option<&BackendInfo>) -> bool {
    if !installed {
        return false;
    }
    let Some(gpu) = gpu else {
        return false;
    };
    match backend {
        None => true,
        Some(info) if info.gpu_ready() => cuda13_upgrade(gpu, info),
        Some(info) => info.failed_gpu.as_deref() != Some(gpu.fingerprint().as_str()),
    }
}

fn cuda_major(variant: &str) -> Option<u32> {
    variant.strip_prefix("cuda-")?.split('.').next()?.parse().ok()
}

fn cuda13_upgrade(gpu: &GpuInfo, info: &BackendInfo) -> bool {
    gpu.vendor == hardware::GpuVendor::Nvidia
        && gpu.cc().is_some_and(|cc| cc >= (12, 0))
        && plan_for(Some(gpu)).first() == Some(&Want::Cuda(13))
        && info.variant.starts_with("cuda")
        && !cuda_major(&info.variant).is_some_and(|major| major >= 13)
}

fn plan_for(gpu: Option<&GpuInfo>) -> Vec<Want> {
    #[cfg(windows)]
    {
        windows_plan(gpu)
    }
    #[cfg(not(windows))]
    {
        let _ = gpu;
        vec![Want::Cpu]
    }
}

#[cfg(any(windows, test))]
fn windows_plan(gpu: Option<&GpuInfo>) -> Vec<Want> {
    let Some(gpu) = gpu else {
        return vec![Want::Cpu];
    };
    if gpu.vendor == hardware::GpuVendor::Nvidia {
        let driver = gpu.driver_major();
        if driver.is_some_and(|d| d >= 580) && gpu.cc().is_some_and(|cc| cc >= (7, 5)) {
            return vec![Want::Cuda(13), Want::Vulkan, Want::Cpu];
        }
        if driver.is_some_and(|d| d >= 551) {
            return vec![Want::Cuda(12), Want::Vulkan, Want::Cpu];
        }
    }
    vec![Want::Vulkan, Want::Cpu]
}

const RELEASES_API: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases?per_page=10";
const NIGHTLY_TAG_URL: &str =
    "https://github.com/ggml-org/llama.cpp/releases/latest/download/nightly-tag.txt";

async fn resolve_assets(client: &reqwest::Client, plan: &[Want]) -> AppResult<(String, Vec<Asset>)> {
    let mut errors = Vec::new();
    match resolve_via_api(client, plan).await {
        Ok(Some(found)) => return Ok(found),
        Ok(None) => errors.push("api.github.com: no recent release has a build for this system".to_string()),
        Err(err) => errors.push(format!("api.github.com: {err}")),
    }
    match resolve_via_html(client).await {
        Ok(Some(found)) => return Ok(found),
        Ok(None) => errors.push("github.com: the nightly release has no build for this system".to_string()),
        Err(err) => errors.push(format!("github.com: {err}")),
    }
    Err(AppError::Download(format!(
        "could not fetch llama-server (check your connection): {}",
        errors.join(" | ")
    )))
}

async fn resolve_via_api(
    client: &reqwest::Client,
    plan: &[Want],
) -> AppResult<Option<(String, Vec<Asset>)>> {
    let response = client
        .get(RELEASES_API)
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(AppError::Download(api_failure(status, response.headers())));
    }
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;
    let releases = value
        .as_array()
        .ok_or_else(|| AppError::Download("unexpected response".to_string()))?;
    Ok(select_release(releases, plan).map(|(tag, assets)| {
        tracing::info!("llama.cpp release {tag} selected");
        (tag, assets)
    }))
}

fn select_release(releases: &[serde_json::Value], plan: &[Want]) -> Option<(String, Vec<Asset>)> {
    let primary = plan.first().copied().unwrap_or(Want::Cpu);
    let mut fallback = None;
    for release in releases {
        if release.get("draft").and_then(|v| v.as_bool()).unwrap_or(false) {
            continue;
        }
        let tag = release
            .get("tag_name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let assets = release
            .get("assets")
            .and_then(|a| a.as_array())
            .map(|list| api_assets(list))
            .unwrap_or_default();
        if cpu_asset(&assets).is_none() {
            continue;
        }
        if package_for(&assets, primary).is_some() {
            return Some((tag, assets));
        }
        if fallback.is_none() {
            fallback = Some((tag, assets));
        }
    }
    fallback
}

fn api_assets(list: &[serde_json::Value]) -> Vec<Asset> {
    let mut out = Vec::new();
    for asset in list {
        let name = asset.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let url = asset
            .get("browser_download_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let size = asset.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
        if safe_asset_name(name) && !url.is_empty() {
            out.push(Asset {
                name: name.to_string(),
                url: url.to_string(),
                size,
            });
        }
    }
    out
}

fn api_failure(status: reqwest::StatusCode, headers: &reqwest::header::HeaderMap) -> String {
    let limited = status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || (status == reqwest::StatusCode::FORBIDDEN
            && (header_value(headers, "x-ratelimit-remaining") == Some("0")
                || header_value(headers, "retry-after").is_some()));
    if !limited {
        return format!("GitHub returned HTTP {}", status.as_u16());
    }
    let wait_secs = header_value(headers, "retry-after")
        .and_then(|v| v.parse::<u64>().ok())
        .or_else(|| {
            let reset = header_value(headers, "x-ratelimit-reset")?.parse::<u64>().ok()?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .ok()?
                .as_secs();
            Some(reset.saturating_sub(now))
        });
    match wait_secs {
        Some(secs) => format!(
            "GitHub rate limit reached, try again in about {} min",
            secs.div_ceil(60).max(1)
        ),
        None => "GitHub rate limit reached, try again later".to_string(),
    }
}

fn header_value<'a>(headers: &'a reqwest::header::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
}

async fn resolve_via_html(client: &reqwest::Client) -> AppResult<Option<(String, Vec<Asset>)>> {
    let body = client
        .get(NIGHTLY_TAG_URL)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?
        .error_for_status()
        .map_err(|e| AppError::Download(e.to_string()))?
        .text()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;
    let tag = body.trim();
    if tag.is_empty()
        || tag.len() > 64
        || !tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        return Err(AppError::Download("could not identify the version".to_string()));
    }

    let assets_url =
        format!("https://github.com/ggml-org/llama.cpp/releases/expanded_assets/{tag}");
    let html = client
        .get(&assets_url)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?
        .error_for_status()
        .map_err(|e| AppError::Download(e.to_string()))?
        .text()
        .await
        .map_err(|e| AppError::Download(e.to_string()))?;

    let out = html_assets(&html, tag)?;
    if cpu_asset(&out).is_some() {
        tracing::info!("llama.cpp release {tag} selected from the nightly tag");
        Ok(Some((tag.to_string(), out)))
    } else {
        Ok(None)
    }
}

fn html_assets(html: &str, tag: &str) -> AppResult<Vec<Asset>> {
    let re = regex::Regex::new(&format!(
        r#"/releases/download/{}/([A-Za-z0-9._-]+)""#,
        regex::escape(tag)
    ))
    .map_err(|e| AppError::Download(e.to_string()))?;
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for caps in re.captures_iter(html) {
        let Some(name) = caps.get(1).map(|m| m.as_str().to_string()) else {
            continue;
        };
        if !seen.insert(name.clone()) {
            continue;
        }
        let url = format!(
            "https://github.com/ggml-org/llama.cpp/releases/download/{tag}/{name}"
        );
        out.push(Asset {
            name,
            url,
            size: 0,
        });
    }
    Ok(out)
}

fn pick<'a>(assets: &'a [Asset], must: &[&str], must_not: &[&str]) -> Option<&'a Asset> {
    assets.iter().find(|a| {
        let lower = a.name.to_ascii_lowercase();
        lower.starts_with("llama-")
            && must.iter().all(|m| lower.contains(m))
            && must_not.iter().all(|m| !lower.contains(m))
    })
}

fn cuda_version_after(lower: &str, marker: &str) -> Option<(u32, u32)> {
    let rest = lower.strip_suffix("-x64.zip")?;
    let index = rest.find(marker)?;
    let (major, minor) = rest[index + marker.len()..].split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn llama_cuda_version(name: &str) -> Option<(u32, u32)> {
    let lower = name.to_ascii_lowercase();
    if !lower.starts_with("llama-") {
        return None;
    }
    cuda_version_after(&lower, "-bin-win-cuda-")
}

fn cudart_cuda_version(name: &str) -> Option<(u32, u32)> {
    let lower = name.to_ascii_lowercase();
    let rest = lower.strip_prefix("cudart-llama-")?;
    let index = rest.find("bin-win-cuda-")?;
    let build = &rest[..index];
    let build_ok = build.is_empty()
        || build
            .strip_prefix('b')
            .and_then(|b| b.strip_suffix('-'))
            .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()));
    if !build_ok {
        return None;
    }
    cuda_version_after(&lower, "bin-win-cuda-")
}

fn cuda_package(assets: &[Asset], major: u32) -> Option<(String, Asset, Asset)> {
    let mut builds: Vec<((u32, u32), &Asset)> = assets
        .iter()
        .filter_map(|asset| llama_cuda_version(&asset.name).map(|version| (version, asset)))
        .filter(|(version, _)| version.0 == major)
        .collect();
    builds.sort_by(|a, b| b.0.cmp(&a.0));
    for (version, llama) in builds {
        if let Some(cudart) = assets
            .iter()
            .find(|asset| cudart_cuda_version(&asset.name) == Some(version))
        {
            return Some((
                format!("{}.{}", version.0, version.1),
                llama.clone(),
                cudart.clone(),
            ));
        }
    }
    None
}

fn package_for(assets: &[Asset], want: Want) -> Option<Package> {
    match want {
        Want::Cuda(major) => {
            let (version, llama, cudart) = cuda_package(assets, major)?;
            Some(Package {
                want,
                variant: format!("cuda-{version}"),
                files: vec![llama, cudart],
            })
        }
        Want::Vulkan => pick(assets, &["-bin-win-vulkan-x64.zip"], &[]).map(|asset| Package {
            want,
            variant: "vulkan".to_string(),
            files: vec![asset.clone()],
        }),
        Want::Cpu => cpu_asset(assets).map(|asset| Package {
            want,
            variant: native_variant().to_string(),
            files: vec![asset.clone()],
        }),
    }
}

fn native_variant() -> &'static str {
    if cfg!(target_os = "macos") {
        "metal"
    } else {
        "cpu"
    }
}

fn cpu_asset(assets: &[Asset]) -> Option<&Asset> {
    #[cfg(windows)]
    let asset = pick(assets, &["win-cpu", "x64", ".zip"], &[]);
    #[cfg(target_os = "macos")]
    let asset = pick(assets, &["macos-arm64"], &[]);
    #[cfg(all(not(windows), not(target_os = "macos")))]
    let asset = pick(assets, &["ubuntu", "x64"], &["cuda"]);
    asset
}

fn extract_zip(archive: &Path, out_dir: &Path) -> AppResult<()> {
    let file = std::fs::File::open(archive)?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| AppError::Download(e.to_string()))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| AppError::Download(e.to_string()))?;
        let rel = match entry.enclosed_name() {
            Some(path) => path,
            None => continue,
        };
        let out = out_dir.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut dest = std::fs::File::create(&out)?;
        std::io::copy(&mut entry, &mut dest)?;
    }
    Ok(())
}

fn extract_targz(archive: &Path, out_dir: &Path) -> AppResult<()> {
    let file = std::fs::File::open(archive)?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    tar.unpack(out_dir)
        .map_err(|e| AppError::Download(e.to_string()))?;
    Ok(())
}

fn find_binary(dir: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut dirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            dirs.push(path);
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
    }
    for sub in dirs {
        if let Some(found) = find_binary(&sub, name) {
            return Some(found);
        }
    }
    None
}

fn set_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(path) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(path, perms);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn configure_settings(state: &SharedState, model_file: Option<&str>, gpu_ready: bool) -> AppResult<()> {
    let raise_layers = gpu_ready && state.settings.read().llm_gpu_layers <= 0;
    if model_file.is_none() && !raise_layers {
        return Ok(());
    }
    state.mutate_settings(|settings| {
        apply_install(settings, model_file, gpu_ready);
        Ok(())
    })?;
    state.emit_settings_changed();
    Ok(())
}

fn apply_install(settings: &mut Settings, model_file: Option<&str>, gpu_ready: bool) {
    if let Some(model_file) = model_file {
        settings.llm_backend = LlmBackend::Local;
        settings.llm_local_model = model_file.to_string();
    }
    if gpu_ready && settings.llm_gpu_layers <= 0 {
        settings.llm_gpu_layers = DEFAULT_GPU_LAYERS;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::GpuVendor;
    use serde_json::json;

    const NAMES: [&str; 9] = [
        "cudart-llama-bin-win-cuda-12.4-x64.zip",
        "cudart-llama-bin-win-cuda-13.4-x64.zip",
        "llama-b11159-bin-macos-arm64.tar.gz",
        "llama-b11159-bin-ubuntu-x64.tar.gz",
        "llama-b11159-bin-win-cpu-arm64.zip",
        "llama-b11159-bin-win-cpu-x64.zip",
        "llama-b11159-bin-win-cuda-12.4-x64.zip",
        "llama-b11159-bin-win-cuda-13.4-arm64.zip",
        "llama-b11159-bin-win-cuda-13.4-x64.zip",
    ];

    const B11433: [&str; 22] = [
        "cudart-llama-b11433-bin-ubuntu-cuda-12.8-x64.tar.gz",
        "cudart-llama-b11433-bin-ubuntu-cuda-13.4-x64.tar.gz",
        "cudart-llama-bin-win-cuda-12.4-x64.zip",
        "cudart-llama-bin-win-cuda-13.4-arm64.zip",
        "cudart-llama-bin-win-cuda-13.4-x64.zip",
        "llama-b11433-bin-macos-arm64.tar.gz",
        "llama-b11433-bin-ubuntu-cuda-13.4-x64.tar.gz",
        "llama-b11433-bin-ubuntu-vulkan-x64.tar.gz",
        "llama-b11433-bin-ubuntu-x64.tar.gz",
        "llama-b11433-bin-win-cpu-arm64.zip",
        "llama-b11433-bin-win-cpu-x64.zip",
        "llama-b11433-bin-win-cuda-12.4-x64.zip",
        "llama-b11433-bin-win-cuda-13.4-arm64.zip",
        "llama-b11433-bin-win-cuda-13.4-x64.zip",
        "llama-b11433-bin-win-opencl-adreno-arm64.zip",
        "llama-b11433-bin-win-openvino-2026.4.1-x64.zip",
        "llama-b11433-bin-win-rocm-10.0-x64.zip",
        "llama-b11433-bin-win-sycl-x64.zip",
        "llama-b11433-bin-win-vulkan-arm64.zip",
        "llama-b11433-bin-win-vulkan-x64.zip",
        "llama-b11433-ui.tar.gz",
        "llama-b11433-xcframework.zip",
    ];

    fn assets(names: &[&str]) -> Vec<Asset> {
        names
            .iter()
            .map(|name| Asset {
                name: name.to_string(),
                url: format!("https://github.com/ggml-org/llama.cpp/releases/download/b11433/{name}"),
                size: 1,
            })
            .collect()
    }

    fn files(package: Option<Package>) -> Vec<String> {
        package
            .map(|p| p.files.into_iter().map(|a| a.name).collect())
            .unwrap_or_default()
    }

    fn release(tag: &str, draft: bool, names: &[&str]) -> serde_json::Value {
        let assets: Vec<serde_json::Value> = names
            .iter()
            .map(|name| {
                json!({
                    "name": name,
                    "browser_download_url": format!("https://github.com/ggml-org/llama.cpp/releases/download/{tag}/{name}"),
                    "size": 1u64
                })
            })
            .collect();
        json!({ "tag_name": tag, "draft": draft, "prerelease": true, "assets": assets })
    }

    fn nvidia(cc: Option<&str>, driver: Option<&str>) -> GpuInfo {
        GpuInfo {
            vendor: GpuVendor::Nvidia,
            name: "NVIDIA GeForce RTX 5070".to_string(),
            compute_cap: cc.map(str::to_string),
            driver_version: driver.map(str::to_string),
        }
    }

    fn other(vendor: GpuVendor) -> GpuInfo {
        GpuInfo {
            vendor,
            name: "Graphics".to_string(),
            compute_cap: None,
            driver_version: None,
        }
    }

    fn backend(variant: &str, devices: &[&str]) -> BackendInfo {
        BackendInfo {
            variant: variant.to_string(),
            tag: "b11433".to_string(),
            devices: devices
                .iter()
                .map(|id| BackendDevice {
                    id: id.to_string(),
                    name: "Device".to_string(),
                })
                .collect(),
            gpu_name: None,
            failed_gpu: None,
            gpu_fingerprint: None,
        }
    }

    fn device(id: &str, name: &str) -> BackendDevice {
        BackendDevice {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    fn package(want: Want, variant: &str) -> Package {
        Package {
            want,
            variant: variant.to_string(),
            files: Vec::new(),
        }
    }

    #[test]
    fn select_release_skips_release_without_binaries_and_drafts() {
        let releases = vec![
            release("v0.5.0", false, &["nightly-tag.txt"]),
            release("b11160", true, &NAMES),
            release("b11159", false, &NAMES),
        ];
        let selected = select_release(&releases, &[Want::Cpu]).map(|(tag, _)| tag);
        assert_eq!(selected.as_deref(), Some("b11159"));
        assert!(select_release(
            &[release("v0.5.0", false, &["nightly-tag.txt"])],
            &[Want::Cuda(12), Want::Cpu]
        )
        .is_none());
    }

    #[test]
    fn pick_never_returns_cudart_archive() {
        let assets = html_assets(
            &NAMES
                .iter()
                .map(|name| format!("<a href=\"/ggml-org/llama.cpp/releases/download/b11159/{name}\" rel=\"nofollow\">"))
                .collect::<String>(),
            "b11159",
        )
        .unwrap_or_default();
        assert_eq!(assets.len(), NAMES.len());
        assert!(assets.iter().any(|a| a.name == "cudart-llama-bin-win-cuda-13.4-x64.zip"));
        assert!(pick(&assets, &["cuda"], &[]).is_some_and(|a| a.name.starts_with("llama-")));
        assert!(pick(&assets, &["cudart"], &[]).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_assets_match_expected_archives() {
        let (_, assets) = select_release(
            &[release("b11159", false, &NAMES)],
            &[Want::Cuda(12), Want::Cpu],
        )
        .unwrap_or_default();
        assert_eq!(
            files(package_for(&assets, Want::Cuda(12))),
            vec![
                "llama-b11159-bin-win-cuda-12.4-x64.zip",
                "cudart-llama-bin-win-cuda-12.4-x64.zip"
            ]
        );
        assert_eq!(
            cpu_asset(&assets).map(|a| a.name.as_str()),
            Some("llama-b11159-bin-win-cpu-x64.zip")
        );
    }

    #[cfg(windows)]
    #[test]
    fn gpu_preference_prefers_complete_release_then_cpu_fallback() {
        let cpu_only = ["llama-b11161-bin-win-cpu-x64.zip"];
        let complete = release("b11159", false, &NAMES);
        let partial = release("b11161", false, &cpu_only);
        let plan = [Want::Cuda(12), Want::Vulkan, Want::Cpu];
        let selected = select_release(&[partial.clone(), complete], &plan).map(|(tag, _)| tag);
        assert_eq!(selected.as_deref(), Some("b11159"));
        let fallback = select_release(&[partial], &plan).map(|(tag, _)| tag);
        assert_eq!(fallback.as_deref(), Some("b11161"));
    }

    #[test]
    fn api_failure_reports_rate_limit() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-ratelimit-remaining", reqwest::header::HeaderValue::from_static("0"));
        headers.insert("retry-after", reqwest::header::HeaderValue::from_static("125"));
        let message = api_failure(reqwest::StatusCode::FORBIDDEN, &headers);
        assert!(message.contains("rate limit"));
        assert!(message.contains("3 min"));
        let other = api_failure(reqwest::StatusCode::NOT_FOUND, &reqwest::header::HeaderMap::new());
        assert_eq!(other, "GitHub returned HTTP 404");
    }

    #[test]
    fn build_plan_follows_the_graphics_card() {
        let cuda13 = vec![Want::Cuda(13), Want::Vulkan, Want::Cpu];
        let cuda12 = vec![Want::Cuda(12), Want::Vulkan, Want::Cpu];
        let vulkan = vec![Want::Vulkan, Want::Cpu];
        assert_eq!(windows_plan(Some(&nvidia(Some("12.0"), Some("610.88")))), cuda13);
        assert_eq!(windows_plan(Some(&nvidia(Some("7.5"), Some("580.00")))), cuda13);
        assert_eq!(windows_plan(Some(&nvidia(Some("8.6"), Some("566.36")))), cuda12);
        assert_eq!(windows_plan(Some(&nvidia(Some("7.0"), Some("610.88")))), cuda12);
        assert_eq!(windows_plan(Some(&nvidia(Some("6.1"), Some("610.88")))), cuda12);
        assert_eq!(windows_plan(Some(&nvidia(None, Some("610.88")))), cuda12);
        assert_eq!(windows_plan(Some(&nvidia(Some("6.1"), Some("551.61")))), cuda12);
        assert_eq!(windows_plan(Some(&nvidia(Some("6.1"), Some("546.33")))), vulkan);
        assert_eq!(windows_plan(Some(&nvidia(None, None))), vulkan);
        assert_eq!(windows_plan(Some(&other(GpuVendor::Amd))), vulkan);
        assert_eq!(windows_plan(Some(&other(GpuVendor::Intel))), vulkan);
        assert_eq!(windows_plan(Some(&other(GpuVendor::Other))), vulkan);
        assert_eq!(windows_plan(None), vec![Want::Cpu]);
    }

    #[test]
    fn b11433_assets_resolve_per_variant() {
        let list = assets(&B11433);
        assert_eq!(
            files(package_for(&list, Want::Cuda(13))),
            vec![
                "llama-b11433-bin-win-cuda-13.4-x64.zip",
                "cudart-llama-bin-win-cuda-13.4-x64.zip"
            ]
        );
        assert_eq!(
            package_for(&list, Want::Cuda(13)).map(|p| p.variant),
            Some("cuda-13.4".to_string())
        );
        assert_eq!(
            files(package_for(&list, Want::Cuda(12))),
            vec![
                "llama-b11433-bin-win-cuda-12.4-x64.zip",
                "cudart-llama-bin-win-cuda-12.4-x64.zip"
            ]
        );
        assert_eq!(
            files(package_for(&list, Want::Vulkan)),
            vec!["llama-b11433-bin-win-vulkan-x64.zip"]
        );
        assert!(package_for(&list, Want::Cuda(11)).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn b11433_cpu_asset_is_the_x64_zip() {
        let list = assets(&B11433);
        assert_eq!(
            files(package_for(&list, Want::Cpu)),
            vec!["llama-b11433-bin-win-cpu-x64.zip"]
        );
        assert_eq!(package_for(&list, Want::Cpu).map(|p| p.variant), Some("cpu".to_string()));
    }

    #[test]
    fn cuda_package_requires_cudart_of_the_same_version() {
        let missing = assets(&[
            "llama-b1-bin-win-cuda-13.5-x64.zip",
            "cudart-llama-bin-win-cuda-13.4-x64.zip",
        ]);
        assert!(package_for(&missing, Want::Cuda(13)).is_none());
        let older = assets(&[
            "llama-b1-bin-win-cuda-13.5-x64.zip",
            "llama-b1-bin-win-cuda-13.4-x64.zip",
            "cudart-llama-bin-win-cuda-13.4-x64.zip",
        ]);
        assert_eq!(
            files(package_for(&older, Want::Cuda(13))),
            vec!["llama-b1-bin-win-cuda-13.4-x64.zip", "cudart-llama-bin-win-cuda-13.4-x64.zip"]
        );
        let newest = assets(&[
            "llama-b1-bin-win-cuda-12.4-x64.zip",
            "llama-b1-bin-win-cuda-12.8-x64.zip",
            "cudart-llama-bin-win-cuda-12.4-x64.zip",
            "cudart-llama-b1-bin-win-cuda-12.8-x64.zip",
        ]);
        assert_eq!(
            files(package_for(&newest, Want::Cuda(12))),
            vec!["llama-b1-bin-win-cuda-12.8-x64.zip", "cudart-llama-b1-bin-win-cuda-12.8-x64.zip"]
        );
        let arm = assets(&[
            "llama-b1-bin-win-cuda-13.4-arm64.zip",
            "cudart-llama-bin-win-cuda-13.4-arm64.zip",
        ]);
        assert!(package_for(&arm, Want::Cuda(13)).is_none());
        assert!(package_for(&arm, Want::Vulkan).is_none());
        assert_eq!(cudart_cuda_version("cudart-llama-xyz-bin-win-cuda-13.4-x64.zip"), None);
    }

    #[test]
    fn list_devices_output_is_parsed() {
        let cuda = "Available devices:\n  CUDA0: NVIDIA GeForce RTX 5070 (12226 MiB, 11036 MiB free)\n";
        assert_eq!(
            parse_devices(cuda),
            vec![BackendDevice {
                id: "CUDA0".to_string(),
                name: "NVIDIA GeForce RTX 5070".to_string()
            }]
        );
        assert!(parse_devices("Available devices:\r\n  (none)\r\n").is_empty());
        let vulkan = "Available devices:\n  Vulkan0: AMD Radeon(TM) Graphics (8192 MiB, 7000 MiB free)\n  Vulkan1: Intel(R) Arc(TM) A770 Graphics\n";
        let parsed = parse_devices(vulkan);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "Vulkan0");
        assert_eq!(parsed[0].name, "AMD Radeon(TM) Graphics");
        assert_eq!(parsed[1].name, "Intel(R) Arc(TM) A770 Graphics");
        assert!(parse_devices("load_backend: loaded CPU backend\nCPU: something\nCUDA: x\nCUDAx1: y\n").is_empty());
    }

    #[test]
    fn backend_json_roundtrip_and_gpu_readiness() {
        let mut info = backend("cuda-13.4", &["CUDA0"]);
        info.gpu_name = Some("NVIDIA GeForce RTX 5070".to_string());
        let text = serde_json::to_string(&info).unwrap_or_default();
        let back: Option<BackendInfo> = serde_json::from_str(&text).ok();
        assert_eq!(back.as_ref(), Some(&info));
        let minimal: Option<BackendInfo> =
            serde_json::from_str(r#"{"variant":"cpu","tag":"b11433"}"#).ok();
        assert_eq!(minimal, Some(backend("cpu", &[])));
        assert!(info.gpu_ready());
        assert!(!backend("cuda-12.4", &[]).gpu_ready());
        assert!(!backend("cuda-12.4", &["Vulkan0"]).gpu_ready());
        assert!(backend("vulkan", &["Vulkan0"]).gpu_ready());
        assert!(!backend("vulkan", &[]).gpu_ready());
        assert!(!backend("cpu", &["CUDA0"]).gpu_ready());
        assert!(backend("metal", &[]).gpu_ready());
        assert_eq!(
            backend("vulkan", &["CUDA0", "Vulkan0"]).gpu_device_name(),
            Some("Device".to_string())
        );
    }

    #[test]
    fn backend_json_file_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "synapse-backend-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        assert!(read_backend(&dir).is_none());
        let info = backend("vulkan", &["Vulkan0"]);
        assert!(write_backend(&dir, &info).is_ok());
        assert_eq!(read_backend(&dir), Some(info));
        assert!(std::fs::write(dir.join(BACKEND_FILE), "{broken").is_ok());
        assert!(read_backend(&dir).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn repairable_decision() {
        let gpu = nvidia(Some("12.0"), Some("610.88"));
        assert!(repairable_for(Some(&gpu), true, None));
        assert!(repairable_for(Some(&gpu), true, Some(&backend("cpu", &[]))));
        assert!(repairable_for(Some(&gpu), true, Some(&backend("cuda-12.4", &[]))));
        assert!(!repairable_for(Some(&gpu), true, Some(&backend("cuda-13.4", &["CUDA0"]))));
        assert!(!repairable_for(Some(&gpu), false, None));
        assert!(!repairable_for(None, true, None));
        assert!(!repairable_for(None, true, Some(&backend("cpu", &[]))));
        let mut tried = backend("cpu", &[]);
        tried.failed_gpu = Some(gpu.fingerprint());
        assert!(!repairable_for(Some(&gpu), true, Some(&tried)));
        let updated = nvidia(Some("12.0"), Some("612.10"));
        assert!(repairable_for(Some(&updated), true, Some(&tried)));
    }

    #[cfg(windows)]
    #[test]
    fn blackwell_on_older_cuda_build_is_repairable() {
        let blackwell = nvidia(Some("12.0"), Some("610.88"));
        for variant in ["cuda-12.4", "cuda-12.8", "cuda"] {
            assert!(
                repairable_for(Some(&blackwell), true, Some(&backend(variant, &["CUDA0"]))),
                "{variant}"
            );
        }
        assert!(!repairable_for(Some(&blackwell), true, Some(&backend("cuda-13.4", &["CUDA0"]))));
        assert!(!repairable_for(Some(&blackwell), true, Some(&backend("cuda-14.0", &["CUDA0"]))));
        assert!(!repairable_for(Some(&blackwell), true, Some(&backend("vulkan", &["Vulkan0"]))));
        let ada = nvidia(Some("8.9"), Some("610.88"));
        assert!(!repairable_for(Some(&ada), true, Some(&backend("cuda-12.4", &["CUDA0"]))));
        let old_driver = nvidia(Some("12.0"), Some("572.16"));
        assert!(!repairable_for(Some(&old_driver), true, Some(&backend("cuda-12.4", &["CUDA0"]))));
        let unknown_cc = nvidia(None, Some("610.88"));
        assert!(!repairable_for(Some(&unknown_cc), true, Some(&backend("cuda-12.4", &["CUDA0"]))));
        let amd = other(GpuVendor::Amd);
        assert!(!repairable_for(Some(&amd), true, Some(&backend("vulkan", &["Vulkan0"]))));
        assert!(!repairable_for(Some(&blackwell), false, Some(&backend("cuda-12.4", &["CUDA0"]))));
    }

    #[test]
    fn cuda_major_reads_the_variant() {
        assert_eq!(cuda_major("cuda-12.4"), Some(12));
        assert_eq!(cuda_major("cuda-13.4"), Some(13));
        assert_eq!(cuda_major("cuda"), None);
        assert_eq!(cuda_major("vulkan"), None);
        assert_eq!(cuda_major("cpu"), None);
    }

    #[test]
    fn gpu_marked_failed_only_when_a_build_runs_without_a_device() {
        let gpu = nvidia(Some("12.0"), Some("610.88"));
        let cuda = package(Want::Cuda(13), "cuda-13.4");
        let vulkan = package(Want::Vulkan, "vulkan");
        let cpu = package(Want::Cpu, "cpu");
        assert_eq!(
            judge(&cuda, Err("no answer within 30 s".to_string()), "b1", Some(&gpu), false),
            Judged::Next { no_device: false }
        );
        assert_eq!(
            judge(&cuda, Ok(Vec::new()), "b1", Some(&gpu), false),
            Judged::Next { no_device: true }
        );
        assert_eq!(
            judge(&vulkan, Ok(vec![device("CUDA0", "RTX")]), "b1", Some(&gpu), false),
            Judged::Next { no_device: true }
        );
        assert_eq!(
            judge(&cpu, Err("crash".to_string()), "b1", Some(&gpu), true),
            Judged::Reject("crash".to_string())
        );
        let Judged::Accept(after_timeouts) = judge(&cpu, Ok(Vec::new()), "b1", Some(&gpu), false)
        else {
            panic!("cpu build rejected");
        };
        assert_eq!(after_timeouts.failed_gpu, None);
        assert_eq!(after_timeouts.gpu_fingerprint, Some(gpu.fingerprint()));
        let Judged::Accept(after_no_device) = judge(&cpu, Ok(Vec::new()), "b1", Some(&gpu), true)
        else {
            panic!("cpu build rejected");
        };
        assert_eq!(after_no_device.failed_gpu, Some(gpu.fingerprint()));
        let Judged::Accept(accepted) = judge(
            &cuda,
            Ok(vec![device("CUDA0", "NVIDIA GeForce RTX 5070")]),
            "b1",
            Some(&gpu),
            false,
        ) else {
            panic!("cuda build rejected");
        };
        assert_eq!(accepted.variant, "cuda-13.4");
        assert_eq!(accepted.gpu_name.as_deref(), Some("NVIDIA GeForce RTX 5070"));
        assert_eq!(accepted.failed_gpu, None);
        assert!(accepted.gpu_ready());
    }

    #[test]
    fn inferred_backend_follows_the_listed_devices() {
        let cuda = inferred_backend(vec![device("CUDA0", "NVIDIA GeForce RTX 5070")]);
        assert_eq!(cuda.variant, "cuda");
        assert_eq!(cuda.tag, INFERRED_TAG);
        assert!(cuda.gpu_ready());
        assert_eq!(cuda.gpu_name.as_deref(), Some("NVIDIA GeForce RTX 5070"));
        let vulkan = inferred_backend(vec![device("Vulkan0", "AMD Radeon")]);
        assert_eq!(vulkan.variant, "vulkan");
        assert!(vulkan.gpu_ready());
        let both = inferred_backend(vec![device("Vulkan0", "AMD"), device("CUDA0", "NVIDIA")]);
        assert_eq!(both.variant, "cuda");
        assert_eq!(both.gpu_name.as_deref(), Some("NVIDIA"));
        let none = inferred_backend(Vec::new());
        assert_eq!(none.variant, "cpu");
        assert!(!none.gpu_ready());
        assert_eq!(none.gpu_name, None);
        let gpu = nvidia(Some("12.0"), Some("610.88"));
        assert!(repairable_for(Some(&gpu), true, Some(&none)));
    }

    #[test]
    fn stale_backend_is_rechecked_with_new_devices() {
        let gpu = nvidia(Some("12.0"), Some("610.88"));
        let mut info = backend("cuda-13.4", &["CUDA0"]);
        info.gpu_fingerprint = Some(gpu.fingerprint());
        assert!(!backend_stale(&info, Some(&gpu)));
        assert!(backend_stale(&info, Some(&nvidia(Some("12.0"), Some("612.10")))));
        assert!(backend_stale(&info, None));
        assert!(backend_stale(&backend("cuda-13.4", &["CUDA0"]), Some(&gpu)));
        assert!(!backend_stale(&backend("cpu", &[]), None));
        let refreshed = refreshed_backend(info.clone(), Vec::new());
        assert_eq!(refreshed.variant, "cuda-13.4");
        assert!(!refreshed.gpu_ready());
        assert_eq!(refreshed.gpu_name, None);
        let back = refreshed_backend(refreshed, vec![device("CUDA0", "RTX 5070")]);
        assert!(back.gpu_ready());
        assert_eq!(back.gpu_name.as_deref(), Some("RTX 5070"));
        assert_eq!(back.gpu_fingerprint, info.gpu_fingerprint);
        let text = serde_json::to_string(&info).unwrap_or_default();
        let parsed: Option<BackendInfo> = serde_json::from_str(&text).ok();
        assert_eq!(parsed, Some(info));
    }

    fn scratch_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "synapse-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn stale_downloads_and_leftovers_are_pruned() {
        let root = scratch_dir("prune");
        let bin = root.join("bin");
        let setup = root.join(SETUP_DIR);
        let downloads = setup.join(DOWNLOADS_DIR);
        let old = root.join(format!("{OLD_DIR_PREFIX}123"));
        for dir in [&bin, &downloads.join("b11433"), &setup.join(STAGE_DIR), &old] {
            assert!(std::fs::create_dir_all(dir).is_ok());
        }
        let names = [
            "llama-b11433-bin-win-cuda-13.4-x64.zip",
            "llama-b11400-bin-win-cuda-13.4-x64.zip",
            "cudart-llama-bin-win-cuda-13.4-x64.zip",
            "llama-b11433-bin-win-cpu-x64.part",
        ];
        for name in names {
            assert!(std::fs::write(downloads.join(name), b"x").is_ok());
        }
        assert!(std::fs::write(bin.join(BIN_NAME), b"x").is_ok());
        cleanup_stale(&bin, None);
        assert!(!setup.join(STAGE_DIR).exists());
        assert!(!downloads.join("b11433").exists());
        assert!(!old.exists());
        assert!(!downloads.join(names[3]).exists());
        assert!(downloads.join(names[0]).exists());
        assert!(downloads.join(names[1]).exists());
        assert!(downloads.join(names[2]).exists());
        let keep: HashSet<String> = [names[0], names[2]].iter().map(|n| n.to_string()).collect();
        cleanup_stale(&bin, Some(&keep));
        assert!(downloads.join(names[0]).exists());
        assert!(!downloads.join(names[1]).exists());
        assert!(downloads.join(names[2]).exists());
        assert!(bin.join(BIN_NAME).exists());
        cleanup_stale(&root.join("missing").join("bin"), Some(&keep));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn stale_download_names() {
        let keep: HashSet<String> = ["a.zip".to_string()].into_iter().collect();
        assert!(stale_download("a.part", None));
        assert!(stale_download("A.PART", Some(&keep)));
        assert!(!stale_download("a.zip", None));
        assert!(!stale_download("a.zip", Some(&keep)));
        assert!(stale_download("b.zip", Some(&keep)));
    }

    #[test]
    fn install_keeps_the_other_provider_settings() {
        let mut settings = Settings {
            llm_backend: LlmBackend::Ollama,
            llm_endpoint: "http://localhost:11434/v1".to_string(),
            llm_model_name: "llama3.2".to_string(),
            llm_enabled: false,
            llm_timeout_ms: 2500,
            llm_temperature: 0.4,
            llm_gpu_layers: 0,
            ..Settings::default()
        };
        apply_install(&mut settings, Some("google_gemma-3-4b-it-Q4_K_M.gguf"), true);
        assert_eq!(settings.llm_backend, LlmBackend::Local);
        assert_eq!(settings.llm_local_model, "google_gemma-3-4b-it-Q4_K_M.gguf");
        assert_eq!(settings.llm_endpoint, "http://localhost:11434/v1");
        assert_eq!(settings.llm_model_name, "llama3.2");
        assert!(!settings.llm_enabled);
        assert_eq!(settings.llm_timeout_ms, 2500);
        assert!((settings.llm_temperature - 0.4).abs() < f32::EPSILON);
        assert_eq!(settings.llm_gpu_layers, DEFAULT_GPU_LAYERS);
        let mut repair = Settings {
            llm_backend: LlmBackend::Groq,
            llm_gpu_layers: 0,
            ..Settings::default()
        };
        apply_install(&mut repair, None, false);
        assert_eq!(repair.llm_backend, LlmBackend::Groq);
        assert_eq!(repair.llm_gpu_layers, 0);
        apply_install(&mut repair, None, true);
        assert_eq!(repair.llm_backend, LlmBackend::Groq);
        assert_eq!(repair.llm_gpu_layers, DEFAULT_GPU_LAYERS);
        let mut custom = Settings {
            llm_gpu_layers: 20,
            ..Settings::default()
        };
        apply_install(&mut custom, None, true);
        assert_eq!(custom.llm_gpu_layers, 20);
    }

    #[test]
    fn asset_names_are_sanitized() {
        assert!(safe_asset_name("llama-b11433-bin-win-cpu-x64.zip"));
        assert!(!safe_asset_name("../evil.zip"));
        assert!(!safe_asset_name("a/b.zip"));
        assert!(!safe_asset_name(""));
    }
}
