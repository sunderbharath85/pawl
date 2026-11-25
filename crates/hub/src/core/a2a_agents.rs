//! Outside A2A agents as sub-agents (PLAN.md "Outside A2A agents", phase A3).
//!
//! The user adds an agent by URL in Settings (`[[a2a.agents]]`). The hub fetches its Agent Card
//! (ETag-cached, size-capped, same-origin endpoint; see `pawl_a2a::client`), lists it as agent
//! `a2a:<id>` next to the ACP agents, and runs its delegations as sessions of this machine (so they
//! show in every client and in paired machines' mirrors). The runtime speaks to the hub exactly like
//! the ACP host does, through `HostEvent`s: user / agent messages, stop reasons, errors, session
//! upserts, and permission requests.
//!
//! A2A -> hub events: working status text and artifact text -> agent message chunks; COMPLETED -> stop
//! `end_turn`; CANCELED -> `cancelled`; REJECTED -> `refusal`; FAILED -> an error. `INPUT_REQUIRED`
//! with a Structured Input schema that picks one value (an enum) -> an approval card whose options
//! are the values (tool kind "other" unless Pawl-style details say otherwise); the session is
//! remote-driven, so rules never auto-approve it. Any other question (text, or a schema Pawl cannot
//! fill in) is shown as the agent's message and the user's next prompt answers it.
//! Everything the agent sends is untrusted data (it lands in the event log like any agent text).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{anyhow, bail, Result};
use futures::StreamExt;
use pawl_a2a::client::{self as a2ac, CardFetch, ClientError, Endpoint};
use pawl_a2a::spec::{AgentCard, Message, Part, PartContent, Role, SendMessageConfiguration, SendMessageRequest, SendMessageResponse, StreamResponse, TaskState, TaskStatus};
use pawl_a2a::structured_input;
use pawl_acp::HostEvent;
use pawl_protocol::{
    global_id, A2aAgentCardSummary, A2aSkillSummary, Agent, AgentSource, EventType, PermissionOption, PermissionOptionKind, PermissionRequest, Session,
    SessionId, SessionSource, SessionStatus, StopReason, ToolCallPayload, ToolKind, UnixMs, A2A_AGENT_PREFIX,
};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::Hub;

/// Longest status text kept as a permission request title.
const TITLE_CHARS: usize = 200;
/// Longest agent text taken from one event.
const TEXT_CHARS: usize = 64 * 1024;

/// A fetched card.
#[derive(Clone)]
pub(crate) struct CachedCard {
    pub card: AgentCard,
    pub etag: Option<String>,
    pub signed: bool,
    pub fetched_at: UnixMs,
    /// The URL it was fetched for (a changed URL fetches again from scratch).
    pub url: String,
}

struct RemoteSession {
    session: Session,
    endpoint: Endpoint,
    streaming: bool,
    /// The agent's context id (after its first answer).
    context: Option<String>,
    /// The agent's task of the turn in progress, or waiting on the user.
    task: Option<String>,
    /// The task asked a question (no approval card): the next prompt answers it.
    waiting_text: bool,
    /// The reader of the current turn's stream: (sequence, abort handle).
    turn: Option<(u64, tokio::task::AbortHandle)>,
    seq: u64,
}

struct PendingInput {
    session_id: SessionId,
    remote_task: String,
    input_id: Option<String>,
    property: String,
    /// option id -> value to send.
    values: HashMap<String, Value>,
}

#[derive(Default)]
struct RemoteState {
    cards: HashMap<String, CachedCard>,
    errors: HashMap<String, String>,
    sessions: HashMap<SessionId, RemoteSession>,
    requests: HashMap<String, PendingInput>,
}

/// The runtime of outside A2A agents' sessions.
pub(crate) struct RemoteAgents {
    machine_id: String,
    http: a2ac::HttpClient,
    events: mpsc::UnboundedSender<HostEvent>,
    st: Mutex<RemoteState>,
}

