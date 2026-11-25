//! The pet window: a small transparent, undecorated, always-on-top window holding the default pet (plus the session
//! pets and room for one bubble, laid out by the webview). The user drags it anywhere (`pet_start_drag`, the OS move
//! loop); where the default pet ends up is saved per monitor setup and restored on start, clamped back onto a
//! visible monitor's work area when that spot is off-screen (monitor unplugged, resolution change).
//!
//! The window is click-through by default; a background thread polls the global cursor (~50 ms) and turns cursor
//! events back on only while the pointer is over a hit-rect the webview reported (pets, bubbles). The same poll
//! reports cursor movement as user activity (`user-activity`), settles drags and watches the monitor setup.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::{
    AppHandle, Emitter, LogicalSize, Manager, Monitor, PhysicalPosition, Runtime, WebviewUrl, WebviewWindow,
    WebviewWindowBuilder,
};

pub const PET_LABEL: &str = "pet";
pub const PANEL_LABEL: &str = "panel";

/// Pet right-click menu ids (handled in `tray::handle_menu_event`: Tauri menu handlers are app-wide).
pub const MENU_OPEN_HUB: &str = "pet_open_hub";
pub const MENU_RENAME: &str = "pet_rename";
pub const MENU_RESET: &str = "pet_reset_position";
pub const MENU_SETTINGS: &str = "pet_settings";
pub const MENU_QUIT: &str = "pet_quit";

const POLL_EVERY: Duration = Duration::from_millis(50);
/// Re-check the monitor setup every N polls (~2 s).
const MONITOR_CHECK_EVERY: u64 = 40;
/// Show the window after this many polls even if the webview never sent its frame (~3 s).
const SHOW_FALLBACK_AFTER: u64 = 60;
/// A drag counts as finished after the window stopped moving this long: then it is clamped and saved.
const DRAG_SETTLE: Duration = Duration::from_millis(500);
/// Emit `user-activity` at most this often (the webview throttles its hub reports further).
const ACTIVITY_EMIT_EVERY: Duration = Duration::from_secs(5);
/// Cursor movement below this many physical px (jitter) is not activity.
const ACTIVITY_MIN_MOVE: f64 = 2.0;
/// Extra slack (logical px) around each hit-rect so the pointer doesn't "fall through" at edges.
const HIT_PADDING: f64 = 6.0;
/// Gap (logical px) between the default pet and the work area's right edge at the default spot.
const DEFAULT_MARGIN: f64 = 24.0;
/// Saved spots kept (one per monitor setup).
const MAX_SAVED_SETUPS: usize = 16;
const POSITION_FILE: &str = "pet-position.json";
/// Our own recent window moves remembered (their `Moved` events can arrive late, after a newer move).
const OWN_MOVES_KEPT: usize = 4;

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

/// The window layout the webview wants (logical px): its size and where the default pet's box sits inside it.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetFrame {
    pub width: f64,
    pub height: f64,
    pub pet_x: f64,
    pub pet_y: f64,
    pub pet_w: f64,
    pub pet_h: f64,
}

impl Default for PetFrame {
    /// Roughly the webview's default (112 px pet with its name, bubble room above) until it reports its own.
    fn default() -> Self {
        Self { width: 340.0, height: 582.0, pet_x: 114.0, pet_y: 440.0, pet_w: 112.0, pet_h: 132.0 }
    }
}

impl PetFrame {
    fn sane(&self) -> bool {
        let ok = |v: f64, max: f64| v.is_finite() && (0.0..=max).contains(&v);
        ok(self.width, 8192.0)
            && ok(self.height, 8192.0)
            && self.width >= 1.0
            && self.height >= 1.0
            && ok(self.pet_x, self.width)
            && ok(self.pet_y, self.height)
            && ok(self.pet_w, self.width)
            && ok(self.pet_h, self.height)
    }
}

/// Free space (logical px) from the default pet's box to its monitor's work-area edges: the webview flips the
/// bubble and lines up session pets with it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PetSpace {
    pub left: f64,
    pub right: f64,
    pub above: f64,
    pub below: f64,
}

/// A rectangle in physical screen px.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Area {
    fn right(&self) -> i64 {
        self.x as i64 + self.w as i64
    }
    fn bottom(&self) -> i64 {
        self.y as i64 + self.h as i64
    }
    fn contains(&self, x: i64, y: i64) -> bool {
        x >= self.x as i64 && x < self.right() && y >= self.y as i64 && y < self.bottom()
    }
    fn overlap(&self, o: &Area) -> i64 {
        let w = self.right().min(o.right()) - (self.x as i64).max(o.x as i64);
        let h = self.bottom().min(o.bottom()) - (self.y as i64).max(o.y as i64);
        if w > 0 && h > 0 {
            w * h
        } else {
            0
        }
    }
    /// Squared distance from a point to this rectangle (0 inside).
    fn distance2(&self, x: i64, y: i64) -> i64 {
        let dx = if x < self.x as i64 { self.x as i64 - x } else { (x - (self.right() - 1)).max(0) };
        let dy = if y < self.y as i64 { self.y as i64 - y } else { (y - (self.bottom() - 1)).max(0) };
        dx * dx + dy * dy
    }
}

/// One monitor: its work area (without taskbar / dock), full bounds and scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    pub work: Area,
    pub full: Area,
    pub scale: f64,
}

fn screen_of(m: &Monitor) -> Screen {
    let wa = m.work_area();
    let (pos, size) = (m.position(), m.size());
    Screen {
        work: Area { x: wa.position.x, y: wa.position.y, w: wa.size.width as i32, h: wa.size.height as i32 },
        full: Area { x: pos.x, y: pos.y, w: size.width as i32, h: size.height as i32 },
        scale: m.scale_factor(),
    }
}

