//! Assistant mode (docs/CONTRACTS.md "v0.2 Assistant mode: the pet does the work"): the default pet's chat
//! goes to an agent session with tools that works in the assistant's own folder.
//!
//! - The session is an ordinary owned session (stored, broadcast, listed, approvals on cards) with
//!   `purpose: "assistant"`, agent = the resolved agent ([`resolve_agent`]), cwd = the workspace (resolved). At most
//!   one is live. It ends after `owned_session_idle_hours` like any owned session, on `assistant_reset` ("new chat",
//!   "start over"), and when the assistant is turned off or its agent, folder or tools change; the next message
//!   starts a new one. The chat history stays. It never gets a pet of its own, and is remote-driven: a user's
//!   auto-allow rule asks for it instead (it reads web pages and files).
//! - Every end of the session (any way) makes the assistant's A2A token due for replacement; a new start first
//!   waits for the replacement ([`Hub::rotate_assistant_token`]), and does not start with Pawl's tools while it
//!   keeps failing. A settings change while a start is under way ends the new session at once (a generation count).
//! - Its first prompt is the persona and rules (`pawl_brain::chat::assistant::persona`), then the user's message;
//!   later messages are plain prompts.
//! - Routing ([`pawl_brain::chat::assistant::route`]): answers to approvals, a pending request's voice code and
//!   commands for another machine's agents stay with the brain; a bare "yes" / "no" never answers a risky card or
//!   one of several; "stop" / "cancel that" cancel the running turn; everything else is a prompt. The assistant
//!   never sees an approval it could talk into, and never answers one: every tool call it makes is an ordinary
//!   approval card under the user's rules.
//! - The pet's reply is one `ChatMessage` (`sessionId` = the assistant session) created when the turn starts
//!   (`streaming: true`), updated by `chat_update` as agent text arrives (at most every [`UPDATE_EVERY`], at most
//!   [`MAX_REPLY_BYTES`]) and finished with `streaming: false`. A spoken message's final update carries `speech`,
//!   the brain's short form. A prompt that reaches the session another way (a paired phone, the session view)
//!   gets a reply in the chat too.
//! - Only this machine's clients (chat) and its paired phones (prompts on the session) reach it: another hub,
//!   an outside A2A caller and the assistant's own A2A caller are refused ([`Hub::check_assistant_caller`]).
//! - If it cannot start (no agent, folder error, token not replaced) the brain answers as today, with one line
//!   saying why and where to fix it.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use pawl_a2a::Caller;
use pawl_brain::chat::assistant::{self as persona, AssistantRoute, PersonaInput};
use pawl_brain::chat::BrainReply;
use pawl_protocol::{
    Agent, AgentSource, AssistantState, ChatImage, ChatMessage, ChatRole, DeviceKind, EventType, HubSettingsAssistant, ServerMsg,
    Session, SessionId, PURPOSE_ASSISTANT,
};
use serde_json::{json, Value};

use super::{AckCode, Actor, Hub};
use crate::config::{AssistantSection, ASSISTANT_AUTO};
use crate::settings as cfgset;
use crate::store::SettingsAuditKind;

/// `Session::purpose` of the assistant session.
pub(crate) const PURPOSE: &str = PURPOSE_ASSISTANT;
/// `chat_update`s of one reply are at least this far apart (about 5 a second).
pub(crate) const UPDATE_EVERY: Duration = Duration::from_millis(200);
/// One reply keeps at most this much of the agent's text (the cap of a mirrored event payload); the rest is cut.
pub(crate) const MAX_REPLY_BYTES: usize = pawl_protocol::MIRROR_PAYLOAD_MAX_BYTES;
/// "auto": these agents in this order (hub id and registry id), else the first installed ACP agent.
const AUTO_ORDER: &[&[&str]] = &[&["claude-code", "claude-acp"], &["codex", "codex-acp"], &["copilot", "github-copilot-cli"]];
/// The settings audit key for the assistant session's start and reset.
const AUDIT_KEY: &str = "assistant.session";
/// The reply to a message that comes while a turn runs.
const BUSY: &str = "I'm still on your last message; say stop to cancel it.";
/// The reply's last line when the agent wrote more than [`MAX_REPLY_BYTES`].
const CUT: &str = "(Reply cut short.)";

/// The running assistant (see the module docs).
#[derive(Default)]
pub(crate) struct AssistantRt {
    /// The live session (kept until it is really closed).
    session_id: Option<SessionId>,
    /// The id of a session being started, before it is registered: its snapshots are marked "assistant".
    starting: Option<SessionId>,
    /// The live session has not had its first prompt (the persona) yet.
    fresh: bool,
    /// Why the last start failed (cleared by a start that works and by a settings change).
    error: Option<String>,
    /// The reply being written.
    turn: Option<Turn>,
    /// Bumped by every assistant setting change: a start that began before one ends its session at once.
    generation: u64,
    /// The assistant's A2A token must be replaced before the next start (every session end sets it).
    rotate_due: bool,
    /// The last replacement failed twice: no start with Pawl's tools until one works.
    rotate_error: Option<String>,
    /// The live session could not be closed: closed again before the next start.
    must_end: bool,
}

