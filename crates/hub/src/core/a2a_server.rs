//! The localhost A2A server (PLAN.md "Pawl as an A2A agent", phase A2): the orchestrator as an A2A
//! agent for outside callers on this machine.
//!
//! Transport and security (127.0.0.1 only, loopback Host / Origin, bearer tokens, rate limits,
//! 0.3 + 1.0) are `pawl_a2a::server`'s. This module is the policy behind it:
//!
//! - A caller is `Caller::Outside(<id>~<generation>)`: it sees and acts on the contexts it created
//!   only. The generation is random and new whenever the caller is added, so a caller removed and added
//!   again under the same id does not get the old caller's sessions.
//! - A caller whose token is rotated or removed loses its open streams and blocking waits at once.
//! - Per caller at most [`STREAMS_PER_CALLER`] open streams, [`WAITS_PER_CALLER`] blocking waits (more
//!   answer at once) and [`SESSIONS_PER_CALLER`] open sessions.
//! - Errors are generic for outside callers (`to_a2a`); the detail goes to the hub log only.
//! - A message without a context or delegate metadata gets a short answer saying how to delegate.
//! - It starts agents only where a paired machine could (an installed agent, a folder this machine
//!   already knows: its repo index or `[peer] start_roots`), and its sessions are remote-driven (rules
//!   never auto-approve their requests).
//! - It answers permission requests only if the user allowed it for that caller, and then never
//!   "always" and never allow on execute / delete / fetch (see `core/a2a.rs` `Answers::Limited`).
//! - Every call is logged in `a2a_log` as `a2a:<id>` (method, target, outcome; each at most
//!   [`LOG_FIELD_CHARS`] characters; never message text or tokens), and so is every request the guard
//!   refused (`a2a:unknown` before authentication; at most [`REFUSALS_LOGGED_PER_MIN`] rows a minute,
//!   the rest counted); decisions are in the permission audit log with that actor.
//! - `SendMessage` blocks until the task finishes or asks for input (spec 3.2.2) unless the caller
//!   sets `returnImmediately`, for at most [`BLOCKING_WAIT`]; streams follow a task until it ends.

use std::collections::HashMap;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use anyhow::Result;
use futures::StreamExt;
use pawl_a2a::card::{self, CardInfo};
use pawl_a2a::server::{self as srv, A2aHandler, CallerAuth, EventStream, Refusal, RunningServer, ServerState, ServiceParams};
use pawl_a2a::spec::{
    error_code, A2AError, AgentCard, CancelTaskRequest, GetExtendedAgentCardRequest, GetTaskRequest, ListTasksRequest, ListTasksResponse, Message, Part, Role,
    SendMessageRequest, SendMessageResponse, StreamResponse, SubscribeToTaskRequest, Task, TaskState,
};
use pawl_a2a::{Caller, DelegateParams};
use pawl_protocol::{A2aTaskSummary, IntentRejectReason};
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::watch;

use super::a2a::{A2aError, Answers};
use super::{AckCode, Actor, Hub, Inner};
use crate::settings::{self as cfgset, InvalidSetting};
use crate::store::{A2aLogEntry, SettingsAuditKind};

/// Longest a blocking `SendMessage` waits for its task to finish or ask for input.
pub const BLOCKING_WAIT: Duration = Duration::from_secs(60);
/// Open streams per caller (`SendStreamingMessage`, `SubscribeToTask`).
pub const STREAMS_PER_CALLER: usize = 4;
/// Blocking `SendMessage` waits per caller; more answer at once (as with `returnImmediately`).
pub const WAITS_PER_CALLER: usize = 4;
/// Open sessions a caller may have started.
pub const SESSIONS_PER_CALLER: usize = 8;
/// `a2a_log` fields are cut to this many characters.
pub const LOG_FIELD_CHARS: usize = 128;
/// Guard refusals written to `a2a_log` per minute (the rest are counted in the next row).
pub const REFUSALS_LOGGED_PER_MIN: u32 = 30;

/// Per-caller bookkeeping: generations and the open streams / waits.
#[derive(Default)]
pub(crate) struct A2aCallerBook {
    /// caller id -> its random generation (new when the caller is added).
    gens: HashMap<String, String>,
    /// caller key -> (open streams, blocking waits).
    load: HashMap<String, (usize, usize)>,
    /// Refusal log throttle: (window start, rows logged, rows not logged).
    refusals: Option<(Instant, u32, u32)>,
    /// Start cards waiting for the user (request id -> the caller and its waiting start).
    start_cards: HashMap<String, (String, tokio::sync::oneshot::Sender<Option<String>>)>,
}

