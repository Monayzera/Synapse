use crate::error::{AppError, AppResult};
use arboard::Clipboard;
#[cfg(not(target_os = "macos"))]
use enigo::{Direction, Enigo, Key, Keyboard, Settings as EnigoSettings};
#[cfg(target_os = "linux")]
use ashpd::desktop::remote_desktop::{
    DeviceType, KeyState, NotifyKeyboardKeysymOptions, RemoteDesktop, SelectDevicesOptions,
    StartOptions,
};
#[cfg(target_os = "linux")]
use ashpd::desktop::{CreateSessionOptions, PersistMode, ResponseError, Session};
#[cfg(target_os = "linux")]
use ashpd::enumflags2::BitFlags;
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_os = "linux")]
use std::sync::{Arc, OnceLock};

#[cfg(target_os = "macos")]
use std::ffi::c_void;

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreateKeyboardEvent(source: *mut c_void, keycode: u16, keydown: bool) -> *mut c_void;
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *mut c_void);
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const c_void);
}

#[cfg(target_os = "macos")]
pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

#[cfg(not(target_os = "linux"))]
pub fn inject_text(text: &str, restore_clipboard: bool, paste_delay_ms: u64) -> AppResult<()> {
    if text.trim().is_empty() {
        return Ok(());
    }

    let mut clipboard =
        Clipboard::new().map_err(|e| AppError::Inject(format!("clipboard open failed: {e}")))?;

    let previous = clipboard.get_text().ok();

    set_clipboard_with_retry(&mut clipboard, &platform_text(text))?;

    send_paste()?;

    std::thread::sleep(Duration::from_millis(paste_delay_ms.clamp(40, 1000)));

    if restore_clipboard {
        if let Some(prev) = previous {
            std::thread::sleep(Duration::from_millis(200));
            let _ = set_clipboard_with_retry(&mut clipboard, &prev);
        }
    }

    Ok(())
}

#[cfg(target_os = "linux")]
pub fn inject_text(text: &str, restore_clipboard: bool, paste_delay_ms: u64) -> AppResult<()> {
    if text.trim().is_empty() {
        return Ok(());
    }

    let platform = platform_text(text);
    let previous = with_clipboard(|clipboard| {
        let previous = clipboard.get_text().ok();
        set_clipboard_with_retry(clipboard, &platform)?;
        Ok(previous)
    })?;

    send_paste()?;

    std::thread::sleep(Duration::from_millis(paste_delay_ms.clamp(40, 1000)));

    if restore_clipboard {
        if let Some(prev) = previous {
            std::thread::sleep(Duration::from_millis(200));
            if let Err(err) = with_clipboard(|clipboard| set_clipboard_with_retry(clipboard, &prev))
            {
                tracing::warn!("restoring the previous clipboard text failed: {err}");
            }
        }
    }

    Ok(())
}

#[cfg(target_os = "linux")]
static CLIPBOARD: OnceLock<parking_lot::Mutex<Option<Clipboard>>> = OnceLock::new();

#[cfg(target_os = "linux")]
fn with_clipboard<T>(op: impl Fn(&mut Clipboard) -> AppResult<T>) -> AppResult<T> {
    let slot = CLIPBOARD.get_or_init(|| parking_lot::Mutex::new(None));
    let mut guard = slot.lock();
    if let Some(clipboard) = guard.as_mut() {
        match op(clipboard) {
            Ok(value) => return Ok(value),
            Err(err) => tracing::warn!("clipboard operation failed, reopening: {err}"),
        }
    }
    *guard = None;
    let opened =
        Clipboard::new().map_err(|e| AppError::Inject(format!("clipboard open failed: {e}")))?;
    let clipboard = guard.insert(opened);
    op(clipboard)
}

fn set_clipboard_with_retry(clipboard: &mut Clipboard, text: &str) -> AppResult<()> {
    let mut last_err = String::new();
    for attempt in 0..5 {
        match clipboard.set_text(text.to_owned()) {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_err = err.to_string();
                std::thread::sleep(Duration::from_millis(15 * (attempt + 1)));
            }
        }
    }
    Err(AppError::Inject(format!(
        "clipboard write failed after retries: {last_err}"
    )))
}

