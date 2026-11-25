//! The localhost A2A server (PLAN.md "Pawl as an A2A agent", phase A2): the orchestrator as an A2A
//! agent for outside callers on this machine.
//!
//! Transport and security (127.0.0.1 only, loopback Host / Origin, bearer tokens, rate limits,
//! 0.3 + 1.0) are `pawl_a2a::server`'s. This module is the policy behind it:
//!
//! - A caller is `Caller::Outside(<id>)`: it sees and acts on the contexts it created only.
//! - It starts agents only where a paired machine could (an installed agent, a folder this machine
//!   already knows: its repo index or `[peer] start_roots`), and its sessions are remote-driven (rules
//!   never auto-approve their requests).
//! - It answers permission requests only if the user allowed it for that caller, and then never
//!   "always" and never allow on execute / delete / fetch (see `core/a2a.rs` `Answers::Limited`).
//! - Every call is logged in `a2a_log` as `a2a:<id>` (method, target, outcome; never message text or
//!   tokens); decisions are in the permission audit log with that actor.
//! - `SendMessage` blocks until the task finishes or asks for input (spec 3.2.2) unless the caller
//!   sets `returnImmediately`, for at most [`BLOCKING_WAIT`]; streams follow a task until it ends.

use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::Result;
use futures::stream::BoxStream;
use futures::StreamExt;
use pawl_a2a::card::{self, CardInfo};
use pawl_a2a::server::{self as srv, CallerAuth, RunningServer, ServerState};
use pawl_a2a::server_sdk::{RequestHandler, ServiceParams};
use pawl_a2a::spec::{
    error_code, A2AError, AgentCard, CancelTaskRequest, DeleteTaskPushNotificationConfigRequest, GetExtendedAgentCardRequest, GetTaskPushNotificationConfigRequest,
    GetTaskRequest, ListTaskPushNotificationConfigsRequest, ListTaskPushNotificationConfigsResponse, ListTasksRequest, ListTasksResponse, SendMessageRequest,
    SendMessageResponse, StreamResponse, SubscribeToTaskRequest, Task, TaskPushNotificationConfig, TaskState,
};
use pawl_a2a::{Caller, DelegateParams};
use pawl_protocol::{A2aTaskSummary, IntentRejectReason};
use serde_json::{json, Value};
use tokio::sync::broadcast::error::RecvError;

use super::a2a::{A2aError, Answers};
use super::{AckCode, Actor, Hub, Inner};
use crate::settings::{self as cfgset, InvalidSetting};
use crate::store::{A2aLogEntry, SettingsAuditKind};

/// Longest a blocking `SendMessage` waits for its task to finish or ask for input.
pub const BLOCKING_WAIT: Duration = Duration::from_secs(60);

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
fn new_token() -> Result<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| anyhow::anyhow!("no randomness for a token: {e}"))?;
    Ok(hex::encode(b))
}

fn ms(ts: Option<chrono::DateTime<chrono::Utc>>) -> Option<i64> {
    ts.map(|t| t.timestamp_millis())
}

impl Hub {
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
        let state = ServerState::new(callers_of(&cfg), move || match card_weak.upgrade() {
            Some(inner) => {
                let hub = Hub { inner };
                card::pawl_card(&hub.card_info(hub.a2a_status().port, false), false)
            }
            None => card::pawl_card(&CardInfo::default(), false),
        });
        match srv::serve(Arc::new(HubHandler { hub: weak }), state.clone(), cfg.port).await {
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
        tracing::info!(caller = %caller, method, status = %status, "A2A call");
        let entry = A2aLogEntry { id: 0, ts: pawl_protocol::now_ms(), caller: format!("a2a:{caller}"), method: method.to_string(), target, status, detail: None };
        if let Err(e) = self.inner.store.run(move |s| s.insert_a2a_log(&entry)).await {
            tracing::error!("a2a log write failed: {e:#}");
        }
    }
}

