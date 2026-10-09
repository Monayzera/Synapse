use crate::config::{RecordMode, Settings};
use crate::pipeline;
use crate::state::SharedState;
use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use ashpd::desktop::{CreateSessionOptions, ResponseError, Session};
use ashpd::zbus::fdo::NameOwnerChangedStream;
use ashpd::zvariant::{self, OwnedObjectPath};
use futures_util::StreamExt;
use parking_lot::Mutex;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tokio::sync::{oneshot, watch};
use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xkb::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, ModMask};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

const RETRY_MIN: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(30);
const STABLE_SESSION: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const PORTAL_CALL_TIMEOUT: Duration = Duration::from_secs(15);
const DETACHED_CREATE_TIMEOUT: Duration = Duration::from_secs(120);
const PORTAL_FAILURES_BEFORE_X11: u32 = 5;
const WIDGET_WAIT_STEP: Duration = Duration::from_millis(250);
const WIDGET_WAIT_STEPS: u32 = 80;
const TOGGLE_RETRY_STEP: Duration = Duration::from_millis(100);
const TOGGLE_RETRY_STEPS: u32 = 50;
const PORTAL_BUS_NAME: &str = "org.freedesktop.portal.Desktop";
const DENIED_FILE: &str = "denied_shortcut";
const RELEASE_GRACE: Duration = Duration::from_millis(150);
const KEYBOARD_SCHEMA: &str = "org.gnome.desktop.peripherals.keyboard";

const NAMED_KEYS: &[(&str, &str, u32)] = &[
    ("space", "space", 0x0020),
    ("enter", "Return", 0xff0d),
    ("return", "Return", 0xff0d),
    ("tab", "Tab", 0xff09),
    ("up", "Up", 0xff52),
    ("down", "Down", 0xff54),
    ("left", "Left", 0xff51),
    ("right", "Right", 0xff53),
    ("backspace", "BackSpace", 0xff08),
    ("delete", "Delete", 0xffff),
    ("del", "Delete", 0xffff),
    ("insert", "Insert", 0xff63),
    ("ins", "Insert", 0xff63),
    ("home", "Home", 0xff50),
    ("end", "End", 0xff57),
    ("pageup", "Prior", 0xff55),
    ("pagedown", "Next", 0xff56),
    ("escape", "Escape", 0xff1b),
    ("esc", "Escape", 0xff1b),
    ("`", "grave", 0x0060),
    ("-", "minus", 0x002d),
    ("=", "equal", 0x003d),
    (",", "comma", 0x002c),
    (".", "period", 0x002e),
    ("/", "slash", 0x002f),
    (";", "semicolon", 0x003b),
    ("'", "apostrophe", 0x0027),
    ("[", "bracketleft", 0x005b),
    ("]", "bracketright", 0x005d),
    ("\\", "backslash", 0x005c),
];

#[derive(Clone, PartialEq, Eq)]
struct Accelerator {
    ctrl: bool,
    alt: bool,
    shift: bool,
    logo: bool,
    key_name: String,
    keysym: u32,
}

impl Accelerator {
    fn trigger(&self) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(5);
        if self.ctrl {
            parts.push("CTRL");
        }
        if self.alt {
            parts.push("ALT");
        }
        if self.shift {
            parts.push("SHIFT");
        }
        if self.logo {
            parts.push("LOGO");
        }
        parts.push(self.key_name.as_str());
        parts.join("+")
    }

    fn portal_id(&self) -> String {
        let slug: String = self
            .trigger()
            .to_ascii_lowercase()
            .chars()
            .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
            .collect();
        format!("synapse-{slug}")
    }

    fn x11_modifiers(&self) -> u16 {
        let mut mask = 0u16;
        if self.shift {
            mask |= u16::from(ModMask::SHIFT);
        }
        if self.ctrl {
            mask |= u16::from(ModMask::CONTROL);
        }
        if self.alt {
            mask |= u16::from(ModMask::M1);
        }
        if self.logo {
            mask |= u16::from(ModMask::M4);
        }
        mask
    }
}

fn keysym_for_token(token: &str) -> Option<(String, u32)> {
    let lower = token.to_ascii_lowercase();
    if let [byte] = lower.as_bytes() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            return Some((lower.clone(), u32::from(*byte)));
        }
    }
    if let Some(digits) = lower
        .strip_prefix('f')
        .filter(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
    {
        let number: u32 = digits.parse().ok()?;
        if (1..=24).contains(&number) {
            return Some((format!("F{number}"), 0xffbe + number - 1));
        }
        return None;
    }
    NAMED_KEYS
        .iter()
        .find(|entry| entry.0 == lower.as_str())
        .map(|entry| (entry.1.to_string(), entry.2))
}

fn parse_accelerator(accelerator: &str) -> Option<Accelerator> {
    let mut parsed = Accelerator {
        ctrl: false,
        alt: false,
        shift: false,
        logo: false,
        key_name: String::new(),
        keysym: 0,
    };
    let mut key: Option<(String, u32)> = None;
    for part in accelerator.split('+') {
        let token = part.trim();
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "commandorcontrol" => parsed.ctrl = true,
            "shift" => parsed.shift = true,
            "alt" | "option" => parsed.alt = true,
            "super" | "win" | "meta" | "cmd" | "command" => parsed.logo = true,
            _ => {
                if key.is_some() {
                    return None;
                }
                key = Some(keysym_for_token(token)?);
            }
        }
    }
    let (key_name, keysym) = key?;
    parsed.key_name = key_name;
    parsed.keysym = keysym;
    Some(parsed)
}

struct Desired {
    accelerator: Option<Accelerator>,
    description: &'static str,
}

enum HotEvent {
    Press,
    Release,
}

static DESIRED: Mutex<Desired> = Mutex::new(Desired {
    accelerator: None,
    description: "Dictate with Synapse",
});
static EPOCH: OnceLock<watch::Sender<u64>> = OnceLock::new();
static ACTIVE: AtomicBool = AtomicBool::new(false);
static DISPATCH: OnceLock<Sender<HotEvent>> = OnceLock::new();
static UNSUPPORTED_NOTIFIED: AtomicBool = AtomicBool::new(false);
static DENIAL_REPORTED: AtomicBool = AtomicBool::new(false);

