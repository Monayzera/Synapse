use crate::config::{RecordMode, Settings};
use crate::pipeline;
use crate::state::SharedState;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::System::Threading::{
    GetCurrentThread, GetCurrentThreadId, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetLastInputInfo, LASTINPUTINFO, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN,
    VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, PeekMessageW, PostThreadMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HC_ACTION, HHOOK, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, MSLLHOOKSTRUCT,
    PM_NOREMOVE, PM_REMOVE, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_APP, WM_KEYDOWN, WM_KEYUP,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SYSKEYDOWN, WM_SYSKEYUP, WM_USER, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

const XBUTTON1: u16 = 0x0001;
const XBUTTON2: u16 = 0x0002;
const WM_REINSTALL: u32 = WM_APP + 0x51;
const KIND_KEY: u64 = 1;
const KIND_MOUSE: u64 = 2;
const WATCHDOG_TICK: Duration = Duration::from_millis(100);
const LIVENESS_EVERY_TICKS: u32 = 50;
const REPEAT_SILENCE_MS: u32 = 1500;
const RECENT_INPUT_MS: u32 = 10_000;
const HOOK_LAG_SLACK_MS: u32 = 3_000;
const LIVENESS_COOLDOWN: Duration = Duration::from_secs(30);
const LIVENESS_STRIKES: u32 = 2;
const INSTALL_RETRY_MIN: Duration = Duration::from_millis(500);
const INSTALL_RETRY_MAX: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MouseBtn {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

impl MouseBtn {
    fn code(self) -> u16 {
        match self {
            MouseBtn::Left => 1,
            MouseBtn::Right => 2,
            MouseBtn::Middle => 3,
            MouseBtn::X1 => 4,
            MouseBtn::X2 => 5,
        }
    }

    fn from_code(code: u16) -> Option<MouseBtn> {
        match code {
            1 => Some(MouseBtn::Left),
            2 => Some(MouseBtn::Right),
            3 => Some(MouseBtn::Middle),
            4 => Some(MouseBtn::X1),
            5 => Some(MouseBtn::X2),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Key(u32),
    Mouse(MouseBtn),
}

#[derive(Clone, Copy)]
struct Binding {
    ctrl: bool,
    shift: bool,
    alt: bool,
    win: bool,
    trigger: Trigger,
}

impl Binding {
    fn pack(&self) -> u64 {
        let (kind, code) = match self.trigger {
            Trigger::Key(vk) => (KIND_KEY, (vk & 0xFFFF) as u64),
            Trigger::Mouse(button) => (KIND_MOUSE, button.code() as u64),
        };
        code | (kind << 16)
            | ((self.ctrl as u64) << 20)
            | ((self.shift as u64) << 21)
            | ((self.alt as u64) << 22)
            | ((self.win as u64) << 23)
    }

    fn unpack(value: u64) -> Option<Binding> {
        let code = (value & 0xFFFF) as u16;
        let trigger = match (value >> 16) & 0x3 {
            KIND_KEY => Trigger::Key(code as u32),
            KIND_MOUSE => Trigger::Mouse(MouseBtn::from_code(code)?),
            _ => return None,
        };
        Some(Binding {
            ctrl: value & (1 << 20) != 0,
            shift: value & (1 << 21) != 0,
            alt: value & (1 << 22) != 0,
            win: value & (1 << 23) != 0,
            trigger,
        })
    }

    fn swallows(&self) -> bool {
        self.ctrl || self.shift || self.alt || self.win || matches!(self.trigger, Trigger::Mouse(_))
    }
}

enum HotEvent {
    Press,
    Release,
}

static BINDING: AtomicU64 = AtomicU64::new(0);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static INSTALLED: AtomicBool = AtomicBool::new(false);
static LAST_TRIGGER_DOWN: AtomicU32 = AtomicU32::new(0);
static LAST_HOOK_EVENT: AtomicU32 = AtomicU32::new(0);
static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);
static DISPATCH: OnceLock<Sender<HotEvent>> = OnceLock::new();

fn tick_now() -> u32 {
    unsafe { GetTickCount() }
}

pub fn set_bindings(settings: &Settings) -> bool {
    let parsed = parse_binding(&settings.hotkey_ptt);
    let packed = parsed.map(|binding| binding.pack()).unwrap_or(0);
    let previous = BINDING.swap(packed, Ordering::AcqRel);
    if previous != packed {
        reset_active("shortcut changed");
    }
    parsed.is_some()
}

pub fn reset_active(reason: &str) {
    if ACTIVE.swap(false, Ordering::AcqRel) {
        tracing::info!("hotkey state reset ({reason}); releasing the held shortcut");
        send(HotEvent::Release);
    }
}

pub fn request_reinstall(reason: &str) {
    let thread_id = HOOK_THREAD_ID.load(Ordering::Acquire);
    if thread_id == 0 {
        return;
    }
    tracing::info!("global shortcut hook reinstall requested ({reason})");
    if let Err(err) = unsafe { PostThreadMessageW(thread_id, WM_REINSTALL, WPARAM(0), LPARAM(0)) } {
        tracing::warn!("could not request hook reinstall: {err}");
    }
}

fn spawn_named<F>(name: &str, body: F) -> bool
where
    F: FnOnce() + Send + 'static,
{
    match std::thread::Builder::new().name(name.to_string()).spawn(body) {
        Ok(_) => true,
        Err(err) => {
            tracing::error!("could not start thread {name}: {err}");
            false
        }
    }
}

pub fn start(app: AppHandle) {
    let (tx, rx) = channel::<HotEvent>();
    if DISPATCH.set(tx).is_err() {
        return;
    }

    let dispatch_app = app.clone();
    spawn_named("synapse-hotkey-dispatch", move || dispatch_loop(dispatch_app, rx));

    let hook_app = app.clone();
    if !spawn_named("synapse-hotkey-hook", move || hook_thread(hook_app)) {
        crate::hotkey::set_hook_ready(&app, false);
        crate::hotkey::report_failure(&app, "The global shortcut thread could not be started.");
    }

    spawn_named("synapse-hotkey-watchdog", watchdog_loop);
}

fn dispatch_loop(app: AppHandle, rx: Receiver<HotEvent>) {
    loop {
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            while let Ok(event) = rx.recv() {
                dispatch(&app, event);
            }
        }));
        match outcome {
            Ok(()) => return,
            Err(_) => tracing::error!("hotkey dispatcher panicked; restarting it"),
        }
    }
}

