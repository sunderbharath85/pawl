//! Connection management, ACP handlers and session state for [`crate::AcpHost`].

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::v1::schema as acp;
use agent_client_protocol::v1::{ConnectionTo, Responder};
use agent_client_protocol::{self as sdk, Agent as AgentRole, ByteStreams, Client, ConnectTo};
use anyhow::{anyhow, bail, Context as _};
use pawl_protocol as pp;
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};

use crate::convert::{self, to_json};
use crate::fs as sandbox;
use crate::spawn::{self, StderrTail};
use crate::HostEvent;

/// `initialize` may include a first-time `npx` download, so be generous.
const INIT_TIMEOUT: Duration = Duration::from_secs(180);
const SESSION_TIMEOUT: Duration = Duration::from_secs(120);
const LIST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the connection may keep draining stdout after the process exited.
const DRAIN_AFTER_EXIT: Duration = Duration::from_secs(2);
/// How long an agent gets to exit by itself after its stdin closed.
const EXIT_GRACE: Duration = Duration::from_secs(3);
const SHUTDOWN_GRACE: Duration = Duration::from_secs(6);
const MAX_LIST_PAGES: usize = 20;
const MAX_HISTORY: usize = 1000;
/// A connection with no sessions that nobody used for this long is closed
/// (e.g. one spawned only for `session/list`).
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);
const IDLE_CHECK_EVERY: Duration = Duration::from_secs(30);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// State

pub(crate) struct Inner {
    /// This hub's machine: stamped on every Session / HistorySession / PermissionRequest
    /// and the prefix of every permission request id.
    machine_id: pp::MachineId,
    events: mpsc::UnboundedSender<HostEvent>,
    /// Connection key (agent + cwd + command) -> slot. The async mutex
    /// serializes spawning so concurrent callers share one process.
    conns: Mutex<HashMap<String, ConnSlot>>,
    sessions: Mutex<HashMap<pp::SessionId, Arc<SessionEntry>>>,
    permissions: Mutex<HashMap<pp::RequestId, PendingPermission>>,
    next_conn_id: AtomicU64,
    shutting_down: AtomicBool,
}

/// Holds the live connection for one key; locked while spawning.
type ConnSlot = Arc<tokio::sync::Mutex<Option<Arc<Conn>>>>;

/// One agent process + ACP connection.
pub(crate) struct Conn {
    shared: Arc<ConnShared>,
    cx: ConnectionTo<AgentRole>,
    caps: pp::AgentCapabilities,
    /// Agent advertised `sessionCapabilities.close` (`session/close`).
    supports_close: bool,
    auth_methods: Vec<String>,
    shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// The part of a connection its handlers and its driver task see.
pub(crate) struct ConnShared {
    id: u64,
    key: String,
    agent_id: pp::AgentId,
    alive: AtomicBool,
    /// ACP session id -> hub session id.
    sessions: Mutex<HashMap<String, pp::SessionId>>,
    stderr: StderrTail,
    last_used: Mutex<std::time::Instant>,
    /// Callers between `get_or_spawn` and registering their session (or finishing
    /// `session/list`). A connection is never stopped for being unused while > 0.
    opening: AtomicUsize,
}

/// Marks a caller as using a connection it has not registered a session on yet.
struct OpenGuard(Arc<ConnShared>);

impl OpenGuard {
    fn new(shared: &Arc<ConnShared>) -> Self {
        shared.opening.fetch_add(1, Ordering::SeqCst);
        shared.touch();
        OpenGuard(shared.clone())
    }
}

impl Drop for OpenGuard {
    fn drop(&mut self) {
        self.0.opening.fetch_sub(1, Ordering::SeqCst);
        self.0.touch();
    }
}

impl ConnShared {
    fn touch(&self) {
        *lock(&self.last_used) = std::time::Instant::now();
    }

    /// Resolves once the connection has had no sessions for `IDLE_TIMEOUT`.
    async fn idle(&self) {
        loop {
            tokio::time::sleep(IDLE_CHECK_EVERY).await;
            let unused = lock(&self.sessions).is_empty() && self.opening.load(Ordering::SeqCst) == 0;
            if unused && lock(&self.last_used).elapsed() >= IDLE_TIMEOUT {
                return;
            }
            if !unused {
                self.touch();
            }
        }
    }
}

struct SessionEntry {
    hub_id: pp::SessionId,
    acp_id: String,
    conn: Arc<Conn>,
    /// Canonical cwd; fs requests must stay inside it.
    root: PathBuf,
    st: Mutex<SessionState>,
}

struct SessionState {
    session: pp::Session,
    turn_active: bool,
    ended: bool,
    /// Session fields changed without a status change; emit an upsert next time.
    dirty: bool,
    /// Latest known state of each tool call in the current turn.
    tools: HashMap<String, pp::ToolCallPayload>,
    /// Tool calls currently in progress.
    running: HashSet<String>,
    /// While `Some`, events are held back (session/load replays history before
    /// the hub has seen the session upsert).
    buffer: Option<Vec<HostEvent>>,
}

struct PendingPermission {
    hub_session_id: pp::SessionId,
    conn_id: u64,
    options: Vec<pp::PermissionOption>,
    tx: oneshot::Sender<acp::RequestPermissionOutcome>,
}

#[derive(Clone, Copy)]
enum StatusChange {
    None,
    /// Apply only during an active turn and never over `waiting_permission`.
    Turn(pp::SessionStatus),
    /// Apply unless the session ended.
    Force(pp::SessionStatus),
}

fn conn_key(agent: &pp::Agent, cwd: &Path, clean_env: bool) -> String {
    let env = serde_json::to_string(&agent.env).unwrap_or_default();
    let cmd = serde_json::to_string(&agent.command).unwrap_or_default();
    let clean = if clean_env { "\u{1f}clean" } else { "" };
    format!("{}\u{1f}{}\u{1f}{cmd}\u{1f}{env}{clean}", agent.id, cwd.display())
}

/// Validate a session cwd: returns (path sent to the agent, canonical root).
fn prepare_cwd(cwd: &Path) -> anyhow::Result<(PathBuf, PathBuf)> {
    let root = sandbox::canonical_root(cwd).with_context(|| format!("invalid session cwd `{}`", cwd.display()))?;
    Ok((sandbox::display_path(&root), root))
}

fn io_to_acp_error(e: &std::io::Error) -> sdk::Error {
    match e.kind() {
        std::io::ErrorKind::NotFound => sdk::Error::new(-32002, format!("Resource not found: {e}")),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidInput => {
            sdk::Error::new(-32602, e.to_string())
        }
        _ => sdk::Error::new(-32603, e.to_string()),
    }
}

fn is_disconnect(e: &sdk::Error) -> bool {
    sdk::is_connection_terminated(e) || sdk::is_incoming_transport_closed(e)
}

fn describe_exit(agent_id: &str, status: Option<ExitStatus>, err: Option<&sdk::Error>, tail: &StderrTail) -> String {
    let mut msg = match status {
        Some(s) => format!("agent `{agent_id}` process exited ({s})"),
        None => format!("agent `{agent_id}` connection closed"),
    };
    if let Some(e) = err {
        msg.push_str(&format!(": {e}"));
    }
    let t = tail.summary(5, 600);
    if !t.is_empty() {
        msg.push_str(&format!(" — stderr: {t}"));
    }
    msg
}

impl Inner {
    pub(crate) fn new(machine_id: pp::MachineId, events: mpsc::UnboundedSender<HostEvent>) -> Self {
        Self {
            machine_id,
            events,
            conns: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            permissions: Mutex::new(HashMap::new()),
            next_conn_id: AtomicU64::new(1),
            shutting_down: AtomicBool::new(false),
        }
    }

