mod atomic_io;
mod audio;
mod cleanup;
mod commands;
mod config;
mod custom_models;
mod dictionary;
mod error;
mod groq;
mod hardware;
mod hf;
mod history;
mod hotkey;
mod llama_setup;
#[cfg(windows)]
mod inputhook;
#[cfg(target_os = "macos")]
mod inputhook_mac;
mod inject;
mod models;
mod pipeline;
mod services;
mod sidecar;
mod sound;
mod state;
mod transcribe;
mod tray;
mod vad;
mod widget_pos;

use crate::audio::AudioEngine;
use crate::cleanup::LlmClient;
use crate::config::{LoadOutcome, Settings};
use crate::state::{AppState, EngineMeta, SharedState, Status};
use parking_lot::{Mutex, RwLock};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_autostart::MacosLauncher;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    init_tracing();
    whisper_rs::install_logging_hooks();

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            show_window(app, "widget");
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![]),
        ))
        .setup(|app| {
            if let Err(err) = setup(app) {
                tracing::error!("setup failed: {err}");
                return Err(err);
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            WindowEvent::Moved(pos) => {
                if window.label() == "widget" {
                    if let Some(state) = window.app_handle().try_state::<SharedState>() {
                        if state.widget_ready.load(Ordering::Acquire) {
                            widget_pos::record_move(&state, pos.x, pos.y);
                        }
                    }
                }
            }
            _ => {}
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_status,
            commands::get_settings,
            commands::save_settings,
            commands::list_audio_devices,
            commands::toggle_recording,
            commands::cancel_recording,
            commands::get_history,
            commands::search_history,
            commands::delete_history,
            commands::clear_history,
            commands::get_stats,
            commands::recopy,
            commands::add_to_dictionary,
            commands::model_statuses,
            commands::download_model,
            commands::reload_engine,
            commands::restart_llm,
            commands::test_llm,
            commands::set_autostart,
            commands::open_window,
            commands::hide_window,
            commands::add_custom_model,
            commands::delete_model,
            commands::setup_llama_auto,
            commands::llama_status,
            hardware::hardware_info,
            hf::hf_detect,
            hf::hf_list_files
        ])
        .build(tauri::generate_context!())
        .expect("error while running Synapse")
        .run(|handle, event| {
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                if let Some(state) = handle.try_state::<SharedState>() {
                    widget_pos::flush(&state);
                    state.stop_sidecar();
                }
            }
        });
}

fn setup(app: &mut tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();

    let base_dir = resolve_base_dir(&handle);
    let config_dir = base_dir.join("config");
    let data_dir = base_dir.join("data");
    let models_dir = data_dir.join("models");
    let bin_dir = data_dir.join("bin");
    let _ = std::fs::create_dir_all(&config_dir);
    let _ = std::fs::create_dir_all(&models_dir);
    let _ = std::fs::create_dir_all(&bin_dir);

    migrate_legacy_data(&handle, &config_dir, &data_dir, &models_dir);

    let resource_dir = handle
        .path()
        .resource_dir()
        .unwrap_or_else(|_| PathBuf::from("."));
    let config_path = config_dir.join("settings.json");
    let widget_pos_path = config_dir.join("widget.json");
    let (widget_x, widget_y) = widget_pos::load(&widget_pos_path).unwrap_or((0, 0));
    let custom_models_path = config_dir.join("custom_models.json");
    let custom_store = custom_models::CustomStore::load(&custom_models_path);

    let settings = match Settings::load(&config_path) {
        LoadOutcome::Loaded(settings) => *settings,
        LoadOutcome::Missing => {
            let settings = Settings::default();
            if let Err(err) = settings.save(&config_path) {
                tracing::warn!("failed to write initial settings: {err}");
            }
            settings
        }
        LoadOutcome::Corrupt => {
            tracing::error!(
                "settings.json contained invalid data and no usable backup; running on in-memory defaults WITHOUT overwriting disk (a timestamped .corrupt copy was saved)"
            );
            Settings::default()
        }
        LoadOutcome::Unreadable => {
            tracing::error!(
                "settings present on disk but unreadable at startup (boot-time AV/cloud lock?); using in-memory defaults and REFUSING to persist so the real config is never clobbered"
            );
            let mut settings = Settings::default();
            settings.transient_unreadable = true;
            settings
        }
    };

    prepend_dll_dirs(&resource_dir);

    let audio = AudioEngine::new(settings.audio_device.clone());

    let db_path = data_dir.join("history.db");
    let connection = history::open(&db_path)?;

    let state: SharedState = Arc::new(AppState {
        app: handle.clone(),
        config_path,
        models_dir,
        bin_dir,
        resource_dir,
        custom_models: RwLock::new(custom_store),
        custom_models_path,
        llama_setup_running: AtomicBool::new(false),
        settings: RwLock::new(settings),
        audio,
        transcribe: RwLock::new(None),
        engine_meta: RwLock::new(EngineMeta::default()),
        vad: RwLock::new(None),
        llm: LlmClient::new(),
        sidecar: Mutex::new(None),
        db: Mutex::new(connection),
        status: RwLock::new(Status::Idle),
        recording: AtomicBool::new(false),
        busy: AtomicBool::new(false),
        sidecar_ready: AtomicBool::new(false),
        sidecar_settled: AtomicBool::new(false),
        downloading: Mutex::new(std::collections::HashSet::new()),
        widget_pos_path,
        widget_x: AtomicI32::new(widget_x),
        widget_y: AtomicI32::new(widget_y),
        widget_move_gen: AtomicU64::new(0),
        widget_ready: AtomicBool::new(false),
    });

    app.manage(state.clone());

    #[cfg(windows)]
    inputhook::start(handle.clone());

    #[cfg(target_os = "macos")]
    inputhook_mac::start(handle.clone());

    tray::build(&handle)?;

    let pos_handle = handle.clone();
    let ready_state = state.clone();
    tauri::async_runtime::spawn(async move {
        for _ in 0..50 {
            let ready = pos_handle
                .get_webview_window("widget")
                .and_then(|w| w.available_monitors().ok())
                .map(|m| !m.is_empty())
                .unwrap_or(false);
            if ready {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        position_widget(&pos_handle);
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        ready_state.widget_ready.store(true, Ordering::Release);
    });

    #[cfg(windows)]
    round_window_corners(&handle);

    spawn_level_emitter(state.clone());

    services::bootstrap(&handle, &state);

    Ok(())
}

fn spawn_level_emitter(state: SharedState) {
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if state.recording.load(Ordering::Acquire) {
                let _ = state.app.emit("audio-level", state.audio.level());
            }
        }
    });
}