struct Hooks {
    keyboard: Option<HHOOK>,
    mouse: Option<HHOOK>,
}

impl Hooks {
    fn install(&mut self) -> Result<(), String> {
        let hmod = unsafe { GetModuleHandleW(None) }
            .map(|h| HINSTANCE(h.0))
            .unwrap_or(HINSTANCE(std::ptr::null_mut()));
        let mut errors = Vec::new();
        if self.keyboard.is_none() {
            match unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(hmod), 0) } {
                Ok(hook) => self.keyboard = Some(hook),
                Err(err) => errors.push(format!("keyboard hook: {err}")),
            }
        }
        if self.mouse.is_none() {
            match unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hmod), 0) } {
                Ok(hook) => self.mouse = Some(hook),
                Err(err) => errors.push(format!("mouse hook: {err}")),
            }
        }
        if errors.is_empty() {
            INSTALLED.store(true, Ordering::Release);
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn uninstall(&mut self) {
        INSTALLED.store(false, Ordering::Release);
        for hook in [self.keyboard.take(), self.mouse.take()].into_iter().flatten() {
            if let Err(err) = unsafe { UnhookWindowsHookEx(hook) } {
                tracing::debug!("unhook failed: {err}");
            }
        }
    }

    fn install_with_retry(&mut self, app: &AppHandle, reason: &str) {
        let mut delay = INSTALL_RETRY_MIN;
        let mut attempt: u32 = 0;
        loop {
            attempt = attempt.saturating_add(1);
            match self.install() {
                Ok(()) => {
                    LAST_HOOK_EVENT.store(tick_now(), Ordering::Release);
                    tracing::info!("global shortcut hooks installed ({reason}, attempt {attempt})");
                    crate::hotkey::set_hook_ready(app, true);
                    return;
                }
                Err(err) => {
                    self.uninstall();
                    if attempt == 1 || attempt % 10 == 0 {
                        tracing::error!(
                            "global shortcut hook install failed ({reason}, attempt {attempt}): {err}"
                        );
                    }
                    crate::hotkey::set_hook_ready(app, false);
                    crate::hotkey::report_failure(
                        app,
                        "The global shortcut could not be installed; retrying automatically.",
                    );
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(INSTALL_RETRY_MAX);
                }
            }
        }
    }
}

