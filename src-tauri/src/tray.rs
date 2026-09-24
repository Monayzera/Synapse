use crate::pipeline;
use crate::state::SharedState;
use tauri::menu::{Menu, MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, Wry};

const TRAY_ID: &str = "main";

struct Labels {
    toggle: &'static str,
    widget: &'static str,
    settings: &'static str,
    history: &'static str,
    quit: &'static str,
}

const PT: Labels = Labels {
    toggle: "Iniciar / parar ditado",
    widget: "Mostrar widget",
    settings: "Ajustes",
    history: "Histórico e estatísticas",
    quit: "Sair do Synapse",
};

const EN: Labels = Labels {
    toggle: "Start / stop dictation",
    widget: "Show widget",
    settings: "Settings",
    history: "History and statistics",
    quit: "Quit Synapse",
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

fn build_menu(app: &AppHandle, ui_language: &str) -> tauri::Result<Menu<Wry>> {
    let text = labels(ui_language);
    let toggle = MenuItemBuilder::with_id("toggle", text.toggle).build(app)?;
    let widget = MenuItemBuilder::with_id("widget", text.widget).build(app)?;
    let settings = MenuItemBuilder::with_id("settings", text.settings).build(app)?;
    let history = MenuItemBuilder::with_id("history", text.history).build(app)?;
    let quit = MenuItemBuilder::with_id("quit", text.quit).build(app)?;

    MenuBuilder::new(app)
        .items(&[&toggle, &widget, &settings, &history])
        .separator()
        .item(&quit)
        .build()
}

pub fn build(app: &AppHandle, ui_language: &str) -> tauri::Result<()> {
    let menu = build_menu(app, ui_language)?;

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
        Ok(menu) => {
            if let Err(err) = tray.set_menu(Some(menu)) {
                tracing::warn!("tray menu refresh failed: {err}");
            } else {
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
        "quit" => {
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
