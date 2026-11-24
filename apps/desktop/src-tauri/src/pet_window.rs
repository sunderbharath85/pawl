//! The "pet" strip: a transparent, undecorated, always-on-top window that spans the
//! monitor's work area just above the taskbar/dock. It is click-through by default;
//! a background thread polls the global cursor (~50 ms) and turns cursor events back
//! on only while the pointer is over a hit-rect the webview reported (pets, bubbles).

use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Runtime, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

pub const PET_LABEL: &str = "pet";
pub const PANEL_LABEL: &str = "panel";
/// Strip height in logical px.
pub const STRIP_HEIGHT: f64 = 220.0;
const POLL_EVERY: Duration = Duration::from_millis(50);
/// Re-check monitor geometry every N polls (~2 s) to follow taskbar/resolution changes.
const REANCHOR_EVERY: u64 = 40;
/// Extra slack (logical px) around each hit-rect so the pointer doesn't "fall through" at edges.
const HIT_PADDING: f64 = 6.0;

/// A hit-rect in CSS px relative to the pet webview's viewport.
#[derive(Debug, Clone, Copy, Deserialize)]
pub struct HitRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl HitRect {
    fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x - HIT_PADDING
            && x <= self.x + self.w + HIT_PADDING
            && y >= self.y - HIT_PADDING
            && y <= self.y + self.h + HIT_PADDING
    }
}

#[derive(Default)]
pub struct PetWindowState {
    rects: Mutex<Vec<HitRect>>,
    paused: AtomicBool,
    /// Set by the webview while it needs the whole strip clickable (e.g. a menu is open).
    capture_all: AtomicBool,
}

impl PetWindowState {
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
}

type Geometry = (PhysicalPosition<i32>, PhysicalSize<u32>);

fn target_geometry<R: Runtime>(win: &WebviewWindow<R>) -> Option<Geometry> {
    let monitor = win
        .primary_monitor()
        .ok()
        .flatten()
        .or_else(|| win.current_monitor().ok().flatten())?;
    let area = *monitor.work_area();
    let scale = monitor.scale_factor();
    let height = ((STRIP_HEIGHT * scale).round() as u32).min(area.size.height.max(1));
    let y = area.position.y + area.size.height as i32 - height as i32;
    Some((PhysicalPosition::new(area.position.x, y), PhysicalSize::new(area.size.width, height)))
}

fn apply_geometry<R: Runtime>(win: &WebviewWindow<R>, geo: Geometry) {
    let _ = win.set_size(geo.1);
    let _ = win.set_position(geo.0);
}

/// Create the pet window (hidden until anchored) and start the cursor poller.
pub fn create<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let win = WebviewWindowBuilder::new(app, PET_LABEL, WebviewUrl::App("index.html".into()))
        .title("AI Pet")
        .transparent(true)
        .decorations(false)
        .shadow(false)
        .always_on_top(true)
        .visible_on_all_workspaces(true)
        .skip_taskbar(true)
        .resizable(false)
        .maximizable(false)
        .minimizable(false)
        .focused(false)
        // macOS: the first click on a bubble should press the button, not just activate the app.
        .accept_first_mouse(true)
        .visible(false)
        .inner_size(960.0, STRIP_HEIGHT)
        .build()?;

    let geo = target_geometry(&win);
    if let Some(geo) = geo {
        apply_geometry(&win, geo);
    }
    win.set_ignore_cursor_events(true)?;
    win.show()?;
    spawn_cursor_poller(app.clone(), geo);
    Ok(())
}

