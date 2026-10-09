use crate::audio::AudioEngine;
use crate::cleanup::LlmClient;
use crate::config::{LlmBackend, Settings, TranscriptionBackend};
use crate::error::{AppError, AppResult};
use crate::hardware::{self, GpuInfo, GpuMark, WhisperGpu};
use crate::llama_setup::BackendInfo;
use crate::sidecar::Sidecar;
use crate::transcribe::TranscribeEngine;
use crate::vad::Vad;
use crate::{history, models};
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

pub const SETTINGS_UNREADABLE: &str = "settings_unreadable";
pub const WHISPER_GPU_DISABLED: &str = "whisper_gpu_disabled";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Idle,
    Recording,
    Processing,
    Loading,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmState {
    Off,
    Starting,
    Ready,
    Failed,
}

impl LlmState {
    pub fn as_u8(self) -> u8 {
        match self {
            LlmState::Off => 0,
            LlmState::Starting => 1,
            LlmState::Ready => 2,
            LlmState::Failed => 3,
        }
    }

    pub fn from_u8(value: u8) -> LlmState {
        match value {
            1 => LlmState::Starting,
            2 => LlmState::Ready,
            3 => LlmState::Failed,
            _ => LlmState::Off,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LocalAiStatus {
    pub state: LlmState,
    pub installed: bool,
    pub model_present: bool,
    pub variant: Option<String>,
    pub device: Option<String>,
    pub device_name: Option<String>,
    pub repairable: bool,
    pub setup_running: bool,
    pub last_error: Option<String>,
}

#[derive(Default)]
pub struct EngineMeta {
    pub loaded_model: String,
    pub on_gpu: bool,
    pub ready: bool,
    pub missing: bool,
    pub error: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct StatusPayload {
    pub status: Status,
    pub recording: bool,
    pub cpu_mode: bool,
    pub engine_ready: bool,
    pub model: String,
    pub audio_available: bool,
    pub vad_active: bool,
    pub error: Option<String>,
    pub error_code: Option<String>,
    pub starting: bool,
    pub hotkey_ready: bool,
    pub llm: LlmState,
    pub autostart_launch: bool,
    pub mic_denied: bool,
    pub accessibility_needed: bool,
}

struct WhisperGuard<'a> {
    state: &'a AppState,
}

impl Drop for WhisperGuard<'_> {
    fn drop(&mut self) {
        self.state.disarm_whisper_guard();
    }
}

pub struct AppState {
    pub app: AppHandle,
    pub config_path: PathBuf,
    pub models_dir: PathBuf,
    pub bin_dir: PathBuf,
    pub resource_dir: PathBuf,
    pub log_dir: PathBuf,
    pub guard_dir: PathBuf,
    pub custom_models: RwLock<crate::custom_models::CustomStore>,
    pub custom_models_path: PathBuf,
    pub llama_setup_running: AtomicBool,
    pub llama_restart_pending: Mutex<bool>,
    pub llama_backend: RwLock<Option<BackendInfo>>,
    pub local_ai_repairable: AtomicBool,
    pub local_ai_error: RwLock<Option<String>>,
    pub local_ai_last: Mutex<Option<LocalAiStatus>>,
    pub local_ai_files: Mutex<Option<(bool, bool)>>,
    pub gpu_fallback: Mutex<Option<PathBuf>>,
    pub llama_files: tokio::sync::Mutex<()>,
    pub settings: RwLock<Settings>,
    pub settings_io: Mutex<()>,
    pub audio: Arc<AudioEngine>,
    pub transcribe: RwLock<Option<TranscribeEngine>>,
    pub engine_meta: RwLock<EngineMeta>,
    pub engine_lock: Mutex<()>,
    pub engine_gen: AtomicU64,
    pub engine_settled: AtomicBool,
    pub whisper_guard: Mutex<u32>,
    pub whisper_gpu_recovered: AtomicBool,
    pub vad: RwLock<Option<Vad>>,
    pub llm: LlmClient,
    pub llm_state: AtomicU8,
    pub sidecar: Mutex<Option<Sidecar>>,
    pub sidecar_gen: AtomicU64,
    pub db: Mutex<Option<rusqlite::Connection>>,
    pub db_path: PathBuf,
    pub status: RwLock<Status>,
    pub recording: AtomicBool,
    pub busy: AtomicBool,
    pub sidecar_ready: AtomicBool,
    pub boot_done: AtomicBool,
    pub autostart_launch: bool,
    pub downloading: Mutex<std::collections::HashSet<String>>,
    pub widget_pos_path: PathBuf,
    pub widget_x: AtomicI32,
    pub widget_y: AtomicI32,
    pub widget_move_gen: AtomicU64,
    pub widget_ready: AtomicBool,
}

impl AppState {
    pub fn settings_snapshot(&self) -> Settings {
        self.settings.read().clone()
    }

    pub fn settings_unreadable(&self) -> bool {
        self.settings.read().transient_unreadable
    }

    pub fn mutate_settings<F>(&self, apply: F) -> AppResult<(Settings, Settings)>
    where
        F: FnOnce(&mut Settings) -> AppResult<()>,
    {
        let _io = self.settings_io.lock();
        let old = self.settings.read().clone();
        if old.transient_unreadable {
            return Err(AppError::Config(SETTINGS_UNREADABLE.to_string()));
        }
        let mut new = old.clone();
        apply(&mut new)?;
        new.save(&self.config_path)?;
        *self.settings.write() = new.clone();
        Ok((old, new))
    }

    pub fn emit_settings_changed(&self) {
        let snapshot = self.settings_snapshot();
        if let Err(err) = self.app.emit("settings-changed", snapshot) {
            tracing::warn!("settings-changed emit failed: {err}");
        }
    }

    pub fn set_llm_state(&self, next: LlmState) {
        let previous = LlmState::from_u8(self.llm_state.swap(next.as_u8(), Ordering::AcqRel));
        if previous != next {
            tracing::info!("local AI state: {previous:?} -> {next:?}");
        }
        self.emit_status();
    }

    pub fn local_llm_state(&self) -> LlmState {
        LlmState::from_u8(self.llm_state.load(Ordering::Acquire))
    }

    pub fn llm_status(&self) -> LlmState {
        let settings = self.settings.read();
        if !settings.llm_enabled && !settings.translation_enabled {
            return LlmState::Off;
        }
        match settings.llm_backend {
            LlmBackend::Local => self.local_llm_state(),
            LlmBackend::Groq => {
                if settings.groq_llm_api_key.trim().is_empty() {
                    LlmState::Off
                } else {
                    LlmState::Ready
                }
            }
            _ => {
                if settings.llm_endpoint.trim().is_empty() {
                    LlmState::Off
                } else {
                    LlmState::Ready
                }
            }
        }
    }

    fn starting(&self) -> bool {
        if self.boot_done.load(Ordering::Acquire) {
            return false;
        }
        let settled = crate::hotkey::is_settled()
            && self.audio.is_settled()
            && self.engine_settled.load(Ordering::Acquire);
        if settled && !self.boot_done.swap(true, Ordering::AcqRel) {
            tracing::info!(
                "startup settled (hotkey ready {}, audio available {}, engine ready {})",
                crate::hotkey::is_ready(),
                self.audio.is_available(),
                self.engine_meta.read().ready
            );
        }
        !settled
    }

    pub fn with_db<T, F>(&self, work: F) -> AppResult<T>
    where
        F: FnOnce(&rusqlite::Connection) -> AppResult<T>,
    {
        let mut guard = self.db.lock();
        if guard.is_none() {
            match history::open(&self.db_path) {
                Ok(conn) => {
                    tracing::info!("history database opened at {}", self.db_path.display());
                    *guard = Some(conn);
                }
                Err(err) => {
                    tracing::warn!("history database unavailable: {err}");
                    return Err(AppError::Db(format!("history unavailable: {err}")));
                }
            }
        }
        match guard.as_ref() {
            Some(conn) => work(conn),
            None => Err(AppError::Db("history unavailable".to_string())),
        }
    }

    pub fn all_models(&self) -> Vec<models::ModelInfo> {
        let mut list = models::registry();
        for custom in &self.custom_models.read().models {
            list.push(crate::custom_models::to_info(custom));
        }
        list
    }

    pub fn resolve_model(&self, id: &str) -> Option<models::ModelInfo> {
        self.all_models().into_iter().find(|m| m.id == id)
    }

    pub fn persist_custom_models(&self) -> AppResult<()> {
        self.custom_models.read().save(&self.custom_models_path)
    }

    pub fn set_status(&self, status: Status) {
        *self.status.write() = status;
        self.emit_status();
    }

    fn set_engine_status(&self, status: Status) {
        let dictating = matches!(*self.status.read(), Status::Recording | Status::Processing);
        if dictating {
            self.emit_status();
        } else {
            self.set_status(status);
        }
    }

    pub fn status_payload(&self) -> StatusPayload {
        let starting = self.starting();
        let llm = self.llm_status();
        let hotkey_ready = crate::hotkey::is_ready();
        let vad_active = self.vad.read().is_some();
        let status = *self.status.read();
        let meta = self.engine_meta.read();
        let settings = self.settings.read();
        let groq_ready = matches!(settings.transcription_backend, TranscriptionBackend::Groq)
            && !settings.groq_api_key.trim().is_empty();
        let error_code = meta.error.as_ref().map(|_| {
            if meta.missing {
                "model_missing".to_string()
            } else {
                "engine_error".to_string()
            }
        });
        StatusPayload {
            status,
            recording: self.recording.load(Ordering::Acquire),
            cpu_mode: meta.ready && !meta.on_gpu,
            engine_ready: meta.ready || groq_ready,
            model: if groq_ready {
                "Groq Cloud".to_string()
            } else {
                meta.loaded_model.clone()
            },
            audio_available: self.audio.is_available(),
            vad_active,
            error: meta.error.clone(),
            error_code,
            starting,
            hotkey_ready,
            llm,
            autostart_launch: self.autostart_launch,
            mic_denied: crate::permissions::mic_denied(),
            accessibility_needed: crate::permissions::accessibility_needed(),
        }
    }

    pub fn emit_status(&self) {
        let payload = self.status_payload();
        if let Err(err) = self.app.emit("status-changed", payload) {
            tracing::warn!("status-changed emit failed: {err}");
        }
        self.emit_local_ai_status();
    }

    pub fn local_ai_gpu(&self) -> bool {
        let layers = self.settings.read().llm_gpu_layers;
        if layers <= 0 || self.gpu_fallback_active() {
            return false;
        }
        if cfg!(target_os = "macos") {
            return true;
        }
        self.llama_backend
            .read()
            .as_ref()
            .is_some_and(BackendInfo::gpu_ready)
    }

    pub fn gpu_fallback_for(&self, model: &Path) -> bool {
        self.gpu_fallback.lock().as_deref() == Some(model)
    }

    fn gpu_fallback_active(&self) -> bool {
        let model_file = self.settings.read().llm_local_model.clone();
        self.gpu_fallback_for(&models::model_path(&self.models_dir, &model_file))
    }

    pub fn set_gpu_fallback(&self, model: Option<PathBuf>) {
        *self.gpu_fallback.lock() = model;
        self.emit_local_ai_status();
    }

    fn scan_local_ai_files(&self) -> (bool, bool) {
        let model_file = self.settings.read().llm_local_model.clone();
        let installed = self.sidecar_binary().exists();
        let model_present = std::fs::metadata(models::model_path(&self.models_dir, &model_file))
            .map(|meta| meta.len() > 1_000_000)
            .unwrap_or(false);
        *self.local_ai_files.lock() = Some((installed, model_present));
        (installed, model_present)
    }

    fn local_ai_files(&self) -> (bool, bool) {
        let cached = *self.local_ai_files.lock();
        match cached {
            Some(files) => files,
            None => self.scan_local_ai_files(),
        }
    }

    pub fn refresh_local_ai_files(&self) {
        self.scan_local_ai_files();
        self.emit_local_ai_status();
    }

    pub fn local_ai_status(&self) -> LocalAiStatus {
        let (installed, model_present) = self.local_ai_files();
        let backend = self.llama_backend.read().clone();
        let gpu = self.local_ai_gpu();
        let fallback = cfg!(any(windows, target_os = "linux")) && self.gpu_fallback_active();
        let device = if !installed {
            None
        } else if gpu {
            Some("gpu".to_string())
        } else {
            Some("cpu".to_string())
        };
        let device_name = if installed && gpu {
            backend.as_ref().and_then(|info| info.gpu_name.clone())
        } else {
            None
        };
        LocalAiStatus {
            state: self.local_llm_state(),
            installed,
            model_present,
            variant: backend.map(|info| info.variant),
            device,
            device_name,
            repairable: installed
                && (fallback || self.local_ai_repairable.load(Ordering::Acquire)),
            setup_running: self.llama_setup_running.load(Ordering::Acquire),
            last_error: self.local_ai_error.read().clone(),
        }
    }

    pub fn emit_local_ai_status(&self) {
        let mut last = self.local_ai_last.lock();
        let status = self.local_ai_status();
        if last.as_ref() == Some(&status) {
            return;
        }
        if let Err(err) = self.app.emit("local-ai-status", status.clone()) {
            tracing::warn!("local-ai-status emit failed: {err}");
        }
        *last = Some(status);
    }

    pub fn set_local_ai_error(&self, error: Option<String>) {
        *self.local_ai_error.write() = error;
        self.emit_local_ai_status();
    }

    pub fn set_llama_backend(&self, info: Option<BackendInfo>) {
        *self.llama_backend.write() = info;
        self.emit_local_ai_status();
    }

    pub fn adopt_llama_backend(
        &self,
        expected: Option<&BackendInfo>,
        info: Option<BackendInfo>,
    ) -> bool {
        {
            let mut slot = self.llama_backend.write();
            if slot.as_ref() != expected {
                return false;
            }
            *slot = info;
        }
        self.emit_local_ai_status();
        true
    }

    pub fn begin_llama_setup(&self) -> bool {
        let mut pending = self.llama_restart_pending.lock();
        if self.llama_setup_running.swap(true, Ordering::AcqRel) {
            return false;
        }
        *pending = false;
        true
    }

    pub fn defer_llama_restart(&self) -> bool {
        let mut pending = self.llama_restart_pending.lock();
        if self.llama_setup_running.load(Ordering::Acquire) {
            *pending = true;
            true
        } else {
            false
        }
    }

    pub fn finish_llama_setup(&self) -> bool {
        let mut pending = self.llama_restart_pending.lock();
        self.llama_setup_running.store(false, Ordering::Release);
        std::mem::take(&mut *pending)
    }

    fn set_engine_failure(&self, model: &str, missing: bool, message: String, status: Status) {
        *self.transcribe.write() = None;
        *self.engine_meta.write() = EngineMeta {
            loaded_model: model.to_string(),
            on_gpu: false,
            ready: false,
            missing,
            error: Some(message),
        };
        self.set_engine_status(status);
    }

    fn guard_file(&self, name: &str) -> PathBuf {
        self.guard_dir.join(name)
    }

    fn app_version(&self) -> String {
        self.app.package_info().version.to_string()
    }

    pub fn whisper_gpu_blocked(&self, gpu: Option<&GpuInfo>) -> bool {
        if !hardware::whisper_gpu_guarded() {
            return false;
        }
        let mark = hardware::read_mark(&self.guard_file(hardware::WHISPER_GPU_BLOCKED));
        hardware::mark_matches(&mark, gpu, &self.app_version())
    }

    pub fn recover_whisper_guard(&self) {
        if !hardware::whisper_gpu_guarded() {
            return;
        }
        if let Some(config_dir) = self.config_path.parent() {
            if config_dir != self.guard_dir.as_path() {
                hardware::remove_legacy_marks(config_dir);
            }
        }
        if let Err(err) = std::fs::create_dir_all(&self.guard_dir) {
            tracing::warn!("could not create {}: {err}", self.guard_dir.display());
        }
        if hardware::recover_whisper_marks(&self.guard_dir) {
            self.whisper_gpu_recovered.store(true, Ordering::Release);
        }
        crate::transcribe::set_crash_paths(
            self.guard_file(hardware::WHISPER_GPU_PENDING),
            self.guard_file(hardware::WHISPER_GPU_CRASHED),
        );
    }

    pub fn clear_whisper_pending(&self) {
        if !hardware::whisper_gpu_guarded() {
            return;
        }
        let _armed = self.whisper_guard.lock();
        crate::transcribe::mirror_armed(0);
        hardware::remove_mark(&self.guard_file(hardware::WHISPER_GPU_PENDING));
    }

    fn arm_whisper_guard(&self, mark: &GpuMark) -> Option<WhisperGuard<'_>> {
        let mut armed = self.whisper_guard.lock();
        if *armed == 0 && !hardware::write_mark(&self.guard_file(hardware::WHISPER_GPU_PENDING), mark) {
            return None;
        }
        *armed = armed.saturating_add(1);
        crate::transcribe::mirror_armed(*armed);
        Some(WhisperGuard { state: self })
    }

    fn disarm_whisper_guard(&self) {
        let mut armed = self.whisper_guard.lock();
        *armed = armed.saturating_sub(1);
        crate::transcribe::mirror_armed(*armed);
        if *armed == 0 {
            hardware::remove_mark(&self.guard_file(hardware::WHISPER_GPU_PENDING));
        }
    }

    fn note_whisper_gpu_gate(&self, gate: WhisperGpu) {
        if !self.whisper_gpu_recovered.swap(false, Ordering::AcqRel) {
            return;
        }
        if gate == WhisperGpu::Blocked {
            crate::services::spawn_whisper_gpu_notice(self.app.clone());
        } else {
            tracing::info!(
                "the recorded whisper graphics card crash belongs to another driver or app version; the graphics card is tried again"
            );
        }
    }

    fn whisper_gpu_plan(&self, prefer_gpu: bool) -> (bool, Option<GpuMark>) {
        let guarded = hardware::whisper_gpu_guarded();
        if !prefer_gpu && !guarded {
            return (false, None);
        }
        let gpu = if guarded {
            hardware::gpu_blocking(false)
        } else {
            None
        };
        let gate = hardware::whisper_gpu(gpu.as_ref(), self.whisper_gpu_blocked(gpu.as_ref()));
        hardware::decide_cuda_visibility(gate);
        if !prefer_gpu {
            return (false, None);
        }
        self.note_whisper_gpu_gate(gate);
        if !gate.allowed() {
            let card = gpu
                .as_ref()
                .map(|gpu| {
                    format!(
                        "{} (compute capability {})",
                        gpu.name,
                        gpu.compute_cap.as_deref().unwrap_or("unknown")
                    )
                })
                .unwrap_or_else(|| "none".to_string());
            tracing::info!("whisper runs on the processor: {} [graphics card: {card}]", gate.reason());
            return (false, None);
        }
        if !guarded {
            return (true, None);
        }
        (true, Some(GpuMark::new(gpu.as_ref(), &self.app_version())))
    }

    pub fn load_engine(&self, prefer_gpu: bool) -> AppResult<()> {
        let _serial = self.engine_lock.lock();
        self.set_engine_status(Status::Loading);
        let settings = self.settings_snapshot();
        let model = settings.whisper_model.clone();
        let info = match self.resolve_model(&model) {
            Some(info) => info,
            None => {
                tracing::warn!("whisper model {model} is not in the catalog");
                self.set_engine_failure(&model, true, "model not downloaded".to_string(), Status::Idle);
                return Err(AppError::Model(format!("unknown model: {model}")));
            }
        };
        let path = models::model_path(&self.models_dir, &info.filename);

        match path.try_exists() {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!("whisper model {model} not downloaded ({})", path.display());
                self.set_engine_failure(&model, true, "model not downloaded".to_string(), Status::Idle);
                return Err(AppError::Model("model not downloaded".to_string()));
            }
            Err(err) => {
                let message = format!("model file not accessible: {err}");
                tracing::warn!("whisper model {model}: {message}");
                self.set_engine_failure(&model, false, message.clone(), Status::Error);
                return Err(AppError::Io(message));
            }
        }
        if let Err(err) = std::fs::File::open(&path) {
            let message = format!("model file not readable: {err}");
            tracing::warn!("whisper model {model}: {message}");
            self.set_engine_failure(&model, false, message.clone(), Status::Error);
            return Err(AppError::Io(message));
        }

        let (mut use_gpu, guard) = self.whisper_gpu_plan(prefer_gpu);
        let armed = match guard.as_ref() {
            Some(mark) => {
                let armed = self.arm_whisper_guard(mark);
                if armed.is_none() {
                    tracing::warn!("whisper graphics card crash guard could not be armed; running whisper on the processor");
                    use_gpu = false;
                }
                armed
            }
            None => None,
        };
        let load_start = std::time::Instant::now();
        tracing::info!(
            "loading whisper model {model} (prefer_gpu {prefer_gpu}, use_gpu {use_gpu}) from {}",
            path.display()
        );
        let loaded = TranscribeEngine::load(&path, use_gpu);
        drop(armed);
        match loaded {
            Ok(engine) => {
                tracing::info!(
                    "whisper model loaded in {} ms (backend {}, on_gpu {})",
                    load_start.elapsed().as_millis(),
                    engine.backend,
                    engine.on_gpu
                );
                if engine.on_gpu {
                    *engine.gpu_check.lock() = guard;
                }
                let on_gpu = engine.on_gpu;
                *self.transcribe.write() = Some(engine);
                *self.engine_meta.write() = EngineMeta {
                    loaded_model: model,
                    on_gpu,
                    ready: true,
                    missing: false,
                    error: None,
                };
                self.set_engine_status(Status::Idle);
                Ok(())
            }
            Err(err) => {
                tracing::error!(
                    "whisper model load FAILED after {} ms: {err}",
                    load_start.elapsed().as_millis()
                );
                let missing = matches!(err, AppError::Model(_));
                self.set_engine_failure(&model, missing, err.to_string(), Status::Error);
                Err(err)
            }
        }
    }

