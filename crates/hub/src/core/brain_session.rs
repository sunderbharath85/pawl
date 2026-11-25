//! The "acp" bot provider: a hidden, long-lived ACP session with an installed agent (Claude Code + Sonnet
//! by default) on the user's own login (their plan's usage, no API key), used only to word replies and
//! read messages. It is locked down:
//! - started with no tools at all, the supported way: `session/new` `_meta.claudeCode.options` =
//!   `{tools: [], strictMcpConfig: true, mcpServers: {}}` (Claude Agent SDK options the adapter passes
//!   through), in an agent process with a clean environment (only `pawl_acp::CLEAN_ENV_KEEP` variables,
//!   so no API keys or tokens of the hub's environment);
//! - defence in depth: cwd = an empty folder in the hub's data dir (`brain-session/`) whose
//!   `.claude/settings.json` denies every tool (`"*"`, `"mcp__*"` and each built-in by name), turns hooks and
//!   claude.ai connectors off and refuses URL MCP servers; other credentials are blanked in its env;
//! - the hub rejects every permission request it raises, and a tool call it starts anyway ends the turn
//!   and the session;
//! - it is never a hub session: not stored, not broadcast, no pet, not in status or the session list;
//!   hook reports from its folder are dropped;
//! - one turn at a time, a per-turn timeout, restarted after an error, closed when idle.
//!
//! The brain only sees [`AcpModel`] through `pawl_brain::chat::ModelSession` (text in, text out).

use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use pawl_brain::chat::{ModelFuture, ModelSession};
use pawl_protocol::{global_id, parse_global_id, Agent, EventType, PermissionOptionKind, PermissionRequest, SessionId};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use super::{Hub, Inner};

/// Folder of the brain session under the data dir.
pub(crate) const BRAIN_DIR: &str = "brain-session";
/// Local-id prefix that marks a brain session's id (`<machine>:brain-<uuid>`).
const ID_PREFIX: &str = "brain-";
/// A fresh session after this many turns, so its history stays small.
const MAX_TURNS: u32 = 20;
/// Longest reply collected from the agent; a longer one ends the turn (an error).
pub(crate) const MAX_TURN_BYTES: usize = 64 * 1024;
/// The only agent whose lockdown exists (Claude Code: its settings file and SDK options).
pub(crate) const LOCKED_AGENT: &str = "claude-code";
/// Credentials blanked in the session's env on top of the clean environment (never the Anthropic login).
const BLANKED_ENV: &[&str] = &[
    "OPENAI_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY", "PAWL_HUB_TOKEN", "GITHUB_TOKEN", "GH_TOKEN", "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN", "HF_TOKEN", "NPM_TOKEN",
];
/// Closed after this long without a turn (the next message starts it again).
pub(crate) const IDLE_CLOSE: Duration = Duration::from_secs(10 * 60);

/// Every tool denied (the settings reference: `"*"` denies every tool, `"mcp__*"` every MCP tool; the
/// built-ins are listed too in case a version reads the glob differently).
const DENIED_TOOLS: &[&str] = &[
    "*", "mcp__*", "Bash", "BashOutput", "KillShell", "KillBash", "PowerShell", "Edit", "MultiEdit", "Write", "Read", "Glob",
    "Grep", "LS", "WebFetch", "WebSearch", "Task", "TaskOutput", "TaskStop", "TaskCreate", "TaskUpdate", "TaskList", "TaskGet",
    "Agent", "NotebookEdit", "NotebookRead", "TodoWrite", "SlashCommand", "Skill", "ExitPlanMode", "EnterPlanMode",
    "AskUserQuestion", "ToolSearch", "ListMcpResourcesTool", "ReadMcpResourceTool", "Monitor", "EnterWorktree", "ExitWorktree",
    "CronCreate", "CronDelete", "CronList", "SendMessage", "LSP",
];