fn hook_thread(app: AppHandle) {
    unsafe {
        if let Err(err) = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) {
            tracing::warn!("could not raise hotkey hook thread priority: {err}");
        }
        let mut probe = MSG::default();
        let _ = PeekMessageW(&mut probe, None, WM_USER, WM_USER, PM_NOREMOVE);
        HOOK_THREAD_ID.store(GetCurrentThreadId(), Ordering::Release);
    }

    let mut hooks = Hooks {
        keyboard: None,
        mouse: None,
    };
    hooks.install_with_retry(&app, "startup");

    let mut msg = MSG::default();
    loop {
        let result = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if result.0 == 0 {
            tracing::info!("hotkey hook thread received quit");
            break;
        }
        if result.0 < 0 {
            tracing::error!("hotkey hook message loop failed; continuing");
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        if msg.message == WM_REINSTALL {
            let mut pending = MSG::default();
            while unsafe {
                PeekMessageW(&mut pending, None, WM_REINSTALL, WM_REINSTALL, PM_REMOVE)
            }
            .as_bool()
            {}
            hooks.uninstall();
            reset_active("hook reinstall");
            hooks.install_with_retry(&app, "reinstall");
        }
    }

    hooks.uninstall();
    HOOK_THREAD_ID.store(0, Ordering::Release);
    crate::hotkey::set_hook_ready(&app, false);
}

fn watchdog_loop() {
    let mut streak: u32 = 0;
    let mut ticks: u32 = 0;
    let mut suspicious: u32 = 0;
    let mut last_reinstall: Option<Instant> = None;
    loop {
        std::thread::sleep(WATCHDOG_TICK);
        let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| {
            streak = check_stuck(streak);
            ticks = ticks.wrapping_add(1);
            if ticks % LIVENESS_EVERY_TICKS == 0 {
                suspicious = check_liveness(suspicious, &mut last_reinstall);
            }
        }));
        if outcome.is_err() {
            tracing::error!("hotkey watchdog panicked; continuing");
            streak = 0;
            suspicious = 0;
        }
    }
}

fn check_stuck(streak: u32) -> u32 {
    if !ACTIVE.load(Ordering::Acquire) {
        return 0;
    }
    let binding = match Binding::unpack(BINDING.load(Ordering::Acquire)) {
        Some(binding) => binding,
        None => return 0,
    };
    let vk = match binding.trigger {
        Trigger::Key(vk) => vk as i32,
        Trigger::Mouse(_) => return 0,
    };
    let released = if binding.swallows() {
        let modifier_up = (binding.ctrl && !key_down(VK_CONTROL.0 as i32))
            || (binding.shift && !key_down(VK_SHIFT.0 as i32))
            || (binding.alt && !key_down(VK_MENU.0 as i32))
            || (binding.win && !(key_down(VK_LWIN.0 as i32) || key_down(VK_RWIN.0 as i32)));
        let quiet = tick_now().wrapping_sub(LAST_TRIGGER_DOWN.load(Ordering::Acquire))
            > REPEAT_SILENCE_MS;
        modifier_up && quiet
    } else {
        !key_down(vk)
    };
    if !released {
        return 0;
    }
    let streak = streak.saturating_add(1);
    if streak >= 2 {
        if ACTIVE.swap(false, Ordering::AcqRel) {
            tracing::warn!("shortcut release was not observed; watchdog released it");
            send(HotEvent::Release);
        }
        return 0;
    }
    streak
}

