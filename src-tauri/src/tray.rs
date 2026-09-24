use crate::pipeline;
use crate::state::SharedState;
use crate::updater::{self, TrayItem};
use parking_lot::Mutex;
use tauri::menu::{Menu, MenuBuilder, MenuItem, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

const TRAY_ID: &str = "main";

static UPDATE_ITEM: Mutex<Option<MenuItem<Wry>>> = Mutex::new(None);

struct Labels {
    toggle: &'static str,
    widget: &'static str,
    settings: &'static str,
    history: &'static str,
    quit: &'static str,
    update_check: &'static str,
    update_checking: &'static str,
    update_downloading: &'static str,
    update_install: &'static str,
    update_installing: &'static str,
}

const PT: Labels = Labels {
    toggle: "Iniciar / parar ditado",
    widget: "Mostrar widget",
    settings: "Ajustes",
    history: "Histórico e estatísticas",
    quit: "Sair do Synapse",
    update_check: "Verificar atualizações",
    update_checking: "Verificando atualizações…",
    update_downloading: "Baixando atualização… {pct}%",
    update_install: "Instalar atualização v{v}",
    update_installing: "Instalando atualização…",
};

const EN: Labels = Labels {
    toggle: "Start / stop dictation",
    widget: "Show widget",
    settings: "Settings",
    history: "History and statistics",
    quit: "Quit Synapse",
    update_check: "Check for updates",
    update_checking: "Checking for updates…",
    update_downloading: "Downloading update… {pct}%",
    update_install: "Install update v{v}",
    update_installing: "Installing update…",
};

pub fn resolve_language(ui_language: &str) -> &'static str {
    match ui_language {
        "pt" => "pt",
        "en" => "en",
        _ => os_language(),
    }
}

#[cfg(windows)]
fn os_language() -> &'static str {
    const LANG_PORTUGUESE: u16 = 0x16;
    let langid = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    if langid & 0x3ff == LANG_PORTUGUESE {
        "pt"
    } else {
        "en"
    }
}

#[cfg(target_os = "macos")]
fn os_language() -> &'static str {
    let output = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLanguages"])
        .output();
    let text = match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).to_string(),
        Ok(_) | Err(_) => std::env::var("LANG").unwrap_or_default(),
    };
    let first = text
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
        .find(|token| !token.is_empty())
        .unwrap_or("");
    if first.to_ascii_lowercase().starts_with("pt") {
        "pt"
    } else {
        "en"
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn os_language() -> &'static str {
    let lang = std::env::var("LANG").unwrap_or_default();
    if lang.to_ascii_lowercase().starts_with("pt") {
        "pt"
    } else {
        "en"
    }
}

fn labels(ui_language: &str) -> &'static Labels {
    if resolve_language(ui_language) == "pt" {
        &PT
    } else {
        &EN
    }
}

fn update_text(ui_language: &str, item: &TrayItem) -> (String, bool) {
    let text = labels(ui_language);
    match item {
        TrayItem::Check => (text.update_check.to_string(), true),
        TrayItem::Checking => (text.update_checking.to_string(), false),
        TrayItem::Downloading(pct) => (
            text.update_downloading.replace("{pct}", &pct.to_string()),
            false,
        ),
        TrayItem::Install(version) => (text.update_install.replace("{v}", version), true),
        TrayItem::Installing => (text.update_installing.to_string(), false),
    }
}

fn build_menu(
    app: &AppHandle,
    ui_language: &str,
) -> tauri::Result<(Menu<Wry>, Option<MenuItem<Wry>>)> {
    let text = labels(ui_language);
    let toggle = MenuItemBuilder::with_id("toggle", text.toggle).build(app)?;
    let widget = MenuItemBuilder::with_id("widget", text.widget).build(app)?;
    let settings = MenuItemBuilder::with_id("settings", text.settings).build(app)?;
    let history = MenuItemBuilder::with_id("history", text.history).build(app)?;
    let quit = MenuItemBuilder::with_id("quit", text.quit).build(app)?;
    let update = match updater::current_tray_item(app) {
        Some(item) => {
            let (label, enabled) = update_text(ui_language, &item);
            Some(
                MenuItemBuilder::with_id("update", label)
                    .enabled(enabled)
                    .build(app)?,
            )
        }
        None => None,
    };

    let mut menu = MenuBuilder::new(app)
        .items(&[&toggle, &widget, &settings, &history])
        .separator();
    if let Some(update) = &update {
        menu = menu.item(update);
    }
    let menu = menu.item(&quit).build()?;
    Ok((menu, update))
}

fn adopt_update_item(app: &AppHandle, item: Option<MenuItem<Wry>>) {
    *UPDATE_ITEM.lock() = item;
    updater::refresh_tray(app);
}

pub fn set_update_item(item: &TrayItem, language: &str) {
    let Some(entry) = UPDATE_ITEM.lock().clone() else {
        return;
    };
    let (label, enabled) = update_text(language, item);
    if let Err(err) = entry.set_text(label) {
        tracing::debug!("tray update item text not set: {err}");
    }
    if let Err(err) = entry.set_enabled(enabled) {
        tracing::debug!("tray update item state not set: {err}");
    }
}

pub fn set_visible(app: &AppHandle, visible: bool) {
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        if let Err(err) = tray.set_visible(visible) {
            tracing::warn!("tray visibility change failed: {err}");
        }
    }
}

pub fn build(app: &AppHandle, ui_language: &str) -> tauri::Result<()> {
    let (menu, update) = build_menu(app, ui_language)?;

    let mut builder = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Synapse")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| handle_menu(app, event.id().as_ref()))
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show(tray.app_handle(), "widget");
            }
        });

    builder = builder
        .icon(tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?)
        .icon_as_template(false);

    builder.build(app)?;
    adopt_update_item(app, update);
    tracing::info!("tray ready (language {})", resolve_language(ui_language));
    Ok(())
}

pub fn refresh(app: &AppHandle, ui_language: &str) {
    let tray = match app.tray_by_id(TRAY_ID) {
        Some(tray) => tray,
        None => {
            tracing::debug!("tray not available; menu language not refreshed");
            return;
        }
    };
    match build_menu(app, ui_language) {
        Ok((menu, update)) => {
            if let Err(err) = tray.set_menu(Some(menu)) {
                tracing::warn!("tray menu refresh failed: {err}");
            } else {
                adopt_update_item(app, update);
                tracing::info!("tray language set to {}", resolve_language(ui_language));
            }
        }
        Err(err) => tracing::warn!("tray menu rebuild failed: {err}"),
    }
}

fn handle_menu(app: &AppHandle, id: &str) {
    match id {
        "toggle" => {
            if let Some(state) = app.try_state::<SharedState>() {
                pipeline::toggle_recording(app.clone(), state.inner().clone());
            }
        }
        "widget" => show(app, "widget"),
        "settings" => show(app, "settings"),
        "history" => show(app, "history"),
        "update" => updater::tray_clicked(app),
        "quit" => {
            if updater::quit_with_update(app) {
                return;
            }
            if let Some(state) = app.try_state::<SharedState>() {
                state.stop_sidecar();
            }
            app.exit(0);
        }
        _ => {}
    }
}

fn show(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}
