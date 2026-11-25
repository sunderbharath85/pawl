//! Screen capture (v0.2 item 4, docs/CONTRACTS.md "v0.2 Screen and Browser"): the picker's list of screens and
//! windows (`screen_sources`), and one capture of the source the user picked (`screen_capture`), scaled and encoded
//! for the chat. Both need an open picker: `screen_begin` (the UI opens the picker) hands out a nonce they require,
//! and `screen_end` closes it (`gate.rs`, with rate limits).
//!
//! Privacy: the UI captures only when the user picked a source and pressed Send. Pictures live in memory only:
//! never written to disk, logged (`Debug` shows only sizes), or kept here after the command returns. Pawl's own
//! windows are never offered, and on Windows and macOS they are left out of a screen capture while it is taken
//! (they stay on the screen itself); if that can't be done, nothing is captured.

mod encode;
mod gate;
mod permission;
mod sources;

use gate::Gate;
use image::RgbaImage;
use serde::Serialize;
use sources::{Source, SourceId};
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, Runtime, Webview, WebviewWindow};

/// How long to wait for the main thread to apply the capture exclusion to Pawl's windows.
const HIDE_WAIT: Duration = Duration::from_secs(2);
/// After the exclusion is applied: measured on Windows 11, it is in effect for the next screen grab (GDI and Windows
/// Graphics Capture alike) within 5 ms.
const HIDE_SETTLE: Duration = Duration::from_millis(20);

const HIDE_FAILED: &str = "Couldn't hide Pawl's windows; try again.";
/// What the user is told when the capture code panics (xcap's Windows Graphics Capture setup `expect`s a D3D11
/// device, for one).
const UNAVAILABLE: &str = "Screen capture isn't available on this PC.";
const NOT_VISIBLE: &str = "Open the screen picker from a window that is showing.";

/// Managed state.
#[derive(Default)]
pub struct Screen {
    gate: Mutex<Gate>,
    /// One listing or capture at a time (they toggle the exclusion of Pawl's windows).
    busy: Mutex<()>,
}

/// `screen_capture`'s answer: a `ChatImage` plus its size.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    pub media_type: &'static str,
    /// Base64 of the encoded picture (at most 4 MiB decoded).
    pub data: String,
    pub width: u32,
    pub height: u32,
}

