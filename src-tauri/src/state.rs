use crate::audio::AudioEngine;
use crate::cleanup::LlmClient;
use crate::config::{Settings, TranscriptionBackend};
use crate::error::{AppError, AppResult};
use crate::sidecar::Sidecar;
use crate::transcribe::TranscribeEngine;
use crate::vad::Vad;
use crate::{history, models};
use parking_lot::{Mutex, RwLock};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Idle,
    Recording,
    Processing,
    Loading,
    Error,
}

#[derive(Default)]
pub struct EngineMeta {
    pub loaded_model: String,
    pub on_gpu: bool,
    pub ready: bool,
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
}

pub struct AppState {
    pub app: AppHandle,
    pub config_path: PathBuf,
    pub models_dir: PathBuf,
    pub bin_dir: PathBuf,
    pub resource_dir: PathBuf,
    pub custom_models: RwLock<crate::custom_models::CustomStore>,
    pub custom_models_path: PathBuf,
    pub llama_setup_running: AtomicBool,
    pub settings: RwLock<Settings>,
    pub audio: Arc<AudioEngine>,
    pub transcribe: RwLock<Option<TranscribeEngine>>,
    pub engine_meta: RwLock<EngineMeta>,
    pub vad: RwLock<Option<Vad>>,
    pub llm: LlmClient,
    pub sidecar: Mutex<Option<Sidecar>>,
    pub db: Mutex<rusqlite::Connection>,
    pub status: RwLock<Status>,
    pub recording: AtomicBool,
    pub busy: AtomicBool,
    pub sidecar_ready: AtomicBool,
    pub sidecar_settled: AtomicBool,
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

    pub fn status_payload(&self) -> StatusPayload {
        let meta = self.engine_meta.read();
        let settings = self.settings.read();
        let groq_ready = matches!(settings.transcription_backend, TranscriptionBackend::Groq)
            && !settings.groq_api_key.trim().is_empty();
        StatusPayload {
            status: *self.status.read(),
            recording: self.recording.load(Ordering::Acquire),
            cpu_mode: meta.ready && !meta.on_gpu,
            engine_ready: meta.ready || groq_ready,
            model: if groq_ready {
                "Groq Cloud".to_string()
            } else {
                meta.loaded_model.clone()
            },
            audio_available: self.audio.is_available(),
            vad_active: self.vad.read().is_some(),
            error: meta.error.clone(),
        }
    }

    pub fn emit_status(&self) {
        let payload = self.status_payload();
        let _ = self.app.emit("status-changed", payload);
    }

    pub fn load_engine(&self, prefer_gpu: bool) -> AppResult<()> {
        self.set_status(Status::Loading);
        let settings = self.settings_snapshot();
        let info = self
            .resolve_model(&settings.whisper_model)
            .ok_or_else(|| AppError::Model(format!("unknown model: {}", settings.whisper_model)))?;
        let path = models::model_path(&self.models_dir, &info.filename);

        if !path.exists() {
            *self.transcribe.write() = None;
            *self.engine_meta.write() = EngineMeta {
                loaded_model: settings.whisper_model.clone(),
                on_gpu: false,
                ready: false,
                error: Some("model not downloaded".to_string()),
            };
            self.set_status(Status::Idle);
            return Err(AppError::Model("model not downloaded".to_string()));
        }

        let load_start = std::time::Instant::now();
        tracing::info!(
            "loading whisper model {} (prefer_gpu {prefer_gpu}) from {}",
            settings.whisper_model,
            path.display()
        );
        match TranscribeEngine::load(&path, prefer_gpu) {
            Ok(engine) => {
                tracing::info!(
                    "whisper model loaded in {} ms (on_gpu {})",
                    load_start.elapsed().as_millis(),
                    engine.on_gpu
                );
                let on_gpu = engine.on_gpu;
                *self.transcribe.write() = Some(engine);
                *self.engine_meta.write() = EngineMeta {
                    loaded_model: settings.whisper_model.clone(),
                    on_gpu,
                    ready: true,
                    error: None,
                };
                self.set_status(Status::Idle);
                Ok(())
            }
            Err(err) => {
                tracing::error!(
                    "whisper model load FAILED after {} ms: {err}",
                    load_start.elapsed().as_millis()
                );
                *self.transcribe.write() = None;
                *self.engine_meta.write() = EngineMeta {
                    loaded_model: settings.whisper_model.clone(),
                    on_gpu: false,
                    ready: false,
                    error: Some(err.to_string()),
                };
                self.set_status(Status::Error);
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
        let conn = self.db.lock();
        history::insert(&conn, entry)
    }

    pub fn stop_sidecar(&self) {
        if let Some(mut sidecar) = self.sidecar.lock().take() {
            sidecar.stop();
        }
    }

    pub fn persist_settings(&self) -> AppResult<()> {
        let settings = self.settings.read().clone();
        settings.save(&self.config_path)
    }
}

pub type SharedState = Arc<AppState>;