fn cut(s: &str) -> String {
    s.chars().take(LOG_FIELD_CHARS).collect()
}

/// The running server and its state (one at a time).
#[derive(Default)]
pub(crate) struct A2aRuntime {
    server: Option<(RunningServer, Arc<ServerState>, u16)>,
}

/// Live state for the settings view.
#[derive(Debug, Clone, Default)]
pub(crate) struct A2aStatus {
    pub running: bool,
    /// The bound port while running.
    pub port: u16,
    pub url: Option<String>,
    pub error: Option<String>,
}

fn callers_of(cfg: &crate::config::A2aSection) -> Vec<CallerAuth> {
    cfg.callers
        .iter()
        .filter_map(|c| c.token.as_deref().filter(|t| !t.trim().is_empty()).map(|t| CallerAuth { id: c.id.clone(), token: t.to_string() }))
        .collect()
}

/// A fresh caller token: 32 random bytes, hex.
pub(super) fn new_token() -> Result<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("no randomness for a token: {e}"))?;
    Ok(hex::encode(b))
}

fn ms(ts: Option<chrono::DateTime<chrono::Utc>>) -> Option<i64> {
    ts.map(|t| t.timestamp_millis())
}

impl Hub {
    fn a2a_book(&self) -> std::sync::MutexGuard<'_, A2aCallerBook> {
        self.inner.a2a_callers.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The task owner of caller `id` (its id and current generation).
    fn caller_key(&self, id: &str) -> String {
        let mut book = self.a2a_book();
        let gen = book.gens.entry(id.to_string()).or_insert_with(|| {
            let mut b = [0u8; 8];
            let _ = getrandom::fill(&mut b);
            hex::encode(b)
        });
        format!("{id}~{gen}")
    }

    /// Take caller `id`'s task-owner generation away (its next call starts a new owner). Returns the
    /// old one, for [`Self::put_a2a_generation`] if what needed the change fails.
    pub(super) fn take_a2a_generation(&self, id: &str) -> Option<String> {
        self.a2a_book().gens.remove(id)
    }

    /// A start card of `caller_id` waits for the user's answer on `tx` (core/a2a_assistant.rs). False
    /// (nothing added) when that caller already has [`super::a2a_assistant::MAX_START_CARDS`] waiting.
    pub(super) fn add_start_card(&self, caller_id: &str, request_id: &str, tx: tokio::sync::oneshot::Sender<Option<String>>) -> bool {
        let mut book = self.a2a_book();
        if book.start_cards.values().filter(|(c, _)| c == caller_id).count() >= super::a2a_assistant::MAX_START_CARDS {
            return false;
        }
        book.start_cards.insert(request_id.to_string(), (caller_id.to_string(), tx));
        true
    }

    /// Is `request_id` a start card still waiting?
    pub(crate) fn is_start_card(&self, request_id: &str) -> bool {
        self.a2a_book().start_cards.contains_key(request_id)
    }

    /// Hand the answer (`None`: cancelled) to a waiting start card. False when it is not one.
    pub(super) fn answer_start_card(&self, request_id: &str, option_id: Option<String>) -> bool {
        let Some((_, tx)) = self.a2a_book().start_cards.remove(request_id) else { return false };
        let _ = tx.send(option_id);
        true
    }

    /// Forget a start card (its start stopped waiting).
    pub(super) fn drop_start_card(&self, request_id: &str) {
        self.a2a_book().start_cards.remove(request_id);
    }

    /// Tests: the task-owner key caller `id` has now.
    #[cfg(test)]
    pub(crate) fn a2a_owner_key(&self, id: &str) -> String {
        self.caller_key(id)
    }

    pub(super) fn put_a2a_generation(&self, id: &str, gen: Option<String>) {
        let mut book = self.a2a_book();
        match gen {
            Some(g) => {
                book.gens.insert(id.to_string(), g);
            }
            None => {
                book.gens.remove(id);
            }
        }
    }

    fn a2a_status_lock(&self) -> std::sync::MutexGuard<'_, A2aStatus> {
        self.inner.a2a_status.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The live state of the localhost A2A server.
    pub(crate) fn a2a_status(&self) -> A2aStatus {
        self.a2a_status_lock().clone()
    }

