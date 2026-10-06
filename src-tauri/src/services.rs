use crate::config::{LlmBackend, LoadOutcome, Settings, TranscriptionBackend};
use crate::error::{AppError, AppResult};
use crate::models::{self, ModelInfo, ModelKind};
use crate::sidecar::{self, Liveness, Sidecar, Startup};
use crate::state::{LlmState, SharedState};
use crate::{autostart, hotkey};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
const SIDECAR_CTX: u32 = 8192;
const PRIME_DEBOUNCE: Duration = Duration::from_millis(1500);
const PRIME_TIMEOUT: Duration = Duration::from_secs(120);
const PRIME_TEXT: &str = "<<<BEGIN_TRANSCRIPT>>>\nok\n<<<END_TRANSCRIPT>>>";
pub const LOCAL_PORT: u16 = 8123;
pub const LOCAL_ENDPOINT: &str = "http://127.0.0.1:8123/v1";
const LOCAL_MODEL_NAME: &str = "local";
const GPU_LOAD_FAILURES: u32 = 2;
const GPU_FALLBACK_NOTICE: &str = "The graphics card could not run the local AI, so it is running on the processor for now. Use Repair to reinstall the graphics files.";

static PRIME_GEN: AtomicU64 = AtomicU64::new(0);
static PRIME_STATE: parking_lot::Mutex<PrimeState> =
    parking_lot::Mutex::new(PrimeState { busy: false, again: false });

