use crate::config::{LlmBackend, LoadOutcome, Settings, TranscriptionBackend};
use crate::error::{AppError, AppResult};
use crate::models::{self, ModelInfo, ModelKind};
use crate::sidecar::{self, Sidecar};
use crate::state::{LlmState, SharedState};
use crate::{autostart, hotkey};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::AppHandle;

const ENGINE_RETRIES: u32 = 5;
const ENGINE_RETRY_MAX: Duration = Duration::from_secs(30);
const SIDECAR_READY_TIMEOUT: Duration = Duration::from_secs(60);
const SIDECAR_HEALTH_EVERY: Duration = Duration::from_secs(5);
const SIDECAR_HEALTH_FAILURES: u32 = 3;
const SIDECAR_MAX_RESTARTS: u32 = 5;
const SIDECAR_RETRY_MAX: Duration = Duration::from_secs(60);
const AUTOSTART_ENGINE_WAIT: Duration = Duration::from_secs(180);
const AUTOSTART_LLM_STAGGER: Duration = Duration::from_secs(4);
const SETTINGS_RETRY_MAX: Duration = Duration::from_secs(60);

pub fn extract_port(endpoint: &str) -> Option<u16> {
    let after = endpoint.split("//").nth(1).unwrap_or(endpoint);
    let host_port = after.split('/').next().unwrap_or(after);
    if let Some(idx) = host_port.rfind(']') {
        return host_port[idx + 1..].trim_start_matches(':').parse().ok();
    }
    let mut parts = host_port.rsplitn(2, ':');
    let last = parts.next()?;
    if parts.next().is_some() {
        last.parse().ok()
    } else {
        None
    }
}

pub fn llm_wanted(settings: &Settings) -> bool {
    settings.llm_enabled || settings.translation_enabled
}

fn sidecar_port(settings: &Settings) -> u16 {
    match extract_port(&settings.llm_endpoint) {
        Some(port) => port,
        None => {
            tracing::warn!(
                "could not parse port from {}; using 8123",
                settings.llm_endpoint
            );
            8123
        }
    }
}

fn local_install(state: &SharedState, settings: &Settings) -> Option<(PathBuf, PathBuf)> {
    let exe = state.sidecar_binary();
    if !exe.exists() {
        tracing::info!("llama-server binary not found at {}", exe.display());
        return None;
    }
    let model = models::model_path(&state.models_dir, &settings.llm_local_model);
    if !model.exists() {
        tracing::info!("llm model not found at {}", model.display());
        return None;
    }
    Some((exe, model))
}

fn take_child_if_current(state: &SharedState, generation: u64) {
    let taken = {
        let mut slot = state.sidecar.lock();
        if state.sidecar_gen.load(Ordering::Acquire) == generation {
            slot.take()
        } else {
            None
        }
    };
    drop(taken);
}

fn child_running(state: &SharedState) -> bool {
    state
        .sidecar
        .lock()
        .as_mut()
        .map(|sidecar| sidecar.is_running())
        .unwrap_or(false)
}

pub async fn restart_sidecar(state: &SharedState) {
    let generation = state.stop_sidecar();

    let settings = state.settings_snapshot();
    if !llm_wanted(&settings) || settings.llm_backend != LlmBackend::Local {
        state.publish_llm_state(generation, LlmState::Off, false);
        return;
    }

    let (exe, model) = match local_install(state, &settings) {
        Some(paths) => paths,
        None => {
            state.publish_llm_state(generation, LlmState::Off, false);
            return;
        }
    };

    let port = sidecar_port(&settings);
    let gpu_layers = settings.llm_gpu_layers.clamp(0, 999);
    let ready = start_sidecar(state, generation, &exe, &model, port, gpu_layers).await;
    if state.sidecar_gen.load(Ordering::Acquire) != generation {
        return;
    }
    let watch_state = state.clone();
    tauri::async_runtime::spawn(async move {
        watch_sidecar(watch_state, generation, exe, model, port, gpu_layers, ready).await;
    });
}

