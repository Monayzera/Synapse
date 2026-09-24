mod atomic_io;
mod audio;
mod autostart;
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
#[cfg(windows)]
mod power;
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
use crate::state::{AppState, EngineMeta, LlmState, SharedState, Status};
use parking_lot::{Mutex, RwLock};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{Emitter, Manager, WindowEvent};
#[cfg(target_os = "macos")]
use tauri_plugin_autostart::MacosLauncher;

const DATA_ROOT_POINTER: &str = "data_root";
const DATA_ROOT_WAIT: Duration = Duration::from_secs(60);
const LOG_KEEP: usize = 5;
const WINDOW_RETRIES: u32 = 8;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    install_panic_hook();

    let context = tauri::generate_context!();
    let identifier = context.config().identifier.clone();
    let version = context.package_info().version.to_string();
    let autostart_launch = autostart::launched_by_autostart();

    #[cfg(windows)]
    let guard = instance_guard(&identifier);
    #[cfg(windows)]
    let primary = guard.primary;
    #[cfg(not(windows))]
    let primary = true;

    let log_dir = app_local_dir(&identifier)
        .map(|dir| dir.join("logs"))
        .unwrap_or_else(|| std::env::temp_dir().join("Synapse").join("logs"));
    init_tracing(&log_dir, primary);
    log_banner(&version, autostart_launch);
    #[cfg(windows)]
    {
        if let Some(note) = &guard.note {
            tracing::info!("instance guard: {note}");
        }
        if guard.exit {
            tracing::warn!("another Synapse instance is still starting; this launch exits");
            return;
        }
    }
    whisper_rs::install_logging_hooks();

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            tracing::info!("second launch forwarded ({argv:?}); showing the widget");
            show_window(app, "widget");
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_positioner::init());
    #[cfg(target_os = "macos")]
    let builder = builder.plugin(tauri_plugin_autostart::init(
        MacosLauncher::LaunchAgent,
        Some(vec![autostart::AUTOSTART_ARG]),
    ));

    let setup_log_dir = log_dir.clone();
    let built = builder
        .setup(move |app| {
            setup(app, autostart_launch, setup_log_dir);
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
            commands::update_settings,
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
            commands::activate_model,
            commands::reload_engine,
            commands::restart_llm,
            commands::test_llm,
            commands::autostart_status,
            commands::set_autostart,
            commands::open_settings,
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
        .build(context);

    let app = match built {
        Ok(app) => app,
        Err(err) => {
            tracing::error!("Synapse could not start: {err}");
            return;
        }
    };

    app.run(|handle, event| {
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

fn setup(app: &mut tauri::App, autostart_launch: bool, log_dir: PathBuf) {
    let handle = app.handle().clone();

    let base_dir = resolve_base_dir(&handle);
    tracing::info!("data root: {}", base_dir.display());
    let config_dir = base_dir.join("config");
    let data_dir = base_dir.join("data");
    let models_dir = data_dir.join("models");
    let bin_dir = data_dir.join("bin");
    for dir in [&config_dir, &models_dir, &bin_dir] {
        if let Err(err) = std::fs::create_dir_all(dir) {
            tracing::warn!("could not create {}: {err}", dir.display());
        }
    }

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

    let (settings, migrated) = match Settings::load(&config_path) {
        LoadOutcome::Loaded(settings, migrated) => {
            let settings = *settings;
            if migrated {
                match settings.save(&config_path) {
                    Ok(()) => tracing::info!("settings migrated to the current format"),
                    Err(err) => tracing::warn!("migrated settings could not be persisted: {err}"),
                }
            }
            (settings, migrated)
        }
        LoadOutcome::Missing => {
            let settings = Settings::default();
            if let Err(err) = settings.save(&config_path) {
                tracing::warn!("failed to write initial settings: {err}");
            }
            (settings, false)
        }
        LoadOutcome::Corrupt => {
            tracing::error!(
                "settings.json contained invalid data and no usable backup; running on in-memory defaults WITHOUT overwriting disk (a timestamped .corrupt copy was saved)"
            );
            (Settings::default(), false)
        }
        LoadOutcome::Unreadable => {
            tracing::error!(
                "settings present on disk but unreadable at startup (boot-time AV/cloud lock?); using in-memory defaults and REFUSING to persist until it can be read"
            );
            let mut settings = Settings::default();
            settings.transient_unreadable = true;
            (settings, false)
        }
    };

    prepend_dll_dirs(&resource_dir);

    let notify_handle = handle.clone();
    let notifier: audio::Notifier = Arc::new(move || {
        if let Some(state) = notify_handle.try_state::<SharedState>() {
            state.emit_status();
        }
    });
    let audio = AudioEngine::new(settings.audio_device.clone(), notifier);

    let db_path = data_dir.join("history.db");
    let connection = match history::open(&db_path) {
        Ok(conn) => Some(conn),
        Err(err) => {
            tracing::error!(
                "history database unavailable at startup ({err}); history disabled until it can be opened"
            );
            None
        }
    };

    let ui_language = settings.ui_language.clone();
    let state: SharedState = Arc::new(AppState {
        app: handle.clone(),
        config_path,
        models_dir,
        bin_dir,
        resource_dir,
        log_dir,
        custom_models: RwLock::new(custom_store),
        custom_models_path,
        llama_setup_running: AtomicBool::new(false),
        settings: RwLock::new(settings),
        settings_io: Mutex::new(()),
        audio,
        transcribe: RwLock::new(None),
        engine_meta: RwLock::new(EngineMeta::default()),
        engine_lock: Mutex::new(()),
        engine_gen: AtomicU64::new(0),
        engine_settled: AtomicBool::new(false),
        vad: RwLock::new(None),
        llm: LlmClient::new(),
        llm_state: AtomicU8::new(LlmState::Off.as_u8()),
        sidecar: Mutex::new(None),
        sidecar_gen: AtomicU64::new(0),
        db: Mutex::new(connection),
        db_path,
        status: RwLock::new(Status::Idle),
        recording: AtomicBool::new(false),
        busy: AtomicBool::new(false),
        sidecar_ready: AtomicBool::new(false),
        boot_done: AtomicBool::new(false),
        autostart_launch,
        downloading: Mutex::new(std::collections::HashSet::new()),
        widget_pos_path,
        widget_x: AtomicI32::new(widget_x),
        widget_y: AtomicI32::new(widget_y),
        widget_move_gen: AtomicU64::new(0),
        widget_ready: AtomicBool::new(false),
    });

    app.manage(state.clone());

    build_windows(app);

    #[cfg(windows)]
    inputhook::start(handle.clone());

    #[cfg(windows)]
    power::start(handle.clone());

    #[cfg(target_os = "macos")]
    inputhook_mac::start(handle.clone());

    if let Err(err) = tray::build(&handle, &ui_language) {
        tracing::error!("tray icon unavailable: {err}");
    }

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
    round_window_corners(&handle, &["settings", "history"]);

    spawn_level_emitter(state.clone());

    services::bootstrap(&handle, &state, migrated);
}

fn build_window(
    handle: &tauri::AppHandle,
    config: &tauri::utils::config::WindowConfig,
) -> tauri::Result<()> {
    tauri::WebviewWindowBuilder::from_config(handle, config)?.build()?;
    Ok(())
}

fn window_created(handle: &tauri::AppHandle, label: &str) {
    tracing::info!("window '{label}' created");
    if label == "widget" {
        position_widget(handle);
    }
    #[cfg(windows)]
    if label == "settings" || label == "history" {
        round_window_corners(handle, &[label]);
    }
}

fn build_windows(app: &tauri::App) {
    let handle = app.handle().clone();
    let mut pending = Vec::new();
    for config in app.config().app.windows.iter() {
        if handle.get_webview_window(&config.label).is_some() {
            continue;
        }
        if let Err(err) = build_window(&handle, config) {
            tracing::error!(
                "window '{}' could not be created ({err}); retrying in background",
                config.label
            );
            pending.push(config.clone());
        }
    }
    if pending.is_empty() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let mut delay = Duration::from_millis(500);
        for attempt in 1..=WINDOW_RETRIES {
            tokio::time::sleep(delay).await;
            pending.retain(|config| {
                if handle.get_webview_window(&config.label).is_some() {
                    return false;
                }
                match build_window(&handle, config) {
                    Ok(()) => {
                        window_created(&handle, &config.label);
                        false
                    }
                    Err(err) => {
                        tracing::warn!(
                            "window '{}' creation attempt {attempt} failed: {err}",
                            config.label
                        );
                        true
                    }
                }
            });
            if pending.is_empty() {
                return;
            }
            delay = (delay * 2).min(Duration::from_secs(15));
        }
        for config in &pending {
            tracing::error!("window '{}' could not be created; giving up", config.label);
        }
    });
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
fn round_window_corners(app: &tauri::AppHandle, labels: &[&str]) {
    use windows::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };

    for label in labels {
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

enum Pointer {
    Missing,
    Found(PathBuf),
    Unreadable,
}

fn read_pointer(path: &Path) -> Pointer {
    match std::fs::read_to_string(path) {
        Ok(content) => {
            let trimmed = content.trim();
            if trimmed.is_empty() {
                Pointer::Missing
            } else {
                Pointer::Found(PathBuf::from(trimmed))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Pointer::Missing,
        Err(err) => {
            tracing::warn!("data root pointer {} unreadable: {err}", path.display());
            Pointer::Unreadable
        }
    }
}

fn is_dir(path: &Path) -> bool {
    matches!(std::fs::metadata(path), Ok(meta) if meta.is_dir())
}

fn wait_for_dir(dir: &Path) -> bool {
    let deadline = Instant::now() + DATA_ROOT_WAIT;
    let mut delay = Duration::from_millis(250);
    let mut announced = false;
    loop {
        if is_dir(dir) {
            return true;
        }
        if let Some(parent) = dir.parent() {
            if is_dir(parent) {
                match std::fs::create_dir_all(dir) {
                    Ok(()) => return true,
                    Err(err) => tracing::debug!("data root {} not creatable yet: {err}", dir.display()),
                }
            }
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        if !announced {
            announced = true;
            tracing::warn!(
                "data root {} not available yet; waiting up to {} s",
                dir.display(),
                DATA_ROOT_WAIT.as_secs()
            );
        }
        std::thread::sleep(delay.min(deadline - now));
        delay = (delay * 2).min(Duration::from_secs(5));
    }
}

fn resolve_base_dir(handle: &tauri::AppHandle) -> PathBuf {
    let pointer = match handle.path().app_local_data_dir() {
        Ok(dir) => Some(dir.join(DATA_ROOT_POINTER)),
        Err(err) => {
            tracing::warn!("app local data dir unknown ({err}); data root pointer disabled");
            None
        }
    };
    let saved = pointer.as_deref().map(read_pointer);
    match saved {
        Some(Pointer::Found(dir)) => {
            if wait_for_dir(&dir) {
                return dir;
            }
            tracing::error!(
                "saved data root {} still unavailable; using a fallback for this run",
                dir.display()
            );
            candidate_base_dir(handle).0
        }
        Some(Pointer::Unreadable) | None => candidate_base_dir(handle).0,
        Some(Pointer::Missing) => {
            let (chosen, preferred) = candidate_base_dir(handle);
            if let (Some(pointer), true) = (pointer, preferred) {
                let text = chosen.to_string_lossy().to_string();
                match crate::atomic_io::write_durable(&pointer, text.as_bytes()) {
                    Ok(()) => tracing::info!("data root remembered in {}", pointer.display()),
                    Err(err) => tracing::warn!("data root pointer not written: {err}"),
                }
            }
            chosen
        }
    }
}

fn candidate_base_dir(handle: &tauri::AppHandle) -> (PathBuf, bool) {
    let mut candidates = Vec::new();
    let documents = handle
        .path()
        .document_dir()
        .ok()
        .map(|dir| dir.join("Synapse"));
    if let Some(documents) = &documents {
        candidates.push(documents.clone());
    }
    if let Ok(data) = handle.path().app_data_dir() {
        candidates.push(data.join("Synapse"));
    }
    for base in &candidates {
        if std::fs::create_dir_all(base).is_ok() {
            return (base.clone(), documents.as_ref() == Some(base));
        }
    }
    let fallback = candidates
        .into_iter()
        .next()
        .unwrap_or_else(|| PathBuf::from("."));
    (fallback, false)
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

#[cfg(all(windows, target_env = "msvc", feature = "cuda"))]
#[repr(C)]
struct DelayLoadInfo {
    cb: u32,
    _pidd: *const core::ffi::c_void,
    _ppfn: *mut core::ffi::c_void,
    sz_dll: *const core::ffi::c_char,
}

#[cfg(all(windows, target_env = "msvc", feature = "cuda"))]
#[no_mangle]
#[allow(non_upper_case_globals)]
static __pfnDliNotifyHook2: Option<
    extern "system" fn(u32, *const DelayLoadInfo) -> *mut core::ffi::c_void,
> = Some(cuda_delay_load_hook);

#[cfg(all(windows, target_env = "msvc", feature = "cuda"))]
extern "system" fn cuda_delay_load_hook(
    notify: u32,
    info: *const DelayLoadInfo,
) -> *mut core::ffi::c_void {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::System::LibraryLoader::{LoadLibraryExW, LOAD_WITH_ALTERED_SEARCH_PATH};

    const DLI_NOTE_PRE_LOAD_LIBRARY: u32 = 1;
    if notify != DLI_NOTE_PRE_LOAD_LIBRARY || info.is_null() {
        return std::ptr::null_mut();
    }
    let info = unsafe { &*info };
    if (info.cb as usize) < std::mem::size_of::<DelayLoadInfo>() || info.sz_dll.is_null() {
        return std::ptr::null_mut();
    }
    let Ok(name) = unsafe { std::ffi::CStr::from_ptr(info.sz_dll) }.to_str() else {
        return std::ptr::null_mut();
    };
    let Ok(exe) = std::env::current_exe() else {
        return std::ptr::null_mut();
    };
    let Some(exe_dir) = exe.parent() else {
        return std::ptr::null_mut();
    };
    let path = exe_dir.join("resources").join("cuda").join(name);
    if !path.is_file() {
        return std::ptr::null_mut();
    }
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    match unsafe { LoadLibraryExW(PCWSTR(wide.as_ptr()), None, LOAD_WITH_ALTERED_SEARCH_PATH) } {
        Ok(module) => module.0,
        Err(_) => std::ptr::null_mut(),
    }
}


fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!("panic in thread '{name}': {info}\nbacktrace:\n{backtrace}");
        previous(info);
    }));
}

fn app_local_dir(identifier: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join("AppData").join("Local"))
        });
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library").join("Application Support"));
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
    });
    base.map(|dir| dir.join(identifier))
}