/// The screen a box belongs to: the one whose work area holds its centre, else the one it overlaps most, else the
/// nearest one. None without screens.
pub fn screen_for(b: Area, screens: &[Screen]) -> Option<&Screen> {
    let (cx, cy) = (b.x as i64 + b.w as i64 / 2, b.y as i64 + b.h as i64 / 2);
    screens
        .iter()
        .find(|s| s.work.contains(cx, cy))
        .or_else(|| screens.iter().filter(|s| s.work.overlap(&b) > 0).max_by_key(|s| s.work.overlap(&b)))
        .or_else(|| screens.iter().min_by_key(|s| s.work.distance2(cx, cy)))
}

/// Top-left for box `b` so that it lies fully inside the work area of the screen it belongs to (pinned to the
/// area's top-left when it is bigger than the area). Unchanged without screens.
pub fn clamp_box(b: Area, screens: &[Screen]) -> (i32, i32) {
    let Some(s) = screen_for(b, screens) else { return (b.x, b.y) };
    let a = s.work;
    let fit = |v: i32, start: i32, len: i32, size: i32| {
        if size >= len {
            start
        } else {
            v.clamp(start, start + len - size)
        }
    };
    (fit(b.x, a.x, a.w, b.w), fit(b.y, a.y, a.h, b.h))
}

/// The default spot: bottom-right of the work area, `margin` px from its right edge.
pub fn default_spot(work: Area, w: i32, h: i32, margin: i32) -> (i32, i32) {
    (work.x + (work.w - w - margin).max(0), work.y + (work.h - h).max(0))
}

/// Space (logical px) around box `b` within `s`'s work area.
pub fn space_around(b: Area, s: &Screen) -> PetSpace {
    let scale = if s.scale > 0.0 { s.scale } else { 1.0 };
    let a = s.work;
    let l = |v: i64| (v.max(0) as f64 / scale).round();
    PetSpace {
        left: l(b.x as i64 - a.x as i64),
        right: l(a.right() - (b.x as i64 + b.w as i64)),
        above: l(b.y as i64 - a.y as i64),
        below: l(a.bottom() - (b.y as i64 + b.h as i64)),
    }
}

/// Identifies a monitor setup (which monitors, where, how big, at what scale); spots are saved per setup.
pub fn setup_key(screens: &[Screen]) -> String {
    let mut parts: Vec<String> = screens
        .iter()
        .map(|s| format!("{},{},{}x{}@{}", s.full.x, s.full.y, s.full.w, s.full.h, (s.scale * 100.0).round() as i64))
        .collect();
    parts.sort();
    parts.join(";")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spot {
    pub x: i32,
    pub y: i32,
}

/// The saved default-pet spots (physical px of its top-left), per monitor setup, plus the last one anywhere.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedSpots {
    #[serde(default)]
    pub setups: BTreeMap<String, Spot>,
    #[serde(default)]
    pub last: Option<Spot>,
    /// Setup keys, oldest first (for dropping old setups).
    #[serde(default)]
    pub order: Vec<String>,
}

impl SavedSpots {
    pub fn get(&self, key: &str) -> Option<Spot> {
        self.setups.get(key).copied()
    }

    pub fn put(&mut self, key: &str, spot: Spot) {
        self.setups.insert(key.to_string(), spot);
        self.order.retain(|k| k != key);
        self.order.push(key.to_string());
        while self.order.len() > MAX_SAVED_SETUPS {
            let old = self.order.remove(0);
            self.setups.remove(&old);
        }
        self.last = Some(spot);
    }

    pub fn forget(&mut self, key: &str) {
        self.setups.remove(key);
        self.order.retain(|k| k != key);
    }

    /// Repair what was read from disk: `order` lists each saved setup once (setups missing from it come first, in
    /// key order, as the oldest), setups and order agree, and at most MAX_SAVED_SETUPS are kept (newest).
    pub fn normalized(mut self) -> Self {
        let mut order: Vec<String> = Vec::new();
        for k in self.setups.keys() {
            if !self.order.contains(k) {
                order.push(k.clone());
            }
        }
        for k in std::mem::take(&mut self.order) {
            if self.setups.contains_key(&k) && !order.contains(&k) {
                order.push(k);
            }
        }
        while order.len() > MAX_SAVED_SETUPS {
            let old = order.remove(0);
            self.setups.remove(&old);
        }
        self.order = order;
        self
    }
}

#[derive(Default)]
struct Place {
    /// Physical screen px of the default pet's top-left. None until first placed.
    anchor: Option<PhysicalPosition<i32>>,
    frame: PetFrame,
    /// Where we recently moved the window ourselves (those Moved events are not a user drag).
    expected: VecDeque<PhysicalPosition<i32>>,
    /// The user moved the window; clamp and save once it stays put for DRAG_SETTLE.
    moved_at: Option<Instant>,
    setup: String,
    space: Option<PetSpace>,
    shown: bool,
    saved: SavedSpots,
    file: Option<PathBuf>,
}

#[derive(Default)]
pub struct PetWindowState {
    rects: Mutex<Vec<HitRect>>,
    paused: AtomicBool,
    /// Set by the webview while it needs the whole window clickable.
    capture_all: AtomicBool,
    place: Mutex<Place>,
}

impl PetWindowState {
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }
}

/// The focus the panel should show, kept until the panel asks (a freshly created panel misses the event).
#[derive(Default)]
pub struct PanelFocusState(Mutex<Option<serde_json::Value>>);

