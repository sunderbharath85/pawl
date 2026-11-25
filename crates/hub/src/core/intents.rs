//! Intents (PLAN step 4): act on another machine's session from here.
//!
//! **Origin** (the hub whose client asked). A ClientMsg naming another machine's id
//! (resolve_permission, prompt, cancel by id prefix; start_session by `machineId`) becomes an
//! `intent` to that machine, the id's owner, never anyone else. Approving and starting need the
//! client's `confirmed`, checked here before anything is sent. The client's ack is the owner's
//! answer, or `offline` / `timeout` / `not_found` from here. Every forwarded intent is logged.
//!
//! **Owner** (the hub that has the session). An `intent` is accepted only from a paired hub over its
//! link (`on_peer_body` checks that). It is deduped by intent id (the same ack again, nothing
//! re-applied), rate-limited per peer, refused wholesale when `[peer] accept_intents = false`, and
//! validated against this hub's own state. It is then applied through the same code paths as a local
//! ClientMsg: decisions through [`Hub::decide`] (first valid decision wins), prompt/cancel/start
//! through the local functions. Rules never re-run: the user decided. Every intent is logged; remote
//! approve/deny are also in the permission audit log with their origin.

use std::collections::{HashMap, VecDeque};
use std::path::Path;

use pawl_a2a::Caller;
use pawl_peer::SendError;
#[cfg(not(test))]
use pawl_protocol::INTENT_TIMEOUT_MS;
use pawl_protocol::{DeviceKind, Intent, IntentRejectReason, IntentStatus, PeerBody, INTENT_RATE_PER_SEC};
use tokio::sync::oneshot;

use super::*;
use crate::store::{IntentLogEntry, IntentRole};

/// Owner: answered intents remembered for dedupe.
const ACK_CACHE_MAX: usize = 1000;
const ACK_CACHE_TTL: Duration = Duration::from_secs(600);
/// Prompts longer than this are refused (local and remote).
pub const MAX_PROMPT_CHARS: usize = 100_000;
/// An owner's intent_ack result larger than this (serialized) is refused by the origin.
const ACK_RESULT_MAX_BYTES: usize = 64 * 1024;
/// How long the origin waits for an intent_ack.
#[cfg(not(test))]
const INTENT_TIMEOUT: Duration = Duration::from_millis(INTENT_TIMEOUT_MS as u64);
#[cfg(test)]
pub(super) const INTENT_TIMEOUT: Duration = Duration::from_millis(400);

/// An intent's answer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Ack {
    pub status: IntentStatus,
    pub reason: Option<IntentRejectReason>,
    pub result: Option<Value>,
}

impl Ack {
    fn applied(result: Value) -> Self {
        Ack { status: IntentStatus::Applied, reason: None, result: Some(result) }
    }

    fn rejected(reason: IntentRejectReason) -> Self {
        Ack { status: IntentStatus::Rejected, reason: Some(reason), result: None }
    }
}

/// One peer's answered intents (owner side), oldest first.
#[derive(Default)]
struct PeerAcks {
    acks: HashMap<String, (tokio::time::Instant, Option<Ack>)>,
    order: VecDeque<String>,
}

impl PeerAcks {
    fn expire(&mut self) {
        let now = tokio::time::Instant::now();
        while let Some(k) = self.order.front() {
            let old = self.acks.get(k).is_none_or(|(t, _)| now.duration_since(*t) > ACK_CACHE_TTL);
            if self.order.len() > ACK_CACHE_MAX || old {
                let k = self.order.pop_front().expect("front exists");
                self.acks.remove(&k);
            } else {
                break;
            }
        }
    }
}

#[derive(Default)]
pub(super) struct IntentState {
    /// Origin: intents waiting for their ack, by intent id, with the device each went to.
    waiting: HashMap<String, (String, oneshot::Sender<Ack>)>,
    /// Owner: per origin device, intents by id: None while being handled, then the ack. Each peer
    /// has its own ACK_CACHE_MAX, so one peer cannot push another's acks out.
    acks: HashMap<String, PeerAcks>,
    /// Owner: intents per peer in the current one-second window.
    rate: HashMap<String, (tokio::time::Instant, u32)>,
    /// Owner: when a flood-type rejection (rate_limited, forbidden by config) was last logged per
    /// peer, and how many were not logged since.
    quiet: HashMap<(String, &'static str), (Option<tokio::time::Instant>, u32)>,
    /// Tests: ignore every incoming intent (so the origin times out).
    #[cfg(test)]
    pub(super) drop_all: bool,
}

impl IntentState {
    fn get(&mut self, peer: &str, id: &str) -> Option<Option<Ack>> {
        let p = self.acks.get_mut(peer)?;
        p.expire();
        p.acks.get(id).map(|(_, a)| a.clone())
    }

