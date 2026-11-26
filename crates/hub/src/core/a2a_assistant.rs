//! Assistant mode, the A2A part (docs/CONTRACTS.md "v0.2 Assistant mode: the pet does the work"):
//! Pawl's own tools for the assistant session that `core/assistant.rs` runs.
//!
//! - The assistant's A2A caller [`ASSISTANT_CALLER_ID`] ("Pawl assistant", `connect_target =
//!   "assistant"` in config.toml): made on first use, never answers approvals, not in the settings
//!   list (a settings patch can neither change it nor take its id; `a2a_rotate_token` refuses it). Its
//!   token is kept in config.toml like every caller's, given only to the assistant session's `pawl
//!   mcp`, and replaced by [`Hub::assistant_caller_rotate`] (assistant reset, assistant turned off):
//!   the old one stops working at once. A caller the user made by hand with that id is taken over
//!   (new token, new task owner), as Connect does.
//! - [`Hub::assistant_tools`]: the session's MCP servers. `pawl mcp` (this binary, `mcp`,
//!   `PAWL_A2A_URL` + `PAWL_A2A_TOKEN`), turning the localhost A2A server on if it is off; and, with
//!   browser use on, the Chrome DevTools server with the browser tasks' pinned package and flags, for
//!   agents whose Chrome DevTools tool calls the hub can read ([`BROWSER_AGENTS`]), so
//!   `browser::classify` decides every one of its steps.
//! - Start folders: the assistant caller may also start agents in the assistant's workspace and its
//!   subfolders ([`Hub::a2a_start_dir`]).
//! - Every start the assistant caller asks for is a hub-side card first ([`Hub::start_card`]):
//!   "Start <Agent> in <folder>" with the whole prompt, allow once / reject once, answered by the user
//!   only (no rule, no A2A caller, no other hub); the A2A call waits for it ([`START_CARD_WAIT`]).
//! - Planted agent config ([`Hub::assistant_launch_plan`]): in the workspace (which the assistant can
//!   write), Claude Code starts with no settings files, no project MCP servers and bypass off; any
//!   other agent is refused in a folder that (or a folder above it, up to the workspace) holds agent
//!   config. Codex starts in its `read-only` mode (asks the user to write or go online).
//! - Tool calls of the assistant session and of the sessions it started that write an agent's config
//!   or hooks (`pawl_brain::rules::writes_agent_config`) are always cards: no user rule allows them.
//! - Every create, takeover and rotate goes to `settings_audit` (`a2a.connect.assistant`); tokens are
//!   never logged.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use pawl_acp::McpServerStdio;
use pawl_protocol::{
    global_id, IntentRejectReason, PermissionOption, PermissionOptionKind, PermissionRequest, ResolvedBy, ToolCallPayload, ToolKind, ToolLocation,
};
use serde_json::{json, Value};
use tokio::sync::watch;

use super::a2a::LaunchPlan;
use super::{a2a_connect, a2a_server, browser, Actor, Hub};
use crate::config::{A2aCaller, ConnectTarget};
use crate::settings::{self as cfgset, ASSISTANT_CALLER_ID};
use crate::store::SettingsAuditKind;