/// The monitors and scale, read from the window. Window getters wait for the main thread when called from another
/// thread, so read this BEFORE locking `Place` (the main thread may be waiting for that lock in a command).
struct Geo {
    all: Vec<Screen>,
    primary: Option<Screen>,
    /// The window's scale, for points on no monitor.
    scale: f64,
}

fn geo<R: Runtime>(win: &WebviewWindow<R>) -> Geo {
    let all: Vec<Screen> = win.available_monitors().map(|ms| ms.iter().map(screen_of).collect()).unwrap_or_default();
    let primary = win.primary_monitor().ok().flatten().map(|m| screen_of(&m)).or_else(|| all.first().copied());
    Geo { all, primary, scale: win.scale_factor().unwrap_or(1.0) }
}

impl Geo {
    /// The scale of the screen under `p` (else the window's).
    fn scale_at(&self, p: PhysicalPosition<i32>) -> f64 {
        self.all.iter().find(|s| s.full.contains(p.x as i64, p.y as i64)).map(|s| s.scale).unwrap_or(self.scale)
    }
}

/// The default pet's box at `anchor` (physical px).
fn pet_box(frame: &PetFrame, anchor: PhysicalPosition<i32>, scale: f64) -> Area {
    Area {
        x: anchor.x,
        y: anchor.y,
        w: ((frame.pet_w * scale).round() as i32).max(1),
        h: ((frame.pet_h * scale).round() as i32).max(1),
    }
}

fn default_anchor(g: &Geo, frame: &PetFrame) -> PhysicalPosition<i32> {
    let Some(s) = g.primary else { return PhysicalPosition::new(0, 0) };
    let b = pet_box(frame, PhysicalPosition::new(0, 0), s.scale);
    let (x, y) = default_spot(s.work, b.w, b.h, (DEFAULT_MARGIN * s.scale).round() as i32);
    PhysicalPosition::new(x, y)
}

/// Clamp the anchor onto a visible work area; returns the new anchor and the space around the pet there.
fn clamped(g: &Geo, frame: &PetFrame, anchor: PhysicalPosition<i32>) -> (PhysicalPosition<i32>, Option<PetSpace>) {
    let b = pet_box(frame, anchor, g.scale_at(anchor));
    let (x, y) = clamp_box(b, &g.all);
    let anchor = PhysicalPosition::new(x, y);
    let b = Area { x, y, ..b };
    (anchor, screen_for(b, &g.all).map(|s| space_around(b, s)))
}

/// Window changes decided while holding the `Place` lock and made after releasing it. On the main thread a move
/// fires `WindowEvent::Moved` synchronously, and `on_moved` takes the same lock: moving the window while holding it
/// would deadlock the app.
#[derive(Default)]
#[must_use]
struct Ops {
    frame: Option<(LogicalSize<f64>, PhysicalPosition<i32>)>,
    show: bool,
    hide: bool,
    space: Option<PetSpace>,
}

impl Ops {
    fn run<R: Runtime>(self, app: &AppHandle<R>, win: &WebviewWindow<R>) {
        if self.hide {
            let _ = win.hide();
        }
        if let Some((size, pos)) = self.frame {
            let _ = win.set_size(size);
            let _ = win.set_position(pos);
        }
        if self.show {
            let _ = win.set_ignore_cursor_events(true);
            let _ = win.show();
        }
        if let Some(space) = self.space {
            let _ = app.emit_to(PET_LABEL, "pet-space", space);
        }
    }
}

/// The window size and position that put the default pet at the anchor (recorded as our own move).
fn apply(place: &mut Place, g: &Geo) -> Option<(LogicalSize<f64>, PhysicalPosition<i32>)> {
    let anchor = place.anchor?;
    let scale = g.scale_at(anchor);
    let f = place.frame;
    let pos = PhysicalPosition::new(
        anchor.x - (f.pet_x * scale).round() as i32,
        anchor.y - (f.pet_y * scale).round() as i32,
    );
    place.expected.push_back(pos);
    while place.expected.len() > OWN_MOVES_KEPT {
        place.expected.pop_front();
    }
    Some((LogicalSize::new(f.width, f.height), pos))
}

/// The new space to tell the webview about, when it changed.
fn changed_space(place: &mut Place, space: Option<PetSpace>) -> Option<PetSpace> {
    if space.is_some() && space != place.space {
        place.space = space;
        return space;
    }
    None
}

fn load_spots(file: &Option<PathBuf>) -> SavedSpots {
    file.as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<SavedSpots>(&s).ok())
        .unwrap_or_default()
        .normalized()
}

/// Write the saved spots: to a temp file next to it, then renamed over it (never a half-written file).
pub fn write_spots(path: &std::path::Path, spots: &SavedSpots) -> std::io::Result<()> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(spots).map_err(std::io::Error::other)?;
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let tmp = path.with_extension(format!("json.{}.{n}.tmp", std::process::id()));
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// A save to do once the `Place` lock is released (no file I/O under the lock).
#[must_use]
struct SpotsWrite(Option<(PathBuf, SavedSpots)>);

impl SpotsWrite {
    fn run(self) {
        if let Some((path, spots)) = self.0 {
            if let Err(err) = write_spots(&path, &spots) {
                eprintln!("[pawl] could not save the pet position: {err}");
            }
        }
    }
}

/// Remember where the default pet is, for this monitor setup; returns the write to do after unlocking.
fn remember(place: &mut Place) -> SpotsWrite {
    let Some(a) = place.anchor else { return SpotsWrite(None) };
    let key = place.setup.clone();
    place.saved.put(&key, Spot { x: a.x, y: a.y });
    SpotsWrite(place.file.clone().map(|path| (path, place.saved.clone())))
}