async fn start_sidecar(
    state: &SharedState,
    generation: u64,
    exe: &Path,
    model: &Path,
    port: u16,
    gpu_layers: i32,
) -> bool {
    if !state.publish_llm_state(generation, LlmState::Starting, false) {
        return false;
    }
    let log_path = state.log_dir.join("llama-server.log");
    let child = match Sidecar::spawn(exe, model, port, gpu_layers, 2048, &log_path) {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!("could not start llama-server: {err}");
            state.publish_llm_state(generation, LlmState::Failed, false);
            return false;
        }
    };
    {
        let mut slot = state.sidecar.lock();
        if state.sidecar_gen.load(Ordering::Acquire) != generation {
            drop(slot);
            drop(child);
            return false;
        }
        *slot = Some(child);
    }

    let alive_state = state.clone();
    let ready = sidecar::wait_until_ready(state.llm.http(), port, SIDECAR_READY_TIMEOUT, move || {
        alive_state.sidecar_gen.load(Ordering::Acquire) == generation && child_running(&alive_state)
    })
    .await;

    if ready {
        if !state.publish_llm_state(generation, LlmState::Ready, true) {
            return false;
        }
        tracing::info!("llama-server ready on port {port}");
        true
    } else {
        take_child_if_current(state, generation);
        if state.publish_llm_state(generation, LlmState::Failed, false) {
            tracing::warn!("llama-server did not become ready");
        }
        false
    }
}

async fn watch_sidecar(
    state: SharedState,
    generation: u64,
    exe: PathBuf,
    model: PathBuf,
    port: u16,
    gpu_layers: i32,
    mut ready: bool,
) {
    let mut failures: u32 = 0;
    let mut restarts: u32 = 0;
    let mut backoff = Duration::from_secs(2);
    loop {
        let pause = if ready { SIDECAR_HEALTH_EVERY } else { backoff };
        tokio::time::sleep(pause).await;
        if state.sidecar_gen.load(Ordering::Acquire) != generation {
            return;
        }
        if ready {
            let alive = child_running(&state);
            let healthy = alive && sidecar::health_ok(state.llm.http(), port).await;
            if state.sidecar_gen.load(Ordering::Acquire) != generation {
                return;
            }
            if healthy {
                failures = 0;
                continue;
            }
            failures = failures.saturating_add(1);
            if alive && failures < SIDECAR_HEALTH_FAILURES {
                continue;
            }
            tracing::warn!(
                "local AI server unhealthy (process alive {alive}, failed checks {failures}); restarting"
            );
            ready = false;
            failures = 0;
            take_child_if_current(&state, generation);
            if !state.publish_llm_state(generation, LlmState::Failed, false) {
                return;
            }
            continue;
        }
        if restarts >= SIDECAR_MAX_RESTARTS {
            tracing::error!(
                "local AI server failed after {restarts} restarts; waiting for a settings change, resume or manual restart"
            );
            return;
        }
        restarts = restarts.saturating_add(1);
        tracing::info!("restarting local AI server (attempt {restarts})");
        ready = start_sidecar(&state, generation, &exe, &model, port, gpu_layers).await;
        if state.sidecar_gen.load(Ordering::Acquire) != generation {
            return;
        }
        if ready {
            restarts = 0;
            backoff = Duration::from_secs(2);
        } else {
            backoff = (backoff * 2).min(SIDECAR_RETRY_MAX);
        }
    }
}

#[cfg(windows)]
pub async fn probe_sidecar(state: &SharedState) {
    let settings = state.settings_snapshot();
    if !llm_wanted(&settings) || settings.llm_backend != LlmBackend::Local {
        return;
    }
    match state.local_llm_state() {
        LlmState::Ready => {
            if sidecar::health_ok(state.llm.http(), sidecar_port(&settings)).await {
                tracing::info!("local AI server answered the wake-up probe");
                return;
            }
            tracing::warn!("local AI server did not answer the wake-up probe; restarting");
            restart_sidecar(state).await;
        }
        LlmState::Failed => restart_sidecar(state).await,
        LlmState::Off | LlmState::Starting => {}
    }
}