/// The assistant caller's name.
pub(crate) const ASSISTANT_CALLER_NAME: &str = "Pawl assistant";
/// The `pawl mcp` server's name in the session (Claude Code: `mcp__pawl__<tool>`).
pub(crate) const PAWL_MCP_SERVER: &str = "pawl";
/// Agents whose Chrome DevTools tool calls the hub can read (claude-agent-acp titles them
/// `chrome-devtools: <tool>`, read in `browser::tool_of`), so the browser rules decide every step.
/// Other agents get no Chrome DevTools server: their steps could reach the user's rules unchecked.
pub(crate) const BROWSER_AGENTS: &[&str] = &[browser::BROWSER_AGENT];
/// The audit key of the assistant caller's changes.
const AUDIT_KEY: &str = "a2a.connect.assistant";
/// `toolCall.toolCallId` prefix of a start card (only the hub's cards carry it; an agent's request
/// with it is renamed on arrival).
pub(crate) const START_CARD_PREFIX: &str = "pawl-start:";
/// `toolCall.rawInput.kind` of a start card.
pub(crate) const START_CARD_KIND: &str = "pawl_start";
/// How long a start waits for the user's answer; then the card is cancelled and the start refused.
pub(crate) const START_CARD_WAIT: Duration = Duration::from_secs(5 * 60);
/// The agents that read Claude Code's settings (and take its `_meta` options).
const CLAUDE_AGENTS: &[&str] = &["claude-code", "claude-acp"];
/// Codex's session mode for sessions the assistant starts (codex-acp `INITIAL_AGENT_MODE`; `read-only`:
/// approval policy on-request, reviewed by the user, read-only sandbox).
const CODEX_MODE: (&str, &str) = ("INITIAL_AGENT_MODE", "read-only");
/// Start cards one caller may have waiting at once.
pub(crate) const MAX_START_CARDS: usize = 2;
/// Parts of a git config key (compared without case) that make git run a program, or a section that
/// pulls in another file.
const GIT_EXEC_KEYS: &[&str] = &[
    "fsmonitor", "hookspath", "sshcommand", "gitproxy", "pager", "editor", "askpass", "external", "textconv", "command", "program", "helper",
    "driver", "smudge", "clean", "process",
];

/// The caution the assistant's settings show for an agent whose own approval settings Pawl cannot
/// override (`HubSettingsAssistant.agentNote`); `None` for Claude Code (Pawl sets its options).
pub(crate) fn assistant_agent_note(agent_id: &str) -> Option<&'static str> {
    match agent_id {
        a if CLAUDE_AGENTS.contains(&a) => None,
        "codex" => Some("Codex's own approval settings apply: if Codex is set to run commands without asking, Pawl can't stop that. Starting sessions still asks you."),
        "copilot" => Some("GitHub Copilot's own approval settings apply: if Copilot is set to allow all tools, Pawl can't stop that. Starting sessions still asks you."),
        "gemini" => Some("Gemini CLI's own approval settings apply: if Gemini is set to run tools without asking (YOLO), Pawl can't stop that. Starting sessions still asks you."),
        _ => Some("This agent's own approval settings apply: if it is set to run tools without asking, Pawl can't stop that. Starting sessions still asks you."),
    }
}

/// Environment for an agent the assistant runs or starts: Codex in its `read-only` mode (asks the user,
/// never its automatic reviewer). Nothing for other agents.
pub(crate) fn assistant_agent_env(agent_id: &str) -> Vec<(String, String)> {
    if agent_id == "codex" {
        vec![(CODEX_MODE.0.into(), CODEX_MODE.1.into())]
    } else {
        Vec::new()
    }
}

/// `session/new` `_meta` for Claude Code started in the assistant's workspace: no settings files (no
/// planted permissions, hooks or modes), no project MCP servers, bypass mode off.
pub(crate) fn workspace_claude_meta() -> Value {
    json!({ "claudeCode": { "options": {
        "settingSources": [],
        "strictMcpConfig": true,
        "mcpServers": {},
        "allowDangerouslySkipPermissions": false,
    } } })
}

/// What reading a git config file gave.
enum GitConfig {
    Missing,
    /// It exists but cannot be read: counts as planted.
    Unreadable,
    Text(String),
}

fn read_git_config(p: &Path) -> GitConfig {
    match std::fs::read(p) {
        Ok(bytes) => GitConfig::Text(String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => GitConfig::Missing,
        Err(_) => GitConfig::Unreadable,
    }
}

/// Where a section header `[...]` ends (its `]` outside quotes), if it does.
fn header_end(line: &str) -> Option<usize> {
    let (mut quoted, mut escaped) = (false, false);
    for (i, c) in line.char_indices().skip(1) {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ']' if !quoted => return Some(i),
            _ => {}
        }
    }
    None
}

