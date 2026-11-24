//! Watchdog: nudges the user when an agent has been waiting on them too long.
//!
//! Three kinds of waits are tracked:
//! - a pending permission request older than `waiting_nudge_after_ms` (Alert; repeats
//!   every `repeat_every_ms`, or never when that is `<= 0`);
//! - a session in `waiting_permission` without a pending request object, e.g. a hook
//!   session whose approval lives in VS Code or a terminal (Alert; repeats likewise);
//! - a live session whose turn finished (`done`) and that has been waiting for the next
//!   prompt (Info; one nudge per turn, ignored once older than [`STALE_DONE_MS`]).
//!
//! State is pruned every tick, so resolved requests and resumed sessions are forgotten.
//!
//! Only this machine's sessions and requests are nudged (the owning hub nudges; peers get the
//! notification through the event stream), so mirrored remote sessions passed in are skipped.

use std::collections::{HashMap, HashSet};

use pawl_protocol::{
    global_id, HubNotification, MachineId, NotificationLevel, PermissionRequest, Session, SessionSource,
    SessionStatus, UnixMs,
};

use crate::util::{basename, fmt_duration, prettify_agent_id, truncate};

pub const DEFAULT_WAITING_NUDGE_AFTER_MS: i64 = 5 * 60_000;
pub const DEFAULT_REPEAT_EVERY_MS: i64 = 10 * 60_000;
/// Finished turns older than this are not nudged (e.g. sessions loaded after a restart).
pub const STALE_DONE_MS: i64 = 12 * 3_600_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WatchdogConfig {
    /// Nudge once a wait exceeds this (default 5 min).
    pub waiting_nudge_after_ms: i64,
    /// Repeat permission nudges this often; `<= 0` disables repeats (default 10 min).
    pub repeat_every_ms: i64,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self { waiting_nudge_after_ms: DEFAULT_WAITING_NUDGE_AFTER_MS, repeat_every_ms: DEFAULT_REPEAT_EVERY_MS }
    }
}

#[derive(Debug, Clone, Copy)]
struct Nudged {
    last_at: UnixMs,
    count: u32,
}

/// Remembers what it already nudged.
#[derive(Debug, Clone)]
pub struct Watchdog {
    cfg: WatchdogConfig,
    machine_id: MachineId,
    nudged: HashMap<String, Nudged>,
}

impl Watchdog {
    /// `machine_id`: this hub's machine; stamped on (and prefixed to the id of) every notification.
    pub fn new(cfg: WatchdogConfig, machine_id: MachineId) -> Self {
        Self { cfg, machine_id, nudged: HashMap::new() }
    }

    pub fn config(&self) -> WatchdogConfig {
        self.cfg
    }

