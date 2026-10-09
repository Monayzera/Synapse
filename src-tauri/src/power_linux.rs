use ashpd::zbus;
use futures_util::StreamExt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

use crate::state::SharedState;

const FANOUT_DEBOUNCE_MS: u64 = 3_000;
const RETRY_START: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(30);

static STARTED: AtomicBool = AtomicBool::new(false);
static LAST_FANOUT_MS: AtomicU64 = AtomicU64::new(0);

pub fn start(app: AppHandle) {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        run(app).await;
    });
}

async fn run(app: AppHandle) {
    let mut delay = RETRY_START;
    loop {
        let started = Instant::now();
        let outcome = listen(&app).await;
        if started.elapsed() >= RETRY_MAX {
            delay = RETRY_START;
        }
        let reason = match outcome {
            Ok(()) => "login1 PrepareForSleep stream ended".to_string(),
            Err(err) => format!("login1 PrepareForSleep listener failed: {err}"),
        };
        if delay == RETRY_START {
            tracing::warn!("{reason}; retrying in {delay:?}");
        } else {
            tracing::debug!("{reason}; retrying in {delay:?}");
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(RETRY_MAX);
    }
}

async fn listen(app: &AppHandle) -> Result<(), String> {
    let connection = zbus::Connection::system()
        .await
        .map_err(|err| format!("system bus unavailable: {err}"))?;
    let proxy = zbus::Proxy::new(
        &connection,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await
    .map_err(|err| format!("login1 manager proxy: {err}"))?;
    let mut signals = proxy
        .receive_signal("PrepareForSleep")
        .await
        .map_err(|err| format!("PrepareForSleep subscription: {err}"))?;
    tracing::info!("power listener subscribed to login1 PrepareForSleep");
    while let Some(message) = signals.next().await {
        match message.body().deserialize::<bool>() {
            Ok(sleeping) => notify(app, sleeping),
            Err(err) => tracing::warn!("PrepareForSleep payload could not be read: {err}"),
        }
    }
    Ok(())
}

fn boot_ms() -> Option<u64> {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
    if rc != 0 {
        tracing::warn!(
            "boot clock unavailable, resume debounce skipped: {}",
            std::io::Error::last_os_error()
        );
        return None;
    }
    let secs = ts.tv_sec.max(0) as u64;
    let millis = (ts.tv_nsec.max(0) as u64) / 1_000_000;
    Some(secs.saturating_mul(1_000).saturating_add(millis))
}

fn notify(app: &AppHandle, sleeping: bool) {
    tracing::info!(
        "system event: {}",
        if sleeping { "suspend" } else { "resume" }
    );
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        handle(app, sleeping).await;
    });
}

async fn handle(app: AppHandle, sleeping: bool) {
    crate::inputhook_linux::reset_active(if sleeping { "suspend" } else { "resume" });
    if sleeping {
        return;
    }
    if let Some(now) = boot_ms() {
        let last = LAST_FANOUT_MS.swap(now, Ordering::AcqRel);
        if last != 0 && now.saturating_sub(last) < FANOUT_DEBOUNCE_MS {
            return;
        }
    }
    tokio::time::sleep(Duration::from_millis(750)).await;
    crate::inputhook_linux::request_rebind("resume");
    let state = match app.try_state::<SharedState>() {
        Some(state) => state.inner().clone(),
        None => return,
    };
    state.audio.refresh();
    crate::services::probe_sidecar(&state).await;
    state.emit_status();
}