#[cfg(target_os = "macos")]
fn send_paste() -> AppResult<()> {
    const V_KEYCODE: u16 = 9;
    const CMD_FLAG: u64 = 0x0010_0000;
    const HID_EVENT_TAP: u32 = 0;
    if !unsafe { AXIsProcessTrusted() } {
        return Err(AppError::Inject(
            "macOS denied keyboard control (Accessibility not effective for this build)".to_string(),
        ));
    }
    unsafe {
        let down = CGEventCreateKeyboardEvent(std::ptr::null_mut(), V_KEYCODE, true);
        if down.is_null() {
            return Err(AppError::Inject("failed to create key-down event".to_string()));
        }
        CGEventSetFlags(down, CMD_FLAG);
        CGEventPost(HID_EVENT_TAP, down);
        CFRelease(down as *const c_void);

        let up = CGEventCreateKeyboardEvent(std::ptr::null_mut(), V_KEYCODE, false);
        if up.is_null() {
            return Err(AppError::Inject("failed to create key-up event".to_string()));
        }
        CGEventSetFlags(up, CMD_FLAG);
        CGEventPost(HID_EVENT_TAP, up);
        CFRelease(up as *const c_void);
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn send_paste_enigo() -> AppResult<()> {
    let mut enigo = Enigo::new(&EnigoSettings::default())
        .map_err(|e| AppError::Inject(format!("input backend init failed: {e}")))?;

    enigo
        .key(Key::Control, Direction::Press)
        .map_err(|e| AppError::Inject(format!("modifier press failed: {e}")))?;

    let paste_result = enigo.key(Key::Unicode('v'), Direction::Click);

    let release_result = enigo.key(Key::Control, Direction::Release);

    paste_result.map_err(|e| AppError::Inject(format!("paste key failed: {e}")))?;
    release_result.map_err(|e| AppError::Inject(format!("modifier release failed: {e}")))?;

    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn send_paste() -> AppResult<()> {
    send_paste_enigo()
}

#[cfg(target_os = "linux")]
static INPUT_DENIED: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "linux")]
const PORTAL_START_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(target_os = "linux")]
const PORTAL_CALL_TIMEOUT: Duration = Duration::from_secs(15);

#[cfg(target_os = "linux")]
const DETACHED_CREATE_TIMEOUT: Duration = Duration::from_secs(120);

#[cfg(target_os = "linux")]
const PORTAL_KEY_GAP: Duration = Duration::from_millis(8);

#[cfg(target_os = "linux")]
const KEYSYM_CONTROL_LEFT: i32 = 0xffe3;

#[cfg(target_os = "linux")]
const KEYSYM_V: i32 = 0x0076;

#[cfg(target_os = "linux")]
pub fn take_input_denied() -> bool {
    INPUT_DENIED.swap(false, Ordering::AcqRel)
}

#[cfg(target_os = "linux")]
enum PortalPaste {
    Denied,
    Unavailable(String),
    Failed(String),
}

#[cfg(target_os = "linux")]
fn send_paste() -> AppResult<()> {
    if !crate::linux_portal::is_wayland() {
        return send_paste_enigo();
    }
    match tauri::async_runtime::block_on(send_paste_portal()) {
        Ok(()) => Ok(()),
        Err(PortalPaste::Denied) => {
            INPUT_DENIED.store(true, Ordering::Release);
            Err(AppError::Inject(
                "remote interaction permission was denied".to_string(),
            ))
        }
        Err(PortalPaste::Unavailable(reason)) => {
            tracing::warn!("RemoteDesktop paste unavailable, using keyboard emulation: {reason}");
            send_paste_enigo()
        }
        Err(PortalPaste::Failed(reason)) => Err(AppError::Inject(format!(
            "RemoteDesktop paste failed: {reason}"
        ))),
    }
}

#[cfg(target_os = "linux")]
async fn send_paste_portal() -> Result<(), PortalPaste> {
    if let Err(err) = crate::linux_portal::register().await {
        tracing::warn!("host app registration failed before RemoteDesktop: {err}");
    }
    let remote = Arc::new(portal_call("open RemoteDesktop", RemoteDesktop::new()).await?);
    let token_path = restore_token_path();
    let saved_token = token_path.as_deref().and_then(read_restore_token);
    let session = await_created_session(spawn_create_session(Arc::clone(&remote))).await?;
    let result = paste_in_session(
        &remote,
        &session,
        saved_token.as_deref(),
        token_path.as_deref(),
    )
    .await;
    match tokio::time::timeout(PORTAL_CALL_TIMEOUT, session.close()).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::warn!("closing the RemoteDesktop session failed: {err}"),
        Err(_) => tracing::warn!(
            "closing the RemoteDesktop session did not answer within {} s",
            PORTAL_CALL_TIMEOUT.as_secs()
        ),
    }
    result
}