/// One turn's reply.
struct Turn {
    session_id: SessionId,
    message: ChatMessage,
    /// The agent's text so far (at most [`MAX_REPLY_BYTES`]).
    raw: String,
    /// The agent wrote more than [`MAX_REPLY_BYTES`]: the rest was dropped.
    cut: bool,
    last_sent: Option<Instant>,
    flush_scheduled: bool,
    spoken: bool,
}

impl Turn {
    fn new(session_id: SessionId, message: ChatMessage, spoken: bool) -> Self {
        Self { session_id, message, raw: String::new(), cut: false, last_sent: None, flush_scheduled: false, spoken }
    }

    /// The reply as sent: scrubbed here, only what goes out.
    fn snapshot(&self) -> ChatMessage {
        ChatMessage { text: persona::reply_text(&self.raw), ..self.message.clone() }
    }

    /// Add agent text, up to [`MAX_REPLY_BYTES`]. False once the reply is full.
    fn push(&mut self, text: &str) -> bool {
        if self.cut {
            return false;
        }
        let room = MAX_REPLY_BYTES.saturating_sub(self.raw.len());
        if text.len() <= room {
            self.raw.push_str(text);
            return true;
        }
        let mut end = room;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.raw.push_str(&text[..end]);
        self.cut = true;
        true
    }
}

/// The agent the assistant runs: `choice` ("auto" or an agent id) among this machine's installed ACP agents.
/// Err = why there is none (shown in Settings > Assistant).
pub(crate) fn resolve_agent(choice: &str, agents: &[Agent], machine_id: &str) -> std::result::Result<Agent, String> {
    let usable = |a: &&Agent| a.installed && a.source != AgentSource::A2a && (a.machine_id.is_empty() || a.machine_id == machine_id);
    let choice = choice.trim();
    if choice.is_empty() || choice == ASSISTANT_AUTO {
        for ids in AUTO_ORDER {
            if let Some(a) = agents.iter().filter(usable).find(|a| ids.contains(&a.id.as_str())) {
                return Ok(a.clone());
            }
        }
        let mut rest: Vec<&Agent> = agents.iter().filter(usable).collect();
        rest.sort_by(|a, b| a.id.cmp(&b.id));
        return rest
            .first()
            .map(|a| (*a).clone())
            .ok_or_else(|| "no coding agent (ACP) is installed on this machine: install Claude Code, Codex or Copilot CLI".to_string());
    }
    match agents.iter().find(|a| a.id == choice) {
        Some(a) if usable(&a) => Ok(a.clone()),
        Some(a) => Err(format!("{} is not installed on this machine", a.name)),
        None => Err(format!("the agent {choice:?} is not installed on this machine")),
    }
}

/// `session/new` `_meta` for the assistant's agent. Claude Code: no settings file loads (`settingSources: []`:
/// not the user's, not one the assistant could write into its own workspace), so no allow rule or default mode
/// lets a tool call skip the hub: every call reaches it as a permission request (and Chrome DevTools steps reach
/// the browser rules). Chrome DevTools' `evaluate_script` is never available. No MCP servers besides the
/// session's own (`strictMcpConfig`).
fn start_meta(agent_id: &str) -> Option<Value> {
    (agent_id == "claude-code" || agent_id == "claude-acp").then(|| {
        json!({ "claudeCode": { "options": {
            "settingSources": [],
            "strictMcpConfig": true,
            "disallowedTools": [super::browser::mcp_tool(super::browser::EVALUATE_SCRIPT)],
            "allowDangerouslySkipPermissions": false,
        } } })
    })
}

/// The pet's line when the assistant cannot do the work: why, and where to fix it.
fn unavailable_line(why: &str) -> String {
    format!("(I can't do the work myself right now: {}. Fix it in Settings > Assistant.)", why.trim_end_matches('.'))
}

impl Hub {
    fn assistant_rt(&self) -> std::sync::MutexGuard<'_, AssistantRt> {
        self.inner.assistant.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The assistant's folder: `[assistant] workspace` with "~" expanded, or `~/Pawl`. Absolute, not
    /// canonicalized; it may not exist until first use. Empty when there is no home folder.
    pub(crate) fn assistant_workspace(&self) -> PathBuf {
        self.cfg().assistant.workspace_path().unwrap_or_default()
    }

    /// One of this machine's assistant sessions (live or not).
    pub(crate) fn is_assistant_session(&self, session_id: &str) -> bool {
        self.session(session_id).is_some_and(|s| s.purpose.as_deref() == Some(PURPOSE) && s.machine_id == self.inner.machine_id)
    }

    /// The id of the assistant session being started (not registered yet): `merge_session` marks its snapshots.
    pub(crate) fn assistant_starting(&self, session_id: &str) -> bool {
        self.assistant_rt().starting.as_deref() == Some(session_id)
    }

