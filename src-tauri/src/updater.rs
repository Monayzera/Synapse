use crate::state::{SharedState, Status};
use crate::{atomic_io, services, tray};
use base64::Engine;
use futures_util::StreamExt;
use minisign_verify::{PublicKey, Signature};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Listener, Manager};
use tauri_plugin_notification::NotificationExt;
use tauri_plugin_updater::{Update, UpdaterExt};
use tokio::io::AsyncWriteExt;

const FIRST_CHECK_DELAY_MS: i64 = 60_000;
const TICK: Duration = Duration::from_secs(15);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const CHECK_GUARD: Duration = Duration::from_secs(45);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const STALL_TIMEOUT: Duration = Duration::from_secs(120);
const CANCEL_POLL: Duration = Duration::from_millis(500);
const CHECK_INTERVAL_MS: i64 = 6 * 60 * 60 * 1000;
const RETRY_MINUTES: [i64; 4] = [5, 15, 30, 60];
const QUICK_RETRIES: u32 = 4;
const MAX_DOWNLOADS: u32 = 3;
const MAX_INVALID: u32 = 2;
const QUIET_MS: i64 = 2 * 60 * 1000;
const INSTALL_LEAD: Duration = Duration::from_secs(3);
const INSTALL_COOLDOWN_MS: i64 = 30 * 60 * 1000;
const QUIT_DEADLINE: Duration = Duration::from_secs(120);
const QUIT_HARD_DEADLINE: Duration = Duration::from_secs(600);
const QUIT_POLL: Duration = Duration::from_secs(5);
const NOTICE_DELAY: Duration = Duration::from_secs(3);
const CLEANUP_DELAY: Duration = Duration::from_secs(120);
const MAX_ATTEMPTS: u32 = 2;
const STATUS_EVENT: &str = "update-status";
const NOTICE_EVENT: &str = "update-notice";
const MARKER_FILE: &str = "update-state.json";
const CACHE_DIR: &str = "updates";
const CACHE_PREFIX: &str = "synapse-";
const CACHE_EXT: &str = "bin";
const PART_EXT: &str = "part";
const FOCUS_WINDOWS: [&str; 2] = ["settings", "history"];
const APP_WINDOWS: [&str; 3] = ["widget", "settings", "history"];

static COMMITTED: AtomicBool = AtomicBool::new(false);
static QUITTING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Unsupported,
    Idle,
    Checking,
    UpToDate,
    Available,
    Downloading,
    Ready,
    Installing,
    Error,
}

#[derive(Debug, Clone, Serialize)]
pub struct UpdateStatus {
    pub phase: Phase,
    pub current_version: String,
    pub version: Option<String>,
    pub downloaded: u64,
    pub total: u64,
    pub error: Option<String>,
    pub detail: Option<String>,
    pub last_checked: Option<i64>,
    pub blocked: bool,
}