pub fn load_engine_supervised(state: &SharedState) {
    let generation = state.engine_gen.fetch_add(1, Ordering::AcqRel) + 1;
    let state = state.clone();
    tauri::async_runtime::spawn(async move {
        let mut delay = Duration::from_secs(2);
        let mut attempt: u32 = 0;
        loop {
            if state.engine_gen.load(Ordering::Acquire) != generation {
                return;
            }
            let worker = state.clone();
            let prefer_gpu = state.settings.read().prefer_gpu;
            let outcome =
                tokio::task::spawn_blocking(move || worker.load_engine(prefer_gpu)).await;
            state.engine_settled.store(true, Ordering::Release);
            state.emit_status();
            let retry = match outcome {
                Ok(Ok(())) => false,
                Ok(Err(AppError::Model(message))) => {
                    tracing::warn!("whisper engine not loaded: {message}");
                    false
                }
                Ok(Err(err)) => {
                    tracing::warn!("whisper engine load failed: {err}");
                    true
                }
                Err(err) => {
                    tracing::error!("whisper engine load task failed: {err}");
                    true
                }
            };
            if !retry {
                return;
            }
            attempt = attempt.saturating_add(1);
            if attempt > ENGINE_RETRIES {
                tracing::error!(
                    "whisper engine still failing after {attempt} attempts; waiting for a settings change or manual reload"
                );
                return;
            }
            tracing::info!(
                "retrying whisper engine load in {} s (retry {attempt} of {ENGINE_RETRIES})",
                delay.as_secs()
            );
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(ENGINE_RETRY_MAX);
        }
    });
}

fn sidecar_signature(settings: &Settings) -> Option<(&str, &str, i32)> {
    if llm_wanted(settings) && settings.llm_backend == LlmBackend::Local {
        Some((
            settings.llm_local_model.as_str(),
            settings.llm_endpoint.as_str(),
            settings.llm_gpu_layers,
        ))
    } else {
        None
    }
}

fn llm_changed(old: &Settings, new: &Settings) -> bool {
    sidecar_signature(old) != sidecar_signature(new)
}

pub fn apply_settings_change(app: &AppHandle, state: &SharedState, old: &Settings, new: &Settings) {
    if old.audio_device != new.audio_device {
        let latest = state.settings.read().audio_device.clone();
        state.audio.set_device(latest);
    }

    if old.hotkey_ptt != new.hotkey_ptt || old.record_mode != new.record_mode {
        if let Err(err) = hotkey::apply(app, state) {
            tracing::warn!("hotkey apply failed: {err}");
            hotkey::report_failure(app, &err.to_string());
        }
    }

    if old.vad_enabled != new.vad_enabled {
        state.reload_vad();
    }

    let switched_to_local = old.transcription_backend != new.transcription_backend
        && new.transcription_backend == TranscriptionBackend::Local;
    if old.whisper_model != new.whisper_model || old.prefer_gpu != new.prefer_gpu || switched_to_local
    {
        load_engine_supervised(state);
    }

    if old.groq_llm_api_key != new.groq_llm_api_key {
        crate::groq::spawn_refresh(state);
    }

    if llm_changed(old, new) {
        let sidecar_state = state.clone();
        tauri::async_runtime::spawn(async move {
            restart_sidecar(&sidecar_state).await;
        });
    }

    if old.ui_language != new.ui_language {
        let latest = state.settings.read().ui_language.clone();
        crate::tray::refresh(app, &latest);
    }
}

pub fn activate_model(state: &SharedState, info: &ModelInfo) -> AppResult<Settings> {
    if !models::is_present(&state.models_dir, info) {
        return Err(AppError::Model(format!("model not downloaded: {}", info.id)));
    }
    let (old, new) = state.mutate_settings(|settings| {
        match info.kind {
            ModelKind::Whisper => settings.whisper_model = info.id.clone(),
            ModelKind::Llm => settings.llm_local_model = info.filename.clone(),
        }
        Ok(())
    })?;
    tracing::info!("model activated: {} ({:?})", info.id, info.kind);
    match info.kind {
        ModelKind::Whisper => {
            let loaded = {
                let meta = state.engine_meta.read();
                meta.ready && meta.loaded_model == new.whisper_model
            };
            if old.whisper_model != new.whisper_model || !loaded {
                load_engine_supervised(state);
            }
        }
        ModelKind::Llm => {
            if new.llm_backend == LlmBackend::Local
                && (old.llm_local_model != new.llm_local_model
                    || state.local_llm_state() != LlmState::Ready)
            {
                let sidecar_state = state.clone();
                tauri::async_runtime::spawn(async move {
                    restart_sidecar(&sidecar_state).await;
                });
            }
        }
    }
    state.emit_settings_changed();
    state.emit_status();
    Ok(new)
}