    /// Apply new timings (e.g. after a settings change) without forgetting what was already nudged:
    /// a request or turn nudged under the old timings is not nudged again just because the threshold
    /// changed; repeats follow the new interval, counted from the last nudge.
    pub fn set_config(&mut self, cfg: WatchdogConfig) {
        self.cfg = cfg;
    }

    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }

    /// Call periodically (e.g. every 15-30 s). Returns the nudges to emit now.
    pub fn tick(&mut self, sessions: &[Session], pending: &[PermissionRequest], now: UnixMs) -> Vec<HubNotification> {
        let threshold = self.cfg.waiting_nudge_after_ms.max(0);
        let me = self.machine_id.clone();
        let sessions: Vec<&Session> = sessions.iter().filter(|s| s.machine_id == me).collect();
        let pending: Vec<&PermissionRequest> = pending.iter().filter(|r| r.machine_id == me).collect();
        let by_id: HashMap<&str, &Session> = sessions.iter().map(|s| (s.id.as_str(), *s)).collect();
        let pending_sessions: HashSet<&str> = pending.iter().map(|r| r.session_id.as_str()).collect();
        let mut live: HashSet<String> = HashSet::new();
        let mut out = Vec::new();

        // 1. Pending permission requests (owned sessions).
        let mut reqs = pending;
        reqs.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.request_id.cmp(&b.request_id)));
        for req in reqs {
            let session = by_id.get(req.session_id.as_str()).copied();
            if session.is_some_and(|s| s.ended_at.is_some()) {
                continue;
            }
            let key = format!("perm:{}", req.request_id);
            live.insert(key.clone());
            let waited = now - req.created_at;
            if waited < threshold {
                continue;
            }
            if let Some(count) = self.due(&key, now, true) {
                out.push(permission_nudge(&me, req, session, waited, count, &key, now));
            }
        }

        let mut ordered: Vec<&Session> = sessions.into_iter().filter(|s| s.ended_at.is_none()).collect();
        ordered.sort_by(|a, b| a.last_event_at.cmp(&b.last_event_at).then_with(|| a.id.cmp(&b.id)));
        for s in ordered {
            let waited = now - s.last_event_at;
            match s.status {
                // 2. Waiting on an approval we have no request object for (hook sessions).
                SessionStatus::WaitingPermission if !pending_sessions.contains(s.id.as_str()) => {
                    let key = format!("wait:{}:{}", s.id, s.last_event_at);
                    live.insert(key.clone());
                    if waited < threshold {
                        continue;
                    }
                    if let Some(count) = self.due(&key, now, true) {
                        out.push(external_wait_nudge(&me, s, waited, count, &key, now));
                    }
                }
                // 3. Turn finished, waiting for the next prompt: one nudge per turn.
                SessionStatus::Done => {
                    if waited > STALE_DONE_MS {
                        continue;
                    }
                    let key = format!("done:{}:{}", s.id, s.last_event_at);
                    live.insert(key.clone());
                    if waited < threshold {
                        continue;
                    }
                    if let Some(count) = self.due(&key, now, false) {
                        out.push(done_nudge(&me, s, waited, count, &key, now));
                    }
                }
                _ => {}
            }
        }

        self.nudged.retain(|k, _| live.contains(k));
        out
    }

    /// Returns the nudge count (1-based) if `key` should be nudged now, recording it.
    fn due(&mut self, key: &str, now: UnixMs, repeat: bool) -> Option<u32> {
        let repeat_every = self.cfg.repeat_every_ms;
        match self.nudged.get_mut(key) {
            None => {
                self.nudged.insert(key.to_string(), Nudged { last_at: now, count: 1 });
                Some(1)
            }
            Some(n) if repeat && repeat_every > 0 && now - n.last_at >= repeat_every => {
                n.last_at = now;
                n.count += 1;
                Some(n.count)
            }
            Some(_) => None,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn notification(
    machine_id: &str,
    key: &str,
    count: u32,
    level: NotificationLevel,
    title: String,
    body: String,
    session_id: Option<&str>,
    agent_id: &str,
    now: UnixMs,
) -> HubNotification {
    HubNotification {
        id: global_id(machine_id, format!("watchdog:{key}:{count}")),
        machine_id: machine_id.to_string(),
        level,
        title,
        body,
        session_id: session_id.map(str::to_string),
        agent_id: Some(agent_id.to_string()),
        ts: now,
    }
}

fn permission_nudge(
    machine_id: &str,
    req: &PermissionRequest,
    session: Option<&Session>,
    waited: i64,
    count: u32,
    key: &str,
    now: UnixMs,
) -> HubNotification {
    let agent = prettify_agent_id(&req.agent_id);
    let what = req
        .tool_call
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| truncate(t, 80))
        .unwrap_or_else(|| "a tool call".into());
    let place = session.map(|s| format!(" in {}", basename(&s.cwd))).unwrap_or_default();
    let still = if count > 1 { "Still waiting" } else { "Waiting" };
    let body = format!("{still} {} for your OK{place}: {what}. Woof!", fmt_duration(waited));
    notification(
        machine_id,
        key,
        count,
        NotificationLevel::Alert,
        format!("{agent} needs your OK"),
        body,
        Some(&req.session_id),
        &req.agent_id,
        now,
    )
}

fn external_wait_nudge(machine_id: &str, s: &Session, waited: i64, count: u32, key: &str, now: UnixMs) -> HubNotification {
    let agent = prettify_agent_id(&s.agent_id);
    let where_ = match s.source {
        SessionSource::VscodeHook => " Approve it in VS Code.",
        SessionSource::CliHook => " Approve it in its terminal.",
        SessionSource::Owned | SessionSource::Wrapper => "",
    };
    let still = if count > 1 { "still " } else { "" };
    let body = format!(
        "{agent} in {} has {still}been waiting {} for your approval.{where_} Woof!",
        basename(&s.cwd),
        fmt_duration(waited)
    );
    notification(machine_id, key, count, NotificationLevel::Alert, format!("{agent} needs your OK"), body, Some(&s.id), &s.agent_id, now)
}

fn done_nudge(machine_id: &str, s: &Session, waited: i64, count: u32, key: &str, now: UnixMs) -> HubNotification {
    let agent = prettify_agent_id(&s.agent_id);
    let body = format!(
        "Finished its turn in {} {} ago and is ready for the next task. *wags tail*",
        basename(&s.cwd),
        fmt_duration(waited)
    );
    notification(machine_id, key, count, NotificationLevel::Info, format!("{agent} is waiting for you"), body, Some(&s.id), &s.agent_id, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawl_protocol::{PermissionOption, PermissionOptionKind, ToolCallPayload};

    const MIN: i64 = 60_000;
    const ME: &str = "abcdef012345";
    const PEER: &str = "zz9988776655";

    fn session(id: &str, status: SessionStatus, last: i64, source: SessionSource) -> Session {
        Session {
            id: id.into(),
            machine_id: ME.into(),
            agent_id: "codex".into(),
            acp_session_id: None,
            external_id: None,
            cwd: "/work/ai-pet".into(),
            title: None,
            source,
            status,
            started_at: 0,
            last_event_at: last,
            ended_at: None,
        }
    }

    fn request(id: &str, session_id: &str, created: i64) -> PermissionRequest {
        PermissionRequest {
            request_id: id.into(),
            machine_id: ME.into(),
            session_id: session_id.into(),
            agent_id: "codex".into(),
            tool_call: ToolCallPayload {
                tool_call_id: "t".into(),
                title: Some("Run cargo test".into()),
                ..Default::default()
            },
            options: vec![PermissionOption {
                option_id: "a".into(),
                name: "Allow".into(),
                kind: PermissionOptionKind::AllowOnce,
            }],
            created_at: created,
        }
    }

    fn dog() -> Watchdog {
        Watchdog::new(WatchdogConfig { waiting_nudge_after_ms: 5 * MIN, repeat_every_ms: 10 * MIN }, ME.into())
    }

    #[test]
    fn nudges_pending_request_after_threshold_and_repeats() {
        let mut w = dog();
        let s = vec![session("s1", SessionStatus::WaitingPermission, 0, SessionSource::Owned)];
        let p = vec![request("r1", "s1", 0)];
        assert!(w.tick(&s, &p, 4 * MIN).is_empty());
        let n = w.tick(&s, &p, 5 * MIN);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].level, NotificationLevel::Alert);
        assert_eq!(n[0].session_id.as_deref(), Some("s1"));
        assert_eq!(n[0].machine_id, ME);
        assert_eq!(pawl_protocol::parse_global_id(&n[0].id).map(|(m, _)| m), Some(ME), "{}", n[0].id);
        assert!(n[0].body.contains("Run cargo test"), "{}", n[0].body);
        assert!(n[0].body.contains("ai-pet"));
        // Deduped until the repeat interval passes.
        assert!(w.tick(&s, &p, 6 * MIN).is_empty());
        assert!(w.tick(&s, &p, 14 * MIN).is_empty());
        let again = w.tick(&s, &p, 15 * MIN);
        assert_eq!(again.len(), 1);
        assert_ne!(again[0].id, n[0].id);
        assert!(again[0].body.starts_with("Still waiting"));
    }

    #[test]
    fn set_config_keeps_nudge_state() {
        let mut w = dog();
        let s = vec![
            session("s1", SessionStatus::WaitingPermission, 0, SessionSource::Owned),
            session("s2", SessionStatus::Done, 0, SessionSource::Owned),
        ];
        let p = vec![request("r1", "s1", 0)];
        assert_eq!(w.tick(&s, &p, 5 * MIN).len(), 2, "request + finished turn");
        // Lower threshold, longer repeat: nothing already nudged is nudged again.
        let cfg = WatchdogConfig { waiting_nudge_after_ms: MIN, repeat_every_ms: 30 * MIN };
        w.set_config(cfg);
        assert_eq!(w.config(), cfg);
        assert!(w.tick(&s, &p, 6 * MIN).is_empty());
        // The old 10 min repeat no longer applies; the new one counts from the last nudge (5 + 30).
        assert!(w.tick(&s, &p, 15 * MIN).is_empty());
        assert!(w.tick(&s, &p, 34 * MIN).is_empty());
        let again = w.tick(&s, &p, 35 * MIN);
        assert_eq!(again.len(), 1);
        assert!(again[0].body.starts_with("Still waiting"), "{}", again[0].body);
        // New items use the new threshold.
        let p2 = vec![request("r1", "s1", 0), request("r2", "s1", 35 * MIN)];
        assert!(w.tick(&s, &p2, 35 * MIN + MIN / 2).is_empty());
        assert_eq!(w.tick(&s, &p2, 36 * MIN).len(), 1);
        // Repeats turned off: no more nudges for what was nudged.
        w.set_config(WatchdogConfig { waiting_nudge_after_ms: MIN, repeat_every_ms: 0 });
        assert!(w.tick(&s, &p2, 200 * MIN).is_empty());
    }

    #[test]
    fn no_repeat_when_disabled_and_state_pruned() {
        let mut w = Watchdog::new(WatchdogConfig { waiting_nudge_after_ms: MIN, repeat_every_ms: 0 }, ME.into());
        let s = vec![session("s1", SessionStatus::WaitingPermission, 0, SessionSource::Owned)];
        let p = vec![request("r1", "s1", 0)];
        assert_eq!(w.tick(&s, &p, 2 * MIN).len(), 1);
        assert!(w.tick(&s, &p, 100 * MIN).is_empty());
        // Resolved (session back to work): forgotten. A new request nudges again.
        let busy = vec![session("s1", SessionStatus::Working, 100 * MIN, SessionSource::Owned)];
        assert!(w.tick(&busy, &[], 101 * MIN).is_empty());
        assert!(w.nudged.is_empty());
        let p2 = vec![request("r2", "s1", 101 * MIN)];
        assert!(w.tick(&s, &p2, 101 * MIN).is_empty());
        assert_eq!(w.tick(&s, &p2, 103 * MIN).len(), 1);
    }

    #[test]
    fn hook_session_waiting_without_request_is_nudged_once_per_interval() {
        let mut w = dog();
        let s = vec![session("h1", SessionStatus::WaitingPermission, 0, SessionSource::VscodeHook)];
        let n = w.tick(&s, &[], 6 * MIN);
        assert_eq!(n.len(), 1);
        assert!(n[0].body.contains("VS Code"));
        assert!(w.tick(&s, &[], 7 * MIN).is_empty());
        // With a pending request for the same session only the request is nudged.
        let mut w = dog();
        let s = vec![session("s1", SessionStatus::WaitingPermission, 0, SessionSource::Owned)];
        assert_eq!(w.tick(&s, &[request("r1", "s1", 0)], 6 * MIN).len(), 1);
    }

    #[test]
    fn finished_turn_nudged_once_and_stale_ignored() {
        let mut w = dog();
        let s = vec![session("s1", SessionStatus::Done, 0, SessionSource::CliHook)];
        let n = w.tick(&s, &[], 5 * MIN);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].level, NotificationLevel::Info);
        assert!(w.tick(&s, &[], 30 * MIN).is_empty());
        // A new turn finishing is a new wait.
        let s2 = vec![session("s1", SessionStatus::Done, 40 * MIN, SessionSource::CliHook)];
        assert!(w.tick(&s2, &[], 41 * MIN).is_empty());
        assert_eq!(w.tick(&s2, &[], 46 * MIN).len(), 1);
        // Stale sessions (e.g. loaded after restart) are not nudged.
        let mut w = dog();
        assert!(w.tick(&s, &[], STALE_DONE_MS + MIN).is_empty());
    }

    #[test]
    fn ended_and_busy_sessions_are_ignored() {
        let mut w = dog();
        let mut ended = session("s1", SessionStatus::WaitingPermission, 0, SessionSource::Owned);
        ended.ended_at = Some(1);
        let working = session("s2", SessionStatus::Working, 0, SessionSource::Owned);
        let out = w.tick(&[ended, working], &[request("r1", "s1", 0)], 60 * MIN);
        assert!(out.is_empty());
    }

    #[test]
    fn other_machines_sessions_are_not_nudged() {
        let mut w = dog();
        let mut remote = session("s9", SessionStatus::WaitingPermission, 0, SessionSource::Owned);
        remote.machine_id = PEER.into();
        let mut remote_done = session("s8", SessionStatus::Done, 0, SessionSource::CliHook);
        remote_done.machine_id = PEER.into();
        let mut req = request("r9", "s9", 0);
        req.machine_id = PEER.into();
        assert!(w.tick(&[remote.clone(), remote_done], &[req], 60 * MIN).is_empty());
        // The same shapes on this machine are nudged.
        let local = session("s1", SessionStatus::WaitingPermission, 0, SessionSource::Owned);
        let out = w.tick(&[remote, local], &[request("r1", "s1", 0)], 60 * MIN);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].session_id.as_deref(), Some("s1"));
    }
}