impl UpdateStatus {
    fn new(phase: Phase, current_version: String) -> Self {
        UpdateStatus {
            phase,
            current_version,
            version: None,
            downloaded: 0,
            total: 0,
            error: None,
            detail: None,
            last_checked: None,
            blocked: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayItem {
    Check,
    Checking,
    Downloading(u8),
    Install(String),
    Installing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Auto,
    About,
    Tray,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Trigger {
    Auto,
    Manual,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Notice {
    Installing,
    Installed,
    UpToDate,
    Downloading,
    Available,
    Failed,
    CheckFailed,
    Postponed,
    Busy,
    Relocate,
}

#[derive(Debug, Clone, Serialize)]
pub struct NoticePayload {
    kind: Notice,
    version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Marker {
    version: String,
    from: String,
    attempts: u32,
    #[serde(default)]
    reported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Nothing,
    Stale,
    Updated(String),
    Failed { version: String, reported: bool },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Activity {
    dictating: bool,
    background_work: bool,
    booting: bool,
    window_focused: bool,
    idle_ms: i64,
}

#[derive(Debug)]
struct Failure {
    code: &'static str,
    detail: String,
}

impl Failure {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Failure {
            code,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum VerifyError {
    Io(String),
    Invalid(String),
}

#[derive(Debug, Default)]
struct Tally {
    version: String,
    signature: String,
    completed: u32,
    failed: u32,
    invalid: u32,
}

#[derive(Deserialize)]
struct StatusProbe {
    #[serde(default)]
    status: String,
    #[serde(default)]
    recording: bool,
}

pub struct AutoUpdater {
    app: AppHandle,
    current: String,
    target: String,
    pubkey: String,
    cache_dir: PathBuf,
    marker_path: PathBuf,
    status: Mutex<UpdateStatus>,
    pending: Mutex<Option<Update>>,
    file: Mutex<Option<PathBuf>>,
    tally: Mutex<Tally>,
    startup_notice: Mutex<Option<NoticePayload>>,
    op: tokio::sync::Mutex<()>,
    applied: Mutex<u64>,
    os_language: OnceLock<&'static str>,
    status_seq: AtomicU64,
    last_activity: AtomicI64,
    was_active: AtomicBool,
    next_check: AtomicI64,
    hold_until: AtomicI64,
    check_failures: AtomicU32,
    hook_ran: AtomicBool,
    in_installer: AtomicBool,
    quit_requested: AtomicBool,
    quit_install: AtomicBool,
    progress_mark: AtomicI64,
}

type Shared = Arc<AutoUpdater>;

pub fn init(app: &AppHandle) {
    if !build_supported() {
        tracing::info!("auto-update is off in this build");
        return;
    }
    let identifier = app.config().identifier.clone();
    let Some(target) = target_key(std::env::consts::OS, &identifier) else {
        tracing::info!("auto-update is not available on {}", std::env::consts::OS);
        return;
    };
    let Some(pubkey) = configured_pubkey(app) else {
        tracing::error!("updater public key missing from the configuration; auto-update disabled");
        return;
    };
    let base = match app.path().app_local_data_dir() {
        Ok(dir) => dir,
        Err(err) => {
            tracing::error!("auto-update disabled: local data dir unknown ({err})");
            return;
        }
    };
    let current = app.package_info().version.to_string();
    let now = now_ms();
    let updater: Shared = Arc::new(AutoUpdater {
        app: app.clone(),
        current: current.clone(),
        target: target.to_string(),
        pubkey,
        cache_dir: base.join(CACHE_DIR),
        marker_path: base.join(MARKER_FILE),
        status: Mutex::new(UpdateStatus::new(Phase::Idle, current)),
        pending: Mutex::new(None),
        file: Mutex::new(None),
        tally: Mutex::new(Tally::default()),
        startup_notice: Mutex::new(None),
        op: tokio::sync::Mutex::new(()),
        applied: Mutex::new(0),
        os_language: OnceLock::new(),
        status_seq: AtomicU64::new(0),
        last_activity: AtomicI64::new(now),
        was_active: AtomicBool::new(false),
        next_check: AtomicI64::new(now + FIRST_CHECK_DELAY_MS),
        hold_until: AtomicI64::new(0),
        check_failures: AtomicU32::new(0),
        hook_ran: AtomicBool::new(false),
        in_installer: AtomicBool::new(false),
        quit_requested: AtomicBool::new(false),
        quit_install: AtomicBool::new(false),
        progress_mark: AtomicI64::new(-1),
    });
    app.manage(updater.clone());
    watch_activity(app, &updater);
    updater.resume_marker();
    tracing::info!(
        "auto-update ready (target {target}, version {})",
        updater.current
    );
    let runner = updater.clone();
    tauri::async_runtime::spawn(async move {
        run(runner).await;
    });
    let janitor = updater.clone();
    launch(updater, async move {
        tokio::time::sleep(CLEANUP_DELAY).await;
        janitor.cleanup().await;
    });
}

pub fn install_committed() -> bool {
    COMMITTED.load(Ordering::Acquire) || QUITTING.load(Ordering::Acquire)
}

pub fn current_tray_item(app: &AppHandle) -> Option<TrayItem> {
    get(app).map(|updater| tray_item(&updater.status()))
}

pub fn refresh_tray(app: &AppHandle) {
    if let Some(updater) = get(app) {
        updater.update_status(|_| {});
    }
}

pub fn tray_clicked(app: &AppHandle) {
    let Some(updater) = get(app) else {
        return;
    };
    let phase = updater.status().phase;
    let worker = updater.clone();
    launch(updater, async move {
        match phase {
            Phase::Ready | Phase::Available => {
                if let Err(reason) = worker.install(Trigger::Manual).await {
                    tracing::info!("update install from the tray not started: {reason}");
                    worker.explain_refusal(reason);
                }
            }
            Phase::Checking | Phase::Downloading | Phase::Installing => {}
            Phase::Idle | Phase::UpToDate | Phase::Error | Phase::Unsupported => {
                worker.check_cycle(Origin::Tray).await;
            }
        }
    });
}

pub fn quit_with_update(app: &AppHandle) -> bool {
    let Some(updater) = get(app) else {
        return false;
    };
    if updater.quit_requested.load(Ordering::Acquire) {
        return true;
    }
    let status = updater.status();
    let installing = status.phase == Phase::Installing;
    let misplaced = status
        .error
        .as_deref()
        .is_some_and(|code| code.starts_with("location"));
    let ready = status.phase == Phase::Ready
        && !status.blocked
        && !misplaced
        && updater.auto_enabled();
    if !installing && !ready {
        return false;
    }
    QUITTING.store(true, Ordering::Release);
    updater.quit_requested.store(true, Ordering::Release);
    hide_everything(app);
    let guard = updater.clone();
    tauri::async_runtime::spawn(async move {
        guard.quit_watchdog().await;
    });
    if ready {
        updater.quit_install.store(true, Ordering::Release);
        let worker = updater.clone();
        launch(updater, async move {
            if let Err(reason) = worker.install(Trigger::Quit).await {
                tracing::info!("update not installed on quit: {reason}");
            }
        });
    }
    true
}

pub fn auto_changed(app: &AppHandle, enabled: bool) {
    let Some(updater) = get(app) else {
        return;
    };
    if enabled {
        updater.next_check.store(now_ms(), Ordering::Release);
    }
}

#[tauri::command]
pub fn update_status(app: AppHandle) -> UpdateStatus {
    match get(&app) {
        Some(updater) => updater.status(),
        None => UpdateStatus::new(Phase::Unsupported, app.package_info().version.to_string()),
    }
}

#[tauri::command]
pub async fn check_update(app: AppHandle) -> Result<(), String> {
    let updater = get(&app).ok_or_else(|| "updates_unsupported".to_string())?;
    let worker = updater.clone();
    launch(updater, async move {
        worker.check_cycle(Origin::About).await;
    });
    Ok(())
}

#[tauri::command]
pub async fn install_update(app: AppHandle) -> Result<(), String> {
    let updater = get(&app).ok_or_else(|| "updates_unsupported".to_string())?;
    if install_blocker(&updater.activity(), Trigger::Manual).is_some() {
        return Err("update_busy".to_string());
    }
    let worker = updater.clone();
    launch(updater, async move {
        if let Err(reason) = worker.install(Trigger::Manual).await {
            tracing::info!("manual update install not started: {reason}");
            worker.explain_refusal(reason);
        }
    });
    Ok(())
}

#[tauri::command]
pub fn take_update_notice(app: AppHandle) -> Option<NoticePayload> {
    get(&app).and_then(|updater| updater.startup_notice.lock().take())
}

fn get(app: &AppHandle) -> Option<Shared> {
    app.try_state::<Shared>().map(|state| state.inner().clone())
}

fn launch<F>(updater: Shared, work: F)
where
    F: Future<Output = ()> + Send + 'static,
{
    tauri::async_runtime::spawn(async move {
        if let Err(err) = tauri::async_runtime::spawn(work).await {
            tracing::error!("update task crashed: {err}");
            updater.recover();
        }
    });
}

async fn run(updater: Shared) {
    loop {
        tokio::time::sleep(TICK).await;
        let worker = updater.clone();
        if let Err(err) = tauri::async_runtime::spawn(async move { worker.tick().await }).await {
            tracing::error!("update cycle crashed: {err}");
            updater.recover();
        }
    }
}

fn watch_activity(app: &AppHandle, updater: &Shared) {
    let weak = Arc::downgrade(updater);
    app.listen("status-changed", move |event| {
        let Some(updater) = weak.upgrade() else {
            return;
        };
        let Ok(probe) = serde_json::from_str::<StatusProbe>(event.payload()) else {
            return;
        };
        let active =
            probe.recording || probe.status == "recording" || probe.status == "processing";
        let was_active = updater.was_active.swap(active, Ordering::AcqRel);
        if activity_edge(was_active, active) {
            updater.last_activity.store(now_ms(), Ordering::Release);
        }
    });
}

fn hide_everything(app: &AppHandle) {
    for label in APP_WINDOWS {
        if let Some(window) = app.get_webview_window(label) {
            if let Err(err) = window.hide() {
                tracing::debug!("window {label} not hidden: {err}");
            }
        }
    }
    tray::set_visible(app, false);
}

impl AutoUpdater {
    fn status(&self) -> UpdateStatus {
        self.status.lock().clone()
    }

    fn update_status(&self, mutate: impl FnOnce(&mut UpdateStatus)) {
        let (snapshot, seq) = {
            let mut status = self.status.lock();
            mutate(&mut status);
            let seq = self.status_seq.fetch_add(1, Ordering::AcqRel).saturating_add(1);
            (status.clone(), seq)
        };
        let mut applied = self.applied.lock();
        if seq <= *applied {
            return;
        }
        *applied = seq;
        if let Err(err) = self.app.emit(STATUS_EVENT, &snapshot) {
            tracing::warn!("update-status emit failed: {err}");
        }
        tray::set_update_item(&tray_item(&snapshot), self.language());
    }

    async fn tick(self: &Arc<Self>) {
        if self.quit_requested.load(Ordering::Acquire) {
            return;
        }
        self.normalize_clock();
        if !self.auto_enabled() {
            return;
        }
        if now_ms() >= self.next_check.load(Ordering::Acquire) {
            self.check_cycle(Origin::Auto).await;
        }
        self.try_auto_install().await;
    }

    fn recover(&self) {
        COMMITTED.store(false, Ordering::Release);
        if self.hook_ran.swap(false, Ordering::AcqRel) {
            tray::set_visible(&self.app, true);
        }
        self.update_status(|status| {
            if matches!(
                status.phase,
                Phase::Checking | Phase::Downloading | Phase::Installing
            ) {
                status.phase = Phase::Error;
                status.error = Some("unknown".to_string());
                status.detail = None;
            }
        });
    }

    fn normalize_clock(&self) {
        let now = now_ms();
        let next = self.next_check.load(Ordering::Acquire);
        if let Some(fixed) = rewound(next, now, CHECK_INTERVAL_MS) {
            self.next_check.store(fixed, Ordering::Release);
        }
        let last = self.last_activity.load(Ordering::Acquire);
        if let Some(fixed) = rewound(last, now, 0) {
            self.last_activity.store(fixed, Ordering::Release);
        }
        let hold = self.hold_until.load(Ordering::Acquire);
        if let Some(fixed) = rewound(hold, now, INSTALL_COOLDOWN_MS) {
            self.hold_until.store(fixed, Ordering::Release);
        }
    }

    fn auto_enabled(&self) -> bool {
        self.app
            .try_state::<SharedState>()
            .map(|state| {
                let settings = state.settings.read();
                settings.auto_update && !settings.transient_unreadable
            })
            .unwrap_or(false)
    }

    fn language(&self) -> &'static str {
        let preference = self
            .app
            .try_state::<SharedState>()
            .map(|state| state.settings.read().ui_language.clone())
            .unwrap_or_default();
        match preference.as_str() {
            "pt" => "pt",
            "en" => "en",
            _ => *self
                .os_language
                .get_or_init(|| tray::resolve_language("auto")),
        }
    }

    fn activity(&self) -> Activity {
        let idle_ms = now_ms().saturating_sub(self.last_activity.load(Ordering::Acquire));
        let Some(state) = self.app.try_state::<SharedState>() else {
            return Activity {
                booting: true,
                idle_ms,
                ..Activity::default()
            };
        };
        let status = *state.status.read();
        let dictating = state.recording.load(Ordering::Acquire)
            || state.busy.load(Ordering::Acquire)
            || matches!(status, Status::Recording | Status::Processing);
        let background_work = !state.downloading.lock().is_empty()
            || state.llama_setup_running.load(Ordering::Acquire);
        let booting = !state.boot_done.load(Ordering::Acquire);
        let window_focused = FOCUS_WINDOWS.iter().any(|label| {
            self.app
                .get_webview_window(label)
                .map(|window| {
                    window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false)
                })
                .unwrap_or(false)
        });
        Activity {
            dictating,
            background_work,
            booting,
            window_focused,
            idle_ms,
        }
    }

    fn blocker(&self, trigger: Trigger) -> Option<&'static str> {
        if trigger == Trigger::Auto && !self.auto_enabled() {
            return Some("disabled");
        }
        install_blocker(&self.activity(), trigger)
    }

    fn flash(&self, notice: Notice, version: Option<String>) {
        let payload = NoticePayload {
            kind: notice,
            version,
        };
        if let Err(err) = self.app.emit(NOTICE_EVENT, &payload) {
            tracing::warn!("update-notice emit failed: {err}");
        }
    }

    fn notify(&self, notice: Notice, version: Option<String>) {
        if self
            .app
            .try_state::<tauri_plugin_notification::Notification<tauri::Wry>>()
            .is_none()
        {
            return;
        }
        let shown = version.unwrap_or_else(|| self.current.clone());
        let (title, body) = notice_text(self.language(), notice, &shown);
        if let Err(err) = self.app.notification().builder().title(title).body(body).show() {
            tracing::warn!("update notification failed: {err}");
        }
    }

    fn announce(&self, notice: Notice, version: Option<String>) {
        self.flash(notice, version.clone());
        self.notify(notice, version);
    }

    fn explain_refusal(&self, reason: &str) {
        match reason {
            "busy" => self.announce(Notice::Busy, None),
            "location" => self.announce(Notice::Relocate, self.status().version),
            _ => {}
        }
    }

    fn startup(self: &Arc<Self>, notice: Notice, version: String) {
        *self.startup_notice.lock() = Some(NoticePayload {
            kind: notice,
            version: Some(version.clone()),
        });
        let owner = self.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(NOTICE_DELAY).await;
            owner.notify(notice, Some(version));
        });
    }

    fn finish_quit(&self) {
        if let Some(state) = self.app.try_state::<SharedState>() {
            state.stop_sidecar();
        }
        self.app.exit(0);
    }

    async fn quit_watchdog(&self) {
        tokio::time::sleep(QUIT_DEADLINE).await;
        let mut waited = QUIT_DEADLINE;
        while self.in_installer.load(Ordering::Acquire) && waited < QUIT_HARD_DEADLINE {
            tokio::time::sleep(QUIT_POLL).await;
            waited += QUIT_POLL;
        }
        tracing::warn!("update did not finish in time; quitting anyway");
        self.finish_quit();
    }

    fn resume_marker(self: &Arc<Self>) {
        let marker = read_marker(&self.marker_path);
        match startup_outcome(marker.as_ref(), &self.current) {
            Outcome::Nothing => {}
            Outcome::Stale => remove_file_quietly(&self.marker_path),
            Outcome::Updated(version) => {
                remove_file_quietly(&self.marker_path);
                tracing::info!("update to {version} completed");
                self.startup(Notice::Installed, version);
            }
            Outcome::Failed { version, reported } => {
                tracing::warn!("update to {version} did not complete");
                self.update_status(|status| {
                    status.phase = Phase::Error;
                    status.error = Some("not_completed".to_string());
                    status.version = Some(version.clone());
                });
                if !reported {
                    self.mark_reported(&version);
                    self.startup(Notice::Failed, version);
                }
            }
        }
    }

    fn blocked_for(&self, version: &str) -> bool {
        let marker = read_marker(&self.marker_path);
        if marker.as_ref().is_some_and(|marker| marker.version != version) {
            remove_file_quietly(&self.marker_path);
            return false;
        }
        is_blocked(marker.as_ref(), version)
    }

    fn record_attempt(&self, version: &str) -> u32 {
        let attempts = next_attempt(read_marker(&self.marker_path).as_ref(), version);
        write_marker(
            &self.marker_path,
            &Marker {
                version: version.to_string(),
                from: self.current.clone(),
                attempts,
                reported: false,
            },
        );
        attempts
    }

    fn mark_reported(&self, version: &str) {
        if let Some(mut marker) =
            read_marker(&self.marker_path).filter(|marker| marker.version == version)
        {
            marker.reported = true;
            write_marker(&self.marker_path, &marker);
        }
    }

    fn track(&self, update: &Update) {
        let mut tally = self.tally.lock();
        if tally.version != update.version || tally.signature != update.signature {
            *tally = Tally {
                version: update.version.clone(),
                signature: update.signature.clone(),
                ..Tally::default()
            };
        }
    }

    async fn fetch(self: &Arc<Self>) -> Result<Option<Update>, Failure> {
        let builder = self
            .app
            .updater_builder()
            .target(self.target.clone())
            .timeout(CHECK_TIMEOUT);
        #[cfg(windows)]
        let builder = {
            let hook = Arc::downgrade(self);
            builder.on_before_exit(move || {
                if let Some(updater) = hook.upgrade() {
                    updater.before_exit();
                }
            })
        };
        let updater = builder
            .build()
            .map_err(|err| Failure::new("unknown", err.to_string()))?;
        match tokio::time::timeout(CHECK_GUARD, updater.check()).await {
            Ok(Ok(found)) => Ok(found),
            Ok(Err(err)) => Err(classify(&err)),
            Err(_) => Err(Failure::new("network", "update check timed out")),
        }
    }

    async fn check_cycle(self: &Arc<Self>, origin: Origin) {
        let Ok(_guard) = self.op.try_lock() else {
            return;
        };
        let previous = self.status().phase;
        self.update_status(|status| {
            status.phase = Phase::Checking;
            status.error = None;
            status.detail = None;
        });
        let found = self.fetch().await;
        let now = now_ms();
        let update = match found {
            Ok(Some(update)) => update,
            Ok(None) => {
                self.check_failures.store(0, Ordering::Release);
                self.next_check
                    .store(now + CHECK_INTERVAL_MS, Ordering::Release);
                *self.pending.lock() = None;
                *self.file.lock() = None;
                let dir = self.cache_dir.clone();
                if let Err(err) = tokio::task::spawn_blocking(move || prune_cache(&dir, &[])).await {
                    tracing::debug!("update cache prune failed: {err}");
                }
                self.update_status(|status| {
                    status.phase = Phase::UpToDate;
                    status.version = None;
                    status.downloaded = 0;
                    status.total = 0;
                    status.blocked = false;
                    status.last_checked = Some(now);
                });
                tracing::info!("no update available (current {})", self.current);
                if origin == Origin::Tray {
                    self.announce(Notice::UpToDate, Some(self.current.clone()));
                }
                return;
            }
            Err(failure) => {
                let count = self
                    .check_failures
                    .fetch_add(1, Ordering::AcqRel)
                    .saturating_add(1);
                self.next_check
                    .store(now + retry_delay_ms(count), Ordering::Release);
                tracing::warn!("update check failed ({}): {}", failure.code, failure.detail);
                let keep = matches!(previous, Phase::Ready | Phase::Available)
                    && self.pending.lock().is_some();
                self.update_status(|status| {
                    if keep {
                        status.phase = previous;
                    } else {
                        status.phase = Phase::Error;
                        status.error = Some(failure.code.to_string());
                        status.detail = Some(failure.detail.clone());
                    }
                });
                if origin == Origin::Tray {
                    self.announce(Notice::CheckFailed, None);
                }
                return;
            }
        };
        self.check_failures.store(0, Ordering::Release);
        self.next_check
            .store(now + CHECK_INTERVAL_MS, Ordering::Release);
        let version = update.version.clone();
        self.track(&update);
        let blocked = self.blocked_for(&version);
        tracing::info!("update {version} available (current {})", self.current);
        *self.pending.lock() = Some(update.clone());
        self.update_status(|status| {
            status.version = Some(version.clone());
            status.last_checked = Some(now);
            status.blocked = blocked;
            status.downloaded = 0;
            status.total = 0;
        });
        if let Some(path) = self.reuse_cached(&update).await {
            *self.file.lock() = Some(path);
            self.update_status(|status| status.phase = Phase::Ready);
            if origin == Origin::Tray {
                self.announce(Notice::Available, Some(version));
            }
            return;
        }
        *self.file.lock() = None;
        if !self.auto_enabled() {
            self.update_status(|status| status.phase = Phase::Available);
            if origin == Origin::Tray {
                self.announce(Notice::Available, Some(version));
            }
            return;
        }
        let (rejected, exhausted) = {
            let tally = self.tally.lock();
            (tally.invalid >= MAX_INVALID, tally.completed >= MAX_DOWNLOADS)
        };
        if origin == Origin::Auto && (rejected || exhausted) {
            let code = if rejected { "signature" } else { "download_limit" };
            tracing::warn!("update {version} is not downloaded again automatically ({code})");
            self.update_status(|status| {
                status.phase = Phase::Error;
                status.error = Some(code.to_string());
                status.detail = None;
            });
            return;
        }
        if origin == Origin::Tray {
            self.announce(Notice::Downloading, Some(version.clone()));
        }
        match self.download(&update, true).await {
            Ok(path) => {
                *self.file.lock() = Some(path);
                self.update_status(|status| status.phase = Phase::Ready);
                tracing::info!("update {version} downloaded and verified");
            }
            Err(failure) => self.download_failed(failure, &update),
        }
    }

    async fn reuse_cached(&self, update: &Update) -> Option<PathBuf> {
        let path = self.cache_path(&update.version)?;
        if !path.is_file() {
            return None;
        }
        let source = path.clone();
        let signature = update.signature.clone();
        let pubkey = self.pubkey.clone();
        let announced = update.version.clone();
        match tokio::task::spawn_blocking(move || {
            verify_file(&source, &signature, &pubkey, &announced)
        })
        .await
        {
            Ok(Ok(())) => {
                tracing::info!("reusing downloaded update {}", path.display());
                Some(path)
            }
            Ok(Err(VerifyError::Invalid(detail))) => {
                tracing::warn!("cached update rejected ({detail}); downloading again");
                remove_file_quietly(&path);
                None
            }
            Ok(Err(VerifyError::Io(detail))) => {
                tracing::warn!("cached update unreadable ({detail})");
                None
            }
            Err(err) => {
                tracing::warn!("cached update check failed: {err}");
                None
            }
        }
    }

    fn cache_path(&self, version: &str) -> Option<PathBuf> {
        cache_file_name(version).map(|name| self.cache_dir.join(name))
    }

    async fn download(self: &Arc<Self>, update: &Update, automatic: bool) -> Result<PathBuf, Failure> {
        let dest = self.cache_path(&update.version).ok_or_else(|| {
            Failure::new("unavailable", format!("invalid version {}", update.version))
        })?;
        let part = dest.with_extension(PART_EXT);
        let dir = self.cache_dir.clone();
        let keep = [dest.clone(), part.clone()];
        let prepared = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir)?;
            prune_cache(&dir, &keep);
            Ok::<(), std::io::Error>(())
        })
        .await;
        match prepared {
            Ok(Ok(())) => {}
            Ok(Err(err)) => return Err(Failure::new("disk", err.to_string())),
            Err(err) => return Err(Failure::new("unknown", err.to_string())),
        }
        self.progress_mark.store(-1, Ordering::Release);
        self.update_status(|status| {
            status.phase = Phase::Downloading;
            status.downloaded = 0;
            status.total = 0;
            status.error = None;
            status.detail = None;
        });
        let url = update.download_url.to_string();
        tracing::info!("downloading update {} from {url}", update.version);
        let fresh = self.transfer(&url, &part, automatic).await?;
        let source = part.clone();
        let signature = update.signature.clone();
        let pubkey = self.pubkey.clone();
        let announced = update.version.clone();
        match tokio::task::spawn_blocking(move || {
            verify_file(&source, &signature, &pubkey, &announced)
        })
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(VerifyError::Invalid(detail))) => {
                remove_file_quietly(&part);
                let code = if fresh { "signature" } else { "download" };
                return Err(Failure::new(code, detail));
            }
            Ok(Err(VerifyError::Io(detail))) => return Err(Failure::new("download", detail)),
            Err(err) => return Err(Failure::new("unknown", err.to_string())),
        }
        tokio::fs::rename(&part, &dest)
            .await
            .map_err(|err| Failure::new("disk", err.to_string()))?;
        {
            let mut tally = self.tally.lock();
            tally.completed = tally.completed.saturating_add(1);
            tally.failed = 0;
        }
        Ok(dest)
    }

    async fn transfer(self: &Arc<Self>, url: &str, part: &Path, automatic: bool) -> Result<bool, Failure> {
        let client = reqwest::Client::builder()
            .user_agent("Synapse")
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|err| Failure::new("download", err.to_string()))?;
        let existing = tokio::fs::metadata(part)
            .await
            .map(|meta| meta.len())
            .unwrap_or(0);
        let mut request = client.get(url);
        if existing > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={existing}-"));
        }
        let cancel = self.cancelled(automatic);
        tokio::pin!(cancel);
        let response = tokio::select! {
            sent = tokio::time::timeout(RESPONSE_TIMEOUT, request.send()) => match sent {
                Ok(Ok(response)) => response,
                Ok(Err(err)) => return Err(Failure::new("download", err.to_string())),
                Err(_) => {
                    return Err(Failure::new("download", "the download server did not respond"))
                }
            },
            _ = &mut cancel => return Err(cancelled_failure()),
        };
        let status = response.status();
        let (append, expected) = if existing > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT {
            match content_range(response.headers()) {
                Some((start, total)) if start == existing => (true, total),
                _ => {
                    remove_file_quietly(part);
                    return Err(Failure::new("download", "unexpected partial response"));
                }
            }
        } else if existing > 0 && status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            return Ok(false);
        } else if status.is_success() {
            (false, response.content_length())
        } else {
            return Err(Failure::new("download", format!("HTTP {status}")));
        };
        let opened = if append {
            tokio::fs::OpenOptions::new().append(true).open(part).await
        } else {
            tokio::fs::File::create(part).await
        };
        let mut file = opened.map_err(|err| Failure::new("disk", err.to_string()))?;
        let mut downloaded = if append { existing } else { 0 };
        let total = expected.unwrap_or(0);
        self.progress(downloaded, total);
        let mut stream = response.bytes_stream();
        loop {
            let next = tokio::select! {
                item = tokio::time::timeout(STALL_TIMEOUT, stream.next()) => match item {
                    Ok(next) => next,
                    Err(_) => return Err(Failure::new("download", "the download stalled")),
                },
                _ = &mut cancel => return Err(cancelled_failure()),
            };
            let chunk = match next {
                Some(Ok(chunk)) => chunk,
                Some(Err(err)) => return Err(Failure::new("download", err.to_string())),
                None => break,
            };
            if let Err(err) = file.write_all(&chunk).await {
                drop(file);
                remove_file_quietly(part);
                return Err(Failure::new("disk", err.to_string()));
            }
            downloaded = downloaded.saturating_add(chunk.len() as u64);
            self.progress(downloaded, total);
        }
        let synced = match file.flush().await {
            Ok(()) => file.sync_all().await,
            Err(err) => Err(err),
        };
        drop(file);
        if let Err(err) = synced {
            remove_file_quietly(part);
            return Err(Failure::new("disk", err.to_string()));
        }
        match expected {
            Some(expected) if downloaded != expected => Err(Failure::new(
                "download",
                format!("incomplete download: {downloaded} of {expected} bytes"),
            )),
            _ => Ok(!append),
        }
    }

    async fn cancelled(&self, automatic: bool) {
        loop {
            tokio::time::sleep(CANCEL_POLL).await;
            if automatic && !self.auto_enabled() {
                return;
            }
        }
    }

    fn progress(&self, downloaded: u64, total: u64) {
        let mark = if total > 0 {
            i64::from(percent(downloaded, total))
        } else {
            1000 + (downloaded / 4_000_000) as i64
        };
        if self.progress_mark.swap(mark, Ordering::AcqRel) == mark {
            return;
        }
        self.update_status(|status| {
            status.downloaded = downloaded;
            status.total = total;
        });
    }

    fn download_failed(&self, failure: Failure, update: &Update) {
        let version = update.version.clone();
        if failure.code == "cancelled" {
            tracing::info!("update {version} download paused: automatic updates turned off");
            self.update_status(|status| {
                status.phase = Phase::Available;
                status.downloaded = 0;
                status.total = 0;
            });
            return;
        }
        let (failed, first_invalid) = {
            let mut tally = self.tally.lock();
            tally.failed = tally.failed.saturating_add(1);
            if failure.code == "signature" {
                tally.invalid = tally.invalid.saturating_add(1);
            }
            (
                tally.failed,
                failure.code == "signature" && tally.invalid == 1,
            )
        };
        self.next_check
            .store(now_ms() + failure_delay_ms(failure.code, failed), Ordering::Release);
        tracing::warn!(
            "update {version} download failed ({}): {}",
            failure.code,
            failure.detail
        );
        self.update_status(|status| {
            status.phase = Phase::Error;
            status.error = Some(failure.code.to_string());
            status.detail = Some(failure.detail.clone());
            status.downloaded = 0;
            status.total = 0;
        });
        if first_invalid {
            self.announce(Notice::Failed, Some(version));
        }
    }

    async fn try_auto_install(self: &Arc<Self>) {
        let status = self.status();
        let misplaced = status
            .error
            .as_deref()
            .is_some_and(|code| code.starts_with("location"));
        if status.phase != Phase::Ready || status.blocked || misplaced {
            return;
        }
        if now_ms() < self.hold_until.load(Ordering::Acquire) {
            return;
        }
        if install_blocker(&self.activity(), Trigger::Auto).is_some() {
            return;
        }
        if let Err(reason) = self.install(Trigger::Auto).await {
            tracing::info!("automatic update install postponed: {reason}");
        }
    }

    async fn install(self: &Arc<Self>, trigger: Trigger) -> Result<(), &'static str> {
        let result = self.install_inner(trigger).await;
        let owns_exit = trigger == Trigger::Quit || !self.quit_install.load(Ordering::Acquire);
        let stray = result == Err("busy") && trigger != Trigger::Quit;
        if self.quit_requested.load(Ordering::Acquire) && owns_exit && !stray {
            self.finish_quit();
        }
        result
    }

    async fn install_inner(self: &Arc<Self>, trigger: Trigger) -> Result<(), &'static str> {
        let _guard = if trigger == Trigger::Quit {
            self.op.lock().await
        } else {
            match self.op.try_lock() {
                Ok(guard) => guard,
                Err(_) => return Err("busy"),
            }
        };
        let Some(update) = self.pending.lock().clone() else {
            return Err("nothing");
        };
        let version = update.version.clone();
        if let Some(issue) = location_issue() {
            if trigger != Trigger::Manual || issue == "location_move" {
                tracing::warn!("update {version} not installed from this location ({issue})");
                self.update_status(|status| {
                    status.error = Some(issue.to_string());
                    status.detail = None;
                });
                return Err("location");
            }
        }
        let cached = self.file.lock().clone().filter(|path| path.is_file());
        let path = match cached {
            Some(path) => path,
            None if trigger == Trigger::Manual => {
                if let Some(reason) = self.blocker(trigger) {
                    return Err(reason);
                }
                self.track(&update);
                match self.download(&update, false).await {
                    Ok(path) => {
                        *self.file.lock() = Some(path.clone());
                        self.update_status(|status| status.phase = Phase::Ready);
                        path
                    }
                    Err(failure) => {
                        self.download_failed(failure, &update);
                        return Err("download");
                    }
                }
            }
            None => {
                *self.file.lock() = None;
                self.update_status(|status| {
                    status.phase = Phase::Error;
                    status.error = Some("download".to_string());
                    status.detail = Some("the downloaded update is missing".to_string());
                });
                self.next_check
                    .store(now_ms() + retry_delay_ms(1), Ordering::Release);
                return Err("missing");
            }
        };
        if let Some(reason) = self.blocker(trigger) {
            return Err(reason);
        }
        let source = path.clone();
        let signature = update.signature.clone();
        let pubkey = self.pubkey.clone();
        let announced = version.clone();
        let bytes = match tokio::task::spawn_blocking(move || {
            read_verified(&source, &signature, &pubkey, &announced)
        })
        .await
        {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(VerifyError::Invalid(detail))) => {
                remove_file_quietly(&path);
                *self.file.lock() = None;
                self.download_failed(Failure::new("download", detail), &update);
                return Err("verify");
            }
            Ok(Err(VerifyError::Io(detail))) => {
                *self.file.lock() = None;
                self.update_status(|status| {
                    status.phase = Phase::Error;
                    status.error = Some("download".to_string());
                    status.detail = Some(detail.clone());
                });
                self.next_check
                    .store(now_ms() + retry_delay_ms(1), Ordering::Release);
                return Err("read");
            }
            Err(err) => {
                self.install_failed(err.to_string(), &version, trigger);
                return Err("install");
            }
        };
        if trigger != Trigger::Quit && self.quit_install.load(Ordering::Acquire) {
            return Err("quitting");
        }
        self.update_status(|status| {
            status.phase = Phase::Installing;
            status.error = None;
            status.detail = None;
        });
        if trigger != Trigger::Quit {
            self.announce(Notice::Installing, Some(version.clone()));
            tokio::time::sleep(INSTALL_LEAD).await;
        }
        COMMITTED.store(true, Ordering::Release);
        if trigger != Trigger::Quit {
            if let Some(reason) = self.blocker(trigger) {
                COMMITTED.store(false, Ordering::Release);
                tracing::info!("update {version} install postponed ({reason})");
                self.update_status(|status| status.phase = Phase::Ready);
                if reason != "disabled" {
                    self.announce(Notice::Postponed, Some(version));
                }
                return Err("interrupted");
            }
        }
        let attempt = self.record_attempt(&version);
        self.hook_ran.store(false, Ordering::Release);
        let relaunch = trigger != Trigger::Quit && !self.quit_requested.load(Ordering::Acquire);
        tracing::info!("installing update {version} (attempt {attempt}, {trigger:?}, relaunch {relaunch})");
        let installer = update.restart_after_install(relaunch);
        self.in_installer.store(true, Ordering::Release);
        let outcome = tokio::task::spawn_blocking(move || installer.install(bytes)).await;
        self.in_installer.store(false, Ordering::Release);
        match outcome {
            Ok(Ok(())) => {
                tracing::info!("update {version} installed");
                *self.pending.lock() = None;
                *self.file.lock() = None;
                #[cfg(target_os = "macos")]
                {
                    if relaunch && !self.quit_requested.load(Ordering::Acquire) {
                        self.app.request_restart();
                    }
                }
                Ok(())
            }
            Ok(Err(err)) => {
                self.install_failed(err.to_string(), &version, trigger);
                Err("install")
            }
            Err(err) => {
                self.install_failed(err.to_string(), &version, trigger);
                Err("install")
            }
        }
    }

    fn install_failed(&self, detail: String, version: &str, trigger: Trigger) {
        COMMITTED.store(false, Ordering::Release);
        tracing::error!("update {version} install failed: {detail}");
        let quitting = trigger == Trigger::Quit || self.quit_requested.load(Ordering::Acquire);
        if !quitting {
            self.mark_reported(version);
        }
        if self.hook_ran.swap(false, Ordering::AcqRel) && !quitting {
            tray::set_visible(&self.app, true);
            if let Some(state) = self.app.try_state::<SharedState>() {
                let state = state.inner().clone();
                tauri::async_runtime::spawn(async move {
                    services::restart_sidecar(&state).await;
                });
            }
        }
        self.hold_until
            .store(now_ms() + INSTALL_COOLDOWN_MS, Ordering::Release);
        let blocked = is_blocked(read_marker(&self.marker_path).as_ref(), version);
        self.update_status(|status| {
            status.phase = Phase::Ready;
            status.error = Some("install".to_string());
            status.detail = Some(detail.clone());
            status.blocked = blocked;
        });
        if !quitting {
            self.announce(Notice::Failed, Some(version.to_string()));
        }
    }

    #[cfg(windows)]
    fn before_exit(&self) {
        self.hook_ran.store(true, Ordering::Release);
        if let Some(state) = self.app.try_state::<SharedState>() {
            crate::widget_pos::flush(&state);
            state.stop_sidecar();
        }
        tray::set_visible(&self.app, false);
    }

    async fn cleanup(self: &Arc<Self>) {
        let _guard = self.op.lock().await;
        let keep = self.file.lock().clone();
        let auto = self.auto_enabled();
        let dir = self.cache_dir.clone();
        let current = self.current.clone();
        #[cfg(windows)]
        let app_name = self.app.package_info().name.clone();
        let result = tokio::task::spawn_blocking(move || {
            if auto {
                prune_stale(&dir, keep.as_deref(), &current);
            } else {
                let keep: Vec<PathBuf> = keep.into_iter().collect();
                prune_cache(&dir, &keep);
            }
            #[cfg(windows)]
            remove_installer_leftovers(&std::env::temp_dir(), &app_name);
        })
        .await;
        if let Err(err) = result {
            tracing::debug!("update cleanup failed: {err}");
        }
    }
}

