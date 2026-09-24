use crate::state::SharedState;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassExW,
    TranslateMessage, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMRESUMESUSPEND, PBT_APMSUSPEND,
    WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WNDCLASSEXW, WS_EX_TOOLWINDOW,
    WS_POPUP, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};

const FANOUT_DEBOUNCE_MS: u64 = 3_000;

static APP: OnceLock<AppHandle> = OnceLock::new();
static LOCKED: AtomicBool = AtomicBool::new(false);
static LAST_FANOUT_MS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
enum PowerEvent {
    Suspend,
    Resume,
    Lock,
    Unlock,
}

pub fn session_locked() -> bool {
    LOCKED.load(Ordering::Acquire)
}

pub fn start(app: AppHandle) {
    if APP.set(app).is_err() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("synapse-power".to_string())
        .spawn(|| {
            if let Err(err) = listen() {
                tracing::error!("power/session listener stopped: {err}");
            }
        });
    if let Err(err) = spawned {
        tracing::error!("could not start power/session listener: {err}");
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn listen() -> Result<(), String> {
    let class_name = wide("SynapsePowerListener");
    let window_name = wide("Synapse power listener");
    unsafe {
        let module = GetModuleHandleW(None).map_err(|e| format!("module handle: {e}"))?;
        let instance = HINSTANCE(module.0);
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            return Err("RegisterClassExW failed".to_string());
        }
        let hwnd = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            PCWSTR(class_name.as_ptr()),
            PCWSTR(window_name.as_ptr()),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )
        .map_err(|e| format!("CreateWindowExW: {e}"))?;
        if let Err(err) = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) {
            tracing::warn!("session lock notifications unavailable: {err}");
        }
        tracing::info!("power/session listener started");
        let mut msg = MSG::default();
        loop {
            let result = GetMessageW(&mut msg, None, 0, 0);
            if result.0 == 0 {
                return Ok(());
            }
            if result.0 < 0 {
                return Err("GetMessageW failed".to_string());
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_POWERBROADCAST => {
            let event = match wparam.0 as u32 {
                PBT_APMSUSPEND => Some(PowerEvent::Suspend),
                PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND => Some(PowerEvent::Resume),
                _ => None,
            };
            if let Some(event) = event {
                notify(event);
            }
            LRESULT(1)
        }
        WM_WTSSESSION_CHANGE => {
            match wparam.0 as u32 {
                WTS_SESSION_LOCK => notify(PowerEvent::Lock),
                WTS_SESSION_UNLOCK => notify(PowerEvent::Unlock),
                _ => {}
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn notify(event: PowerEvent) {
    let app = match APP.get() {
        Some(app) => app.clone(),
        None => return,
    };
    match event {
        PowerEvent::Lock => LOCKED.store(true, Ordering::Release),
        PowerEvent::Unlock => LOCKED.store(false, Ordering::Release),
        _ => {}
    }
    tracing::info!("system event: {event:?}");
    tauri::async_runtime::spawn(async move {
        handle(app, event).await;
    });
}

async fn handle(app: AppHandle, event: PowerEvent) {
    crate::inputhook::reset_active(match event {
        PowerEvent::Suspend => "suspend",
        PowerEvent::Resume => "resume",
        PowerEvent::Lock => "session lock",
        PowerEvent::Unlock => "session unlock",
    });
    if matches!(event, PowerEvent::Suspend | PowerEvent::Lock) {
        return;
    }
    let now = now_ms();
    let last = LAST_FANOUT_MS.swap(now, Ordering::AcqRel);
    if now.saturating_sub(last) < FANOUT_DEBOUNCE_MS {
        return;
    }
    tokio::time::sleep(Duration::from_millis(750)).await;
    crate::inputhook::request_reinstall("resume or unlock");
    let state = match app.try_state::<SharedState>() {
        Some(state) => state.inner().clone(),
        None => return,
    };
    state.audio.refresh();
    crate::services::probe_sidecar(&state).await;
    state.emit_status();
}