    /// What Pawl's Agent Card says (`extended`: the installed agents and repo names too).
    pub(crate) fn card_info(&self, port: u16, extended: bool) -> CardInfo {
        let (agents, repos) = if extended {
            let agents = self.agents().into_iter().filter(|a| a.installed).map(|a| (a.id, a.name)).collect();
            (agents, self.repos().into_iter().map(|r| r.name).collect())
        } else {
            (Vec::new(), Vec::new())
        };
        CardInfo { version: env!("CARGO_PKG_VERSION").to_string(), base_url: format!("http://127.0.0.1:{port}"), agents, repos }
    }

    /// Make the running server match `[a2a]`: start, stop, move to another port, or only replace
    /// its callers. A start that fails is reported in the settings view (`a2a.error`).
    pub(crate) async fn sync_a2a_server(&self) {
        let cfg = self.cfg().a2a.clone();
        // A removed caller's generation goes: added again, it is a new caller.
        self.a2a_book().gens.retain(|id, _| cfg.callers.iter().any(|c| &c.id == id));
        let mut rt = self.inner.a2a_runtime.lock().await;
        if let Some((_, state, port)) = &rt.server {
            if cfg.server && *port == cfg.port {
                state.set_callers(callers_of(&cfg));
                return;
            }
        }
        if let Some((server, _, port)) = rt.server.take() {
            server.stop().await;
            tracing::info!("A2A server on 127.0.0.1:{port} stopped");
        }
        if !cfg.server {
            *self.a2a_status_lock() = A2aStatus::default();
            return;
        }
        let weak: Weak<Inner> = Arc::downgrade(&self.inner);
        let card_weak = weak.clone();
        let audit_weak = weak.clone();
        let state = ServerState::new(callers_of(&cfg), move || match card_weak.upgrade() {
            Some(inner) => {
                let hub = Hub { inner };
                card::pawl_card(&hub.card_info(hub.a2a_status().port, false), false)
            }
            None => card::pawl_card(&CardInfo::default(), false),
        })
        .with_audit(move |r| {
            if let Some(inner) = audit_weak.upgrade() {
                Hub { inner }.audit_refusal(r);
            }
        });
        match srv::serve(Arc::new(HubHandler { hub: weak, state: state.clone() }), state.clone(), cfg.port).await {
            Ok(server) => {
                let port = server.addr.port();
                tracing::info!("A2A server on 127.0.0.1:{port} ({} caller(s))", cfg.callers.len());
                *self.a2a_status_lock() = A2aStatus { running: true, port, url: Some(format!("http://127.0.0.1:{port}")), error: None };
                rt.server = Some((server, state, port));
            }
            Err(e) => {
                tracing::warn!("A2A server could not listen on 127.0.0.1:{}: {e}", cfg.port);
                *self.a2a_status_lock() =
                    A2aStatus { running: false, port: 0, url: None, error: Some(format!("could not listen on 127.0.0.1:{}: {e}", cfg.port)) };
            }
        }
    }

    /// Stop the server (hub shutdown).
    pub(crate) async fn stop_a2a_server(&self) {
        if let Some((server, _, _)) = self.inner.a2a_runtime.lock().await.server.take() {
            server.stop().await;
        }
        *self.a2a_status_lock() = A2aStatus::default();
    }

    /// `a2a_rotate_token{callerId}`: a new token for an existing caller, saved like a setting (audited
    /// without its value) and returned once. The old token stops working at once.
    pub async fn a2a_rotate_token(&self, caller_id: &str, actor: &Actor) -> Result<Value> {
        let _one = self.inner.settings_write.lock().await;
        let cur = self.config();
        let mut next = cur.clone();
        let Some(c) = next.a2a.callers.iter_mut().find(|c| c.id == caller_id) else {
            return Err(InvalidSetting { field: "callerId".into(), reason: "is not a configured A2A caller".into() }.into());
        };
        if c.is_assistant() {
            return Err(InvalidSetting { field: "callerId".into(), reason: "is Pawl's assistant; it gets a new token when the assistant is reset".into() }.into());
        }
        let token = new_token()?;
        c.token = Some(token.clone());
        self.commit_settings(&cur, next, &[cfgset::A2A_TOKENS], actor, SettingsAuditKind::Settings).await?;
        Ok(json!({ "token": token }))
    }

