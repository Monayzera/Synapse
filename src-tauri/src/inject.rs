use crate::error::{AppError, AppResult};
use arboard::Clipboard;
#[cfg(not(target_os = "macos"))]
use enigo::{Direction, Enigo, Key, Keyboard, Settings as EnigoSettings};
use std::time::Duration;

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

pub fn inject_text(text: &str, restore_clipboard: bool, paste_delay_ms: u64) -> AppResult<()> {
    if text.trim().is_empty() {
        return Ok(());
    }

    let mut clipboard =
        Clipboard::new().map_err(|e| AppError::Inject(format!("clipboard open failed: {e}")))?;

    let previous = clipboard.get_text().ok();

    set_clipboard_with_retry(&mut clipboard, text)?;

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
fn send_paste() -> AppResult<()> {
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

pub fn copy_to_clipboard(text: &str) -> AppResult<()> {
    let mut clipboard =
        Clipboard::new().map_err(|e| AppError::Inject(format!("clipboard open failed: {e}")))?;
    set_clipboard_with_retry(&mut clipboard, text)
}