    pub(crate) fn machine_id(&self) -> &str {
        &self.machine_id
    }

    /// A hub never runs another machine's agents. An empty `machine_id` (hand-built agent) is
    /// treated as local.
    fn ensure_local_agent(&self, agent: &pp::Agent) -> anyhow::Result<()> {
        if !agent.machine_id.is_empty() && agent.machine_id != self.machine_id {
            bail!(
                "agent `{}` belongs to machine `{}`, not this machine (`{}`)",
                agent.id,
                agent.machine_id,
                self.machine_id
            );
        }
        Ok(())
    }

    fn emit(&self, ev: HostEvent) {
        // The hub may have stopped listening (shutdown); that is not an error here.
        let _ = self.events.send(ev);
    }

    fn session(&self, hub_id: &str) -> anyhow::Result<Arc<SessionEntry>> {
        lock(&self.sessions).get(hub_id).cloned().ok_or_else(|| anyhow!("unknown or ended session `{hub_id}`"))
    }

    fn session_for(&self, conn: &ConnShared, acp_id: &str) -> Option<Arc<SessionEntry>> {
        let hub_id = lock(&conn.sessions).get(acp_id).cloned()?;
        lock(&self.sessions).get(&hub_id).cloned()
    }

    // ----- session state helpers -------------------------------------------

    /// Record an event and/or a status change for `entry`, then emit them
    /// (event first, then the session upsert if anything changed).
    fn apply(&self, entry: &SessionEntry, event: Option<(pp::EventType, serde_json::Value)>, change: StatusChange) {
        let mut out = Vec::with_capacity(2);
        {
            let mut st = lock(&entry.st);
            if st.ended {
                return;
            }
            let now = pp::now_ms();
            st.session.last_event_at = now;
            if let Some((kind, payload)) = event {
                out.push(HostEvent::Event { session_id: entry.hub_id.clone(), kind, payload });
            }
            let target = match change {
                StatusChange::None => None,
                StatusChange::Turn(s) => {
                    (st.turn_active && st.session.status != pp::SessionStatus::WaitingPermission).then_some(s)
                }
                StatusChange::Force(s) => Some(s),
            };
            let mut changed = std::mem::take(&mut st.dirty);
            if let Some(s) = target {
                if st.session.status != s {
                    st.session.status = s;
                    changed = true;
                }
            }
            if changed {
                out.push(HostEvent::Session(st.session.clone()));
            }
            if let Some(buf) = st.buffer.as_mut() {
                buf.append(&mut out);
            }
        }
        for ev in out {
            self.emit(ev);
        }
    }

    fn snapshot(entry: &SessionEntry) -> pp::Session {
        lock(&entry.st).session.clone()
    }