/// `session/new` `_meta` for claude-agent-acp: Claude Agent SDK options with no tools at all and no MCP
/// servers but the (empty) ones given (`tools: []`, `strictMcpConfig`, `mcpServers: {}`), plus every tool
/// name disallowed (merged with the adapter's own list).
pub(crate) fn no_tools_meta() -> Value {
    let disallowed: Vec<&str> = DENIED_TOOLS.iter().copied().filter(|t| *t != "*").collect();
    json!({ "claudeCode": { "options": { "tools": [], "strictMcpConfig": true, "mcpServers": {}, "disallowedTools": disallowed } } })
}

/// The project settings written into the brain session's folder.
pub(crate) fn locked_settings(model: &str) -> Value {
    json!({
        "permissions": { "deny": DENIED_TOOLS, "defaultMode": "default" },
        "disableAllHooks": true,
        "enableAllProjectMcpServers": false,
        "disableClaudeAiConnectors": true,
        "deniedMcpServers": [{ "serverUrl": "*" }],
        "model": model,
    })
}

/// True for the id of a brain session (any of them, also one already closed).
pub(crate) fn is_brain_session(id: &str) -> bool {
    parse_global_id(id).is_some_and(|(_, local)| local.starts_with(ID_PREFIX))
}

/// True when `cwd` is the brain session's folder (or inside it): its hook reports are dropped.
pub(crate) fn in_brain_dir(data_dir: &Path, cwd: &str) -> bool {
    let dir = crate::repos::path_key(&data_dir.join(BRAIN_DIR));
    !cwd.trim().is_empty() && crate::repos::path_key(Path::new(cwd.trim())).starts_with(&dir)
}

/// The running brain session.
pub(crate) struct Live {
    pub id: SessionId,
    pub turns: u32,
    pub last_used: Instant,
}

/// The turn being collected.
pub(crate) struct Turn {
    pub session_id: SessionId,
    pub text: String,
    pub done: oneshot::Sender<Result<String>>,
}

#[derive(Default)]
pub(crate) struct BrainSessionState {
    pub live: Option<Live>,
    pub turn: Option<Turn>,
}

/// What the brain holds: a weak handle on the hub (the hub owns the brain) and the agent and model.
pub(crate) struct AcpModel {
    pub hub: Weak<Inner>,
    pub agent_id: String,
    pub model: String,
}

impl ModelSession for AcpModel {
    fn label(&self) -> String {
        format!("{}:{}", self.agent_id, self.model)
    }

    fn complete<'a>(&'a self, system: &'a str, user: &'a str, timeout: Duration) -> ModelFuture<'a> {
        Box::pin(async move {
            let inner = self.hub.upgrade().ok_or_else(|| anyhow!("the hub is shutting down"))?;
            let hub = Hub { inner };
            hub.brain_turn(&self.agent_id, &self.model, system, user, timeout).await
        })
    }
}

impl Hub {
    fn brain_dir(&self) -> PathBuf {
        self.inner.paths.data_dir.join(BRAIN_DIR)
    }

    /// The "acp" provider's session handle, while its agent is installed on this machine. Only Claude Code:
    /// the lockdown (settings file, SDK options) exists for it alone.
    pub(crate) fn acp_model(&self) -> Option<std::sync::Arc<dyn ModelSession>> {
        let cfg = self.cfg();
        let agent_id = cfg.brain.effective_acp_agent();
        if agent_id != LOCKED_AGENT {
            return None;
        }
        let installed = self.st().agents.iter().any(|a| a.id == agent_id && a.installed);
        installed.then(|| {
            std::sync::Arc::new(AcpModel { hub: std::sync::Arc::downgrade(&self.inner), agent_id, model: cfg.brain.effective_acp_model() })
                as std::sync::Arc<dyn ModelSession>
        })
    }

