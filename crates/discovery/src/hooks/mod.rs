//! Agent hooks: normalize raw hook JSON from Claude Code / Codex / Copilot, forward it to the
//! hub, and install hook snippets into the agents' config files.
//!
//! Flow: agent fires a hook -> runs `<hub_exe> hook <source>` (stdin JSON, or for Codex
//! `notify` the JSON as the last argv) -> [`run_hook_cli`] / [`forward`] adds a small
//! `_ai_pet` block (origin hints from the hook process env, never secrets) and POSTs to
//! `{hub}/hooks/{source}` -> the hub calls [`normalize`].

use pet_protocol::{AgentId, EventType, HookPayload, HookSource, NotificationLevel, SessionSource, SessionStatus};
use serde_json::{json, Map, Value};
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

mod install;
mod normalize;

pub use install::{install_with_home, InstallEnv};

/// Key of the metadata object `forward` adds to the raw hook JSON.
pub const META_KEY: &str = "_ai_pet";
/// Env var the Copilot hook entries set so camelCase payloads (no event name) stay identifiable.
pub const HOOK_EVENT_ENV: &str = "AI_PET_HOOK_EVENT";
/// Hard cap on hook input read from stdin.
const MAX_INPUT_BYTES: u64 = 16 * 1024 * 1024;
const FORWARD_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedHook {
    /// e.g. Claude `session_id`; synthesized from cwd if absent.
    pub external_session_id: String,
    /// "claude-code" | "codex" | "copilot"
    pub agent_id: AgentId,
    /// VscodeHook only for the vendors' VS Code extensions (Claude `claude-vscode` entrypoint,
    /// Codex VS Code originator, Copilot agent hooks run by the extension host). CLIs in any
    /// terminal, including VS Code's integrated terminal, are CliHook.
    pub source: SessionSource,
    pub cwd: Option<String>,
    pub title: Option<String>,
    /// SessionStart->idle, UserPromptSubmit->thinking, PreToolUse->working,
    /// Notification(permission)->waiting_permission, Stop->done, SessionEnd->done+ended
    pub status: Option<SessionStatus>,
    pub ended: bool,
    /// Usually Hook; SessionStart/SessionEnd map to session_start/session_end.
    pub event: EventType,
    pub payload: HookPayload,
    /// (level, title, body) worth surfacing.
    pub notification: Option<(NotificationLevel, String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookTarget {
    /// `~/.claude/settings.json` (or `$CLAUDE_CONFIG_DIR/settings.json`)
    Claude,
    /// `~/.codex/config.toml` `notify` (+ Codex lifecycle hooks), `$CODEX_HOME` honoured
    Codex,
    /// `~/.copilot/hooks/ai-pet.json` (`$COPILOT_HOME` honoured)
    CopilotCli,
    /// `<repo>/.github/hooks/ai-pet.json` (VS Code Copilot agent hooks)
    CopilotVscode { repo: PathBuf },
}

impl HookTarget {
    /// Parse the `pet-hub install-hooks <target>` argument; `repo` is used by `copilot-vscode`.
    pub fn from_cli(name: &str, repo: impl Into<PathBuf>) -> Option<HookTarget> {
        match name.trim().to_ascii_lowercase().as_str() {
            "claude" | "claude-code" => Some(HookTarget::Claude),
            "codex" => Some(HookTarget::Codex),
            "copilot-cli" | "copilot" => Some(HookTarget::CopilotCli),
            "copilot-vscode" | "vscode" => Some(HookTarget::CopilotVscode { repo: repo.into() }),
            _ => None,
        }
    }
}

pub fn hook_source_str(source: HookSource) -> &'static str {
    match source {
        HookSource::Claude => "claude",
        HookSource::Codex => "codex",
        HookSource::Copilot => "copilot",
    }
}

pub fn parse_hook_source(s: &str) -> Option<HookSource> {
    match s.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude-code" => Some(HookSource::Claude),
        "codex" => Some(HookSource::Codex),
        "copilot" | "copilot-cli" | "copilot-vscode" => Some(HookSource::Copilot),
        _ => None,
    }
}

/// Turn a raw hook payload into a hub-level update. `None` if `raw` is not a JSON object or is
/// not recognizable as a hook event of `source`.
pub fn normalize(source: HookSource, raw: serde_json::Value) -> Option<NormalizedHook> {
    match source {
        HookSource::Claude => normalize::claude(raw),
        HookSource::Codex => normalize::codex(raw),
        HookSource::Copilot => normalize::copilot(raw),
    }
}

/// Origin hints read from the hook process environment. Only non-secret, fixed variables.
pub fn env_meta() -> Value {
    env_meta_from(|k| std::env::var(k).ok())
}