fn spawn_cursor_poller<R: Runtime>(app: AppHandle<R>, initial: Option<Geometry>) {
    std::thread::Builder::new()
        .name("pet-cursor-poller".into())
        .spawn(move || {
            let mut interactive = false;
            let mut last_geo = initial;
            let mut tick: u64 = 0;
            loop {
                std::thread::sleep(POLL_EVERY);
                tick = tick.wrapping_add(1);
                let Some(win) = app.get_webview_window(PET_LABEL) else {
                    continue;
                };
                let state = app.state::<PetWindowState>();
                if state.is_paused() {
                    continue;
                }

                if tick % REANCHOR_EVERY == 0 {
                    if let Some(geo) = target_geometry(&win) {
                        if last_geo != Some(geo) {
                            apply_geometry(&win, geo);
                            last_geo = Some(geo);
                        }
                    }
                }

                let hit = state.capture_all.load(Ordering::Relaxed) || cursor_over_hit_rect(&app, &win, &state);
                if hit != interactive {
                    if win.set_ignore_cursor_events(!hit).is_ok() {
                        interactive = hit;
                        let _ = app.emit_to(PET_LABEL, "pet-hover", hit);
                    }
                }
            }
        })
        .expect("spawn pet cursor poller");
}

fn cursor_over_hit_rect<R: Runtime>(app: &AppHandle<R>, win: &WebviewWindow<R>, state: &PetWindowState) -> bool {
    let Ok(cursor) = app.cursor_position() else { return false };
    let Ok(origin) = win.inner_position() else { return false };
    let scale = win.scale_factor().unwrap_or(1.0);
    let x = (cursor.x - origin.x as f64) / scale;
    let y = (cursor.y - origin.y as f64) / scale;
    let Ok(rects) = state.rects.lock() else { return false };
    rects.iter().any(|r| r.contains(x, y))
}

/// Show/hide the pet strip ("Pause pets").
pub fn set_paused<R: Runtime>(app: &AppHandle<R>, paused: bool) {
    let state = app.state::<PetWindowState>();
    state.paused.store(paused, Ordering::Relaxed);
    if let Some(win) = app.get_webview_window(PET_LABEL) {
        if paused {
            let _ = win.hide();
        } else {
            if let Some(geo) = target_geometry(&win) {
                apply_geometry(&win, geo);
            }
            let _ = win.set_ignore_cursor_events(true);
            let _ = win.show();
        }
    }
    crate::tray::sync_paused(app, paused);
    let _ = app.emit("pets-paused", paused);
}

/// Bring the panel window to the front, optionally focusing a pet/session.
pub fn show_panel<R: Runtime>(app: &AppHandle<R>, focus: Option<serde_json::Value>) {
    if let Some(panel) = app.get_webview_window(PANEL_LABEL) {
        let _ = panel.unminimize();
        let _ = panel.show();
        let _ = panel.set_focus();
        if let Some(focus) = focus {
            let _ = app.emit_to(PANEL_LABEL, "panel-focus", focus);
        }
    }
}

// ---------- commands ----------

#[tauri::command]
pub fn set_hit_rects(state: tauri::State<'_, PetWindowState>, rects: Vec<HitRect>) {
    if let Ok(mut guard) = state.rects.lock() {
        // Ignore degenerate rects and cap the count; the stage only has a handful of pets.
        *guard = rects.into_iter().filter(|r| r.w > 0.0 && r.h > 0.0).take(128).collect();
    }
}

#[tauri::command]
pub fn set_capture_all(state: tauri::State<'_, PetWindowState>, capture: bool) {
    state.capture_all.store(capture, Ordering::Relaxed);
}

#[tauri::command]
pub fn set_pets_paused<R: Runtime>(app: AppHandle<R>, paused: bool) {
    set_paused(&app, paused);
}

#[tauri::command]
pub fn get_pets_paused(state: tauri::State<'_, PetWindowState>) -> bool {
    state.is_paused()
}

#[tauri::command]
pub fn open_panel<R: Runtime>(app: AppHandle<R>, focus: Option<serde_json::Value>) {
    show_panel(&app, focus);
}

#[cfg(test)]
mod tests {
    use super::HitRect;

    #[test]
    fn hit_rect_padding() {
        let r = HitRect { x: 100.0, y: 50.0, w: 80.0, h: 60.0 };
        assert!(r.contains(140.0, 80.0));
        assert!(r.contains(97.0, 48.0));
        assert!(!r.contains(80.0, 80.0));
        assert!(!r.contains(140.0, 130.0));
    }
}