fn build_supported() -> bool {
    !cfg!(debug_assertions)
        && (cfg!(all(windows, target_arch = "x86_64"))
            || cfg!(all(target_os = "macos", target_arch = "aarch64")))
}

fn target_key(os: &str, identifier: &str) -> Option<&'static str> {
    match os {
        "windows" if identifier.ends_with(".cpu") => Some("windows-x86_64-cpu"),
        "windows" => Some("windows-x86_64-nvidia"),
        "macos" => Some("darwin-aarch64"),
        _ => None,
    }
}

fn configured_pubkey(app: &AppHandle) -> Option<String> {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|value| value.get("pubkey"))
        .and_then(|value| value.as_str())
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
}

fn classify(err: &tauri_plugin_updater::Error) -> Failure {
    use tauri_plugin_updater::Error as E;
    let code = match err {
        E::Reqwest(_) | E::Network(_) | E::Io(_) => "network",
        E::ReleaseNotFound
        | E::Serialization(_)
        | E::TargetNotFound(_)
        | E::TargetsNotFound(_)
        | E::Semver(_)
        | E::UrlParse(_) => "unavailable",
        _ => "unknown",
    };
    Failure::new(code, err.to_string())
}

fn cancelled_failure() -> Failure {
    Failure::new("cancelled", "automatic updates were turned off")
}