#[cfg(target_os = "linux")]
fn spawn_create_session(
    remote: Arc<RemoteDesktop>,
) -> tokio::sync::oneshot::Receiver<Result<Session<RemoteDesktop>, ashpd::Error>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    tauri::async_runtime::spawn(async move {
        let Ok(created) = tokio::time::timeout(
            DETACHED_CREATE_TIMEOUT,
            remote.create_session(CreateSessionOptions::default()),
        )
        .await
        else {
            tracing::warn!("the RemoteDesktop service never answered CreateSession");
            return;
        };
        match tx.send(created) {
            Ok(()) => {}
            Err(Ok(orphan)) => crate::linux_portal::close_abandoned(orphan).await,
            Err(Err(err)) => tracing::warn!("abandoned RemoteDesktop session request failed: {err}"),
        }
    });
    rx
}

#[cfg(target_os = "linux")]
async fn await_created_session(
    created: tokio::sync::oneshot::Receiver<Result<Session<RemoteDesktop>, ashpd::Error>>,
) -> Result<Session<RemoteDesktop>, PortalPaste> {
    match tokio::time::timeout(PORTAL_CALL_TIMEOUT, created).await {
        Ok(Ok(Ok(session))) => Ok(session),
        Ok(Ok(Err(err))) => Err(portal_error("create RemoteDesktop session", err)),
        Ok(Err(_)) => Err(PortalPaste::Unavailable(
            "create RemoteDesktop session: the request was dropped".to_string(),
        )),
        Err(_) => Err(PortalPaste::Unavailable(format!(
            "create RemoteDesktop session: no answer within {} s",
            PORTAL_CALL_TIMEOUT.as_secs()
        ))),
    }
}

#[cfg(target_os = "linux")]
async fn paste_in_session(
    remote: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
    saved_token: Option<&str>,
    token_path: Option<&Path>,
) -> Result<(), PortalPaste> {
    let devices = SelectDevicesOptions::default()
        .set_devices(BitFlags::from_flag(DeviceType::Keyboard))
        .set_persist_mode(PersistMode::ExplicitlyRevoked)
        .set_restore_token(saved_token);
    portal_call("select keyboard device", async {
        remote
            .select_devices(session, devices)
            .await
            .and_then(|request| request.response())
    })
    .await?;

    let started = tokio::time::timeout(PORTAL_START_TIMEOUT, async {
        remote
            .start(session, None, StartOptions::default())
            .await?
            .response()
    })
    .await;
    let selected = match started {
        Ok(Ok(selected)) => selected,
        Ok(Err(err)) => return Err(portal_error("start RemoteDesktop", err)),
        Err(_) => {
            return Err(PortalPaste::Failed(format!(
                "the permission prompt was not answered within {} s",
                PORTAL_START_TIMEOUT.as_secs()
            )))
        }
    };

    if let (Some(path), Some(token)) = (token_path, selected.restore_token()) {
        if saved_token != Some(token) {
            if let Err(err) = save_restore_token(path, token) {
                tracing::warn!("saving the RemoteDesktop restore token failed: {err}");
            }
        }
    }

    if !selected.devices().contains(DeviceType::Keyboard) {
        return Err(PortalPaste::Denied);
    }

    send_ctrl_v(remote, session)
        .await
        .map_err(PortalPaste::Failed)
}

#[cfg(target_os = "linux")]
async fn notify_key(
    remote: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
    keysym: i32,
    state: KeyState,
) -> Result<(), String> {
    match tokio::time::timeout(
        PORTAL_CALL_TIMEOUT,
        remote.notify_keyboard_keysym(
            session,
            keysym,
            state,
            NotifyKeyboardKeysymOptions::default(),
        ),
    )
    .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(format!("keyboard event failed: {err}")),
        Err(_) => Err(format!(
            "keyboard event did not answer within {} s",
            PORTAL_CALL_TIMEOUT.as_secs()
        )),
    }
}

