use crate::autostart::{self, AutostartStatus};
use crate::config::Settings;
use crate::error::AppError;
use crate::history::{HistoryEntry, Stats};
use crate::models::ModelStatus;
use crate::state::{SharedState, StatusPayload, SETTINGS_UNREADABLE};
use crate::{audio, history, inject, models, permissions, pipeline, services};
use tauri::{AppHandle, Emitter, Manager, State};

const SETTINGS_SECTIONS: [&str; 5] = ["general", "voice", "ai", "dictionary", "advanced"];

fn settings_error(err: AppError) -> String {
    match err {
        AppError::Config(message) if message == SETTINGS_UNREADABLE => SETTINGS_UNREADABLE.to_string(),
        other => other.to_string(),
    }
}

fn refuse_unreadable(shared: &SharedState) -> Result<(), String> {
    if shared.settings_unreadable() {
        pipeline::emit_error(
            &shared.app,
            "settings",
            SETTINGS_UNREADABLE,
            "The settings file is locked; changes cannot be saved yet.",
        );
        return Err(SETTINGS_UNREADABLE.to_string());
    }
    Ok(())
}

#[tauri::command]
pub fn get_status(state: State<'_, SharedState>) -> StatusPayload {
    permissions::refresh_mic(&state);
    state.status_payload()
}

#[tauri::command]
pub fn get_settings(state: State<'_, SharedState>) -> Settings {
    state.settings_snapshot()
}