    pub fn reload_vad(&self) {
        let settings = self.settings_snapshot();
        if !settings.vad_enabled {
            *self.vad.write() = None;
            return;
        }
        let path = self.resource_path("silero_vad.onnx");
        match Vad::load(&path) {
            Ok(vad) => *self.vad.write() = Some(vad),
            Err(err) => {
                tracing::warn!("vad load failed ({err}); continuing without vad");
                *self.vad.write() = None;
            }
        }
    }

    pub fn transcribe_blocking(
        &self,
        samples: &[f32],
        language: Option<&str>,
    ) -> AppResult<(String, bool)> {
        let guard = loop {
            if crate::pipeline::CANCEL.load(Ordering::Acquire) {
                return Ok((String::new(), false));
            }
            if let Some(guard) = self
                .transcribe
                .try_read_for(std::time::Duration::from_millis(200))
            {
                break guard;
            }
            tracing::info!("waiting for whisper engine lock (model loading)");
        };
        let engine = guard
            .as_ref()
            .ok_or_else(|| AppError::Transcribe("engine not loaded".to_string()))?;
        let logical = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let cap = if engine.on_gpu { 4 } else { 6 };
        let threads = (logical / 2).max(1).min(cap) as i32;
        let prompt = self.vocabulary_prompt();
        let check = engine.gpu_check.lock().clone();
        let armed = match check.as_ref() {
            Some(mark) => {
                let armed = self.arm_whisper_guard(mark);
                if armed.is_none() {
                    tracing::warn!("whisper graphics card crash guard could not be armed for the first GPU transcription");
                }
                armed
            }
            None => None,
        };
        let result = engine.transcribe(samples, language, threads, prompt.as_deref());
        drop(armed);
        if check.is_some()
            && result.is_ok()
            && !samples.is_empty()
            && !crate::pipeline::CANCEL.load(Ordering::Acquire)
        {
            *engine.gpu_check.lock() = None;
            hardware::remove_mark(&self.guard_file(hardware::WHISPER_GPU_STRIKES));
            tracing::info!("whisper finished its first transcription on the graphics card");
        }
        let text = result?;
        Ok((text, engine.on_gpu))
    }

