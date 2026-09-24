use crate::config::{RecordMode, Settings};
use crate::pipeline;
use crate::state::SharedState;
use parking_lot::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};
use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, GetMessageW, SetWindowsHookExW, HC_ACTION, KBDLLHOOKSTRUCT, MSG,
    MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_KEYDOWN, WM_KEYUP, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

const XBUTTON1: u16 = 0x0001;
const XBUTTON2: u16 = 0x0002;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MouseBtn {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Key(u32),
    Mouse(MouseBtn),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BindKind {
    Ptt,
    Toggle,
}

struct Binding {
    ctrl: bool,
    shift: bool,
    alt: bool,
    win: bool,
    trigger: Trigger,
    kind: BindKind,
    active: AtomicBool,
}

enum HotEvent {
    Press(BindKind),
    Release(BindKind),
}

static BINDINGS: OnceLock<RwLock<Vec<Binding>>> = OnceLock::new();
static DISPATCH: OnceLock<Sender<HotEvent>> = OnceLock::new();

fn bindings() -> &'static RwLock<Vec<Binding>> {
    BINDINGS.get_or_init(|| RwLock::new(Vec::new()))
}

pub fn set_bindings(settings: &Settings) {
    let mut list = Vec::new();
    if let Some(binding) = parse_binding(&settings.hotkey_ptt, BindKind::Ptt) {
        list.push(binding);
    }
    if let Some(binding) = parse_binding(&settings.hotkey_toggle, BindKind::Toggle) {
        list.push(binding);
    }
    *bindings().write() = list;
}

pub fn start(app: AppHandle) {
    let (tx, rx) = channel::<HotEvent>();
    if DISPATCH.set(tx).is_err() {
        return;
    }

    std::thread::Builder::new()
        .name("synapse-hotkey-dispatch".to_string())
        .spawn(move || {
            while let Ok(event) = rx.recv() {
                dispatch(&app, event);
            }
        })
        .ok();

    std::thread::Builder::new()
        .name("synapse-hotkey-hook".to_string())
        .spawn(|| unsafe { hook_thread() })
        .ok();
}

unsafe fn hook_thread() {
    let hmod = GetModuleHandleW(None)
        .map(|h| HINSTANCE(h.0))
        .unwrap_or(HINSTANCE(std::ptr::null_mut()));

    let kbd = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(hmod.into()), 0);
    if let Err(err) = &kbd {
        tracing::error!("keyboard hook install failed: {err}");
    }
    let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hmod.into()), 0);
    if let Err(err) = &mouse {
        tracing::error!("mouse hook install failed: {err}");
    }
    if kbd.is_err() && mouse.is_err() {
        return;
    }

    let mut msg = MSG::default();
    loop {
        let result = GetMessageW(&mut msg, None, 0, 0);
        if result.0 <= 0 {
            break;
        }
    }
}

fn dispatch(app: &AppHandle, event: HotEvent) {
    let state = match app.try_state::<SharedState>() {
        Some(state) => state.inner().clone(),
        None => return,
    };
    let mode = state.settings_snapshot().record_mode;
    match event {
        HotEvent::Press(BindKind::Ptt) => match mode {
            RecordMode::PushToTalk => pipeline::begin_recording(&state),
            RecordMode::Toggle => pipeline::toggle_recording(app.clone(), state.clone()),
        },
        HotEvent::Release(BindKind::Ptt) => {
            if mode == RecordMode::PushToTalk {
                pipeline::finish_recording(app.clone(), state.clone());
            }
        }
        HotEvent::Press(BindKind::Toggle) => {
            pipeline::toggle_recording(app.clone(), state.clone())
        }
        HotEvent::Release(BindKind::Toggle) => {}
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

fn handle(trigger: Trigger, is_down: bool) -> bool {
    let guard = bindings().read();
    let mut swallow = false;
    for binding in guard.iter() {
        if binding.trigger != trigger {
            continue;
        }
        if is_down {
            if !binding.active.load(Ordering::Relaxed) && modifiers_match(binding) {
                binding.active.store(true, Ordering::Relaxed);
                send(HotEvent::Press(binding.kind));
                if should_swallow(binding) {
                    swallow = true;
                }
            }
        } else if binding.active.swap(false, Ordering::Relaxed) {
            send(HotEvent::Release(binding.kind));
            if should_swallow(binding) {
                swallow = true;
            }
        }
    }
    swallow
}

fn should_swallow(binding: &Binding) -> bool {
    binding.ctrl
        || binding.shift
        || binding.alt
        || binding.win
        || matches!(binding.trigger, Trigger::Mouse(_))
}

fn send(event: HotEvent) {
    if let Some(tx) = DISPATCH.get() {
        let _ = tx.send(event);
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let info = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
        let msg = wparam.0 as u32;
        let is_down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let is_up = msg == WM_KEYUP || msg == WM_SYSKEYUP;
        if (is_down || is_up) && handle(Trigger::Key(info.vkCode), is_down) {
            return LRESULT(1);
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
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
            if handle(Trigger::Mouse(button), is_down) {
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

fn parse_binding(accelerator: &str, kind: BindKind) -> Option<Binding> {
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
        kind,
        active: AtomicBool::new(false),
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
