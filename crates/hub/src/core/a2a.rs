//! The orchestrator's A2A facade (PLAN.md "A2A orchestration", phase A1).
//!
//! Every delegation of the orchestrator goes through here as A2A: starting a session is a
//! `SendMessage` without a `contextId` carrying [`DelegateParams`]; prompting one is a
//! `SendMessage` in the session's context (a new Task per turn); stopping a turn is `CancelTask`;
//! status is `GetTask` / `ListTasks` (and the latest task per session in the bot's context);
//! answering a tool-call confirmation is a continuation message carrying Structured Input.
//!
//! This is an in-process A2A agent (no HTTP). Its sub-agents today are this machine's ACP agents
//! (the ACP host underneath executes the turns); the task view is [`pawl_a2a::TaskRegistry`], fed
//! with every event of the hub's owned sessions. Behaviour, approvals, the audit log and the owner
//! rules are the ones of the hub paths it replaced: the same checks run in the same order.

use anyhow::Result;
use pawl_a2a::spec::{ListTasksRequest, ListTasksResponse, Message, PartContent, Role, SendMessageRequest, SendMessageResponse, Task};
use pawl_a2a::structured_input::{self, AnswerError};
use pawl_a2a::{Caller, DelegateParams, TaskRegistry};
use pawl_protocol::{PermissionOptionKind, ResolvedBy, Session, ToolKind};

use super::{intents, AckCode, Actor, Hub};

/// A2A-specific failures (the localhost server maps them to the A2A error codes; hub acks show
/// the text).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum A2aError {
    #[error("task_not_found")]
    TaskNotFound,
    #[error("task_not_cancelable")]
    TaskNotCancelable,
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("invalid_params: {0}")]
    InvalidParams(String),
}

impl A2aError {
    /// The code in `e`'s chain, if any.
    pub fn find(e: &anyhow::Error) -> Option<A2aError> {
        e.chain().find_map(|c| c.downcast_ref::<A2aError>().cloned())
    }
}

/// The result of one `SendMessage`: the A2A answer, plus what the hub's own API reports (the
/// session a start created, a start's warning).
#[derive(Debug, Clone)]
pub struct Delegated {
    pub response: SendMessageResponse,
    pub session: Option<Session>,
    pub warning: Option<String>,
}