#[cfg(target_os = "macos")]
fn location_issue() -> Option<&'static str> {
    let Ok(exe) = std::env::current_exe() else {
        return Some("location_move");
    };
    let text = exe.to_string_lossy();
    if text.contains("/AppTranslocation/") || text.starts_with("/Volumes/") {
        return Some("location_move");
    }
    let Some(bundle) = exe
        .ancestors()
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
    else {
        return Some("location_move");
    };
    let Some(parent) = bundle.parent() else {
        return Some("location_move");
    };
    let probe = parent.join(format!(".synapse-update-{}", std::process::id()));
    match std::fs::File::create(&probe) {
        Ok(file) => {
            drop(file);
            remove_file_quietly(&probe);
            None
        }
        Err(err) => {
            tracing::info!("app folder {} is not writable: {err}", parent.display());
            Some("location_admin")
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn location_issue() -> Option<&'static str> {
    None
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn rewound(stamp: i64, now: i64, horizon: i64) -> Option<i64> {
    (stamp > now.saturating_add(horizon)).then_some(now)
}

fn retry_delay_ms(failures: u32) -> i64 {
    let index = (failures.max(1) as usize - 1).min(RETRY_MINUTES.len() - 1);
    RETRY_MINUTES[index] * 60_000
}

fn failure_delay_ms(code: &str, failed: u32) -> i64 {
    match code {
        "signature" | "disk" => CHECK_INTERVAL_MS,
        _ if failed > QUICK_RETRIES => CHECK_INTERVAL_MS,
        _ => retry_delay_ms(failed),
    }
}

fn activity_edge(was_active: bool, active: bool) -> bool {
    active || was_active
}

fn percent(done: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    let value = u128::from(done.min(total)) * 100 / u128::from(total);
    u8::try_from(value).unwrap_or(100)
}

fn tray_item(status: &UpdateStatus) -> TrayItem {
    match status.phase {
        Phase::Checking => TrayItem::Checking,
        Phase::Downloading => TrayItem::Downloading(percent(status.downloaded, status.total)),
        Phase::Ready | Phase::Available => {
            TrayItem::Install(status.version.clone().unwrap_or_default())
        }
        Phase::Installing => TrayItem::Installing,
        Phase::Unsupported | Phase::Idle | Phase::UpToDate | Phase::Error => TrayItem::Check,
    }
}

fn install_blocker(activity: &Activity, trigger: Trigger) -> Option<&'static str> {
    match trigger {
        Trigger::Quit => None,
        Trigger::Manual => (activity.dictating || activity.background_work).then_some("busy"),
        Trigger::Auto => (activity.dictating
            || activity.background_work
            || activity.booting
            || activity.window_focused
            || activity.idle_ms < QUIET_MS)
            .then_some("waiting"),
    }
}

fn startup_outcome(marker: Option<&Marker>, current: &str) -> Outcome {
    let Some(marker) = marker else {
        return Outcome::Nothing;
    };
    let (Ok(target), Ok(running)) = (
        semver::Version::parse(marker.version.trim_start_matches('v')),
        semver::Version::parse(current.trim_start_matches('v')),
    ) else {
        return Outcome::Stale;
    };
    match running.cmp(&target) {
        std::cmp::Ordering::Equal => Outcome::Updated(marker.version.clone()),
        std::cmp::Ordering::Less => Outcome::Failed {
            version: marker.version.clone(),
            reported: marker.reported,
        },
        std::cmp::Ordering::Greater => Outcome::Stale,
    }
}

fn next_attempt(marker: Option<&Marker>, version: &str) -> u32 {
    marker
        .filter(|marker| marker.version == version)
        .map(|marker| marker.attempts)
        .unwrap_or(0)
        .saturating_add(1)
}

fn is_blocked(marker: Option<&Marker>, version: &str) -> bool {
    marker
        .map(|marker| marker.version == version && marker.attempts >= MAX_ATTEMPTS)
        .unwrap_or(false)
}

fn read_marker(path: &Path) -> Option<Marker> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_marker(path: &Path, marker: &Marker) {
    match serde_json::to_vec(marker) {
        Ok(bytes) => {
            if let Err(err) = atomic_io::write_durable(path, &bytes) {
                tracing::warn!("update marker not written: {err}");
            }
        }
        Err(err) => tracing::warn!("update marker not serialized: {err}"),
    }
}

fn remove_file_quietly(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::debug!("could not remove {}: {err}", path.display()),
    }
}