fn epoch() -> &'static watch::Sender<u64> {
    EPOCH.get_or_init(|| watch::channel(0).0)
}

fn bump_epoch() {
    epoch().send_modify(|value| *value = value.wrapping_add(1));
}

fn snapshot() -> (Option<Accelerator>, &'static str) {
    let desired = DESIRED.lock();
    (desired.accelerator.clone(), desired.description)
}

fn dictation_description(ui_language: &str) -> &'static str {
    if crate::tray::resolve_language(ui_language) == "pt" {
        "Ditar com o Synapse"
    } else {
        "Dictate with Synapse"
    }
}

fn denied_marker() -> Option<PathBuf> {
    match crate::linux_portal::xdg_data_home() {
        Ok(dir) => Some(
            dir.join("com.synapse.voice")
                .join("portal")
                .join(DENIED_FILE),
        ),
        Err(err) => {
            tracing::warn!("the declined shortcut will not be remembered: {err}");
            None
        }
    }
}

fn read_denied(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let id = contents.trim();
            (!id.is_empty()).then(|| id.to_string())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!("reading the declined shortcut failed: {err}");
            None
        }
    }
}

fn write_denied(path: &Path, id: &str) {
    if let Err(err) = crate::atomic_io::write_durable(path, id.as_bytes()) {
        tracing::warn!("could not remember the declined shortcut: {err}");
    }
}

fn clear_denied(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => tracing::warn!("could not clear the declined shortcut: {err}"),
    }
}

fn denied_shortcut() -> Option<String> {
    read_denied(&denied_marker()?)
}

fn remember_denied_shortcut(id: &str) {
    if let Some(path) = denied_marker() {
        write_denied(&path, id);
    }
}

fn forget_denied_shortcut() {
    if let Some(path) = denied_marker() {
        clear_denied(&path);
    }
}

pub fn set_bindings(settings: &Settings) -> bool {
    let accelerator = parse_accelerator(&settings.hotkey_ptt);
    let parsed = accelerator.is_some();
    if accelerator.is_none() {
        forget_denied_shortcut();
    }
    let description = dictation_description(&settings.ui_language);
    let changed = {
        let mut desired = DESIRED.lock();
        let changed = desired.accelerator != accelerator || desired.description != description;
        desired.accelerator = accelerator;
        desired.description = description;
        changed
    };
    if changed {
        reset_active("shortcut changed");
        bump_epoch();
    }
    parsed
}

pub fn reset_active(reason: &str) {
    if ACTIVE.swap(false, Ordering::AcqRel) {
        tracing::info!("hotkey state reset ({reason}); releasing the held shortcut");
        send(HotEvent::Release);
    }
}

pub fn request_rebind(reason: &str) {
    tracing::info!("global shortcut rebind requested ({reason})");
    bump_epoch();
}

pub fn retry_after_denial() {
    forget_denied_shortcut();
    request_rebind("retry requested in Settings");
}

pub fn toggle_from_cli(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        for _ in 0..TOGGLE_RETRY_STEPS {
            if let Some(state) = app.try_state::<SharedState>() {
                pipeline::hotkey_toggle(app.clone(), state.inner().clone());
                return;
            }
            tokio::time::sleep(TOGGLE_RETRY_STEP).await;
        }
        tracing::warn!("--toggle ignored: the application state did not become ready");
    });
}

pub fn start(app: AppHandle) {
    let (tx, rx) = channel::<HotEvent>();
    if DISPATCH.set(tx).is_err() {
        return;
    }
    let dispatch_app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("synapse-hotkey-dispatch".to_string())
        .spawn(move || dispatch_loop(dispatch_app, rx));
    if let Err(err) = spawned {
        tracing::error!("could not start thread synapse-hotkey-dispatch: {err}");
        crate::hotkey::set_hook_ready(&app, false);
        report_hotkey_failure(&app, "The global shortcut thread could not be started.");
        return;
    }
    tauri::async_runtime::spawn(backend(app));
}

async fn backend(app: AppHandle) {
    if let Err(err) = crate::linux_portal::register().await {
        tracing::warn!("desktop app registration failed: {err}");
    }
    let mut changes = epoch().subscribe();
    loop {
        if let Some(portal) = open_portal().await {
            if portal_loop(app.clone(), portal).await {
                start_x11(app);
            }
            return;
        }
        if x11_session() {
            start_x11(app);
            return;
        }
        without_portal(&app);
        if changes.changed().await.is_err() {
            return;
        }
    }
}

fn x11_session() -> bool {
    crate::linux_portal::has_x11() && !crate::linux_portal::is_wayland()
}

async fn within<T, E>(
    what: &str,
    call: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, String>
where
    E: std::fmt::Display,
{
    match tokio::time::timeout(PORTAL_CALL_TIMEOUT, call).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(err)) => Err(format!("{what}: {err}")),
        Err(_) => Err(format!(
            "{what}: no answer within {} s",
            PORTAL_CALL_TIMEOUT.as_secs()
        )),
    }
}

async fn open_portal() -> Option<GlobalShortcuts> {
    if !portal_service_known().await {
        tracing::info!("no desktop portal service is installed on this session bus");
        return None;
    }
    match within("the desktop shortcut service", GlobalShortcuts::new()).await {
        Ok(portal) => {
            tracing::info!(
                "global shortcuts use the desktop portal (interface v{})",
                portal.version()
            );
            Some(portal)
        }
        Err(err) => {
            tracing::warn!("global shortcuts portal unavailable: {err}");
            None
        }
    }
}

async fn portal_service_known() -> bool {
    match within("the session bus check", bus_knows_portal()).await {
        Ok(known) => known,
        Err(err) => {
            tracing::warn!("could not check the session bus for the desktop portal: {err}");
            true
        }
    }
}

