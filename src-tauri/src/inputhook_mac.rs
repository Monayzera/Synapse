use crate::config::{RecordMode, Settings};
use crate::pipeline;
use crate::state::SharedState;
use parking_lot::RwLock;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;
use std::time::Duration;
use tauri::{AppHandle, Manager};

type CFMachPortRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFAllocatorRef = *mut c_void;
type CFStringRef = *const c_void;
type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;

type CFDictionaryRef = *const c_void;

type CGEventTapCallBack =
    extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRun();
    fn CFDictionaryCreate(
        allocator: CFAllocatorRef,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
    ) -> CFDictionaryRef;
    fn CFRelease(cf: *const c_void);
    #[allow(non_upper_case_globals)]
    static kCFRunLoopCommonModes: CFStringRef;
    #[allow(non_upper_case_globals)]
    static kCFBooleanTrue: *const c_void;
}

const KEY_DOWN: u32 = 10;
const KEY_UP: u32 = 11;
const TAP_DISABLED_TIMEOUT: u32 = 0xFFFF_FFFE;
const TAP_DISABLED_USER_INPUT: u32 = 0xFFFF_FFFF;
const FIELD_KEYCODE: u32 = 9;

const FLAG_SHIFT: u64 = 0x0002_0000;
const FLAG_CONTROL: u64 = 0x0004_0000;
const FLAG_ALT: u64 = 0x0008_0000;
const FLAG_COMMAND: u64 = 0x0010_0000;

struct TapPort(CFMachPortRef);
unsafe impl Send for TapPort {}
unsafe impl Sync for TapPort {}

struct Binding {
    ctrl: bool,
    shift: bool,
    alt: bool,
    meta: bool,
    keycode: u16,
    active: AtomicBool,
}

enum HotEvent {
    Press,
    Release,
}

static BINDINGS: OnceLock<RwLock<Vec<Binding>>> = OnceLock::new();
static DISPATCH: OnceLock<Sender<HotEvent>> = OnceLock::new();
static TAP_PORT: OnceLock<TapPort> = OnceLock::new();

fn bindings() -> &'static RwLock<Vec<Binding>> {
    BINDINGS.get_or_init(|| RwLock::new(Vec::new()))
}

pub fn set_bindings(settings: &Settings) -> bool {
    let mut list = Vec::new();
    if let Some(binding) = parse_binding(&settings.hotkey_ptt) {
        list.push(binding);
    }
    let parsed = !list.is_empty();
    *bindings().write() = list;
    parsed
}

pub fn start(app: AppHandle) {
    let (tx, rx) = channel::<HotEvent>();
    if DISPATCH.set(tx).is_err() {
        return;
    }

    let dispatch_app = app.clone();
    let dispatch_spawn = std::thread::Builder::new()
        .name("synapse-hotkey-dispatch".to_string())
        .spawn(move || loop {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                while let Ok(event) = rx.recv() {
                    dispatch(&dispatch_app, event);
                }
            }));
            match outcome {
                Ok(()) => return,
                Err(_) => tracing::error!("hotkey dispatcher panicked; restarting it"),
            }
        });
    if let Err(err) = dispatch_spawn {
        tracing::error!("could not start thread synapse-hotkey-dispatch: {err}");
    }

    let grab_app = app.clone();
    let grab_spawn = std::thread::Builder::new()
        .name("synapse-hotkey-grab".to_string())
        .spawn(move || {
            let app = grab_app;
            let trusted = prompt_accessibility();
            let mut warned = false;
            let mut attempts: u32 = 0;
            loop {
                if run_event_tap(&app) {
                    break;
                }
                attempts += 1;
                crate::hotkey::set_hook_ready(&app, false);
                if !warned {
                    warned = true;
                    let message = if trusted || unsafe { AXIsProcessTrusted() } {
                        tracing::error!("hotkey tap denied despite Accessibility shown as granted; stale TCC entry");
                        "macOS is blocking the global shortcut even though Accessibility looks enabled (stale permission from a previous build). In System Settings > Privacy & Security > Accessibility, remove Synapse with the minus button, add it again and turn it on."
                    } else {
                        tracing::error!("global hotkey tap failed; needs Accessibility permission");
                        open_accessibility_settings();
                        "Allow Synapse in System Settings > Privacy & Security > Accessibility. The global shortcut starts working as soon as you enable it."
                    };
                    crate::hotkey::report_failure(&app, message);
                } else {
                    tracing::debug!("hotkey tap retry {attempts} failed");
                }
                let backoff = if attempts < 10 { 3 } else { 30 };
                std::thread::sleep(Duration::from_secs(backoff));
            }
        });
    if let Err(err) = grab_spawn {
        tracing::error!("could not start thread synapse-hotkey-grab: {err}");
        crate::hotkey::set_hook_ready(&app, false);
    }
}