/// Does git config `text` make git run a program ([`GIT_EXEC_KEYS`], an `alias` starting with `!`,
/// `uploadpack.packObjectsHook`, `remote.<name>.vcs`), pull in another file (`[include]`,
/// `[includeIf ...]`) or switch on per-worktree config (`extensions.worktreeConfig`)? Keys and section
/// names without case; a key after the header on the same line counts; a header that does not close
/// counts as yes.
fn config_runs(text: &str) -> bool {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut section = String::new();
    for raw in text.lines() {
        let mut line = raw.trim();
        if line.starts_with('[') {
            let Some(end) = header_end(line) else { return true };
            section = line[1..end].trim().split(|c: char| c.is_whitespace() || c == '.' || c == '"').next().unwrap_or_default().to_lowercase();
            if section.starts_with("include") {
                return true;
            }
            line = line[end + 1..].trim();
        }
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let (key, value) = match line.split_once('=') {
            Some((k, v)) => (k.trim().to_lowercase(), v.trim().trim_matches('"').trim()),
            None => (line.to_lowercase(), ""),
        };
        if GIT_EXEC_KEYS.iter().any(|k| key.contains(k)) {
            return true;
        }
        let runs = match (section.as_str(), key.as_str()) {
            ("alias", _) => value.starts_with('!'),
            ("uploadpack", "packobjectshook") | ("remote", "vcs") | ("extensions", "worktreeconfig") => true,
            _ => false,
        };
        if runs {
            return true;
        }
    }
    false
}

/// A planted git setup in folder `d`: a `.git` that is not a plain folder (a `gitdir:` file, a link),
/// a `commondir` pointing at another repository, a hook that is not one of git's `.sample` files, or a
/// `config` / `config.worktree` that runs a program ([`config_runs`]) or cannot be read.
fn planted_git(d: &Path) -> Option<PathBuf> {
    let git = d.join(".git");
    match std::fs::symlink_metadata(&git) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(git),
        Ok(m) if !m.is_dir() => return Some(git),
        Ok(_) => {}
    }
    if git.join("commondir").exists() {
        return Some(git.join("commondir"));
    }
    let hooks = git.join("hooks");
    match std::fs::read_dir(&hooks) {
        Ok(rd) => {
            for e in rd {
                let Ok(e) = e else { return Some(hooks) };
                if !e.file_name().to_string_lossy().to_lowercase().ends_with(".sample") {
                    return Some(e.path());
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Some(hooks),
    }
    for name in ["config", "config.worktree"] {
        let p = git.join(name);
        match read_git_config(&p) {
            GitConfig::Missing => {}
            GitConfig::Unreadable => return Some(p),
            GitConfig::Text(t) if config_runs(&t) => return Some(p),
            GitConfig::Text(_) => {}
        }
    }
    None
}

/// `dir` (canonical), then each folder above it up to and including `workspace` (canonical).
fn up_to(dir: &Path, workspace: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut cur = Some(dir);
    while let Some(d) = cur {
        out.push(d.to_path_buf());
        if crate::repos::path_key(d) == crate::repos::path_key(workspace) {
            break;
        }
        cur = d.parent().filter(|p| crate::repos::is_within(p, workspace));
    }
    out
}

/// The first planted git setup ([`planted_git`]) in `dir` or a folder above it, up to the workspace.
/// It matters for every agent (Claude Code runs `git status` when it starts).
pub(crate) fn git_config_in(dir: &Path, workspace: &Path) -> Option<PathBuf> {
    up_to(dir, workspace).iter().find_map(|d| planted_git(d))
}

/// The first agent config in `dir` or a folder above it, up to and including `workspace` (both
/// canonical): an entry named on the shared lists (`pawl_brain::rules::is_agent_config_name`, the same
/// lists as the write guard, file or folder alike) or a planted git setup. Its path, for the refusal.
pub(crate) fn agent_config_in(dir: &Path, workspace: &Path) -> Option<PathBuf> {
    up_to(dir, workspace).iter().find_map(|d| {
        let entry = std::fs::read_dir(d).into_iter().flatten().flatten().find(|e| pawl_brain::rules::is_agent_config_name(&e.file_name().to_string_lossy()));
        entry.map(|e| e.path()).or_else(|| planted_git(d))
    })
}

fn actor() -> Actor {
    Actor::internal("assistant")
}

/// Cancels a start card whose start stopped waiting without saying so (the A2A request was dropped:
/// the client went away, `pawl mcp` was stopped): the card is closed (`permission_resolved` cancelled,
/// audited) and forgotten. Disarmed once the start handles the outcome itself.
struct StartCardGuard {
    hub: Hub,
    id: String,
    armed: bool,
}

impl Drop for StartCardGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let (waiting, deciding) = {
            let mut st = self.hub.st();
            (st.pending.remove(&self.id), st.deciding.contains(&self.id))
        };
        match waiting {
            Some(req) => match tokio::runtime::Handle::try_current() {
                Ok(rt) => {
                    let hub = self.hub.clone();
                    rt.spawn(async move {
                        hub.close_permission(req, ResolvedBy::System, &Actor::internal("assistant-start"), "the start stopped waiting (its caller went away)").await;
                    });
                }
                Err(_) => self.hub.drop_start_card(&self.id),
            },
            // An answer is being applied right now: it takes the entry itself.
            None if deciding => {}
            None => self.hub.drop_start_card(&self.id),
        }
    }
}

