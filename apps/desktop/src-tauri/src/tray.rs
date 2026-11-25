//! Tray icon + menu: Open hub, Pause pets, Reset pet position, Start hub, Quit. Menu handlers in Tauri are
//! app-wide, so the pet's right-click menu (Open hub, Reset position, Settings, Quit) is handled here too.
//! The panel only opens when the user asks for it (Open hub / Settings); a left click on the tray icon opens the
//! pet's bubble instead.

use crate::hub::{self, HubProcess};
use crate::pet_window::{self, PetWindowState};
use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Runtime};

const ID_OPEN_HUB: &str = "open_hub";
const ID_PAUSE: &str = "pause_pets";
const ID_RESET_POSITION: &str = "reset_pet_position";
const ID_START_HUB: &str = "start_hub";
const ID_QUIT: &str = "quit";

/// Kept in managed state so the check mark follows pause changes made from the UI.
pub struct TrayHandles<R: Runtime> {
    pause: CheckMenuItem<R>,
}

/// Every menu event of the app (tray menu and the pet's right-click menu).
fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    match event.id().as_ref() {
        ID_OPEN_HUB | pet_window::MENU_OPEN_HUB => pet_window::show_panel(app, None),
        pet_window::MENU_SETTINGS => pet_window::show_panel(app, Some(serde_json::json!({ "view": "settings" }))),
        pet_window::MENU_RENAME => pet_window::pet_rename(app),
        ID_PAUSE => {
            let paused = !app.state::<PetWindowState>().is_paused();
            pet_window::set_paused(app, paused);
        }
        ID_RESET_POSITION | pet_window::MENU_RESET => pet_window::reset_position(app),
        ID_START_HUB => spawn_start_hub(app.clone()),
        ID_QUIT | pet_window::MENU_QUIT => crate::quit(app),
        _ => {}
    }
}

pub fn create<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, ID_OPEN_HUB, "Open hub", true, None::<&str>)?;
    let pause = CheckMenuItem::with_id(app, ID_PAUSE, "Pause pets", true, false, None::<&str>)?;
    let reset = MenuItem::with_id(app, ID_RESET_POSITION, "Reset pet position", true, None::<&str>)?;
    let start_hub = MenuItem::with_id(app, ID_START_HUB, "Start hub", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, ID_QUIT, "Quit Pawl", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &pause, &reset, &start_hub, &sep, &quit])?;

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("Pawl")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(handle_menu_event)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                pet_window::pet_attention(tray.app_handle());
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