/// The hub's error as the A2A error an outside caller gets.
fn to_a2a(e: &anyhow::Error) -> A2AError {
    let text = format!("{e:#}");
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
        Some(AckCode::Forbidden) => A2AError::unsupported_operation(text),
        Some(AckCode::AlreadyDecided) => A2AError::unsupported_operation("that request was already answered"),
        _ => A2AError::invalid_params(text),
    }
}

fn reject_reason(r: IntentRejectReason) -> A2AError {
    match r {
        IntentRejectReason::Forbidden => A2AError::unsupported_operation(
            "forbidden: A2A callers start agents only in folders this machine already uses (its repos or [peer] start roots)",
        ),
        _ => A2AError::invalid_params("cwd must be an existing absolute folder"),
    }
}

fn interrupted_or_final(s: &TaskState) -> bool {
    s.is_terminal() || matches!(s, TaskState::InputRequired | TaskState::AuthRequired)
}

/// The SDK's request handler over the hub (weak: the server never keeps a stopped hub alive).
struct HubHandler {
    hub: Weak<Inner>,
}

/// Who is calling and what they may do.
struct Who {
    hub: Hub,
    id: String,
    caller: Caller,
    actor: Actor,
    answers: Answers,
}

impl HubHandler {
    fn who(&self, params: &ServiceParams) -> Result<Who, A2AError> {
        let inner = self.hub.upgrade().ok_or_else(|| A2AError::internal("the hub is shutting down"))?;
        let hub = Hub { inner };
        let id = srv::caller_of(params).ok_or_else(|| A2AError::internal("no caller"))?.to_string();
        let allow = {
            let cfg = hub.cfg();
            let Some(c) = cfg.a2a.callers.iter().find(|c| c.id == id) else {
                return Err(A2AError::invalid_request("this caller is no longer configured"));
            };
            c.allow_approvals
        };
        Ok(Who {
            caller: Caller::Outside(id.clone()),
            actor: Actor::internal(format!("a2a:{id}")),
            answers: if allow { Answers::Limited } else { Answers::Never },
            hub,
            id,
        })
    }

    /// The outside-caller policy for a start, then the facade.
    async fn send(&self, w: &Who, mut req: SendMessageRequest) -> Result<SendMessageResponse, A2AError> {
        let m = &req.message;
        if m.task_id.is_none() && m.context_id.is_none() {
            let mut params = DelegateParams::of(m).ok_or_else(|| A2AError::invalid_params("a message without a contextId starts a session: give agentId and cwd in its delegate metadata"))?;
            if params.machine_id.as_deref().is_some_and(|x| x != w.hub.machine_id()) {
                return Err(A2AError::unsupported_operation("this server starts sessions on this machine only"));
            }
            // A caller's own word is not a user's confirmation.
            params.confirmed = None;
            let agent_id = params.agent_id.clone().unwrap_or_default();
            if !w.hub.agents().iter().any(|a| a.id == agent_id && a.installed) {
                return Err(A2AError::invalid_params(format!("agent {agent_id:?} is not installed on this machine")));
            }
            let dir = w.hub.remote_start_dir(params.cwd.as_deref().unwrap_or_default()).await.map_err(reject_reason)?;
            params.cwd = Some(dir.to_string_lossy().into_owned());
            params.attach(&mut req.message);
        }
        let d = w.hub.a2a_send(req, &w.caller, &w.actor, w.answers).await.map_err(|e| to_a2a(&e))?;
        Ok(d.response)
    }