/// Why a start card stopped waiting.
enum CardEnd {
    Answered(Option<String>),
    TimedOut,
    Revoked,
    AskerGone,
}

fn push(changed: &mut Vec<&'static str>, key: &'static str) {
    if !changed.contains(&key) {
        changed.push(key);
    }
}

/// `dir` (canonical) inside the assistant's workspace? The workspace is canonicalized too (symlinks
/// and `..` resolved on both sides), so a link out of it or a `..` path does not count.
fn within_workspace(dir: &Path, workspace: &Path) -> bool {
    crate::repos::canonical_dir(workspace).is_some_and(|ws| crate::repos::is_within(dir, &ws))
}

impl Hub {
    /// The assistant session's MCP servers (see the module docs). `agent_id`: the session's agent;
    /// `sessions`: `pawl mcp`; `browser`: the Chrome DevTools server, only while `[browser] enabled`
    /// is on and only for [`BROWSER_AGENTS`]. Fails only when the assistant caller cannot be saved.
    pub(crate) async fn assistant_tools(&self, agent_id: &str, sessions: bool, browser: bool) -> Result<Vec<McpServerStdio>> {
        let mut out = Vec::new();
        if sessions {
            let exe = Self::hook_exe()?;
            let (url, token) = self.ensure_assistant_caller().await?;
            out.push(McpServerStdio {
                name: PAWL_MCP_SERVER.into(),
                command: exe,
                args: vec![pawl_discovery::mcp_config::MCP_ARG.into()],
                env: vec![(pawl_a2a::mcp::URL_ENV.into(), url), (pawl_a2a::mcp::TOKEN_ENV.into(), token)],
            });
        }
        if browser && self.cfg().browser.enabled {
            if !BROWSER_AGENTS.contains(&agent_id) {
                tracing::info!(agent = agent_id, "the assistant's agent gets no browser tools: Pawl cannot check its Chrome DevTools steps");
            } else {
                match tokio::task::spawn_blocking(browser::mcp_stdio).await.context("find npx")? {
                    Some(s) => out.push(s),
                    None => tracing::warn!("the assistant gets no browser tools: npx (Node.js) is not on the PATH"),
                }
            }
        }
        Ok(out)
    }

