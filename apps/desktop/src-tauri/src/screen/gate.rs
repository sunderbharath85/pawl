//! The picker handshake. Opening the picker, the UI calls `screen_begin`, which hands out a nonce for that webview,
//! valid for 2 minutes; `screen_sources` and `screen_capture` require it (from the same webview), and a capture only
//! takes an id from that session's own list. Each step is rate-limited, so page code can't take pictures in a loop.
//!
//! The nonce is not a secret from the page itself (any code in it could call `screen_begin` too): it ties every
//! listing and capture to an open picker, and the limits bound what a misbehaving page could do.

use super::sources::SourceId;
use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

/// How long a picker session (its nonce) is good for.
pub const SESSION_FOR: Duration = Duration::from_secs(120);
/// `screen_begin`: at most once a second, and at most `BEGINS_PER_WINDOW` per `BEGINS_WINDOW`.
const BEGIN_GAP: Duration = Duration::from_secs(1);
const BEGINS_WINDOW: Duration = Duration::from_secs(10 * 60);
const BEGINS_PER_WINDOW: usize = 20;
/// `screen_sources` (it captures a thumbnail of everything): at most every 2 s, 5 times per session.
const SOURCES_GAP: Duration = Duration::from_secs(2);
const SOURCES_PER_SESSION: u32 = 5;
/// `screen_capture`: at most every second, 3 times per session (a preview and a couple of retakes).
const CAPTURE_GAP: Duration = Duration::from_secs(1);
const CAPTURES_PER_SESSION: u32 = 3;

pub const PICK_AGAIN: &str = "Open the screen picker again.";
pub const NOT_IN_LIST: &str = "Pick the screen or window again.";
pub const SLOW_DOWN: &str = "Too many screen pictures in a short time. Wait a moment, then try again.";

struct Session {
    nonce: String,
    /// The webview that opened the picker (its label).
    who: String,
    began: Instant,
    offered: HashSet<SourceId>,
    listings: u32,
    last_listing: Option<Instant>,
    captures: u32,
    last_capture: Option<Instant>,
}

/// At most one open picker session (a new `screen_begin` replaces it).
#[derive(Default)]
pub struct Gate {
    session: Option<Session>,
    begins: VecDeque<Instant>,
}

/// Equal strings, compared without an early exit.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn too_soon(last: Option<Instant>, gap: Duration, now: Instant) -> bool {
    last.is_some_and(|t| now.saturating_duration_since(t) < gap)
}

impl Gate {
    /// The picker opened in webview `who`: `nonce` is its session from now on.
    pub fn begin(&mut self, nonce: String, who: &str, now: Instant) -> Result<(), String> {
        while self.begins.front().is_some_and(|t| now.saturating_duration_since(*t) >= BEGINS_WINDOW) {
            self.begins.pop_front();
        }
        if too_soon(self.begins.back().copied(), BEGIN_GAP, now) || self.begins.len() >= BEGINS_PER_WINDOW {
            return Err(SLOW_DOWN.into());
        }
        self.begins.push_back(now);
        self.session = Some(Session {
            nonce,
            who: who.to_string(),
            began: now,
            offered: HashSet::new(),
            listings: 0,
            last_listing: None,
            captures: 0,
            last_capture: None,
        });
        Ok(())
    }

    /// The picker closed (sent or cancelled): its nonce stops working.
    pub fn end(&mut self, nonce: &str, who: &str) {
        if self.session.as_ref().is_some_and(|s| same(&s.nonce, nonce) && s.who == who) {
            self.session = None;
        }
    }

    fn live(&mut self, nonce: &str, who: &str, now: Instant) -> Result<&mut Session, String> {
        match &mut self.session {
            Some(s) if same(&s.nonce, nonce) && s.who == who && now.saturating_duration_since(s.began) < SESSION_FOR => {
                Ok(s)
            }
            _ => Err(PICK_AGAIN.into()),
        }
    }

    /// May this session list the sources now? (Counts as one listing.)
    pub fn start_listing(&mut self, nonce: &str, who: &str, now: Instant) -> Result<(), String> {
        let s = self.live(nonce, who, now)?;
        if s.listings >= SOURCES_PER_SESSION || too_soon(s.last_listing, SOURCES_GAP, now) {
            return Err(SLOW_DOWN.into());
        }
        s.listings += 1;
        s.last_listing = Some(now);
        Ok(())
    }

    /// The ids the session's latest list offered (ignored if the session ended meanwhile).
    pub fn listed(&mut self, nonce: &str, who: &str, ids: HashSet<SourceId>, now: Instant) {
        if let Ok(s) = self.live(nonce, who, now) {
            s.offered = ids;
        }
    }