    /// The empty, locked folder the session runs in (settings rewritten at every start).
    fn prepare_brain_dir(&self, model: &str) -> Result<PathBuf> {
        let dir = self.brain_dir();
        let claude = dir.join(".claude");
        std::fs::create_dir_all(&claude).with_context(|| format!("create {}", claude.display()))?;
        let settings = serde_json::to_string_pretty(&locked_settings(model))?;
        std::fs::write(claude.join("settings.json"), settings).context("write the brain session's settings")?;
        Ok(dir)
    }

    /// The agent to run: installed here, with the model and no extended thinking set through its
    /// documented env vars (`ANTHROPIC_MODEL` outranks settings; `MAX_THINKING_TOKENS=0` turns thinking off),
    /// claude.ai connectors off (`ENABLE_CLAUDEAI_MCP_SERVERS=false`) and other credentials blanked.
    fn brain_agent(&self, agent_id: &str, model: &str) -> Result<Agent> {
        if agent_id != LOCKED_AGENT {
            bail!("only {LOCKED_AGENT} can run as the pet's model");
        }
        let mut agent = self
            .st()
            .agents
            .iter()
            .find(|a| a.id == agent_id && a.installed)
            .cloned()
            .ok_or_else(|| anyhow!("{agent_id} is not installed on this machine"))?;
        let mut env = agent.env.clone().unwrap_or_default();
        for key in BLANKED_ENV {
            env.insert((*key).into(), String::new());
        }
        env.insert("ANTHROPIC_MODEL".into(), model.to_string());
        env.insert("MAX_THINKING_TOKENS".into(), "0".into());
        env.insert("ENABLE_CLAUDEAI_MCP_SERVERS".into(), "false".into());
        agent.env = Some(env);
        Ok(agent)
    }

    /// One turn of the brain session: started (or restarted) as needed, one at a time, within `timeout`.
    pub(crate) async fn brain_turn(&self, agent_id: &str, model: &str, system: &str, user: &str, timeout: Duration) -> Result<String> {
        let _one = self.inner.brain_turn.lock().await;
        match tokio::time::timeout(timeout, self.brain_turn_inner(agent_id, model, system, user)).await {
            Ok(Ok(text)) => Ok(text),
            Ok(Err(e)) => {
                self.reset_brain_session("error").await;
                Err(e)
            }
            Err(_) => {
                self.reset_brain_session("timeout").await;
                bail!("the {agent_id} session did not answer within {} s", timeout.as_secs())
            }
        }
    }

    async fn brain_turn_inner(&self, agent_id: &str, model: &str, system: &str, user: &str) -> Result<String> {
        let stale = {
            let st = self.inner.brain_session.lock().unwrap_or_else(|p| p.into_inner());
            st.live.as_ref().is_some_and(|l| l.turns >= MAX_TURNS)
        };
        if stale {
            self.reset_brain_session("turn limit").await;
        }
        let live_id = self.inner.brain_session.lock().unwrap_or_else(|p| p.into_inner()).live.as_ref().map(|l| l.id.clone());
        let id = match live_id {
            Some(id) => id,
            None => {
                let agent = self.brain_agent(agent_id, model)?;
                let cwd = self.prepare_brain_dir(model)?;
                let id = global_id(&self.inner.machine_id, format!("{ID_PREFIX}{}", uuid::Uuid::new_v4()));
                // Straight to the host: never merged into the hub's sessions. No tools, clean environment.
                let opts = pawl_acp::StartOptions { meta: Some(no_tools_meta()), clean_env: true };
                self.inner.host.start_session_with(&agent, cwd, id.clone(), None, opts).await.context("start the brain session")?;
                tracing::info!(agent = %agent_id, %model, "brain: agent session started");
                self.inner.brain_session.lock().unwrap_or_else(|p| p.into_inner()).live =
                    Some(Live { id: id.clone(), turns: 0, last_used: Instant::now() });
                id
            }
        };
        let (tx, rx) = oneshot::channel();
        self.inner.brain_session.lock().unwrap_or_else(|p| p.into_inner()).turn = Some(Turn { session_id: id.clone(), text: String::new(), done: tx });
        let text = format!("{system}\n\n---\n\n{user}");
        self.inner.host.prompt(&id, text).await.context("prompt the brain session")?;
        let out = rx.await.map_err(|_| anyhow!("the brain session went away"))??;
        if let Some(l) = self.inner.brain_session.lock().unwrap_or_else(|p| p.into_inner()).live.as_mut() {
            l.turns += 1;
            l.last_used = Instant::now();
        }
        Ok(out)
    }