/// Create the pet window (hidden until the webview sends its layout) and start the cursor poller.
pub fn create<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let frame = PetFrame::default();
    let win = WebviewWindowBuilder::new(app, PET_LABEL, WebviewUrl::App("index.html".into()))
        .title("Pawl")
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
        .inner_size(frame.width, frame.height)
        .build()?;

    let ops = {
        let g = geo(&win);
        let state = app.state::<PetWindowState>();
        let mut place = state.place.lock().expect("pet place lock");
        place.file = app.path().app_config_dir().ok().map(|d| d.join(POSITION_FILE));
        place.saved = load_spots(&place.file);
        place.setup = setup_key(&g.all);
        let start = place
            .saved
            .get(&place.setup)
            .or(place.saved.last)
            .map(|s| PhysicalPosition::new(s.x, s.y))
            .unwrap_or_else(|| default_anchor(&g, &frame));
        let (anchor, space) = clamped(&g, &frame, start);
        place.anchor = Some(anchor);
        place.space = space;
        Ops { frame: apply(&mut place, &g), ..Ops::default() }
    };
    ops.run(app, &win);
    win.set_ignore_cursor_events(true)?;
    spawn_cursor_poller(app.clone());
    Ok(())
}

/// Whether to show the window now (once, and not while paused).
fn show_pet(place: &mut Place, paused: bool) -> bool {
    if place.shown || paused {
        return false;
    }
    place.shown = true;
    true
}

/// True when a `Moved` to `pos` is our own doing: one of our recent moves (forgotten once seen, with the older
/// ones), or where the window already is (`current`). Within 1 px.
pub fn own_move(expected: &mut VecDeque<PhysicalPosition<i32>>, current: Option<PhysicalPosition<i32>>, pos: PhysicalPosition<i32>) -> bool {
    let near = |a: &PhysicalPosition<i32>| (a.x - pos.x).abs() <= 1 && (a.y - pos.y).abs() <= 1;
    if let Some(i) = expected.iter().position(near) {
        expected.drain(..=i);
        return true;
    }
    current.as_ref().is_some_and(near)
}

/// The pet window moved. Our own moves are ignored; anything else (a drag, the OS) moves the default pet with it.
pub fn on_moved<R: Runtime>(app: &AppHandle<R>, pos: PhysicalPosition<i32>) {
    let Some(win) = app.get_webview_window(PET_LABEL) else { return };
    let scale = win.scale_factor().unwrap_or(1.0);
    let state = app.state::<PetWindowState>();
    let Ok(mut place) = state.place.lock() else { return };
    // A hidden window is not being dragged: its moves are window creation and our own placing.
    if !place.shown {
        return;
    }
    let f = place.frame;
    let current = place.anchor.map(|a| {
        PhysicalPosition::new(a.x - (f.pet_x * scale).round() as i32, a.y - (f.pet_y * scale).round() as i32)
    });
    if own_move(&mut place.expected, current, pos) {
        return;
    }
    place.anchor = Some(PhysicalPosition::new(
        pos.x + (f.pet_x * scale).round() as i32,
        pos.y + (f.pet_y * scale).round() as i32,
    ));
    place.moved_at = Some(Instant::now());
}

fn spawn_cursor_poller<R: Runtime>(app: AppHandle<R>) {
    std::thread::Builder::new()
        .name("pet-cursor-poller".into())
        .spawn(move || {
            let mut interactive = false;
            let mut tick: u64 = 0;
            let mut last_cursor: Option<PhysicalPosition<f64>> = None;
            let mut last_activity: Option<Instant> = None;
            loop {
                std::thread::sleep(POLL_EVERY);
                tick = tick.wrapping_add(1);
                let Some(win) = app.get_webview_window(PET_LABEL) else {
                    continue;
                };
                let state = app.state::<PetWindowState>();
                let cursor = app.cursor_position().ok();

                // Cursor movement is user activity on this machine (reported even while pets are paused).
                if let Some(c) = cursor {
                    let moved = last_cursor.is_some_and(|p| cursor_moved(p, c));
                    last_cursor = Some(c);
                    if moved && last_activity.is_none_or(|t| t.elapsed() >= ACTIVITY_EMIT_EVERY) {
                        last_activity = Some(Instant::now());
                        let _ = app.emit_to(PET_LABEL, "user-activity", ());
                    }
                }

                if state.is_paused() {
                    continue;
                }

                settle(&app, &win, &state, tick);

                let hit = state.capture_all.load(Ordering::Relaxed)
                    || cursor.is_some_and(|c| cursor_over_hit_rect(c, &win, &state));
                if hit != interactive && win.set_ignore_cursor_events(!hit).is_ok() {
                    interactive = hit;
                    let _ = app.emit_to(PET_LABEL, "pet-hover", hit);
                }
            }
        })
        .expect("spawn pet cursor poller");
}

