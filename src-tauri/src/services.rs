use crate::config::LlmBackend;
use crate::sidecar::{self, Sidecar};
use crate::state::SharedState;
use crate::{hotkey, models};
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::AppHandle;

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

pub async fn restart_sidecar(state: &SharedState) {
    state.stop_sidecar();
    state.sidecar_ready.store(false, Ordering::Release);
    state.sidecar_settled.store(false, Ordering::Release);

    let settings = state.settings_snapshot();
    if (!settings.llm_enabled && !settings.translation_enabled)
        || settings.llm_backend != LlmBackend::Local
    {
        state.sidecar_settled.store(true, Ordering::Release);
        return;
    }

    let exe = state.sidecar_binary();
    if !exe.exists() {
        tracing::warn!("llama-server binary not found at {}", exe.display());
        state.sidecar_settled.store(true, Ordering::Release);
        return;
    }

    let model = models::model_path(&state.models_dir, &settings.llm_local_model);
    if !model.exists() {
        tracing::warn!("llm model not found at {}", model.display());
        state.sidecar_settled.store(true, Ordering::Release);
        return;
    }

    let port = match extract_port(&settings.llm_endpoint) {
        Some(p) => p,
        None => {
            tracing::warn!(
                "could not parse port from {}; using 8123",
                settings.llm_endpoint
            );
            8123
        }
    };

    let gpu_layers = settings.llm_gpu_layers.clamp(0, 999);
    match Sidecar::spawn(&exe, &model, port, gpu_layers, 2048) {
        Ok(child) => {
            *state.sidecar.lock() = Some(child);
            let ready =
                sidecar::wait_until_ready(state.llm.http(), port, Duration::from_secs(60)).await;
            state.sidecar_ready.store(ready, Ordering::Release);
            if ready {
                tracing::info!("llama-server ready on port {port}");
            } else {
                tracing::warn!("llama-server did not become ready in time");
            }
        }
        Err(err) => tracing::warn!("could not start llama-server: {err}"),
    }
    state.sidecar_settled.store(true, Ordering::Release);
}

pub fn bootstrap(app: &AppHandle, state: &SharedState) {
    if let Err(err) = hotkey::apply(app, state) {
        tracing::warn!("hotkey bootstrap: {err}");
    }

    state.reload_vad();

    let settings = state.settings_snapshot();
    if settings.transcription_backend == crate::config::TranscriptionBackend::Groq {
        tracing::info!("transcription backend = Groq; skipping local whisper engine load");
    } else {
        let engine_state = state.clone();
        let prefer_gpu = settings.prefer_gpu;
        tauri::async_runtime::spawn_blocking(move || {
            if let Err(err) = engine_state.load_engine(prefer_gpu) {
                tracing::warn!("engine bootstrap: {err}");
            }
        });
    }

    let sidecar_state = state.clone();
    tauri::async_runtime::spawn(async move {
        restart_sidecar(&sidecar_state).await;
    });
}

pub fn set_autostart(app: &AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    let result = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    result.map_err(|e| e.to_string())
}
