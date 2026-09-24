use crate::pipeline;
use crate::state::SharedState;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let toggle = MenuItemBuilder::with_id("toggle", "Start / stop dictation").build(app)?;
    let widget = MenuItemBuilder::with_id("widget", "Show widget").build(app)?;
    let settings = MenuItemBuilder::with_id("settings", "Settings").build(app)?;
    let history = MenuItemBuilder::with_id("history", "History and statistics").build(app)?;
    let quit = MenuItemBuilder::with_id("quit", "Quit Synapse").build(app)?;

    let menu = MenuBuilder::new(app)
        .items(&[&toggle, &widget, &settings, &history])
        .separator()
        .item(&quit)
        .build()?;

    let mut builder = TrayIconBuilder::with_id("main")
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
    Ok(())
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