/// Finish drags (clamp + save), follow monitor changes, and show the window if the webview never did.
fn settle<R: Runtime>(app: &AppHandle<R>, win: &WebviewWindow<R>, state: &PetWindowState, tick: u64) {
    let (dropped, show) = {
        let Ok(mut place) = state.place.lock() else { return };
        let show = tick >= SHOW_FALLBACK_AFTER && show_pet(&mut place, false);
        (place.moved_at.is_some_and(|t| t.elapsed() >= DRAG_SETTLE), show)
    };
    if show {
        Ops { show, ..Ops::default() }.run(app, win);
    }
    if !dropped && !tick.is_multiple_of(MONITOR_CHECK_EVERY) {
        return;
    }
    // Getters first, then the lock (see `Geo`).
    let g = geo(win);
    if g.all.is_empty() {
        return;
    }
    let mut place = match state.place.lock() {
        Ok(p) => p,
        Err(_) => return,
    };
    let dropped = place.moved_at.is_some_and(|t| t.elapsed() >= DRAG_SETTLE);
    let Some(anchor) = place.anchor else { return };
    let key = setup_key(&g.all);
    let mut start = anchor;
    let new_setup = key != place.setup;
    if new_setup {
        // Monitors changed: go back to where the pet was on this setup before, if it ever was.
        place.setup = key;
        if let Some(s) = place.saved.get(&place.setup) {
            start = PhysicalPosition::new(s.x, s.y);
        }
    }
    let frame = place.frame;
    let (next, space) = clamped(&g, &frame, start);
    let mut ops = Ops::default();
    // After a drop also re-apply the frame: the webview may have changed it while the pet was moving.
    if next != anchor || dropped || new_setup {
        place.anchor = Some(next);
        ops.frame = apply(&mut place, &g);
    }
    let write = if dropped {
        place.moved_at = None;
        remember(&mut place)
    } else {
        SpotsWrite(None)
    };
    ops.space = changed_space(&mut place, space);
    drop(place);
    ops.run(app, win);
    write.run();
}

/// More than jitter between two cursor samples (physical px).
fn cursor_moved(a: PhysicalPosition<f64>, b: PhysicalPosition<f64>) -> bool {
    (a.x - b.x).abs() >= ACTIVITY_MIN_MOVE || (a.y - b.y).abs() >= ACTIVITY_MIN_MOVE
}

fn cursor_over_hit_rect<R: Runtime>(cursor: PhysicalPosition<f64>, win: &WebviewWindow<R>, state: &PetWindowState) -> bool {
    let Ok(origin) = win.inner_position() else { return false };
    let scale = win.scale_factor().unwrap_or(1.0);
    let x = (cursor.x - origin.x as f64) / scale;
    let y = (cursor.y - origin.y as f64) / scale;
    let Ok(rects) = state.rects.lock() else { return false };
    rects.iter().any(|r| r.contains(x, y))
}

/// Show/hide the pet window ("Pause pets").
pub fn set_paused<R: Runtime>(app: &AppHandle<R>, paused: bool) {
    let state = app.state::<PetWindowState>();
    state.paused.store(paused, Ordering::Relaxed);
    if let Some(win) = app.get_webview_window(PET_LABEL) {
        let g = geo(&win);
        let ops = match state.place.lock() {
            Ok(mut place) if paused => {
                place.shown = false;
                Ops { hide: true, ..Ops::default() }
            }
            Ok(mut place) => Ops { frame: apply(&mut place, &g), show: show_pet(&mut place, false), ..Ops::default() },
            Err(_) => Ops::default(),
        };
        ops.run(app, &win);
    }
    crate::tray::sync_paused(app, paused);
    let _ = app.emit("pets-paused", paused);
}

/// Put the default pet back at its default spot (bottom-right of the primary monitor) and remember that.
pub fn reset_position<R: Runtime>(app: &AppHandle<R>) {
    let Some(win) = app.get_webview_window(PET_LABEL) else { return };
    let g = geo(&win);
    let state = app.state::<PetWindowState>();
    let (ops, write) = {
        let Ok(mut place) = state.place.lock() else { return };
        place.setup = setup_key(&g.all);
        let frame = place.frame;
        let (anchor, space) = clamped(&g, &frame, default_anchor(&g, &frame));
        place.anchor = Some(anchor);
        place.moved_at = None;
        let frame = apply(&mut place, &g);
        let key = place.setup.clone();
        place.saved.forget(&key);
        let write = remember(&mut place);
        (Ops { frame, space: changed_space(&mut place, space), ..Ops::default() }, write)
    };
    ops.run(app, &win);
    write.run();
    if state.is_paused() {
        set_paused(app, false);
    }
}

/// Ask the pet to open its bubble (tray click, a second launch): the panel itself only opens on request.
pub fn pet_attention<R: Runtime>(app: &AppHandle<R>) {
    if app.state::<PetWindowState>().is_paused() {
        set_paused(app, false);
    }
    let _ = app.emit_to(PET_LABEL, "pet-attention", ());
}

fn create_panel<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<WebviewWindow<R>> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == PANEL_LABEL)
        .cloned()
        .ok_or_else(|| tauri::Error::WindowNotFound)?;
    WebviewWindowBuilder::from_config(app, &config)?.build()
}

/// Show, unminimize, bring to front and focus the panel.
fn bring_to_front<R: Runtime>(panel: &WebviewWindow<R>) {
    // Order matters on Windows: a window must be visible and restored before it can take the foreground.
    let _ = panel.show();
    if panel.is_minimized().unwrap_or(false) {
        let _ = panel.unminimize();
    }
    let _ = panel.set_focus();
    // Bring it above other windows even when the OS refused the focus change.
    let _ = panel.set_always_on_top(true);
    let _ = panel.set_always_on_top(false);
}