    /// Mark a session ended. `error` = Some(message) emits Event{Error} and status error.
    fn end_session(&self, entry: &SessionEntry, error: Option<String>) {
        let mut out = Vec::new();
        {
            let mut st = lock(&entry.st);
            if st.ended {
                return;
            }
            st.ended = true;
            st.turn_active = false;
            st.tools.clear();
            st.running.clear();
            st.buffer = None;
            let now = pp::now_ms();
            st.session.last_event_at = now;
            st.session.ended_at = Some(now);
            match error {
                Some(message) => {
                    out.push(HostEvent::Event {
                        session_id: entry.hub_id.clone(),
                        kind: pp::EventType::Error,
                        payload: to_json(&pp::ErrorPayload { message }),
                    });
                    st.session.status = pp::SessionStatus::Error;
                }
                None => {
                    if st.session.status != pp::SessionStatus::Error {
                        st.session.status = pp::SessionStatus::Done;
                    }
                }
            }
            out.push(HostEvent::Session(st.session.clone()));
        }
        for ev in out {
            self.emit(ev);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn register_session(
        &self,
        conn: &Arc<Conn>,
        agent_id: &str,
        hub_id: pp::SessionId,
        acp_id: String,
        cwd: &Path,
        root: PathBuf,
        buffering: bool,
    ) -> anyhow::Result<Arc<SessionEntry>> {
        let now = pp::now_ms();
        let entry = Arc::new(SessionEntry {
            hub_id: hub_id.clone(),
            acp_id: acp_id.clone(),
            conn: conn.clone(),
            root,
            st: Mutex::new(SessionState {
                session: pp::Session {
                    id: hub_id.clone(),
                    machine_id: self.machine_id.clone(),
                    agent_id: agent_id.to_string(),
                    acp_session_id: Some(acp_id.clone()),
                    external_id: None,
                    cwd: cwd.to_string_lossy().into_owned(),
                    title: None,
                    source: pp::SessionSource::Owned,
                    status: pp::SessionStatus::Idle,
                    started_at: now,
                    last_event_at: now,
                    ended_at: None,
                },
                turn_active: false,
                ended: false,
                dirty: false,
                tools: HashMap::new(),
                running: HashSet::new(),
                buffer: buffering.then(Vec::new),
            }),
        });
        let mut sessions = lock(&self.sessions);
        let mut by_acp = lock(&conn.shared.sessions);
        if sessions.contains_key(&hub_id) {
            bail!("hub session id `{hub_id}` is already in use");
        }
        if let Some(existing) = by_acp.get(&acp_id) {
            bail!("ACP session `{acp_id}` is already open as hub session `{existing}`");
        }
        sessions.insert(hub_id.clone(), entry.clone());
        by_acp.insert(acp_id, hub_id);
        Ok(entry)
    }

    fn unregister_session(&self, entry: &SessionEntry) {
        lock(&self.sessions).remove(&entry.hub_id);
        let mut by_acp = lock(&entry.conn.shared.sessions);
        if by_acp.get(&entry.acp_id) == Some(&entry.hub_id) {
            by_acp.remove(&entry.acp_id);
        }
    }

    fn ensure_new_hub_id(&self, hub_id: &str) -> anyhow::Result<()> {
        if lock(&self.sessions).contains_key(hub_id) {
            bail!("hub session id `{hub_id}` is already in use");
        }
        Ok(())
    }

    // ----- permissions -------------------------------------------------------

    /// Cancel matching pending permission requests. With `by = Some(..)` an
    /// `Event{PermissionResolved}` is emitted for each so the hub can clear it.
    fn cancel_permissions(&self, filter: impl Fn(&PendingPermission) -> bool, by: Option<pp::ResolvedBy>) {
        let removed: Vec<(pp::RequestId, PendingPermission)> = {
            let mut map = lock(&self.permissions);
            let ids: Vec<pp::RequestId> = map.iter().filter(|(_, p)| filter(p)).map(|(k, _)| k.clone()).collect();
            ids.into_iter().filter_map(|id| map.remove(&id).map(|p| (id, p))).collect()
        };
        for (request_id, p) in removed {
            let _ = p.tx.send(acp::RequestPermissionOutcome::Cancelled);
            if let Some(by) = by {
                if let Some(entry) = lock(&self.sessions).get(&p.hub_session_id).cloned() {
                    self.apply(&entry, Some(permission_resolved(&request_id, pp::PermissionOutcome::Cancelled, by)), StatusChange::None);
                }
            }
        }
    }

    fn has_pending_for(&self, hub_id: &str) -> bool {
        lock(&self.permissions).values().any(|p| p.hub_session_id == hub_id)
    }

    fn on_permission_request(
        self: &Arc<Self>,
        conn: &ConnShared,
        req: acp::RequestPermissionRequest,
        responder: Responder<acp::RequestPermissionResponse>,
        cx: &ConnectionTo<AgentRole>,
    ) -> Result<(), sdk::Error> {
        let acp_id = req.session_id.to_string();
        let Some(entry) = self.session_for(conn, &acp_id) else {
            tracing::warn!(agent = %conn.agent_id, session = %acp_id, "permission request for unknown session; cancelling");
            return responder.respond(acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Cancelled));
        };
        if self.shutting_down.load(Ordering::SeqCst) {
            return responder.respond(acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Cancelled));
        }

        let update = convert::tool_update_payload(&req.tool_call);
        let tool_call = {
            let mut st = lock(&entry.st);
            let merged = match st.tools.get(&update.tool_call_id) {
                Some(prev) => convert::merge_tool(prev, &update),
                None => update,
            };
            st.tools.insert(merged.tool_call_id.clone(), merged.clone());
            merged
        };
        let options: Vec<pp::PermissionOption> = req.options.iter().map(convert::permission_option).collect();
        let request_id = pp::global_id(&self.machine_id, uuid::Uuid::new_v4());
        let (tx, rx) = oneshot::channel();
        lock(&self.permissions).insert(
            request_id.clone(),
            PendingPermission { hub_session_id: entry.hub_id.clone(), conn_id: conn.id, options: options.clone(), tx },
        );

        let request = pp::PermissionRequest {
            request_id: request_id.clone(),
            machine_id: self.machine_id.clone(),
            session_id: entry.hub_id.clone(),
            agent_id: conn.agent_id.clone(),
            tool_call,
            options,
            created_at: pp::now_ms(),
            voice_code: None,
        };
        // Status first: the hub may resolve the request (rules) as soon as it sees it, and
        // that resolution's status change must not be overwritten by this one.
        self.apply(&entry, None, StatusChange::Force(pp::SessionStatus::WaitingPermission));
        self.emit(HostEvent::PermissionRequest(request));

        // Wait off the dispatch loop: the agent keeps streaming while the user decides.
        let cancellation = responder.cancellation();
        let weak = Arc::downgrade(self);
        let request_id_for_cleanup = request_id.clone();
        let spawned = cx.spawn(async move {
            tokio::select! {
                outcome = rx => {
                    let outcome = outcome.unwrap_or(acp::RequestPermissionOutcome::Cancelled);
                    responder.respond(acp::RequestPermissionResponse::new(outcome))
                }
                () = cancellation.cancelled() => {
                    // The agent withdrew the request ($/cancel_request).
                    if let Some(inner) = weak.upgrade() {
                        inner.withdraw_permission(&request_id);
                    }
                    responder.respond_with_error(sdk::Error::request_cancelled())
                }
            }
        });
        if spawned.is_err() {
            lock(&self.permissions).remove(&request_id_for_cleanup);
        }
        spawned
    }

    /// The agent cancelled its own permission request: clear it and resume the turn status.
    fn withdraw_permission(&self, request_id: &str) {
        let Some(p) = lock(&self.permissions).remove(request_id) else { return };
        let Ok(entry) = self.session(&p.hub_session_id) else { return };
        // `Turn` never overrides waiting_permission, so force once nothing else is pending.
        let next = if !self.has_pending_for(&entry.hub_id) && lock(&entry.st).turn_active {
            StatusChange::Force(pp::SessionStatus::Thinking)
        } else {
            StatusChange::None
        };
        self.apply(&entry, Some(permission_resolved(request_id, pp::PermissionOutcome::Cancelled, pp::ResolvedBy::System)), next);
    }

    // ----- session/update -----------------------------------------------------

