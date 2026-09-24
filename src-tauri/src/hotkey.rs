use crate::error::AppResult;
use crate::state::SharedState;
use tauri::AppHandle;

pub fn apply(_app: &AppHandle, state: &SharedState) -> AppResult<()> {
    #[cfg(windows)]
    crate::inputhook::set_bindings(&state.settings_snapshot());
    #[cfg(target_os = "macos")]
    crate::inputhook_mac::set_bindings(&state.settings_snapshot());
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = state;
    Ok(())
}
