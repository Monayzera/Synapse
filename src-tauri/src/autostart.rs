use parking_lot::Mutex;
use serde::Serialize;
use tauri::AppHandle;

pub const AUTOSTART_ARG: &str = "--autostart";

#[derive(Debug, Clone, Serialize)]
pub struct AutostartStatus {
    pub enabled: bool,
    pub disabled_by_windows: bool,
    pub error: Option<String>,
}

static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);

fn remember(result: &Result<(), String>) {
    let mut last = LAST_ERROR.lock();
    match result {
        Ok(()) => *last = None,
        Err(err) => *last = Some(err.clone()),
    }
}

fn last_error() -> Option<String> {
    LAST_ERROR.lock().clone()
}

pub fn launched_by_autostart() -> bool {
    std::env::args().skip(1).any(|arg| arg == AUTOSTART_ARG)
}

pub fn reconcile(app: &AppHandle, desired: bool) {
    let result = platform::reconcile(app, desired);
    if let Err(err) = &result {
        tracing::error!("autostart reconcile failed: {err}");
    }
    remember(&result);
}

pub fn apply(app: &AppHandle, enabled: bool) -> Result<(), String> {
    let result = platform::apply(app, enabled);
    match &result {
        Ok(()) => tracing::info!("autostart {}", if enabled { "enabled" } else { "disabled" }),
        Err(err) => tracing::error!("autostart apply failed: {err}"),
    }
    remember(&result);
    result
}

pub fn status(app: &AppHandle) -> AutostartStatus {
    let mut status = platform::status(app);
    if status.error.is_none() {
        status.error = last_error();
    }
    status
}