    /// `list_tasks{sessionId?}`: this machine's delegations, newest first.
    pub fn list_task_summaries(&self, session_id: Option<&str>) -> Vec<A2aTaskSummary> {
        let mut req = ListTasksRequest {
            context_id: session_id.map(str::to_string),
            status: None,
            page_size: Some(pawl_a2a::tasks::LIST_PAGE_MAX as i32),
            page_token: None,
            history_length: Some(0),
            status_timestamp_after: None,
            include_artifacts: Some(false),
            tenant: None,
        };
        let mut out = Vec::new();
        loop {
            let Ok(page) = self.tasks().list(&Caller::Local, &req) else { break };
            for t in &page.tasks {
                out.push(A2aTaskSummary {
                    task_id: t.id.clone(),
                    session_id: t.context_id.clone(),
                    state: pawl_a2a::mapping::state_name(&t.status.state).to_string(),
                    text: t.status.message.as_ref().and_then(|m| m.text()).map(|s| s.chars().take(200).collect()),
                    caller: self.tasks().owner_of(&t.context_id).label(),
                    updated_at: ms(t.status.timestamp),
                });
            }
            if page.next_page_token.is_empty() || out.len() >= 1000 {
                break;
            }
            req.page_token = Some(page.next_page_token);
        }
        out
    }

    async fn log_a2a(&self, caller: &str, method: &str, target: Option<String>, outcome: &Result<(), A2AError>) {
        let status = match outcome {
            Ok(()) => "ok".to_string(),
            Err(e) => pawl_a2a::spec::error_reason(e.code).to_ascii_lowercase(),
        };
        self.write_a2a_log(&format!("a2a:{caller}"), method, target, &status, None).await;
    }

    async fn write_a2a_log(&self, caller: &str, method: &str, target: Option<String>, status: &str, detail: Option<String>) {
        tracing::info!(caller = %cut(caller), method = %cut(method), status = %cut(status), "A2A call");
        let entry = A2aLogEntry {
            id: 0,
            ts: pawl_protocol::now_ms(),
            caller: cut(caller),
            method: cut(method),
            target: target.as_deref().map(cut),
            status: cut(status),
            detail: detail.as_deref().map(cut),
        };
        if let Err(e) = self.inner.store.run(move |s| s.insert_a2a_log(&entry)).await {
            tracing::error!("a2a log write failed: {e:#}");
        }
    }

    /// A request the server's guard refused: an `a2a_log` row, at most REFUSALS_LOGGED_PER_MIN a
    /// minute (a flood is counted, not written row by row).
    pub(crate) fn audit_refusal(&self, r: Refusal) {
        let detail = {
            let mut book = self.a2a_book();
            let now = Instant::now();
            let t = book.refusals.get_or_insert((now, 0, 0));
            if now.duration_since(t.0) >= Duration::from_secs(60) {
                *t = (now, 0, t.2);
            }
            if t.1 >= REFUSALS_LOGGED_PER_MIN {
                t.2 += 1;
                return;
            }
            t.1 += 1;
            let skipped = std::mem::take(&mut t.2);
            (skipped > 0).then(|| format!("{skipped} more refusal(s) not logged before this one"))
        };
        let caller = format!("a2a:{}", r.caller.as_deref().unwrap_or("unknown"));
        let hub = self.clone();
        tokio::spawn(async move { hub.write_a2a_log(&caller, &r.request, None, r.status, detail).await });
    }

    /// Take a stream or wait slot of caller `key` (None when it has its share already).
    fn take_slot(&self, key: &str, stream: bool) -> Option<SlotGuard> {
        let mut book = self.a2a_book();
        let load = book.load.entry(key.to_string()).or_default();
        let (n, max) = if stream { (&mut load.0, STREAMS_PER_CALLER) } else { (&mut load.1, WAITS_PER_CALLER) };
        if *n >= max {
            return None;
        }
        *n += 1;
        Some(SlotGuard { hub: Arc::downgrade(&self.inner), key: key.to_string(), stream })
    }
}

/// A caller's open stream or blocking wait; released when dropped.
pub(super) struct SlotGuard {
    hub: Weak<Inner>,
    key: String,
    stream: bool,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let Some(inner) = self.hub.upgrade() else { return };
        let mut book = inner.a2a_callers.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(load) = book.load.get_mut(&self.key) {
            let n = if self.stream { &mut load.0 } else { &mut load.1 };
            *n = n.saturating_sub(1);
            if *load == (0, 0) {
                book.load.remove(&self.key);
            }
        }
    }
}

