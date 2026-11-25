//! The A2A view of this machine's ACP sessions: one context per session, one Task per prompt turn.
//!
//! The hub feeds every event of its owned sessions into [`TaskRegistry::on_event`] (in order, from
//! the same place it records them), and starts tasks for the prompts it sends with
//! [`TaskRegistry::begin`] / [`TaskRegistry::commit`] / [`TaskRegistry::abort`]. Readers get task
//! snapshots and a live stream of [`TaskUpdate`]s.
//!
//! Tasks live in memory only (bounded): owned sessions end with the hub process, and the events
//! they are built from are in the hub's event log anyway.
//!
//! **Caller binding:** a context remembers who created it ([`Caller`]). Outside A2A callers see
//! and act on their own contexts only; this machine's own clients and paired hubs (which act for
//! the user) see every context.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

use a2a::{
    Artifact, ListTasksRequest, ListTasksResponse, Message, Part, PartContent, Role, StreamResponse, Task,
    TaskArtifactUpdateEvent, TaskState, TaskStatus, TaskStatusUpdateEvent,
};
use chrono::Utc;
use pawl_protocol::{
    ErrorPayload, EventType, HubEvent, PermissionRequest, PermissionResolvedPayload, PlanPayload, StopPayload,
    ToolCallPayload,
};
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::mapping;
use crate::structured_input;

/// One update of a task (the A2A stream response shapes).
pub type TaskUpdate = StreamResponse;

/// Tasks kept per context (oldest finished ones are dropped first).
pub const TASKS_PER_CONTEXT: usize = 50;
/// Tasks kept in all (oldest finished ones are dropped first).
pub const TASKS_MAX: usize = 2000;
/// Bytes of reply text kept per task; the rest is dropped (metadata `pawl/truncated`).
pub const RESPONSE_MAX_BYTES: usize = 256 * 1024;
/// Updates buffered per subscriber before it lags (it then gets a fresh snapshot).
pub const STREAM_CAPACITY: usize = 512;
/// Default and maximum ListTasks page size (spec 3.1.4: 50 default, 100 max).
pub const LIST_PAGE_DEFAULT: usize = 50;
pub const LIST_PAGE_MAX: usize = 100;

/// Metadata key set when a task's reply text was cut at [`RESPONSE_MAX_BYTES`].
pub const TRUNCATED_KEY: &str = "pawl/truncated";

/// Who created a context (and may act on it).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Caller {
    /// This machine's own clients (desktop, VS Code, CLI), its bot and rules: every context.
    Local,
    /// A paired hub acting for the user (the step-4 owner rules apply on top): every context.
    Peer(String),
    /// An outside A2A client of the localhost A2A server, by caller id: its own contexts only.
    Outside(String),
}

impl Caller {
    /// May this caller see contexts it did not create?
    pub fn sees_all(&self) -> bool {
        !matches!(self, Caller::Outside(_))
    }

    /// Audit label: "local", "peer:<machine>", "a2a:<caller>".
    pub fn label(&self) -> String {
        match self {
            Caller::Local => "local".into(),
            Caller::Peer(m) => format!("peer:{m}"),
            Caller::Outside(c) => format!("a2a:{c}"),
        }
    }
}

struct TaskEntry {
    task: Task,
    /// Creation order (paging cursor).
    seq: u64,
    /// Permission requests of this turn still waiting, oldest first; the first is the one the
    /// status asks about (Structured Input: one pending input request per task).
    pending: VecDeque<PermissionRequest>,
    response_bytes: usize,
    /// The context's previously active task, restored by [`TaskRegistry::abort`].
    prev_active: Option<String>,
    committed: bool,
    tx: broadcast::Sender<TaskUpdate>,
}

#[derive(Default)]
struct ContextEntry {
    owner: Option<Caller>,
    active: Option<String>,
    tasks: VecDeque<String>,
}

#[derive(Default)]
struct State {
    tasks: HashMap<String, TaskEntry>,
    contexts: HashMap<String, ContextEntry>,
    order: VecDeque<String>,
    next_seq: u64,
}