    fn on_session_notification(&self, conn: &ConnShared, n: acp::SessionNotification) {
        let acp_id = n.session_id.to_string();
        let Some(entry) = self.session_for(conn, &acp_id) else {
            tracing::debug!(agent = %conn.agent_id, session = %acp_id, "session/update for unknown session ignored");
            return;
        };
        use acp::SessionUpdate as U;
        use pp::EventType as E;
        use pp::SessionStatus as S;
        let text = |c: &acp::ContentBlock| convert::content_text(c).map(|text| to_json(&pp::TextPayload { text }));
        match &n.update {
            U::UserMessageChunk(c) => {
                if let Some(p) = text(&c.content) {
                    self.apply(&entry, Some((E::UserMessage, p)), StatusChange::None);
                }
            }
            U::AgentMessageChunk(c) => {
                if let Some(p) = text(&c.content) {
                    self.apply(&entry, Some((E::AgentMessageChunk, p)), StatusChange::Turn(S::Thinking));
                }
            }
            U::AgentThoughtChunk(c) => {
                if let Some(p) = text(&c.content) {
                    self.apply(&entry, Some((E::AgentThoughtChunk, p)), StatusChange::Turn(S::Thinking));
                }
            }
            U::ToolCall(tc) => {
                let payload = convert::tool_call_payload(tc);
                let change = self.track_tool(&entry, &payload, true);
                self.apply(&entry, Some((E::ToolCall, to_json(&payload))), change);
            }
            U::ToolCallUpdate(u) => {
                let partial = convert::tool_update_payload(u);
                let merged = {
                    let st = lock(&entry.st);
                    match st.tools.get(&partial.tool_call_id) {
                        // Fill kind/title/locations from the original call, but only send
                        // rawInput again when this update carries it.
                        Some(prev) => {
                            let mut m = convert::merge_tool(prev, &partial);
                            m.raw_input = partial.raw_input.clone();
                            m
                        }
                        None => partial.clone(),
                    }
                };
                let change = self.track_tool(&entry, &partial, false);
                self.apply(&entry, Some((E::ToolCallUpdate, to_json(&merged))), change);
            }
            U::Plan(plan) => {
                self.apply(&entry, Some((E::Plan, to_json(&convert::plan_payload(&plan.entries)))), StatusChange::None);
            }
            U::PlanUpdate(pu) => {
                if let acp::PlanUpdateContent::Items(items) = &pu.plan {
                    self.apply(&entry, Some((E::Plan, to_json(&convert::plan_payload(&items.entries)))), StatusChange::None);
                }
            }
            U::PlanRemoved(_) => {
                self.apply(&entry, Some((E::Plan, to_json(&pp::PlanPayload { entries: vec![] }))), StatusChange::None);
            }
            U::SessionInfoUpdate(info) => {
                let new_title = match &info.title {
                    agent_client_protocol::schema::MaybeUndefined::Undefined => return,
                    agent_client_protocol::schema::MaybeUndefined::Null => None,
                    agent_client_protocol::schema::MaybeUndefined::Value(t) => Some(t.clone()).filter(|t| !t.trim().is_empty()),
                };
                {
                    let mut st = lock(&entry.st);
                    if st.session.title == new_title {
                        return;
                    }
                    st.session.title = new_title;
                    st.dirty = true;
                }
                self.apply(&entry, None, StatusChange::None);
            }
            U::Notice(notice) if matches!(notice.severity, acp::NoticeSeverity::Error) => {
                let message = match &notice.description {
                    Some(d) if !d.is_empty() => format!("{}: {d}", notice.title),
                    _ => notice.title.clone(),
                };
                self.apply(&entry, Some((E::Error, to_json(&pp::ErrorPayload { message }))), StatusChange::None);
            }
            other => {
                tracing::trace!(agent = %conn.agent_id, update = ?std::mem::discriminant(other), "unmapped session/update");
            }
        }
    }

    /// Update the tool cache / running set; returns the status change implied.
    fn track_tool(&self, entry: &SessionEntry, update: &pp::ToolCallPayload, full: bool) -> StatusChange {
        let mut st = lock(&entry.st);
        let id = update.tool_call_id.clone();
        let merged = match (full, st.tools.get(&id)) {
            (false, Some(prev)) => convert::merge_tool(prev, update),
            _ => update.clone(),
        };
        let status = merged.status;
        st.tools.insert(id.clone(), merged);
        match status {
            Some(pp::ToolCallStatus::InProgress) => {
                st.running.insert(id);
                StatusChange::Turn(pp::SessionStatus::Working)
            }
            Some(pp::ToolCallStatus::Completed) | Some(pp::ToolCallStatus::Failed) => {
                st.running.remove(&id);
                if st.running.is_empty() && st.session.status == pp::SessionStatus::Working {
                    StatusChange::Turn(pp::SessionStatus::Thinking)
                } else {
                    StatusChange::None
                }
            }
            _ => StatusChange::None,
        }
    }

    // ----- prompt turns ---------------------------------------------------------

    fn on_turn_finished(&self, entry: &SessionEntry, res: Result<acp::PromptResponse, sdk::Error>) {
        {
            let mut st = lock(&entry.st);
            st.turn_active = false;
            st.tools.clear();
            st.running.clear();
        }
        // The agent must have answered every permission request before ending the turn.
        let hub_id = entry.hub_id.clone();
        self.cancel_permissions(move |p| p.hub_session_id == hub_id, Some(pp::ResolvedBy::System));
        match res {
            Ok(resp) => {
                let reason = convert::stop_reason(&resp.stop_reason);
                self.apply(
                    entry,
                    Some((pp::EventType::Stop, to_json(&pp::StopPayload { stop_reason: reason }))),
                    StatusChange::Force(convert::status_after_stop(reason)),
                );
            }
            Err(e) if is_disconnect(&e) || self.shutting_down.load(Ordering::SeqCst) => {
                // The connection-closed handler reports the process exit.
                tracing::debug!(session = %entry.hub_id, error = %e, "prompt ended by disconnect");
            }
            Err(e) => {
                let message = format!("session/prompt failed: {e}");
                self.apply(
                    entry,
                    Some((pp::EventType::Error, to_json(&pp::ErrorPayload { message }))),
                    StatusChange::Force(pp::SessionStatus::Error),
                );
            }
        }
    }

    // ----- connections -----------------------------------------------------------

    fn request_error(&self, conn: &Conn, method: &str, e: sdk::Error) -> anyhow::Error {
        if e.code == sdk::ErrorCode::AUTH_REQUIRED {
            let methods = if conn.auth_methods.is_empty() { String::new() } else { format!(" (auth methods: {})", conn.auth_methods.join(", ")) };
            return anyhow!(
                "agent `{}` requires authentication{methods}; sign in with the agent's own CLI first ({method}: {e})",
                conn.shared.agent_id
            );
        }
        if is_disconnect(&e) {
            let tail = conn.shared.stderr.summary(5, 600);
            if !tail.is_empty() {
                return anyhow!("{method} failed: agent `{}` disconnected: {e} — stderr: {tail}", conn.shared.agent_id);
            }
        }
        anyhow!("{method} failed: {e}")
    }

    fn find_live_conn(&self, agent: &pp::Agent) -> Option<(Arc<Conn>, OpenGuard)> {
        let prefix = format!("{}\u{1f}", agent.id);
        let cmd = serde_json::to_string(&agent.command).unwrap_or_default();
        let slots: Vec<_> = lock(&self.conns).iter().filter(|(k, _)| k.starts_with(&prefix)).map(|(_, v)| v.clone()).collect();
        for slot in slots {
            if let Ok(guard) = slot.try_lock() {
                if let Some(c) = guard.as_ref() {
                    if c.shared.alive.load(Ordering::SeqCst) && c.shared.key.split('\u{1f}').nth(2) == Some(cmd.as_str()) {
                        return Some((c.clone(), OpenGuard::new(&c.shared)));
                    }
                }
            }
        }
        None
    }