/// Show the panel window, every time: create it again if it is gone, show, unminimize, bring to front, focus.
/// `focus` picks the view (kept for a freshly created panel until it asks with `take_panel_focus`).
pub fn show_panel<R: Runtime>(app: &AppHandle<R>, focus: Option<serde_json::Value>) {
    if let Some(panel) = app.get_webview_window(PANEL_LABEL) {
        bring_to_front(&panel);
        if let Some(focus) = focus {
            let _ = app.emit_to(PANEL_LABEL, "panel-focus", focus);
        }
        return;
    }
    // A new panel is not listening yet: it asks for the view once it loaded.
    if let Some(focus) = focus {
        if let Ok(mut pending) = app.state::<PanelFocusState>().0.lock() {
            *pending = Some(focus);
        }
    }
    // Building a window from a sync command or a menu / tray handler (the main thread) can deadlock on Windows
    // (WebviewWindowBuilder::build docs): build it on the async runtime instead.
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let panel = match app.get_webview_window(PANEL_LABEL) {
            Some(p) => p,
            None => match create_panel(&app) {
                Ok(p) => p,
                Err(err) => {
                    eprintln!("[pawl] could not create the panel window: {err}");
                    return;
                }
            },
        };
        bring_to_front(&panel);
    });
}

/// Longest pet name shown at the top of the menu (pet names are at most 40 characters; this is a backstop).
const MENU_NAME_MAX: usize = 40;

/// The pet's name as a menu label: one line, at most `MENU_NAME_MAX` characters, and `&` kept literal (on Windows
/// a single `&` marks the next letter as the access key).
fn menu_name_label(name: &str) -> Option<String> {
    let one_line: String = name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let trimmed = one_line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut label: String = trimmed.chars().take(MENU_NAME_MAX).collect();
    if trimmed.chars().count() > MENU_NAME_MAX {
        label.push('…');
    }
    Some(label.replace('&', "&&"))
}