/// See the module docs. Cheap operations under one lock; broadcasting never blocks.
pub struct TaskRegistry {
    machine_id: String,
    st: Mutex<State>,
}

fn now_status(state: TaskState, message: Option<Message>) -> TaskStatus {
    TaskStatus { state, message, timestamp: Some(Utc::now()) }
}

fn status_event(t: &Task, metadata: Option<HashMap<String, Value>>) -> TaskUpdate {
    StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
        task_id: t.id.clone(),
        context_id: t.context_id.clone(),
        status: t.status.clone(),
        metadata,
    })
}

fn artifact_event(t: &Task, artifact: Artifact, append: bool, last_chunk: bool) -> TaskUpdate {
    StreamResponse::ArtifactUpdate(TaskArtifactUpdateEvent {
        task_id: t.id.clone(),
        context_id: t.context_id.clone(),
        artifact,
        append: Some(append),
        last_chunk: Some(last_chunk),
        metadata: None,
    })
}

/// Cut `text` to at most `max` bytes on a char boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut i = max;
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    &text[..i]
}

impl TaskEntry {
    fn publish(&self, u: TaskUpdate) {
        let _ = self.tx.send(u);
    }

    fn set_status(&mut self, state: TaskState, message: Option<Message>, metadata: Option<HashMap<String, Value>>) {
        self.task.status = now_status(state, message);
        if let Some(m) = &metadata {
            self.task.metadata.get_or_insert_with(HashMap::new).extend(m.clone());
        }
        let ev = status_event(&self.task, metadata);
        self.publish(ev);
    }

    fn is_terminal(&self) -> bool {
        self.task.status.state.is_terminal()
    }

    /// The text of the `response` artifact so far.
    fn response_text(&self) -> Option<String> {
        let id = format!("{}/response", self.task.id);
        let a = self.task.artifacts.as_ref()?.iter().find(|a| a.artifact_id == id)?;
        a.parts.iter().find_map(|p| p.as_text().map(str::to_string))
    }

    fn upsert_artifact(&mut self, artifact: Artifact) {
        let list = self.task.artifacts.get_or_insert_with(Vec::new);
        match list.iter_mut().find(|a| a.artifact_id == artifact.artifact_id) {
            Some(a) => *a = artifact,
            None => list.push(artifact),
        }
    }

    fn append_response(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        let room = RESPONSE_MAX_BYTES.saturating_sub(self.response_bytes);
        if room == 0 {
            return;
        }
        let piece = cut(chunk, room).to_string();
        if piece.len() < chunk.len() {
            self.task.metadata.get_or_insert_with(HashMap::new).insert(TRUNCATED_KEY.into(), json!(true));
        }
        let first = self.response_bytes == 0;
        self.response_bytes += piece.len();
        let id = format!("{}/response", self.task.id);
        let list = self.task.artifacts.get_or_insert_with(Vec::new);
        match list.iter_mut().find(|a| a.artifact_id == id) {
            Some(a) => match a.parts.first_mut().map(|p| &mut p.content) {
                Some(PartContent::Text(t)) => t.push_str(&piece),
                _ => a.parts = vec![Part::text(piece.clone())],
            },
            None => list.push(mapping::response_chunk(&self.task.id, &piece)),
        }
        let ev = artifact_event(&self.task, mapping::response_chunk(&self.task.id, &piece), !first, false);
        self.publish(ev);
    }

    fn working(&mut self) {
        if self.task.status.state == TaskState::Submitted {
            self.set_status(TaskState::Working, None, None);
        }
    }

    /// Show the oldest pending request (INPUT_REQUIRED) or go back to WORKING.
    fn show_pending(&mut self, errors: Option<Vec<Value>>) {
        match self.pending.front().cloned() {
            Some(req) => {
                let msg = structured_input::input_required_message(&req, &self.task.context_id, &self.task.id, errors);
                self.set_status(TaskState::InputRequired, Some(msg), None);
            }
            None => self.set_status(TaskState::Working, None, None),
        }
    }