    /// The live connection for (agent, cwd), spawning it if needed. The guard keeps
    /// it from being stopped as unused until the caller registered its session.
    async fn get_or_spawn(self: &Arc<Self>, agent: &pp::Agent, cwd: &Path, clean_env: bool) -> anyhow::Result<(Arc<Conn>, OpenGuard)> {
        if self.shutting_down.load(Ordering::SeqCst) {
            bail!("ACP host is shut down");
        }
        let key = conn_key(agent, cwd, clean_env);
        let slot = lock(&self.conns).entry(key.clone()).or_default().clone();
        let mut guard = slot.lock().await;
        if let Some(c) = guard.as_ref() {
            if c.shared.alive.load(Ordering::SeqCst) {
                return Ok((c.clone(), OpenGuard::new(&c.shared)));
            }
        }
        let spawned = spawn::spawn_agent(&agent.id, &agent.command, agent.env.as_ref(), cwd, clean_env)?;
        tracing::info!(agent = %agent.id, program = %spawned.program.display(), cwd = %cwd.display(), "spawned ACP agent");
        let transport = ByteStreams::new(spawned.stdin.compat_write(), spawned.stdout.compat());
        let conn = self.attach(&agent.id, key, transport, Some((spawned.child, spawned.tree)), spawned.stderr_tail).await?;
        if self.shutting_down.load(Ordering::SeqCst) {
            Self::stop_conn(&conn);
            bail!("ACP host is shut down");
        }
        *guard = Some(conn.clone());
        let open = OpenGuard::new(&conn.shared);
        Ok((conn, open))
    }

    /// Run an ACP client connection over `transport`, then `initialize` it.
    pub(crate) async fn attach(
        self: &Arc<Self>,
        agent_id: &str,
        key: String,
        transport: impl ConnectTo<Client> + 'static,
        process: Option<(tokio::process::Child, spawn::ProcessTree)>,
        stderr: StderrTail,
    ) -> anyhow::Result<Arc<Conn>> {
        let shared = Arc::new(ConnShared {
            id: self.next_conn_id.fetch_add(1, Ordering::SeqCst),
            key,
            agent_id: agent_id.to_string(),
            alive: AtomicBool::new(true),
            sessions: Mutex::new(HashMap::new()),
            stderr,
            last_used: Mutex::new(std::time::Instant::now()),
            opening: AtomicUsize::new(0),
        });
        let weak = Arc::downgrade(self);

        let (w_n, s_n) = (weak.clone(), shared.clone());
        let (w_p, s_p) = (weak.clone(), shared.clone());
        let (w_r, s_r) = (weak.clone(), shared.clone());
        let (w_w, s_w) = (weak.clone(), shared.clone());
        let builder = Client
            .v1()
            .name("pawl")
            .on_receive_notification(
                async move |n: acp::SessionNotification, _cx: ConnectionTo<AgentRole>| {
                    if let Some(inner) = w_n.upgrade() {
                        inner.on_session_notification(&s_n, n);
                    }
                    Ok(())
                },
                sdk::on_receive_notification!(),
            )
            .on_receive_request(
                async move |req: acp::RequestPermissionRequest,
                            responder: Responder<acp::RequestPermissionResponse>,
                            cx: ConnectionTo<AgentRole>| {
                    match w_p.upgrade() {
                        Some(inner) => inner.on_permission_request(&s_p, req, responder, &cx),
                        None => responder.respond(acp::RequestPermissionResponse::new(acp::RequestPermissionOutcome::Cancelled)),
                    }
                },
                sdk::on_receive_request!(),
            )
            .on_receive_request(
                async move |req: acp::ReadTextFileRequest,
                            responder: Responder<acp::ReadTextFileResponse>,
                            cx: ConnectionTo<AgentRole>| {
                    let root = w_r.upgrade().and_then(|i| i.session_for(&s_r, &req.session_id.to_string())).map(|e| e.root.clone());
                    let Some(root) = root else {
                        return responder.respond_with_error(sdk::Error::new(-32602, "unknown session"));
                    };
                    cx.spawn(async move {
                        match sandbox::read_text_file(&root, &req.path, req.line, req.limit).await {
                            Ok(content) => responder.respond(acp::ReadTextFileResponse::new(content)),
                            Err(e) => responder.respond_with_error(io_to_acp_error(&e)),
                        }
                    })
                },
                sdk::on_receive_request!(),
            )
            .on_receive_request(
                async move |req: acp::WriteTextFileRequest,
                            responder: Responder<acp::WriteTextFileResponse>,
                            cx: ConnectionTo<AgentRole>| {
                    let root = w_w.upgrade().and_then(|i| i.session_for(&s_w, &req.session_id.to_string())).map(|e| e.root.clone());
                    let Some(root) = root else {
                        return responder.respond_with_error(sdk::Error::new(-32602, "unknown session"));
                    };
                    cx.spawn(async move {
                        match sandbox::write_text_file(&root, &req.path, &req.content).await {
                            Ok(()) => responder.respond(acp::WriteTextFileResponse::new()),
                            Err(e) => responder.respond_with_error(io_to_acp_error(&e)),
                        }
                    })
                },
                sdk::on_receive_request!(),
            );

        let (ready_tx, ready_rx) = oneshot::channel::<ConnectionTo<AgentRole>>();
        let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();
        let idle_shared = shared.clone();
        let conn_fut = builder.connect_with(transport, async move |cx: ConnectionTo<AgentRole>| {
            let _ = ready_tx.send(cx.clone());
            tokio::select! {
                _ = shutdown_rx => {}
                () = cx.incoming_closed() => {}
                () = idle_shared.idle() => {
                    tracing::info!(agent = %idle_shared.agent_id, "closing idle agent connection");
                }
            }
            Ok::<(), sdk::Error>(())
        });

        let task_shared = shared.clone();
        let task_weak = weak.clone();
        let task = tokio::spawn(async move {
            let (mut child, tree) = match process {
                Some((child, tree)) => (Some(child), Some(tree)),
                None => (None, None),
            };
            let mut conn_fut = std::pin::pin!(conn_fut);
            let mut exit: Option<ExitStatus> = None;
            let result = match child.as_mut() {
                Some(c) => tokio::select! {
                    r = &mut conn_fut => r,
                    status = c.wait() => {
                        exit = status.ok();
                        tokio::time::timeout(DRAIN_AFTER_EXIT, &mut conn_fut).await.unwrap_or(Ok(()))
                    }
                },
                None => conn_fut.await,
            };
            if let (None, Some(c)) = (exit, child.as_mut()) {
                exit = match tokio::time::timeout(EXIT_GRACE, c.wait()).await {
                    Ok(s) => s.ok(),
                    Err(_) => {
                        if let Some(tree) = &tree {
                            tree.kill();
                        }
                        let _ = c.kill().await;
                        c.try_wait().ok().flatten()
                    }
                };
            }
            // Whatever the agent started (npx -> node, tool subprocesses) dies with it.
            if let Some(tree) = tree {
                tree.kill();
            }
            let reason = describe_exit(&task_shared.agent_id, exit, result.as_ref().err(), &task_shared.stderr);
            tracing::info!(agent = %task_shared.agent_id, "{reason}");
            match task_weak.upgrade() {
                Some(inner) => inner.on_connection_closed(&task_shared, reason),
                None => task_shared.alive.store(false, Ordering::SeqCst),
            }
        });

        let cx = match ready_rx.await {
            Ok(cx) => cx,
            Err(_) => {
                let _ = task.await;
                bail!("{}", describe_exit(agent_id, None, None, &shared.stderr));
            }
        };

        let init_req = acp::InitializeRequest::new(ProtocolVersion::V1)
            .client_capabilities(
                acp::ClientCapabilities::new()
                    .fs(acp::FileSystemCapabilities::new().read_text_file(true).write_text_file(true))
                    .terminal(false),
            )
            .client_info(acp::Implementation::new("pawl", env!("CARGO_PKG_VERSION")));
        let init = tokio::time::timeout(INIT_TIMEOUT, cx.send_request(init_req).block_task()).await;
        let init = match init {
            Ok(Ok(resp)) => resp,
            failure => {
                let _ = shutdown_tx.send(());
                let _ = tokio::time::timeout(EXIT_GRACE + DRAIN_AFTER_EXIT, task).await;
                let detail = match failure {
                    Ok(Err(e)) => e.to_string(),
                    _ => format!("no response within {}s", INIT_TIMEOUT.as_secs()),
                };
                let tail = shared.stderr.summary(5, 600);
                if tail.is_empty() {
                    bail!("initialize failed for agent `{agent_id}`: {detail}");
                }
                bail!("initialize failed for agent `{agent_id}`: {detail} — stderr: {tail}");
            }
        };

        let caps = convert::agent_capabilities(&init.agent_capabilities);
        let supports_close = init.agent_capabilities.session_capabilities.close.is_some();
        let auth_methods = init
            .auth_methods
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .filter_map(|v| v.get("name").or_else(|| v.get("id")).and_then(|n| n.as_str()).map(str::to_string))
            .collect();
        tracing::info!(
            agent = %agent_id,
            agent_info = ?init.agent_info.as_ref().map(|i| format!("{} {}", i.name, i.version)),
            ?caps,
            "ACP agent initialized"
        );
        self.emit(HostEvent::AgentCapabilities { agent_id: agent_id.to_string(), capabilities: caps.clone() });

        Ok(Arc::new(Conn {
            shared,
            cx,
            caps,
            supports_close,
            auth_methods,
            shutdown_tx: Mutex::new(Some(shutdown_tx)),
            task: Mutex::new(Some(task)),
        }))
    }