#[cfg(windows)]
fn round_window_corners(app: &tauri::AppHandle) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };

    for label in ["settings", "history"] {
        if let Some(window) = app.get_webview_window(label) {
            if let Ok(hwnd) = window.hwnd() {
                let preference = DWMWCP_ROUND;
                unsafe {
                    let _ = DwmSetWindowAttribute(
                        windows::Win32::Foundation::HWND(hwnd.0 as *mut core::ffi::c_void),
                        DWMWA_WINDOW_CORNER_PREFERENCE,
                        &preference as *const _ as *const core::ffi::c_void,
                        std::mem::size_of_val(&preference) as u32,
                    );
                }
            }
        }
    }
}

fn show_window(app: &tauri::AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn position_widget(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("widget") {
        if let Some(state) = app.try_state::<SharedState>() {
            if let Some((x, y)) = widget_pos::load(&state.widget_pos_path) {
                if let Some((cx, cy)) = clamp_into_view(&window, x, y) {
                    let _ = window.set_position(tauri::PhysicalPosition { x: cx, y: cy });
                    return;
                }
            }
        }
        if let Ok(Some(monitor)) = window.current_monitor() {
            let monitor_size = monitor.size();
            let monitor_pos = monitor.position();
            let scale = monitor.scale_factor();
            let widget_size = window.outer_size().unwrap_or(tauri::PhysicalSize {
                width: (268.0 * scale) as u32,
                height: (40.0 * scale) as u32,
            });
            let margin = (24.0 * scale) as i32;
            let taskbar = (taskbar_reserve() * scale) as i32;
            let x = monitor_pos.x + monitor_size.width as i32 - widget_size.width as i32 - margin;
            let y =
                monitor_pos.y + monitor_size.height as i32 - widget_size.height as i32 - taskbar;
            let _ = window.set_position(tauri::PhysicalPosition { x, y });
        }
    }
}

fn taskbar_reserve() -> f64 {
    #[cfg(target_os = "macos")]
    {
        88.0
    }
    #[cfg(not(target_os = "macos"))]
    {
        64.0
    }
}

fn widget_size(window: &tauri::WebviewWindow) -> (i32, i32) {
    let size = window.outer_size().unwrap_or(tauri::PhysicalSize {
        width: 268,
        height: 40,
    });
    (size.width as i32, size.height as i32)
}

fn clamp_into_view(window: &tauri::WebviewWindow, x: i32, y: i32) -> Option<(i32, i32)> {
    let (w, h) = widget_size(window);
    let monitors = window.available_monitors().ok()?;
    if monitors.is_empty() {
        return None;
    }

    let mut best_idx = None;
    let mut best_score = i64::MIN;
    for (i, monitor) in monitors.iter().enumerate() {
        let pos = monitor.position();
        let dim = monitor.size();
        let left = pos.x;
        let top = pos.y;
        let right = pos.x + dim.width as i32;
        let bottom = pos.y + dim.height as i32;
        let overlap_w = ((x + w).min(right) - x.max(left)).max(0) as i64;
        let overlap_h = ((y + h).min(bottom) - y.max(top)).max(0) as i64;
        let overlap = overlap_w * overlap_h;
        let score = if overlap > 0 {
            overlap
        } else {
            let dx = ((x + w / 2) - (left + right) / 2) as i64;
            let dy = ((y + h / 2) - (top + bottom) / 2) as i64;
            -(dx * dx + dy * dy)
        };
        if score > best_score {
            best_score = score;
            best_idx = Some(i);
        }
    }

    let monitor = &monitors[best_idx?];
    let pos = monitor.position();
    let dim = monitor.size();
    let scale = monitor.scale_factor();
    let margin = (24.0 * scale) as i32;
    let taskbar = (taskbar_reserve() * scale) as i32;
    let min_x = pos.x + margin;
    let max_x = (pos.x + dim.width as i32 - w - margin).max(min_x);
    let min_y = pos.y + margin;
    let max_y = (pos.y + dim.height as i32 - h - taskbar).max(min_y);
    Some((x.clamp(min_x, max_x), y.clamp(min_y, max_y)))
}

fn resolve_base_dir(handle: &tauri::AppHandle) -> PathBuf {
    let mut candidates = Vec::new();
    if let Ok(documents) = handle.path().document_dir() {
        candidates.push(documents.join("Synapse"));
    }
    if let Ok(data) = handle.path().app_data_dir() {
        candidates.push(data.join("Synapse"));
    }
    for base in &candidates {
        if std::fs::create_dir_all(base).is_ok() {
            return base.clone();
        }
    }
    candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("."))
}

