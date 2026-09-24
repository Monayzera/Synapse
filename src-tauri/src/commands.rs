use crate::config::Settings;
use crate::history::{HistoryEntry, Stats};
use crate::models::ModelStatus;
use crate::state::{SharedState, StatusPayload};
use crate::{audio, history, hotkey, inject, models, pipeline, services};
use tauri::{AppHandle, Emitter, Manager, State};

#[tauri::command]
pub fn get_status(state: State<'_, SharedState>) -> StatusPayload {
    state.status_payload()
}

#[tauri::command]
pub fn get_settings(state: State<'_, SharedState>) -> Settings {
    state.settings_snapshot()
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    state: State<'_, SharedState>,
    settings: Settings,
) -> Result<(), String> {
    let shared = state.inner().clone();
    let old = shared.settings_snapshot();
    *shared.settings.write() = settings.clone();
    shared.persist_settings().map_err(|e| e.to_string())?;

    if old.audio_device != settings.audio_device {
        shared.audio.set_device(settings.audio_device.clone());
    }

    if old.hotkey_ptt != settings.hotkey_ptt
        || old.hotkey_toggle != settings.hotkey_toggle
        || old.record_mode != settings.record_mode
    {
        if let Err(err) = hotkey::apply(&app, &shared) {
            let _ = app.emit("pipeline-error", err.to_string());
        }
    }

    if old.vad_enabled != settings.vad_enabled {
        shared.reload_vad();
    }

    let switched_to_local = old.transcription_backend != settings.transcription_backend
        && settings.transcription_backend == crate::config::TranscriptionBackend::Local;
    if old.whisper_model != settings.whisper_model
        || old.prefer_gpu != settings.prefer_gpu
        || switched_to_local
    {
        let engine_state = shared.clone();
        let prefer = settings.prefer_gpu;
        let outcome = tokio::task::spawn_blocking(move || engine_state.load_engine(prefer)).await;
        let failure = match outcome {
            Ok(Ok(())) => None,
            Ok(Err(err)) => Some(err.to_string()),
            Err(err) => Some(err.to_string()),
        };
        if let Some(message) = failure {
            let _ = app.emit(
                "pipeline-error",
                serde_json::json!({ "stage": "engine", "message": message }),
            );
        }
    }

    if llm_changed(&old, &settings) {
        let sidecar_state = shared.clone();
        tauri::async_runtime::spawn(async move {
            services::restart_sidecar(&sidecar_state).await;
        });
    }

    if old.autostart != settings.autostart {
        let _ = services::set_autostart(&app, settings.autostart);
    }

    shared.emit_status();
    Ok(())
}

fn llm_changed(old: &Settings, new: &Settings) -> bool {
    old.llm_enabled != new.llm_enabled
        || old.translation_enabled != new.translation_enabled
        || old.llm_backend != new.llm_backend
        || old.llm_local_model != new.llm_local_model
        || old.llm_endpoint != new.llm_endpoint
}

#[tauri::command]
pub fn list_audio_devices() -> Vec<String> {
    audio::list_devices()
}

#[tauri::command]
pub fn toggle_recording(app: AppHandle, state: State<'_, SharedState>) {
    pipeline::toggle_recording(app, state.inner().clone());
}

#[tauri::command]
pub fn cancel_recording(state: State<'_, SharedState>) {
    pipeline::cancel_recording(state.inner());
}

