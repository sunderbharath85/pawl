//! Pawl desktop shell (Tauri 2): pet window, panel window, tray, and
//! the small amount of native glue the React UI needs (hub credentials, health,
//! spawning `pawl serve`, click-through hit-testing, moving the pet).

mod hub;
mod pet_window;
mod tray;

use hub::{HubCredentials, HubHealth, HubProcess, StartHubResult};
use pet_window::{PanelFocusState, PetWindowState, PANEL_LABEL, PET_LABEL};
use std::time::Duration;
use tauri::{Emitter, Manager, WindowEvent};

/// `{url, wsUrl, token, tokenSource, dataDir}` from env overrides or the shared data dir.
#[tauri::command]
async fn hub_credentials() -> Result<HubCredentials, String> {
    tauri::async_runtime::spawn_blocking(hub::credentials)
        .await
        .map_err(|e| e.to_string())
}

/// `GET /health` against the configured hub URL (done natively: the hub sends no CORS headers).
#[tauri::command]
async fn hub_health() -> Result<HubHealth, String> {
    tauri::async_runtime::spawn_blocking(|| hub::hub_health_now(Duration::from_millis(1500)))
        .await
        .map_err(|e| e.to_string())
}

/// Spawn `pawl serve` (PATH, next to the app, or `PAWL_HUB_BIN`) unless already healthy.
#[tauri::command]
async fn start_hub(app: tauri::AppHandle) -> Result<StartHubResult, String> {
    let handle = app.clone();
    let res = tauri::async_runtime::spawn_blocking(move || hub::start_hub_blocking(&handle.state::<HubProcess>()))
        .await
        .map_err(|e| e.to_string())??;
    let _ = app.emit("hub-started", res.clone());
    Ok(res)
}

/// Path of the `pawl` binary we would launch, if any.
#[tauri::command]
fn locate_hub() -> Option<String> {
    hub::find_hub_binary().map(|p| p.display().to_string())
}

/// This machine's main network address (suggested for the peer listener), if it has one.
#[tauri::command]
async fn lan_address() -> Option<String> {
    tauri::async_runtime::spawn_blocking(hub::lan_address).await.ok().flatten()
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    quit(&app);
}

/// "Quit Pawl" (tray, the pet's menu, Settings): stop the hub too if this app started it, then exit.
pub fn quit<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(pid) = app.state::<HubProcess>().stop_owned() {
        eprintln!("[pawl] stopped the hub this app started (pid {pid})");
    }
    app.exit(0);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must be registered first. A second launch (also what clicking an OS notification can do) only wakes
        // the pet's bubble: the panel opens when the user asks for it (right-click the pet -> Open hub, or the tray).
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            pet_window::pet_attention(app);
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(PetWindowState::default())
        .manage(PanelFocusState::default())
        .manage(HubProcess::default())
        .invoke_handler(tauri::generate_handler![
            hub_credentials,
            hub_health,
            start_hub,
            locate_hub,
            lan_address,
            quit_app,
            pet_window::set_hit_rects,
            pet_window::set_capture_all,
            pet_window::set_pet_frame,
            pet_window::pet_start_drag,
            pet_window::pet_context_menu,
            pet_window::reset_pet_position,
            pet_window::set_pets_paused,
            pet_window::get_pets_paused,
            pet_window::open_panel,
            pet_window::take_panel_focus,
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let handle = app.handle();
            pet_window::create(handle)?;
            tray::create(handle)?;
            Ok(())
        })
        .on_window_event(|window, event| match (window.label(), event) {
            // Closing the panel hides it (the app lives in the tray); Open hub shows it again, or recreates it.
            (PANEL_LABEL, WindowEvent::CloseRequested { api, .. }) => {
                api.prevent_close();
                let _ = window.hide();
            }
            // The user dragged the pet (or the OS moved it): follow it, save once it is dropped.
            (PET_LABEL, WindowEvent::Moved(pos)) => pet_window::on_moved(window.app_handle(), *pos),
            _ => {}
        })
        .run(tauri::generate_context!())
        .expect("error while running Pawl desktop");
}