fn check_liveness(suspicious: u32, last_reinstall: &mut Option<Instant>) -> u32 {
    if !INSTALLED.load(Ordering::Acquire) || crate::power::session_locked() {
        return 0;
    }
    let mut info = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return suspicious;
    }
    let now = tick_now();
    let idle = now.wrapping_sub(info.dwTime);
    if idle > RECENT_INPUT_MS {
        return suspicious;
    }
    let hook_age = now.wrapping_sub(LAST_HOOK_EVENT.load(Ordering::Acquire));
    if hook_age <= idle.saturating_add(HOOK_LAG_SLACK_MS) {
        return 0;
    }
    let suspicious = suspicious.saturating_add(1);
    if suspicious < LIVENESS_STRIKES {
        return suspicious;
    }
    if let Some(at) = last_reinstall {
        if at.elapsed() < LIVENESS_COOLDOWN {
            return suspicious;
        }
    }
    *last_reinstall = Some(Instant::now());
    tracing::warn!(
        "global shortcut hook looks inactive (last input {idle} ms ago, last hook event {hook_age} ms ago)"
    );
    request_reinstall("liveness check");
    0
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
            RecordMode::Toggle => pipeline::hotkey_toggle(app.clone(), state),
        },
        HotEvent::Release => {
            if mode == RecordMode::PushToTalk {
                pipeline::finish_recording(app.clone(), state);
            }
        }
    }
}

fn key_down(vk: i32) -> bool {
    unsafe { (GetAsyncKeyState(vk) as u16 & 0x8000) != 0 }
}

fn modifiers_match(binding: &Binding) -> bool {
    let ctrl = key_down(VK_CONTROL.0 as i32);
    let shift = key_down(VK_SHIFT.0 as i32);
    let alt = key_down(VK_MENU.0 as i32);
    let win = key_down(VK_LWIN.0 as i32) || key_down(VK_RWIN.0 as i32);
    ctrl == binding.ctrl && shift == binding.shift && alt == binding.alt && win == binding.win
}

fn handle(trigger: Trigger, is_down: bool, now: u32) -> bool {
    let binding = match Binding::unpack(BINDING.load(Ordering::Acquire)) {
        Some(binding) => binding,
        None => return false,
    };
    if binding.trigger != trigger {
        return false;
    }
    let swallow = binding.swallows();
    if is_down {
        if ACTIVE.load(Ordering::Acquire) {
            LAST_TRIGGER_DOWN.store(now, Ordering::Release);
            return swallow;
        }
        if modifiers_match(&binding) {
            LAST_TRIGGER_DOWN.store(now, Ordering::Release);
            ACTIVE.store(true, Ordering::Release);
            send(HotEvent::Press);
            return swallow;
        }
        false
    } else if ACTIVE.swap(false, Ordering::AcqRel) {
        send(HotEvent::Release);
        swallow
    } else {
        false
    }
}