#[tauri::command]
pub async fn update_settings(
    app: AppHandle,
    state: State<'_, SharedState>,
    patch: serde_json::Value,
) -> Result<Settings, String> {
    let shared = state.inner().clone();
    refuse_unreadable(&shared)?;
    let patch = match patch {
        serde_json::Value::Object(map) => map,
        _ => return Err("invalid settings patch: expected an object".to_string()),
    };
    let worker = shared.clone();
    let (old, new) = tokio::task::spawn_blocking(move || {
        worker.mutate_settings(|settings| {
            *settings = settings.merged_with(&patch)?;
            Ok(())
        })
    })
    .await
    .map_err(|e| format!("settings update task failed: {e}"))?
    .map_err(settings_error)?;

    services::apply_settings_change(&app, &shared, &old, &new);
    if old.autostart != new.autostart {
        let desired = shared.settings.read().autostart;
        if let Err(err) = autostart::apply(&app, desired) {
            tracing::warn!("autostart change not applied: {err}");
        }
    }
    if old.auto_update != new.auto_update {
        crate::updater::auto_changed(&app, new.auto_update);
    }

    shared.emit_settings_changed();
    shared.emit_status();
    Ok(new)
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
    state
        .with_db(|conn| history::list(conn, limit.clamp(1, 500), offset.max(0)))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn search_history(
    state: State<'_, SharedState>,
    query: String,
    limit: i64,
) -> Result<Vec<HistoryEntry>, String> {
    state
        .with_db(|conn| history::search(conn, &query, limit.clamp(1, 500)))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_history(state: State<'_, SharedState>, id: i64) -> Result<(), String> {
    state
        .with_db(|conn| history::delete(conn, id))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn clear_history(state: State<'_, SharedState>) -> Result<(), String> {
    state
        .with_db(|conn| history::clear(conn))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_stats(state: State<'_, SharedState>) -> Result<Stats, String> {
    state
        .with_db(|conn| history::stats(conn))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn recopy(state: State<'_, SharedState>, id: i64) -> Result<(), String> {
    let entry = state
        .with_db(|conn| history::get(conn, id))
        .map_err(|e| e.to_string())?;
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
    let shared = state.inner().clone();
    refuse_unreadable(&shared)?;
    shared
        .mutate_settings(|settings| {
            settings.dictionary.insert(phrase, replacement);
            Ok(())
        })
        .map_err(settings_error)?;
    shared.emit_settings_changed();
    Ok(())
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
    activate: bool,
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
        match models::download(&app_handle, &models_dir, &info).await {
            Ok(()) => {
                tracing::info!("model {} downloaded", info.id);
                if activate {
                    if let Err(err) = services::activate_model(&shared, &info) {
                        tracing::warn!("downloaded model {} not activated: {err}", info.id);
                    }
                }
            }
            Err(err) => {
                tracing::warn!("model {} download failed: {err}", info.id);
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
        }
    });
    Ok(())
}

#[tauri::command]
pub fn download_model(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
    activate: bool,
) -> Result<(), String> {
    let info = state
        .resolve_model(&id)
        .ok_or_else(|| format!("unknown model: {id}"))?;
    spawn_download(app, state.inner().clone(), info, activate)
}

#[tauri::command]
pub async fn activate_model(
    state: State<'_, SharedState>,
    id: String,
) -> Result<Settings, String> {
    let shared = state.inner().clone();
    refuse_unreadable(&shared)?;
    let info = shared
        .resolve_model(&id)
        .ok_or_else(|| format!("unknown model: {id}"))?;
    services::activate_model(&shared, &info).map_err(settings_error)
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
        if let Err(err) = spawn_download(app, state.inner().clone(), info.clone(), false) {
            tracing::warn!("custom model download not started: {err}");
        }
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

    let current = shared.settings_snapshot();
    let reload_whisper =
        info.kind == models::ModelKind::Whisper && current.whisper_model == id;
    let restart_llm =
        info.kind == models::ModelKind::Llm && current.llm_local_model == info.filename;
    if reload_whisper || restart_llm {
        shared
            .mutate_settings(|settings| {
                if reload_whisper {
                    settings.whisper_model = present
                        .iter()
                        .find(|m| m.kind == models::ModelKind::Whisper && m.id == "large-v3-turbo")
                        .or_else(|| present.iter().find(|m| m.kind == models::ModelKind::Whisper))
                        .map(|m| m.id.clone())
                        .unwrap_or_else(|| "large-v3-turbo".to_string());
                }
                if restart_llm {
                    settings.llm_local_model = present
                        .iter()
                        .find(|m| m.kind == models::ModelKind::Llm && m.filename == "google_gemma-3-4b-it-Q4_K_M.gguf")
                        .or_else(|| present.iter().find(|m| m.kind == models::ModelKind::Llm))
                        .map(|m| m.filename.clone())
                        .unwrap_or_else(|| "google_gemma-3-4b-it-Q4_K_M.gguf".to_string());
                }
                Ok(())
            })
            .map_err(settings_error)?;
        shared.emit_settings_changed();
    }
    if reload_whisper {
        services::load_engine_supervised(&shared);
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
pub async fn groq_models(
    state: State<'_, SharedState>,
    refresh: bool,
) -> Result<crate::groq::GroqModels, String> {
    let shared = state.inner().clone();
    if refresh {
        Ok(crate::groq::refresh(&shared).await)
    } else {
        Ok(crate::groq::cached())
    }
}

#[tauri::command]
pub fn autostart_status(app: AppHandle) -> AutostartStatus {
    autostart::status(&app)
}

#[tauri::command]
pub async fn set_autostart(
    app: AppHandle,
    state: State<'_, SharedState>,
    enabled: bool,
) -> Result<AutostartStatus, String> {
    let shared = state.inner().clone();
    refuse_unreadable(&shared)?;
    shared
        .mutate_settings(|settings| {
            settings.autostart = enabled;
            Ok(())
        })
        .map_err(settings_error)?;
    let desired = shared.settings.read().autostart;
    let applied = autostart::apply(&app, desired);
    shared.emit_settings_changed();
    let mut status = autostart::status(&app);
    if let Err(err) = applied {
        status.error = Some(err);
    }
    Ok(status)
}

#[tauri::command]
pub fn open_settings(app: AppHandle, section: Option<String>) -> Result<(), String> {
    let window = app
        .get_webview_window("settings")
        .ok_or_else(|| "settings window unavailable".to_string())?;
    if let Err(err) = window.show() {
        tracing::warn!("settings window show failed: {err}");
    }
    if let Err(err) = window.unminimize() {
        tracing::debug!("settings window unminimize failed: {err}");
    }
    if let Err(err) = window.set_focus() {
        tracing::debug!("settings window focus failed: {err}");
    }
    if let Some(section) = section {
        if !SETTINGS_SECTIONS.contains(&section.as_str()) {
            tracing::warn!("unknown settings section requested: {section}");
        }
        app.emit_to("settings", "settings-navigate", section)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
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

#[tauri::command]
pub async fn open_privacy_settings(kind: String) -> Result<(), String> {
    permissions::open_privacy_settings(&kind)
}