    fn remember(&mut self, peer: &str, id: &str, ack: Option<Ack>) {
        let p = self.acks.entry(peer.to_string()).or_default();
        if p.acks.insert(id.to_string(), (tokio::time::Instant::now(), ack)).is_none() {
            p.order.push_back(id.to_string());
        }
        p.expire();
    }

    fn forget(&mut self, peer: &str, id: &str) {
        if let Some(p) = self.acks.get_mut(peer) {
            p.acks.remove(id);
            p.order.retain(|k| k != id);
        }
    }

    /// Log this flood-type rejection now? Some(n) = yes, n similar ones were skipped since the last.
    fn should_log(&mut self, peer: &str, what: &'static str) -> Option<u32> {
        let now = tokio::time::Instant::now();
        let e = self.quiet.entry((peer.to_string(), what)).or_insert((None, 0));
        match e.0 {
            Some(t) if now.duration_since(t) < Duration::from_secs(60) => {
                e.1 = e.1.saturating_add(1);
                None
            }
            _ => {
                let n = e.1;
                *e = (Some(now), 0);
                Some(n)
            }
        }
    }
}

/// `allow_*` option kinds.
pub(crate) fn is_allow(kind: PermissionOptionKind) -> bool {
    matches!(kind, PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways)
}

pub(super) fn kind_name(i: &Intent) -> &'static str {
    match i {
        Intent::Approve { .. } => "approve",
        Intent::Deny { .. } => "deny",
        Intent::Prompt { .. } => "prompt",
        Intent::Start { .. } => "start",
        Intent::Cancel { .. } => "cancel",
    }
}

/// What an intent acts on, for the log (never the prompt text).
pub(super) fn target_of(i: &Intent) -> (String, Option<String>) {
    match i {
        Intent::Approve { request_id, option_id } => (request_id.clone(), Some(option_id.clone())),
        Intent::Deny { request_id, option_id } => (request_id.clone(), option_id.clone()),
        Intent::Prompt { session_id, .. } | Intent::Cancel { session_id } => (session_id.clone(), None),
        Intent::Start { agent_id, .. } => (agent_id.clone(), None),
    }
}

pub(super) fn reason_name(r: IntentRejectReason) -> String {
    serde_json::to_value(r).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// The client-facing code for an owner's reject reason.
pub(super) fn reason_code(r: IntentRejectReason) -> AckCode {
    match r {
        IntentRejectReason::AlreadyDecided => AckCode::AlreadyDecided,
        IntentRejectReason::NotFound => AckCode::NotFound,
        IntentRejectReason::NotOwner => AckCode::NotOwner,
        IntentRejectReason::Forbidden => AckCode::Forbidden,
        IntentRejectReason::Unconfirmed => AckCode::Unconfirmed,
        IntentRejectReason::Invalid => AckCode::Invalid,
        IntentRejectReason::RateLimited => AckCode::RateLimited,
    }
}

/// The reject reason for a local error while applying an intent.
pub(super) fn reason_of(e: &anyhow::Error) -> IntentRejectReason {
    match AckCode::find(e) {
        Some(AckCode::NotOwner) => IntentRejectReason::NotOwner,
        Some(AckCode::NotFound) => IntentRejectReason::NotFound,
        Some(AckCode::AlreadyDecided) => IntentRejectReason::AlreadyDecided,
        Some(AckCode::Unconfirmed) => IntentRejectReason::Unconfirmed,
        Some(AckCode::Forbidden) => IntentRejectReason::Forbidden,
        Some(AckCode::RateLimited) => IntentRejectReason::RateLimited,
        _ => IntentRejectReason::Invalid,
    }
}

/// Prompt text rules shared by local prompts and intents.
pub(crate) fn validate_prompt(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Err(anyhow::Error::new(AckCode::Invalid).context("prompt text is empty"));
    }
    if text.chars().count() > MAX_PROMPT_CHARS {
        return Err(anyhow::Error::new(AckCode::Invalid).context(format!("prompt is longer than {MAX_PROMPT_CHARS} characters")));
    }
    Ok(())
}