fn prompt_accessibility() -> bool {
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt as *const c_void];
        let values = [kCFBooleanTrue];
        let options = CFDictionaryCreate(
            ptr::null_mut(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            ptr::null(),
            ptr::null(),
        );
        let trusted = AXIsProcessTrustedWithOptions(options);
        if !options.is_null() {
            CFRelease(options);
        }
        trusted
    }
}

fn run_event_tap(app: &AppHandle) -> bool {
    unsafe {
        let mask: u64 = (1u64 << KEY_DOWN) | (1u64 << KEY_UP);
        let port = CGEventTapCreate(0, 0, 0, mask, tap_callback, ptr::null_mut());
        if port.is_null() {
            return false;
        }
        let source = CFMachPortCreateRunLoopSource(ptr::null_mut(), port, 0);
        if source.is_null() {
            return false;
        }
        let runloop = CFRunLoopGetCurrent();
        CFRunLoopAddSource(runloop, source, kCFRunLoopCommonModes);
        let _ = TAP_PORT.set(TapPort(port));
        CGEventTapEnable(port, true);
        tracing::info!("global shortcut event tap installed");
        crate::hotkey::set_hook_ready(app, true);
        CFRunLoopRun();
        true
    }
}

extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    etype: u32,
    event: CGEventRef,
    _user: *mut c_void,
) -> CGEventRef {
    if etype == TAP_DISABLED_TIMEOUT || etype == TAP_DISABLED_USER_INPUT {
        if let Some(port) = TAP_PORT.get() {
            unsafe { CGEventTapEnable(port.0, true) };
        }
        return event;
    }

    let is_down = etype == KEY_DOWN;
    let is_up = etype == KEY_UP;
    if !is_down && !is_up {
        return event;
    }

    let keycode = unsafe { CGEventGetIntegerValueField(event, FIELD_KEYCODE) } as u16;
    let flags = unsafe { CGEventGetFlags(event) };

    if handle(keycode, is_down, flags) {
        ptr::null_mut()
    } else {
        event
    }
}

fn open_accessibility_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn();
}

fn dispatch(app: &AppHandle, event: HotEvent) {
    let state = match app.try_state::<SharedState>() {
        Some(state) => state.inner().clone(),
        None => return,
    };
    let mode = state.settings.read().record_mode;
    match event {
        HotEvent::Press => match mode {
            RecordMode::PushToTalk => pipeline::begin_recording(&state),
            RecordMode::Toggle => pipeline::hotkey_toggle(app.clone(), state.clone()),
        },
        HotEvent::Release => {
            if mode == RecordMode::PushToTalk {
                pipeline::finish_recording(app.clone(), state.clone());
            }
        }
    }
}

fn send(event: HotEvent) {
    if let Some(tx) = DISPATCH.get() {
        let _ = tx.send(event);
    }
}

fn modifiers_match(binding: &Binding, flags: u64) -> bool {
    let ctrl = flags & FLAG_CONTROL != 0;
    let shift = flags & FLAG_SHIFT != 0;
    let alt = flags & FLAG_ALT != 0;
    let meta = flags & FLAG_COMMAND != 0;
    ctrl == binding.ctrl && shift == binding.shift && alt == binding.alt && meta == binding.meta
}

fn should_swallow(binding: &Binding) -> bool {
    binding.ctrl || binding.shift || binding.alt || binding.meta
}