    /// Ask a connection to close (stdin is closed; the process gets a grace period).
    fn stop_conn(conn: &Conn) {
        if let Some(tx) = lock(&conn.shutdown_tx).take() {
            let _ = tx.send(());
        }
    }

    fn on_connection_closed(&self, shared: &ConnShared, reason: String) {
        shared.alive.store(false, Ordering::SeqCst);
        let shutting = self.shutting_down.load(Ordering::SeqCst);
        let conn_id = shared.id;
        self.cancel_permissions(move |p| p.conn_id == conn_id, Some(pp::ResolvedBy::System));
        let hub_ids: Vec<pp::SessionId> = lock(&shared.sessions).drain().map(|(_, h)| h).collect();
        for id in hub_ids {
            let entry = lock(&self.sessions).remove(&id);
            if let Some(entry) = entry {
                self.end_session(&entry, (!shutting).then(|| reason.clone()));
            }
        }
        let slot = lock(&self.conns).get(&shared.key).cloned();
        if let Some(slot) = slot {
            if let Ok(mut guard) = slot.try_lock() {
                if guard.as_ref().is_some_and(|c| c.shared.id == shared.id) {
                    *guard = None;
                }
            }
        }
    }

    // ----- public operations ----------------------------------------------------------

    pub(crate) async fn start_session(
        self: &Arc<Self>,
        agent: &pp::Agent,
        cwd: PathBuf,
        hub_id: pp::SessionId,
        prompt: Option<String>,
        opts: crate::StartOptions,
    ) -> anyhow::Result<pp::Session> {
        self.ensure_local_agent(agent)?;
        self.ensure_new_hub_id(&hub_id)?;
        let (cwd_abs, root) = prepare_cwd(&cwd)?;
        let (conn, _open) = self.get_or_spawn(agent, &cwd_abs, opts.clean_env).await?;
        let mut req = acp::NewSessionRequest::new(cwd_abs.clone());
        req.meta = opts.meta.as_ref().and_then(|m| m.as_object().cloned());
        let resp = tokio::time::timeout(SESSION_TIMEOUT, conn.cx.send_request(req).block_task())
            .await
            .map_err(|_| anyhow!("session/new timed out after {}s", SESSION_TIMEOUT.as_secs()))?
            .map_err(|e| self.request_error(&conn, "session/new", e))?;
        let entry = self.register_session(&conn, &agent.id, hub_id, resp.session_id.to_string(), &cwd_abs, root, false)?;
        self.emit(HostEvent::Session(Self::snapshot(&entry)));
        if let Some(text) = prompt.filter(|p| !p.trim().is_empty()) {
            if let Err(e) = self.prompt(&entry.hub_id, text).await {
                tracing::warn!(session = %entry.hub_id, error = %e, "initial prompt failed");
                self.apply(
                    &entry,
                    Some((pp::EventType::Error, to_json(&pp::ErrorPayload { message: format!("initial prompt failed: {e}") }))),
                    StatusChange::Force(pp::SessionStatus::Error),
                );
            }
        }
        Ok(Self::snapshot(&entry))
    }