    /// A new token for the assistant caller; the old one stops working at once (its open streams
    /// end). Nothing to do when there is no assistant caller yet.
    pub(crate) async fn assistant_caller_rotate(&self) -> Result<()> {
        let _one = self.inner.settings_write.lock().await;
        let cur = self.config();
        let mut next = cur.clone();
        let Some(c) = next.a2a.callers.iter_mut().find(|c| c.is_assistant()) else { return Ok(()) };
        c.token = Some(a2a_server::new_token()?);
        c.allow_approvals = false;
        self.commit_settings(&cur, next, &[cfgset::A2A_CALLERS, cfgset::A2A_TOKENS], &actor(), SettingsAuditKind::Settings).await?;
        tracing::info!("the assistant's A2A token was replaced");
        self.audit_settings(&actor(), SettingsAuditKind::A2aConnect, vec![AUDIT_KEY.into()], Some("rotate".into())).await;
        Ok(())
    }

    /// The assistant caller (made, taken over or given a token if needed) and the A2A server (turned
    /// on if it is off, 127.0.0.1 only): `(url, token)` for `pawl mcp`.
    async fn ensure_assistant_caller(&self) -> Result<(String, String)> {
        let _one = self.inner.settings_write.lock().await;
        let cur = self.config();
        let mut next = cur.clone();
        let mut changed = Vec::new();
        if !next.a2a.server {
            next.a2a.server = true;
            push(&mut changed, cfgset::A2A_SERVER);
        }
        let mut what = None;
        let token = match next.a2a.callers.iter_mut().find(|c| c.id == ASSISTANT_CALLER_ID) {
            Some(c) if c.is_assistant() && c.token.as_deref().is_some_and(|t| !t.trim().is_empty()) => {
                // config.toml edited by hand: approvals stay off.
                if c.allow_approvals {
                    c.allow_approvals = false;
                    push(&mut changed, cfgset::A2A_CALLERS);
                }
                c.token.clone().unwrap_or_default()
            }
            Some(c) => {
                what = Some(if c.is_assistant() { "create" } else { "takeover" });
                let token = a2a_server::new_token()?;
                *c = A2aCaller {
                    id: ASSISTANT_CALLER_ID.into(),
                    name: ASSISTANT_CALLER_NAME.into(),
                    token: Some(token.clone()),
                    allow_approvals: false,
                    connect_target: Some(ConnectTarget::Assistant),
                    connect_added_servers: false,
                };
                push(&mut changed, cfgset::A2A_CALLERS);
                push(&mut changed, cfgset::A2A_TOKENS);
                token
            }
            None => {
                what = Some("create");
                let token = a2a_server::new_token()?;
                next.a2a.callers.push(A2aCaller {
                    id: ASSISTANT_CALLER_ID.into(),
                    name: ASSISTANT_CALLER_NAME.into(),
                    token: Some(token.clone()),
                    allow_approvals: false,
                    connect_target: Some(ConnectTarget::Assistant),
                    connect_added_servers: false,
                });
                push(&mut changed, cfgset::A2A_CALLERS);
                push(&mut changed, cfgset::A2A_TOKENS);
                token
            }
        };
        if !changed.is_empty() {
            self.commit_settings(&cur, next.clone(), &changed, &actor(), SettingsAuditKind::Settings).await?;
        }
        // A hand-made caller taken over starts as a new task owner: it does not see that caller's
        // sessions. Only now, with the old token already revoked.
        if what == Some("takeover") {
            self.take_a2a_generation(ASSISTANT_CALLER_ID);
        }
        if let Some(w) = what {
            tracing::info!(change = w, starts_server = !cur.a2a.server, "the assistant's A2A caller was saved");
            self.audit_settings(&actor(), SettingsAuditKind::A2aConnect, vec![AUDIT_KEY.into()], Some(w.into())).await;
        }
        if !self.a2a_status().running {
            tracing::warn!("the A2A server is not running: the assistant's Pawl tools fail until it is (Settings, A2A)");
        }
        Ok((a2a_connect::a2a_url(&next), token))
    }