/// The origin's check of an owner's `applied` result: a start must return a session of the target
/// machine (bounded size; an optional warning is kept, cleaned); every other kind returns `{}`.
pub(crate) fn checked_result(kind: &str, target: &str, result: Option<Value>) -> std::result::Result<Value, AckCode> {
    if kind != "start" {
        return Ok(json!({}));
    }
    let v = result.ok_or(AckCode::Invalid)?;
    if serde_json::to_vec(&v).map_or(usize::MAX, |b| b.len()) > ACK_RESULT_MAX_BYTES {
        return Err(AckCode::Invalid);
    }
    let session: Session = v.get("session").cloned().and_then(|s| serde_json::from_value(s).ok()).ok_or(AckCode::Invalid)?;
    let owned = parse_global_id(&session.id).is_some_and(|(m, _)| m == target);
    if session.machine_id != target || !owned {
        return Err(AckCode::Invalid);
    }
    let mut out = json!({ "session": session });
    if let Some(w) = v.get("warning").and_then(Value::as_str) {
        let clean: String = w.chars().filter(|c| !c.is_control()).take(300).collect();
        out["warning"] = Value::String(clean);
    }
    Ok(out)
}

impl Hub {
    pub(super) fn intents(&self) -> MutexGuard<'_, IntentState> {
        self.inner.intents.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The other machine `id` belongs to (None for this machine's ids and unprefixed ids).
    pub(super) fn foreign_owner(&self, id: &str) -> Option<String> {
        parse_global_id(id).map(|(m, _)| m.to_string()).filter(|m| *m != self.inner.machine_id)
    }

    pub(super) fn peer_name(&self, machine_id: &str) -> Option<String> {
        self.devices().into_iter().find(|d| d.id == machine_id).map(|d| d.name)
    }

    pub(super) async fn log_intent(&self, entry: IntentLogEntry) {
        if let Err(e) = self.inner.store.run(move |s| s.insert_intent_log(&entry)).await {
            tracing::error!("intent log write failed: {e:#}");
        }
    }

    // ------------------------------------------------------------ origin

    /// ClientMsg resolve_permission for another machine's request: approve (allow option; needs
    /// `confirmed`) or deny (reject option or none). The option kind comes from the mirrored request;
    /// an option of a request the mirror does not know counts as an approval (it needs confirming).
    pub(super) async fn resolve_remote(
        &self,
        target: &str,
        request_id: &str,
        option_id: Option<String>,
        confirmed: Option<bool>,
        actor: &Actor,
    ) -> Result<Value> {
        let known = self.mirror_pending().into_iter().find(|r| r.request_id == request_id);
        let approve = match (&option_id, &known) {
            (None, _) => false,
            (Some(o), Some(req)) => match req.options.iter().find(|x| &x.option_id == o) {
                Some(x) => is_allow(x.kind),
                None => return Err(AckCode::Invalid.into()),
            },
            (Some(_), None) => true,
        };
        let intent = if approve {
            if confirmed != Some(true) {
                return Err(AckCode::Unconfirmed.into());
            }
            Intent::Approve { request_id: request_id.to_string(), option_id: option_id.unwrap_or_default() }
        } else {
            Intent::Deny { request_id: request_id.to_string(), option_id }
        };
        self.forward(target, actor, confirmed == Some(true), intent).await
    }

    /// start_session on another machine (needs `confirmed`).
    pub(super) async fn start_remote(
        &self,
        target: &str,
        agent_id: &str,
        cwd: &str,
        prompt: Option<String>,
        confirmed: Option<bool>,
        actor: &Actor,
    ) -> Result<Value> {
        if confirmed != Some(true) {
            return Err(AckCode::Unconfirmed.into());
        }
        let intent = Intent::Start { agent_id: agent_id.to_string(), cwd: cwd.to_string(), prompt };
        self.forward(target, actor, true, intent).await
    }

    /// Send `intent` to `target` (the id's owner) and wait for its ack. A v9 owner gets start /
    /// prompt / cancel as A2A over the link instead (`core/a2a_peer.rs`); approve / deny stay intents.
    pub(super) async fn forward(&self, target: &str, actor: &Actor, confirmed: bool, intent: Intent) -> Result<Value> {
        let paired = self.devices().iter().any(|d| d.id == target && d.kind == DeviceKind::Hub);
        if !paired {
            return Err(AckCode::NotFound.into());
        }
        if matches!(intent, Intent::Start { .. } | Intent::Prompt { .. } | Intent::Cancel { .. }) && self.peer_speaks_a2a(target) {
            return self.forward_a2a(target, actor, confirmed, &intent).await;
        }
        let client = actor.kind.unwrap_or(ClientKind::Cli);
        let (kind, (tgt, option)) = (kind_name(&intent), target_of(&intent));
        let intent_id = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.intents().waiting.insert(intent_id.clone(), (target.to_string(), tx));
        let body =
            PeerBody::Intent { intent_id: intent_id.clone(), client, confirmed, intent, ts: pawl_protocol::now_ms() };
        let outcome: std::result::Result<Ack, AckCode> = match self.inner.peer.send(target, body) {
            Err(SendError::NotLinked) | Err(SendError::Backpressure) => Err(AckCode::Offline),
            Err(e) => {
                tracing::warn!(device = %target, "intent not sent: {e}");
                Err(AckCode::Invalid)
            }
            Ok(()) => match tokio::time::timeout(INTENT_TIMEOUT, rx).await {
                Ok(Ok(ack)) => Ok(ack),
                _ => Err(AckCode::Timeout),
            },
        };
        self.intents().waiting.remove(&intent_id);
        let (status, reason) = match &outcome {
            Ok(a) if a.status == IntentStatus::Applied => ("applied".to_string(), None),
            Ok(a) => ("rejected".to_string(), a.reason.map(reason_name)),
            Err(code) => (code.to_string(), None),
        };
        tracing::info!(intent = %intent_id, kind, device = %target, status = %status, reason = ?reason, "intent sent");
        self.log_intent(IntentLogEntry {
            id: 0,
            ts: pawl_protocol::now_ms(),
            role: IntentRole::Origin,
            intent_id,
            kind: kind.to_string(),
            peer_machine_id: target.to_string(),
            peer_name: self.peer_name(target),
            client: client_label(client).to_string(),
            target: Some(tgt),
            option_id: option,
            status,
            reason,
            repeats: 0,
        })
        .await;
        match outcome {
            Ok(Ack { status: IntentStatus::Applied, result, .. }) => checked_result(kind, target, result).map_err(Into::into),
            Ok(Ack { reason, .. }) => Err(reason_code(reason.unwrap_or(IntentRejectReason::Invalid)).into()),
            Err(code) => Err(code.into()),
        }
    }

    /// An `intent_ack` from `from`: hand it to the waiting request (only if it went to `from`).
    pub(super) fn on_intent_ack(&self, from: &str, intent_id: &str, ack: Ack) {
        let waiter = {
            let mut s = self.intents();
            match s.waiting.get(intent_id) {
                Some((t, _)) if t == from => s.waiting.remove(intent_id),
                _ => None,
            }
        };
        match waiter {
            Some((_, tx)) => {
                let _ = tx.send(ack);
            }
            None => tracing::debug!(device = %from, "ignoring an intent_ack nobody is waiting for"),
        }
    }

    /// `list_agents{machineId}` for a paired hub: the agents of its latest presence, or the last
    /// ones it reported (offline).
    pub(super) async fn peer_agents(&self, machine_id: &str) -> Result<Vec<Agent>> {
        if !self.devices().iter().any(|d| d.id == machine_id && d.kind == DeviceKind::Hub) {
            return Err(AckCode::NotFound.into());
        }
        if let Some(a) = self.presence_agents(machine_id) {
            return Ok(a);
        }
        let id = machine_id.to_string();
        Ok(self.inner.store.run(move |s| s.device_agents(&id)).await?.unwrap_or_default())
    }

    // ------------------------------------------------------------ owner

    /// Where a remote start may run. The cwd must be absolute and exist (`invalid` otherwise); it is
    /// canonicalized, network-share and device paths are `forbidden`, and the result must be a known
    /// folder ([`Hub::is_known_folder`]: in this machine's repo index or under `[peer] start_roots`).
    /// Anything else is `forbidden`.
    pub(super) async fn remote_start_dir(&self, cwd: &str) -> std::result::Result<std::path::PathBuf, IntentRejectReason> {
        let raw = cwd.trim();
        if raw.starts_with("\\\\") || raw.starts_with("//") {
            return Err(IntentRejectReason::Forbidden);
        }
        let p = Path::new(raw);
        if !p.is_absolute() {
            return Err(IntentRejectReason::Invalid);
        }
        let canon = std::fs::canonicalize(p).map_err(|_| IntentRejectReason::Invalid)?;
        if !canon.is_dir() {
            return Err(IntentRejectReason::Invalid);
        }
        let dir = crate::repos::plain_path(&canon).ok_or(IntentRejectReason::Forbidden)?;
        if !self.is_known_folder(&dir).await {
            return Err(IntentRejectReason::Forbidden);
        }
        Ok(dir)
    }

    /// More than INTENT_RATE_PER_SEC intents from `from` within one second?
    pub(super) fn intent_rate_ok(&self, from: &str) -> bool {
        let now = tokio::time::Instant::now();
        let mut s = self.intents();
        let w = s.rate.entry(from.to_string()).or_insert((now, 0));
        if now.duration_since(w.0) >= Duration::from_secs(1) {
            *w = (now, 0);
        }
        w.1 = w.1.saturating_add(1);
        w.1 <= INTENT_RATE_PER_SEC
    }

    fn send_ack(&self, to: &str, intent_id: &str, ack: &Ack) {
        let body = PeerBody::IntentAck {
            intent_id: intent_id.to_string(),
            status: ack.status,
            reason: ack.reason,
            result: ack.result.clone(),
        };
        if let Err(e) = self.inner.peer.send(to, body) {
            tracing::warn!(device = %to, intent = %intent_id, "intent_ack not sent: {e}");
        }
    }

    /// An `intent` from paired hub `from` (already authenticated as linked and paired). Handled off
    /// the peer loop (a start may take a while); the ack goes back on the link.
    pub(super) fn on_intent(&self, from: &str, intent_id: String, client: ClientKind, confirmed: bool, intent: Intent) {
        #[cfg(test)]
        if self.intents().drop_all {
            return;
        }
        let hub = self.clone();
        let from = from.to_string();
        tokio::spawn(async move {
            if let Some(ack) = hub.handle_intent(&from, &intent_id, client, confirmed, &intent).await {
                hub.send_ack(&from, &intent_id, &ack);
            }
        });
    }

    /// The owner's whole answer to one intent: dedupe, `forbidden`, rate limit, validate, apply,
    /// log. None = a repeat of an intent still being handled (its first copy will be answered).
    pub(super) async fn handle_intent(
        &self,
        from: &str,
        intent_id: &str,
        client: ClientKind,
        confirmed: bool,
        intent: &Intent,
    ) -> Option<Ack> {
        // Checked and marked under one lock: a repeat can never run the intent a second time.
        let seen = {
            let mut s = self.intents();
            match s.get(from, intent_id) {
                Some(ack) => Some(ack),
                None => {
                    s.remember(from, intent_id, None);
                    None
                }
            }
        };
        match seen {
            // A repeat: the same answer, nothing re-applied.
            Some(Some(ack)) => return Some(ack),
            Some(None) => {
                tracing::debug!(device = %from, intent = %intent_id, "repeat of an intent still being handled");
                return None;
            }
            None => {}
        }
        let refused = if !self.cfg().peer.accept_intents {
            Some(IntentRejectReason::Forbidden)
        } else if !self.intent_rate_ok(from) {
            Some(IntentRejectReason::RateLimited)
        } else {
            None
        };
        let ack = match refused {
            Some(reason) => Ack::rejected(reason),
            None => match self.apply_intent(from, intent_id, client, confirmed, intent).await {
                Ok(v) => Ack::applied(v),
                Err(r) => Ack::rejected(r),
            },
        };
        // Flood-type refusals are logged once per peer per minute (the rest are counted); every
        // other answer is logged.
        let repeats = {
            let mut s = self.intents();
            if ack.reason == Some(IntentRejectReason::RateLimited) {
                // A rate-limited intent may be retried with the same id.
                s.forget(from, intent_id);
            } else {
                s.remember(from, intent_id, Some(ack.clone()));
            }
            match refused {
                Some(IntentRejectReason::RateLimited) => s.should_log(from, "rate_limited"),
                Some(IntentRejectReason::Forbidden) => s.should_log(from, "forbidden"),
                _ => Some(0),
            }
        };
        if let Some(repeats) = repeats {
            self.log_owner(from, intent_id, client, intent, &ack, repeats).await;
        }
        Some(ack)
    }

    async fn log_owner(&self, from: &str, intent_id: &str, client: ClientKind, intent: &Intent, ack: &Ack, repeats: u32) {
        let (tgt, option) = target_of(intent);
        let status = if ack.status == IntentStatus::Applied { "applied" } else { "rejected" };
        tracing::info!(intent = %intent_id, kind = kind_name(intent), device = %from, status, reason = ?ack.reason, "intent handled");
        self.log_intent(IntentLogEntry {
            id: 0,
            ts: pawl_protocol::now_ms(),
            role: IntentRole::Owner,
            intent_id: intent_id.to_string(),
            kind: kind_name(intent).to_string(),
            peer_machine_id: from.to_string(),
            peer_name: self.peer_name(from),
            client: client_label(client).to_string(),
            target: Some(tgt),
            option_id: option,
            status: status.to_string(),
            reason: ack.reason.map(reason_name),
            repeats,
        })
        .await;
    }

    /// Validate an intent against this hub's own state and apply it like a local ClientMsg.
    async fn apply_intent(
        &self,
        from: &str,
        intent_id: &str,
        client: ClientKind,
        confirmed: bool,
        intent: &Intent,
    ) -> std::result::Result<Value, IntentRejectReason> {
        let who = self.peer_name(from).unwrap_or_else(|| from.to_string());
        let actor = Actor { label: format!("{}@{who}", client_label(client)), kind: Some(client) };
        let owned = |id: &str| self.check_owned(id).map_err(|e| reason_of(&e));
        match intent {
            Intent::Approve { request_id, option_id } => {
                owned(request_id)?;
                if !confirmed {
                    return Err(IntentRejectReason::Unconfirmed);
                }
                self.remote_decision(from, intent_id, client, &actor, request_id, Some(option_id.clone()), true).await?;
                Ok(json!({}))
            }
            Intent::Deny { request_id, option_id } => {
                owned(request_id)?;
                self.remote_decision(from, intent_id, client, &actor, request_id, option_id.clone(), false).await?;
                Ok(json!({}))
            }
            Intent::Prompt { session_id, text } => {
                owned(session_id)?;
                validate_prompt(text).map_err(|e| reason_of(&e))?;
                self.owned_open_session(session_id).map_err(|e| reason_of(&e))?;
                // A2A delegation for the peer: from now on a human answers every request of this
                // session (the facade marks it remote-driven; rules never auto-approve).
                let req = super::a2a::prompt_request(session_id, text);
                self.a2a_send(req, &Caller::Peer(from.to_string()), &actor, super::a2a::Answers::Never)
                    .await
                    .map_err(|e| reason_of(&e))?;
                Ok(json!({}))
            }
            Intent::Cancel { session_id } => {
                owned(session_id)?;
                self.cancel_as(session_id, &actor, &Caller::Peer(from.to_string())).await.map_err(|e| reason_of(&e))?;
                Ok(json!({}))
            }
            Intent::Start { agent_id, cwd, prompt } => {
                if !confirmed {
                    return Err(IntentRejectReason::Unconfirmed);
                }
                let installed = self.st().agents.iter().any(|a| &a.id == agent_id && a.installed);
                if !installed {
                    return Err(IntentRejectReason::Invalid);
                }
                let dir = self.remote_start_dir(cwd).await?;
                if let Some(t) = prompt.as_deref().filter(|t| !t.trim().is_empty()) {
                    validate_prompt(t).map_err(|e| reason_of(&e))?;
                }
                // A2A delegation for the peer: the facade marks the session remote-driven before its
                // first prompt can raise a request.
                let caller = Caller::Peer(from.to_string());
                let d = self.a2a_start(agent_id, &dir.to_string_lossy(), prompt.clone(), &caller).await.map_err(|e| {
                    tracing::warn!(device = %from, "remote start of {agent_id} failed: {e:#}");
                    IntentRejectReason::Invalid
                })?;
                let (session, warning) = (d.session.ok_or(IntentRejectReason::Invalid)?, d.warning);
                Ok(match warning {
                    Some(w) => json!({ "session": session, "warning": w }),
                    None => json!({ "session": session }),
                })
            }
        }
    }

    /// Approve/deny from another machine through [`Hub::decide`]; an applied approval is announced
    /// on this machine ("Approved from Mac mini: run `bun test`").
    #[allow(clippy::too_many_arguments)]
    async fn remote_decision(
        &self,
        from: &str,
        intent_id: &str,
        client: ClientKind,
        actor: &Actor,
        request_id: &str,
        option_id: Option<String>,
        approve: bool,
    ) -> std::result::Result<(), IntentRejectReason> {
        let name = self.peer_name(from);
        let pending = self.st().pending.get(request_id).map(|r| {
            (r.session_id.clone(), r.tool_call.title.clone().unwrap_or_else(|| "a tool call".into()))
        });
        let remote = RemoteDecision {
            approve,
            origin: AuditOrigin {
                machine_id: from.to_string(),
                name: name.clone(),
                intent_id: intent_id.to_string(),
                client: client_label(client).to_string(),
            },
        };
        self.decide(request_id, option_id, ResolvedBy::User, actor, Some(remote)).await.map_err(|e| reason_of(&e))?;
        if let (true, Some((session_id, tool))) = (approve, pending) {
            let who = name.unwrap_or_else(|| from.to_string());
            let notification = notify::make(
                &self.inner.machine_id,
                NotificationLevel::Info,
                format!("Approved from {who}"),
                notify::truncate(&tool, 120),
                Some(session_id),
                None,
            );
            self.inner.bus.send(ServerMsg::Notification { notification });
        }
        Ok(())
    }
}

#[cfg(test)]
impl Hub {
    /// Tests: stop answering intents (the origin then times out).
    pub(super) fn drop_intents(&self, on: bool) {
        self.intents().drop_all = on;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ack(n: u32) -> Ack {
        Ack::applied(json!({ "n": n }))
    }

    /// L7: each peer has its own ACK_CACHE_MAX, so a busy peer cannot evict another's acks.
    #[test]
    fn the_dedupe_cache_is_per_peer() {
        let mut s = IntentState::default();
        s.remember("aaaaaa111111", "keep", Some(ack(0)));
        for i in 0..(ACK_CACHE_MAX + 50) {
            s.remember("bbbbbb222222", &format!("b{i}"), Some(ack(1)));
        }
        assert_eq!(s.get("aaaaaa111111", "keep"), Some(Some(ack(0))), "another peer's flood never evicts it");
        assert_eq!(s.get("bbbbbb222222", "b0"), None, "the flooding peer's own oldest go");
        assert!(s.get("bbbbbb222222", &format!("b{}", ACK_CACHE_MAX + 49)).is_some());
        assert_eq!(s.acks["bbbbbb222222"].order.len(), ACK_CACHE_MAX);
        assert_eq!(s.get("aaaaaa111111", "other"), None);
        s.forget("aaaaaa111111", "keep");
        assert_eq!(s.get("aaaaaa111111", "keep"), None);
    }

    #[test]
    fn flood_logging_is_once_a_minute_per_peer() {
        let mut s = IntentState::default();
        assert_eq!(s.should_log("aaaaaa111111", "rate_limited"), Some(0));
        for _ in 0..5 {
            assert_eq!(s.should_log("aaaaaa111111", "rate_limited"), None);
        }
        assert_eq!(s.should_log("aaaaaa111111", "forbidden"), Some(0), "per reason");
        assert_eq!(s.should_log("bbbbbb222222", "rate_limited"), Some(0), "per peer");
        // A minute later the skipped ones are reported with the next entry.
        s.quiet.get_mut(&("aaaaaa111111".to_string(), "rate_limited")).unwrap().0 =
            Some(tokio::time::Instant::now() - Duration::from_secs(61));
        assert_eq!(s.should_log("aaaaaa111111", "rate_limited"), Some(5));
    }
}