struct PrimeState {
    busy: bool,
    again: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidecarOutcome {
    Ready,
    Failed,
    Superseded,
    Off,
    Deferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Launch {
    Ready,
    LoadFailed,
    SpawnFailed,
    Superseded,
}

impl Launch {
    fn outcome(self) -> SidecarOutcome {
        match self {
            Launch::Ready => SidecarOutcome::Ready,
            Launch::LoadFailed | Launch::SpawnFailed => SidecarOutcome::Failed,
            Launch::Superseded => SidecarOutcome::Superseded,
        }
    }
}

pub fn llm_wanted(settings: &Settings) -> bool {
    settings.llm_enabled || settings.translation_enabled
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

fn sidecar_gpu_layers(state: &SharedState, settings: &Settings, model: &Path) -> i32 {
    if state.gpu_fallback_for(model) {
        0
    } else {
        settings.llm_gpu_layers.clamp(0, 999)
    }
}

fn gpu_attempt(state: &SharedState, gpu_layers: i32) -> bool {
    gpu_layers > 0 && state.local_ai_gpu()
}

fn load_failures_after(failures: u32, launch: Launch, gpu_attempt: bool) -> u32 {
    if launch == Launch::LoadFailed && gpu_attempt {
        failures.saturating_add(1)
    } else {
        0
    }
}

fn gpu_fallback_due(gpu_layers: i32, load_failures: u32) -> bool {
    gpu_layers > 0 && load_failures >= GPU_LOAD_FAILURES
}

fn off_outcome(state: &SharedState, generation: u64) -> SidecarOutcome {
    if state.publish_llm_state(generation, LlmState::Off, false) {
        SidecarOutcome::Off
    } else {
        SidecarOutcome::Superseded
    }
}

pub async fn restart_sidecar(state: &SharedState) -> SidecarOutcome {
    if state.defer_llama_restart() {
        tracing::info!("local AI restart deferred until the running setup finishes");
        return SidecarOutcome::Deferred;
    }
    let generation = state.stop_sidecar();

    let settings = state.settings_snapshot();
    state.refresh_local_ai_files();
    if !llm_wanted(&settings) || settings.llm_backend != LlmBackend::Local {
        return off_outcome(state, generation);
    }

    let (exe, model) = match local_install(state, &settings) {
        Some(paths) => paths,
        None => return off_outcome(state, generation),
    };

    let port = LOCAL_PORT;
    let gpu_layers = sidecar_gpu_layers(state, &settings, &model);
    let launch = start_sidecar(state, generation, &exe, &model, port, gpu_layers).await;
    if state.sidecar_gen.load(Ordering::Acquire) != generation {
        return SidecarOutcome::Superseded;
    }
    let watch_state = state.clone();
    tauri::async_runtime::spawn(async move {
        watch_sidecar(watch_state, generation, exe, model, port, gpu_layers, launch).await;
    });
    launch.outcome()
}

async fn start_sidecar(
    state: &SharedState,
    generation: u64,
    exe: &Path,
    model: &Path,
    port: u16,
    gpu_layers: i32,
) -> Launch {
    if !state.publish_llm_state(generation, LlmState::Starting, false) {
        return Launch::Superseded;
    }
    let log_path = state.log_dir.join("llama-server.log");
    let child = match Sidecar::spawn(exe, model, port, gpu_layers, SIDECAR_CTX, &log_path) {
        Ok(child) => child,
        Err(err) => {
            tracing::warn!("could not start llama-server: {err}");
            if state.publish_llm_state(generation, LlmState::Failed, false) {
                state.set_local_ai_error(Some(err.to_string()));
                return Launch::SpawnFailed;
            }
            return Launch::Superseded;
        }
    };
    {
        let mut slot = state.sidecar.lock();
        if state.sidecar_gen.load(Ordering::Acquire) != generation {
            drop(slot);
            drop(child);
            tracing::info!("llama-server start superseded by a newer restart");
            return Launch::Superseded;
        }
        *slot = Some(child);
    }

    let alive_state = state.clone();
    let startup = sidecar::wait_until_ready(state.llm.http(), port, SIDECAR_READY_TIMEOUT, move || {
        if alive_state.sidecar_gen.load(Ordering::Acquire) != generation {
            Liveness::Superseded
        } else if child_running(&alive_state) {
            Liveness::Alive
        } else {
            Liveness::Exited
        }
    })
    .await;

    match startup {
        Startup::Ready => {
            if !state.publish_llm_state(generation, LlmState::Ready, true) {
                return Launch::Superseded;
            }
            tracing::info!("llama-server ready on port {port} (gpu layers {gpu_layers})");
            let notice = (gpu_layers == 0 && state.gpu_fallback_for(model))
                .then(|| GPU_FALLBACK_NOTICE.to_string());
            state.set_local_ai_error(notice);
            schedule_prime(state, Duration::ZERO);
            Launch::Ready
        }
        Startup::Superseded => Launch::Superseded,
        Startup::Exited | Startup::TimedOut => {
            take_child_if_current(state, generation);
            if !state.publish_llm_state(generation, LlmState::Failed, false) {
                return Launch::Superseded;
            }
            let reason = if startup == Startup::Exited {
                "the local AI server stopped while starting (see llama-server.log)".to_string()
            } else {
                format!(
                    "the local AI server did not answer within {} s",
                    SIDECAR_READY_TIMEOUT.as_secs()
                )
            };
            tracing::warn!("llama-server did not become ready: {reason}");
            state.set_local_ai_error(Some(reason));
            Launch::LoadFailed
        }
    }
}

pub fn schedule_prime(state: &SharedState, delay: Duration) {
    let ticket = PRIME_GEN.fetch_add(1, Ordering::AcqRel) + 1;
    let state = state.clone();
    tauri::async_runtime::spawn(async move {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        if PRIME_GEN.load(Ordering::Acquire) != ticket || !prime_claim() {
            return;
        }
        loop {
            prime_local(&state).await;
            if !prime_release() {
                break;
            }
        }
    });
}

fn prime_claim() -> bool {
    let mut prime = PRIME_STATE.lock();
    if prime.busy {
        prime.again = true;
        false
    } else {
        prime.busy = true;
        prime.again = false;
        true
    }
}

fn prime_release() -> bool {
    let mut prime = PRIME_STATE.lock();
    if prime.again {
        prime.again = false;
        true
    } else {
        prime.busy = false;
        false
    }
}

async fn prime_local(state: &SharedState) {
    let settings = state.settings_snapshot();
    if !llm_wanted(&settings)
        || settings.llm_backend != LlmBackend::Local
        || state.local_llm_state() != LlmState::Ready
    {
        return;
    }
    let system = crate::cleanup::system_prompt(&settings);
    let url = format!("{LOCAL_ENDPOINT}/chat/completions");
    let body = serde_json::json!({
        "model": LOCAL_MODEL_NAME,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": PRIME_TEXT }
        ],
        "temperature": 0.0,
        "max_tokens": 1,
        "stream": false,
        "cache_prompt": true,
        "chat_template_kwargs": { "enable_thinking": false }
    });
    let started = std::time::Instant::now();
    let request = state
        .llm
        .http()
        .post(&url)
        .timeout(PRIME_TIMEOUT)
        .json(&body);
    let exchange = async {
        let response = request.send().await?;
        let status = response.status();
        response.bytes().await?;
        Ok::<reqwest::StatusCode, reqwest::Error>(status)
    };
    match tokio::time::timeout(PRIME_TIMEOUT + Duration::from_secs(5), exchange).await {
        Ok(Ok(status)) if status.is_success() => tracing::info!(
            "local AI prompt primed in {} ms",
            started.elapsed().as_millis()
        ),
        Ok(Ok(status)) => tracing::warn!(
            "local AI prompt priming returned HTTP {}",
            status.as_u16()
        ),
        Ok(Err(err)) => tracing::warn!("local AI prompt priming failed: {err}"),
        Err(_) => tracing::warn!("local AI prompt priming timed out"),
    }
}

async fn watch_sidecar(
    state: SharedState,
    generation: u64,
    exe: PathBuf,
    model: PathBuf,
    port: u16,
    mut gpu_layers: i32,
    first: Launch,
) {
    let mut ready = first == Launch::Ready;
    let mut load_failures = load_failures_after(0, first, gpu_attempt(&state, gpu_layers));
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
        if gpu_fallback_due(gpu_layers, load_failures) {
            tracing::warn!(
                "local AI server failed to load on the graphics card {load_failures} times in a row; using the processor for this session"
            );
            gpu_layers = 0;
            state.set_gpu_fallback(Some(model.clone()));
        }
        tracing::info!("restarting local AI server (attempt {restarts}, gpu layers {gpu_layers})");
        let launch = start_sidecar(&state, generation, &exe, &model, port, gpu_layers).await;
        if state.sidecar_gen.load(Ordering::Acquire) != generation {
            return;
        }
        ready = launch == Launch::Ready;
        load_failures =
            load_failures_after(load_failures, launch, gpu_attempt(&state, gpu_layers));
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
            if sidecar::health_ok(state.llm.http(), LOCAL_PORT).await {
                tracing::info!("local AI server answered the wake-up probe");
                return;
            }
            tracing::warn!("local AI server did not answer the wake-up probe; restarting");
            restart_sidecar(state).await;
        }
        LlmState::Failed => {
            restart_sidecar(state).await;
        }
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

fn sidecar_signature(settings: &Settings) -> Option<(&str, i32)> {
    if llm_wanted(settings) && settings.llm_backend == LlmBackend::Local {
        Some((settings.llm_local_model.as_str(), settings.llm_gpu_layers))
    } else {
        None
    }
}

fn llm_changed(old: &Settings, new: &Settings) -> bool {
    sidecar_signature(old) != sidecar_signature(new)
}

fn prompt_changed(old: &Settings, new: &Settings) -> bool {
    old.llm_enabled != new.llm_enabled
        || old.translation_enabled != new.translation_enabled
        || old.translation_target != new.translation_target
        || old.llm_format_paragraphs != new.llm_format_paragraphs
        || old.vocabulary != new.vocabulary
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

    if old.llm_local_model != new.llm_local_model {
        state.refresh_local_ai_files();
    }

    if llm_changed(old, new) {
        let sidecar_state = state.clone();
        tauri::async_runtime::spawn(async move {
            restart_sidecar(&sidecar_state).await;
        });
    } else if prompt_changed(old, new) {
        schedule_prime(state, PRIME_DEBOUNCE);
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
            state.refresh_local_ai_files();
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

    let leftovers_state = state.clone();
    tauri::async_runtime::spawn(async move {
        crate::llama_setup::clean_leftovers(&leftovers_state).await;
    });

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
            tauri::async_runtime::spawn(async move {
                check_local_ai(&sidecar_state).await;
            });
            return;
        }
        state.set_llm_state(LlmState::Starting);
        let deferred_generation = state.sidecar_gen.load(Ordering::Acquire);
        tauri::async_runtime::spawn(async move {
            let raised = check_local_ai(&sidecar_state).await;
            let deadline = tokio::time::Instant::now() + AUTOSTART_ENGINE_WAIT;
            while !sidecar_state.engine_settled.load(Ordering::Acquire)
                && tokio::time::Instant::now() < deadline
            {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            tokio::time::sleep(AUTOSTART_LLM_STAGGER).await;
            if !raised && sidecar_state.sidecar_gen.load(Ordering::Acquire) != deferred_generation {
                tracing::info!("autostart launch: local AI server already restarted meanwhile");
                return;
            }
            tracing::info!("autostart launch: starting local AI server after the voice engine");
            restart_sidecar(&sidecar_state).await;
        });
    } else {
        if local_wanted && local_install(state, &settings).is_some() {
            state.set_llm_state(LlmState::Starting);
        }
        tauri::async_runtime::spawn(async move {
            check_local_ai(&sidecar_state).await;
            restart_sidecar(&sidecar_state).await;
        });
    }
}

async fn check_local_ai(state: &SharedState) -> bool {
    let worker = state.clone();
    let check = tauri::async_runtime::spawn(async move {
        crate::llama_setup::startup(&worker).await
    });
    match check.await {
        Ok(raised) => raised,
        Err(err) => {
            tracing::error!("local AI startup check failed: {err}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_endpoint(endpoint: &str) -> Settings {
        Settings {
            llm_endpoint: endpoint.to_string(),
            ..Settings::default()
        }
    }

    #[test]
    fn local_endpoint_is_the_loopback_sidecar_port() {
        let url = reqwest::Url::parse(LOCAL_ENDPOINT).unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.port(), Some(LOCAL_PORT));
    }

    #[test]
    fn launch_maps_to_public_outcome() {
        assert_eq!(Launch::Ready.outcome(), SidecarOutcome::Ready);
        assert_eq!(Launch::LoadFailed.outcome(), SidecarOutcome::Failed);
        assert_eq!(Launch::SpawnFailed.outcome(), SidecarOutcome::Failed);
        assert_eq!(Launch::Superseded.outcome(), SidecarOutcome::Superseded);
    }

    #[test]
    fn gpu_fallback_needs_two_gpu_load_failures_in_a_row() {
        let run = |steps: &[(Launch, bool)]| {
            steps
                .iter()
                .fold(0, |count, (launch, gpu)| load_failures_after(count, *launch, *gpu))
        };
        let twice = run(&[(Launch::LoadFailed, true), (Launch::LoadFailed, true)]);
        assert_eq!(twice, 2);
        assert!(gpu_fallback_due(99, twice));
        assert!(!gpu_fallback_due(0, twice));
        let once = run(&[(Launch::LoadFailed, true)]);
        assert!(!gpu_fallback_due(99, once));
        let broken = run(&[
            (Launch::LoadFailed, true),
            (Launch::SpawnFailed, true),
            (Launch::LoadFailed, true),
        ]);
        assert!(!gpu_fallback_due(99, broken));
        let recovered = run(&[
            (Launch::LoadFailed, true),
            (Launch::Ready, true),
            (Launch::LoadFailed, true),
        ]);
        assert!(!gpu_fallback_due(99, recovered));
        let cpu_only = run(&[(Launch::LoadFailed, false), (Launch::LoadFailed, false)]);
        assert_eq!(cpu_only, 0);
        assert!(!gpu_fallback_due(99, cpu_only));
        let superseded = run(&[
            (Launch::LoadFailed, true),
            (Launch::Superseded, true),
            (Launch::LoadFailed, true),
        ]);
        assert!(!gpu_fallback_due(99, superseded));
    }

    #[test]
    fn sidecar_restart_ignores_the_other_provider_endpoint() {
        let mut old = with_endpoint("http://127.0.0.1:8123/v1");
        old.llm_enabled = true;
        let mut remote = old.clone();
        remote.llm_endpoint = "https://api.openai.com/v1".to_string();
        assert!(!llm_changed(&old, &remote));
        let mut ollama = old.clone();
        ollama.llm_endpoint = "http://localhost:11434/v1".to_string();
        assert!(!llm_changed(&old, &ollama));
        let mut layers = old.clone();
        layers.llm_gpu_layers = 0;
        assert!(llm_changed(&old, &layers));
    }
}