async fn bus_knows_portal() -> Result<bool, String> {
    let connection = ashpd::zbus::Connection::session()
        .await
        .map_err(|err| format!("could not reach the session bus: {err}"))?;
    let bus = ashpd::zbus::fdo::DBusProxy::new(&connection)
        .await
        .map_err(|err| format!("could not query the session bus: {err}"))?;
    let name = ashpd::zbus::names::BusName::try_from(PORTAL_BUS_NAME)
        .map_err(|err| format!("invalid portal bus name: {err}"))?;
    let running = bus
        .name_has_owner(name)
        .await
        .map_err(|err| format!("could not query the portal owner: {err}"))?;
    if running {
        return Ok(true);
    }
    let activatable = bus
        .list_activatable_names()
        .await
        .map_err(|err| format!("could not list the activatable services: {err}"))?;
    Ok(activatable
        .iter()
        .any(|entry| entry.as_str() == PORTAL_BUS_NAME))
}

fn without_portal(app: &AppHandle) {
    crate::hotkey::set_hook_ready(app, false);
    if UNSUPPORTED_NOTIFIED.swap(true, Ordering::AcqRel) {
        return;
    }
    let command = toggle_command();
    when_widget_ready(app, move |app| {
        crate::pipeline::emit_error_with_params(
            app,
            "hotkey",
            "hotkey_unsupported",
            &format!("This desktop session has no global shortcut service. Create a keyboard shortcut that runs: {command}"),
            serde_json::json!({ "command": command }),
        );
    });
}

fn toggle_command() -> String {
    let exe = match crate::linux_portal::live_executable() {
        Ok(path) => path,
        Err(err) => {
            tracing::warn!("could not resolve the Synapse executable for the toggle command: {err}");
            return "synapse --toggle".to_string();
        }
    };
    match exe.to_str() {
        Some(text) => format!("'{}' --toggle", text.replace('\'', "'\\''")),
        None => {
            tracing::warn!(
                "the Synapse executable path is not valid UTF-8: {}",
                exe.display()
            );
            "synapse --toggle".to_string()
        }
    }
}

fn when_widget_ready<F>(app: &AppHandle, notify: F)
where
    F: FnOnce(&AppHandle) + Send + 'static,
{
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        for _ in 0..WIDGET_WAIT_STEPS {
            let ready = app
                .try_state::<SharedState>()
                .is_some_and(|state| state.widget_ready.load(Ordering::Acquire));
            if ready {
                break;
            }
            tokio::time::sleep(WIDGET_WAIT_STEP).await;
        }
        notify(&app);
    });
}

fn report_hotkey_failure(app: &AppHandle, message: &str) {
    let message = message.to_string();
    when_widget_ready(app, move |app| crate::hotkey::report_failure(app, &message));
}