/// The hub's error as the A2A error an outside caller gets: Pawl's own sentences (the facade's A2A
/// errors, the outermost text of a refusal), never an inner error's detail (paths, agent output),
/// which goes to the hub log only.
fn to_a2a(e: &anyhow::Error) -> A2AError {
    if let Some(a) = A2aError::find(e) {
        return match a {
            A2aError::TaskNotFound => A2AError::task_not_found("the task does not exist or is not this caller's"),
            A2aError::TaskNotCancelable => A2AError::new(error_code::TASK_NOT_CANCELABLE, "the task has finished"),
            A2aError::Unsupported(m) => A2AError::unsupported_operation(m),
            A2aError::InvalidParams(m) => A2AError::invalid_params(m),
        };
    }
    match AckCode::find(e) {
        Some(AckCode::NotFound | AckCode::NotOwner) => A2AError::invalid_params("unknown context: it does not exist or is not this caller's"),
        Some(AckCode::Forbidden) => A2AError::unsupported_operation(e.to_string()),
        Some(AckCode::AlreadyDecided) => A2AError::unsupported_operation("that request was already answered"),
        Some(AckCode::Invalid) => A2AError::invalid_params(e.to_string()),
        _ => {
            tracing::info!("A2A call refused: {e:#}");
            A2AError::invalid_params("the request could not be done here")
        }
    }
}

fn reject_reason(r: IntentRejectReason, assistant: bool) -> A2AError {
    match r {
        IntentRejectReason::Forbidden if assistant => A2AError::unsupported_operation(
            "forbidden: the assistant starts agents only in its workspace (and its subfolders) or in folders this machine already uses (its repos or [peer] start roots)",
        ),
        IntentRejectReason::Forbidden => A2AError::unsupported_operation(
            "forbidden: A2A callers start agents only in folders this machine already uses (its repos or [peer] start roots)",
        ),
        _ => A2AError::invalid_params("cwd must be an existing absolute folder"),
    }
}

fn interrupted_or_final(s: &TaskState) -> bool {
    s.is_terminal() || matches!(s, TaskState::InputRequired | TaskState::AuthRequired)
}

/// The server's request handler over the hub (weak: the server never keeps a stopped hub alive).
pub(super) struct HubHandler {
    hub: Weak<Inner>,
    state: Arc<ServerState>,
}

/// Who is calling and what they may do.
struct Who {
    hub: Hub,
    id: String,
    caller: Caller,
    /// The task owner key (`id~generation`).
    key: String,
    actor: Actor,
    answers: Answers,
    /// Changes when this caller's token is rotated or the caller removed.
    revoked: watch::Receiver<u64>,
}

/// Is `rx` signalled (or its sender gone)?
fn revoked_now(rx: &watch::Receiver<u64>) -> bool {
    rx.has_changed().unwrap_or(true)
}

impl HubHandler {
    fn who(&self, params: &ServiceParams) -> Result<Who, A2AError> {
        let inner = self.hub.upgrade().ok_or_else(|| A2AError::internal("the hub is shutting down"))?;
        let hub = Hub { inner };
        let id = srv::caller_of(params).ok_or_else(|| A2AError::internal("no caller"))?.to_string();
        let revoked = self.state.revocation(&id);
        let allow = {
            let cfg = hub.cfg();
            cfg.a2a.callers.iter().find(|c| c.id == id).map(|c| c.may_answer_approvals())
        };
        let Some(allow) = allow else {
            let h = hub.clone();
            let caller = format!("a2a:{id}");
            tokio::spawn(async move { h.write_a2a_log(&caller, "request", None, "caller_removed", None).await });
            return Err(A2AError::invalid_request("this caller is no longer configured"));
        };
        let key = hub.caller_key(&id);
        Ok(Who {
            caller: Caller::Outside(key.clone()),
            key,
            actor: Actor::internal(format!("a2a:{id}")),
            answers: if allow { Answers::Limited } else { Answers::Never },
            hub,
            id,
            revoked,
        })
    }