fn migrate_legacy_data(
    handle: &tauri::AppHandle,
    config_dir: &Path,
    data_dir: &Path,
    models_dir: &Path,
) {
    if let Ok(old_config) = handle.path().app_config_dir() {
        move_if_absent(
            &old_config.join("settings.json"),
            &config_dir.join("settings.json"),
        );
    }
    if let Ok(old_data) = handle.path().app_data_dir() {
        for name in ["history.db", "history.db-wal", "history.db-shm"] {
            move_if_absent(&old_data.join(name), &data_dir.join(name));
        }
        if let Ok(entries) = std::fs::read_dir(old_data.join("models")) {
            for entry in entries.flatten() {
                let from = entry.path();
                if from.is_file() {
                    if let Some(file) = from.file_name() {
                        move_if_absent(&from, &models_dir.join(file));
                    }
                }
            }
        }
    }
}

fn move_if_absent(from: &Path, to: &Path) {
    if to.exists() || !from.exists() {
        return;
    }
    if let Some(parent) = to.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::rename(from, to).is_ok() {
        return;
    }
    let mut tmp = to.as_os_str().to_owned();
    tmp.push(".migrating");
    let tmp = PathBuf::from(tmp);
    let _ = std::fs::remove_file(&tmp);
    if std::fs::copy(from, &tmp).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, to).is_ok() {
        let _ = std::fs::remove_file(from);
    } else {
        let _ = std::fs::remove_file(&tmp);
    }
}

fn prepend_dll_dirs(resource_dir: &Path) {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let candidates = [
        resource_dir.to_path_buf(),
        resource_dir.join("resources"),
        resource_dir.join("resources").join("cuda"),
        resource_dir.join("resources").join("binaries"),
        manifest.join("resources"),
        manifest.join("resources").join("cuda"),
        manifest.join("resources").join("binaries"),
    ];
    let existing: Vec<PathBuf> = candidates.into_iter().filter(|p| p.exists()).collect();
    if existing.is_empty() {
        return;
    }
    let prefix = existing
        .iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join(";");
    let current = std::env::var("PATH").unwrap_or_default();
    std::env::set_var("PATH", format!("{prefix};{current}"));
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    match log_file() {
        Some((file, path)) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_ansi(false)
                .with_writer(std::sync::Mutex::new(file))
                .try_init();
            tracing::info!("logging to {}", path.display());
        }
        None => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .try_init();
        }
    }
}

fn log_file() -> Option<(std::fs::File, PathBuf)> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        let home = PathBuf::from(home);
        if let Some(onedrive) = std::env::var_os("OneDrive") {
            let onedrive = PathBuf::from(onedrive);
            candidates.push(onedrive.join("Documents"));
            candidates.push(onedrive.join("Documentos"));
        }
        candidates.push(home.join("Documents"));
    }
    candidates.push(std::env::temp_dir());
    for base in candidates {
        if !base.exists() {
            continue;
        }
        let dir = base.join("Synapse").join("logs");
        if std::fs::create_dir_all(&dir).is_err() {
            continue;
        }
        let path = dir.join("synapse.log");
        if let Ok(file) = std::fs::File::create(&path) {
            return Some((file, path));
        }
    }
    None
}
