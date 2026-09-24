use crate::config::{RecordMode, Settings};
use crate::permissions::AccessibilityIssue;
use crate::pipeline;
use crate::state::SharedState;
use parking_lot::RwLock;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
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
    fn CGEventTapIsEnabled(tap: CFMachPortRef) -> bool;
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
    fn CFMachPortInvalidate(port: CFMachPortRef);
    fn CFMachPortIsValid(port: CFMachPortRef) -> u8;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRunInMode(mode: CFStringRef, seconds: f64, return_after_source_handled: u8) -> i32;
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
    static kCFRunLoopDefaultMode: CFStringRef;
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

const RUN_LOOP_FINISHED: i32 = 1;
const RUN_LOOP_STOPPED: i32 = 2;
const TRUST_CHECK_SECS: f64 = 1.0;
const UNTRUSTED_RETRY: Duration = Duration::from_secs(5);
const MIC_PROMPT_WAIT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, PartialEq, Eq)]
enum TapExit {
    Denied,
    NoSource,
    Revoked,
    Lost,
}

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
static TAP_PORT: AtomicPtr<c_void> = AtomicPtr::new(ptr::null_mut());

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
            if !unsafe { AXIsProcessTrusted() } && crate::permissions::mic_prompt_pending() {
                crate::hotkey::set_hook_ready(&app, false);
                wait_for_mic_prompt();
            }
            let trusted = prompt_accessibility();
            tracing::info!("Accessibility trusted at startup: {trusted}");
            let mut warned = false;
            let mut attempts: u32 = 0;
            loop {
                let exit = run_event_tap(&app);
                release_active();
                crate::hotkey::set_hook_ready(&app, false);
                let trusted = unsafe { AXIsProcessTrusted() };
                if matches!(exit, TapExit::Revoked | TapExit::Lost) {
                    warned = false;
                    attempts = 0;
                }
                match exit {
                    TapExit::Revoked => {
                        tracing::warn!("Accessibility permission revoked; global shortcut tap removed")
                    }
                    TapExit::Lost => tracing::warn!("global shortcut event tap stopped; reinstalling"),
                    TapExit::NoSource | TapExit::Denied => {}
                }
                attempts = attempts.saturating_add(1);
                let issue = match exit {
                    TapExit::Denied if trusted => Some(AccessibilityIssue::Stale),
                    _ if !trusted => Some(AccessibilityIssue::Missing),
                    _ => None,
                };
                if let Some(issue) = issue {
                    if !warned {
                        warned = true;
                        match issue {
                            AccessibilityIssue::Stale => tracing::error!(
                                "hotkey tap denied despite Accessibility shown as granted; stale TCC entry"
                            ),
                            AccessibilityIssue::Missing => tracing::error!(
                                "global hotkey tap failed; needs Accessibility permission"
                            ),
                        }
                    } else {
                        tracing::debug!("hotkey tap retry {attempts} failed");
                    }
                    crate::permissions::set_accessibility_issue(&app, Some(issue));
                }
                let mut backoff = Duration::from_secs(if attempts < 10 { 3 } else { 30 });
                if issue.is_some() {
                    backoff = backoff.min(UNTRUSTED_RETRY);
                }
                std::thread::sleep(backoff);
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

fn wait_for_mic_prompt() {
    tracing::info!("waiting for the microphone prompt before asking for Accessibility");
    let deadline = Instant::now() + MIC_PROMPT_WAIT;
    while crate::permissions::mic_prompt_pending() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(250));
    }
}

fn release_active() {
    let guard = bindings().read();
    for binding in guard.iter() {
        if binding.active.swap(false, Ordering::Relaxed) {
            send(HotEvent::Release);
        }
    }
}

fn run_event_tap(app: &AppHandle) -> TapExit {
    unsafe {
        let mask: u64 = (1u64 << KEY_DOWN) | (1u64 << KEY_UP);
        let port = CGEventTapCreate(0, 0, 0, mask, tap_callback, ptr::null_mut());
        if port.is_null() {
            return TapExit::Denied;
        }
        let source = CFMachPortCreateRunLoopSource(ptr::null_mut(), port, 0);
        if source.is_null() {
            tracing::warn!("event tap run loop source could not be created");
            CFMachPortInvalidate(port);
            CFRelease(port as *const c_void);
            return TapExit::NoSource;
        }
        let runloop = CFRunLoopGetCurrent();
        CFRunLoopAddSource(runloop, source, kCFRunLoopCommonModes);
        TAP_PORT.store(port, Ordering::Release);
        CGEventTapEnable(port, true);
        tracing::info!("global shortcut event tap installed");
        crate::permissions::set_accessibility_issue(app, None);
        crate::hotkey::set_hook_ready(app, true);
        let exit = loop {
            let result = CFRunLoopRunInMode(kCFRunLoopDefaultMode, TRUST_CHECK_SECS, 0);
            if !AXIsProcessTrusted() {
                break TapExit::Revoked;
            }
            if result == RUN_LOOP_FINISHED
                || result == RUN_LOOP_STOPPED
                || CFMachPortIsValid(port) == 0
            {
                break TapExit::Lost;
            }
            if !CGEventTapIsEnabled(port) {
                CGEventTapEnable(port, true);
                if !CGEventTapIsEnabled(port) {
                    tracing::warn!("global shortcut event tap could not be enabled again; reinstalling it");
                    break TapExit::Lost;
                }
                tracing::info!("global shortcut event tap was disabled; enabled it again");
            }
        };
        TAP_PORT.store(ptr::null_mut(), Ordering::Release);
        if CFMachPortIsValid(port) != 0 {
            CGEventTapEnable(port, false);
        }
        CFRunLoopRemoveSource(runloop, source, kCFRunLoopCommonModes);
        CFMachPortInvalidate(port);
        CFRelease(source as *const c_void);
        CFRelease(port as *const c_void);
        exit
    }
}

extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    etype: u32,
    event: CGEventRef,
    _user: *mut c_void,
) -> CGEventRef {
    if etype == TAP_DISABLED_TIMEOUT || etype == TAP_DISABLED_USER_INPUT {
        let port = TAP_PORT.load(Ordering::Acquire);
        if !port.is_null() && unsafe { AXIsProcessTrusted() } {
            unsafe { CGEventTapEnable(port, true) };
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