fn rotated_log(dir: &Path, index: usize) -> PathBuf {
    if index == 0 {
        dir.join("synapse.log")
    } else {
        dir.join(format!("synapse.{index}.log"))
    }
}

fn rotate_logs(dir: &Path) {
    let oldest = rotated_log(dir, LOG_KEEP - 1);
    if oldest.exists() {
        let _ = std::fs::remove_file(&oldest);
    }
    for index in (0..LOG_KEEP - 1).rev() {
        let from = rotated_log(dir, index);
        if from.exists() {
            let _ = std::fs::rename(&from, rotated_log(dir, index + 1));
        }
    }
}

fn open_log(dir: &Path, primary: bool) -> Option<(std::fs::File, PathBuf)> {
    if std::fs::create_dir_all(dir).is_err() {
        return None;
    }
    let path = if primary {
        rotate_logs(dir);
        rotated_log(dir, 0)
    } else {
        dir.join("synapse.secondary.log")
    };
    std::fs::File::create(&path).ok().map(|file| (file, path))
}

fn init_tracing(log_dir: &Path, primary: bool) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,ort=warn"));
    let opened = open_log(log_dir, primary).or_else(|| {
        open_log(&std::env::temp_dir().join("Synapse").join("logs"), primary)
    });
    match opened {
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

fn system_uptime() -> String {
    #[cfg(windows)]
    {
        let millis = unsafe { windows::Win32::System::SystemInformation::GetTickCount64() };
        format!("{} s", millis / 1000)
    }
    #[cfg(not(windows))]
    {
        "n/a".to_string()
    }
}

fn log_banner(version: &str, autostart_launch: bool) {
    let exe = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("unknown ({err})"));
    let cwd = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|err| format!("unknown ({err})"));
    let args: Vec<String> = std::env::args().collect();
    tracing::info!(
        "Synapse {version} starting: pid {}, exe {exe}, cwd {cwd}, args {args:?}, autostart launch {autostart_launch}, system uptime {}",
        std::process::id(),
        system_uptime()
    );
}