fn handle(keycode: u16, is_down: bool, flags: u64) -> bool {
    let guard = bindings().read();
    let mut swallow = false;
    for binding in guard.iter() {
        if binding.keycode != keycode {
            continue;
        }
        if is_down {
            if binding.active.load(Ordering::Relaxed) {
                if should_swallow(binding) {
                    swallow = true;
                }
            } else if modifiers_match(binding, flags) {
                binding.active.store(true, Ordering::Relaxed);
                send(HotEvent::Press);
                if should_swallow(binding) {
                    swallow = true;
                }
            }
        } else if binding.active.swap(false, Ordering::Relaxed) {
            send(HotEvent::Release);
            if should_swallow(binding) {
                swallow = true;
            }
        }
    }
    swallow
}

fn parse_binding(accelerator: &str) -> Option<Binding> {
    let trimmed = accelerator.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut meta = false;
    let mut keycode = None;
    for part in trimmed.split('+') {
        let token = part.trim();
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "commandorcontrol" => ctrl = true,
            "shift" => shift = true,
            "alt" | "option" => alt = true,
            "super" | "win" | "meta" | "cmd" | "command" => meta = true,
            _ => keycode = token_to_keycode(token),
        }
    }
    Some(Binding {
        ctrl,
        shift,
        alt,
        meta,
        keycode: keycode?,
        active: AtomicBool::new(false),
    })
}

fn token_to_keycode(token: &str) -> Option<u16> {
    let upper = token.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    if bytes.len() == 1 {
        let c = bytes[0];
        if c.is_ascii_uppercase() {
            return letter_keycode(c);
        }
        if c.is_ascii_digit() {
            return digit_keycode(c);
        }
    }
    if let Some(rest) = upper.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u32>() {
            return fkey_keycode(n);
        }
    }
    let code = match upper.as_str() {
        "SPACE" => 49,
        "ENTER" | "RETURN" => 36,
        "TAB" => 48,
        "ESCAPE" | "ESC" => 53,
        "BACKSPACE" => 51,
        "DELETE" | "DEL" => 117,
        "INSERT" | "INS" => 114,
        "HOME" => 115,
        "END" => 119,
        "PAGEUP" => 116,
        "PAGEDOWN" => 121,
        "UP" => 126,
        "DOWN" => 125,
        "LEFT" => 123,
        "RIGHT" => 124,
        "`" | "BACKQUOTE" => 50,
        "-" | "MINUS" => 27,
        "=" | "EQUAL" => 24,
        "[" => 33,
        "]" => 30,
        "\\" => 42,
        ";" => 41,
        "'" => 39,
        "," => 43,
        "." => 47,
        "/" => 44,
        _ => return None,
    };
    Some(code)
}

fn letter_keycode(c: u8) -> Option<u16> {
    let code = match c {
        b'A' => 0,
        b'B' => 11,
        b'C' => 8,
        b'D' => 2,
        b'E' => 14,
        b'F' => 3,
        b'G' => 5,
        b'H' => 4,
        b'I' => 34,
        b'J' => 38,
        b'K' => 40,
        b'L' => 37,
        b'M' => 46,
        b'N' => 45,
        b'O' => 31,
        b'P' => 35,
        b'Q' => 12,
        b'R' => 15,
        b'S' => 1,
        b'T' => 17,
        b'U' => 32,
        b'V' => 9,
        b'W' => 13,
        b'X' => 7,
        b'Y' => 16,
        b'Z' => 6,
        _ => return None,
    };
    Some(code)
}

fn digit_keycode(c: u8) -> Option<u16> {
    let code = match c {
        b'0' => 29,
        b'1' => 18,
        b'2' => 19,
        b'3' => 20,
        b'4' => 21,
        b'5' => 23,
        b'6' => 22,
        b'7' => 26,
        b'8' => 28,
        b'9' => 25,
        _ => return None,
    };
    Some(code)
}

fn fkey_keycode(n: u32) -> Option<u16> {
    let code = match n {
        1 => 122,
        2 => 120,
        3 => 99,
        4 => 118,
        5 => 96,
        6 => 97,
        7 => 98,
        8 => 100,
        9 => 101,
        10 => 109,
        11 => 103,
        12 => 111,
        _ => return None,
    };
    Some(code)
}