fn context_menu<R: Runtime>(app: &AppHandle<R>, name: Option<&str>, can_rename: bool) -> tauri::Result<Menu<R>> {
    let header = match name.and_then(menu_name_label) {
        Some(label) => Some(MenuItem::new(app, label, false, None::<&str>)?),
        None => None,
    };
    let top_sep = PredefinedMenuItem::separator(app)?;
    let open = MenuItem::with_id(app, MENU_OPEN_HUB, "Open hub", true, None::<&str>)?;
    let rename = MenuItem::with_id(app, MENU_RENAME, "Rename…", can_rename, None::<&str>)?;
    let reset = MenuItem::with_id(app, MENU_RESET, "Reset position", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, MENU_SETTINGS, "Settings", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit Pawl", true, None::<&str>)?;
    match &header {
        Some(h) => Menu::with_items(app, &[h, &top_sep, &open, &rename, &reset, &settings, &sep, &quit]),
        None => Menu::with_items(app, &[&open, &rename, &reset, &settings, &sep, &quit]),
    }
}

/// "Rename…" in the pet's menu: the pet window opens its bubble with the rename field.
pub fn pet_rename<R: Runtime>(app: &AppHandle<R>) {
    // The rename field needs the keyboard (the pet window is created without focus).
    if let Some(win) = app.get_webview_window(PET_LABEL) {
        let _ = win.set_focus();
    }
    let _ = app.emit_to(PET_LABEL, "pet-rename", ());
}

// ---------- commands ----------

#[tauri::command]
pub fn set_hit_rects(state: tauri::State<'_, PetWindowState>, rects: Vec<HitRect>) {
    if let Ok(mut guard) = state.rects.lock() {
        // Ignore degenerate rects and cap the count; the window only has a handful of pets.
        *guard = rects.into_iter().filter(|r| r.w > 0.0 && r.h > 0.0).take(128).collect();
    }
}

#[tauri::command]
pub fn set_capture_all(state: tauri::State<'_, PetWindowState>, capture: bool) {
    state.capture_all.store(capture, Ordering::Relaxed);
}

/// The webview's layout: size the window, keep the default pet where it is, and say how much room is around it.
#[tauri::command]
pub fn set_pet_frame<R: Runtime>(app: AppHandle<R>, frame: PetFrame) -> Result<Option<PetSpace>, String> {
    if !frame.sane() {
        return Err("invalid pet frame".into());
    }
    let win = app.get_webview_window(PET_LABEL).ok_or("no pet window")?;
    let g = geo(&win);
    let state = app.state::<PetWindowState>();
    let paused = state.is_paused();
    let mut place = state.place.lock().map_err(|_| "pet state poisoned")?;
    let changed_size = place.frame.pet_w != frame.pet_w || place.frame.pet_h != frame.pet_h;
    place.frame = frame;
    let anchor = match place.anchor {
        Some(a) => a,
        None => default_anchor(&g, &frame),
    };
    // A bigger pet may no longer fit where it was; a moving pet is clamped once it is dropped.
    let (anchor, space) = if changed_size || place.moved_at.is_none() {
        clamped(&g, &frame, anchor)
    } else {
        (anchor, place.space)
    };
    place.anchor = Some(anchor);
    let mut ops = Ops::default();
    if place.moved_at.is_none() {
        ops.frame = apply(&mut place, &g);
    }
    ops.show = show_pet(&mut place, paused);
    if space.is_some() {
        place.space = space;
    }
    let out = place.space;
    drop(place);
    ops.run(&app, &win);
    Ok(out)
}

/// Start moving the pet window with the mouse (the OS move loop; the button is down).
#[tauri::command]
pub fn pet_start_drag<R: Runtime>(app: AppHandle<R>) -> Result<(), String> {
    let win = app.get_webview_window(PET_LABEL).ok_or("no pet window")?;
    win.start_dragging().map_err(|e| e.to_string())
}

/// The pet's right-click menu at the cursor: its name, then Open hub, Rename… (only while the hub can take a new
/// name), Reset position, Settings, Quit.
#[tauri::command]
pub fn pet_context_menu<R: Runtime>(app: AppHandle<R>, name: Option<String>, can_rename: Option<bool>) -> Result<(), String> {
    let win = app.get_webview_window(PET_LABEL).ok_or("no pet window")?;
    let menu = context_menu(&app, name.as_deref(), can_rename.unwrap_or(false)).map_err(|e| e.to_string())?;
    win.popup_menu(&menu).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn reset_pet_position<R: Runtime>(app: AppHandle<R>) {
    reset_position(&app);
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

/// The view the panel was asked to show before it could listen (once).
#[tauri::command]
pub fn take_panel_focus(state: tauri::State<'_, PanelFocusState>) -> Option<serde_json::Value> {
    state.0.lock().ok().and_then(|mut f| f.take())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_shows_the_pets_name_on_one_line() {
        assert_eq!(menu_name_label("Mochi").as_deref(), Some("Mochi"));
        assert_eq!(menu_name_label("  Salt & Pepper ").as_deref(), Some("Salt && Pepper"));
        assert_eq!(menu_name_label("two\nlines").as_deref(), Some("two lines"));
        assert_eq!(menu_name_label("   "), None);
        let long = "a".repeat(50);
        let label = menu_name_label(&long).unwrap();
        assert_eq!(label.chars().count(), MENU_NAME_MAX + 1);
        assert!(label.ends_with('…'));
    }

    fn screen(x: i32, y: i32, w: i32, h: i32, taskbar: i32, scale: f64) -> Screen {
        Screen { work: Area { x, y, w, h: h - taskbar }, full: Area { x, y, w, h }, scale }
    }

    fn pet(x: i32, y: i32) -> Area {
        Area { x, y, w: 112, h: 132 }
    }

    #[test]
    fn cursor_jitter_is_not_activity() {
        let a = PhysicalPosition::new(100.0, 100.0);
        assert!(!cursor_moved(a, PhysicalPosition::new(101.0, 100.5)));
        assert!(cursor_moved(a, PhysicalPosition::new(102.0, 100.0)));
        assert!(cursor_moved(a, PhysicalPosition::new(100.0, 97.0)));
    }

    #[test]
    fn hit_rect_padding() {
        let r = HitRect { x: 100.0, y: 50.0, w: 80.0, h: 60.0 };
        assert!(r.contains(140.0, 80.0));
        assert!(r.contains(97.0, 48.0));
        assert!(!r.contains(80.0, 80.0));
        assert!(!r.contains(140.0, 130.0));
    }

    #[test]
    fn a_spot_on_screen_stays_put() {
        let screens = [screen(0, 0, 1920, 1080, 40, 1.0)];
        assert_eq!(clamp_box(pet(500, 300), &screens), (500, 300));
        // Touching the work area's edges is fine.
        assert_eq!(clamp_box(pet(0, 0), &screens), (0, 0));
        assert_eq!(clamp_box(pet(1920 - 112, 1040 - 132), &screens), (1808, 908));
    }

    #[test]
    fn a_spot_over_the_taskbar_or_past_an_edge_is_pulled_back_in() {
        let screens = [screen(0, 0, 1920, 1080, 40, 1.0)];
        assert_eq!(clamp_box(pet(1850, 1000), &screens), (1808, 908));
        assert_eq!(clamp_box(pet(-50, -20), &screens), (0, 0));
    }

    #[test]
    fn a_spot_on_an_unplugged_monitor_goes_to_the_nearest_one() {
        // The pet was on a second monitor to the right that is gone now.
        let screens = [screen(0, 0, 1920, 1080, 40, 1.0)];
        assert_eq!(clamp_box(pet(2500, 400), &screens), (1808, 400));
        // Far below and left of everything: nearest corner.
        assert_eq!(clamp_box(pet(-4000, 5000), &screens), (0, 908));
    }

    #[test]
    fn two_monitors_the_pet_stays_on_the_one_holding_its_centre() {
        let screens = [screen(0, 0, 1920, 1080, 40, 1.0), screen(1920, -200, 2560, 1440, 0, 1.5)];
        // Fully on the second monitor: unchanged.
        assert_eq!(clamp_box(pet(2200, 0), &screens), (2200, 0));
        // Straddling the seam with its centre on the second: pulled fully onto the second.
        assert_eq!(clamp_box(pet(1900, 300), &screens), (1920, 300));
        // Above the first monitor's top but beside the taller second: the second holds no part, so the first.
        assert_eq!(clamp_box(pet(100, -150), &screens), (100, 0));
    }

    #[test]
    fn a_pet_bigger_than_the_work_area_is_pinned_to_its_corner() {
        let screens = [screen(10, 20, 100, 100, 0, 1.0)];
        assert_eq!(clamp_box(pet(50, 50), &screens), (10, 20));
    }

    #[test]
    fn without_monitors_nothing_moves() {
        assert_eq!(clamp_box(pet(-9999, 9999), &[]), (-9999, 9999));
    }

    #[test]
    fn default_spot_is_bottom_right_of_the_work_area() {
        let s = screen(0, 0, 1920, 1080, 40, 1.0);
        assert_eq!(default_spot(s.work, 112, 132, 24), (1920 - 112 - 24, 1040 - 132));
        let tiny = Area { x: 5, y: 5, w: 50, h: 50 };
        assert_eq!(default_spot(tiny, 112, 132, 24), (5, 5));
    }

    #[test]
    fn space_is_logical_px_to_the_work_area_edges() {
        let s = screen(0, 0, 3000, 2000, 100, 2.0);
        let b = Area { x: 1000, y: 1000, w: 224, h: 264 };
        assert_eq!(space_around(b, &s), PetSpace { left: 500.0, right: 888.0, above: 500.0, below: 318.0 });
        // Off the area (never after clamping) reads as no space, not negative space.
        let off = Area { x: -100, y: 1000, w: 224, h: 264 };
        assert_eq!(space_around(off, &s).left, 0.0);
    }

    #[test]
    fn setup_key_ignores_monitor_order_and_taskbars_but_not_scale() {
        let a = screen(0, 0, 1920, 1080, 40, 1.0);
        let b = screen(1920, 0, 2560, 1440, 0, 1.5);
        assert_eq!(setup_key(&[a, b]), setup_key(&[b, a]));
        assert_eq!(setup_key(&[a]), setup_key(&[screen(0, 0, 1920, 1080, 80, 1.0)]));
        assert_ne!(setup_key(&[a]), setup_key(&[screen(0, 0, 1920, 1080, 40, 1.25)]));
        assert_ne!(setup_key(&[a]), setup_key(&[a, b]));
    }

    #[test]
    fn saved_spots_per_setup_with_a_last_resort_and_a_cap() {
        let mut s = SavedSpots::default();
        s.put("one", Spot { x: 1, y: 2 });
        s.put("two", Spot { x: 3, y: 4 });
        assert_eq!(s.get("one"), Some(Spot { x: 1, y: 2 }));
        assert_eq!(s.last, Some(Spot { x: 3, y: 4 }));
        assert_eq!(s.get("three"), None);
        s.put("one", Spot { x: 5, y: 6 });
        assert_eq!(s.order, vec!["two".to_string(), "one".to_string()]);
        s.forget("two");
        assert_eq!(s.get("two"), None);
        for i in 0..40 {
            s.put(&format!("k{i}"), Spot { x: i, y: i });
        }
        assert_eq!(s.setups.len(), MAX_SAVED_SETUPS);
        assert_eq!(s.order.len(), MAX_SAVED_SETUPS);
        assert_eq!(s.get("k39"), Some(Spot { x: 39, y: 39 }));
        assert_eq!(s.get("k0"), None);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<SavedSpots>(&json).unwrap(), s);
        // An old or partial file still loads.
        assert_eq!(serde_json::from_str::<SavedSpots>("{}").unwrap(), SavedSpots::default());
    }

    #[test]
    fn own_moves_are_told_apart_from_drags() {
        let p = PhysicalPosition::new;
        let mut expected: VecDeque<_> = [p(10, 10), p(20, 20), p(30, 30)].into_iter().collect();
        // A late event for an older move of ours: ignored, and it and the ones before it are forgotten.
        assert!(own_move(&mut expected, None, p(20, 21)));
        assert_eq!(expected, VecDeque::from([p(30, 30)]));
        // Where the window already is: ours.
        assert!(own_move(&mut expected, Some(p(500, 500)), p(501, 499)));
        // Anything else is the user (or the OS) moving it.
        assert!(!own_move(&mut expected, Some(p(500, 500)), p(540, 500)));
        assert!(!own_move(&mut expected, None, p(10, 10)));
        assert!(own_move(&mut expected, None, p(30, 30)));
        assert!(expected.is_empty());
    }

    #[test]
    fn saved_spots_are_repaired_on_load() {
        let spot = |x| Spot { x, y: x };
        let mut setups = BTreeMap::new();
        for i in 0..20 {
            setups.insert(format!("s{i:02}"), spot(i));
        }
        let raw = SavedSpots {
            setups,
            last: Some(spot(7)),
            // Duplicates, a key with no spot, and most setups missing from the order.
            order: vec!["s05".into(), "gone".into(), "s05".into(), "s19".into()],
        };
        let s = raw.normalized();
        assert_eq!(s.order.len(), MAX_SAVED_SETUPS);
        assert_eq!(s.setups.len(), MAX_SAVED_SETUPS);
        assert!(s.order.iter().all(|k| s.setups.contains_key(k)));
        assert!(s.setups.keys().all(|k| s.order.contains(k)));
        // Listed ones are the newest (kept, in their order, last); unlisted ones count as older.
        assert_eq!(&s.order[MAX_SAVED_SETUPS - 2..], &["s05".to_string(), "s19".to_string()]);
        assert!(!s.setups.contains_key("s00"));
        assert_eq!(s.last, Some(spot(7)));
        // A clean file is unchanged.
        let mut clean = SavedSpots::default();
        clean.put("a", spot(1));
        clean.put("b", spot(2));
        assert_eq!(clean.clone().normalized(), clean);
    }

    #[test]
    fn spots_are_written_whole_via_a_temp_file() {
        let dir = std::env::temp_dir().join(format!("pawl-spots-test-{}", std::process::id()));
        let path = dir.join("nested").join(POSITION_FILE);
        let mut spots = SavedSpots::default();
        spots.put("setup", Spot { x: 10, y: 20 });
        write_spots(&path, &spots).unwrap();
        spots.put("setup", Spot { x: 30, y: 40 });
        write_spots(&path, &spots).unwrap();
        assert_eq!(load_spots(&Some(path.clone())), spots);
        // No temp files left behind.
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from(POSITION_FILE)]);
        // An unreadable file loads as nothing saved.
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(load_spots(&Some(path.clone())), SavedSpots::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn frames_from_the_webview_are_checked() {
        assert!(PetFrame::default().sane());
        assert!(!PetFrame { width: f64::NAN, ..PetFrame::default() }.sane());
        assert!(!PetFrame { pet_x: 9999.0, ..PetFrame::default() }.sane());
        assert!(!PetFrame { width: 0.0, ..PetFrame::default() }.sane());
        let f: PetFrame = serde_json::from_str(r#"{"width":340,"height":582,"petX":114,"petY":440,"petW":112,"petH":132}"#).unwrap();
        assert_eq!(f, PetFrame::default());
    }
}