    /// The outside-caller policy for a start, then the facade.
    async fn send(&self, w: &Who, req: SendMessageRequest) -> Result<SendMessageResponse, A2AError> {
        let m = &req.message;
        if m.task_id.is_none() && m.context_id.is_none() {
            let Some(params) = DelegateParams::of(m) else {
                // A plain message (e.g. another Pawl's first prompt): say how to delegate.
                let mut answer = Message::new(
                    Role::Agent,
                    vec![Part::text(
                        "Pawl delegates coding work to the agents on this machine. Send a message with delegate metadata \
                         (agentId and cwd, see the \"delegate\" skill of the Agent Card) to start a session, or continue \
                         a session with its contextId.",
                    )],
                );
                answer.context_id = None;
                return Ok(SendMessageResponse::Message(answer));
            };
            if params.machine_id.as_deref().is_some_and(|x| x != w.hub.machine_id()) {
                return Err(A2AError::unsupported_operation("this server starts sessions on this machine only"));
            }
            // A caller's own word is not a user's confirmation: `confirmed` is not passed on.
            let too_many = || {
                let open = w.hub.tasks().contexts_owned_by(&w.caller).iter().filter(|c| w.hub.session(c).is_some_and(|s| s.ended_at.is_none())).count();
                (open >= SESSIONS_PER_CALLER).then(|| A2AError::unsupported_operation(format!("this caller has {open} open sessions; let one finish first")))
            };
            if let Some(e) = too_many() {
                return Err(e);
            }
            let agent_id = params.agent_id.clone().unwrap_or_default();
            if !w.hub.agents().iter().any(|a| a.id == agent_id && a.installed) {
                return Err(A2AError::invalid_params(format!("agent {agent_id:?} is not installed on this machine")));
            }
            let assistant = w.hub.is_assistant_caller(&w.id);
            let cwd = params.cwd.clone().unwrap_or_default();
            let dir = w.hub.a2a_start_dir(&w.id, &cwd).await.map_err(|r| reject_reason(r, assistant))?;
            let text = super::a2a::message_text(&req.message);
            // The prompt's own checks first: a card never shows a prompt that could not be sent.
            if !text.trim().is_empty() {
                super::intents::validate_prompt(&text).map_err(|e| A2AError::invalid_params(e.to_string()))?;
            }
            let plan_for = |dir: &std::path::Path| if assistant { w.hub.assistant_launch_plan(&agent_id, dir) } else { w.hub.workspace_launch_plan(&agent_id, dir) };
            let plan = plan_for(&dir).map_err(A2AError::unsupported_operation)?;
            if assistant {
                // The user approves every start the assistant asks for, on a card, before it runs.
                w.hub.start_card(&w.id, &agent_id, &dir, &text, w.revoked.clone()).await.map_err(A2AError::unsupported_operation)?;
                if revoked_now(&w.revoked) {
                    return Err(A2AError::invalid_request("this caller is no longer configured"));
                }
                // The folder may have changed while the user decided (a link swapped, config planted):
                // the same folder and the same plan, or no start.
                let again = w.hub.a2a_start_dir(&w.id, &cwd).await.ok();
                let same_plan = again.as_deref().map(plan_for).and_then(|p| p.ok()).is_some_and(|p| p == plan);
                if again.as_deref() != Some(dir.as_path()) || !same_plan {
                    return Err(A2AError::unsupported_operation("The folder changed while you were deciding; ask again."));
                }
                // Sessions may have started while the card waited (another card allowed meanwhile).
                if let Some(e) = too_many() {
                    return Err(e);
                }
            }
            let prompt = Some(text).filter(|t| !t.trim().is_empty());
            let d = w
                .hub
                .a2a_start_in(&agent_id, super::a2a::StartDir::Checked(dir), prompt, &w.caller, plan)
                .await
                .map_err(|e| to_a2a(&e))?;
            return Ok(d.response);
        }
        let d = w.hub.a2a_send(req, &w.caller, &w.actor, w.answers).await.map_err(|e| to_a2a(&e))?;
        Ok(d.response)
    }

