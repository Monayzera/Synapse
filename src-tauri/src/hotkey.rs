use crate::error::AppResult;
use crate::state::SharedState;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Manager};

static HOOK_READY: AtomicBool = AtomicBool::new(false);
static BINDING_OK: AtomicBool = AtomicBool::new(false);
static SETTLED: AtomicBool = AtomicBool::new(false);
static FAILURE_REPORTED: AtomicBool = AtomicBool::new(false);

pub fn apply(_app: &AppHandle, state: &SharedState) -> AppResult<()> {
    let settings = state.settings_snapshot();
    #[cfg(windows)]
    let parsed = crate::inputhook::set_bindings(&settings);
    #[cfg(target_os = "macos")]
    let parsed = crate::inputhook_mac::set_bindings(&settings);
    #[cfg(not(any(windows, target_os = "macos")))]
    let parsed = {
        SETTLED.store(true, Ordering::Release);
        false
    };
    if parsed {
        tracing::info!(
            "hotkey bound: {} ({:?})",
            settings.hotkey_ptt,
            settings.record_mode
        );
    } else {
        tracing::warn!("hotkey '{}' could not be parsed", settings.hotkey_ptt);
    }
    BINDING_OK.store(parsed, Ordering::Release);
    state.emit_status();
    Ok(())
}

pub fn set_hook_ready(app: &AppHandle, ready: bool) {
    let previous = HOOK_READY.swap(ready, Ordering::AcqRel);
    SETTLED.store(true, Ordering::Release);
    if previous != ready {
        tracing::info!("global shortcut hook ready: {previous} -> {ready}");
    }
    if ready {
        FAILURE_REPORTED.store(false, Ordering::Release);
    }
    if let Some(state) = app.try_state::<SharedState>() {
        state.emit_status();
    }
}

pub fn report_failure(app: &AppHandle, message: &str) {
    if FAILURE_REPORTED.swap(true, Ordering::AcqRel) {
        return;
    }
    crate::pipeline::emit_error(app, "hotkey", "hotkey_failed", message);
}

pub fn is_ready() -> bool {
    HOOK_READY.load(Ordering::Acquire) && BINDING_OK.load(Ordering::Acquire)
}

pub fn is_settled() -> bool {
    SETTLED.load(Ordering::Acquire)
}