fn cache_file_name(version: &str) -> Option<String> {
    let parsed = semver::Version::parse(version.trim_start_matches('v')).ok()?;
    Some(format!("{CACHE_PREFIX}{parsed}.{CACHE_EXT}"))
}

fn cached_version(name: &str) -> Option<semver::Version> {
    let rest = name.strip_prefix(CACHE_PREFIX)?;
    let stem = rest
        .strip_suffix(CACHE_EXT)
        .or_else(|| rest.strip_suffix(PART_EXT))?
        .strip_suffix('.')?;
    semver::Version::parse(stem).ok()
}

fn prune_cache(dir: &Path, keep: &[PathBuf]) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if keep.iter().any(|kept| kept == &path) || !path.is_file() {
            continue;
        }
        remove_file_quietly(&path);
    }
}

fn prune_stale(dir: &Path, keep: Option<&Path>, current: &str) {
    let running = semver::Version::parse(current.trim_start_matches('v')).ok();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if keep == Some(path.as_path()) || !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let newer = match (cached_version(&name), running.as_ref()) {
            (Some(version), Some(running)) => version > *running,
            _ => false,
        };
        if !newer {
            remove_file_quietly(&path);
        }
    }
}

#[cfg(windows)]
fn remove_installer_leftovers(temp: &Path, app_name: &str) {
    let Ok(entries) = std::fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_installer_leftover(&name, app_name) {
            continue;
        }
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => tracing::info!("removed installer leftovers {}", path.display()),
            Err(err) => tracing::debug!("installer leftovers {} kept: {err}", path.display()),
        }
    }
}