    /// Wait (bounded) until `task` finishes or asks for input. A caller with all its wait slots in use,
    /// or revoked meanwhile, gets the task as it is.
    async fn settle(&self, w: &Who, task: Task, history_length: Option<i32>) -> Task {
        if interrupted_or_final(&task.status.state) {
            return w.hub.a2a_get(&task.id, history_length, &w.caller).unwrap_or(task);
        }
        let Some(_slot) = w.hub.take_slot(&w.key, false) else {
            return w.hub.a2a_get(&task.id, history_length, &w.caller).unwrap_or(task);
        };
        let Some((_, Some(mut rx))) = w.hub.tasks().subscribe(&task.id) else {
            return w.hub.a2a_get(&task.id, history_length, &w.caller).unwrap_or(task);
        };
        let mut revoked = w.revoked.clone();
        let finished = async {
            loop {
                match rx.recv().await {
                    Ok(StreamResponse::StatusUpdate(s)) if interrupted_or_final(&s.status.state) => break,
                    Ok(_) | Err(RecvError::Lagged(_)) => {
                        if w.hub.tasks().get(&task.id, Some(0)).is_none_or(|t| interrupted_or_final(&t.status.state)) {
                            break;
                        }
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        };
        let wait = async {
            tokio::select! {
                _ = finished => {}
                _ = revoked.changed() => {}
            }
        };
        let _ = tokio::time::timeout(BLOCKING_WAIT, wait).await;
        w.hub.a2a_get(&task.id, history_length, &w.caller).unwrap_or(task)
    }

    /// A task's stream: its snapshot, then its updates until it ends (a lagging reader gets a fresh
    /// snapshot instead of the updates it missed). It also ends when `stop` changes (the caller was
    /// revoked); `slot` (the caller's stream slot) is held until the stream is dropped.
    pub(super) fn follow(hub: Hub, caller: Caller, task_id: String, stop: Option<watch::Receiver<u64>>, slot: Option<SlotGuard>) -> EventStream {
        let Some((snapshot, rx)) = hub.tasks().subscribe(&task_id) else {
            return futures::stream::iter(vec![Err(A2AError::task_not_found(&task_id))]).boxed();
        };
        let first = futures::stream::iter(vec![Ok(StreamResponse::Task(snapshot))]);
        let Some(rx) = rx else { return first.boxed() };
        let rest = futures::stream::unfold(Some((rx, hub, caller, task_id, stop, slot)), |state| async move {
            let (mut rx, hub, caller, task_id, mut stop, slot) = state?;
            let got = match stop.as_mut() {
                Some(s) => tokio::select! {
                    r = rx.recv() => Some(r),
                    _ = s.changed() => None,
                },
                None => Some(rx.recv().await),
            };
            match got? {
                Ok(ev) => {
                    let done = matches!(&ev, StreamResponse::StatusUpdate(s) if s.status.state.is_terminal());
                    let next = (!done).then_some((rx, hub, caller, task_id, stop, slot));
                    Some((Ok(ev), next))
                }
                Err(RecvError::Lagged(_)) => {
                    let snap = hub.a2a_get(&task_id, None, &caller).ok()?;
                    let done = snap.status.state.is_terminal();
                    let next = (!done).then_some((rx, hub, caller, task_id, stop, slot));
                    Some((Ok(StreamResponse::Task(snap)), next))
                }
                Err(RecvError::Closed) => None,
            }
        });
        first.chain(rest).boxed()
    }

    /// A stream for `w`: its slot (or a refusal when it has its share), then [`Self::follow`].
    fn stream_for(w: &Who, task_id: String) -> Result<EventStream, A2AError> {
        if revoked_now(&w.revoked) {
            return Err(A2AError::invalid_request("this caller's token was replaced"));
        }
        let slot = w.hub.take_slot(&w.key, true).ok_or_else(|| A2AError::unsupported_operation(format!("at most {STREAMS_PER_CALLER} open streams per caller")))?;
        Ok(Self::follow(w.hub.clone(), w.caller.clone(), task_id, Some(w.revoked.clone()), Some(slot)))
    }
}

fn target_of(req: &SendMessageRequest) -> Option<String> {
    req.message.task_id.clone().or_else(|| req.message.context_id.clone())
}

#[pawl_a2a::async_trait]
impl A2aHandler for HubHandler {
    async fn send_message(&self, params: &ServiceParams, req: SendMessageRequest) -> Result<SendMessageResponse, A2AError> {
        let w = self.who(params)?;
        let target = target_of(&req);
        let immediate = req.configuration.as_ref().and_then(|c| c.return_immediately).unwrap_or(false);
        let history = req.configuration.as_ref().and_then(|c| c.history_length);
        let r = self.send(&w, req).await;
        let r = match r {
            Ok(SendMessageResponse::Task(t)) if !immediate => Ok(SendMessageResponse::Task(self.settle(&w, t, history).await)),
            other => other,
        };
        let target = target.or_else(|| match &r {
            Ok(SendMessageResponse::Task(t)) => Some(t.id.clone()),
            Ok(SendMessageResponse::Message(m)) => m.context_id.clone(),
            Err(_) => None,
        });
        w.hub.log_a2a(&w.id, "SendMessage", target, &r.as_ref().map(|_| ()).map_err(Clone::clone)).await;
        r
    }

    async fn send_streaming_message(&self, params: &ServiceParams, req: SendMessageRequest) -> Result<EventStream, A2AError> {
        let w = self.who(params)?;
        let target = target_of(&req);
        let r = self.send(&w, req).await;
        w.hub.log_a2a(&w.id, "SendStreamingMessage", target, &r.as_ref().map(|_| ()).map_err(Clone::clone)).await;
        match r? {
            SendMessageResponse::Message(m) => Ok(futures::stream::iter(vec![Ok(StreamResponse::Message(m))]).boxed()),
            SendMessageResponse::Task(t) => Self::stream_for(&w, t.id),
        }
    }

    async fn get_task(&self, params: &ServiceParams, req: GetTaskRequest) -> Result<Task, A2AError> {
        let w = self.who(params)?;
        let r = w.hub.a2a_get(&req.id, req.history_length, &w.caller).map_err(|e| to_a2a(&e));
        w.hub.log_a2a(&w.id, "GetTask", Some(req.id.clone()), &r.as_ref().map(|_| ()).map_err(Clone::clone)).await;
        r
    }

    async fn list_tasks(&self, params: &ServiceParams, req: ListTasksRequest) -> Result<ListTasksResponse, A2AError> {
        let w = self.who(params)?;
        let r = w.hub.a2a_list(&req, &w.caller).map_err(|e| to_a2a(&e));
        w.hub.log_a2a(&w.id, "ListTasks", req.context_id.clone(), &r.as_ref().map(|_| ()).map_err(Clone::clone)).await;
        r
    }

    async fn cancel_task(&self, params: &ServiceParams, req: CancelTaskRequest) -> Result<Task, A2AError> {
        let w = self.who(params)?;
        let r = w.hub.a2a_cancel(&req.id, &w.caller, &w.actor).await.map_err(|e| to_a2a(&e));
        w.hub.log_a2a(&w.id, "CancelTask", Some(req.id.clone()), &r.as_ref().map(|_| ()).map_err(Clone::clone)).await;
        r
    }

    async fn subscribe_to_task(&self, params: &ServiceParams, req: SubscribeToTaskRequest) -> Result<EventStream, A2AError> {
        let w = self.who(params)?;
        let r = w.hub.a2a_get(&req.id, None, &w.caller).map_err(|e| to_a2a(&e)).and_then(|t| {
            if t.status.state.is_terminal() {
                Err(A2AError::unsupported_operation("the task has finished"))
            } else {
                Ok(t)
            }
        });
        w.hub.log_a2a(&w.id, "SubscribeToTask", Some(req.id.clone()), &r.as_ref().map(|_| ()).map_err(Clone::clone)).await;
        let t = r?;
        Self::stream_for(&w, t.id)
    }

    async fn get_extended_agent_card(&self, params: &ServiceParams, _r: GetExtendedAgentCardRequest) -> Result<AgentCard, A2AError> {
        let w = self.who(params)?;
        let card = card::pawl_card(&w.hub.card_info(w.hub.a2a_status().port, true), true);
        w.hub.log_a2a(&w.id, "GetExtendedAgentCard", None, &Ok(())).await;
        Ok(card)
    }
}

/// Tests: the server's router for this hub, with Host / Origin checks for `port` (no socket).
#[cfg(test)]
pub(crate) fn test_router(hub: &Hub, port: u16) -> axum::Router {
    let cfg = hub.cfg().a2a.clone();
    let weak = Arc::downgrade(&hub.inner);
    let card_hub = hub.clone();
    let audit_hub = hub.clone();
    let state = ServerState::new(callers_of(&cfg), move || card::pawl_card(&card_hub.card_info(port, false), false)).with_audit(move |r| audit_hub.audit_refusal(r));
    *hub.inner.a2a_test_state.lock().unwrap() = Some(state.clone());
    srv::router_for_port(Arc::new(HubHandler { hub: weak, state: state.clone() }), state, port)
}

/// Tests: the server state of the last [`test_router`] (to rotate callers on it).
#[cfg(test)]
pub(crate) fn test_state(hub: &Hub) -> Arc<ServerState> {
    hub.inner.a2a_test_state.lock().unwrap().clone().expect("test_router first")
}
