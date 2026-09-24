use crate::audio::AudioEngine;
use crate::cleanup::LlmClient;
use crate::config::{LlmBackend, Settings, TranscriptionBackend};
use crate::error::{AppError, AppResult};
use crate::sidecar::Sidecar;
use crate::transcribe::TranscribeEngine;
use crate::vad::Vad;
use crate::{history, models};
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

pub const SETTINGS_UNREADABLE: &str = "settings_unreadable";

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
}

pub struct AppState {
    pub app: AppHandle,
    pub config_path: PathBuf,
    pub models_dir: PathBuf,
    pub bin_dir: PathBuf,
    pub resource_dir: PathBuf,
    pub log_dir: PathBuf,
    pub custom_models: RwLock<crate::custom_models::CustomStore>,
    pub custom_models_path: PathBuf,
    pub llama_setup_running: AtomicBool,
    pub settings: RwLock<Settings>,
    pub settings_io: Mutex<()>,
    pub audio: Arc<AudioEngine>,
    pub transcribe: RwLock<Option<TranscribeEngine>>,
    pub engine_meta: RwLock<EngineMeta>,
    pub engine_lock: Mutex<()>,
    pub engine_gen: AtomicU64,
    pub engine_settled: AtomicBool,
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
        }
    }

    pub fn emit_status(&self) {
        let payload = self.status_payload();
        if let Err(err) = self.app.emit("status-changed", payload) {
            tracing::warn!("status-changed emit failed: {err}");
        }
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

        let load_start = std::time::Instant::now();
        tracing::info!(
            "loading whisper model {model} (prefer_gpu {prefer_gpu}) from {}",
            path.display()
        );
        match TranscribeEngine::load(&path, prefer_gpu) {
            Ok(engine) => {
                tracing::info!(
                    "whisper model loaded in {} ms (backend {}, on_gpu {})",
                    load_start.elapsed().as_millis(),
                    engine.backend,
                    engine.on_gpu
                );
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
        translate: bool,
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
        let text = engine.transcribe(samples, language, threads, prompt.as_deref(), translate)?;
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
        let installed = self.bin_dir.join(name);
        if installed.exists() {
            return installed;
        }
        let bundled = self
            .resource_dir
            .join("resources")
            .join("binaries")
            .join(name);
        if bundled.exists() {
            return bundled;
        }
        self.dev_resource_path(&format!("binaries/{name}"))
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