    /// May this session capture `id` now? (Counts as one capture.)
    pub fn start_capture(&mut self, nonce: &str, who: &str, id: SourceId, now: Instant) -> Result<(), String> {
        let s = self.live(nonce, who, now)?;
        if !s.offered.contains(&id) {
            return Err(NOT_IN_LIST.into());
        }
        if s.captures >= CAPTURES_PER_SESSION || too_soon(s.last_capture, CAPTURE_GAP, now) {
            return Err(SLOW_DOWN.into());
        }
        s.captures += 1;
        s.last_capture = Some(now);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PET: &str = "pet";
    const S1: SourceId = SourceId::Screen(1);
    const W9: SourceId = SourceId::Window(9);

    fn opened(now: Instant) -> Gate {
        let mut g = Gate::default();
        g.begin("n1".into(), PET, now).unwrap();
        g.start_listing("n1", PET, now).unwrap();
        g.listed("n1", PET, [S1, W9].into_iter().collect(), now);
        g
    }

    #[test]
    fn nothing_without_an_open_picker() {
        let now = Instant::now();
        let mut g = Gate::default();
        assert_eq!(g.start_listing("guess", PET, now).unwrap_err(), PICK_AGAIN);
        assert_eq!(g.start_capture("guess", PET, S1, now).unwrap_err(), PICK_AGAIN);
        g.begin("n1".into(), PET, now).unwrap();
        assert_eq!(g.start_listing("n2", PET, now).unwrap_err(), PICK_AGAIN, "wrong nonce");
        assert_eq!(g.start_listing("n1", "panel", now).unwrap_err(), PICK_AGAIN, "another webview");
        assert_eq!(g.start_capture("n1", PET, S1, now).unwrap_err(), NOT_IN_LIST, "not listed yet");
        g.start_listing("n1", PET, now).unwrap();
    }

    #[test]
    fn a_capture_takes_only_ids_from_its_own_list() {
        let now = Instant::now();
        let mut g = opened(now);
        g.start_capture("n1", PET, W9, now).unwrap();
        assert_eq!(g.start_capture("n1", PET, SourceId::Window(10), now + CAPTURE_GAP).unwrap_err(), NOT_IN_LIST);
        // A new listing replaces the offer.
        let later = now + SOURCES_GAP;
        g.start_listing("n1", PET, later).unwrap();
        g.listed("n1", PET, [SourceId::Window(10)].into_iter().collect(), later);
        assert_eq!(g.start_capture("n1", PET, W9, later).unwrap_err(), NOT_IN_LIST);
        g.start_capture("n1", PET, SourceId::Window(10), later).unwrap();
    }

    #[test]
    fn sessions_expire_end_and_are_replaced() {
        let now = Instant::now();
        let mut g = opened(now);
        g.start_capture("n1", PET, S1, now + SESSION_FOR - Duration::from_secs(1)).unwrap();
        assert_eq!(g.start_capture("n1", PET, S1, now + SESSION_FOR).unwrap_err(), PICK_AGAIN, "2 minutes");

        let mut g = opened(now);
        g.end("n1", "panel"); // another webview can't close it
        g.start_capture("n1", PET, S1, now).unwrap();
        g.end("n1", PET);
        assert_eq!(g.start_capture("n1", PET, S1, now + CAPTURE_GAP).unwrap_err(), PICK_AGAIN, "ended");

        let mut g = opened(now);
        g.begin("n2".into(), PET, now + BEGIN_GAP).unwrap();
        assert_eq!(g.start_capture("n1", PET, S1, now + BEGIN_GAP).unwrap_err(), PICK_AGAIN, "replaced");
        assert_eq!(g.start_capture("n2", PET, S1, now + BEGIN_GAP).unwrap_err(), NOT_IN_LIST, "a new list is needed");
    }

    #[test]
    fn every_step_is_rate_limited() {
        let t0 = Instant::now();
        let mut g = Gate::default();
        g.begin("a".into(), PET, t0).unwrap();
        assert_eq!(g.begin("b".into(), PET, t0 + Duration::from_millis(500)).unwrap_err(), SLOW_DOWN);
        // At most 20 begins in 10 minutes.
        let mut g = Gate::default();
        for i in 0..BEGINS_PER_WINDOW as u32 {
            g.begin(format!("n{i}"), PET, t0 + BEGIN_GAP * i).unwrap();
        }
        let next = t0 + BEGIN_GAP * BEGINS_PER_WINDOW as u32;
        assert_eq!(g.begin("x".into(), PET, next).unwrap_err(), SLOW_DOWN);
        g.begin("x".into(), PET, t0 + BEGINS_WINDOW + Duration::from_secs(1)).unwrap();

        // Listings: every 2 s, 5 per session.
        let mut g = Gate::default();
        g.begin("n".into(), PET, t0).unwrap();
        g.start_listing("n", PET, t0).unwrap();
        assert_eq!(g.start_listing("n", PET, t0 + Duration::from_secs(1)).unwrap_err(), SLOW_DOWN);
        for i in 1..SOURCES_PER_SESSION {
            g.start_listing("n", PET, t0 + SOURCES_GAP * i).unwrap();
        }
        assert_eq!(g.start_listing("n", PET, t0 + SOURCES_GAP * 10).unwrap_err(), SLOW_DOWN);

        // Captures: every second, 3 per session.
        let mut g = opened(t0);
        g.start_capture("n1", PET, S1, t0).unwrap();
        assert_eq!(g.start_capture("n1", PET, S1, t0 + Duration::from_millis(300)).unwrap_err(), SLOW_DOWN);
        g.start_capture("n1", PET, S1, t0 + CAPTURE_GAP).unwrap();
        g.start_capture("n1", PET, W9, t0 + CAPTURE_GAP * 2).unwrap();
        assert_eq!(g.start_capture("n1", PET, S1, t0 + CAPTURE_GAP * 5).unwrap_err(), SLOW_DOWN);
    }

    #[test]
    fn nonces_compare_exactly() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));
        assert!(!same("", "a"));
    }
}