fn report_denied(app: &AppHandle) {
    when_widget_ready(app, |app| {
        crate::pipeline::emit_error(
            app,
            "hotkey",
            "hotkey_portal_denied",
            "The shortcut was not confirmed in the desktop dialog.",
        );
    });
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

fn send(event: HotEvent) {
    match DISPATCH.get() {
        Some(tx) => {
            if tx.send(event).is_err() {
                tracing::error!("hotkey dispatcher is not running");
            }
        }
        None => tracing::error!("hotkey dispatcher is not started"),
    }
}

fn press_shortcut() {
    if !ACTIVE.swap(true, Ordering::AcqRel) {
        send(HotEvent::Press);
    }
}

fn release_shortcut() {
    if ACTIVE.swap(false, Ordering::AcqRel) {
        send(HotEvent::Release);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyRepeat {
    On { delay: Duration, interval: Duration },
    Off,
}

fn key_repeat_from(repeat: bool, delay_ms: u32, interval_ms: u32) -> KeyRepeat {
    if repeat && interval_ms > 0 {
        KeyRepeat::On {
            delay: Duration::from_millis(u64::from(delay_ms)),
            interval: Duration::from_millis(u64::from(interval_ms)),
        }
    } else {
        KeyRepeat::Off
    }
}

fn read_key_repeat() -> Option<KeyRepeat> {
    use gtk::gio::prelude::SettingsExt;
    let schema = gtk::gio::SettingsSchemaSource::default()?.lookup(KEYBOARD_SCHEMA, true)?;
    if !["repeat", "delay", "repeat-interval"]
        .iter()
        .all(|key| schema.has_key(key))
    {
        return None;
    }
    let settings = gtk::gio::Settings::new(KEYBOARD_SCHEMA);
    let repeat = settings.value("repeat").get::<bool>()?;
    let delay = settings.value("delay").get::<u32>()?;
    let interval = settings.value("repeat-interval").get::<u32>()?;
    Some(key_repeat_from(repeat, delay, interval))
}

fn repeats_global_shortcuts(current_desktop: Option<&str>) -> bool {
    current_desktop.is_some_and(|value| {
        value
            .split(':')
            .any(|part| part.trim().eq_ignore_ascii_case("gnome"))
    })
}

async fn key_repeat() -> Option<KeyRepeat> {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").ok();
    if !repeats_global_shortcuts(desktop.as_deref()) {
        return None;
    }
    match tokio::task::spawn_blocking(read_key_repeat).await {
        Ok(repeat) => repeat,
        Err(err) => {
            tracing::warn!("the keyboard repeat settings could not be read: {err}");
            None
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Beat {
    pressed_at: Duration,
    last_at: Duration,
    last: Instant,
    repeated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Activation {
    Ignore,
    Press,
    Repeat,
    PressAgain,
}

fn activation_action(
    active: bool,
    beat: Option<Beat>,
    repeat: Option<KeyRepeat>,
    timestamp: Duration,
    released_at: Option<Duration>,
) -> Activation {
    if released_at.is_some_and(|released| timestamp <= released) {
        return Activation::Ignore;
    }
    if !active {
        return Activation::Press;
    }
    match (repeat, beat) {
        (Some(KeyRepeat::Off), _) => Activation::PressAgain,
        (Some(KeyRepeat::On { interval, .. }), Some(beat))
            if timestamp.saturating_sub(beat.last_at) <= interval * 2 =>
        {
            Activation::Repeat
        }
        (Some(KeyRepeat::On { delay, interval }), Some(beat))
            if !beat.repeated
                && timestamp.saturating_sub(beat.pressed_at) < delay.saturating_sub(interval) =>
        {
            Activation::PressAgain
        }
        _ => Activation::Repeat,
    }
}

fn release_deadline(last: Instant, repeated: bool, repeat: KeyRepeat) -> Option<Instant> {
    match repeat {
        KeyRepeat::On { delay, interval } => {
            last.checked_add(if repeated { interval } else { delay } + RELEASE_GRACE)
        }
        KeyRepeat::Off => None,
    }
}

enum PortalEnd {
    Rebind,
    Wait,
    Lost(Duration),
}

async fn portal_loop(app: AppHandle, portal: GlobalShortcuts) -> bool {
    let mut portal = Arc::new(portal);
    let mut changes = epoch().subscribe();
    let mut delay = RETRY_MIN;
    let mut failed_sessions = 0u32;
    loop {
        changes.borrow_and_update();
        let (accelerator, description) = snapshot();
        let end = match accelerator {
            Some(accelerator) => {
                run_portal_session(&app, &portal, &accelerator, description, &mut changes).await
            }
            None => {
                crate::hotkey::set_hook_ready(&app, false);
                PortalEnd::Wait
            }
        };
        match end {
            PortalEnd::Rebind => delay = RETRY_MIN,
            PortalEnd::Wait => {
                delay = RETRY_MIN;
                if changes.changed().await.is_err() {
                    return false;
                }
            }
            PortalEnd::Lost(uptime) => {
                if uptime >= STABLE_SESSION {
                    delay = RETRY_MIN;
                    failed_sessions = 0;
                } else {
                    failed_sessions = failed_sessions.saturating_add(1);
                    if failed_sessions >= PORTAL_FAILURES_BEFORE_X11 && x11_session() {
                        tracing::warn!(
                            "the desktop shortcut service keeps failing; switching to X11 key grabs"
                        );
                        return true;
                    }
                }
                tracing::warn!("global shortcut session lost; retrying in {delay:?}");
                report_hotkey_failure(&app, "The desktop shortcut service stopped; retrying.");
                portal = reconnect_portal(&mut changes, &mut delay).await;
            }
        }
    }
}

async fn reconnect_portal(
    changes: &mut watch::Receiver<u64>,
    delay: &mut Duration,
) -> Arc<GlobalShortcuts> {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(*delay) => {}
            _ = changes.changed() => {}
        }
        *delay = (*delay * 2).min(RETRY_MAX);
        if let Err(err) = crate::linux_portal::perform_registration().await {
            tracing::warn!("desktop app registration failed on reconnect: {err}");
        }
        match within("the desktop shortcut service", GlobalShortcuts::new()).await {
            Ok(fresh) => return Arc::new(fresh),
            Err(err) => tracing::warn!("desktop shortcut service unavailable: {err}"),
        }
    }
}

async fn run_portal_session(
    app: &AppHandle,
    portal: &Arc<GlobalShortcuts>,
    accelerator: &Accelerator,
    description: &str,
    changes: &mut watch::Receiver<u64>,
) -> PortalEnd {
    if denied_shortcut().as_deref() == Some(accelerator.portal_id().as_str()) {
        tracing::info!(
            "the dictation shortcut was declined earlier; not asking again until it changes"
        );
        crate::hotkey::set_hook_ready(app, false);
        if !DENIAL_REPORTED.swap(true, Ordering::AcqRel) {
            report_denied(app);
        }
        return PortalEnd::Wait;
    }
    let created = spawn_create_session(Arc::clone(portal));
    let session = tokio::select! {
        result = tokio::time::timeout(PORTAL_CALL_TIMEOUT, created) => match result {
            Ok(Ok(Ok(session))) => session,
            Ok(Ok(Err(err))) => {
                crate::hotkey::set_hook_ready(app, false);
                report_hotkey_failure(
                    app,
                    &format!("The desktop shortcut service did not open a session: {err}"),
                );
                return PortalEnd::Lost(Duration::ZERO);
            }
            Ok(Err(_)) => {
                crate::hotkey::set_hook_ready(app, false);
                report_hotkey_failure(app, "The desktop shortcut service did not open a session.");
                return PortalEnd::Lost(Duration::ZERO);
            }
            Err(_) => {
                crate::hotkey::set_hook_ready(app, false);
                report_hotkey_failure(
                    app,
                    "The desktop shortcut service did not open a session in time.",
                );
                return PortalEnd::Lost(Duration::ZERO);
            }
        },
        _ = changes.changed() => return PortalEnd::Rebind,
    };
    let end = listen(app, portal, &session, accelerator, description, changes).await;
    match tokio::time::timeout(PORTAL_CALL_TIMEOUT, session.close()).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::warn!("closing the shortcut session failed: {err}"),
        Err(_) => tracing::warn!("closing the shortcut session did not answer in time"),
    }
    reset_active("shortcut session closed");
    crate::hotkey::set_hook_ready(app, false);
    end
}

fn spawn_create_session(
    portal: Arc<GlobalShortcuts>,
) -> oneshot::Receiver<Result<Session<GlobalShortcuts>, ashpd::Error>> {
    let (tx, rx) = oneshot::channel();
    tauri::async_runtime::spawn(async move {
        let Ok(created) = tokio::time::timeout(
            DETACHED_CREATE_TIMEOUT,
            portal.create_session(CreateSessionOptions::default()),
        )
        .await
        else {
            tracing::warn!("the desktop shortcut service never answered CreateSession");
            return;
        };
        match tx.send(created) {
            Ok(()) => {}
            Err(Ok(orphan)) => crate::linux_portal::close_abandoned(orphan).await,
            Err(Err(err)) => tracing::warn!("abandoned shortcut session request failed: {err}"),
        }
    });
    rx
}

async fn watch_portal_owner() -> Result<Pin<Box<NameOwnerChangedStream>>, String> {
    let connection = ashpd::zbus::Connection::session()
        .await
        .map_err(|err| format!("could not reach the session bus: {err}"))?;
    let bus = ashpd::zbus::fdo::DBusProxy::new(&connection)
        .await
        .map_err(|err| format!("could not query the session bus: {err}"))?;
    let stream = bus
        .receive_name_owner_changed_with_args(&[(0, PORTAL_BUS_NAME)])
        .await
        .map_err(|err| format!("could not watch the desktop shortcut service: {err}"))?;
    Ok(Box::pin(stream))
}

async fn listen(
    app: &AppHandle,
    portal: &GlobalShortcuts,
    session: &Session<GlobalShortcuts>,
    accelerator: &Accelerator,
    description: &str,
    changes: &mut watch::Receiver<u64>,
) -> PortalEnd {
    let shortcut_id = accelerator.portal_id();
    let trigger = accelerator.trigger();

    let mut activated = match within(
        "the desktop shortcut service did not report key presses",
        portal.receive_activated(),
    )
    .await
    {
        Ok(stream) => Box::pin(stream),
        Err(err) => {
            report_hotkey_failure(app, &err);
            return PortalEnd::Lost(Duration::ZERO);
        }
    };
    let mut deactivated = match within(
        "the desktop shortcut service did not report key releases",
        portal.receive_deactivated(),
    )
    .await
    {
        Ok(stream) => Box::pin(stream),
        Err(err) => {
            report_hotkey_failure(app, &err);
            return PortalEnd::Lost(Duration::ZERO);
        }
    };
    let mut closed = match within(
        "the desktop shortcut session could not be watched",
        session.receive_closed(),
    )
    .await
    {
        Ok(stream) => Box::pin(stream),
        Err(err) => {
            report_hotkey_failure(app, &err);
            return PortalEnd::Lost(Duration::ZERO);
        }
    };
    let session_path = match serialized_object_path(session) {
        Ok(path) => path,
        Err(err) => {
            report_hotkey_failure(app, &err);
            return PortalEnd::Lost(Duration::ZERO);
        }
    };
    let mut owner_changes = match within(
        "could not watch the desktop shortcut service",
        watch_portal_owner(),
    )
    .await
    {
        Ok(stream) => stream,
        Err(err) => {
            report_hotkey_failure(app, &err);
            return PortalEnd::Lost(Duration::ZERO);
        }
    };

    let mut repeat = key_repeat().await;
    tracing::info!("portal shortcut release follows the key repeat settings: {repeat:?}");
    let shortcut =
        NewShortcut::new(shortcut_id.as_str(), description).preferred_trigger(trigger.as_str());
    let bound = tokio::select! {
        result = portal.bind_shortcuts(
            session,
            std::slice::from_ref(&shortcut),
            None,
            BindShortcutsOptions::default(),
        ) => Some(result.and_then(|request| request.response())),
        _ = changes.changed() => None,
        _ = owner_changes.next() => return PortalEnd::Lost(Duration::ZERO),
    };
    let bound = match bound {
        Some(bound) => bound,
        None => return PortalEnd::Rebind,
    };
    match bound {
        Ok(response) => {
            match response
                .shortcuts()
                .iter()
                .find(|entry| entry.id() == shortcut_id.as_str())
            {
                Some(bound_shortcut) => tracing::info!(
                    "global shortcut bound through the desktop portal: {} ({})",
                    bound_shortcut.id(),
                    bound_shortcut.trigger_description()
                ),
                None => {
                    tracing::warn!(
                        "the desktop portal confirmed the bind without listing {shortcut_id}"
                    );
                    crate::hotkey::set_hook_ready(app, false);
                    report_hotkey_failure(
                        app,
                        "The desktop did not list the dictation shortcut after binding it.",
                    );
                    return PortalEnd::Lost(Duration::ZERO);
                }
            }
            crate::hotkey::set_hook_ready(app, true);
            DENIAL_REPORTED.store(false, Ordering::Release);
            forget_denied_shortcut();
        }
        Err(ashpd::Error::Response(ResponseError::Cancelled)) => {
            tracing::info!("the desktop shortcut dialog was declined: {shortcut_id}");
            remember_denied_shortcut(shortcut_id.as_str());
            DENIAL_REPORTED.store(true, Ordering::Release);
            report_denied(app);
            return PortalEnd::Wait;
        }
        Err(err) => {
            report_hotkey_failure(
                app,
                &format!("The desktop did not bind the dictation shortcut: {err}"),
            );
            return PortalEnd::Lost(Duration::ZERO);
        }
    }

    let bound_at = Instant::now();
    let mut beat: Option<Beat> = None;
    let mut released_at: Option<Duration> = None;
    loop {
        let release_at = match (beat, repeat) {
            (Some(beat), Some(repeat)) if ACTIVE.load(Ordering::Acquire) => {
                release_deadline(beat.last, beat.repeated, repeat)
            }
            _ => None,
        };
        let release_timer = async move {
            match release_at {
                Some(at) => tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            biased;
            _ = changes.changed() => return PortalEnd::Rebind,
            _ = owner_changes.next() => return PortalEnd::Lost(bound_at.elapsed()),
            item = activated.next() => match item {
                Some(signal) => {
                    if signal.shortcut_id() == shortcut_id.as_str()
                        && signal.session_handle().as_str() == session_path.as_str()
                    {
                        let timestamp = signal.timestamp();
                        let now = Instant::now();
                        match activation_action(
                            ACTIVE.load(Ordering::Acquire),
                            beat,
                            repeat,
                            timestamp,
                            released_at,
                        ) {
                            Activation::Ignore => {}
                            Activation::Repeat => {
                                if let Some(current) = beat.as_mut() {
                                    current.last = now;
                                    current.last_at = timestamp;
                                    current.repeated = true;
                                }
                            }
                            action => {
                                if action == Activation::PressAgain {
                                    release_shortcut();
                                }
                                press_shortcut();
                                beat = Some(Beat {
                                    pressed_at: timestamp,
                                    last_at: timestamp,
                                    last: now,
                                    repeated: false,
                                });
                                let fresh = key_repeat().await;
                                if fresh != repeat {
                                    tracing::info!(
                                        "portal shortcut release follows the key repeat settings: {fresh:?}"
                                    );
                                    repeat = fresh;
                                }
                            }
                        }
                    }
                }
                None => return PortalEnd::Lost(bound_at.elapsed()),
            },
            item = deactivated.next() => match item {
                Some(signal) => {
                    if signal.shortcut_id() == shortcut_id.as_str()
                        && signal.session_handle().as_str() == session_path.as_str()
                    {
                        released_at = Some(signal.timestamp());
                        beat = None;
                        release_shortcut();
                    }
                }
                None => return PortalEnd::Lost(bound_at.elapsed()),
            },
            _ = closed.next() => return PortalEnd::Lost(bound_at.elapsed()),
            _ = release_timer => {
                beat = None;
                tracing::info!("shortcut release inferred: the key repeat stopped");
                release_shortcut();
            }
        }
    }
}

fn serialized_object_path<T>(value: &T) -> Result<OwnedObjectPath, String>
where
    T: serde::Serialize + zvariant::DynamicType,
{
    let context = zvariant::serialized::Context::new_dbus(zvariant::Endian::Little, 0);
    let data = zvariant::to_bytes(context, value)
        .map_err(|err| format!("could not read the shortcut session handle: {err}"))?;
    let (path, _) = data
        .deserialize::<OwnedObjectPath>()
        .map_err(|err| format!("could not read the shortcut session handle: {err}"))?;
    Ok(path)
}

fn start_x11(app: AppHandle) {
    let thread_app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("synapse-hotkey-x11".to_string())
        .spawn(move || x11_thread(thread_app));
    if let Err(err) = spawned {
        tracing::error!("could not start thread synapse-hotkey-x11: {err}");
        crate::hotkey::set_hook_ready(&app, false);
        report_hotkey_failure(&app, "The X11 shortcut thread could not be started.");
    }
}

struct Grab {
    keycode: u8,
    modifiers: u16,
}

enum GrabError {
    Connection(String),
    Rejected(String),
}

fn x11_thread(app: AppHandle) {
    let mut changes = epoch().subscribe();
    let mut delay = RETRY_MIN;
    loop {
        let started = Instant::now();
        let reason = run_x11_session(&app, &mut changes);
        if started.elapsed() >= STABLE_SESSION {
            delay = RETRY_MIN;
        }
        reset_active("X11 session ended");
        crate::hotkey::set_hook_ready(&app, false);
        report_hotkey_failure(&app, &reason);
        tracing::warn!("X11 global shortcut session ended: {reason}; retrying in {delay:?}");
        sleep_unless_changed(&mut changes, delay);
        delay = (delay * 2).min(RETRY_MAX);
    }
}

fn sleep_unless_changed(changes: &mut watch::Receiver<u64>, delay: Duration) {
    let until = Instant::now() + delay;
    while Instant::now() < until && !matches!(changes.has_changed(), Ok(true)) {
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn run_x11_session(app: &AppHandle, changes: &mut watch::Receiver<u64>) -> String {
    let (conn, screen) = match x11rb::connect(None) {
        Ok(connection) => connection,
        Err(err) => return format!("could not connect to the X11 display: {err}"),
    };
    let setup = conn.setup();
    let Some(root) = setup.roots.get(screen).map(|root| root.root) else {
        return "the X11 display has no screen".to_string();
    };
    let (min_keycode, max_keycode) = (setup.min_keycode, setup.max_keycode);
    if let Err(reason) = enable_detectable_auto_repeat(&conn) {
        return reason;
    }
    changes.borrow_and_update();
    let mut grabs: Vec<Grab> = Vec::new();
    if let Err(reason) = apply_x11_binding(app, &conn, root, min_keycode, max_keycode, &mut grabs) {
        return reason;
    }
    loop {
        if matches!(changes.has_changed(), Ok(true)) {
            changes.borrow_and_update();
            if let Err(reason) = release_grabs(&conn, root, &mut grabs) {
                return reason;
            }
            reset_active("shortcut changed");
            if let Err(reason) =
                apply_x11_binding(app, &conn, root, min_keycode, max_keycode, &mut grabs)
            {
                return reason;
            }
        }
        match conn.poll_for_event() {
            Ok(Some(event)) => handle_x11_event(event, &grabs),
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(err) => return format!("the X11 connection failed: {err}"),
        }
    }
}

fn enable_detectable_auto_repeat(conn: &RustConnection) -> Result<(), String> {
    let supported = conn
        .xkb_use_extension(1, 0)
        .map_err(|err| format!("could not query the XKB extension: {err}"))?
        .reply()
        .map_err(|err| format!("could not query the XKB extension: {err}"))?
        .supported;
    if !supported {
        return Err("the X11 server does not provide the XKB extension".to_string());
    }
    conn.xkb_per_client_flags(
        xkb::ID::USE_CORE_KBD.into(),
        xkb::PerClientFlag::DETECTABLE_AUTO_REPEAT,
        xkb::PerClientFlag::DETECTABLE_AUTO_REPEAT,
        xkb::BoolCtrl::default(),
        xkb::BoolCtrl::default(),
        xkb::BoolCtrl::default(),
    )
    .map_err(|err| format!("could not enable detectable key repeat: {err}"))?
    .reply()
    .map_err(|err| format!("could not enable detectable key repeat: {err}"))?;
    Ok(())
}

fn apply_x11_binding(
    app: &AppHandle,
    conn: &RustConnection,
    root: u32,
    min_keycode: u8,
    max_keycode: u8,
    grabs: &mut Vec<Grab>,
) -> Result<(), String> {
    let (accelerator, _) = snapshot();
    let Some(accelerator) = accelerator else {
        crate::hotkey::set_hook_ready(app, false);
        return Ok(());
    };
    match grab_accelerator(conn, root, min_keycode, max_keycode, &accelerator) {
        Ok(granted) => {
            tracing::info!("global shortcut grabbed on X11: {}", accelerator.trigger());
            *grabs = granted;
            crate::hotkey::set_hook_ready(app, true);
            Ok(())
        }
        Err(GrabError::Rejected(reason) | GrabError::Connection(reason)) => Err(reason),
    }
}

fn keycodes_for_keysym(
    conn: &RustConnection,
    min_keycode: u8,
    max_keycode: u8,
    keysym: u32,
) -> Result<Vec<u8>, GrabError> {
    let count = max_keycode.saturating_sub(min_keycode).saturating_add(1);
    let mapping = conn
        .get_keyboard_mapping(min_keycode, count)
        .map_err(|err| GrabError::Connection(format!("could not read the keyboard map: {err}")))?
        .reply()
        .map_err(|err| GrabError::Connection(format!("could not read the keyboard map: {err}")))?;
    let per_keycode = usize::from(mapping.keysyms_per_keycode);
    if per_keycode == 0 {
        return Err(GrabError::Connection(
            "the X11 keyboard map is empty".to_string(),
        ));
    }
    let mut keycodes = Vec::new();
    for (index, symbols) in mapping.keysyms.chunks(per_keycode).enumerate() {
        if symbols.contains(&keysym) {
            let code = u8::try_from(usize::from(min_keycode) + index)
                .map_err(|err| GrabError::Connection(format!("invalid X11 keycode: {err}")))?;
            keycodes.push(code);
        }
    }
    Ok(keycodes)
}

fn grab_accelerator(
    conn: &RustConnection,
    root: u32,
    min_keycode: u8,
    max_keycode: u8,
    accelerator: &Accelerator,
) -> Result<Vec<Grab>, GrabError> {
    let keycodes = keycodes_for_keysym(conn, min_keycode, max_keycode, accelerator.keysym)?;
    if keycodes.is_empty() {
        return Err(GrabError::Rejected(format!(
            "the key {} is not on the keyboard layout",
            accelerator.key_name
        )));
    }
    let base = accelerator.x11_modifiers();
    let lock_states = [
        0,
        u16::from(ModMask::LOCK),
        u16::from(ModMask::M2),
        u16::from(ModMask::LOCK) | u16::from(ModMask::M2),
    ];
    let mut grabs: Vec<Grab> = Vec::new();
    for keycode in keycodes {
        for lock in lock_states {
            let modifiers = base | lock;
            let cookie = conn
                .grab_key(
                    false,
                    root,
                    ModMask::from(modifiers),
                    keycode,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )
                .map_err(|err| {
                    GrabError::Connection(format!("could not request the shortcut grab: {err}"))
                })?;
            match cookie.check() {
                Ok(()) => grabs.push(Grab { keycode, modifiers }),
                Err(ReplyError::ConnectionError(err)) => {
                    return Err(GrabError::Connection(format!(
                        "the X11 connection failed while grabbing the shortcut: {err}"
                    )));
                }
                Err(ReplyError::X11Error(err)) => {
                    tracing::warn!("X11 refused the shortcut grab: {err:?}");
                    if let Err(reason) = release_grabs(conn, root, &mut grabs) {
                        tracing::warn!("could not roll back partial shortcut grabs: {reason}");
                    }
                    return Err(GrabError::Rejected(format!(
                        "the shortcut {} is already used by another program",
                        accelerator.trigger()
                    )));
                }
            }
        }
    }
    Ok(grabs)
}

fn release_grabs(conn: &RustConnection, root: u32, grabs: &mut Vec<Grab>) -> Result<(), String> {
    for grab in grabs.drain(..) {
        conn.ungrab_key(grab.keycode, root, ModMask::from(grab.modifiers))
            .map_err(|err| format!("could not release the shortcut: {err}"))?;
    }
    conn.flush()
        .map_err(|err| format!("the X11 connection failed: {err}"))
}

fn handle_x11_event(event: Event, grabs: &[Grab]) {
    match event {
        Event::KeyPress(press) if grabs.iter().any(|grab| grab.keycode == press.detail) => {
            press_shortcut();
        }
        Event::KeyRelease(release) if grabs.iter().any(|grab| grab.keycode == release.detail) => {
            release_shortcut();
        }
        Event::Error(err) => tracing::warn!("X11 request failed: {err:?}"),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{
        activation_action, clear_denied, key_repeat_from, parse_accelerator, read_denied,
        release_deadline, repeats_global_shortcuts, serialized_object_path, write_denied,
        Accelerator, Activation, Beat, KeyRepeat, RELEASE_GRACE,
    };
    use std::time::{Duration, Instant};
    use ashpd::zvariant::OwnedObjectPath;
    use std::path::PathBuf;

    fn parsed(text: &str) -> Accelerator {
        parse_accelerator(text).expect("accelerator should parse")
    }

    #[test]
    fn declined_shortcut_is_remembered_until_cleared() {
        let root = std::env::temp_dir().join(format!(
            "synapse-denied-shortcut-test-{}",
            std::process::id()
        ));
        let path: PathBuf = root.join("portal").join("denied_shortcut");
        clear_denied(&path);
        assert_eq!(read_denied(&path), None);
        write_denied(&path, "synapse-ctrl-shift-space");
        assert_eq!(
            read_denied(&path).as_deref(),
            Some("synapse-ctrl-shift-space")
        );
        write_denied(&path, "synapse-ctrl-space");
        assert_eq!(read_denied(&path).as_deref(), Some("synapse-ctrl-space"));
        clear_denied(&path);
        assert_eq!(read_denied(&path), None);
        clear_denied(&path);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn object_path_round_trips_through_dbus_serialization() {
        let path =
            OwnedObjectPath::try_from("/org/freedesktop/portal/desktop/session/1_42/synapse_7")
                .expect("valid object path");
        let read_back = serialized_object_path(&path).expect("serialize object path");
        assert_eq!(read_back.as_str(), path.as_str());
    }

    #[test]
    fn parses_default_accelerator() {
        let accelerator = parsed("Ctrl+Shift+Space");
        assert_eq!(accelerator.trigger(), "CTRL+SHIFT+space");
        assert_eq!(accelerator.portal_id(), "synapse-ctrl-shift-space");
        assert_eq!(accelerator.keysym, 0x20);
        assert_eq!(accelerator.x11_modifiers(), (1 << 0) | (1 << 2));
    }

    #[test]
    fn orders_modifiers_for_portal_trigger() {
        assert_eq!(parsed("Win+Shift+Enter").trigger(), "SHIFT+LOGO+Return");
        assert_eq!(parsed("Super+a").trigger(), "LOGO+a");
        assert_eq!(parsed("CommandOrControl+Alt+F12").trigger(), "CTRL+ALT+F12");
        assert_eq!(parsed("Option+Cmd+F12").trigger(), "ALT+LOGO+F12");
    }

    #[test]
    fn maps_letters_digits_and_function_keys_to_keysyms() {
        assert_eq!(parsed("Alt+A").keysym, 0x61);
        assert_eq!(parsed("Alt+1").keysym, 0x31);
        assert_eq!(parsed("Alt+F1").keysym, 0xffbe);
        assert_eq!(parsed("Alt+F24").keysym, 0xffd5);
        assert_eq!(parsed("Alt+F12").keysym, 0xffc9);
    }

    #[test]
    fn maps_named_and_punctuation_keys() {
        assert_eq!(parsed("Ctrl+`").trigger(), "CTRL+grave");
        assert_eq!(parsed("Ctrl+`").keysym, 0x60);
        assert_eq!(parsed("Ctrl+PageUp").trigger(), "CTRL+Prior");
        assert_eq!(parsed("Ctrl+PageDown").keysym, 0xff56);
        assert_eq!(parsed("Ctrl+esc").keysym, 0xff1b);
        assert_eq!(parsed("Ctrl+\\").trigger(), "CTRL+backslash");
    }

    #[test]
    fn rejects_unsupported_accelerators() {
        for text in [
            "",
            "Ctrl",
            "Ctrl+Shift",
            "Ctrl+A+B",
            "Ctrl+",
            "Ctrl++",
            "MouseMiddle",
            "Ctrl+Foo",
            "F25",
            "F0",
        ] {
            assert!(
                parse_accelerator(text).is_none(),
                "{text} should be rejected"
            );
        }
    }

    #[test]
    fn key_repeat_settings_map_to_release_timing() {
        assert_eq!(
            key_repeat_from(true, 500, 30),
            KeyRepeat::On {
                delay: Duration::from_millis(500),
                interval: Duration::from_millis(30)
            }
        );
        assert_eq!(key_repeat_from(false, 500, 30), KeyRepeat::Off);
        assert_eq!(key_repeat_from(true, 500, 0), KeyRepeat::Off);
    }

    #[test]
    fn release_is_inferred_after_the_repeat_stops() {
        let last = Instant::now();
        let repeat = key_repeat_from(true, 500, 30);
        assert_eq!(
            release_deadline(last, false, repeat),
            Some(last + Duration::from_millis(500) + RELEASE_GRACE)
        );
        assert_eq!(
            release_deadline(last, true, repeat),
            Some(last + Duration::from_millis(30) + RELEASE_GRACE)
        );
        assert_eq!(release_deadline(last, true, KeyRepeat::Off), None);
    }

    #[test]
    fn only_gnome_sessions_use_the_repeat_heartbeat() {
        assert!(repeats_global_shortcuts(Some("ubuntu:GNOME")));
        assert!(repeats_global_shortcuts(Some("GNOME")));
        assert!(repeats_global_shortcuts(Some("pop:GNOME")));
        assert!(!repeats_global_shortcuts(Some("KDE")));
        assert!(!repeats_global_shortcuts(Some("X-Cinnamon")));
        assert!(!repeats_global_shortcuts(None));
    }

    #[test]
    fn activations_map_to_press_repeat_or_new_press() {
        let on = Some(key_repeat_from(true, 500, 30));
        let first = Beat {
            pressed_at: Duration::from_millis(1_000),
            last_at: Duration::from_millis(1_000),
            last: Instant::now(),
            repeated: false,
        };
        let ms = Duration::from_millis;
        assert_eq!(activation_action(false, None, on, ms(1_000), None), Activation::Press);
        assert_eq!(activation_action(true, Some(first), on, ms(1_500), None), Activation::Repeat);
        assert_eq!(activation_action(true, Some(first), on, ms(1_100), None), Activation::PressAgain);
        assert_eq!(activation_action(true, Some(first), on, ms(1_300), None), Activation::PressAgain);
        assert_eq!(activation_action(true, Some(first), on, ms(1_469), None), Activation::PressAgain);
        assert_eq!(activation_action(true, Some(first), on, ms(1_501), None), Activation::Repeat);
        assert_eq!(activation_action(true, Some(first), on, ms(1_030), None), Activation::Repeat);
        assert_eq!(activation_action(true, Some(first), on, ms(1_060), None), Activation::Repeat);
        assert_eq!(activation_action(true, Some(first), on, ms(1_061), None), Activation::PressAgain);
        let repeating = Beat { repeated: true, ..first };
        assert_eq!(activation_action(true, Some(repeating), on, ms(1_100), None), Activation::Repeat);
        assert_eq!(
            activation_action(true, Some(first), Some(KeyRepeat::Off), ms(3_000), None),
            Activation::PressAgain
        );
        assert_eq!(activation_action(true, Some(first), None, ms(1_100), None), Activation::Repeat);
        assert_eq!(
            activation_action(false, None, on, ms(2_000), Some(ms(2_000))),
            Activation::Ignore
        );
        assert_eq!(
            activation_action(false, None, on, ms(2_001), Some(ms(2_000))),
            Activation::Press
        );
    }
}