/// The text of `m`'s text parts, joined by blank lines.
pub fn message_text(m: &Message) -> String {
    m.parts
        .iter()
        .filter_map(|p| match &p.content {
            PartContent::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn user_message(text: Option<&str>) -> Message {
    let parts = text.map(|t| vec![pawl_a2a::spec::Part::text(t)]).unwrap_or_default();
    Message::new(Role::User, parts)
}

/// `SendMessage` that starts `agent_id` in `cwd` (with `prompt` as its first turn, if any).
pub fn start_request(agent_id: &str, cwd: &str, prompt: Option<&str>) -> SendMessageRequest {
    let mut message = user_message(prompt.filter(|p| !p.trim().is_empty()));
    DelegateParams { agent_id: Some(agent_id.to_string()), cwd: Some(cwd.to_string()), ..Default::default() }.attach(&mut message);
    SendMessageRequest { message, configuration: None, metadata: None, tenant: None }
}

/// `SendMessage` that prompts the session `session_id` (a new task in its context).
pub fn prompt_request(session_id: &str, text: &str) -> SendMessageRequest {
    let mut message = user_message(Some(text));
    message.context_id = Some(session_id.to_string());
    SendMessageRequest { message, configuration: None, metadata: None, tenant: None }
}

/// What a caller may do with tool-call confirmations (Structured Input continuations).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answers {
    /// Not at all (outside callers by default; paired hubs, which use approve/deny intents).
    Never,
    /// Any option (this machine's own clients).
    Any,
    /// Reject anything; allow once only `read`, `search`, `think` and `edit` requests ([`limited_may_allow`],
    /// an allowlist: execute / delete / fetch, move, switch_mode, other and unknown kinds need a person
    /// on a Pawl client); never "always" (an outside caller the user allowed).
    Limited,
}

/// May an outside caller with approval rights allow (once) a request of this tool kind?
pub fn limited_may_allow(kind: Option<ToolKind>) -> bool {
    matches!(kind, Some(ToolKind::Read | ToolKind::Search | ToolKind::Think | ToolKind::Edit))
}

impl Hub {
    pub(crate) fn tasks(&self) -> &TaskRegistry {
        &self.inner.tasks
    }

    /// One A2A `SendMessage` from `caller`: a continuation of a waiting task (an answer), a new
    /// turn in an existing context (a prompt), or a delegation that starts a session.
    pub(crate) async fn a2a_send(&self, req: SendMessageRequest, caller: &Caller, actor: &Actor, answers: Answers) -> Result<Delegated> {
        let msg = req.message;
        if let Some(task_id) = msg.task_id.clone() {
            let task = self.a2a_continue(&task_id, &msg, caller, actor, answers).await?;
            let session = self.session(&task.context_id);
            return Ok(Delegated { response: SendMessageResponse::Task(task), session, warning: None });
        }
        if let Some(ctx) = msg.context_id.clone() {
            let task = self.a2a_prompt(&ctx, message_text(&msg), caller).await?;
            return Ok(Delegated { response: SendMessageResponse::Task(task), session: self.session(&ctx), warning: None });
        }
        let params = DelegateParams::of(&msg).ok_or_else(|| {
            A2aError::InvalidParams("a message without a contextId starts a session: give agentId and cwd in its delegate metadata".into())
        })?;
        if let Some(m) = params.machine_id.as_deref().filter(|m| *m != self.inner.machine_id) {
            return Err(A2aError::Unsupported(format!("this hub starts sessions on its own machine only, not {m}")).into());
        }
        // Empty values go on to the start's own checks (unknown agent, bad folder).
        let agent_id = params.agent_id.ok_or_else(|| A2aError::InvalidParams("agentId is required".into()))?;
        let cwd = params.cwd.ok_or_else(|| A2aError::InvalidParams("cwd is required".into()))?;
        let text = message_text(&msg);
        self.a2a_start(&agent_id, &cwd, Some(text).filter(|t| !t.trim().is_empty()), caller).await
    }

    /// Start a session of `agent_id` in `cwd` for `caller` (its context is bound to the caller),
    /// then its first turn. A first prompt that cannot be sent leaves the session running and is
    /// reported as a warning instead of failing the start.
    pub(crate) async fn a2a_start(&self, agent_id: &str, cwd: &str, prompt: Option<String>, caller: &Caller) -> Result<Delegated> {
        let session = self.launch_session(agent_id, cwd).await?;
        self.tasks().bind_context(&session.id, caller.clone());
        // Driven from elsewhere before its first prompt can raise a request: rules never
        // auto-approve what another machine or an outside caller asked for.
        if *caller != Caller::Local {
            self.mark_remote_driven(&session.id).await;
        }
        let started = |note: Option<&str>| {
            let mut text = format!("Started {} in {}.", self.st().agent_name(&session.agent_id), session.cwd);
            if let Some(n) = note {
                text.push(' ');
                text.push_str(n);
            }
            let mut m = Message::new(Role::Agent, vec![pawl_a2a::spec::Part::text(text)]);
            m.context_id = Some(session.id.clone());
            SendMessageResponse::Message(m)
        };
        let (response, warning) = match prompt.filter(|p| !p.trim().is_empty()) {
            None => (started(None), None),
            Some(p) => match self.a2a_prompt(&session.id, p, caller).await {
                Ok(task) => (SendMessageResponse::Task(task), None),
                Err(e) => {
                    tracing::warn!("session {} started, but its first prompt was not sent: {e:#}", session.id);
                    let w = format!("the session started, but its first prompt was not sent: {e:#}");
                    (started(Some(&w)), Some(w))
                }
            },
        };
        let session = self.session(&session.id).unwrap_or(session);
        Ok(Delegated { response, session: Some(session), warning })
    }

    /// A new turn (task) in the context `session_id` for `caller`. The checks are the hub's prompt
    /// checks; the agent's refusal leaves no task behind.
    pub(crate) async fn a2a_prompt(&self, session_id: &str, text: String, caller: &Caller) -> Result<Task> {
        self.a2a_prompt_with(session_id, text, Vec::new(), caller).await
    }

    /// [`Self::a2a_prompt`] with pictures (this machine's own clients only; see `Hub::prompt_with_images`).
    pub(crate) async fn a2a_prompt_with(&self, session_id: &str, text: String, images: Vec<pawl_protocol::ChatImage>, caller: &Caller) -> Result<Task> {
        if !self.tasks().can_access(caller, session_id) {
            return Err(AckCode::NotFound.into());
        }
        self.owned_open_session(session_id)?;
        intents::validate_prompt(&text)?;
        // A browser task drives this machine's Chrome: only this machine's own clients prompt it.
        if *caller != Caller::Local && self.is_browser_session(session_id) {
            return Err(anyhow::Error::new(AckCode::Forbidden).context("a browser task takes prompts from this machine only"));
        }
        if *caller != Caller::Local {
            self.mark_remote_driven(session_id).await;
        }
        let task = self.tasks().begin(session_id, &text, caller);
        // The ACP host emits the `user_message` event for every prompt it sends; recording it here
        // too would log (and print) the prompt twice.
        match self.exec_prompt_with(session_id, text, images).await {
            Ok(()) => {
                if self.is_browser_session(session_id) {
                    self.browser_turn_started(session_id);
                }
                self.tasks().commit(&task.id);
                Ok(self.tasks().get(&task.id, None).unwrap_or(task))
            }
            Err(e) => {
                self.tasks().abort(&task.id);
                Err(e)
            }
        }
    }

    /// A2A `CancelTask` for `caller`: `session/cancel` to the agent, the task ends CANCELED.
    pub(crate) async fn a2a_cancel(&self, task_id: &str, caller: &Caller, actor: &Actor) -> Result<Task> {
        let ctx = self
            .tasks()
            .context_of(task_id)
            .filter(|c| self.tasks().can_access(caller, c))
            .ok_or(A2aError::TaskNotFound)?;
        let task = self.tasks().get(task_id, None).ok_or(A2aError::TaskNotFound)?;
        if task.status.state.is_terminal() {
            return Err(A2aError::TaskNotCancelable.into());
        }
        self.acp_cancel(&ctx, actor).await?;
        Ok(self.tasks().cancel(task_id).unwrap_or(task))
    }

    /// Stop the running turn of `session_id` (the orchestrator's cancel): its task through
    /// `CancelTask`, or the session itself when no turn is known.
    pub(crate) async fn cancel_as(&self, session_id: &str, actor: &Actor, caller: &Caller) -> Result<()> {
        if !self.tasks().can_access(caller, session_id) {
            return Err(AckCode::NotFound.into());
        }
        match self.tasks().active_task(session_id) {
            Some(t) => self.a2a_cancel(&t.id, caller, actor).await.map(|_| ()),
            None => self.acp_cancel(session_id, actor).await,
        }
    }

    /// A2A `GetTask` for `caller`.
    pub(crate) fn a2a_get(&self, task_id: &str, history_length: Option<i32>, caller: &Caller) -> Result<Task> {
        let t = self.tasks().get(task_id, history_length).ok_or(A2aError::TaskNotFound)?;
        if !self.tasks().can_access(caller, &t.context_id) {
            return Err(A2aError::TaskNotFound.into());
        }
        Ok(t)
    }

    /// A2A `ListTasks` for `caller`.
    pub(crate) fn a2a_list(&self, req: &ListTasksRequest, caller: &Caller) -> Result<ListTasksResponse> {
        self.tasks().list(caller, req).map_err(|e| A2aError::InvalidParams(e).into())
    }

    /// A continuation of `task_id`: the answer to its pending tool-call confirmation. An answer that
    /// does not fit re-asks (spec 7) and is not an error.
    async fn a2a_continue(&self, task_id: &str, msg: &Message, caller: &Caller, actor: &Actor, answers: Answers) -> Result<Task> {
        let task = self.a2a_get(task_id, None, caller)?;
        if msg.context_id.as_deref().is_some_and(|c| c != task.context_id) {
            return Err(A2aError::InvalidParams("the contextId does not match the task's context".into()).into());
        }
        if task.status.state.is_terminal() {
            return Err(A2aError::Unsupported(format!("task {task_id} has finished; send a new message in its context")).into());
        }
        let Some(req) = self.tasks().pending_request(task_id) else {
            return Err(A2aError::Unsupported(format!("task {task_id} is working and takes no input until it asks")).into());
        };
        if answers == Answers::Never {
            return Err(anyhow::Error::new(AckCode::Forbidden).context("this caller may not answer permission requests"));
        }
        let ids: Vec<String> = req.options.iter().map(|o| o.option_id.clone()).collect();
        let option = match structured_input::parse_answer(msg, &req.request_id, &ids) {
            Ok(o) => o,
            Err(AnswerError::Invalid(errors)) => {
                self.tasks().reject_input(task_id, Some(errors));
                return self.a2a_get(task_id, None, caller);
            }
            Err(AnswerError::Missing | AnswerError::WrongRequest | AnswerError::Ambiguous) => {
                self.tasks().reject_input(task_id, None);
                return self.a2a_get(task_id, None, caller);
            }
        };
        if answers == Answers::Limited {
            let kind = req.options.iter().find(|o| o.option_id == option).map(|o| o.kind);
            match kind {
                Some(PermissionOptionKind::RejectOnce | PermissionOptionKind::RejectAlways) => {}
                Some(PermissionOptionKind::AllowAlways) => {
                    return Err(anyhow::Error::new(AckCode::Forbidden).context("\"always allow\" is chosen on a Pawl client only"));
                }
                // An allowlist, not a denylist: only these tool kinds may be allowed once by a program.
                Some(PermissionOptionKind::AllowOnce) if limited_may_allow(req.tool_call.kind) => {}
                _ => {
                    return Err(anyhow::Error::new(AckCode::Forbidden)
                        .context("only read, search, think and edit requests may be allowed by an A2A caller; a person approves the rest on a Pawl client"));
                }
            }
        }
        self.resolve_permission(&req.request_id, Some(option), ResolvedBy::User, actor).await?;
        self.a2a_get(task_id, None, caller)
    }
}