#[tauri::command]
pub fn get_history(
    state: State<'_, SharedState>,
    limit: i64,
    offset: i64,
) -> Result<Vec<HistoryEntry>, String> {
    let conn = state.db.lock();
    history::list(&conn, limit.clamp(1, 500), offset.max(0)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn search_history(
    state: State<'_, SharedState>,
    query: String,
    limit: i64,
) -> Result<Vec<HistoryEntry>, String> {
    let conn = state.db.lock();
    history::search(&conn, &query, limit.clamp(1, 500)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_history(state: State<'_, SharedState>, id: i64) -> Result<(), String> {
    let conn = state.db.lock();
    history::delete(&conn, id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_history(state: State<'_, SharedState>) -> Result<(), String> {
    let conn = state.db.lock();
    history::clear(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_stats(state: State<'_, SharedState>) -> Result<Stats, String> {
    let conn = state.db.lock();
    history::stats(&conn).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn recopy(state: State<'_, SharedState>, id: i64) -> Result<(), String> {
    let entry = {
        let conn = state.db.lock();
        history::get(&conn, id).map_err(|e| e.to_string())?
    };
    let entry = entry.ok_or_else(|| "entry not found".to_string())?;
    inject::copy_to_clipboard(&entry.final_text).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn add_to_dictionary(
    state: State<'_, SharedState>,
    phrase: String,
    replacement: String,
) -> Result<(), String> {
    if phrase.trim().is_empty() {
        return Err("phrase cannot be empty".to_string());
    }
    {
        let mut settings = state.settings.write();
        settings.dictionary.insert(phrase, replacement);
    }
    state.persist_settings().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn model_statuses(state: State<'_, SharedState>) -> Vec<ModelStatus> {
    state
        .all_models()
        .into_iter()
        .map(|info| models::status_for(&state.models_dir, &info))
        .collect()
}

struct DownloadGuard {
    state: SharedState,
    id: String,
}

impl Drop for DownloadGuard {
    fn drop(&mut self) {
        self.state.downloading.lock().remove(&self.id);
    }
}

fn spawn_download(
    app: AppHandle,
    shared: SharedState,
    info: models::ModelInfo,
) -> Result<(), String> {
    {
        let mut active = shared.downloading.lock();
        if active.contains(&info.id) {
            return Err("download in progress".to_string());
        }
        active.insert(info.id.clone());
    }
    let models_dir = shared.models_dir.clone();
    let app_handle = app.clone();
    let guard = DownloadGuard {
        state: shared.clone(),
        id: info.id.clone(),
    };
    tauri::async_runtime::spawn(async move {
        let _guard = guard;
        if let Err(err) = models::download(&app_handle, &models_dir, &info).await {
            let _ = app_handle.emit(
                "model-download-progress",
                serde_json::json!({
                    "id": info.id,
                    "downloaded": 0,
                    "total": info.size_bytes,
                    "pct": 0.0,
                    "done": false,
                    "error": err.to_string(),
                }),
            );
        }
    });
    Ok(())
}

#[tauri::command]
pub fn download_model(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
) -> Result<(), String> {
    let info = state
        .resolve_model(&id)
        .ok_or_else(|| format!("unknown model: {id}"))?;
    spawn_download(app, state.inner().clone(), info)
}

#[tauri::command]
pub fn add_custom_model(
    app: AppHandle,
    state: State<'_, SharedState>,
    label: String,
    kind: models::ModelKind,
    filename: String,
    url: String,
    size_bytes: u64,
    download_now: bool,
) -> Result<models::ModelInfo, String> {
    let filename = crate::custom_models::sanitize_filename(&filename)
        .ok_or_else(|| "invalid file name".to_string())?;
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err("empty link".to_string());
    }
    let label = if label.trim().is_empty() {
        filename.clone()
    } else {
        label.trim().to_string()
    };
    if models::registry().iter().any(|m| m.filename == filename) {
        return Err("a recommended model already uses this file".to_string());
    }
    let id = crate::custom_models::make_id(&filename, &url);
    {
        let store = state.custom_models.read();
        if store.find(&id).is_some() || store.has_filename(&filename) {
            return Err("this model was already added".to_string());
        }
    }
    let model = crate::custom_models::CustomModel {
        id,
        label,
        kind,
        filename,
        url,
        size_bytes,
    };
    state.custom_models.write().upsert(model.clone());
    state.persist_custom_models().map_err(|e| e.to_string())?;
    let info = crate::custom_models::to_info(&model);
    let _ = app.emit("models-changed", ());
    if download_now {
        let _ = spawn_download(app, state.inner().clone(), info.clone());
    }
    Ok(info)
}

#[tauri::command]
pub async fn delete_model(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
) -> Result<(), String> {
    let shared = state.inner().clone();
    let info = shared
        .resolve_model(&id)
        .ok_or_else(|| format!("unknown model: {id}"))?;

    if shared.downloading.lock().contains(&id) {
        return Err("download in progress".to_string());
    }

    let path = models::model_path(&shared.models_dir, &info.filename);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("part"));

    if id.starts_with("custom-") {
        shared.custom_models.write().remove(&id);
        shared.persist_custom_models().map_err(|e| e.to_string())?;
    }

    let present: Vec<models::ModelInfo> = shared
        .all_models()
        .into_iter()
        .filter(|m| m.id != id && models::is_present(&shared.models_dir, m))
        .collect();

    let mut reload_whisper = false;
    let mut restart_llm = false;
    {
        let mut settings = shared.settings.write();
        if info.kind == models::ModelKind::Whisper && settings.whisper_model == id {
            settings.whisper_model = present
                .iter()
                .find(|m| m.kind == models::ModelKind::Whisper && m.id == "large-v3-turbo")
                .or_else(|| present.iter().find(|m| m.kind == models::ModelKind::Whisper))
                .map(|m| m.id.clone())
                .unwrap_or_else(|| "large-v3-turbo".to_string());
            reload_whisper = true;
        }
        if info.kind == models::ModelKind::Llm && settings.llm_local_model == info.filename {
            settings.llm_local_model = present
                .iter()
                .find(|m| m.kind == models::ModelKind::Llm && m.filename == "google_gemma-3-4b-it-Q4_K_M.gguf")
                .or_else(|| present.iter().find(|m| m.kind == models::ModelKind::Llm))
                .map(|m| m.filename.clone())
                .unwrap_or_else(|| "google_gemma-3-4b-it-Q4_K_M.gguf".to_string());
            restart_llm = true;
        }
    }
    if reload_whisper || restart_llm {
        shared.persist_settings().map_err(|e| e.to_string())?;
    }
    if reload_whisper {
        let engine_state = shared.clone();
        let prefer = engine_state.settings_snapshot().prefer_gpu;
        let _ = tokio::task::spawn_blocking(move || engine_state.load_engine(prefer)).await;
    }
    if restart_llm {
        services::restart_sidecar(&shared).await;
    }

    let _ = app.emit("models-changed", ());
    Ok(())
}

#[tauri::command]
pub fn setup_llama_auto(app: AppHandle, state: State<'_, SharedState>) -> Result<(), String> {
    crate::llama_setup::launch(app, state.inner().clone());
    Ok(())
}

#[derive(serde::Serialize)]
pub struct LlamaStatus {
    binary: bool,
    model_present: bool,
    ready: bool,
}

#[tauri::command]
pub fn llama_status(state: State<'_, SharedState>) -> LlamaStatus {
    let binary = state.sidecar_binary().exists();
    let model_present = state
        .all_models()
        .into_iter()
        .filter(|m| m.kind == models::ModelKind::Llm)
        .any(|m| {
            let path = models::model_path(&state.models_dir, &m.filename);
            std::fs::metadata(&path)
                .map(|meta| meta.len() > 1_000_000)
                .unwrap_or(false)
        });
    let ready = state
        .sidecar_ready
        .load(std::sync::atomic::Ordering::Acquire);
    LlamaStatus {
        binary,
        model_present,
        ready,
    }
}

#[tauri::command]
pub async fn reload_engine(state: State<'_, SharedState>) -> Result<(), String> {
    let shared = state.inner().clone();
    let prefer = shared.settings_snapshot().prefer_gpu;
    tokio::task::spawn_blocking(move || shared.load_engine(prefer))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn restart_llm(state: State<'_, SharedState>) -> Result<(), String> {
    let shared = state.inner().clone();
    services::restart_sidecar(&shared).await;
    Ok(())
}

#[tauri::command]
pub async fn test_llm(state: State<'_, SharedState>) -> Result<String, String> {
    let shared = state.inner().clone();
    let settings = shared.settings_snapshot();
    shared.llm.test(&settings).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    services::set_autostart(&app, enabled)
}

#[tauri::command]
pub fn open_window(app: AppHandle, label: String) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
    Ok(())
}

#[tauri::command]
pub fn hide_window(app: AppHandle, label: String) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.hide();
    }
    Ok(())
}
