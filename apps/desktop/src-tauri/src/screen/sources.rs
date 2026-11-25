//! What can be shown: the screens and the other apps' windows (xcap), and capturing one of them by id.
//!
//! Left out: Pawl's own windows (the pet and the panel), minimized and invisible windows, windows without a title
//! (tool windows, the taskbar), tiny ones, and macOS's own system surfaces. xcap already skips invisible and cloaked
//! windows (Windows: other virtual desktops) and lists only on-screen ones (macOS).

use super::encode::{self, Alpha};
use image::RgbaImage;
use serde::Serialize;
use std::fmt;
use xcap::{Monitor, Window};

/// At most this many windows in the picker (front to back), so listing stays quick.
pub const MAX_WINDOWS: usize = 40;
/// Smaller windows are not offered (menu bar items, tooltips, helper windows).
const MIN_WINDOW_SIDE: u32 = 64;
/// macOS processes whose windows are part of the desktop, not apps.
const MACOS_SYSTEM_OWNERS: &[&str] =
    &["Window Server", "Dock", "Control Center", "SystemUIServer", "Notification Center", "Spotlight"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Screen,
    Window,
}

/// An entry in the picker (`screen_sources`).
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub id: String,
    pub kind: SourceKind,
    pub title: String,
    pub app: String,
    /// Base64 PNG, long side at most 240 px.
    pub thumb: String,
}

/// Without the thumbnail or the window title (which can name a private document): their lengths only.
impl fmt::Debug for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Source")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("app", &self.app)
            .field("title_len", &self.title.len())
            .field("thumb_len", &self.thumb.len())
            .finish()
    }
}

/// `screen:<n>` or `window:<n>` (xcap's ids: the monitor handle / the window id).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceId {
    Screen(u32),
    Window(u32),
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceId::Screen(n) => write!(f, "screen:{n}"),
            SourceId::Window(n) => write!(f, "window:{n}"),
        }
    }
}

impl SourceId {
    pub fn parse(s: &str) -> Option<SourceId> {
        let (kind, n) = s.split_once(':')?;
        // Digits only (`u32::from_str` would also take a leading '+').
        if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n = n.parse().ok()?;
        match kind {
            "screen" => Some(SourceId::Screen(n)),
            "window" => Some(SourceId::Window(n)),
            _ => None,
        }
    }
}

/// What the filter needs to know about a window.
/// (No `Debug`: the title can name a private document.)
#[derive(Clone, Default)]
pub struct WindowInfo {
    pub pid: u32,
    pub title: String,
    pub app: String,
    pub width: u32,
    pub height: u32,
    pub minimized: bool,
}

impl WindowInfo {
    fn read(w: &Window) -> Option<WindowInfo> {
        Some(WindowInfo {
            pid: w.pid().ok()?,
            title: w.title().unwrap_or_default().trim().to_string(),
            app: w.app_name().unwrap_or_default().trim().to_string(),
            width: w.width().ok()?,
            height: w.height().ok()?,
            minimized: w.is_minimized().unwrap_or(true),
        })
    }

    /// Whether the picker offers this window (and a capture of it is allowed).
    pub fn offered(&self, own_pid: u32) -> bool {
        self.pid != own_pid
            && !self.minimized
            && !self.title.is_empty()
            && self.width >= MIN_WINDOW_SIDE
            && self.height >= MIN_WINDOW_SIDE
            && !(cfg!(target_os = "macos") && MACOS_SYSTEM_OWNERS.contains(&self.app.as_str()))
    }
}

fn xcap_err(e: xcap::XCapError) -> String {
    format!("Could not read the screen: {e}")
}

/// A screen's name in the picker: "Screen 1", with the monitor's own name when it has a useful one.
fn screen_title(n: usize, friendly: &str) -> String {
    let friendly = friendly.trim();
    if friendly.is_empty() || friendly.starts_with("Unknown Monitor") {
        format!("Screen {n}")
    } else {
        format!("Screen {n} ({friendly})")
    }
}

