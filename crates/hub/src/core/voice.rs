//! Approvals by voice (PLAN.md "Next release (v0.2)" item 3), the hub's side.
//!
//! - Every permission request this hub creates gets a two-digit `voiceCode` (random, not used by
//!   another of its pending requests). It travels with the request to clients and paired hubs and
//!   proves a spoken approval came from someone who can see the screen. It is never stored in event
//!   history and never sent to a model (the brain's context pack leaves it out).
//! - A chat message with `via: "voice"` is a transcript. The brain applies the voice rules
//!   (`pawl_brain::chat::spoken`); the hub checks them again before acting: no "always allow", no
//!   hook-session request, and for risky or remote approvals the transcript must say the code. A right
//!   code on another machine's request is the confirmation on this device (`confirmed: true`).
//! - Per chat, the request the pet just read back is kept for one message; wrong codes are counted and
//!   three in ten minutes pause approving by voice.
//! - Decisions record `via: "voice"` and the transcript (300 characters at most) in the audit log (this
//!   machine's requests) or the intent log (another machine's), and in `permission_resolved`.

use std::collections::{HashMap, VecDeque};

use anyhow::{bail, Result};
use pawl_brain::chat::spoken;
use pawl_protocol::{PermissionOptionKind, PermissionRequest, PetId, RequestId, ResolvedBy, SessionSource, UnixMs};

use super::{AckCode, Actor, Hub};

/// The `via` value of a spoken message or decision.
pub(crate) const VOICE: &str = "voice";
/// Transcript characters kept in the audit and intent logs.
pub(crate) const TRANSCRIPT_CHARS: usize = 300;
/// How long a read-back stays good for the "approve" that follows it.
const READ_BACK_MS: UnixMs = 2 * 60_000;
/// Wrong codes within [`MISS_WINDOW_MS`] that pause approving by voice.
const MAX_MISSES: usize = 3;
const MISS_WINDOW_MS: UnixMs = 10 * 60_000;

#[derive(Default)]
pub(super) struct VoiceState {
    /// Per chat (the pet's id; None = the default pet): the request the last voice reply read back.
    read_back: HashMap<Option<PetId>, (RequestId, UnixMs)>,
    /// When wrong codes were said (the latest few).
    misses: VecDeque<UnixMs>,
}

/// A transcript as the logs keep it (cutting twice changes nothing).
pub(crate) fn cut_transcript(text: &str) -> String {
    let cut: String = text.trim().chars().take(TRANSCRIPT_CHARS).collect();
    cut.trim_end().to_string()
}

/// A random code that none of `taken` uses (10..=99, so no leading zero to say), or None when all are.
fn pick_code<'a>(taken: impl Iterator<Item = &'a str>) -> Option<String> {
    let taken: Vec<&str> = taken.collect();
    let free: Vec<String> = (10..=99u8).map(|n| n.to_string()).filter(|c| !taken.contains(&c.as_str())).collect();
    if free.is_empty() {
        return None;
    }
    let r = u64::from_le_bytes(uuid::Uuid::new_v4().as_bytes()[..8].try_into().expect("8 bytes"));
    Some(free[(r % free.len() as u64) as usize].clone())
}

impl Hub {
    /// Put one of this hub's requests in `pending` with a fresh voice code (any code it came with is
    /// replaced), under one lock so two requests never share one. Returns the stored request.
    pub(super) fn insert_pending(&self, mut req: PermissionRequest) -> PermissionRequest {
        let mut st = self.st();
        let code = pick_code(st.pending.values().filter(|r| r.request_id != req.request_id).filter_map(|r| r.voice_code.as_deref()));
        if code.is_none() {
            tracing::warn!(request = %req.request_id, "no free voice code: this request is answered on the screen only");
        }
        req.voice_code = code;
        st.pending.insert(req.request_id.clone(), req.clone());
        req
    }

    /// The read-back of the chat's previous voice reply, if it is recent (taken: it is good for one message).
    pub(super) fn take_read_back(&self, pet: Option<&PetId>, now: UnixMs) -> Option<RequestId> {
        let mut v = self.inner.voice.lock().unwrap_or_else(|p| p.into_inner());
        v.read_back.retain(|_, (_, at)| now - *at <= READ_BACK_MS);
        v.read_back.remove(&pet.cloned()).map(|(id, _)| id)
    }