fn env_meta_from(get: impl Fn(&str) -> Option<String>) -> Value {
    let mut m = Map::new();
    let mut put = |key: &str, var: &str| {
        if let Some(v) = get(var).filter(|v| !v.is_empty()) {
            m.insert(key.to_string(), Value::String(v.chars().take(200).collect()));
        }
    };
    put("term_program", "TERM_PROGRAM");
    put("claude_entrypoint", "CLAUDE_CODE_ENTRYPOINT");
    put("codex_originator", "CODEX_INTERNAL_ORIGINATOR_OVERRIDE");
    put("hook_event", HOOK_EVENT_ENV);
    // `extensionHost` for processes spawned by VS Code extensions (e.g. Copilot agent hooks).
    put("vscode_process_type", "VSCODE_CRASH_REPORTER_PROCESS_TYPE");
    // Variables VS Code's extension host carries but strips from integrated-terminal shells.
    // Terminal-only markers (VSCODE_IPC_HOOK_CLI, VSCODE_GIT_IPC_HANDLE, VSCODE_INJECTION) are
    // deliberately not used: an integrated terminal counts as a CLI session.
    let ext_host = ["VSCODE_PID", "VSCODE_IPC_HOOK", "VSCODE_HANDLES_UNCAUGHT_ERRORS", "VSCODE_NLS_CONFIG", "VSCODE_ESM_ENTRYPOINT", "VSCODE_AMD_ENTRYPOINT"]
        .iter()
        .any(|v| get(v).is_some_and(|s| !s.is_empty()));
    m.insert("vscode_ext_host".to_string(), Value::Bool(ext_host));
    Value::Object(m)
}

/// Add the `_ai_pet` block (unless the payload already has one).
pub fn enrich(raw: &mut Value) {
    if let Value::Object(m) = raw {
        m.entry(META_KEY.to_string()).or_insert_with(env_meta);
    }
}

/// Used by `pet-hub hook <source>`: POST raw hook JSON to `{hub_url}/hooks/{source}` with the
/// bearer token, 2s total timeout. Errors are returned for logging; callers must not fail the
/// agent because of them.
pub async fn forward(source: HookSource, raw: serde_json::Value, hub_url: &str, token: &str) -> anyhow::Result<()> {
    let mut raw = raw;
    enrich(&mut raw);
    let url = format!("{}/hooks/{}", hub_url.trim().trim_end_matches('/'), hook_source_str(source));
    let client = reqwest::Client::builder()
        .timeout(FORWARD_TIMEOUT)
        .connect_timeout(Duration::from_millis(800))
        .no_proxy() // the hub only listens on localhost
        .build()?;
    let resp = client.post(&url).bearer_auth(token.trim()).json(&raw).send().await?;
    let status = resp.status();
    anyhow::ensure!(status.is_success(), "hub rejected hook: HTTP {status}");
    Ok(())
}

/// Read the hook payload: a trailing argv JSON object (Codex `notify` passes it as the last
/// argument), else stdin (never blocks on an interactive terminal). Empty input -> `{}`.
pub fn read_input(extra_args: &[String]) -> anyhow::Result<Value> {
    if let Some(v) = extra_args.iter().rev().find_map(|a| {
        let t = a.trim();
        if t.starts_with('{') { serde_json::from_str::<Value>(t).ok().filter(Value::is_object) } else { None }
    }) {
        return Ok(v);
    }
    let stdin = std::io::stdin();
    if stdin.is_terminal() {
        return Ok(json!({}));
    }
    let mut buf = Vec::new();
    stdin.lock().take(MAX_INPUT_BYTES).read_to_end(&mut buf)?;
    let text = String::from_utf8_lossy(&buf);
    let text = text.trim().trim_start_matches('\u{feff}');
    if text.is_empty() {
        return Ok(json!({}));
    }
    Ok(serde_json::from_str(text)?)
}

/// What the hook process should print on stdout. Codex requires JSON for `Stop`/`Interrupt`
/// and Copilot documents `{}` as "no decision"; Claude Code gets nothing.
pub fn stdout_reply(source: HookSource, raw: &Value) -> Option<&'static str> {
    match source {
        HookSource::Claude => None,
        // notify payloads (`type: agent-turn-complete`) ignore stdout; lifecycle hooks want JSON.
        HookSource::Codex => raw.get("hook_event_name").map(|_| "{}"),
        HookSource::Copilot => Some("{}"),
    }
}

/// Complete `pet-hub hook <source> [payload...]` behaviour: read input, forward with the 2s
/// timeout, print the expected stdout reply. Never returns an error: hooks must not break or
/// block the agent when the hub is down.
pub async fn run_hook_cli(source: HookSource, extra_args: &[String], hub_url: &str, token: &str) {
    let raw = match read_input(extra_args) {
        Ok(v) => v,
        Err(e) => {
            tracing::debug!(error = %e, "hook input unreadable");
            json!({})
        }
    };
    if let Some(reply) = stdout_reply(source, &raw) {
        println!("{reply}");
    }
    if raw.as_object().is_some_and(|m| !m.is_empty()) {
        if let Err(e) = tokio::time::timeout(FORWARD_TIMEOUT, forward(source, raw, hub_url, token)).await.unwrap_or_else(|_| Err(anyhow::anyhow!("timeout"))) {
            tracing::debug!(error = %e, "hook forward failed (hub down?)");
        }
    }
}

/// Install hook snippets that run `<hub_exe> hook <source>`. `apply=false` returns a dry-run
/// diff; `apply=true` writes a timestamped `.bak` of each touched file and merges without
/// removing other hooks. Re-running is idempotent (our previous entries are replaced).
pub fn install(target: HookTarget, hub_exe: &Path, apply: bool) -> anyhow::Result<String> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("cannot determine home directory"))?;
    install_with_home(target, hub_exe, apply, &home, &InstallEnv::from_system())
}

#[cfg(test)]
mod tests;