    /// The assistant session, or a session the assistant's caller started: changes to agent config are
    /// always cards there.
    pub(crate) fn assistant_guarded(&self, session_id: &str) -> bool {
        if self.is_assistant_session(session_id) {
            return true;
        }
        match self.tasks().owner_of(session_id) {
            pawl_a2a::Caller::Outside(key) => key.split('~').next() == Some(ASSISTANT_CALLER_ID),
            _ => false,
        }
    }

    /// How the assistant caller's start of `agent_id` in `dir` (canonical) runs, or why it may not:
    /// [`Self::workspace_launch_plan`], and Codex in its `read-only` mode everywhere.
    pub(super) fn assistant_launch_plan(&self, agent_id: &str, dir: &Path) -> std::result::Result<LaunchPlan, String> {
        let mut plan = self.workspace_launch_plan(agent_id, dir)?;
        plan.env.extend(assistant_agent_env(agent_id));
        Ok(plan)
    }

    /// How any remote start (an A2A caller, a paired machine) of `agent_id` in `dir` (canonical) runs,
    /// or why it may not. In the assistant's workspace (which the assistant can write): Claude Code
    /// gets [`workspace_claude_meta`] (clean environment, claude.ai connectors off), and any other
    /// agent is refused where agent config lies in that folder or a folder above it (up to the
    /// workspace). Elsewhere: nothing changes.
    pub(crate) fn workspace_launch_plan(&self, agent_id: &str, dir: &Path) -> std::result::Result<LaunchPlan, String> {
        let mut plan = LaunchPlan::default();
        let Some(ws) = crate::repos::canonical_dir(&self.assistant_workspace()) else { return Ok(plan) };
        if !crate::repos::is_within(dir, &ws) {
            return Ok(plan);
        }
        // Git runs for every agent: a planted hook or config would run a program with no card.
        if let Some(found) = git_config_in(dir, &ws) {
            return Err(format!(
                "forbidden: {} could make git run a program when the agent starts; remove it or start the agent in a folder without it",
                found.display()
            ));
        }
        if CLAUDE_AGENTS.contains(&agent_id) {
            plan.opts = pawl_acp::StartOptions { meta: Some(workspace_claude_meta()), clean_env: true, ..Default::default() };
            plan.env.push(("ENABLE_CLAUDEAI_MCP_SERVERS".into(), "false".into()));
            return Ok(plan);
        }
        match agent_config_in(dir, &ws) {
            Some(found) => Err(format!(
                "forbidden: {} holds agent settings, and in the assistant's workspace only Claude Code starts where agent settings are (Pawl turns them off for it); start the agent in a folder without them",
                found.display()
            )),
            None => Ok(plan),
        }
    }