    /// Final state: the reply text becomes the status message and joins the history.
    fn finish(&mut self, state: TaskState, text: Option<String>, metadata: Option<HashMap<String, Value>>) {
        if self.is_terminal() {
            return;
        }
        self.pending.clear();
        let text = text.or_else(|| self.response_text()).filter(|t| !t.trim().is_empty());
        let msg = text.map(|t| mapping::agent_message(t, &self.task.context_id, &self.task.id));
        if let Some(m) = &msg {
            self.task.history.get_or_insert_with(Vec::new).push(m.clone());
        }
        if self.response_bytes > 0 {
            if let Some(a) = self.task.artifacts.as_ref().and_then(|v| v.iter().find(|a| a.name.as_deref() == Some(mapping::RESPONSE_ARTIFACT))).cloned() {
                let ev = artifact_event(&self.task, Artifact { parts: vec![Part::text("")], ..a }, true, true);
                self.publish(ev);
            }
        }
        self.set_status(state, msg, metadata);
    }
}

impl TaskRegistry {
    pub fn new(machine_id: impl Into<String>) -> Self {
        Self { machine_id: machine_id.into(), st: Mutex::new(State::default()) }
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.st.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// A fresh global task id on this machine.
    fn new_task_id(&self) -> String {
        pawl_protocol::global_id(&self.machine_id, a2a::new_task_id())
    }

    /// Record who created `context_id` (first binding wins).
    pub fn bind_context(&self, context_id: &str, owner: Caller) {
        let mut st = self.st();
        let c = st.contexts.entry(context_id.to_string()).or_default();
        if c.owner.is_none() {
            c.owner = Some(owner);
        }
    }

    /// Who created `context_id` (contexts nobody bound belong to this machine's own clients).
    pub fn owner_of(&self, context_id: &str) -> Caller {
        self.st().contexts.get(context_id).and_then(|c| c.owner.clone()).unwrap_or(Caller::Local)
    }

    /// May `caller` see and act on `context_id`?
    pub fn can_access(&self, caller: &Caller, context_id: &str) -> bool {
        caller.sees_all() || self.owner_of(context_id) == *caller
    }

    /// Start a task for a prompt about to be sent to `context_id`'s agent. It is the context's
    /// active task at once (so the turn's events attach to it); call [`Self::commit`] once the
    /// agent accepted the prompt, or [`Self::abort`] if it did not.
    pub fn begin(&self, context_id: &str, text: &str, caller: &Caller) -> Task {
        let id = self.new_task_id();
        let mut user = Message::new(Role::User, vec![Part::text(text)]);
        user.context_id = Some(context_id.to_string());
        user.task_id = Some(id.clone());
        let task = Task {
            id: id.clone(),
            context_id: context_id.to_string(),
            status: now_status(TaskState::Submitted, None),
            artifacts: None,
            history: Some(vec![user]),
            metadata: None,
        };
        let mut st = self.st();
        st.next_seq += 1;
        let seq = st.next_seq;
        let ctx = st.contexts.entry(context_id.to_string()).or_default();
        if ctx.owner.is_none() {
            ctx.owner = Some(caller.clone());
        }
        let prev_active = ctx.active.replace(id.clone());
        ctx.tasks.push_back(id.clone());
        let (tx, _) = broadcast::channel(STREAM_CAPACITY);
        st.tasks.insert(
            id.clone(),
            TaskEntry { task: task.clone(), seq, pending: VecDeque::new(), response_bytes: 0, prev_active, committed: false, tx },
        );
        st.order.push_back(id);
        Self::evict(&mut st, context_id);
        task
    }

    /// The agent accepted the prompt of `task_id`. A previous task of the context that never saw
    /// its end (the agent's turn is evidently over) is closed as failed.
    pub fn commit(&self, task_id: &str) {
        let mut st = self.st();
        let Some(e) = st.tasks.get_mut(task_id) else { return };
        e.committed = true;
        let Some(prev) = e.prev_active.take() else { return };
        if let Some(p) = st.tasks.get_mut(&prev) {
            p.finish(TaskState::Failed, Some("The turn ended without a stop reason.".into()), None);
        }
    }

    /// The prompt of `task_id` was not sent: forget the task as if it never existed.
    pub fn abort(&self, task_id: &str) {
        let mut st = self.st();
        let Some(e) = st.tasks.remove(task_id) else { return };
        st.order.retain(|t| t != task_id);
        if let Some(ctx) = st.contexts.get_mut(&e.task.context_id) {
            ctx.tasks.retain(|t| t != task_id);
            if ctx.active.as_deref() == Some(task_id) {
                ctx.active = e.prev_active;
            }
        }
    }

    /// Drop the oldest finished tasks over the caps (never an active one).
    fn evict(st: &mut State, context_id: &str) {
        let active: Vec<String> = st.contexts.values().filter_map(|c| c.active.clone()).collect();
        let finished = |st: &State, id: &str| !active.iter().any(|a| a == id) && st.tasks.get(id).is_none_or(|e| e.is_terminal());
        loop {
            let over = st.contexts.get(context_id).map(|c| c.tasks.len()).unwrap_or(0) > TASKS_PER_CONTEXT;
            if !over {
                break;
            }
            let victim = st.contexts.get(context_id).and_then(|c| c.tasks.iter().find(|t| finished(st, t)).cloned());
            let Some(v) = victim else { break };
            Self::drop_task(st, &v);
        }
        while st.order.len() > TASKS_MAX {
            let victim = st.order.iter().find(|t| finished(st, t)).cloned();
            let Some(v) = victim else { break };
            Self::drop_task(st, &v);
        }
    }

    fn drop_task(st: &mut State, id: &str) {
        st.order.retain(|t| t != id);
        if let Some(e) = st.tasks.remove(id) {
            if let Some(c) = st.contexts.get_mut(&e.task.context_id) {
                c.tasks.retain(|t| t != id);
            }
        }
    }

    /// The context's task the next event belongs to (implicitly created for a user message sent
    /// by someone other than the orchestrator, e.g. a prompt typed into a client of an old hub).
    fn active_entry<'a>(st: &'a mut State, ctx: &str) -> Option<&'a mut TaskEntry> {
        let id = st.contexts.get(ctx)?.active.clone()?;
        st.tasks.get_mut(&id).filter(|e| !e.is_terminal())
    }

    /// Apply one hub event of an owned session (the session id is the context id).
    pub fn on_event(&self, ev: &HubEvent) {
        let ctx = ev.session_id.as_str();
        let mut st = self.st();
        if ev.kind == EventType::UserMessage && Self::active_entry(&mut st, ctx).is_none() {
            let text = ev.payload.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
            drop(st);
            let t = self.begin(ctx, &text, &Caller::Local);
            self.commit(&t.id);
            return;
        }
        let Some(e) = Self::active_entry(&mut st, ctx) else { return };
        match ev.kind {
            EventType::UserMessage | EventType::SessionStart | EventType::Notification | EventType::Hook => {}
            EventType::AgentThoughtChunk | EventType::ToolCallUpdate => e.working(),
            EventType::AgentMessageChunk => {
                e.working();
                if let Some(t) = ev.payload.get("text").and_then(Value::as_str) {
                    e.append_response(t);
                }
            }
            EventType::ToolCall => {
                let tc: ToolCallPayload = serde_json::from_value(ev.payload.clone()).unwrap_or_default();
                if let Some(a) = mapping::diff_artifact(&tc) {
                    e.upsert_artifact(a.clone());
                    let u = artifact_event(&e.task, a, false, true);
                    e.publish(u);
                }
                if e.pending.is_empty() {
                    let msg = mapping::agent_message(mapping::tool_status_text(&tc), &e.task.context_id, &e.task.id);
                    e.set_status(TaskState::Working, Some(msg), None);
                }
            }
            EventType::Plan => {
                if let Ok(plan) = serde_json::from_value::<PlanPayload>(ev.payload.clone()) {
                    e.working();
                    let a = mapping::plan_artifact(&plan);
                    e.upsert_artifact(a.clone());
                    let u = artifact_event(&e.task, a, false, true);
                    e.publish(u);
                }
            }
            EventType::RequestPermission => {
                if let Ok(req) = serde_json::from_value::<PermissionRequest>(ev.payload.clone()) {
                    if !e.pending.iter().any(|p| p.request_id == req.request_id) {
                        e.pending.push_back(req);
                        if e.pending.len() == 1 {
                            e.show_pending(None);
                        }
                    }
                }
            }
            EventType::PermissionResolved => {
                if let Ok(p) = serde_json::from_value::<PermissionResolvedPayload>(ev.payload.clone()) {
                    let was_first = e.pending.front().is_some_and(|r| r.request_id == p.request_id);
                    e.pending.retain(|r| r.request_id != p.request_id);
                    if was_first {
                        e.show_pending(None);
                    }
                }
            }
            EventType::Stop => {
                if let Ok(stop) = serde_json::from_value::<StopPayload>(ev.payload.clone()) {
                    let meta = mapping::stop_metadata(stop.stop_reason);
                    e.finish(mapping::state_for_stop(stop.stop_reason), None, Some(meta));
                    Self::clear_active(&mut st, ctx);
                }
            }
            EventType::Error => {
                let msg = serde_json::from_value::<ErrorPayload>(ev.payload.clone()).map(|e| e.message).unwrap_or_else(|_| "The agent failed.".into());
                e.finish(TaskState::Failed, Some(msg), None);
                Self::clear_active(&mut st, ctx);
            }
            EventType::SessionEnd => {
                e.finish(TaskState::Failed, Some("The session ended.".into()), None);
                Self::clear_active(&mut st, ctx);
            }
        }
    }

    fn clear_active(st: &mut State, ctx: &str) {
        if let Some(c) = st.contexts.get_mut(ctx) {
            c.active = None;
        }
    }

    /// The session behind `context_id` ended (agent exit, closed): its running task fails.
    pub fn on_session_ended(&self, context_id: &str) {
        let mut st = self.st();
        if let Some(e) = Self::active_entry(&mut st, context_id) {
            e.finish(TaskState::Failed, Some("The session ended.".into()), None);
        }
        Self::clear_active(&mut st, context_id);
    }

    /// Mark `task_id` canceled (after the agent accepted `session/cancel`). Returns the task.
    pub fn cancel(&self, task_id: &str) -> Option<Task> {
        let mut st = self.st();
        let e = st.tasks.get_mut(task_id)?;
        let ctx = e.task.context_id.clone();
        if !e.is_terminal() {
            e.finish(TaskState::Canceled, None, Some(HashMap::from([(mapping::STOP_REASON_KEY.into(), json!("cancelled"))])));
            Self::clear_active(&mut st, &ctx);
        }
        st.tasks.get(task_id).map(|e| e.task.clone())
    }

    /// Re-publish the pending input request of `task_id` with validation errors (spec 7).
    pub fn reject_input(&self, task_id: &str, errors: Option<Vec<Value>>) {
        let mut st = self.st();
        if let Some(e) = st.tasks.get_mut(task_id).filter(|e| !e.is_terminal()) {
            e.show_pending(errors);
        }
    }

    /// The permission request `task_id` is waiting on (its oldest pending one).
    pub fn pending_request(&self, task_id: &str) -> Option<PermissionRequest> {
        self.st().tasks.get(task_id).and_then(|e| e.pending.front().cloned())
    }

    /// Snapshot of `task_id`; `history_length` per spec 3.2.4 (None = all, 0 = none).
    pub fn get(&self, task_id: &str, history_length: Option<i32>) -> Option<Task> {
        let mut t = self.st().tasks.get(task_id).map(|e| e.task.clone())?;
        trim_history(&mut t, history_length);
        Some(t)
    }

    /// The context a task belongs to.
    pub fn context_of(&self, task_id: &str) -> Option<String> {
        self.st().tasks.get(task_id).map(|e| e.task.context_id.clone())
    }

    /// The running (non-terminal) task of `context_id`.
    pub fn active_task(&self, context_id: &str) -> Option<Task> {
        let mut st = self.st();
        Self::active_entry(&mut st, context_id).map(|e| e.task.clone())
    }

    /// The newest task of every context, newest first (for status summaries).
    pub fn latest_per_context(&self) -> Vec<Task> {
        let st = self.st();
        let mut v: Vec<(u64, Task)> = st
            .contexts
            .values()
            .filter_map(|c| c.tasks.back().and_then(|id| st.tasks.get(id)).map(|e| (e.seq, e.task.clone())))
            .collect();
        v.sort_by_key(|(seq, _)| std::cmp::Reverse(*seq));
        v.into_iter().map(|(_, t)| t).collect()
    }

    /// Live updates of a running task, with its snapshot at the moment of subscribing. None for an
    /// unknown task; Some((snapshot, None)) for a finished one.
    pub fn subscribe(&self, task_id: &str) -> Option<(Task, Option<broadcast::Receiver<TaskUpdate>>)> {
        let st = self.st();
        let e = st.tasks.get(task_id)?;
        let rx = (!e.is_terminal()).then(|| e.tx.subscribe());
        Some((e.task.clone(), rx))
    }

    /// ListTasks for `caller` (spec 3.1.4): newest first, filtered by context and state, paged by
    /// an opaque cursor (the creation seq of the last task of the previous page).
    pub fn list(&self, caller: &Caller, req: &ListTasksRequest) -> Result<ListTasksResponse, String> {
        let page = match req.page_size {
            None | Some(0) => LIST_PAGE_DEFAULT,
            Some(n) if n < 0 => return Err("pageSize must be positive".into()),
            Some(n) => (n as usize).min(LIST_PAGE_MAX),
        };
        let after: Option<u64> = match req.page_token.as_deref().filter(|t| !t.is_empty()) {
            Some(t) => Some(t.parse().map_err(|_| "invalid pageToken".to_string())?),
            None => None,
        };
        let st = self.st();
        let visible = |e: &&TaskEntry| {
            let owner = st.contexts.get(&e.task.context_id).and_then(|c| c.owner.clone()).unwrap_or(Caller::Local);
            (caller.sees_all() || owner == *caller)
                && req.context_id.as_ref().is_none_or(|c| *c == e.task.context_id)
                && req.status.as_ref().is_none_or(|s| *s == e.task.status.state)
                && req.status_timestamp_after.is_none_or(|after| e.task.status.timestamp.is_some_and(|ts| ts > after))
        };
        let mut all: Vec<&TaskEntry> = st.tasks.values().filter(visible).collect();
        all.sort_by_key(|e| std::cmp::Reverse(e.seq));
        let total = all.len();
        let rest: Vec<&TaskEntry> = all.into_iter().filter(|e| after.is_none_or(|a| e.seq < a)).collect();
        let more = rest.len() > page;
        let chosen: Vec<&TaskEntry> = rest.into_iter().take(page).collect();
        let next_page_token = if more { chosen.last().map(|e| e.seq.to_string()).unwrap_or_default() } else { String::new() };
        let include_artifacts = req.include_artifacts.unwrap_or(false);
        let tasks = chosen
            .iter()
            .map(|e| {
                let mut t = e.task.clone();
                trim_history(&mut t, req.history_length);
                if !include_artifacts {
                    t.artifacts = None;
                }
                t
            })
            .collect::<Vec<_>>();
        Ok(ListTasksResponse { page_size: tasks.len() as i32, tasks, next_page_token, total_size: total as i32 })
    }
}

/// Spec 3.2.4: `history_length` 0 = no history, n = the newest n messages, None = all.
pub fn trim_history(t: &mut Task, history_length: Option<i32>) {
    match history_length {
        None => {}
        Some(n) if n <= 0 => t.history = None,
        Some(n) => {
            if let Some(h) = t.history.as_mut() {
                let keep = n as usize;
                if h.len() > keep {
                    h.drain(..h.len() - keep);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
