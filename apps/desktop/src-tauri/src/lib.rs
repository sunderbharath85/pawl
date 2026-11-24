//! Krypto desktop shell (Tauri 2): pet strip window, panel window, tray, and
//! the small amount of native glue the React UI needs (hub credentials, health,
//! spawning `krypto serve`, click-through hit-testing).

mod hub;
mod pet_window;
mod tray;

use hub::{HubCredentials, HubHealth, HubProcess, StartHubResult};
use pet_window::{PetWindowState, PANEL_LABEL};
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
    tauri::async_runtime::spawn_blocking(|| hub::probe_health(&hub::hub_http_url(), Duration::from_millis(1500)))
        .await
        .map_err(|e| e.to_string())
}

/// Spawn `krypto serve` (PATH, next to the app, or `KRYPTO_HUB_BIN`) unless already healthy.
#[tauri::command]
async fn start_hub(app: tauri::AppHandle) -> Result<StartHubResult, String> {
    let handle = app.clone();
    let res = tauri::async_runtime::spawn_blocking(move || hub::start_hub_blocking(&handle.state::<HubProcess>()))
        .await
        .map_err(|e| e.to_string())??;
    let _ = app.emit("hub-started", res.clone());
    Ok(res)
}

/// Path of the `krypto` binary we would launch, if any.
#[tauri::command]
fn locate_hub() -> Option<String> {
    hub::find_hub_binary().map(|p| p.display().to_string())
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    app.exit(0);
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // Must be registered first: a second launch just surfaces the panel.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            pet_window::show_panel(app, None);
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(PetWindowState::default())
        .manage(HubProcess::default())
        .invoke_handler(tauri::generate_handler![
            hub_credentials,
            hub_health,
            start_hub,
            locate_hub,
            quit_app,
            pet_window::set_hit_rects,
            pet_window::set_capture_all,
            pet_window::set_pets_paused,
            pet_window::get_pets_paused,
            pet_window::open_panel,
        ])
        .setup(|app| {
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let handle = app.handle();
            pet_window::create(handle)?;
            tray::create(handle)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the panel hides it; the app lives in the tray.
            if window.label() == PANEL_LABEL {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Krypto desktop");
}