fn text_of(m: &Message) -> String {
    let t: String = m
        .parts
        .iter()
        .filter_map(|p| match &p.content {
            PartContent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    t.chars().take(TEXT_CHARS).collect()
}

fn parts_text(parts: &[Part]) -> String {
    let mut out = Vec::new();
    for p in parts {
        match &p.content {
            PartContent::Text(t) => out.push(t.clone()),
            PartContent::Data(_) => out.push("[structured data]".into()),
            PartContent::Raw(_) | PartContent::Url(_) => out.push(format!("[file{}]", p.filename.as_deref().map(|f| format!(": {f}")).unwrap_or_default())),
        }
    }
    out.join("\n").chars().take(TEXT_CHARS).collect()
}

/// An option kind for an enum value with no Pawl-style details: its word decides.
fn kind_for_value(v: &str) -> PermissionOptionKind {
    let w = v.to_ascii_lowercase().replace(['-', ' '], "_");
    if matches!(w.as_str(), "reject" | "deny" | "no" | "cancel" | "decline" | "abort" | "reject_once" | "refuse" | "stop") || w.starts_with("reject") || w.starts_with("deny") {
        PermissionOptionKind::RejectOnce
    } else if w.contains("always") {
        PermissionOptionKind::AllowAlways
    } else {
        PermissionOptionKind::AllowOnce
    }
}

fn kind_from_name(s: &str) -> Option<PermissionOptionKind> {
    serde_json::from_value(json!(s)).ok()
}

/// Pawl-style details of an input request (another Pawl's `request_details_part`): the tool call and
/// the options' names and kinds.
fn details_of(m: &Message) -> Option<&serde_json::Map<String, Value>> {
    m.parts.iter().find_map(|p| match &p.content {
        PartContent::Data(Value::Object(o)) if o.contains_key("toolCall") && o.contains_key("options") => Some(o),
        _ => None,
    })
}

impl RemoteAgents {
    pub(crate) fn new(machine_id: String, events: mpsc::UnboundedSender<HostEvent>) -> Self {
        Self { machine_id, http: a2ac::http_client(), events, st: Mutex::new(RemoteState::default()) }
    }

    fn st(&self) -> MutexGuard<'_, RemoteState> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn emit(&self, ev: HostEvent) {
        let _ = self.events.send(ev);
    }

    fn event(&self, session_id: &str, kind: EventType, payload: Value) {
        self.emit(HostEvent::Event { session_id: session_id.to_string(), kind, payload });
    }

    pub(crate) fn owns_session(&self, id: &str) -> bool {
        self.st().sessions.contains_key(id)
    }

    pub(crate) fn owns_request(&self, id: &str) -> bool {
        self.st().requests.contains_key(id)
    }

    pub(crate) fn card(&self, slug: &str) -> Option<CachedCard> {
        self.st().cards.get(slug).cloned()
    }

    pub(crate) fn error(&self, slug: &str) -> Option<String> {
        self.st().errors.get(slug).cloned()
    }

    /// Fetch (or revalidate) the card of agent `slug` at `url`. Errors are kept for Settings.
    pub(crate) async fn refresh_card(&self, slug: &str, url: &str, token: Option<&str>) {
        let cached = self.card(slug).filter(|c| c.url == url);
        let etag = cached.as_ref().and_then(|c| c.etag.clone());
        match a2ac::fetch_card(&self.http, url, etag.as_deref(), token).await {
            Ok(CardFetch::Fresh { card, etag, signed }) => {
                let mut st = self.st();
                st.errors.remove(slug);
                st.cards.insert(slug.to_string(), CachedCard { card: *card, etag, signed, fetched_at: pawl_protocol::now_ms(), url: url.to_string() });
            }
            Ok(CardFetch::NotModified) => {
                let mut st = self.st();
                st.errors.remove(slug);
                if let Some(c) = st.cards.get_mut(slug) {
                    c.fetched_at = pawl_protocol::now_ms();
                }
            }
            Err(e) => {
                tracing::warn!(agent = %slug, "A2A agent card: {e}");
                let mut st = self.st();
                st.errors.insert(slug.to_string(), e.to_string());
                if cached.is_none() {
                    st.cards.remove(slug);
                }
            }
        }
    }

    /// Forget agents that are no longer configured.
    pub(crate) fn retain(&self, slugs: &[String]) {
        let mut st = self.st();
        st.cards.retain(|k, _| slugs.contains(k));
        st.errors.retain(|k, _| slugs.contains(k));
    }

    /// A new session of the agent behind `endpoint` (nothing is sent until the first prompt).
    pub(crate) fn launch(&self, session: Session, endpoint: Endpoint, streaming: bool) {
        self.st().sessions.insert(
            session.id.clone(),
            RemoteSession { session, endpoint, streaming, context: None, task: None, waiting_text: false, turn: None, seq: 0 },
        );
    }

    fn set_status(&self, session_id: &str, status: SessionStatus, ended: bool) {
        let s = {
            let mut st = self.st();
            let Some(rs) = st.sessions.get_mut(session_id) else { return };
            rs.session.status = status;
            rs.session.last_event_at = pawl_protocol::now_ms();
            if ended {
                rs.session.ended_at = Some(rs.session.last_event_at);
            }
            rs.session.clone()
        };
        self.emit(HostEvent::Session(s));
    }

    /// A prompt: a new turn of the agent (or the answer to its question).
    pub(crate) async fn prompt(self: &Arc<Self>, session_id: &str, text: String) -> Result<()> {
        let (endpoint, streaming, mut message) = {
            let st = self.st();
            let rs = st.sessions.get(session_id).ok_or_else(|| anyhow!("unknown A2A session {session_id}"))?;
            if rs.turn.is_some() {
                bail!("session `{session_id}` is already running a prompt; cancel it first");
            }
            let mut m = Message::new(Role::User, vec![Part::text(text.clone())]);
            m.context_id = rs.context.clone();
            m.task_id = rs.waiting_text.then(|| rs.task.clone()).flatten();
            (rs.endpoint.clone(), rs.streaming, m)
        };
        message.metadata = None;
        let req = SendMessageRequest { message, configuration: None, metadata: None, tenant: None };
        self.event(session_id, EventType::UserMessage, json!({ "text": text }));
        self.set_status(session_id, SessionStatus::Thinking, false);
        if let Err(e) = self.start_turn(session_id, &endpoint, streaming, req).await {
            self.set_status(session_id, SessionStatus::Error, false);
            return Err(anyhow!("the A2A agent did not take the prompt: {e}"));
        }
        if let Some(rs) = self.st().sessions.get_mut(session_id) {
            rs.waiting_text = false;
        }
        Ok(())
    }

    /// Send `req` and read the agent's answer in the background (a stream, or one answer).
    async fn start_turn(self: &Arc<Self>, session_id: &str, endpoint: &Endpoint, streaming: bool, req: SendMessageRequest) -> Result<(), ClientError> {
        let seq = {
            let mut st = self.st();
            let Some(rs) = st.sessions.get_mut(session_id) else { return Ok(()) };
            rs.seq += 1;
            rs.seq
        };
        let me = self.clone();
        let sid = session_id.to_string();
        let handle = if streaming {
            let stream = endpoint.send_streaming(&self.http, &req).await?;
            tokio::spawn(async move {
                let mut stream = stream;
                let mut failed = None;
                while let Some(item) = stream.next().await {
                    match item {
                        Ok(ev) => me.apply(&sid, ev),
                        Err(e) => {
                            failed = Some(e);
                            break;
                        }
                    }
                }
                me.turn_ended(&sid, seq, failed);
            })
        } else {
            let mut req = req;
            req.configuration = Some(SendMessageConfiguration { accepted_output_modes: None, task_push_notification_config: None, history_length: Some(0), return_immediately: Some(false) });
            let endpoint = endpoint.clone();
            tokio::spawn(async move {
                match endpoint.send_message(&me.http, &req).await {
                    Ok(SendMessageResponse::Task(t)) => me.apply(&sid, StreamResponse::Task(t)),
                    Ok(SendMessageResponse::Message(m)) => me.apply(&sid, StreamResponse::Message(m)),
                    Err(e) => return me.turn_ended(&sid, seq, Some(e)),
                }
                me.turn_ended(&sid, seq, None);
            })
        };
        if let Some(rs) = self.st().sessions.get_mut(session_id) {
            rs.turn = Some((seq, handle.abort_handle()));
        }
        Ok(())
    }

    /// A turn's reader finished: an error (or a stream that ended mid-task) fails the turn.
    fn turn_ended(&self, session_id: &str, seq: u64, failed: Option<ClientError>) {
        let unfinished = {
            let mut guard = self.st();
            let st = &mut *guard;
            let asked = st.requests.values().any(|r| r.session_id == session_id);
            let Some(rs) = st.sessions.get_mut(session_id) else { return };
            if rs.turn.as_ref().map(|t| t.0) != Some(seq) {
                return;
            }
            rs.turn = None;
            let waiting = rs.waiting_text || asked;
            let running = rs.task.is_some() && !waiting;
            if failed.is_some() || running {
                rs.task = None;
            }
            running
        };
        let message = match failed {
            Some(e) => Some(format!("A2A agent: {e}")),
            None if unfinished => Some("A2A agent: the stream ended before the task finished".into()),
            None => None,
        };
        if let Some(message) = message {
            self.event(session_id, EventType::Error, json!({ "message": message }));
            self.set_status(session_id, SessionStatus::Error, false);
        }
    }

    /// One event of the agent.
    fn apply(&self, session_id: &str, ev: StreamResponse) {
        match ev {
            StreamResponse::Task(t) => {
                self.track(session_id, &t.context_id, &t.id);
                // A finished snapshot (one-answer agents): its artifacts are the reply.
                if t.status.state.is_terminal() {
                    for a in t.artifacts.iter().flatten() {
                        let text = parts_text(&a.parts);
                        if !text.trim().is_empty() {
                            self.event(session_id, EventType::AgentMessageChunk, json!({ "text": text }));
                        }
                    }
                }
                self.status(session_id, &t.id, t.status);
            }
            StreamResponse::StatusUpdate(s) => {
                self.track(session_id, &s.context_id, &s.task_id);
                self.status(session_id, &s.task_id, s.status);
            }
            StreamResponse::ArtifactUpdate(a) => {
                self.track(session_id, &a.context_id, &a.task_id);
                let text = parts_text(&a.artifact.parts);
                if !text.is_empty() {
                    self.event(session_id, EventType::AgentMessageChunk, json!({ "text": text }));
                }
            }
            StreamResponse::Message(m) => {
                if let Some(c) = &m.context_id {
                    if let Some(rs) = self.st().sessions.get_mut(session_id) {
                        rs.context = Some(c.clone());
                    }
                }
                let text = text_of(&m);
                if !text.is_empty() {
                    self.event(session_id, EventType::AgentMessageChunk, json!({ "text": text }));
                }
                self.stop(session_id, StopReason::EndTurn);
            }
        }
    }

    fn track(&self, session_id: &str, context: &str, task: &str) {
        if let Some(rs) = self.st().sessions.get_mut(session_id) {
            if !context.is_empty() {
                rs.context = Some(context.to_string());
            }
            if !task.is_empty() {
                rs.task = Some(task.to_string());
            }
        }
    }

    fn stop(&self, session_id: &str, reason: StopReason) {
        if let Some(rs) = self.st().sessions.get_mut(session_id) {
            rs.task = None;
            rs.waiting_text = false;
        }
        self.event(session_id, EventType::Stop, json!({ "stopReason": reason }));
        let status = match reason {
            StopReason::Cancelled => SessionStatus::Idle,
            StopReason::Refusal => SessionStatus::Error,
            _ => SessionStatus::Done,
        };
        self.set_status(session_id, status, false);
    }

    fn status(&self, session_id: &str, task_id: &str, s: TaskStatus) {
        let text = s.message.as_ref().map(text_of).unwrap_or_default();
        match s.state {
            TaskState::Submitted | TaskState::Working | TaskState::Unspecified => {
                let current = self.st().sessions.get(session_id).map(|r| r.session.status);
                if current != Some(SessionStatus::Working) {
                    self.set_status(session_id, SessionStatus::Working, false);
                }
                if !text.trim().is_empty() {
                    self.event(session_id, EventType::AgentMessageChunk, json!({ "text": text }));
                }
            }
            TaskState::InputRequired => self.input_required(session_id, task_id, s.message.as_ref(), &text),
            TaskState::AuthRequired => {
                let q = if text.trim().is_empty() { "The agent needs you to sign in before it can go on.".to_string() } else { text };
                self.question(session_id, &q);
            }
            TaskState::Completed => {
                if !text.trim().is_empty() {
                    self.event(session_id, EventType::AgentMessageChunk, json!({ "text": text }));
                }
                self.stop(session_id, StopReason::EndTurn);
            }
            TaskState::Rejected => {
                if !text.trim().is_empty() {
                    self.event(session_id, EventType::AgentMessageChunk, json!({ "text": text }));
                }
                self.stop(session_id, StopReason::Refusal);
            }
            TaskState::Canceled => self.stop(session_id, StopReason::Cancelled),
            TaskState::Failed => {
                if let Some(rs) = self.st().sessions.get_mut(session_id) {
                    rs.task = None;
                }
                let message = if text.trim().is_empty() { "The A2A agent's task failed.".to_string() } else { format!("The A2A agent's task failed: {text}") };
                self.event(session_id, EventType::Error, json!({ "message": message }));
                self.set_status(session_id, SessionStatus::Error, false);
            }
        }
    }

    /// A question without an approval card: shown as the agent's message; the next prompt answers.
    fn question(&self, session_id: &str, text: &str) {
        if let Some(rs) = self.st().sessions.get_mut(session_id) {
            rs.waiting_text = true;
        }
        if !text.trim().is_empty() {
            self.event(session_id, EventType::AgentMessageChunk, json!({ "text": text }));
        }
        // This turn is over for the user; the agent's task waits for the answer.
        self.event(session_id, EventType::Stop, json!({ "stopReason": StopReason::EndTurn }));
        self.set_status(session_id, SessionStatus::Idle, false);
    }

    fn input_required(&self, session_id: &str, task_id: &str, msg: Option<&Message>, text: &str) {
        let Some(msg) = msg else { return self.question(session_id, text) };
        let Some((schema, input_id)) = structured_input::read_input_request(msg) else { return self.question(session_id, text) };
        let Some((property, values)) = structured_input::single_enum_choice(&schema) else {
            let title = schema.get("title").and_then(Value::as_str).unwrap_or("its form");
            let q = format!("{text}\n\n(The agent wants structured input Pawl cannot fill in: {title}. Cancel the session or answer in words.)");
            return self.question(session_id, q.trim());
        };
        let already = self.st().requests.values().any(|r| r.session_id == session_id && r.input_id == input_id && input_id.is_some());
        if already {
            return; // the same request again (validation loop)
        }
        let details = details_of(msg);
        let tool = details.and_then(|d| d.get("toolCall")).cloned().unwrap_or(Value::Null);
        let named: Vec<(String, String, Option<PermissionOptionKind>)> = details
            .and_then(|d| d.get("options"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|o| {
                        Some((o.get("optionId")?.as_str()?.to_string(), o.get("name")?.as_str()?.to_string(), o.get("kind").and_then(Value::as_str).and_then(kind_from_name)))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let options: Vec<PermissionOption> = values
            .iter()
            .map(|v| match named.iter().find(|(id, _, _)| id == v) {
                Some((_, name, kind)) => PermissionOption { option_id: v.clone(), name: name.chars().take(80).collect(), kind: kind.unwrap_or_else(|| kind_for_value(v)) },
                None => PermissionOption { option_id: v.clone(), name: v.chars().take(80).collect(), kind: kind_for_value(v) },
            })
            .collect();
        let kind = tool.get("kind").and_then(|k| serde_json::from_value::<ToolKind>(k.clone()).ok()).unwrap_or(ToolKind::Other);
        let title = tool
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| if text.trim().is_empty() { "The A2A agent asks for your OK".into() } else { text.to_string() });
        let (agent_id, request_id) = {
            let st = self.st();
            let Some(rs) = st.sessions.get(session_id) else { return };
            (rs.session.agent_id.clone(), global_id(&self.machine_id, uuid::Uuid::new_v4()))
        };
        let req = PermissionRequest {
            request_id: request_id.clone(),
            machine_id: self.machine_id.clone(),
            session_id: session_id.to_string(),
            agent_id,
            tool_call: ToolCallPayload {
                tool_call_id: input_id.clone().unwrap_or_else(|| task_id.to_string()),
                title: Some(title.chars().take(TITLE_CHARS).collect()),
                kind: Some(kind),
                status: None,
                locations: tool.get("locations").and_then(|l| serde_json::from_value(l.clone()).ok()),
                raw_input: tool.get("rawInput").cloned(),
            },
            options,
            created_at: pawl_protocol::now_ms(),
        };
        self.st().requests.insert(
            request_id,
            PendingInput {
                session_id: session_id.to_string(),
                remote_task: task_id.to_string(),
                input_id,
                property,
                values: values.into_iter().map(|v| (v.clone(), Value::String(v))).collect(),
            },
        );
        self.set_status(session_id, SessionStatus::WaitingPermission, false);
        self.emit(HostEvent::PermissionRequest(req));
    }

    /// The user's answer: the chosen value as Structured Input, or (None) cancel the agent's task.
    pub(crate) async fn resolve(self: &Arc<Self>, request_id: &str, option: Option<String>) -> Result<()> {
        let Some(p) = self.st().requests.remove(request_id) else { return Ok(()) };
        let restore = |me: &Arc<Self>, p: PendingInput| {
            me.st().requests.insert(request_id.to_string(), p);
        };
        let (endpoint, streaming, context, alive) = {
            let st = self.st();
            let Some(rs) = st.sessions.get(&p.session_id) else { return Ok(()) };
            (rs.endpoint.clone(), rs.streaming, rs.context.clone(), rs.turn.is_some())
        };
        let Some(option) = option else {
            if let Err(e) = endpoint.cancel_task(&self.http, &p.remote_task).await {
                tracing::warn!(session = %p.session_id, "A2A agent did not cancel its task: {e}");
            }
            return Ok(());
        };
        let Some(value) = p.values.get(&option).cloned() else {
            let e = anyhow!("unknown option {option:?}");
            restore(self, p);
            return Err(e);
        };
        let mut m = Message::new(Role::User, vec![structured_input::structured_input_part(p.input_id.as_deref(), json!({ p.property.clone(): value }))]);
        m.task_id = Some(p.remote_task.clone());
        m.context_id = context;
        let mut req = SendMessageRequest { message: m, configuration: None, metadata: None, tenant: None };
        let sent = if alive {
            // The open stream carries what follows.
            req.configuration = Some(SendMessageConfiguration { accepted_output_modes: None, task_push_notification_config: None, history_length: Some(0), return_immediately: Some(true) });
            endpoint.send_message(&self.http, &req).await.map(|_| ())
        } else {
            self.start_turn(&p.session_id, &endpoint, streaming, req).await
        };
        // The agent's own events drive the status from here (they may already have arrived).
        match sent {
            Ok(()) => Ok(()),
            Err(e) => {
                restore(self, p);
                Err(anyhow!("the A2A agent did not take the answer: {e}"))
            }
        }
    }

    /// Stop the running turn: `CancelTask` to the agent, the reader stops, the turn ends cancelled.
    pub(crate) async fn cancel(&self, session_id: &str) -> Result<()> {
        let (endpoint, task, turn) = {
            let mut st = self.st();
            let Some(rs) = st.sessions.get_mut(session_id) else { bail!("unknown A2A session {session_id}") };
            let turn = rs.turn.take();
            let task = rs.task.take();
            rs.waiting_text = false;
            let endpoint = rs.endpoint.clone();
            st.requests.retain(|_, r| r.session_id != session_id);
            (endpoint, task, turn)
        };
        if let Some((_, h)) = turn {
            h.abort();
        }
        if let Some(t) = task {
            if let Err(e) = endpoint.cancel_task(&self.http, &t).await {
                tracing::warn!(session = %session_id, "A2A agent did not cancel its task: {e}");
            }
            self.stop(session_id, StopReason::Cancelled);
        }
        Ok(())
    }

    /// End the session for good.
    pub(crate) async fn close(&self, session_id: &str) -> Result<()> {
        let running = self.st().sessions.get(session_id).is_some_and(|r| r.task.is_some() || r.turn.is_some());
        if running {
            self.cancel(session_id).await?;
        }
        let status = self.st().sessions.get(session_id).map(|r| r.session.status).unwrap_or(SessionStatus::Done);
        self.set_status(session_id, if status == SessionStatus::Error { status } else { SessionStatus::Done }, true);
        self.st().sessions.remove(session_id);
        Ok(())
    }

    /// Hub shutdown: stop every reader.
    pub(crate) fn shutdown(&self) {
        let mut st = self.st();
        for rs in st.sessions.values_mut() {
            if let Some((_, h)) = rs.turn.take() {
                h.abort();
            }
        }
    }
}

/// `a2a:<slug>` -> slug.
pub(crate) fn slug_of(agent_id: &str) -> Option<&str> {
    agent_id.strip_prefix(A2A_AGENT_PREFIX)
}

impl Hub {
    fn remote(&self) -> &Arc<RemoteAgents> {
        &self.inner.remote
    }

    /// The configured outside agents as agents (`installed` once their card was fetched).
    pub(crate) fn a2a_agent_list(&self) -> Vec<Agent> {
        let cfg = self.cfg().a2a.agents.clone();
        cfg.iter()
            .map(|a| {
                let card = self.remote().card(&a.id).filter(|c| c.url == a.url);
                let name = a.name.clone().filter(|n| !n.trim().is_empty()).or_else(|| card.as_ref().map(|c| c.card.name.chars().take(80).collect())).unwrap_or_else(|| a.id.clone());
                Agent {
                    id: format!("{A2A_AGENT_PREFIX}{}", a.id),
                    machine_id: self.inner.machine_id.clone(),
                    name,
                    command: Vec::new(),
                    env: None,
                    version: card.as_ref().map(|c| c.card.version.chars().take(40).collect()).filter(|v: &String| !v.is_empty()),
                    description: card.as_ref().map(|c| c.card.description.chars().take(300).collect()).filter(|d: &String| !d.is_empty()),
                    icon: None,
                    installed: card.is_some(),
                    source: AgentSource::A2a,
                    capabilities: None,
                }
            })
            .collect()
    }

    /// What Settings shows about an outside agent's card.
    pub(crate) fn a2a_card_summary(&self, slug: &str) -> (Option<A2aAgentCardSummary>, Option<String>) {
        let card = self.remote().card(slug).map(|c| {
            let version = c
                .card
                .supported_interfaces
                .iter()
                .filter(|i| i.protocol_binding.eq_ignore_ascii_case("JSONRPC"))
                .map(|i| i.protocol_version.clone())
                .max_by_key(|v| v.starts_with("1."))
                .unwrap_or_default();
            A2aAgentCardSummary {
                name: c.card.name.chars().take(80).collect(),
                description: c.card.description.chars().take(500).collect(),
                version: c.card.version.chars().take(40).collect(),
                protocol_version: version,
                streaming: c.card.capabilities.streaming.unwrap_or(false),
                skills: c
                    .card
                    .skills
                    .iter()
                    .take(50)
                    .map(|s| A2aSkillSummary { id: s.id.chars().take(80).collect(), name: s.name.chars().take(80).collect(), description: s.description.chars().take(300).collect() })
                    .collect(),
                signature: if c.signed { "unverified".into() } else { "unsigned".into() },
                fetched_at: c.fetched_at,
            }
        });
        (card, self.remote().error(slug))
    }

    /// Fetch every configured agent's card again (ETag), then rebuild the agent list.
    pub(crate) async fn refresh_a2a_cards(&self) {
        let agents = self.cfg().a2a.agents.clone();
        self.remote().retain(&agents.iter().map(|a| a.id.clone()).collect::<Vec<_>>());
        for a in &agents {
            self.remote().refresh_card(&a.id, &a.url, a.token.as_deref()).await;
        }
        self.merge_a2a_agents().await;
    }

    /// Replace the outside agents in the agent list (persisted, announced, pets ensured).
    pub(crate) async fn merge_a2a_agents(&self) {
        let list = self.a2a_agent_list();
        let agents = {
            let mut st = self.st();
            st.agents.retain(|a| a.source != AgentSource::A2a);
            st.agents.extend(list.iter().cloned());
            st.agents.clone()
        };
        let a2 = agents.clone();
        if let Err(e) = self.inner.store.run(move |s| s.replace_agents(&a2)).await {
            tracing::warn!("persist agents: {e:#}");
        }
        self.inner.bus.send(pawl_protocol::ServerMsg::Agents { agents });
        for a in list.iter().filter(|a| a.installed) {
            self.ensure_pet(&a.id).await;
        }
        self.refresh_presence();
    }

    /// Start a session of an outside agent (ACP's `launch_session` counterpart).
    pub(super) async fn launch_remote(&self, agent: &Agent) -> Result<Session> {
        let slug = slug_of(&agent.id).ok_or_else(|| anyhow!("{} is not an A2A agent", agent.id))?.to_string();
        let cfg = self.cfg().a2a.agents.iter().find(|a| a.id == slug).cloned().ok_or_else(|| anyhow!("A2A agent {slug} is not configured"))?;
        if self.remote().card(&slug).filter(|c| c.url == cfg.url).is_none() {
            self.remote().refresh_card(&slug, &cfg.url, cfg.token.as_deref()).await;
        }
        let card = self.remote().card(&slug).filter(|c| c.url == cfg.url).ok_or_else(|| {
            let why = self.remote().error(&slug).unwrap_or_else(|| "no card yet".into());
            anyhow!("{} has no Agent Card: {why}", agent.name)
        })?;
        let endpoint = a2ac::endpoint(&card.card, &cfg.url, cfg.token.clone()).map_err(|e| anyhow!("{}: {e}", agent.name))?;
        let now = pawl_protocol::now_ms();
        let session = Session {
            id: global_id(&self.inner.machine_id, uuid::Uuid::new_v4()),
            machine_id: self.inner.machine_id.clone(),
            agent_id: agent.id.clone(),
            acp_session_id: None,
            external_id: None,
            cwd: cfg.url.clone(),
            title: None,
            source: SessionSource::Owned,
            status: SessionStatus::Idle,
            started_at: now,
            last_event_at: now,
            ended_at: None,
        };
        self.remote().launch(session.clone(), endpoint, card.card.capabilities.streaming.unwrap_or(false));
        // An outside agent's questions are never auto-approved (rules ask instead).
        self.mark_remote_driven(&session.id).await;
        let session = self.merge_session(session).await;
        self.record_event(&session.id, EventType::SessionStart, json!({ "agentId": agent.id, "cwd": session.cwd, "a2a": true })).await?;
        Ok(self.session(&session.id).unwrap_or(session))
    }

    // ---------------------------------------------------------------- the executor switch

    /// Send a prompt to the session's agent: ACP, or an outside A2A agent.
    pub(super) async fn exec_prompt(&self, session_id: &str, text: String) -> Result<()> {
        if self.remote().owns_session(session_id) {
            return self.remote().prompt(session_id, text).await;
        }
        self.inner.host.prompt(session_id, text).await
    }

    pub(super) async fn exec_cancel(&self, session_id: &str) -> Result<()> {
        if self.remote().owns_session(session_id) {
            return self.remote().cancel(session_id).await;
        }
        self.inner.host.cancel(session_id).await
    }

    pub(super) async fn exec_resolve(&self, request_id: &str, option_id: Option<String>) -> Result<()> {
        if self.remote().owns_request(request_id) {
            return self.remote().resolve(request_id, option_id).await;
        }
        let owned_by_remote = self.st().pending.get(request_id).is_some_and(|r| self.remote().owns_session(&r.session_id));
        if owned_by_remote {
            return Ok(()); // already answered or withdrawn on the agent's side
        }
        self.inner.host.resolve_permission(request_id, option_id).await
    }

    pub(super) async fn exec_close(&self, session_id: &str) -> Result<()> {
        if self.remote().owns_session(session_id) {
            return self.remote().close(session_id).await;
        }
        self.inner.host.close_session(session_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_values_get_option_kinds_from_their_words() {
        assert_eq!(kind_for_value("approve"), PermissionOptionKind::AllowOnce);
        assert_eq!(kind_for_value("Deny"), PermissionOptionKind::RejectOnce);
        assert_eq!(kind_for_value("reject_once"), PermissionOptionKind::RejectOnce);
        assert_eq!(kind_for_value("allow_always"), PermissionOptionKind::AllowAlways);
        assert_eq!(kind_for_value("staging"), PermissionOptionKind::AllowOnce, "a pick is an affirmative choice");
        assert_eq!(slug_of("a2a:gemini"), Some("gemini"));
        assert_eq!(slug_of("codex"), None);
    }
}
