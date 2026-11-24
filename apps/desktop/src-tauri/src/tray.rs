//! Tray icon + menu: Show panel, Pause pets, Start hub, Quit.

use crate::hub::{self, HubProcess};
use crate::pet_window::{self, PetWindowState};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime};

const ID_SHOW: &str = "show_panel";
const ID_PAUSE: &str = "pause_pets";
const ID_START_HUB: &str = "start_hub";
const ID_QUIT: &str = "quit";

/// Kept in managed state so the check mark follows pause changes made from the UI.
pub struct TrayHandles<R: Runtime> {
    pause: CheckMenuItem<R>,
}

pub fn create<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, ID_SHOW, "Show panel", true, None::<&str>)?;
    let pause = CheckMenuItem::with_id(app, ID_PAUSE, "Pause pets", true, false, None::<&str>)?;
    let start_hub = MenuItem::with_id(app, ID_START_HUB, "Start hub", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, ID_QUIT, "Quit AI Pet Hub", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &pause, &start_hub, &sep, &quit])?;

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("AI Pet Hub")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            ID_SHOW => pet_window::show_panel(app, None),
            ID_PAUSE => {
                let paused = !app.state::<PetWindowState>().is_paused();
                pet_window::set_paused(app, paused);
            }
            ID_START_HUB => spawn_start_hub(app.clone()),
            ID_QUIT => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                pet_window::show_panel(tray.app_handle(), None);
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app)?;
    app.manage(TrayHandles { pause });
    Ok(())
}

pub fn sync_paused<R: Runtime>(app: &AppHandle<R>, paused: bool) {
    if let Some(handles) = app.try_state::<TrayHandles<R>>() {
        let _ = handles.pause.set_checked(paused);
    }
}

/// Start the hub off the main thread and tell every window how it went.
pub fn spawn_start_hub<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let handle = app.clone();
        let result =
            tauri::async_runtime::spawn_blocking(move || hub::start_hub_blocking(&handle.state::<HubProcess>())).await;
        match result {
            Ok(Ok(res)) => {
                let _ = app.emit("hub-started", res);
            }
            Ok(Err(err)) => {
                let _ = app.emit("hub-start-failed", err);
            }
            Err(join_err) => {
                let _ = app.emit("hub-start-failed", join_err.to_string());
            }
        }
    });
}