    pub(crate) async fn prompt(self: &Arc<Self>, hub_id: &str, text: String) -> anyhow::Result<()> {
        let entry = self.session(hub_id)?;
        if !entry.conn.shared.alive.load(Ordering::SeqCst) {
            bail!("agent process for session `{hub_id}` is not running");
        }
        if text.trim().is_empty() {
            bail!("prompt is empty");
        }
        {
            let mut st = lock(&entry.st);
            if st.ended {
                bail!("session `{hub_id}` has ended");
            }
            if st.turn_active {
                bail!("session `{hub_id}` is already running a prompt; cancel it first");
            }
            st.turn_active = true;
            st.tools.clear();
            st.running.clear();
            if st.session.title.is_none() {
                st.session.title = convert::title_from_prompt(&text);
                st.dirty = true;
            }
        }
        let req = acp::PromptRequest::new(entry.acp_id.clone(), vec![acp::ContentBlock::Text(acp::TextContent::new(text.clone()))]);
        let response = entry.conn.cx.send_request(req).block_task();
        self.apply(
            &entry,
            Some((pp::EventType::UserMessage, to_json(&pp::TextPayload { text }))),
            StatusChange::Force(pp::SessionStatus::Thinking),
        );
        // Weak refs: a dropped host must not be kept alive by an unanswered turn.
        let (weak, weak_entry) = (Arc::downgrade(self), Arc::downgrade(&entry));
        drop(entry);
        tokio::spawn(async move {
            let res = response.await;
            if let (Some(inner), Some(entry)) = (weak.upgrade(), weak_entry.upgrade()) {
                inner.on_turn_finished(&entry, res);
            }
        });
        Ok(())
    }

    pub(crate) async fn cancel(&self, hub_id: &str) -> anyhow::Result<()> {
        let entry = self.session(hub_id)?;
        let turn_active = lock(&entry.st).turn_active;
        // Per ACP, the client answers outstanding permission requests with `cancelled`.
        let id = entry.hub_id.clone();
        self.cancel_permissions(move |p| p.hub_session_id == id, Some(pp::ResolvedBy::User));
        if turn_active {
            entry
                .conn
                .cx
                .send_notification(acp::CancelNotification::new(entry.acp_id.clone()))
                .map_err(|e| anyhow!("session/cancel failed: {e}"))?;
            let waiting = Self::snapshot(&entry).status == pp::SessionStatus::WaitingPermission;
            if waiting {
                self.apply(&entry, None, StatusChange::Force(pp::SessionStatus::Thinking));
            }
        }
        Ok(())
    }

    pub(crate) async fn resolve_permission(&self, request_id: &str, option_id: Option<String>) -> anyhow::Result<()> {
        let (pending, allowed) = {
            let mut map = lock(&self.permissions);
            let p = map.get(request_id).ok_or_else(|| anyhow!("unknown or already resolved permission request `{request_id}`"))?;
            let allowed = match &option_id {
                Some(id) => {
                    let opt = p
                        .options
                        .iter()
                        .find(|o| &o.option_id == id)
                        .ok_or_else(|| anyhow!("option `{id}` was not offered for permission request `{request_id}`"))?;
                    matches!(opt.kind, pp::PermissionOptionKind::AllowOnce | pp::PermissionOptionKind::AllowAlways)
                }
                None => false,
            };
            (map.remove(request_id).expect("checked above"), allowed)
        };
        let outcome = match option_id {
            Some(id) => acp::RequestPermissionOutcome::Selected(acp::SelectedPermissionOutcome::new(id)),
            None => acp::RequestPermissionOutcome::Cancelled,
        };
        if pending.tx.send(outcome).is_err() {
            tracing::debug!(request = %request_id, "agent stopped waiting for this permission");
        }
        if let Ok(entry) = self.session(&pending.hub_session_id) {
            if !self.has_pending_for(&entry.hub_id) && lock(&entry.st).turn_active {
                let next = if allowed { pp::SessionStatus::Working } else { pp::SessionStatus::Thinking };
                self.apply(&entry, None, StatusChange::Force(next));
            }
        }
        Ok(())
    }