    /// The registered live assistant session's id (None while there is none, or while a start is not registered).
    pub(crate) fn assistant_session_id(&self) -> Option<SessionId> {
        self.live_assistant().map(|s| s.id)
    }

    /// The live assistant session, if any (never one that is waiting to be closed).
    fn live_assistant(&self) -> Option<Session> {
        let id = {
            let rt = self.assistant_rt();
            if rt.must_end {
                return None;
            }
            rt.session_id.clone()?
        };
        self.session(&id).filter(|s| s.ended_at.is_none())
    }

    /// The assistant session takes prompts from this machine's own clients and its paired phones only: never from
    /// another hub, an outside A2A caller or the assistant's own A2A caller (`pawl mcp`).
    pub(crate) fn check_assistant_caller(&self, session_id: &str, caller: &Caller) -> Result<()> {
        if !self.is_assistant_session(session_id) {
            return Ok(());
        }
        let ok = match caller {
            Caller::Local => true,
            Caller::Peer(id) => self.devices().iter().any(|d| d.id == *id && d.kind == DeviceKind::Phone),
            _ => false,
        };
        if ok {
            Ok(())
        } else {
            Err(anyhow::Error::new(AckCode::Forbidden).context("the assistant takes messages from this machine and its paired phones only"))
        }
    }

    /// The live view for Settings > Assistant (`resolvedAgent`, `state`, `sessionId`, `error`, `agentNote`).
    pub(crate) fn fill_assistant_view(&self, view: &mut HubSettingsAssistant) {
        let cfg = self.cfg().assistant.clone();
        let agents = self.st().agents.clone();
        let resolved = resolve_agent(&cfg.agent, &agents, &self.inner.machine_id);
        view.resolved_agent = resolved.as_ref().ok().map(|a| a.id.clone());
        view.agent_note = resolved.as_ref().ok().and_then(|a| super::a2a_assistant::assistant_agent_note(&a.id)).map(str::to_string);
        let live = self.live_assistant().map(|s| s.id);
        let rt = self.assistant_rt();
        view.session_id = live;
        let rotate_blocks = cfg.sessions.then(|| rt.rotate_error.clone()).flatten();
        view.error = rt.error.clone().or(rotate_blocks).or_else(|| resolved.err());
        view.state = if !cfg.enabled {
            AssistantState::Off
        } else if rt.turn.is_some() {
            AssistantState::Working
        } else if view.error.is_some() {
            AssistantState::Error
        } else {
            AssistantState::Idle
        };
        if !cfg.enabled {
            view.error = None;
        }
    }