    /// Close the brain session (error, timeout, idle, settings change); the next turn starts a new one.
    pub(crate) async fn reset_brain_session(&self, why: &str) {
        let (live, turn) = {
            let mut st = self.inner.brain_session.lock().unwrap_or_else(|p| p.into_inner());
            (st.live.take(), st.turn.take())
        };
        if let Some(t) = turn {
            let _ = t.done.send(Err(anyhow!("the brain session was reset ({why})")));
        }
        if let Some(l) = live {
            tracing::info!(%why, "brain: agent session closed");
            if let Err(e) = self.inner.host.close_session(&l.id).await {
                tracing::debug!("close the brain session: {e:#}");
            }
        }
    }

    /// Host events of a brain session: collected into the turn, never stored, broadcast or shown.
    pub(crate) async fn on_brain_event(&self, ev: pawl_acp::HostEvent) {
        use pawl_acp::HostEvent as H;
        match ev {
            H::Session(s) => {
                if s.ended_at.is_some() {
                    self.finish_turn(&s.id, Err(anyhow!("the brain session ended")));
                    let mut st = self.inner.brain_session.lock().unwrap_or_else(|p| p.into_inner());
                    if st.live.as_ref().is_some_and(|l| l.id == s.id) {
                        st.live = None;
                    }
                }
            }
            H::Event { session_id, kind, payload } => match kind {
                EventType::AgentMessageChunk => {
                    let chunk = payload.get("text").and_then(Value::as_str).unwrap_or_default();
                    let too_long = {
                        let mut st = self.inner.brain_session.lock().unwrap_or_else(|p| p.into_inner());
                        match st.turn.as_mut().filter(|t| t.session_id == session_id) {
                            Some(t) if t.text.len() + chunk.len() > MAX_TURN_BYTES => true,
                            Some(t) => {
                                t.text.push_str(chunk);
                                false
                            }
                            None => false,
                        }
                    };
                    if too_long {
                        self.finish_turn(&session_id, Err(anyhow!("the agent's reply is longer than {} KiB", MAX_TURN_BYTES / 1024)));
                        let _ = self.inner.host.cancel(&session_id).await;
                    }
                }
                EventType::Stop => {
                    let text = {
                        let st = self.inner.brain_session.lock().unwrap_or_else(|p| p.into_inner());
                        st.turn.as_ref().filter(|t| t.session_id == session_id).map(|t| t.text.trim().to_string())
                    };
                    match text {
                        Some(t) if !t.is_empty() => self.finish_turn(&session_id, Ok(t)),
                        Some(_) => self.finish_turn(&session_id, Err(anyhow!("the agent ended its turn without text"))),
                        None => {}
                    }
                }
                EventType::Error => {
                    let msg = payload.get("message").and_then(Value::as_str).unwrap_or("agent error").to_string();
                    self.finish_turn(&session_id, Err(anyhow!("the agent reported an error: {}", crate::notify::truncate(&msg, 200))));
                }
                EventType::ToolCall => {
                    // Every tool is denied in its settings; one that starts anyway ends the turn and the session.
                    tracing::warn!("brain: the agent session started a tool call; ending it");
                    self.finish_turn(&session_id, Err(anyhow!("the agent tried to use a tool")));
                    let _ = self.inner.host.cancel(&session_id).await;
                }
                _ => {}
            },
            H::PermissionRequest(req) => {
                // It tried to use a tool: rejected, and the turn's answer is not used.
                let sid = req.session_id.clone();
                self.reject_brain_permission(req).await;
                self.finish_turn(&sid, Err(anyhow!("the agent asked to use a tool")));
            }
            H::AgentCapabilities { .. } => {}
        }
    }

