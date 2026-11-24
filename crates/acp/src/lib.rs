//! pawl-acp: the Pawl hub's ACP (Agent Client Protocol) client host.
//!
//! [`AcpHost`] spawns ACP agents as stdio subprocesses, drives them with the
//! official [`agent_client_protocol`] SDK (protocol v1) and turns everything
//! they report into [`HostEvent`]s carrying `pawl-protocol` wire types.
//!
//! # Process granularity
//!
//! One agent process (one ACP connection) per **(agent, cwd)** pair, shared by
//! every session started for that agent in that directory. The process is
//! spawned with the session cwd as its working directory, so agents that read
//! project config from their process cwd behave correctly, and a crash only
//! affects sessions in that one project. A changed `command`/`env` for the
//! same agent id gets a fresh process. A process is stopped when its last session
//! is closed ([`AcpHost::close_session`]), after 5 minutes without any session
//! (e.g. one spawned only for `list_history`), on [`AcpHost::shutdown`], or when
//! the last `AcpHost` clone is dropped.
//!
//! Stopping closes the agent's stdin, waits a short grace period, then kills the
//! agent's **whole process tree** (a Windows Job Object with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`; a dedicated process group on Unix), so
//! `npx`/`.cmd` launchers never leave the real agent orphaned.
//!
//! # Status mapping
//!
//! prompt sent / thought or message chunk → `thinking`; tool call in progress →
//! `working` (back to `thinking` when all running tools finish); permission
//! request → `waiting_permission`; stop `end_turn`/`max_tokens`/`max_turn_requests`
//! → `done`; `cancelled` → `idle`; `refusal` or a failed prompt → `error`;
//! process exit → `error` + `ended_at`.
//!
//! # Client capabilities
//!
//! `fs/read_text_file` and `fs/write_text_file` are served, restricted to the
//! session cwd (symlinks resolved). Terminals are not implemented and are
//! advertised as unsupported.

mod convert;
mod fs;
mod host;
mod spawn;

use std::path::PathBuf;
use std::sync::Arc;

use pawl_protocol::{Agent, AgentId, HistorySession, MachineId, Session, SessionId};

/// Events emitted by the host to the hub.
#[derive(Debug, Clone)]
pub enum HostEvent {
    /// Upsert (status changes, title, ended).
    Session(pawl_protocol::Session),
    /// Normalized ACP update for the event log. payload = protocol payload shapes
    /// (TextPayload, ToolCallPayload, PlanPayload, StopPayload, ErrorPayload,
    /// PermissionResolvedPayload).
    ///
    /// `PermissionResolved` (payload `PermissionResolvedPayload`, outcome `cancelled`)
    /// is only emitted when the *host* cancels a pending request: `by: user` for
    /// [`AcpHost::cancel`], `by: system` for automatic cancels (turn end, agent
    /// withdrew it, [`AcpHost::close_session`], process exit, shutdown). Requests the
    /// hub resolves through [`AcpHost::resolve_permission`] are not echoed.
    Event { session_id: SessionId, kind: pawl_protocol::EventType, payload: serde_json::Value },
    /// Agent asked `session/request_permission`. Host waits for resolve_permission().
    PermissionRequest(pawl_protocol::PermissionRequest),
    AgentCapabilities { agent_id: AgentId, capabilities: pawl_protocol::AgentCapabilities },
}

/// ACP client host. Cheap to clone; all clones share the same processes.
#[derive(Clone)]
pub struct AcpHost {
    inner: Arc<host::Inner>,
}

impl std::fmt::Debug for AcpHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcpHost").finish_non_exhaustive()
    }
}

impl AcpHost {
    /// `machine_id`: this hub's machine. Every Session / HistorySession / PermissionRequest the
    /// host builds carries it, and permission request ids are global
    /// (`global_id(machine_id, uuid)`). Session ids passed in (`hub_session_id`) are already
    /// global and used verbatim. Agents of another machine are refused.
    pub fn new(machine_id: MachineId, events: tokio::sync::mpsc::UnboundedSender<HostEvent>) -> Self {
        Self { inner: Arc::new(host::Inner::new(machine_id, events)) }
    }

    /// The machine this host runs agents for.
    pub fn machine_id(&self) -> &str {
        self.inner.machine_id()
    }

    /// Spawn (or reuse) the agent process, `initialize`, `session/new`. If `prompt` is Some, send it (non-blocking).
    pub async fn start_session(&self, agent: &Agent, cwd: PathBuf, hub_session_id: SessionId, prompt: Option<String>) -> anyhow::Result<Session> {
        self.inner.start_session(agent, cwd, hub_session_id, prompt).await
    }

    /// Starts a turn; returns once sent. Turn end arrives as Event{kind: Stop} + Session upsert.
    pub async fn prompt(&self, hub_session_id: &str, text: String) -> anyhow::Result<()> {
        self.inner.prompt(hub_session_id, text).await
    }

    /// `session/cancel` for the running turn; pending permission requests are answered `cancelled`.
    pub async fn cancel(&self, hub_session_id: &str) -> anyhow::Result<()> {
        self.inner.cancel(hub_session_id).await
    }

    /// None = cancelled outcome.
    pub async fn resolve_permission(&self, request_id: &str, option_id: Option<String>) -> anyhow::Result<()> {
        self.inner.resolve_permission(request_id, option_id).await
    }

    /// `session/list` if capability present, else Ok(vec![]).
    pub async fn list_history(&self, agent: &Agent, cwd: Option<PathBuf>) -> anyhow::Result<Vec<HistorySession>> {
        self.inner.list_history(agent, cwd).await
    }

    /// `session/resume` (preferred) or `session/load` fallback per capabilities.
    pub async fn resume_session(&self, agent: &Agent, acp_session_id: String, cwd: PathBuf, hub_session_id: SessionId) -> anyhow::Result<Session> {
        self.inner.resume_session(agent, acp_session_id, cwd, hub_session_id).await
    }

    /// End a session for good: cancels its pending permission requests (`by: system`),
    /// sends `session/close` when the agent supports it (else `session/cancel` for a
    /// running turn), upserts the session `done` with `endedAt`, and stops the agent
    /// process once it serves no other session. Unknown or already-ended ids are a no-op.
    pub async fn close_session(&self, hub_session_id: &str) -> anyhow::Result<()> {
        self.inner.close_session(hub_session_id).await
    }

    /// Close every agent connection (stdin closed, then killed after a grace
    /// period) and mark open sessions ended. New calls fail afterwards.
    pub async fn shutdown(&self) {
        self.inner.shutdown().await
    }
}

#[cfg(test)]
mod tests;

/// Compile-time check that the public API is `Send + Sync + Clone` with `Send` futures.
#[allow(dead_code)]
fn assert_public_api_is_send_sync() {
    fn check<T: Send + Sync + Clone + 'static>() {}
    check::<AcpHost>();
    fn fut<F: std::future::Future + Send>(_: F) {}
    let host = AcpHost::new("abcdef012345".into(), tokio::sync::mpsc::unbounded_channel().0);
    let agent: Agent = serde_json::from_value(serde_json::json!({
        "id": "a", "machineId": "abcdef012345", "name": "a", "command": ["a"], "installed": true, "source": "custom"
    }))
    .unwrap();
    fut(host.start_session(&agent, PathBuf::new(), String::new(), None));
    fut(host.prompt("", String::new()));
    fut(host.cancel(""));
    fut(host.resolve_permission("", None));
    fut(host.list_history(&agent, None));
    fut(host.resume_session(&agent, String::new(), PathBuf::new(), String::new()));
    fut(host.close_session(""));
    fut(host.shutdown());
}