#[cfg(windows)]
struct InstanceGuard {
    primary: bool,
    exit: bool,
    note: Option<String>,
}

#[cfg(windows)]
fn instance_guard(identifier: &str) -> InstanceGuard {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS, WAIT_ABANDONED, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
    use windows::Win32::UI::WindowsAndMessaging::FindWindowW;

    fn wide(text: String) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let mutex_name = wide(format!("{identifier}-boot-guard"));
    let class_name = wide(format!("{identifier}-sic"));
    let window_name = wide(format!("{identifier}-siw"));

    let handle = match unsafe { CreateMutexW(None, true, PCWSTR(mutex_name.as_ptr())) } {
        Ok(handle) => handle,
        Err(err) => {
            return InstanceGuard {
                primary: true,
                exit: false,
                note: Some(format!("guard unavailable ({err})")),
            }
        }
    };
    if unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
        return InstanceGuard {
            primary: true,
            exit: false,
            note: None,
        };
    }

    let started = Instant::now();
    let deadline = started + Duration::from_secs(30);
    loop {
        let found = unsafe { FindWindowW(PCWSTR(class_name.as_ptr()), PCWSTR(window_name.as_ptr())) }
            .map(|hwnd| !hwnd.is_invalid())
            .unwrap_or(false);
        if found {
            return InstanceGuard {
                primary: false,
                exit: false,
                note: Some(format!(
                    "another instance is running; forwarding after {} ms",
                    started.elapsed().as_millis()
                )),
            };
        }
        let wait = unsafe { WaitForSingleObject(handle, 0) };
        if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
            return InstanceGuard {
                primary: true,
                exit: false,
                note: Some("previous instance ended; continuing as the primary instance".to_string()),
            };
        }
        if Instant::now() >= deadline {
            return InstanceGuard {
                primary: false,
                exit: true,
                note: Some("the first instance did not finish starting within 30 s".to_string()),
            };
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