    pub fn vocabulary_prompt(&self) -> Option<String> {
        let settings = self.settings.read();
        let mut terms: Vec<String> = Vec::new();
        let mut budget = 0usize;
        for term in &settings.vocabulary {
            let clean: String = term
                .chars()
                .filter(|c| *c != '\u{0}' && !c.is_control())
                .collect();
            let clean = clean.trim();
            if clean.is_empty() {
                continue;
            }
            if budget + clean.len() > 200 {
                break;
            }
            budget += clean.len() + 2;
            terms.push(clean.to_string());
            if terms.len() >= 32 {
                break;
            }
        }
        if terms.is_empty() {
            return None;
        }
        let joined = terms.join(", ");
        let prompt = match settings.language.as_str() {
            "en" => format!("The text may contain these terms: {joined}."),
            _ => format!("O texto pode conter os termos: {joined}."),
        };
        Some(prompt)
    }

    pub fn resource_path(&self, name: &str) -> PathBuf {
        let primary = self.resource_dir.join("resources").join(name);
        if primary.exists() {
            return primary;
        }
        let direct = self.resource_dir.join(name);
        if direct.exists() {
            return direct;
        }
        self.dev_resource_path(name)
    }

    pub fn sidecar_binary(&self) -> PathBuf {
        let name = if cfg!(windows) {
            "llama-server.exe"
        } else {
            "llama-server"
        };
        self.bin_dir.join(name)
    }

    fn dev_resource_path(&self, name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(name)
    }

    pub fn db_insert(&self, entry: &history::NewEntry) -> AppResult<i64> {
        self.with_db(|conn| history::insert(conn, entry))
    }

    pub fn stop_sidecar(&self) -> u64 {
        let (generation, taken) = {
            let mut slot = self.sidecar.lock();
            let generation = self.sidecar_gen.fetch_add(1, Ordering::AcqRel) + 1;
            self.sidecar_ready.store(false, Ordering::Release);
            (generation, slot.take())
        };
        if let Some(mut sidecar) = taken {
            sidecar.stop();
        }
        generation
    }

    pub fn publish_llm_state(&self, generation: u64, next: LlmState, ready: bool) -> bool {
        let previous = {
            let _slot = self.sidecar.lock();
            if self.sidecar_gen.load(Ordering::Acquire) != generation {
                return false;
            }
            self.sidecar_ready.store(ready, Ordering::Release);
            LlmState::from_u8(self.llm_state.swap(next.as_u8(), Ordering::AcqRel))
        };
        if previous != next {
            tracing::info!("local AI state: {previous:?} -> {next:?}");
        }
        self.emit_status();
        true
    }
}

pub type SharedState = Arc<AppState>;