    /// The brain session never gets permission for anything: reject (or cancel when there is no reject option).
    async fn reject_brain_permission(&self, req: PermissionRequest) {
        let reject = req
            .options
            .iter()
            .find(|o| o.kind == PermissionOptionKind::RejectOnce)
            .or_else(|| req.options.iter().find(|o| o.kind == PermissionOptionKind::RejectAlways))
            .map(|o| o.option_id.clone());
        tracing::warn!(request = %req.request_id, "brain: rejected a permission request of the agent session");
        if let Err(e) = self.inner.host.resolve_permission(&req.request_id, reject).await {
            tracing::warn!("reject the brain session's permission request: {e:#}");
        }
    }

    fn finish_turn(&self, session_id: &str, result: Result<String>) {
        let turn = {
            let mut st = self.inner.brain_session.lock().unwrap_or_else(|p| p.into_inner());
            match &st.turn {
                Some(t) if t.session_id == session_id => st.turn.take(),
                _ => None,
            }
        };
        if let Some(t) = turn {
            let _ = t.done.send(result);
        }
    }

    /// Closes the brain session after [`IDLE_CLOSE`] without a turn.
    pub(crate) async fn brain_session_loop(self) {
        let mut shutdown = self.shutdown_rx();
        let mut iv = tokio::time::interval(Duration::from_secs(60));
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = iv.tick() => {
                    let idle = {
                        let st = self.inner.brain_session.lock().unwrap_or_else(|p| p.into_inner());
                        st.turn.is_none() && st.live.as_ref().is_some_and(|l| l.last_used.elapsed() > IDLE_CLOSE)
                    };
                    if idle {
                        self.reset_brain_session("idle").await;
                    }
                }
                _ = shutdown.changed() => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_settings_deny_every_tool_and_hooks() {
        let v = locked_settings("sonnet");
        let deny: Vec<&str> = v["permissions"]["deny"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        for t in ["*", "mcp__*", "Bash", "Edit", "Write", "Read", "Glob", "Grep", "WebFetch", "WebSearch", "Task", "NotebookEdit"] {
            assert!(deny.contains(&t), "{t}");
        }
        assert_eq!((v["disableAllHooks"].as_bool(), v["model"].as_str()), (Some(true), Some("sonnet")));
        assert_eq!(v["disableClaudeAiConnectors"], true);
        assert!(v["permissions"].get("allow").is_none());
        for t in ["PowerShell", "KillBash", "TaskOutput", "TaskStop", "TaskCreate", "ExitPlanMode", "EnterPlanMode", "AskUserQuestion", "ToolSearch", "ListMcpResourcesTool", "ReadMcpResourceTool", "Monitor"] {
            assert!(deny.contains(&t), "{t}");
        }
        let meta = no_tools_meta();
        let o = &meta["claudeCode"]["options"];
        assert_eq!((o["tools"].clone(), o["strictMcpConfig"].clone(), o["mcpServers"].clone()), (json!([]), json!(true), json!({})));
        assert!(o["disallowedTools"].as_array().unwrap().iter().any(|t| t == "Bash"));
    }

    #[test]
    fn brain_ids_and_folders_are_recognized() {
        assert!(is_brain_session(&global_id("abcdef012345", "brain-1234")));
        assert!(!is_brain_session(&global_id("abcdef012345", "1234")) && !is_brain_session("brain-x"));
        let data = std::env::temp_dir().join("pawl-test-data");
        let inside = data.join(BRAIN_DIR).join("sub");
        assert!(in_brain_dir(&data, &data.join(BRAIN_DIR).to_string_lossy()));
        assert!(in_brain_dir(&data, &inside.to_string_lossy()));
        assert!(!in_brain_dir(&data, &data.join("brain-session-other").to_string_lossy()));
        assert!(!in_brain_dir(&data, ""));
    }
}