/// Screens (the main one first), then windows front to back, each with a thumbnail. A window whose thumbnail can't
/// be taken is left out (it could not be captured either).
pub fn list(own_pid: u32, alpha: Alpha) -> Result<Vec<Source>, String> {
    let mut monitors = Monitor::all().map_err(xcap_err)?;
    monitors.sort_by_key(|m| !m.is_primary().unwrap_or(false));
    let mut out = Vec::new();
    for (i, m) in monitors.iter().enumerate() {
        let Ok(id) = m.id() else { continue };
        let thumb = m
            .capture_image()
            .map_err(xcap_err)
            .and_then(|img| encode::thumbnail(&img, alpha))
            .map(|png| encode::base64(&png))
            .unwrap_or_default();
        out.push(Source {
            id: SourceId::Screen(id).to_string(),
            kind: SourceKind::Screen,
            title: screen_title(i + 1, &m.friendly_name().unwrap_or_default()),
            app: String::new(),
            thumb,
        });
    }
    let windows = Window::all().map_err(xcap_err)?;
    let mut listed = 0;
    for w in windows {
        if listed >= MAX_WINDOWS {
            break;
        }
        let Some(info) = WindowInfo::read(&w) else { continue };
        if !info.offered(own_pid) {
            continue;
        }
        let Ok(id) = w.id() else { continue };
        let Ok(img) = w.capture_image() else { continue };
        let Ok(png) = encode::thumbnail(&img, alpha) else { continue };
        drop(img);
        listed += 1;
        out.push(Source {
            id: SourceId::Window(id).to_string(),
            kind: SourceKind::Window,
            title: info.title,
            app: info.app,
            thumb: encode::base64(&png),
        });
    }
    Ok(out)
}

/// Capture a screen or a window now (full size). A window must still pass the picker's filter.
pub fn capture(id: SourceId, own_pid: u32) -> Result<RgbaImage, String> {
    match id {
        SourceId::Screen(n) => {
            let m = Monitor::all()
                .map_err(xcap_err)?
                .into_iter()
                .find(|m| m.id().ok() == Some(n))
                .ok_or("That screen is not connected any more.")?;
            m.capture_image().map_err(xcap_err)
        }
        SourceId::Window(n) => {
            let w = Window::all()
                .map_err(xcap_err)?
                .into_iter()
                .find(|w| w.id().ok() == Some(n))
                .ok_or("That window was closed.")?;
            let info = WindowInfo::read(&w).ok_or("That window was closed.")?;
            if info.minimized {
                return Err("That window is minimized. Restore it, then try again.".into());
            }
            if !info.offered(own_pid) {
                return Err("That window can't be shown.".into());
            }
            w.capture_image().map_err(xcap_err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_reject_junk() {
        for id in [SourceId::Screen(0), SourceId::Screen(65537), SourceId::Window(u32::MAX)] {
            assert_eq!(SourceId::parse(&id.to_string()), Some(id));
        }
        assert_eq!(SourceId::parse("window:42"), Some(SourceId::Window(42)));
        for bad in ["", "screen", "screen:", "screen:+1", "screen:-1", "screen:1a", "tab:1", "window:4294967296", " window:1", "window:1 "] {
            assert_eq!(SourceId::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_filter_leaves_out_pawl_minimized_untitled_and_tiny_windows() {
        let ok = WindowInfo { pid: 7, title: "Report.docx - Word".into(), app: "Microsoft Word".into(), width: 1200, height: 800, minimized: false };
        assert!(ok.offered(1));
        assert!(!ok.offered(7), "Pawl's own windows (same process)");
        assert!(!WindowInfo { minimized: true, ..ok.clone() }.offered(1));
        assert!(!WindowInfo { title: String::new(), ..ok.clone() }.offered(1), "no title: taskbar, tool windows");
        assert!(!WindowInfo { width: 40, ..ok.clone() }.offered(1));
        assert!(!WindowInfo { height: 20, ..ok.clone() }.offered(1));
        let dock = WindowInfo { app: "Dock".into(), title: "Dock".into(), ..ok.clone() };
        assert_eq!(dock.offered(1), !cfg!(target_os = "macos"));
    }

    #[test]
    fn screen_titles() {
        assert_eq!(screen_title(1, "DELL U2720Q"), "Screen 1 (DELL U2720Q)");
        assert_eq!(screen_title(2, ""), "Screen 2");
        assert_eq!(screen_title(1, "Unknown Monitor 65537"), "Screen 1");
    }

    #[test]
    fn source_serializes_like_the_contract() {
        let s = Source {
            id: "window:5".into(),
            kind: SourceKind::Window,
            title: "Notes".into(),
            app: "Notepad".into(),
            thumb: "iVBO".into(),
        };
        assert_eq!(
            serde_json::to_value(&s).unwrap(),
            serde_json::json!({"id":"window:5","kind":"window","title":"Notes","app":"Notepad","thumb":"iVBO"})
        );
        let dbg = format!("{s:?}");
        assert!(!dbg.contains("iVBO") && !dbg.contains("Notes"), "{dbg}");
        assert!(dbg.contains("thumb_len: 4") && dbg.contains("title_len: 5") && dbg.contains("window:5"), "{dbg}");
    }
}