#[cfg(target_os = "linux")]
async fn send_ctrl_v(
    remote: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
) -> Result<(), String> {
    let steps = [
        (KEYSYM_CONTROL_LEFT, KeyState::Pressed),
        (KEYSYM_V, KeyState::Pressed),
        (KEYSYM_V, KeyState::Released),
        (KEYSYM_CONTROL_LEFT, KeyState::Released),
    ];
    let mut held: Vec<i32> = Vec::new();
    let mut outcome = Ok(());
    for (index, (keysym, state)) in steps.into_iter().enumerate() {
        if index > 0 {
            tokio::time::sleep(PORTAL_KEY_GAP).await;
        }
        if matches!(state, KeyState::Pressed) {
            held.push(keysym);
        }
        if let Err(err) = notify_key(remote, session, keysym, state).await {
            outcome = Err(err);
            break;
        }
        if matches!(state, KeyState::Released) {
            held.retain(|key| *key != keysym);
        }
    }
    for keysym in held.into_iter().rev() {
        if let Err(err) = notify_key(remote, session, keysym, KeyState::Released).await {
            tracing::warn!("releasing keysym {keysym:#x} after a failed paste failed: {err}");
        }
    }
    outcome
}

#[cfg(target_os = "linux")]
fn portal_error(phase: &str, err: ashpd::Error) -> PortalPaste {
    match err {
        ashpd::Error::Response(ResponseError::Cancelled) => PortalPaste::Denied,
        other => PortalPaste::Unavailable(format!("{phase}: {other}")),
    }
}

#[cfg(target_os = "linux")]
async fn portal_call<T>(
    phase: &str,
    call: impl std::future::Future<Output = Result<T, ashpd::Error>>,
) -> Result<T, PortalPaste> {
    match tokio::time::timeout(PORTAL_CALL_TIMEOUT, call).await {
        Ok(result) => result.map_err(|err| portal_error(phase, err)),
        Err(_) => Err(PortalPaste::Unavailable(format!(
            "{phase}: no answer within {} s",
            PORTAL_CALL_TIMEOUT.as_secs()
        ))),
    }
}

#[cfg(target_os = "linux")]
fn restore_token_path() -> Option<PathBuf> {
    match crate::linux_portal::xdg_data_home() {
        Ok(dir) => Some(
            dir.join("com.synapse.voice")
                .join("portal")
                .join("remote_desktop.token"),
        ),
        Err(err) => {
            tracing::warn!("the RemoteDesktop restore token will not be stored: {err}");
            None
        }
    }
}

#[cfg(target_os = "linux")]
fn read_restore_token(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let token = contents.trim();
            (!token.is_empty()).then(|| token.to_string())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!("reading the RemoteDesktop restore token failed: {err}");
            None
        }
    }
}

#[cfg(target_os = "linux")]
fn save_restore_token(path: &Path, token: &str) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "restore token path has no parent directory",
        )
    })?;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    crate::atomic_io::write_durable(path, token.as_bytes())
}

#[cfg(not(target_os = "linux"))]
pub fn copy_to_clipboard(text: &str) -> AppResult<()> {
    let mut clipboard =
        Clipboard::new().map_err(|e| AppError::Inject(format!("clipboard open failed: {e}")))?;
    set_clipboard_with_retry(&mut clipboard, &platform_text(text))
}

#[cfg(target_os = "linux")]
pub fn copy_to_clipboard(text: &str) -> AppResult<()> {
    let platform = platform_text(text);
    with_clipboard(|clipboard| set_clipboard_with_retry(clipboard, &platform))
}

#[cfg(windows)]
fn platform_text(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains('\n') {
        std::borrow::Cow::Owned(text.replace("\r\n", "\n").replace('\n', "\r\n"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

#[cfg(not(windows))]
fn platform_text(text: &str) -> std::borrow::Cow<'_, str> {
    std::borrow::Cow::Borrowed(text)
}

#[cfg(all(test, windows))]
mod tests {
    use super::platform_text;

    #[test]
    fn windows_paste_uses_crlf() {
        assert_eq!(platform_text("a\nb"), "a\r\nb");
        assert_eq!(platform_text("a\r\nb\n\nc"), "a\r\nb\r\n\r\nc");
        assert_eq!(platform_text("one line"), "one line");
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::save_restore_token;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn restore_token_file_is_private_and_replaced() {
        let root = std::env::temp_dir().join(format!(
            "synapse-restore-token-test-{}",
            std::process::id()
        ));
        let path = root.join("portal").join("remote_desktop.token");

        save_restore_token(&path, "first-token").unwrap();
        save_restore_token(&path, "second-token").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second-token");
        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(file_mode, 0o600);
        assert_eq!(dir_mode, 0o700);
        assert_eq!(std::fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);

        let _ = std::fs::remove_dir_all(&root);
    }
}