#[cfg(windows)]
mod platform {
    use super::{AutostartStatus, AUTOSTART_ARG};
    use tauri::AppHandle;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_BINARY,
        REG_EXPAND_SZ, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_SZ, REG_VALUE_TYPE,
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const APPROVED_KEY: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    const APPROVED_ENABLED: [u8; 12] = [0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn check(code: WIN32_ERROR, what: &str) -> Result<(), String> {
        if code == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("{what} failed (error {})", code.0))
        }
    }

    fn open(subkey: &str, access: REG_SAM_FLAGS) -> Result<Option<Key>, String> {
        let name = wide(subkey);
        let mut handle = HKEY::default();
        let code =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(name.as_ptr()), None, access, &mut handle) };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(code, "open registry key")?;
        Ok(Some(Key(handle)))
    }

    fn create(subkey: &str) -> Result<Key, String> {
        let name = wide(subkey);
        let mut handle = HKEY::default();
        let code = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(name.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_SET_VALUE,
                None,
                &mut handle,
                None,
            )
        };
        check(code, "create registry key")?;
        Ok(Key(handle))
    }

    fn read(key: &Key, value: &str) -> Result<Option<(REG_VALUE_TYPE, Vec<u8>)>, String> {
        let name = wide(value);
        for _ in 0..3 {
            let mut kind = REG_VALUE_TYPE::default();
            let mut size: u32 = 0;
            let code = unsafe {
                RegQueryValueExW(
                    key.0,
                    PCWSTR(name.as_ptr()),
                    None,
                    Some(&mut kind as *mut REG_VALUE_TYPE),
                    None,
                    Some(&mut size as *mut u32),
                )
            };
            if code == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            check(code, "query registry value size")?;
            let mut data = vec![0u8; size as usize];
            let mut filled = size;
            let code = unsafe {
                RegQueryValueExW(
                    key.0,
                    PCWSTR(name.as_ptr()),
                    None,
                    Some(&mut kind as *mut REG_VALUE_TYPE),
                    Some(data.as_mut_ptr()),
                    Some(&mut filled as *mut u32),
                )
            };
            if code == ERROR_MORE_DATA {
                continue;
            }
            if code == ERROR_FILE_NOT_FOUND {
                return Ok(None);
            }
            check(code, "read registry value")?;
            data.truncate(filled as usize);
            return Ok(Some((kind, data)));
        }
        Err("registry value kept changing while reading".to_string())
    }

    fn decode_string(data: &[u8]) -> String {
        let units: Vec<u16> = data
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let end = units.iter().position(|unit| *unit == 0).unwrap_or(units.len());
        String::from_utf16_lossy(&units[..end])
    }

    fn write(key: &Key, value: &str, kind: REG_VALUE_TYPE, data: &[u8]) -> Result<(), String> {
        let name = wide(value);
        let code = unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, kind, Some(data)) };
        check(code, "write registry value")
    }

    fn delete(key: &Key, value: &str) -> Result<(), String> {
        let name = wide(value);
        let code = unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        check(code, "delete registry value")
    }

    fn value_name(app: &AppHandle) -> String {
        app.package_info().name.clone()
    }

    fn expected_command() -> Result<String, String> {
        let exe = std::env::current_exe().map_err(|e| format!("current exe unknown: {e}"))?;
        Ok(format!("\"{}\" {AUTOSTART_ARG}", exe.display()))
    }

    fn string_bytes(text: &str) -> Vec<u8> {
        text.encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(|unit| unit.to_le_bytes())
            .collect()
    }

    fn current_command(app: &AppHandle) -> Result<Option<String>, String> {
        let key = match open(RUN_KEY, KEY_READ)? {
            Some(key) => key,
            None => return Ok(None),
        };
        match read(&key, &value_name(app))? {
            Some((kind, data)) if kind == REG_SZ || kind == REG_EXPAND_SZ => {
                Ok(Some(decode_string(&data)))
            }
            Some(_) => Ok(Some(String::new())),
            None => Ok(None),
        }
    }

    fn approved_disabled(app: &AppHandle) -> Result<bool, String> {
        let key = match open(APPROVED_KEY, KEY_READ)? {
            Some(key) => key,
            None => return Ok(false),
        };
        match read(&key, &value_name(app))? {
            Some((_, data)) => Ok(data.first().map(|flag| flag & 0x01 == 0x01).unwrap_or(false)),
            None => Ok(false),
        }
    }

    fn register(app: &AppHandle) -> Result<bool, String> {
        let expected = expected_command()?;
        if current_command(app)?.as_deref() == Some(expected.as_str()) {
            return Ok(false);
        }
        let key = create(RUN_KEY)?;
        write(&key, &value_name(app), REG_SZ, &string_bytes(&expected))?;
        Ok(true)
    }

    fn owned_by_us(command: &str) -> bool {
        let lowered = command.to_lowercase();
        if lowered.contains(AUTOSTART_ARG) {
            return true;
        }
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.file_name().map(|name| name.to_string_lossy().to_lowercase()))
            .map(|name| lowered.contains(&name))
            .unwrap_or(false)
    }

    fn unregister(app: &AppHandle) -> Result<bool, String> {
        match current_command(app)? {
            None => return Ok(false),
            Some(command) if !owned_by_us(&command) => {
                tracing::warn!(
                    "autostart entry {} does not point to this app; left untouched",
                    value_name(app)
                );
                return Ok(false);
            }
            Some(_) => {}
        }
        let key = match open(RUN_KEY, KEY_SET_VALUE)? {
            Some(key) => key,
            None => return Ok(false),
        };
        delete(&key, &value_name(app))?;
        Ok(true)
    }

    fn debug_build() -> bool {
        cfg!(debug_assertions)
    }

    pub fn reconcile(app: &AppHandle, desired: bool) -> Result<(), String> {
        if debug_build() {
            tracing::info!("debug build: autostart registry entry left untouched (desired {desired})");
            return Ok(());
        }
        if desired {
            if register(app)? {
                tracing::info!("autostart entry written or refreshed for {}", value_name(app));
            }
            if approved_disabled(app).unwrap_or(false) {
                tracing::warn!("autostart is disabled in Task Manager (StartupApproved)");
            }
        } else if unregister(app)? {
            tracing::info!("stale autostart entry removed for {}", value_name(app));
        }
        Ok(())
    }

    pub fn apply(app: &AppHandle, enabled: bool) -> Result<(), String> {
        if debug_build() {
            return Err("autostart is not registered from debug builds".to_string());
        }
        if !enabled {
            unregister(app)?;
            return Ok(());
        }
        register(app)?;
        if approved_disabled(app)? {
            let key = create(APPROVED_KEY)?;
            write(&key, &value_name(app), REG_BINARY, &APPROVED_ENABLED)?;
            tracing::info!("autostart re-approved in StartupApproved at user request");
        }
        Ok(())
    }

    pub fn status(app: &AppHandle) -> AutostartStatus {
        let mut error = None;
        let enabled = match current_command(app) {
            Ok(value) => value.map(|command| owned_by_us(&command)).unwrap_or(false),
            Err(err) => {
                error = Some(err);
                false
            }
        };
        let disabled_by_windows = match approved_disabled(app) {
            Ok(disabled) => enabled && disabled,
            Err(err) => {
                error.get_or_insert(err);
                false
            }
        };
        AutostartStatus {
            enabled,
            disabled_by_windows,
            error,
        }
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::AutostartStatus;
    use tauri::{AppHandle, Manager};
    use tauri_plugin_autostart::AutoLaunchManager;

    fn with_manager<T>(
        app: &AppHandle,
        work: impl FnOnce(&AutoLaunchManager) -> Result<T, String>,
    ) -> Result<T, String> {
        match app.try_state::<AutoLaunchManager>() {
            Some(manager) => work(manager.inner()),
            None => Err("autostart plugin is not available".to_string()),
        }
    }

    pub fn reconcile(app: &AppHandle, desired: bool) -> Result<(), String> {
        if cfg!(debug_assertions) {
            tracing::info!("debug build: autostart launch agent left untouched (desired {desired})");
            return Ok(());
        }
        with_manager(app, |manager| {
            if desired {
                manager.enable().map_err(|e| e.to_string())
            } else if manager.is_enabled().unwrap_or(false) {
                manager.disable().map_err(|e| e.to_string())
            } else {
                Ok(())
            }
        })
    }

    pub fn apply(app: &AppHandle, enabled: bool) -> Result<(), String> {
        if cfg!(debug_assertions) {
            return Err("autostart is not registered from debug builds".to_string());
        }
        with_manager(app, |manager| {
            if enabled {
                manager.enable().map_err(|e| e.to_string())
            } else if manager.is_enabled().unwrap_or(false) {
                manager.disable().map_err(|e| e.to_string())
            } else {
                Ok(())
            }
        })
    }

    pub fn status(app: &AppHandle) -> AutostartStatus {
        match with_manager(app, |manager| manager.is_enabled().map_err(|e| e.to_string())) {
            Ok(enabled) => AutostartStatus {
                enabled,
                disabled_by_windows: false,
                error: None,
            },
            Err(err) => AutostartStatus {
                enabled: false,
                disabled_by_windows: false,
                error: Some(err),
            },
        }
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
mod platform {
    use super::AutostartStatus;
    use tauri::AppHandle;

    pub fn reconcile(_app: &AppHandle, _desired: bool) -> Result<(), String> {
        Ok(())
    }

    pub fn apply(_app: &AppHandle, _enabled: bool) -> Result<(), String> {
        Err("autostart is not supported on this platform".to_string())
    }

    pub fn status(_app: &AppHandle) -> AutostartStatus {
        AutostartStatus {
            enabled: false,
            disabled_by_windows: false,
            error: None,
        }
    }
}