#[cfg(any(windows, test))]
fn is_installer_leftover(name: &str, app_name: &str) -> bool {
    let Some(rest) = name
        .strip_prefix(app_name)
        .and_then(|rest| rest.strip_prefix('-'))
    else {
        return false;
    };
    let Some((version, suffix)) = rest.split_once("-updater-") else {
        return false;
    };
    semver::Version::parse(version).is_ok()
        && !suffix.is_empty()
        && suffix.chars().all(|c| c.is_ascii_alphanumeric())
}

fn content_range(headers: &reqwest::header::HeaderMap) -> Option<(u64, Option<u64>)> {
    let value = headers.get(reqwest::header::CONTENT_RANGE)?.to_str().ok()?;
    parse_content_range(value)
}

fn parse_content_range(value: &str) -> Option<(u64, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let start = start.trim().parse::<u64>().ok()?;
    let end = end.trim().parse::<u64>().ok()?;
    if end < start {
        return None;
    }
    let total = match total.trim() {
        "*" => None,
        size => {
            let size = size.parse::<u64>().ok()?;
            if size <= end {
                return None;
            }
            Some(size)
        }
    };
    Some((start, total))
}

fn decode_text(encoded: &str) -> Result<String, VerifyError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|err| VerifyError::Invalid(format!("invalid base64: {err}")))?;
    String::from_utf8(bytes)
        .map_err(|_| VerifyError::Invalid("signature data is not UTF-8".to_string()))
}

fn decode_keys(signature: &str, pubkey: &str) -> Result<(PublicKey, Signature), VerifyError> {
    let public_key = PublicKey::decode(&decode_text(pubkey)?)
        .map_err(|err| VerifyError::Invalid(format!("invalid public key: {err}")))?;
    let signature = Signature::decode(&decode_text(signature)?)
        .map_err(|err| VerifyError::Invalid(format!("invalid signature: {err}")))?;
    Ok((public_key, signature))
}

fn signed_version(trusted_comment: &str) -> Option<&str> {
    trusted_comment
        .split('\t')
        .find_map(|field| field.strip_prefix("version:"))
}

fn check_signed_version(trusted_comment: &str, announced: &str) -> Result<(), VerifyError> {
    let Some(signed) = signed_version(trusted_comment) else {
        return Ok(());
    };
    let matches = match (
        semver::Version::parse(signed.trim_start_matches('v')),
        semver::Version::parse(announced.trim_start_matches('v')),
    ) {
        (Ok(signed), Ok(announced)) => signed == announced,
        _ => signed == announced,
    };
    if matches {
        Ok(())
    } else {
        Err(VerifyError::Invalid(format!(
            "signed for version {signed}, announced as {announced}"
        )))
    }
}

fn verify_file(
    path: &Path,
    signature: &str,
    pubkey: &str,
    announced: &str,
) -> Result<(), VerifyError> {
    let (public_key, signature) = decode_keys(signature, pubkey)?;
    let mut verifier = public_key
        .verify_stream(&signature)
        .map_err(|err| VerifyError::Invalid(format!("unsupported signature: {err}")))?;
    let mut file = std::fs::File::open(path)
        .map_err(|err| VerifyError::Io(format!("cannot read update: {err}")))?;
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => verifier.update(&buffer[..read]),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(VerifyError::Io(format!("cannot read update: {err}"))),
        }
    }
    verifier
        .finalize()
        .map_err(|err| VerifyError::Invalid(format!("signature mismatch: {err}")))?;
    check_signed_version(signature.trusted_comment(), announced)
}

fn verify_bytes(
    bytes: &[u8],
    signature: &str,
    pubkey: &str,
    announced: &str,
) -> Result<(), VerifyError> {
    let (public_key, signature) = decode_keys(signature, pubkey)?;
    public_key
        .verify(bytes, &signature, false)
        .map_err(|err| VerifyError::Invalid(format!("signature mismatch: {err}")))?;
    check_signed_version(signature.trusted_comment(), announced)
}

fn read_verified(
    path: &Path,
    signature: &str,
    pubkey: &str,
    announced: &str,
) -> Result<Vec<u8>, VerifyError> {
    let bytes = std::fs::read(path)
        .map_err(|err| VerifyError::Io(format!("cannot read update: {err}")))?;
    verify_bytes(&bytes, signature, pubkey, announced)?;
    Ok(bytes)
}