    pub(crate) async fn list_history(self: &Arc<Self>, agent: &pp::Agent, cwd: Option<PathBuf>) -> anyhow::Result<Vec<pp::HistorySession>> {
        self.ensure_local_agent(agent)?;
        if agent.capabilities.as_ref().is_some_and(|c| !c.list_sessions) {
            return Ok(vec![]);
        }
        let filter_cwd = match &cwd {
            Some(c) => Some(prepare_cwd(c)?.0),
            None => None,
        };
        let (conn, _open) = match &filter_cwd {
            Some(c) => self.get_or_spawn(agent, c, false).await?,
            None => match self.find_live_conn(agent) {
                Some(c) => c,
                None => {
                    let home = dirs::home_dir()
                        .or_else(|| std::env::current_dir().ok())
                        .context("no home directory to start the agent in")?;
                    let (home, _) = prepare_cwd(&home)?;
                    self.get_or_spawn(agent, &home, false).await?
                }
            },
        };
        if !conn.caps.list_sessions {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_LIST_PAGES {
            let req = acp::ListSessionsRequest::new().cwd(filter_cwd.clone()).cursor(cursor.clone());
            let resp = tokio::time::timeout(LIST_TIMEOUT, conn.cx.send_request(req).block_task())
                .await
                .map_err(|_| anyhow!("session/list timed out after {}s", LIST_TIMEOUT.as_secs()))?
                .map_err(|e| self.request_error(&conn, "session/list", e))?;
            out.extend(resp.sessions.iter().map(|s| convert::history_session(&self.machine_id, &agent.id, s)));
            match resp.next_cursor {
                Some(c) if !c.is_empty() && out.len() < MAX_HISTORY && cursor.as_ref() != Some(&c) => cursor = Some(c),
                _ => break,
            }
        }
        out.truncate(MAX_HISTORY);
        out.sort_by_key(|h| std::cmp::Reverse(h.updated_at));
        Ok(out)
    }

    pub(crate) async fn resume_session(
        self: &Arc<Self>,
        agent: &pp::Agent,
        acp_session_id: String,
        cwd: PathBuf,
        hub_id: pp::SessionId,
    ) -> anyhow::Result<pp::Session> {
        self.ensure_local_agent(agent)?;
        self.ensure_new_hub_id(&hub_id)?;
        if let Some(c) = &agent.capabilities {
            if !c.resume_session && !c.load_session {
                bail!("agent `{}` supports neither session/resume nor session/load", agent.id);
            }
        }
        let (cwd_abs, root) = prepare_cwd(&cwd)?;
        let (conn, _open) = self.get_or_spawn(agent, &cwd_abs, false).await?;
        let caps = conn.caps.clone();
        if !caps.resume_session && !caps.load_session {
            bail!("agent `{}` supports neither session/resume nor session/load", agent.id);
        }
        // Register first so a session/load history replay is routed (and buffered).
        let entry = self.register_session(&conn, &agent.id, hub_id, acp_session_id.clone(), &cwd_abs, root, true)?;
        let (method, res) = if caps.resume_session {
            let req = acp::ResumeSessionRequest::new(acp_session_id, cwd_abs.clone());
            ("session/resume", tokio::time::timeout(SESSION_TIMEOUT, conn.cx.send_request(req).block_task()).await.map(|r| r.map(drop)))
        } else {
            let req = acp::LoadSessionRequest::new(acp_session_id, cwd_abs.clone());
            ("session/load", tokio::time::timeout(SESSION_TIMEOUT, conn.cx.send_request(req).block_task()).await.map(|r| r.map(drop)))
        };
        let res = res
            .map_err(|_| anyhow!("{method} timed out after {}s", SESSION_TIMEOUT.as_secs()))
            .and_then(|r| r.map_err(|e| self.request_error(&conn, method, e)));
        if let Err(e) = res {
            self.unregister_session(&entry);
            return Err(e);
        }
        // Session upsert first, then the replayed history.
        let (snapshot, buffered) = {
            let mut st = lock(&entry.st);
            st.tools.clear();
            st.running.clear();
            st.dirty = false;
            (st.session.clone(), st.buffer.take().unwrap_or_default())
        };
        self.emit(HostEvent::Session(snapshot.clone()));
        for ev in buffered {
            if !matches!(ev, HostEvent::Session(_)) {
                self.emit(ev);
            }
        }
        Ok(Self::snapshot(&entry))
    }

    /// End a session for good: pending permissions are cancelled (`by: system`), the
    /// agent is told (`session/close` when supported, else `session/cancel` for a running
    /// turn), the session is upserted `done` + `endedAt`, and the agent process is stopped
    /// once no session uses it. Unknown or already-ended sessions are a no-op.
    pub(crate) async fn close_session(&self, hub_id: &str) -> anyhow::Result<()> {
        let Some(entry) = lock(&self.sessions).get(hub_id).cloned() else {
            return Ok(());
        };
        let id = entry.hub_id.clone();
        self.cancel_permissions(move |p| p.hub_session_id == id, Some(pp::ResolvedBy::System));
        let turn_active = lock(&entry.st).turn_active;
        self.unregister_session(&entry);
        self.end_session(&entry, None);

        let conn = entry.conn.clone();
        let acp_id = entry.acp_id.clone();
        drop(entry);
        if conn.shared.alive.load(Ordering::SeqCst) {
            if conn.supports_close {
                // The agent cancels any running turn and frees the session.
                let req = conn.cx.send_request(acp::CloseSessionRequest::new(acp_id.clone())).block_task();
                match tokio::time::timeout(CLOSE_TIMEOUT, req).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => tracing::debug!(session = %hub_id, error = %e, "session/close failed"),
                    Err(_) => tracing::debug!(session = %hub_id, "session/close timed out"),
                }
            } else if turn_active {
                if let Err(e) = conn.cx.send_notification(acp::CancelNotification::new(acp_id)) {
                    tracing::debug!(session = %hub_id, error = %e, "session/cancel on close failed");
                }
            }
        }
        self.stop_if_unused(&conn).await;
        Ok(())
    }

    /// Stop `conn` if it serves no session and nobody is about to use it.
    async fn stop_if_unused(&self, conn: &Arc<Conn>) {
        let slot = lock(&self.conns).get(&conn.shared.key).cloned();
        // Holding the slot lock serializes with `get_or_spawn` handing the connection out.
        let mut guard = match &slot {
            Some(slot) => tokio::time::timeout(Duration::from_secs(5), slot.lock()).await.ok(),
            None => None,
        };
        if !lock(&conn.shared.sessions).is_empty() || conn.shared.opening.load(Ordering::SeqCst) > 0 {
            return;
        }
        tracing::info!(agent = %conn.shared.agent_id, "last session closed; stopping agent process");
        Self::stop_conn(conn);
        if let Some(g) = guard.as_mut() {
            if g.as_ref().is_some_and(|c| c.shared.id == conn.shared.id) {
                **g = None;
            }
        }
    }

    pub(crate) async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        // Unblock agents waiting on the user.
        self.cancel_permissions(|_| true, Some(pp::ResolvedBy::System));
        let slots: Vec<_> = lock(&self.conns).drain().map(|(_, s)| s).collect();
        let mut tasks = Vec::new();
        for slot in slots {
            // A spawn in progress notices `shutting_down` and stops its own connection.
            let Ok(mut guard) = tokio::time::timeout(Duration::from_secs(2), slot.lock()).await else { continue };
            if let Some(conn) = guard.take() {
                Self::stop_conn(&conn);
                if let Some(t) = lock(&conn.task).take() {
                    tasks.push(t);
                }
            }
        }
        let wait_all = futures::future::join_all(tasks.iter_mut());
        if tokio::time::timeout(SHUTDOWN_GRACE, wait_all).await.is_err() {
            for t in &tasks {
                t.abort();
            }
        }
        let remaining: Vec<_> = lock(&self.sessions).drain().map(|(_, e)| e).collect();
        for e in remaining {
            self.end_session(&e, None);
        }
    }

    #[cfg(test)]
    pub(crate) fn find_live_conn_for_test(&self, agent: &pp::Agent) -> Option<u64> {
        self.find_live_conn(agent).map(|(c, _)| c.shared.id)
    }

    /// Test hook: serve `agent` in `cwd` over an in-process transport instead of a subprocess.
    #[cfg(test)]
    pub(crate) async fn attach_for_test(
        self: &Arc<Self>,
        agent: &pp::Agent,
        cwd: &Path,
        transport: impl ConnectTo<Client> + 'static,
    ) -> anyhow::Result<()> {
        let (cwd_abs, _) = prepare_cwd(cwd)?;
        let key = conn_key(agent, &cwd_abs, false);
        let conn = self.attach(&agent.id, key.clone(), transport, None, StderrTail::default()).await?;
        let slot = lock(&self.conns).entry(key).or_default().clone();
        *slot.lock().await = Some(conn);
        Ok(())
    }
}

fn permission_resolved(request_id: &str, outcome: pp::PermissionOutcome, by: pp::ResolvedBy) -> (pp::EventType, serde_json::Value) {
    (
        pp::EventType::PermissionResolved,
        to_json(&pp::PermissionResolvedPayload { request_id: request_id.to_string(), outcome, by, by_machine_id: None, via: None }),
    )
}