fn apply_recovered_settings(
    app: &AppHandle,
    state: &SharedState,
    loaded: Option<Settings>,
    persist: bool,
) {
    let (old, new) = {
        let _io = state.settings_io.lock();
        let old = state.settings_snapshot();
        let mut new = loaded.unwrap_or_else(|| old.clone());
        new.transient_unreadable = false;
        if persist {
            if let Err(err) = new.save(&state.config_path) {
                tracing::warn!("recovered settings could not be persisted: {err}");
            }
        }
        *state.settings.write() = new.clone();
        (old, new)
    };
    tracing::info!("settings file became readable; applying it");
    apply_settings_change(app, state, &old, &new);
    autostart::reconcile(app, new.autostart);
    state.emit_settings_changed();
    state.emit_status();
}

fn spawn_settings_recovery(app: AppHandle, state: SharedState) {
    tauri::async_runtime::spawn(async move {
        let mut delay = Duration::from_secs(2);
        loop {
            tokio::time::sleep(delay).await;
            let path = state.config_path.clone();
            let outcome = tokio::task::spawn_blocking(move || Settings::load(&path)).await;
            match outcome {
                Ok(LoadOutcome::Loaded(settings, migrated)) => {
                    apply_recovered_settings(&app, &state, Some(*settings), migrated);
                    return;
                }
                Ok(LoadOutcome::Missing) => {
                    apply_recovered_settings(&app, &state, None, true);
                    return;
                }
                Ok(LoadOutcome::Corrupt) => {
                    apply_recovered_settings(&app, &state, None, false);
                    return;
                }
                Ok(LoadOutcome::Unreadable) => {
                    tracing::info!(
                        "settings still unreadable; next attempt in {} s",
                        delay.as_secs()
                    );
                }
                Err(err) => tracing::warn!("settings re-read task failed: {err}"),
            }
            delay = (delay * 2).min(SETTINGS_RETRY_MAX);
        }
    });
}

fn spawn_unreadable_notice(state: SharedState) {
    tauri::async_runtime::spawn(async move {
        for _ in 0..80 {
            if state.widget_ready.load(Ordering::Acquire) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        if state.settings_unreadable() {
            crate::pipeline::emit_error(
                &state.app,
                "settings",
                crate::state::SETTINGS_UNREADABLE,
                "The settings file is locked; running on defaults until it can be read.",
            );
        }
    });
}

pub fn bootstrap(app: &AppHandle, state: &SharedState, migrated: bool) {
    if let Err(err) = hotkey::apply(app, state) {
        tracing::warn!("hotkey bootstrap: {err}");
    }

    state.reload_vad();

    crate::groq::init(state);

    let settings = state.settings_snapshot();
    if settings.transcription_backend == TranscriptionBackend::Groq {
        tracing::info!("transcription backend = Groq; skipping local whisper engine load");
        state.engine_settled.store(true, Ordering::Release);
        state.emit_status();
    } else {
        load_engine_supervised(state);
    }

    if settings.transient_unreadable {
        spawn_settings_recovery(app.clone(), state.clone());
        spawn_unreadable_notice(state.clone());
    } else {
        autostart::reconcile(app, settings.autostart);
    }

    if migrated {
        state.emit_settings_changed();
    }

    let sidecar_state = state.clone();
    let local_wanted = llm_wanted(&settings) && settings.llm_backend == LlmBackend::Local;
    if state.autostart_launch && local_wanted {
        if local_install(state, &settings).is_none() {
            state.set_llm_state(LlmState::Off);
            return;
        }
        state.set_llm_state(LlmState::Starting);
        let deferred_generation = state.sidecar_gen.load(Ordering::Acquire);
        tauri::async_runtime::spawn(async move {
            let deadline = tokio::time::Instant::now() + AUTOSTART_ENGINE_WAIT;
            while !sidecar_state.engine_settled.load(Ordering::Acquire)
                && tokio::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            tokio::time::sleep(AUTOSTART_LLM_STAGGER).await;
            if sidecar_state.sidecar_gen.load(Ordering::Acquire) != deferred_generation {
                tracing::info!("autostart launch: local AI server already restarted meanwhile");
                return;
            }
            tracing::info!("autostart launch: starting local AI server after the voice engine");
            restart_sidecar(&sidecar_state).await;
        });
    } else {
        tauri::async_runtime::spawn(async move {
            restart_sidecar(&sidecar_state).await;
        });
    }
}