/// Sizes only, never the picture (like `ChatImage`).
impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture")
            .field("media_type", &self.media_type)
            .field("data_len", &self.data.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .finish()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A fresh nonce: 128 bits from std's randomly keyed SipHash (`RandomState`), over a counter and the time.
fn new_nonce() -> String {
    use std::hash::{BuildHasher, Hasher};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut out = String::with_capacity(32);
    for half in 0..2u8 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(n);
        h.write_u128(t);
        h.write_u8(half);
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

/// Pawl's windows (the pet, its bubble, the panel) kept out of screen captures while this lives: Tauri's content
/// protection, which is `WDA_EXCLUDEFROMCAPTURE` on Windows and `NSWindowSharingNone` on macOS. They stay visible
/// on the screen. Linux has no such switch (the pet may be in the picture there).
struct HideOwnWindows<R: Runtime> {
    windows: Vec<WebviewWindow<R>>,
}

/// Whether the exclusion is in place: it must have been posted to the main thread, applied to every window there,
/// and confirmed within `HIDE_WAIT`. Anything else fails closed.
fn hidden_in_time(posted: bool, reply: Result<bool, RecvTimeoutError>) -> Result<(), String> {
    match (posted, reply) {
        (true, Ok(true)) => Ok(()),
        _ => Err(HIDE_FAILED.into()),
    }
}

impl<R: Runtime> HideOwnWindows<R> {
    /// Hide now, or fail: then nothing may be captured.
    fn new(app: &AppHandle<R>) -> Result<Self, String> {
        if !cfg!(any(windows, target_os = "macos")) {
            return Ok(Self { windows: Vec::new() });
        }
        let windows: Vec<WebviewWindow<R>> = app.webview_windows().into_values().collect();
        // From here on, dropping `guard` restores every window, also after a failure. Restoring is harmless, and if
        // the main thread runs the apply late, the restore is queued behind it (one event queue), so nothing stays
        // protected.
        let guard = Self { windows };
        if guard.windows.is_empty() {
            return Ok(guard);
        }
        // On the main thread the setter applies at once; wait for it there rather than guessing a delay.
        let (tx, rx) = mpsc::channel();
        let ws = guard.windows.clone();
        let posted = app
            .run_on_main_thread(move || {
                let mut all = true;
                for w in &ws {
                    all &= w.set_content_protected(true).is_ok();
                }
                let _ = tx.send(all);
            })
            .is_ok();
        hidden_in_time(posted, if posted { rx.recv_timeout(HIDE_WAIT) } else { Err(RecvTimeoutError::Disconnected) })?;
        std::thread::sleep(HIDE_SETTLE);
        Ok(guard)
    }
}

impl<R: Runtime> Drop for HideOwnWindows<R> {
    fn drop(&mut self) {
        for w in &self.windows {
            let _ = w.set_content_protected(false);
        }
    }
}

/// Capture work (xcap, the image codecs) with a panic turned into `UNAVAILABLE`; only the panic's message is
/// logged, never image data. Runs on the blocking worker thread.
fn no_panic<T>(f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    crate::panics::guarded("screen capture", UNAVAILABLE, f)
}

/// Listing, given how to hide Pawl's windows and how to list: the session must allow it, the windows must be
/// hidden (or nothing is listed), and the ids offered are remembered for `capture_flow`.
fn list_flow<G>(
    screen: &Screen,
    nonce: &str,
    who: &str,
    hide: impl FnOnce() -> Result<G, String>,
    list: impl FnOnce() -> Result<Vec<Source>, String>,
) -> Result<Vec<Source>, String> {
    lock(&screen.gate).start_listing(nonce, who, Instant::now())?;
    permission::ensure()?;
    let _busy = lock(&screen.busy);
    let listed = {
        let _hidden = hide()?;
        no_panic(list)?
    };
    let ids: HashSet<SourceId> = listed.iter().filter_map(|s| SourceId::parse(&s.id)).collect();
    lock(&screen.gate).listed(nonce, who, ids, Instant::now());
    Ok(listed)
}

/// Capturing, likewise: an id from the session's list, Pawl's windows hidden for a screen (or no capture), then
/// scaled and encoded.
fn capture_flow<G>(
    screen: &Screen,
    nonce: &str,
    who: &str,
    id: SourceId,
    hide: impl FnOnce() -> Result<G, String>,
    capture: impl FnOnce() -> Result<RgbaImage, String>,
) -> Result<Capture, String> {
    lock(&screen.gate).start_capture(nonce, who, id, Instant::now())?;
    permission::ensure()?;
    let _busy = lock(&screen.busy);
    let img = {
        let _hidden = match id {
            SourceId::Screen(_) => Some(hide()?),
            SourceId::Window(_) => None, // a window capture shows only that window
        };
        no_panic(capture)?
    };
    let encoded = no_panic(|| encode::for_chat(&img, encode::PLATFORM_ALPHA))?;
    drop(img);
    Ok(Capture {
        media_type: encoded.media_type.as_str(),
        data: encode::base64(&encoded.bytes),
        width: encoded.width,
        height: encoded.height,
    })
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

/// The picker opened: returns the nonce `screen_sources` and `screen_capture` need (valid 2 minutes, for this
/// webview only; a new one replaces the old). Only from a window that is showing; at most once a second and 20
/// times in 10 minutes.
#[tauri::command]
pub async fn screen_begin<R: Runtime>(app: AppHandle<R>, webview: Webview<R>) -> Result<String, String> {
    blocking(move || {
        if !webview.window().is_visible().unwrap_or(false) {
            return Err(NOT_VISIBLE.into());
        }
        let nonce = new_nonce();
        lock(&app.state::<Screen>().gate).begin(nonce.clone(), webview.label(), Instant::now())?;
        Ok(nonce)
    })
    .await
}

/// The picker closed (sent or cancelled): its nonce stops working.
#[tauri::command]
pub fn screen_end<R: Runtime>(app: AppHandle<R>, webview: Webview<R>, nonce: String) {
    lock(&app.state::<Screen>().gate).end(&nonce, webview.label());
}

/// The screen picker: every screen, then the other apps' windows (front to back), each with a small thumbnail.
/// Pawl's own windows and minimized or invisible ones are not listed. Needs the picker's nonce; at most every 2 s,
/// 5 times per picker. macOS: asks for the Screen Recording permission first (an error says what to do).
#[tauri::command]
pub async fn screen_sources<R: Runtime>(app: AppHandle<R>, webview: Webview<R>, nonce: String) -> Result<Vec<Source>, String> {
    let who = webview.label().to_string();
    blocking(move || {
        let screen = app.state::<Screen>();
        list_flow(&screen, &nonce, &who, || HideOwnWindows::new(&app), || {
            sources::list(std::process::id(), encode::PLATFORM_ALPHA)
        })
    })
    .await
}

/// Capture the source the user picked (an id from this picker's `screen_sources`), now: scaled to at most 2048 px,
/// JPEG (quality 85) unless PNG is smaller, at most 4 MiB. In memory only. At most every second, 3 times per picker.
#[tauri::command]
pub async fn screen_capture<R: Runtime>(
    app: AppHandle<R>,
    webview: Webview<R>,
    nonce: String,
    id: String,
) -> Result<Capture, String> {
    let who = webview.label().to_string();
    blocking(move || {
        let id = SourceId::parse(&id).ok_or("That is not a screen or window.")?;
        let screen = app.state::<Screen>();
        capture_flow(&screen, &nonce, &who, id, || HideOwnWindows::new(&app), || sources::capture(id, std::process::id()))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const PET: &str = "pet";

    fn source(id: &str) -> Source {
        Source { id: id.into(), kind: sources::SourceKind::Screen, title: "Screen 1".into(), app: String::new(), thumb: String::new() }
    }

    /// A picker that is open and has listed `ids` (with the windows "hidden" by a no-op).
    fn opened(ids: &[&str]) -> (Screen, String) {
        let screen = Screen::default();
        let nonce = new_nonce();
        lock(&screen.gate).begin(nonce.clone(), PET, Instant::now()).unwrap();
        let list: Vec<Source> = ids.iter().map(|id| source(id)).collect();
        list_flow(&screen, &nonce, PET, || Ok(()), || Ok(list)).unwrap();
        (screen, nonce)
    }

    fn tiny() -> RgbaImage {
        RgbaImage::from_pixel(8, 8, image::Rgba([10, 20, 30, 255]))
    }

    #[test]
    fn nothing_is_listed_or_captured_when_pawl_cannot_be_hidden() {
        let screen = Screen::default();
        let nonce = new_nonce();
        lock(&screen.gate).begin(nonce.clone(), PET, Instant::now()).unwrap();
        let listed = Cell::new(false);
        let r = list_flow(&screen, &nonce, PET, || Err::<(), _>(HIDE_FAILED.to_string()), || {
            listed.set(true);
            Ok(vec![])
        });
        assert_eq!(r.unwrap_err(), HIDE_FAILED);
        assert!(!listed.get(), "fail closed: no thumbnails taken");

        let (screen, nonce) = opened(&["screen:1", "window:9"]);
        let captured = Cell::new(false);
        let r = capture_flow(&screen, &nonce, PET, SourceId::Screen(1), || Err::<(), _>(HIDE_FAILED.to_string()), || {
            captured.set(true);
            Ok(tiny())
        });
        assert_eq!(r.unwrap_err(), HIDE_FAILED);
        assert!(!captured.get(), "fail closed: no screen capture");
        // A window capture shows only that window: it does not need the exclusion.
        std::thread::sleep(Duration::from_millis(1100));
        let c = capture_flow(&screen, &nonce, PET, SourceId::Window(9), || Err::<(), _>(HIDE_FAILED.to_string()), || Ok(tiny()))
            .unwrap();
        assert_eq!((c.width, c.height), (8, 8));
    }

    #[test]
    fn the_exclusion_must_be_confirmed_in_time() {
        assert!(hidden_in_time(true, Ok(true)).is_ok());
        assert_eq!(hidden_in_time(true, Ok(false)).unwrap_err(), HIDE_FAILED, "a window refused");
        assert_eq!(hidden_in_time(true, Err(RecvTimeoutError::Timeout)).unwrap_err(), HIDE_FAILED, "main thread busy");
        assert_eq!(hidden_in_time(true, Err(RecvTimeoutError::Disconnected)).unwrap_err(), HIDE_FAILED);
        assert_eq!(hidden_in_time(false, Ok(true)).unwrap_err(), HIDE_FAILED, "could not post");
    }

    #[test]
    fn flows_need_the_pickers_nonce_and_its_list() {
        let screen = Screen::default();
        let r = list_flow(&screen, "guess", PET, || Ok(()), || Ok(vec![source("screen:1")]));
        assert_eq!(r.unwrap_err(), gate::PICK_AGAIN);
        let (screen, nonce) = opened(&["screen:1"]);
        let c = capture_flow(&screen, &nonce, PET, SourceId::Screen(1), || Ok(()), || Ok(tiny())).unwrap();
        assert_eq!(c.media_type, "image/png");
        let r = capture_flow(&screen, &nonce, PET, SourceId::Window(5), || Ok(()), || Ok(tiny()));
        assert_eq!(r.unwrap_err(), gate::NOT_IN_LIST);
        let r = capture_flow(&screen, &nonce, "panel", SourceId::Screen(1), || Ok(()), || Ok(tiny()));
        assert_eq!(r.unwrap_err(), gate::PICK_AGAIN, "another webview");
        lock(&screen.gate).end(&nonce, PET);
        let r = capture_flow(&screen, &nonce, PET, SourceId::Screen(1), || Ok(()), || Ok(tiny()));
        assert_eq!(r.unwrap_err(), gate::PICK_AGAIN, "closed");
    }

    #[test]
    fn nonces_are_long_and_fresh() {
        let a = new_nonce();
        let b = new_nonce();
        assert_eq!(a.len(), 32);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn a_panicking_capture_is_an_error() {
        let r: Result<(), String> = no_panic(|| panic!("Failed to create D3D11 device"));
        assert_eq!(r.unwrap_err(), "Screen capture isn't available on this PC.");
        assert_eq!(no_panic(|| Ok::<_, String>(1)), Ok(1));
        let (screen, nonce) = opened(&["screen:1"]);
        let r = capture_flow(&screen, &nonce, PET, SourceId::Screen(1), || Ok(()), || panic!("driver"));
        assert_eq!(r.unwrap_err(), UNAVAILABLE);
        assert!(screen.busy.try_lock().is_ok(), "the lock is not poisoned");
    }

    #[test]
    fn capture_serializes_like_a_chat_image_and_debug_hides_it() {
        let c = Capture { media_type: "image/jpeg", data: "/9j/4AAQSkZJRg".into(), width: 2048, height: 1152 };
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            serde_json::json!({"mediaType":"image/jpeg","data":"/9j/4AAQSkZJRg","width":2048,"height":1152})
        );
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("/9j/"), "{dbg}");
        assert!(dbg.contains("data_len: 14") && dbg.contains("2048"), "{dbg}");
        assert!(permission::MISSING.contains("Screen & System Audio Recording"));
        assert!(permission::ensure().is_ok() || cfg!(target_os = "macos"));
    }

    /// Real screens and windows (local only: `PAWL_SCREEN_TEST=1`), through the same flows as the commands (the
    /// exclusion of Pawl's windows is a no-op here: there are none). Nothing is saved; only sizes are printed.
    #[test]
    fn real_capture() {
        if std::env::var_os("PAWL_SCREEN_TEST").is_none() {
            eprintln!("skipped: set PAWL_SCREEN_TEST=1");
            return;
        }
        let screen = Screen::default();
        let nonce = new_nonce();
        lock(&screen.gate).begin(nonce.clone(), PET, Instant::now()).unwrap();
        let t0 = Instant::now();
        let list = list_flow(&screen, &nonce, PET, || Ok(()), || sources::list(std::process::id(), encode::PLATFORM_ALPHA))
            .unwrap();
        let screens = list.iter().filter(|s| s.kind == sources::SourceKind::Screen).count();
        eprintln!("{} sources ({screens} screens) in {:?}", list.len(), t0.elapsed());
        assert!(screens >= 1, "at least one screen");
        for s in &list {
            assert!(SourceId::parse(&s.id).is_some(), "{}", s.id);
            assert!(s.thumb.len() < 400 * 1024, "{}: thumb {} bytes", s.id, s.thumb.len());
        }
        let ids: HashSet<_> = list.iter().map(|s| s.id.clone()).collect();
        assert_eq!(ids.len(), list.len(), "unique ids");
        let first_window = list.iter().find(|s| s.kind == sources::SourceKind::Window);
        for s in std::iter::once(&list[0]).chain(first_window) {
            let id = SourceId::parse(&s.id).unwrap();
            let t1 = Instant::now();
            let c = capture_flow(&screen, &nonce, PET, id, || Ok(()), || sources::capture(id, std::process::id())).unwrap();
            eprintln!("{:?}: {:?} in {:?}", s.kind, c, t1.elapsed());
            assert!(c.width.max(c.height) <= encode::MAX_SIDE);
            assert!(c.data.len() <= encode::MAX_BYTES.div_ceil(3) * 4);
            std::thread::sleep(Duration::from_millis(1100)); // the capture rate limit
        }
        // Pawl's own windows (this process) are never offered.
        assert!(sources::capture(SourceId::Window(u32::MAX), std::process::id()).is_err());
    }
}