    /// Too many wrong codes lately: approving by voice is paused.
    pub(super) fn voice_locked(&self, now: UnixMs) -> bool {
        let mut v = self.inner.voice.lock().unwrap_or_else(|p| p.into_inner());
        while v.misses.front().is_some_and(|t| now - *t > MISS_WINDOW_MS) {
            v.misses.pop_front();
        }
        v.misses.len() >= MAX_MISSES
    }

    /// Remember what a voice reply read back, and count a wrong code.
    pub(super) fn note_voice_reply(&self, pet: Option<&PetId>, read_back: Option<RequestId>, code_miss: bool, now: UnixMs) {
        let mut v = self.inner.voice.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(id) = read_back {
            v.read_back.insert(pet.cloned(), (id, now));
        }
        if code_miss {
            v.misses.push_back(now);
            while v.misses.len() > MAX_MISSES {
                v.misses.pop_front();
            }
            tracing::info!("voice approval: a spoken code matched no request");
        }
    }

    /// Answer a request from a voice transcript (the brain's action), checking the voice rules again:
    /// never "always allow" or a hook session's request; a risky or remote approval needs the
    /// request's code in the transcript. Another machine's request goes to its owner as an intent
    /// (an approval confirmed by the code). Returns the request's session when it was this machine's.
    pub(super) async fn resolve_by_voice(&self, request_id: &str, option_id: Option<String>, actor: &Actor, transcript: &str) -> Result<Option<String>> {
        let local = self.st().pending.get(request_id).cloned();
        let req = match &local {
            Some(r) => r.clone(),
            None => self.mirror_pending().into_iter().find(|r| r.request_id == request_id).ok_or(AckCode::NotFound)?,
        };
        let kind = match &option_id {
            Some(o) => Some(req.options.iter().find(|x| &x.option_id == o).map(|x| x.kind).ok_or(AckCode::Invalid)?),
            None => None,
        };
        if kind == Some(PermissionOptionKind::AllowAlways) {
            bail!("“always allow” is not available by voice");
        }
        if self.on_hook_session(&req) {
            bail!("a hook session's request is answered in its terminal");
        }
        let approve = matches!(kind, Some(PermissionOptionKind::AllowOnce));
        let owner = self.foreign_owner(request_id);
        if approve && spoken::needs_code(owner.is_none(), req.tool_call.kind) {
            if self.voice_locked(pawl_protocol::now_ms()) {
                bail!("approving by voice is paused after too many wrong codes");
            }
            if !req.voice_code.as_deref().is_some_and(|c| spoken::says_code(transcript, c)) {
                bail!("the spoken code does not match the request");
            }
        }
        let actor = Actor { label: format!("{VOICE}:{}", actor.label), kind: actor.kind };
        let transcript = cut_transcript(transcript);
        match owner {
            Some(m) => {
                // The right code is the confirmation on this device; the owner's rules still apply.
                let confirmed = approve.then_some(true);
                self.resolve_remote_as(&m, request_id, option_id, confirmed, &actor, Some(&transcript)).await?;
                Ok(None)
            }
            None => {
                self.decide_as(request_id, option_id, ResolvedBy::User, &actor, None, Some(&transcript)).await?;
                Ok(Some(req.session_id))
            }
        }
    }

    fn on_hook_session(&self, req: &PermissionRequest) -> bool {
        let hook = |s: SessionSource| matches!(s, SessionSource::VscodeHook | SessionSource::CliHook);
        if let Some(s) = self.st().sessions.get(&req.session_id) {
            return hook(s.source);
        }
        self.mirror().sessions.get(&req.session_id).is_some_and(|s| hook(s.source))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_two_digits_and_never_taken() {
        for _ in 0..200 {
            let c = pick_code(["10", "11", "47"].into_iter()).unwrap();
            assert!(spoken::is_voice_code(&c) && !c.starts_with('0'), "{c}");
            assert!(!["10", "11", "47"].contains(&c.as_str()));
        }
        let all: Vec<String> = (10..=98u8).map(|n| n.to_string()).collect();
        assert_eq!(pick_code(all.iter().map(String::as_str)), Some("99".into()), "the only free one");
        let all: Vec<String> = (10..=99u8).map(|n| n.to_string()).collect();
        assert_eq!(pick_code(all.iter().map(String::as_str)), None);
        assert_eq!(cut_transcript(&"x".repeat(400)).len(), TRANSCRIPT_CHARS);
        let once = cut_transcript(&"ab ".repeat(200));
        assert_eq!(cut_transcript(&once), once);
    }
}