    /// Wait (bounded) until `task` finishes or asks for input.
    async fn settle(&self, w: &Who, task: Task, history_length: Option<i32>) -> Task {
        if interrupted_or_final(&task.status.state) {
            return w.hub.a2a_get(&task.id, history_length, &w.caller).unwrap_or(task);
        }
        let Some((_, Some(mut rx))) = w.hub.tasks().subscribe(&task.id) else {
            return w.hub.a2a_get(&task.id, history_length, &w.caller).unwrap_or(task);
        };
        let wait = async {
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
        let _ = tokio::time::timeout(BLOCKING_WAIT, wait).await;
        w.hub.a2a_get(&task.id, history_length, &w.caller).unwrap_or(task)
    }

    /// A task's stream: its snapshot, then its updates until it ends (a lagging reader gets a fresh
    /// snapshot instead of the updates it missed).
    fn follow(hub: Hub, caller: Caller, task_id: String) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        let Some((snapshot, rx)) = hub.tasks().subscribe(&task_id) else {
            return futures::stream::iter(vec![Err(A2AError::task_not_found(&task_id))]).boxed();
        };
        let first = futures::stream::iter(vec![Ok(StreamResponse::Task(snapshot))]);
        let Some(rx) = rx else { return first.boxed() };
        let rest = futures::stream::unfold(Some((rx, hub, caller, task_id)), |state| async move {
            let (mut rx, hub, caller, task_id) = state?;
            match rx.recv().await {
                Ok(ev) => {
                    let done = matches!(&ev, StreamResponse::StatusUpdate(s) if s.status.state.is_terminal());
                    let next = (!done).then_some((rx, hub, caller, task_id));
                    Some((Ok(ev), next))
                }
                Err(RecvError::Lagged(_)) => {
                    let snap = hub.a2a_get(&task_id, None, &caller).ok()?;
                    let done = snap.status.state.is_terminal();
                    let next = (!done).then_some((rx, hub, caller, task_id));
                    Some((Ok(StreamResponse::Task(snap)), next))
                }
                Err(RecvError::Closed) => None,
            }
        });
        first.chain(rest).boxed()
    }
}

fn target_of(req: &SendMessageRequest) -> Option<String> {
    req.message.task_id.clone().or_else(|| req.message.context_id.clone())
}

#[pawl_a2a::async_trait]
impl RequestHandler for HubHandler {
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

    async fn send_streaming_message(&self, params: &ServiceParams, req: SendMessageRequest) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        let w = self.who(params)?;
        let target = target_of(&req);
        let r = self.send(&w, req).await;
        w.hub.log_a2a(&w.id, "SendStreamingMessage", target, &r.as_ref().map(|_| ()).map_err(Clone::clone)).await;
        match r? {
            SendMessageResponse::Message(m) => Ok(futures::stream::iter(vec![Ok(StreamResponse::Message(m))]).boxed()),
            SendMessageResponse::Task(t) => Ok(Self::follow(w.hub.clone(), w.caller.clone(), t.id)),
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

    async fn subscribe_to_task(&self, params: &ServiceParams, req: SubscribeToTaskRequest) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
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
        Ok(Self::follow(w.hub.clone(), w.caller.clone(), t.id))
    }

    async fn create_push_config(&self, _p: &ServiceParams, _r: TaskPushNotificationConfig) -> Result<TaskPushNotificationConfig, A2AError> {
        Err(A2AError::push_notification_not_supported())
    }

    async fn get_push_config(&self, _p: &ServiceParams, _r: GetTaskPushNotificationConfigRequest) -> Result<TaskPushNotificationConfig, A2AError> {
        Err(A2AError::push_notification_not_supported())
    }

    async fn list_push_configs(&self, _p: &ServiceParams, _r: ListTaskPushNotificationConfigsRequest) -> Result<ListTaskPushNotificationConfigsResponse, A2AError> {
        Err(A2AError::push_notification_not_supported())
    }

    async fn delete_push_config(&self, _p: &ServiceParams, _r: DeleteTaskPushNotificationConfigRequest) -> Result<(), A2AError> {
        Err(A2AError::push_notification_not_supported())
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
    let state = ServerState::new(callers_of(&cfg), move || card::pawl_card(&card_hub.card_info(port, false), false));
    srv::router_for_port(Arc::new(HubHandler { hub: weak }), state, port)
}