    /// The workspace, created if needed, resolved (links followed) and checked again where it really is: not a
    /// drive root, the home folder, a system folder or overlapping Pawl's data folder; writable. Returns the
    /// resolved folder (the session's cwd).
    fn ensure_assistant_workspace(&self, cfg: &AssistantSection) -> std::result::Result<PathBuf, String> {
        let home = crate::config::home_dir();
        let stored = cfg.workspace.trim();
        if !stored.is_empty() {
            cfgset::check_workspace(stored, home.as_deref()).map_err(|e| format!("the workspace {}", e.reason))?;
        }
        cfgset::check_workspace_apart(cfg, &self.inner.paths.data_dir).map_err(|e| format!("the workspace {}", e.reason))?;
        let ws = cfg.workspace_path().ok_or_else(|| "there is no home folder for the workspace; set one".to_string())?;
        std::fs::create_dir_all(&ws).map_err(|e| format!("the workspace {} could not be created ({e})", ws.display()))?;
        // Made now: judged again where it really is (a link or junction created meanwhile counts).
        let real = std::fs::canonicalize(&ws)
            .ok()
            .and_then(|c| crate::repos::plain_path(&c))
            .ok_or_else(|| format!("the workspace {} is not a folder on this computer. Use a folder on this computer.", ws.display()))?;
        if let Some(why) = cfgset::workspace_place_problem(&real, home.as_deref(), Some(&self.inner.paths.data_dir)) {
            return Err(format!("the workspace {why}"));
        }
        let probe = real.join(format!(".pawl-write-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::write(&probe, b"").map_err(|e| format!("the workspace {} is not writable ({e})", real.display()))?;
        let _ = std::fs::remove_file(&probe);
        Ok(real)
    }

    /// Remember why the assistant could not start (Settings shows it) and pass it on.
    fn assistant_failed(&self, why: String) -> String {
        tracing::warn!("the assistant could not start: {why}");
        self.assistant_rt().error = Some(why.clone());
        self.announce_settings();
        why
    }

    // ------------------------------------------------------------ the A2A token

    /// Replace the assistant's A2A token if it is due (serialized; never called under `settings_write`). A failure
    /// is retried once; a second one is remembered (Settings shows it; no start with Pawl's tools) and returned.
    pub(crate) async fn rotate_assistant_token(&self) -> std::result::Result<(), String> {
        let _one = self.inner.assistant_rotate.lock().await;
        if !self.assistant_rt().rotate_due {
            return Ok(());
        }
        let mut last = String::new();
        for attempt in 1..=2 {
            match self.assistant_caller_rotate().await {
                Ok(()) => {
                    {
                        let mut rt = self.assistant_rt();
                        rt.rotate_due = false;
                        rt.rotate_error = None;
                    }
                    if attempt > 1 {
                        self.announce_settings();
                    }
                    return Ok(());
                }
                Err(e) => {
                    last = format!("{e:#}");
                    tracing::warn!(attempt, "could not replace the assistant's A2A token: {last}");
                }
            }
        }
        let why = format!("its old A2A token could not be replaced ({last}), so it does not start with Pawl's tools until it can be");
        self.assistant_rt().rotate_error = Some(why.clone());
        self.announce_settings();
        Err(why)
    }

    /// A session ended (or the assistant was reset or turned off): the token is due. It is replaced now in the
    /// background, once whoever holds `settings_write` is done; a new start waits for it (see above).
    pub(crate) fn assistant_token_due(&self) {
        self.assistant_rt().rotate_due = true;
        let hub = self.clone();
        tokio::spawn(async move {
            let _ = hub.rotate_assistant_token().await;
        });
    }

    // ------------------------------------------------------------ the session

    /// The live assistant session, started if there is none. Err = why it cannot start.
    async fn assistant_session(&self) -> std::result::Result<Session, String> {
        let _one = self.inner.assistant_start.lock().await;
        if self.assistant_rt().must_end {
            self.end_assistant_session("closing the previous session before a new start").await;
            if self.assistant_rt().must_end {
                return Err(self.assistant_failed("its previous session could not be closed".into()));
            }
        }
        if let Some(s) = self.live_assistant() {
            return Ok(s);
        }
        // A settings change while this start runs ends what it started; it tries once more with the new settings.
        for _ in 0..2 {
            let generation = self.assistant_rt().generation;
            let cfg = self.cfg().assistant.clone();
            if !cfg.enabled {
                return Err("assistant mode is off".into());
            }
            // The token of the previous session is replaced before anything new starts.
            if let Err(why) = self.rotate_assistant_token().await {
                if cfg.sessions {
                    return Err(self.assistant_failed(why));
                }
            }
            // The state lock is not held while a failure is announced (the settings view reads it).
            let agents = self.st().agents.clone();
            let agent = resolve_agent(&cfg.agent, &agents, &self.inner.machine_id).map_err(|e| self.assistant_failed(e))?;
            let ws = self.ensure_assistant_workspace(&cfg).map_err(|e| self.assistant_failed(e))?;
            // Config planted in the workspace would steer the agent without a card: an agent other than Claude
            // Code reads its own project config there (Pawl cannot switch that off); Claude Code loads none but runs
            // git there, so a git setting that runs a program is refused for it too.
            let claude_agent = start_meta(&agent.id).is_some();
            let planted = if claude_agent {
                super::a2a_assistant::git_config_in(&ws, &ws).map(|f| {
                    format!("There's a git setting that runs a program in your workspace ({}). Remove it.", f.display())
                })
            } else {
                super::a2a_assistant::agent_config_in(&ws, &ws)
                    .map(|f| format!("There's agent config in your workspace ({}). Remove it or use Claude Code.", f.display()))
            };
            if let Some(why) = planted {
                return Err(self.assistant_failed(why));
            }
            // Pawl's tools and the browser's (A2A part, `core/a2a_assistant.rs`). Without them it still works.
            let tools = match self.assistant_tools(&agent.id, cfg.sessions, cfg.browser).await {
                Ok(t) => t,
                Err(e) => {
                    tracing::warn!("the assistant starts without Pawl's tools: {e:#}");
                    Vec::new()
                }
            };
            let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
            let mut agent = agent;
            let claude = start_meta(&agent.id).is_some();
            let mut env = agent.env.clone().unwrap_or_default();
            if claude {
                // No claude.ai connectors (mail, drive, ...): the assistant's tools are the ones listed here.
                env.insert("ENABLE_CLAUDEAI_MCP_SERVERS".into(), "false".into());
            }
            // The agent's own safest mode where it has one (Codex: read-only, every approval reviewed by the user).
            env.extend(super::a2a_assistant::assistant_agent_env(&agent.id));
            agent.env = Some(env);
            // Claude Code: a clean environment too (its login lives in its config folder): the hub's own keys, such
            // as an API key for the pet's model, never reach the assistant or bill it.
            let opts = pawl_acp::StartOptions { meta: start_meta(&agent.id), mcp_servers: tools, clean_env: claude };
            let id = self.new_id();
            self.assistant_rt().starting = Some(id.clone());
            let started = self.inner.host.start_session_with(&agent, ws.clone(), id.clone(), None, opts).await;
            let mut started = match started {
                Ok(s) => s,
                Err(e) => {
                    self.assistant_rt().starting = None;
                    return Err(self.assistant_failed(format!("{} could not start ({e:#})", agent.name)));
                }
            };
            started.purpose = Some(PURPOSE.into());
            let session = self.merge_session(started).await;
            // It reads web pages and files: a user's auto-allow rule asks for it instead (as for A2A-started sessions).
            self.mark_remote_driven(&session.id).await;
            self.tasks().bind_context(&session.id, Caller::Local);
            let enabled_now = self.cfg().assistant.enabled;
            let registered = {
                let mut rt = self.assistant_rt();
                rt.starting = None;
                if rt.generation == generation && enabled_now {
                    rt.session_id = Some(session.id.clone());
                    rt.fresh = true;
                    rt.error = None;
                    true
                } else {
                    false
                }
            };
            if !registered {
                tracing::info!(session = %session.id, "the assistant's settings changed while it started: that session ends");
                let closed = self.close_assistant(&session.id).await;
                self.assistant_token_due();
                if !closed {
                    // Kept until it is closed: the next start closes it first (never reused meanwhile).
                    tracing::warn!(session = %session.id, "the stale assistant session could not be closed; it is closed again before the next start");
                    {
                        let mut rt = self.assistant_rt();
                        rt.session_id = Some(session.id.clone());
                        rt.must_end = true;
                    }
                    return Err(self.assistant_failed("its previous session could not be closed".into()));
                }
                continue;
            }
            if let Err(e) = self
                .record_event(&session.id, EventType::SessionStart, json!({ "agentId": agent.id, "cwd": session.cwd, "purpose": PURPOSE }))
                .await
            {
                tracing::warn!("record the assistant's session start: {e:#}");
            }
            tracing::info!(session = %session.id, agent = %agent.id, tools = %tool_names.join(","), "assistant session started");
            let tools = if tool_names.is_empty() { "none".to_string() } else { tool_names.join(", ") };
            let detail = format!("start {} in {} (tools: {tools})", agent.id, ws.display());
            self.audit_settings(&Actor::internal("assistant"), SettingsAuditKind::Assistant, vec![AUDIT_KEY.into()], Some(detail)).await;
            self.announce_settings();
            return Ok(self.session(&session.id).unwrap_or(session));
        }
        Err(self.assistant_failed("its settings changed while it was starting; send your message again".into()))
    }

    /// Close an assistant session (one retry). True when it is closed (or had ended already).
    async fn close_assistant(&self, id: &str) -> bool {
        let ended = |hub: &Hub| hub.session(id).is_none_or(|s| s.ended_at.is_some());
        for attempt in 1..=2 {
            if ended(self) {
                return true;
            }
            match self.close_session(id).await {
                Ok(()) => return true,
                Err(e) => tracing::warn!(session = %id, attempt, "could not close the assistant session: {e:#}"),
            }
        }
        ended(self)
    }

    /// `assistant_reset`: end the live session (the next message starts a fresh one) and replace the assistant's
    /// A2A token now. The chat history stays.
    pub(crate) async fn assistant_reset(&self, why: &str, actor: &Actor) -> Result<()> {
        let ended = self.end_assistant_session(why).await;
        self.assistant_rt().rotate_due = true;
        let rotated = self.rotate_assistant_token().await;
        let detail = format!(
            "reset ({why}){}{}",
            if ended { "" } else { ", no session was live" },
            if rotated.is_ok() { "" } else { ", token not replaced" }
        );
        self.audit_settings(actor, SettingsAuditKind::Assistant, vec![AUDIT_KEY.into()], Some(detail)).await;
        Ok(())
    }

    /// Close the live assistant session, if any. True if one was live. Its id is kept until it is really closed
    /// (else the next start closes it first); the token is due either way.
    pub(crate) async fn end_assistant_session(&self, why: &str) -> bool {
        let id = {
            let mut rt = self.assistant_rt();
            rt.fresh = false;
            rt.session_id.clone()
        };
        let Some(id) = id else {
            self.announce_settings();
            return false;
        };
        tracing::info!(session = %id, %why, "assistant session ending");
        self.finish_assistant_turn(&id, Some("(The assistant's session ended.)")).await;
        let closed = self.close_assistant(&id).await;
        {
            let mut rt = self.assistant_rt();
            if closed {
                if rt.session_id.as_deref() == Some(id.as_str()) {
                    rt.session_id = None;
                }
                rt.must_end = false;
            } else {
                rt.must_end = true;
            }
        }
        if !closed {
            tracing::warn!(session = %id, "the assistant session could not be closed; it is closed again before the next start");
        }
        self.assistant_token_due();
        self.announce_settings();
        true
    }

    /// Settings changed (`apply_runtime`, `settings_write` held): an assistant setting ends the live session (and
    /// any start under way); turning it off makes the token due. A previous start error is forgotten.
    pub(crate) async fn on_assistant_settings(&self, next: &crate::config::HubConfig, changed: &[&'static str]) {
        let assistant_changed = cfgset::ASSISTANT_KEYS.iter().any(|k| changed.contains(k));
        // Browser use turned off: the assistant's Chrome DevTools server goes with its session.
        let browser_off = changed.contains(&cfgset::BROWSER_ENABLED) && !next.browser.enabled && next.assistant.browser;
        {
            let mut rt = self.assistant_rt();
            rt.error = None;
            if assistant_changed || browser_off {
                rt.generation += 1;
            }
        }
        if assistant_changed || browser_off {
            self.end_assistant_session("settings changed").await;
        } else {
            self.announce_settings();
        }
        if changed.contains(&cfgset::ASSISTANT_ENABLED) && !next.assistant.enabled {
            self.assistant_token_due();
        }
    }

    /// The assistant session ended (any way: closed, idle, crashed): forget it; its token is due.
    pub(crate) async fn on_assistant_session_ended(&self, s: &Session) {
        {
            let mut rt = self.assistant_rt();
            if rt.session_id.as_deref() == Some(s.id.as_str()) {
                rt.session_id = None;
                rt.fresh = false;
                rt.must_end = false;
            }
        }
        self.finish_assistant_turn(&s.id, Some("(The assistant's session ended.)")).await;
        self.assistant_token_due();
        self.announce_settings();
    }

    // ------------------------------------------------------------ the chat

    /// A message to the default pet while assistant mode is on. Some(reply) = the brain answers (`None` text
    /// prefix = as today; a line = why the assistant could not take it); None = handled here.
    pub(super) async fn assistant_chat(
        &self,
        user: &ChatMessage,
        route: AssistantRoute,
        spoken: bool,
        image: Option<ChatImage>,
        actor: &Actor,
        ctx: &pawl_brain::chat::BrainContext,
    ) -> Option<(Option<String>, Option<ChatImage>)> {
        match route {
            AssistantRoute::Brain => Some((None, image)),
            AssistantRoute::CardWaiting => {
                self.pet_says(&persona::card_waiting_text(ctx)).await;
                None
            }
            AssistantRoute::Reset => {
                if let Err(e) = self.assistant_reset("new chat", actor).await {
                    tracing::warn!("assistant reset: {e:#}");
                }
                self.pet_says("Starting fresh: my next reply begins a new session. Our chat history stays here.").await;
                None
            }
            AssistantRoute::Stop => {
                let busy = self.assistant_rt().turn.as_ref().map(|t| t.session_id.clone());
                match busy {
                    Some(sid) => {
                        if let Err(e) = self.cancel(&sid, &actor.via_chat()).await {
                            self.pet_says(&format!("I couldn't stop it: {e:#}")).await;
                        }
                        // The turn's own message ends with its stop event.
                    }
                    None => self.pet_says("Nothing is running right now.").await,
                }
                None
            }
            AssistantRoute::Assistant => match self.assistant_prompt(user, spoken, image).await {
                Ok(()) => None,
                Err((why, image)) => Some((Some(unavailable_line(&why)), image)),
            },
        }
    }

    /// A new pet reply for a turn of `session_id` (empty, streaming).
    fn new_reply(&self, session_id: &str) -> ChatMessage {
        ChatMessage {
            id: self.new_id(),
            machine_id: self.inner.machine_id.clone(),
            pet_id: None,
            role: ChatRole::Pet,
            text: String::new(),
            ts: pawl_protocol::now_ms(),
            session_id: Some(session_id.to_string()),
            proposals: Vec::new(),
            image: false,
            streaming: true,
            speech: None,
        }
    }

    /// Take the turn for `message` (false: a turn is already running), then store and send the empty reply.
    async fn claim_turn(&self, session_id: &str, message: &ChatMessage, spoken: bool) -> bool {
        {
            let mut rt = self.assistant_rt();
            if rt.turn.is_some() {
                return false;
            }
            rt.turn = Some(Turn::new(session_id.to_string(), message.clone(), spoken));
        }
        let m2 = message.clone();
        if let Err(e) = self.inner.store.run(move |s| s.insert_chat(&m2)).await {
            tracing::warn!("persist the assistant's reply: {e:#}");
        }
        self.inner.bus.send(ServerMsg::Chat { message: message.clone() });
        self.announce_settings();
        true
    }

    /// Send the user's message to the assistant session as one turn. Err = why it could not start (the brain
    /// answers instead; the picture comes back for it).
    async fn assistant_prompt(&self, user: &ChatMessage, spoken: bool, image: Option<ChatImage>) -> std::result::Result<(), (String, Option<ChatImage>)> {
        let session = match self.assistant_session().await {
            Ok(s) => s,
            Err(why) => return Err((why, image)),
        };
        let agent_name = self.st().agent_name(&session.agent_id);
        if image.is_some() && !self.inner.host.accepts_images(&session.id) {
            self.pet_says(&format!("{agent_name} can't see pictures, so I can't look at this one. Ask me in words instead?")).await;
            return Ok(());
        }
        // The reply exists from the start of the turn (empty, streaming), so the bubble shows it is coming. The turn
        // is taken in one step with the check, so of two messages at once exactly one becomes a turn.
        let message = self.new_reply(&session.id);
        if !self.claim_turn(&session.id, &message, spoken).await {
            self.pet_says(BUSY).await;
            return Ok(());
        }
        let cfg = self.cfg().assistant.clone();
        let mut text = user.text.clone();
        if text.trim().is_empty() && image.is_some() {
            text = "What's in this picture?".into();
        }
        if spoken {
            text = format!("{}\n{text}", persona::SPOKEN_NOTE);
        }
        let fresh = self.assistant_rt().fresh;
        if fresh {
            let p = PersonaInput {
                pet_name: self.default_pet_name(),
                agent_name: agent_name.clone(),
                machine_name: self.machine().name,
                workspace: session.cwd.clone(),
                sessions: cfg.sessions,
                browser: cfg.browser && self.cfg().browser.enabled && super::a2a_assistant::BROWSER_AGENTS.contains(&session.agent_id.as_str()),
            };
            text = persona::first_prompt(&p, &text);
        }
        let images: Vec<ChatImage> = image.into_iter().collect();
        match self.a2a_prompt_with(&session.id, text, images, &Caller::Local).await {
            Ok(_) => {
                if fresh {
                    self.assistant_rt().fresh = false;
                }
            }
            Err(e) => {
                tracing::warn!("prompt the assistant: {e:#}");
                self.finish_assistant_turn(&session.id, Some(&format!("(I couldn't send that to {agent_name}: {e:#})"))).await;
            }
        }
        Ok(())
    }

    /// A deterministic line from the default pet (stored and broadcast like a brain reply).
    async fn pet_says(&self, text: &str) {
        self.deliver_reply(None, BrainReply { text: text.into(), ..Default::default() }, &Actor::internal("assistant"), None).await;
    }

    /// An event of the assistant session: a prompt that came another way (a paired phone, the session view) gets
    /// a reply in the chat too; text streams into the turn's reply; the turn ends on stop or error.
    pub(crate) async fn on_assistant_event(&self, session_id: &str, kind: EventType, payload: &Value) {
        match kind {
            EventType::UserMessage => {
                let idle = self.assistant_rt().turn.is_none();
                if idle {
                    let message = self.new_reply(session_id);
                    self.claim_turn(session_id, &message, false).await;
                }
            }
            EventType::AgentMessageChunk => {
                if let Some(t) = payload.get("text").and_then(Value::as_str) {
                    self.on_assistant_text(session_id, t);
                }
            }
            EventType::Stop => {
                let cancelled = payload.get("stopReason").and_then(Value::as_str) == Some("cancelled");
                self.finish_assistant_turn(session_id, cancelled.then_some("(Stopped.)")).await;
            }
            EventType::Error => {
                let msg = payload.get("message").and_then(Value::as_str).unwrap_or("the agent reported an error");
                self.finish_assistant_turn(session_id, Some(&format!("(Something went wrong: {})", persona::reply_text(msg)))).await;
            }
            _ => {}
        }
    }

    /// More agent text: a `chat_update` now if the last one is [`UPDATE_EVERY`] old, else one is scheduled. Text
    /// past [`MAX_REPLY_BYTES`] is dropped.
    fn on_assistant_text(&self, session_id: &str, text: &str) {
        let (now_msg, wait) = {
            let mut rt = self.assistant_rt();
            let Some(turn) = rt.turn.as_mut().filter(|t| t.session_id == session_id) else { return };
            if !turn.push(text) {
                return;
            }
            let since = turn.last_sent.map(|t| t.elapsed());
            match since {
                Some(d) if d < UPDATE_EVERY => {
                    if turn.flush_scheduled {
                        (None, None)
                    } else {
                        turn.flush_scheduled = true;
                        (None, Some((UPDATE_EVERY - d, turn.message.id.clone())))
                    }
                }
                _ => {
                    turn.last_sent = Some(Instant::now());
                    (Some(turn.snapshot()), None)
                }
            }
        };
        if let Some(message) = now_msg {
            self.inner.bus.send(ServerMsg::ChatUpdate { message });
        }
        if let Some((wait, message_id)) = wait {
            let hub = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep(wait).await;
                hub.flush_assistant_text(&message_id);
            });
        }
    }

    /// The scheduled `chat_update` of reply `message_id` (nothing if that turn is over).
    fn flush_assistant_text(&self, message_id: &str) {
        let msg = {
            let mut rt = self.assistant_rt();
            let Some(turn) = rt.turn.as_mut().filter(|t| t.message.id == message_id) else { return };
            turn.flush_scheduled = false;
            turn.last_sent = Some(Instant::now());
            turn.snapshot()
        };
        self.inner.bus.send(ServerMsg::ChatUpdate { message: msg });
    }

    /// End the running turn of `session_id`: its reply gets its final text (`note` added) and `streaming: false`,
    /// is saved (awaited), and goes out as the last `chat_update`.
    async fn finish_assistant_turn(&self, session_id: &str, note: Option<&str>) {
        let Some(turn) = ({
            let mut rt = self.assistant_rt();
            if rt.turn.as_ref().is_some_and(|t| t.session_id == session_id) {
                rt.turn.take()
            } else {
                None
            }
        }) else {
            return;
        };
        let mut text = persona::reply_text(&turn.raw);
        let mut notes: Vec<&str> = Vec::new();
        if turn.cut {
            notes.push(CUT);
        }
        notes.extend(note);
        for n in notes {
            if !text.is_empty() {
                text.push_str("\n\n");
            }
            text.push_str(n);
        }
        if text.is_empty() {
            text = "Done.".into();
        }
        let speech = turn.spoken.then(|| persona::short_form(&text));
        let message = ChatMessage { text, streaming: false, speech, ..turn.message };
        let stored = ChatMessage { speech: None, ..message.clone() };
        if let Err(e) = self.inner.store.run(move |s| s.finish_chat(&stored)).await {
            tracing::warn!("save the assistant's reply: {e:#}");
        }
        self.inner.bus.send(ServerMsg::ChatUpdate { message });
        self.announce_settings();
    }

    /// Ack `{}` for `assistant_reset` (local clients).
    pub(crate) async fn assistant_reset_msg(&self, actor: &Actor) -> Result<Value> {
        if !self.cfg().assistant.enabled && self.assistant_rt().session_id.is_none() {
            return Ok(json!({}));
        }
        self.assistant_reset("reset from a client", actor).await?;
        Ok(json!({}))
    }

    /// Tests: whether the token is due, and the last replacement error.
    #[cfg(test)]
    pub(crate) fn assistant_token_state(&self) -> (bool, Option<String>) {
        let rt = self.assistant_rt();
        (rt.rotate_due, rt.rotate_error.clone())
    }

    /// Tests: the id of the session a start is bringing up.
    #[cfg(test)]
    pub(crate) fn assistant_starting_id(&self) -> Option<SessionId> {
        self.assistant_rt().starting.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, installed: bool, source: AgentSource) -> Agent {
        Agent {
            id: id.into(),
            machine_id: "abcdef012345".into(),
            name: crate::core::pretty_agent_name(id),
            command: vec![id.into()],
            env: None,
            version: None,
            description: None,
            icon: None,
            installed,
            source,
            capabilities: None,
        }
    }

    #[test]
    fn auto_picks_claude_then_codex_then_copilot_then_any_acp_agent() {
        let me = "abcdef012345";
        let all = vec![
            agent("gemini", true, AgentSource::Registry),
            agent("copilot", true, AgentSource::Registry),
            agent("codex", true, AgentSource::Registry),
            agent("claude-code", true, AgentSource::Registry),
        ];
        assert_eq!(resolve_agent("auto", &all, me).unwrap().id, "claude-code");
        assert_eq!(resolve_agent("auto", &all[..3], me).unwrap().id, "codex");
        assert_eq!(resolve_agent("", &all[..2], me).unwrap().id, "copilot");
        assert_eq!(resolve_agent("auto", &all[..1], me).unwrap().id, "gemini");
        // Not installed, an outside A2A agent or another machine's agent never counts.
        let none = vec![
            agent("claude-code", false, AgentSource::Registry),
            agent("a2a:helper", true, AgentSource::A2a),
            Agent { machine_id: "0123456789ab".into(), ..agent("codex", true, AgentSource::Registry) },
        ];
        let e = resolve_agent("auto", &none, me).unwrap_err();
        assert!(e.contains("no coding agent"), "{e}");
        // A chosen agent: used when installed, an error state when not (never another agent instead).
        assert_eq!(resolve_agent("gemini", &all, me).unwrap().id, "gemini");
        let e = resolve_agent("claude-code", &none, me).unwrap_err();
        assert!(e.contains("Claude Code is not installed"), "{e}");
        assert!(resolve_agent("my-agent", &all, me).unwrap_err().contains("\"my-agent\" is not installed"));
    }

    #[test]
    fn claude_code_loads_no_settings_file_and_never_evaluates_scripts() {
        let m = start_meta("claude-code").unwrap();
        assert_eq!(m["claudeCode"]["options"]["settingSources"], json!([]), "no settings file can allow a tool");
        assert_eq!(m["claudeCode"]["options"]["disallowedTools"], json!(["mcp__chrome-devtools__evaluate_script"]));
        assert_eq!(m["claudeCode"]["options"]["allowDangerouslySkipPermissions"], json!(false));
        assert!(m["claudeCode"]["options"].get("mcpServers").is_none(), "the session's servers only");
        assert!(start_meta("codex").is_none());
    }
}