fn notice_text(language: &str, notice: Notice, version: &str) -> (String, String) {
    let (title, body) = if language == "pt" {
        match notice {
            Notice::Installing => (
                "Atualizando o Synapse",
                "Instalando a versão {v}. O Synapse vai reabrir sozinho.",
            ),
            Notice::Installed => ("Synapse atualizado", "Agora você está na versão {v}."),
            Notice::UpToDate => (
                "Synapse está atualizado",
                "Você já tem a versão mais recente ({v}).",
            ),
            Notice::Downloading => (
                "Atualização encontrada",
                "Baixando a versão {v}. Ela será instalada quando o Synapse estiver ocioso.",
            ),
            Notice::Available => (
                "Atualização disponível",
                "A versão {v} está disponível. Instale pela bandeja ou em Ajustes > Sobre.",
            ),
            Notice::Failed => (
                "Atualização não concluída",
                "Não foi possível instalar a versão {v}. Veja Ajustes > Sobre.",
            ),
            Notice::CheckFailed => (
                "Não foi possível verificar atualizações",
                "Tente de novo em alguns minutos.",
            ),
            Notice::Postponed => (
                "Atualização adiada",
                "A versão {v} será instalada quando o Synapse estiver ocioso.",
            ),
            Notice::Busy => (
                "Atualização aguardando",
                "Aguarde o Synapse terminar o que está fazendo.",
            ),
            Notice::Relocate => (
                "Mova o Synapse para atualizar",
                "Mova o Synapse para a pasta Aplicativos e abra de novo para instalar a versão {v}.",
            ),
        }
    } else {
        match notice {
            Notice::Installing => (
                "Updating Synapse",
                "Installing version {v}. Synapse will reopen by itself.",
            ),
            Notice::Installed => ("Synapse updated", "You're now on version {v}."),
            Notice::UpToDate => (
                "Synapse is up to date",
                "You already have the latest version ({v}).",
            ),
            Notice::Downloading => (
                "Update found",
                "Downloading version {v}. It will install when Synapse is idle.",
            ),
            Notice::Available => (
                "Update available",
                "Version {v} is available. Install it from the tray or Settings > About.",
            ),
            Notice::Failed => (
                "Update not completed",
                "Version {v} could not be installed. See Settings > About.",
            ),
            Notice::CheckFailed => (
                "Couldn't check for updates",
                "Try again in a few minutes.",
            ),
            Notice::Postponed => (
                "Update postponed",
                "Version {v} will install when Synapse is idle.",
            ),
            Notice::Busy => (
                "Update waiting",
                "Wait for Synapse to finish what it's doing.",
            ),
            Notice::Relocate => (
                "Move Synapse to update",
                "Move Synapse to the Applications folder and open it again to install version {v}.",
            ),
        }
    };
    (title.to_string(), body.replace("{v}", version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    const TEST_PUBKEY: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDJBMjZGMEU1QTA1Q0UyM0MKUldRODRseWc1ZkFtS2dyZWZNQ2xzM09XV2JOTVJGUGxOTFYxWDFNZjViMm1GU2V3NW9EVzVFb2kK";
    const TEST_SIGNATURE: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRODRseWc1ZkFtS3Blcjg2MnQ2K0hlZmt3TW9yRWY2SUx2VC9QVzlCZEFiREZFN25sQ0Y3ZzlqVzNxU21oaTI0bTIzSjc2N2RjZ3hGNEQyNWQwa1lTTVQ5OE5FNlZJaWdvPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkwMjYxMTYxCWZpbGU6ZGF0YS5iaW4KT1p2OVFxN3RlNmloWU5KN1BCSmo0NUxSM3FibHA2aUpYWDdaRWxBZTAvTSs2RDZkZDc3eGJWZnIwYUQwUTl1dHdOT0RiR2hqZ3o2TWI0TGNVbjVWREE9PQo=";

    static N: AtomicU64 = AtomicU64::new(0);

    fn payload() -> Vec<u8> {
        (0..3_145_728u32).map(|i| (i % 251) as u8).collect()
    }

    fn scratch(tag: &str) -> PathBuf {
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("synapse_upd_{}_{n}_{tag}", std::process::id()))
    }

    fn write_scratch(tag: &str, bytes: &[u8]) -> PathBuf {
        let path = scratch(tag);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    fn app_config() -> serde_json::Value {
        serde_json::from_str(include_str!("../tauri.conf.json")).unwrap()
    }

    fn activity(idle_ms: i64) -> Activity {
        Activity {
            idle_ms,
            ..Activity::default()
        }
    }

    fn marker(version: &str, attempts: u32, reported: bool) -> Marker {
        Marker {
            version: version.to_string(),
            from: "1.0.0".to_string(),
            attempts,
            reported,
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut left: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        left.sort();
        left
    }

    #[test]
    fn stream_verification_accepts_tauri_signature() {
        let path = write_scratch("ok", &payload());
        assert_eq!(verify_file(&path, TEST_SIGNATURE, TEST_PUBKEY, "1.0.0"), Ok(()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn memory_verification_accepts_tauri_signature() {
        assert_eq!(verify_bytes(&payload(), TEST_SIGNATURE, TEST_PUBKEY, "1.0.0"), Ok(()));
        let path = write_scratch("read", &payload());
        assert_eq!(
            read_verified(&path, TEST_SIGNATURE, TEST_PUBKEY, "1.0.0").map(|bytes| bytes.len()),
            Ok(payload().len())
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn verification_rejects_tampered_payload() {
        let mut data = payload();
        data[1_234_567] ^= 0x01;
        let path = write_scratch("tampered", &data);
        assert!(matches!(
            verify_file(&path, TEST_SIGNATURE, TEST_PUBKEY, "1.0.0"),
            Err(VerifyError::Invalid(_))
        ));
        assert!(matches!(
            verify_bytes(&data, TEST_SIGNATURE, TEST_PUBKEY, "1.0.0"),
            Err(VerifyError::Invalid(_))
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn verification_rejects_truncated_payload() {
        let mut data = payload();
        data.truncate(data.len() - 1);
        let path = write_scratch("truncated", &data);
        assert!(matches!(
            verify_file(&path, TEST_SIGNATURE, TEST_PUBKEY, "1.0.0"),
            Err(VerifyError::Invalid(_))
        ));
        assert!(matches!(
            verify_bytes(&data, TEST_SIGNATURE, TEST_PUBKEY, "1.0.0"),
            Err(VerifyError::Invalid(_))
        ));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn verification_rejects_other_key() {
        let config = app_config();
        let app_key = config["plugins"]["updater"]["pubkey"].as_str().unwrap();
        assert!(matches!(
            verify_bytes(&payload(), TEST_SIGNATURE, app_key, "1.0.0"),
            Err(VerifyError::Invalid(_))
        ));
    }

    #[test]
    fn verification_separates_io_from_invalid_input() {
        assert!(matches!(
            verify_bytes(&payload(), "not base64 at all!", TEST_PUBKEY, "1.0.0"),
            Err(VerifyError::Invalid(_))
        ));
        assert!(matches!(
            verify_bytes(&payload(), TEST_SIGNATURE, "bm90IGEga2V5", "1.0.0"),
            Err(VerifyError::Invalid(_))
        ));
        assert!(matches!(
            verify_bytes(&payload(), "", TEST_PUBKEY, "1.0.0"),
            Err(VerifyError::Invalid(_))
        ));
        let missing = scratch("missing");
        assert!(matches!(
            verify_file(&missing, TEST_SIGNATURE, TEST_PUBKEY, "1.0.0"),
            Err(VerifyError::Io(_))
        ));
        assert!(matches!(
            read_verified(&missing, TEST_SIGNATURE, TEST_PUBKEY, "1.0.0"),
            Err(VerifyError::Io(_))
        ));
    }

    #[test]
    fn signed_version_must_match_when_present() {
        assert_eq!(signed_version("timestamp:1\tfile:a.exe"), None);
        assert_eq!(
            signed_version("timestamp:1\tfile:a.exe\tversion:1.2.3"),
            Some("1.2.3")
        );
        assert_eq!(check_signed_version("timestamp:1\tfile:a.exe", "9.9.9"), Ok(()));
        assert_eq!(check_signed_version("timestamp:1\tversion:1.2.3", "1.2.3"), Ok(()));
        assert_eq!(check_signed_version("timestamp:1\tversion:v1.2.3", "1.2.3"), Ok(()));
        assert!(matches!(
            check_signed_version("timestamp:1\tversion:1.2.2", "1.2.3"),
            Err(VerifyError::Invalid(_))
        ));
        assert!(matches!(
            check_signed_version("version:nightly", "1.2.3"),
            Err(VerifyError::Invalid(_))
        ));
        assert_eq!(check_signed_version("version:nightly", "nightly"), Ok(()));
    }

    #[test]
    fn configured_updater_is_valid() {
        let config = app_config();
        let updater = &config["plugins"]["updater"];
        let key = updater["pubkey"].as_str().unwrap();
        assert!(PublicKey::decode(&decode_text(key).unwrap()).is_ok());
        let endpoints = updater["endpoints"].as_array().unwrap();
        assert_eq!(endpoints.len(), 1);
        assert_eq!(
            endpoints[0].as_str().unwrap(),
            "https://github.com/Monayzera/Synapse/releases/latest/download/latest.json"
        );
        assert_eq!(updater["windows"]["installMode"].as_str().unwrap(), "passive");
        assert_eq!(
            config["bundle"]["windows"]["nsis"]["installMode"].as_str().unwrap(),
            "currentUser"
        );
        assert!(config["bundle"].get("createUpdaterArtifacts").is_none());
    }

    #[test]
    fn ci_config_only_turns_on_updater_artifacts() {
        let ci: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.updater.conf.json")).unwrap();
        assert_eq!(ci["bundle"]["createUpdaterArtifacts"].as_bool(), Some(true));
        assert!(ci.get("plugins").is_none());
        assert!(ci.get("identifier").is_none());
    }

    #[test]
    fn each_variant_has_its_own_target() {
        let main = app_config();
        let cpu: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.cpu.conf.json")).unwrap();
        assert_eq!(
            target_key("windows", main["identifier"].as_str().unwrap()),
            Some("windows-x86_64-nvidia")
        );
        assert_eq!(
            target_key("windows", cpu["identifier"].as_str().unwrap()),
            Some("windows-x86_64-cpu")
        );
        assert_eq!(
            target_key("macos", main["identifier"].as_str().unwrap()),
            Some("darwin-aarch64")
        );
        assert_eq!(target_key("linux", "com.synapse.voice"), None);
    }

    #[test]
    fn retry_delays_back_off_and_cap() {
        assert_eq!(retry_delay_ms(0), 5 * 60_000);
        assert_eq!(retry_delay_ms(1), 5 * 60_000);
        assert_eq!(retry_delay_ms(2), 15 * 60_000);
        assert_eq!(retry_delay_ms(3), 30 * 60_000);
        assert_eq!(retry_delay_ms(4), 60 * 60_000);
        assert_eq!(retry_delay_ms(u32::MAX), 60 * 60_000);
    }

    #[test]
    fn download_failures_slow_down() {
        assert_eq!(failure_delay_ms("download", 1), 5 * 60_000);
        assert_eq!(failure_delay_ms("download", QUICK_RETRIES), 60 * 60_000);
        assert_eq!(failure_delay_ms("download", QUICK_RETRIES + 1), CHECK_INTERVAL_MS);
        assert_eq!(failure_delay_ms("signature", 1), CHECK_INTERVAL_MS);
        assert_eq!(failure_delay_ms("disk", 1), CHECK_INTERVAL_MS);
        assert_eq!(failure_delay_ms("unknown", 2), 15 * 60_000);
    }

    #[test]
    fn clock_rewinds_are_repaired() {
        let now = 1_000_000_000;
        assert_eq!(rewound(now + CHECK_INTERVAL_MS, now, CHECK_INTERVAL_MS), None);
        assert_eq!(rewound(now + CHECK_INTERVAL_MS + 1, now, CHECK_INTERVAL_MS), Some(now));
        assert_eq!(rewound(now - 5, now, CHECK_INTERVAL_MS), None);
        assert_eq!(rewound(now, now, 0), None);
        assert_eq!(rewound(now + 1, now, 0), Some(now));
        assert_eq!(rewound(i64::MAX, i64::MAX, CHECK_INTERVAL_MS), None);
    }

    #[test]
    fn quiet_window_counts_from_the_end_of_dictation() {
        assert!(activity_edge(false, true));
        assert!(activity_edge(true, true));
        assert!(activity_edge(true, false));
        assert!(!activity_edge(false, false));
    }

    #[test]
    fn percent_is_bounded() {
        assert_eq!(percent(0, 0), 0);
        assert_eq!(percent(50, 100), 50);
        assert_eq!(percent(100, 100), 100);
        assert_eq!(percent(250, 100), 100);
        assert_eq!(percent(u64::MAX, u64::MAX), 100);
        assert_eq!(percent(608_999_999, 609_000_000), 99);
    }

    #[test]
    fn automatic_install_waits_for_quiet_idle_app() {
        assert_eq!(install_blocker(&activity(QUIET_MS), Trigger::Auto), None);
        assert!(install_blocker(&activity(QUIET_MS - 1), Trigger::Auto).is_some());
        for busy in [
            Activity { dictating: true, ..activity(QUIET_MS * 10) },
            Activity { background_work: true, ..activity(QUIET_MS * 10) },
            Activity { booting: true, ..activity(QUIET_MS * 10) },
            Activity { window_focused: true, ..activity(QUIET_MS * 10) },
        ] {
            assert!(install_blocker(&busy, Trigger::Auto).is_some());
        }
    }

    #[test]
    fn manual_install_only_waits_for_real_work() {
        assert_eq!(install_blocker(&activity(0), Trigger::Manual), None);
        let focused = Activity { window_focused: true, booting: true, ..activity(0) };
        assert_eq!(install_blocker(&focused, Trigger::Manual), None);
        let dictating = Activity { dictating: true, ..activity(0) };
        assert_eq!(install_blocker(&dictating, Trigger::Manual), Some("busy"));
        let downloading = Activity { background_work: true, ..activity(0) };
        assert_eq!(install_blocker(&downloading, Trigger::Manual), Some("busy"));
        assert_eq!(install_blocker(&dictating, Trigger::Quit), None);
    }

    #[test]
    fn startup_outcome_follows_versions() {
        assert_eq!(startup_outcome(None, "1.0.1"), Outcome::Nothing);
        assert_eq!(
            startup_outcome(Some(&marker("1.0.1", 1, false)), "1.0.1"),
            Outcome::Updated("1.0.1".to_string())
        );
        assert_eq!(
            startup_outcome(Some(&marker("1.0.2", 1, false)), "1.0.1"),
            Outcome::Failed { version: "1.0.2".to_string(), reported: false }
        );
        assert_eq!(
            startup_outcome(Some(&marker("1.0.2", 1, true)), "1.0.1"),
            Outcome::Failed { version: "1.0.2".to_string(), reported: true }
        );
        assert_eq!(startup_outcome(Some(&marker("1.0.1", 1, false)), "1.0.3"), Outcome::Stale);
        assert_eq!(startup_outcome(Some(&marker("garbage", 1, false)), "1.0.1"), Outcome::Stale);
        assert_eq!(
            startup_outcome(Some(&marker("v1.0.1", 1, false)), "1.0.1"),
            Outcome::Updated("v1.0.1".to_string())
        );
    }

    #[test]
    fn install_attempts_are_counted_per_version() {
        assert_eq!(next_attempt(None, "1.0.2"), 1);
        assert_eq!(next_attempt(Some(&marker("1.0.2", 1, true)), "1.0.2"), 2);
        assert_eq!(next_attempt(Some(&marker("1.0.2", 5, true)), "1.0.3"), 1);
        assert!(!is_blocked(None, "1.0.2"));
        assert!(!is_blocked(Some(&marker("1.0.2", MAX_ATTEMPTS - 1, true)), "1.0.2"));
        assert!(is_blocked(Some(&marker("1.0.2", MAX_ATTEMPTS, true)), "1.0.2"));
        assert!(!is_blocked(Some(&marker("1.0.2", MAX_ATTEMPTS, true)), "1.0.3"));
    }

    #[test]
    fn marker_roundtrip_and_legacy_fields() {
        let path = scratch("marker.json");
        assert_eq!(read_marker(&path), None);
        let written = marker("1.0.2", 2, true);
        write_marker(&path, &written);
        assert_eq!(read_marker(&path), Some(written));
        std::fs::write(&path, br#"{"version":"1.0.3","from":"1.0.1","attempts":1}"#).unwrap();
        assert_eq!(read_marker(&path).map(|m| m.reported), Some(false));
        std::fs::write(&path, b"{ broken").unwrap();
        assert_eq!(read_marker(&path), None);
        remove_file_quietly(&path);
        remove_file_quietly(&path);
        assert!(!path.exists());
    }

    #[test]
    fn cache_names_are_safe() {
        assert_eq!(cache_file_name("1.2.3").as_deref(), Some("synapse-1.2.3.bin"));
        assert_eq!(cache_file_name("v1.2.3").as_deref(), Some("synapse-1.2.3.bin"));
        assert_eq!(cache_file_name("../../evil"), None);
        assert_eq!(cache_file_name("1.2.3/../../x"), None);
        assert_eq!(cached_version("synapse-1.2.3.bin"), semver::Version::parse("1.2.3").ok());
        assert_eq!(cached_version("synapse-1.2.3.part"), semver::Version::parse("1.2.3").ok());
        assert_eq!(cached_version("synapse-1.2.3.tmp"), None);
        assert_eq!(cached_version("other-1.2.3.bin"), None);
    }

    #[test]
    fn stale_cache_is_pruned_but_newer_download_kept() {
        let dir = scratch("cache");
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "synapse-1.0.0.bin",
            "synapse-0.9.0.bin",
            "synapse-0.9.0.part",
            "synapse-9.0.0.bin",
            "synapse-9.0.0.part",
            "junk.txt",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        prune_stale(&dir, None, "1.0.0");
        assert_eq!(
            names(&dir),
            vec!["synapse-9.0.0.bin".to_string(), "synapse-9.0.0.part".to_string()]
        );
        std::fs::write(dir.join("synapse-8.0.0.bin"), b"x").unwrap();
        prune_cache(&dir, &[dir.join("synapse-9.0.0.bin")]);
        assert_eq!(names(&dir), vec!["synapse-9.0.0.bin".to_string()]);
        prune_cache(&dir, &[]);
        assert!(names(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installer_leftovers_match_only_this_app() {
        assert!(is_installer_leftover("Synapse-1.0.2-updater-Ab12Cd", "Synapse"));
        assert!(is_installer_leftover("Synapse CPU-1.0.2-updater-x9Y8z7", "Synapse CPU"));
        assert!(!is_installer_leftover("Synapse CPU-1.0.2-updater-x9Y8z7", "Synapse"));
        assert!(!is_installer_leftover("Synapse-1.0.2-updater-", "Synapse"));
        assert!(!is_installer_leftover("Synapse-latest-updater-abc", "Synapse"));
        assert!(!is_installer_leftover("Other-1.0.2-updater-abc", "Synapse"));
        assert!(!is_installer_leftover("Synapse-1.0.2-installer.exe", "Synapse"));
        assert!(!is_installer_leftover("Synapse-1.0.2-updater-ab.cd", "Synapse"));
    }

    #[test]
    fn content_range_headers_are_parsed_strictly() {
        assert_eq!(
            parse_content_range("bytes 1000-608999999/609000000"),
            Some((1000, Some(609_000_000)))
        );
        assert_eq!(parse_content_range("bytes 0-99/*"), Some((0, None)));
        assert_eq!(parse_content_range(" bytes 5-9/10 "), Some((5, Some(10))));
        assert_eq!(parse_content_range("bytes 9-5/10"), None);
        assert_eq!(parse_content_range("bytes 0-10/10"), None);
        assert_eq!(parse_content_range("bytes */10"), None);
        assert_eq!(parse_content_range("items 0-9/10"), None);
        assert_eq!(parse_content_range("bytes 0-9"), None);
        assert_eq!(parse_content_range(""), None);
    }

    #[test]
    fn tray_item_follows_phase() {
        let mut status = UpdateStatus::new(Phase::UpToDate, "1.0.0".to_string());
        assert_eq!(tray_item(&status), TrayItem::Check);
        status.phase = Phase::Checking;
        assert_eq!(tray_item(&status), TrayItem::Checking);
        status.phase = Phase::Downloading;
        status.downloaded = 30;
        status.total = 120;
        assert_eq!(tray_item(&status), TrayItem::Downloading(25));
        status.phase = Phase::Ready;
        status.version = Some("1.0.2".to_string());
        assert_eq!(tray_item(&status), TrayItem::Install("1.0.2".to_string()));
        status.phase = Phase::Available;
        assert_eq!(tray_item(&status), TrayItem::Install("1.0.2".to_string()));
        status.phase = Phase::Installing;
        assert_eq!(tray_item(&status), TrayItem::Installing);
        status.phase = Phase::Error;
        assert_eq!(tray_item(&status), TrayItem::Check);
    }

    #[test]
    fn notices_name_the_version_in_both_languages() {
        for notice in [
            Notice::Installing,
            Notice::Installed,
            Notice::UpToDate,
            Notice::Downloading,
            Notice::Available,
            Notice::Failed,
            Notice::Postponed,
            Notice::Relocate,
        ] {
            for language in ["pt", "en"] {
                let (title, body) = notice_text(language, notice, "7.8.9");
                assert!(!title.is_empty());
                assert!(body.contains("7.8.9"), "{language} {notice:?}");
                assert!(!body.contains("{v}"));
            }
        }
        for notice in [Notice::CheckFailed, Notice::Busy] {
            let (pt_title, _) = notice_text("pt", notice, "1.0.0");
            let (en_title, _) = notice_text("en", notice, "1.0.0");
            assert_ne!(pt_title, en_title);
        }
    }

    #[test]
    fn payloads_serialize_for_the_frontend() {
        let status = serde_json::to_value(UpdateStatus::new(Phase::UpToDate, "1.0.0".to_string()))
            .unwrap();
        assert_eq!(status["phase"], "up_to_date");
        assert_eq!(status["current_version"], "1.0.0");
        assert!(status["version"].is_null());
        for (notice, kind) in [
            (Notice::CheckFailed, "check_failed"),
            (Notice::Postponed, "postponed"),
            (Notice::Busy, "busy"),
            (Notice::Relocate, "relocate"),
        ] {
            let value = serde_json::to_value(NoticePayload {
                kind: notice,
                version: None,
            })
            .unwrap();
            assert_eq!(value["kind"], kind);
        }
    }

    #[test]
    fn status_probe_reads_status_payloads() {
        let probe: StatusProbe =
            serde_json::from_str(r#"{"status":"processing","recording":false,"extra":1}"#).unwrap();
        assert_eq!(probe.status, "processing");
        assert!(!probe.recording);
        let probe: StatusProbe = serde_json::from_str("{}").unwrap();
        assert_eq!(probe.status, "");
    }
}
