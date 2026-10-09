use parking_lot::Mutex;
use serde::Serialize;
use tauri::AppHandle;

pub const AUTOSTART_ARG: &str = "--autostart";
const LEGACY_CPU_NAME: &str = "Synapse CPU";

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

pub fn remove_legacy_cpu_entry() -> bool {
    match platform::remove_stale_entry(LEGACY_CPU_NAME) {
        Ok(removed) => removed,
        Err(err) => {
            tracing::warn!("autostart entry {LEGACY_CPU_NAME} could not be checked: {err}");
            false
        }
    }
}

#[cfg(windows)]
fn command_exe(command: &str) -> Option<std::path::PathBuf> {
    let command = command.trim();
    let raw = match command.strip_prefix('"') {
        Some(rest) => rest.split_once('"')?.0,
        None => {
            let end = command.to_ascii_lowercase().find(".exe")?;
            &command[..end + 4]
        }
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let path = std::path::PathBuf::from(raw);
    path.is_absolute().then_some(path)
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

    pub fn remove_stale_entry(name: &str) -> Result<bool, String> {
        if debug_build() {
            return Ok(false);
        }
        let key = match open(RUN_KEY, KEY_READ | KEY_SET_VALUE)? {
            Some(key) => key,
            None => return Ok(false),
        };
        let command = match read(&key, name)? {
            Some((kind, data)) if kind == REG_SZ => decode_string(&data),
            Some(_) => {
                tracing::info!("autostart entry {name} is not a plain command; left untouched");
                return Ok(false);
            }
            None => return Ok(false),
        };
        let Some(exe) = super::command_exe(&command) else {
            tracing::info!("autostart entry {name} has no readable program path; left untouched");
            return Ok(false);
        };
        match exe.try_exists() {
            Ok(false) => {}
            Ok(true) => return Ok(false),
            Err(err) => {
                tracing::info!(
                    "autostart entry {name} points to {} which could not be checked ({err}); left untouched",
                    exe.display()
                );
                return Ok(false);
            }
        }
        delete(&key, name)?;
        match open(APPROVED_KEY, KEY_SET_VALUE) {
            Ok(Some(approved)) => {
                if let Err(err) = delete(&approved, name) {
                    tracing::warn!("startup approval for {name} not removed: {err}");
                }
            }
            Ok(None) => {}
            Err(err) => tracing::warn!("startup approvals not opened: {err}"),
        }
        tracing::info!(
            "stale autostart entry {name} removed (its program {} no longer exists)",
            exe.display()
        );
        Ok(true)
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

    pub fn remove_stale_entry(_name: &str) -> Result<bool, String> {
        Ok(false)
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

#[cfg(target_os = "linux")]
mod platform {
    use super::{AutostartStatus, AUTOSTART_ARG};
    use crate::linux_portal::exec_value;
    use std::io::ErrorKind;
    use std::path::{Path, PathBuf};
    use tauri::{AppHandle, Manager};

    const ENTRY_FILE: &str = "com.synapse.voice.desktop";
    const DELETED_SUFFIX: &str = " (deleted)";
    const ERR_FILE_FAILED: &str = "autostart_file_failed";
    const ERR_PROGRAM_INVALID: &str = "autostart_program_invalid";
    const ERR_RESTART_REQUIRED: &str = "autostart_restart_required";

    #[derive(Default)]
    struct Entry {
        exec: Option<String>,
        hidden: bool,
    }

    fn entry_path(app: &AppHandle) -> Result<PathBuf, String> {
        let config = app.path().config_dir().map_err(|err| {
            tracing::warn!("config directory unknown: {err}");
            ERR_FILE_FAILED.to_string()
        })?;
        Ok(config.join("autostart").join(ENTRY_FILE))
    }

    fn checked_program(path: &Path) -> Result<String, String> {
        let text = path.to_str().ok_or_else(|| {
            tracing::warn!("program path is not valid UTF-8: {}", path.display());
            ERR_PROGRAM_INVALID.to_string()
        })?;
        if text.chars().any(|ch| ch.is_control()) {
            tracing::warn!("program path has control characters: {text:?}");
            return Err(ERR_PROGRAM_INVALID.to_string());
        }
        Ok(text.to_string())
    }

    fn live_program(path: PathBuf) -> Result<PathBuf, String> {
        let live = match path.to_str().and_then(|text| text.strip_suffix(DELETED_SUFFIX)) {
            Some(stripped) => PathBuf::from(stripped),
            None => path,
        };
        match live.try_exists() {
            Ok(true) => Ok(live),
            Ok(false) => {
                tracing::warn!("running program is no longer on disk: {}", live.display());
                Err(ERR_RESTART_REQUIRED.to_string())
            }
            Err(err) => {
                tracing::warn!("running program could not be checked {}: {err}", live.display());
                Err(ERR_FILE_FAILED.to_string())
            }
        }
    }

    fn program_path() -> Result<String, String> {
        let path = match std::env::var_os("APPIMAGE") {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => std::env::current_exe().map_err(|err| {
                tracing::warn!("current exe unknown: {err}");
                ERR_PROGRAM_INVALID.to_string()
            })?,
        };
        checked_program(&live_program(path)?)
    }

    fn exec_line(program: &str) -> String {
        format!("{} {AUTOSTART_ARG}", exec_value(program))
    }

    fn content(program: &str) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName=Synapse\nExec={}\nIcon=com.synapse.voice\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
            exec_line(program)
        )
    }

    fn parse(text: &str) -> Entry {
        let mut entry = Entry::default();
        let mut in_group = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_group = line == "[Desktop Entry]";
                continue;
            }
            if !in_group {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "Exec" if entry.exec.is_none() => entry.exec = Some(value.to_string()),
                "Hidden" => entry.hidden = value == "true",
                _ => {}
            }
        }
        entry
    }

    fn read_entry(path: &Path) -> Result<Option<Entry>, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Some(parse(&text))),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
            Err(err) => {
                tracing::warn!("could not read {}: {err}", path.display());
                Err(ERR_FILE_FAILED.to_string())
            }
        }
    }

    fn write_entry(path: &Path, program: &str) -> Result<(), String> {
        crate::atomic_io::write_durable(path, content(program).as_bytes()).map_err(|err| {
            tracing::warn!("could not write {}: {err}", path.display());
            ERR_FILE_FAILED.to_string()
        })
    }

    fn remove_entry(path: &Path) -> Result<bool, String> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
            Err(err) => {
                tracing::warn!("could not remove {}: {err}", path.display());
                Err(ERR_FILE_FAILED.to_string())
            }
        }
    }

    fn points_to(entry: &Entry, program: &str) -> bool {
        entry.exec.as_deref() == Some(exec_line(program).as_str())
    }

    fn is_enabled(entry: &Entry, program: &str) -> bool {
        points_to(entry, program) && !entry.hidden
    }

    fn read_state(app: &AppHandle) -> Result<bool, String> {
        let path = entry_path(app)?;
        match read_entry(&path)? {
            Some(entry) => Ok(is_enabled(&entry, &program_path()?)),
            None => Ok(false),
        }
    }

    pub fn reconcile(app: &AppHandle, desired: bool) -> Result<(), String> {
        if cfg!(debug_assertions) {
            tracing::info!("debug build: autostart entry left untouched (desired {desired})");
            return Ok(());
        }
        let path = entry_path(app)?;
        if !desired {
            if remove_entry(&path)? {
                tracing::info!("autostart entry removed at {}", path.display());
            }
            return Ok(());
        }
        let program = program_path()?;
        match read_entry(&path)? {
            Some(entry) if points_to(&entry, &program) => {}
            _ => {
                write_entry(&path, &program)?;
                tracing::info!("autostart entry written at {}", path.display());
            }
        }
        Ok(())
    }

    pub fn apply(app: &AppHandle, enabled: bool) -> Result<(), String> {
        if cfg!(debug_assertions) {
            return Err("autostart is not registered from debug builds".to_string());
        }
        let path = entry_path(app)?;
        if enabled {
            write_entry(&path, &program_path()?)
        } else {
            remove_entry(&path).map(|_| ())
        }
    }

    pub fn remove_stale_entry(_name: &str) -> Result<bool, String> {
        Ok(false)
    }

    pub fn status(app: &AppHandle) -> AutostartStatus {
        match read_state(app) {
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

    #[cfg(test)]
    mod tests {
        use super::*;

        fn unquote(text: &str) -> Option<String> {
            let mut chars = text.strip_prefix('"')?.chars();
            let mut out = String::new();
            while let Some(ch) = chars.next() {
                match ch {
                    '"' => return Some(out),
                    '\\' => out.push(chars.next()?),
                    _ => out.push(ch),
                }
            }
            None
        }

        fn decode_exec(raw: &str) -> Option<String> {
            unquote(&raw.replace("\\\\", "\\")).map(|text| text.replace("%%", "%"))
        }

        #[test]
        fn quoted_program_round_trips() {
            for program in [
                "/opt/Synapse/synapse",
                "/home/me/My Apps/Synapse.AppImage",
                "/tmp/a\"b`c$d\\e",
                "/home/me/Apps/100%/Synapse.AppImage",
            ] {
                let raw = exec_value(program);
                assert_eq!(decode_exec(&raw).as_deref(), Some(program));
            }
        }

        #[test]
        fn percent_is_escaped_in_exec_line() {
            let program = "/home/me/Apps/100%/Synapse.AppImage";
            assert_eq!(
                exec_line(program),
                "\"/home/me/Apps/100%%/Synapse.AppImage\" --autostart"
            );
        }

        #[test]
        fn program_path_accepts_percent_and_rejects_control_characters() {
            assert_eq!(
                checked_program(Path::new("/opt/100%/synapse")).ok().as_deref(),
                Some("/opt/100%/synapse")
            );
            assert!(checked_program(Path::new("/opt/a\nb/synapse")).is_err());
            assert_eq!(
                checked_program(Path::new("/opt/a b/synapse")).ok().as_deref(),
                Some("/opt/a b/synapse")
            );
        }

        #[test]
        fn replaced_program_resolves_to_the_new_file() {
            let dir = std::env::temp_dir().join(format!("synapse-replaced-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("test dir created");
            let program = dir.join("synapse");
            std::fs::write(&program, b"").expect("test program written");
            let replaced = PathBuf::from(format!("{} (deleted)", program.display()));
            assert_eq!(live_program(replaced).ok(), Some(program.clone()));
            let removed = PathBuf::from(format!("{} (deleted)", dir.join("gone").display()));
            assert_eq!(live_program(removed), Err(ERR_RESTART_REQUIRED.to_string()));
            assert_eq!(live_program(program.clone()).ok(), Some(program));
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn missing_appimage_requires_restart() {
            let dir = std::env::temp_dir().join(format!("synapse-moved-{}", std::process::id()));
            let appimage = dir.join("Synapse-1.AppImage");
            assert_eq!(live_program(appimage), Err(ERR_RESTART_REQUIRED.to_string()));
        }

        #[test]
        fn enabled_requires_our_exec_and_not_hidden() {
            let program = "/usr/bin/synapse";
            let hidden = parse(
                &content(program).replace("[Desktop Entry]\n", "[Desktop Entry]\nHidden=true\n"),
            );
            assert!(!is_enabled(&hidden, program));
            let clean = parse(&content(program));
            assert!(is_enabled(&clean, program));
            assert!(!is_enabled(&clean, "/home/me/Synapse.AppImage"));
        }

        #[test]
        fn written_entry_points_to_its_program() {
            let program = "/home/me/Apps/Synapse.AppImage";
            let entry = parse(&content(program));
            assert!(points_to(&entry, program));
            assert!(!entry.hidden);
        }

        #[test]
        fn other_program_is_not_current() {
            let entry = parse(&content("/usr/bin/synapse"));
            assert!(!points_to(&entry, "/home/me/Synapse.AppImage"));
        }

        #[test]
        fn exec_outside_desktop_entry_group_is_ignored() {
            let entry = parse("[Desktop Action Quit]\nExec=/bin/false\n");
            assert!(entry.exec.is_none());
        }

        #[test]
        fn entry_file_round_trip() {
            let dir = std::env::temp_dir().join(format!("synapse-autostart-{}", std::process::id()));
            let path = dir.join(ENTRY_FILE);
            let program = "/home/me/My Apps/Synapse.AppImage";
            write_entry(&path, program).expect("entry written");
            let text = std::fs::read_to_string(&path).expect("entry readable");
            assert!(text.contains("Exec=\"/home/me/My Apps/Synapse.AppImage\" --autostart\n"));
            let entry = read_entry(&path).expect("entry parsed").expect("entry present");
            assert!(points_to(&entry, program));
            assert!(remove_entry(&path).expect("entry removed"));
            assert!(!remove_entry(&path).expect("missing entry is success"));
            assert!(read_entry(&path).expect("missing entry read").is_none());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
mod platform {
    use super::AutostartStatus;
    use tauri::AppHandle;

    pub fn reconcile(_app: &AppHandle, _desired: bool) -> Result<(), String> {
        Ok(())
    }

    pub fn apply(_app: &AppHandle, _enabled: bool) -> Result<(), String> {
        Err("autostart is not supported on this platform".to_string())
    }

    pub fn remove_stale_entry(_name: &str) -> Result<bool, String> {
        Ok(false)
    }

    pub fn status(_app: &AppHandle) -> AutostartStatus {
        AutostartStatus {
            enabled: false,
            disabled_by_windows: false,
            error: None,
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn run_command_program_is_extracted() {
        assert_eq!(
            command_exe(r#""C:\Users\me\AppData\Local\Synapse CPU\synapse.exe" --autostart"#),
            Some(PathBuf::from(r"C:\Users\me\AppData\Local\Synapse CPU\synapse.exe"))
        );
        assert_eq!(
            command_exe(r"C:\Apps\Synapse\synapse.EXE --autostart"),
            Some(PathBuf::from(r"C:\Apps\Synapse\synapse.EXE"))
        );
        assert_eq!(
            command_exe(r#"  "D:\Synapse\synapse.exe"  "#),
            Some(PathBuf::from(r"D:\Synapse\synapse.exe"))
        );
        assert_eq!(command_exe(r#""C:\Apps\Synapse\synapse.exe"#), None);
        assert_eq!(command_exe(r#""" --autostart"#), None);
        assert_eq!(command_exe(r"synapse.exe --autostart"), None);
        assert_eq!(command_exe(r"C:\Apps\Synapse\synapse --autostart"), None);
        assert_eq!(command_exe(""), None);
    }
}