fn send(event: HotEvent) {
    if let Some(tx) = DISPATCH.get() {
        let _ = tx.send(event);
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        let now = tick_now();
        LAST_HOOK_EVENT.store(now, Ordering::Relaxed);
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        if info.flags.0 & LLKHF_INJECTED.0 == 0 {
            let msg = wparam.0 as u32;
            let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
            if is_down || is_up {
                let vk = info.vkCode;
                let swallow =
                    std::panic::catch_unwind(move || handle(Trigger::Key(vk), is_down, now))
                        .unwrap_or(false);
                if swallow {
                    return LRESULT(1);
                }
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 && lparam.0 != 0 {
        let now = tick_now();
        LAST_HOOK_EVENT.store(now, Ordering::Relaxed);
        let info = &*(lparam.0 as *const MSLLHOOKSTRUCT);
        let msg = wparam.0 as u32;
        let parsed = match msg {
            WM_LBUTTONDOWN => Some((MouseBtn::Left, true)),
            WM_LBUTTONUP => Some((MouseBtn::Left, false)),
            WM_RBUTTONDOWN => Some((MouseBtn::Right, true)),
            WM_RBUTTONUP => Some((MouseBtn::Right, false)),
            WM_MBUTTONDOWN => Some((MouseBtn::Middle, true)),
            WM_MBUTTONUP => Some((MouseBtn::Middle, false)),
            WM_XBUTTONDOWN => x_button(info).map(|b| (b, true)),
            WM_XBUTTONUP => x_button(info).map(|b| (b, false)),
            _ => None,
        };
        if let Some((button, is_down)) = parsed {
            let swallow =
                std::panic::catch_unwind(move || handle(Trigger::Mouse(button), is_down, now))
                    .unwrap_or(false);
            if swallow {
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

fn x_button(info: &MSLLHOOKSTRUCT) -> Option<MouseBtn> {
    let high = (info.mouseData >> 16) as u16;
    match high {
        XBUTTON1 => Some(MouseBtn::X1),
        XBUTTON2 => Some(MouseBtn::X2),
        _ => None,
    }
}

fn parse_binding(accelerator: &str) -> Option<Binding> {
    let trimmed = accelerator.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut win = false;
    let mut trigger = None;
    for part in trimmed.split('+') {
        let token = part.trim();
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "commandorcontrol" => ctrl = true,
            "shift" => shift = true,
            "alt" | "option" => alt = true,
            "super" | "win" | "meta" | "cmd" | "command" => win = true,
            _ => trigger = parse_trigger(token),
        }
    }
    Some(Binding {
        ctrl,
        shift,
        alt,
        win,
        trigger: trigger?,
    })
}

fn parse_trigger(token: &str) -> Option<Trigger> {
    match token.to_ascii_lowercase().as_str() {
        "mouseleft" => return Some(Trigger::Mouse(MouseBtn::Left)),
        "mouseright" => return Some(Trigger::Mouse(MouseBtn::Right)),
        "mousemiddle" | "mouse3" => return Some(Trigger::Mouse(MouseBtn::Middle)),
        "mouseback" | "mouse4" | "x1" => return Some(Trigger::Mouse(MouseBtn::X1)),
        "mouseforward" | "mouse5" | "x2" => return Some(Trigger::Mouse(MouseBtn::X2)),
        _ => {}
    }
    key_to_vk(token).map(Trigger::Key)
}

fn key_to_vk(token: &str) -> Option<u32> {
    let upper = token.to_ascii_uppercase();
    let bytes = upper.as_bytes();
    if bytes.len() == 1 {
        let c = bytes[0];
        if c.is_ascii_uppercase() || c.is_ascii_digit() {
            return Some(c as u32);
        }
    }
    if let Some(rest) = upper.strip_prefix('F') {
        if let Ok(n) = rest.parse::<u32>() {
            if (1..=24).contains(&n) {
                return Some(0x70 + (n - 1));
            }
        }
    }
    let vk = match upper.as_str() {
        "SPACE" => 0x20,
        "ENTER" | "RETURN" => 0x0D,
        "TAB" => 0x09,
        "ESCAPE" | "ESC" => 0x1B,
        "BACKSPACE" => 0x08,
        "DELETE" | "DEL" => 0x2E,
        "INSERT" | "INS" => 0x2D,
        "HOME" => 0x24,
        "END" => 0x23,
        "PAGEUP" => 0x21,
        "PAGEDOWN" => 0x22,
        "UP" => 0x26,
        "DOWN" => 0x28,
        "LEFT" => 0x25,
        "RIGHT" => 0x27,
        "`" | "BACKQUOTE" => 0xC0,
        "-" | "MINUS" => 0xBD,
        "=" | "EQUAL" => 0xBB,
        "[" => 0xDB,
        "]" => 0xDD,
        "\\" => 0xDC,
        ";" => 0xBA,
        "'" => 0xDE,
        "," => 0xBC,
        "." => 0xBE,
        "/" => 0xBF,
        _ => return None,
    };
    Some(vk)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_pack_roundtrip() {
        for accelerator in ["Ctrl+Shift+Space", "F9", "Alt+MouseForward", "Win+Ctrl+Z"] {
            let binding = parse_binding(accelerator).unwrap();
            let unpacked = Binding::unpack(binding.pack()).unwrap();
            assert!(unpacked.trigger == binding.trigger);
            assert_eq!(unpacked.ctrl, binding.ctrl);
            assert_eq!(unpacked.shift, binding.shift);
            assert_eq!(unpacked.alt, binding.alt);
            assert_eq!(unpacked.win, binding.win);
        }
        assert!(Binding::unpack(0).is_none());
        assert!(parse_binding("Ctrl+Shift").is_none());
    }
}