    /// The card for one start the assistant asks for: "Start <Agent> in <folder>" with the whole prompt,
    /// on the assistant session, allow once / reject once. Waits for the user's answer, at most
    /// [`START_CARD_WAIT`], and ends early when the caller is revoked (`revoked` changes) or the
    /// assistant session that asked is no longer the live one. `Err`: why the start does not run.
    pub(super) async fn start_card(&self, caller_id: &str, agent_id: &str, dir: &Path, prompt: &str, mut revoked: watch::Receiver<u64>) -> std::result::Result<(), String> {
        let Some(sid) = self.assistant_session_id() else {
            return Err("the assistant session is not running, so nobody can approve this start".into());
        };
        let asking_agent = self.session(&sid).map(|s| s.agent_id).unwrap_or_default();
        let name = self.st().agent_name(agent_id);
        let folder = dir.to_string_lossy().into_owned();
        let local = uuid::Uuid::new_v4();
        let req = PermissionRequest {
            request_id: global_id(&self.inner.machine_id, local),
            machine_id: self.inner.machine_id.clone(),
            session_id: sid.clone(),
            agent_id: asking_agent,
            tool_call: ToolCallPayload {
                tool_call_id: format!("{START_CARD_PREFIX}{local}"),
                title: Some(format!("Start {name} in {folder}")),
                kind: Some(ToolKind::Execute),
                status: None,
                locations: Some(vec![ToolLocation { path: folder.clone(), line: None }]),
                raw_input: Some(json!({ "kind": START_CARD_KIND, "agentId": agent_id, "agentName": name, "cwd": folder, "prompt": prompt })),
            },
            options: vec![
                PermissionOption { option_id: "allow".into(), name: "Start".into(), kind: PermissionOptionKind::AllowOnce },
                PermissionOption { option_id: "reject".into(), name: "Don't start".into(), kind: PermissionOptionKind::RejectOnce },
            ],
            created_at: pawl_protocol::now_ms(),
            voice_code: None,
        };
        let id = req.request_id.clone();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        if !self.add_start_card(caller_id, &id, tx) {
            return Err("You already have a start waiting for the user.".into());
        }
        let mut guard = StartCardGuard { hub: self.clone(), id: id.clone(), armed: true };
        tracing::info!(request = %id, agent = agent_id, "the assistant asks to start a session; waiting for the user");
        self.show_card(req).await;
        let asker_gone = async {
            while self.assistant_session_id().as_deref() == Some(sid.as_str()) {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        };
        let end = tokio::select! {
            r = &mut rx => CardEnd::Answered(r.ok().flatten()),
            _ = tokio::time::sleep(START_CARD_WAIT) => CardEnd::TimedOut,
            _ = revoked.changed() => CardEnd::Revoked,
            _ = asker_gone => CardEnd::AskerGone,
        };
        // From here on this start handles the card itself.
        guard.armed = false;
        let answer = match end {
            CardEnd::Answered(a) => a,
            other => {
                // Not answered: cancel the card, unless an answer is on its way right now.
                let waiting = self.st().pending.remove(&id);
                match waiting {
                    Some(req) => {
                        let (why, refusal) = match other {
                            CardEnd::Revoked => ("its caller was revoked", "the assistant's access was replaced, so this start was dropped".to_string()),
                            CardEnd::AskerGone => ("the assistant session ended", "the assistant session that asked for this start has ended".to_string()),
                            _ => ("not answered in time", format!("the user did not answer within {} minutes, so {name} was not started", START_CARD_WAIT.as_secs() / 60)),
                        };
                        self.close_permission(req, ResolvedBy::System, &Actor::internal("assistant-start"), why).await;
                        self.drop_start_card(&id);
                        return Err(refusal);
                    }
                    None => rx.await.ok().flatten(),
                }
            }
        };
        match answer.as_deref() {
            Some("allow") if self.assistant_session_id().as_deref() == Some(sid.as_str()) => Ok(()),
            Some("allow") => Err("the assistant session that asked for this start has ended".into()),
            _ => Err(format!("the user did not allow starting {name} in {folder}")),
        }
    }

    /// Caller `id` is the assistant's own caller.
    pub(super) fn is_assistant_caller(&self, id: &str) -> bool {
        self.cfg().a2a.callers.iter().any(|c| c.id == id && c.is_assistant())
    }

    /// Where A2A caller `caller_id` may start an agent: [`Hub::remote_start_dir`] (this machine's
    /// repos and `[peer] start_roots`), and for the assistant caller also the assistant's workspace
    /// and its subfolders (same checks: absolute, existing, canonical, no network share).
    pub(super) async fn a2a_start_dir(&self, caller_id: &str, cwd: &str) -> std::result::Result<PathBuf, IntentRejectReason> {
        match self.remote_start_dir(cwd).await {
            Err(IntentRejectReason::Forbidden) if self.is_assistant_caller(caller_id) => {
                let raw = cwd.trim();
                if raw.starts_with("\\\\") || raw.starts_with("//") {
                    return Err(IntentRejectReason::Forbidden);
                }
                let dir = std::fs::canonicalize(raw).ok().and_then(|c| crate::repos::plain_path(&c)).ok_or(IntentRejectReason::Forbidden)?;
                if within_workspace(&dir, &self.assistant_workspace()) {
                    Ok(dir)
                } else {
                    Err(IntentRejectReason::Forbidden)
                }
            }
            other => other,
        }
    }
}
