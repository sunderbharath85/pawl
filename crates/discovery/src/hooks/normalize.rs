//! Per-agent hook payload normalization.
//!
//! Payload references (checked 2026-11-23):
//! - Claude Code: <https://code.claude.com/docs/en/hooks> — snake_case, common fields
//!   `session_id`, `transcript_path`, `cwd`, `permission_mode`, `hook_event_name`.
//! - Codex: `notify` JSON (`type: agent-turn-complete`, `thread-id`, `turn-id`, `cwd`,
//!   `input-messages`, `last-assistant-message`) and Claude-compatible lifecycle hooks
//!   (<https://developers.openai.com/codex/hooks>).
//! - Copilot (CLI + VS Code share one harness, <https://docs.github.com/en/copilot/reference/hooks-reference>):
//!   camelCase payloads for camelCase event names (`sessionId`, `toolName`, `toolArgs`, no
//!   event name field) or snake_case "VS Code compatible" payloads with `hook_event_name`
//!   for PascalCase event names.

use super::{NormalizedHook, META_KEY};
use krypto_protocol::{EventType, HookPayload, HookSource, NotificationLevel, SessionSource, SessionStatus};
use serde_json::Value;

const MAX_MESSAGE: usize = 500;
const MAX_TITLE: usize = 80;
const MAX_BODY: usize = 240;
const MAX_RAW_STRING: usize = 4000;
const MAX_RAW_ARRAY: usize = 200;

// ---------- helpers ----------

fn s<'a>(raw: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| raw.get(*k).and_then(Value::as_str)).map(str::trim).filter(|v| !v.is_empty())
}

fn b(raw: &Value, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|k| raw.get(*k).and_then(Value::as_bool))
}

/// UTF-8 safe truncation with an ellipsis; collapses whitespace runs to single spaces.
pub(crate) fn clip(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Bound the stored raw payload: hooks can carry whole file contents (`tool_input.content`,
/// `tool_response`). Long strings / arrays are cut, structure is kept.
fn trim_raw(v: &mut Value, depth: usize) {
    match v {
        Value::String(st) if st.len() > MAX_RAW_STRING => {
            let total = st.chars().count();
            let mut cut: String = st.chars().take(MAX_RAW_STRING).collect();
            cut.push_str(&format!("…[truncated {} chars]", total.saturating_sub(MAX_RAW_STRING)));
            *st = cut;
        }
        Value::Array(a) => {
            if a.len() > MAX_RAW_ARRAY {
                let extra = a.len() - MAX_RAW_ARRAY;
                a.truncate(MAX_RAW_ARRAY);
                a.push(Value::String(format!("…[{extra} more items]")));
            }
            if depth < 16 {
                a.iter_mut().for_each(|x| trim_raw(x, depth + 1));
            }
        }
        Value::Object(m) if depth < 16 => m.values_mut().for_each(|x| trim_raw(x, depth + 1)),
        _ => {}
    }
}

fn meta(raw: &Value) -> Option<&Value> {
    raw.get(META_KEY)
}

/// PLAN.md: only the vendors' VS Code extensions are `vscode_hook`; terminal CLIs, including
/// ones in VS Code's integrated terminal, are `cli_hook`. Order matters:
/// 1. Claude entrypoint `claude-vscode` / Codex originator `codex_vscode` -> extension.
/// 2. Process type `extensionHost` -> spawned by an extension (Copilot agent hooks).
/// 3. `TERM_PROGRAM=vscode` -> integrated terminal -> CLI.
/// 4. Other extension-host markers (stripped from terminal shells) -> extension.
fn detect_source(raw: &Value) -> SessionSource {
    let Some(m) = meta(raw) else { return SessionSource::CliHook };
    let has = |k: &str, needle: &str| m.get(k).and_then(Value::as_str).is_some_and(|v| v.to_ascii_lowercase().contains(needle));
    if has("claude_entrypoint", "vscode") || has("codex_originator", "vscode") || has("vscode_process_type", "extensionhost") {
        return SessionSource::VscodeHook;
    }
    if has("term_program", "vscode") {
        return SessionSource::CliHook;
    }
    if m.get("vscode_ext_host").and_then(Value::as_bool) == Some(true) {
        SessionSource::VscodeHook
    } else {
        SessionSource::CliHook
    }
}

/// Short human summary of a tool call, e.g. `Bash: npm test`, `Edit: src/main.rs`.
fn tool_summary(tool: Option<&str>, input: Option<&Value>) -> Option<String> {
    let tool = tool?;
    let parsed;
    let input = match input {
        // Copilot camelCase `toolArgs` may be a JSON string.
        Some(Value::String(st)) => {
            parsed = serde_json::from_str::<Value>(st).unwrap_or(Value::String(st.clone()));
            Some(&parsed)
        }
        other => other,
    };
    let detail = input.and_then(|i| match i {
        Value::String(st) => Some(st.as_str()),
        Value::Object(_) => ["command", "file_path", "filePath", "path", "notebook_path", "url", "query", "pattern", "description", "prompt"]
            .iter()
            .find_map(|k| i.get(*k).and_then(Value::as_str)),
        _ => None,
    });
    Some(match detail.map(str::trim).filter(|d| !d.is_empty()) {
        Some(d) => clip(&format!("{tool}: {d}"), MAX_MESSAGE),
        None => tool.to_string(),
    })
}

struct Draft {
    agent_id: &'static str,
    agent_name: &'static str,
    source: HookSource,
    hook_event: String,
    session_id: Option<String>,
    cwd: Option<String>,
    title: Option<String>,
    status: Option<SessionStatus>,
    ended: bool,
    event: EventType,
    message: Option<String>,
    notification: Option<(NotificationLevel, String, String)>,
}

impl Draft {
    fn new(agent_id: &'static str, agent_name: &'static str, source: HookSource, hook_event: String, raw: &Value) -> Self {
        Draft {
            agent_id,
            agent_name,
            source,
            hook_event,
            session_id: s(raw, &["session_id", "sessionId", "thread-id", "thread_id", "threadId", "conversation_id"]).map(str::to_string),
            cwd: s(raw, &["cwd", "workspace_root", "workspaceRoot"]).map(str::to_string),
            title: s(raw, &["session_title", "sessionTitle"]).map(|t| clip(t, MAX_TITLE)),
            status: None,
            ended: false,
            event: EventType::Hook,
            message: None,
            notification: None,
        }
    }

    fn notify(&mut self, level: NotificationLevel, title: impl Into<String>, body: impl AsRef<str>) {
        self.notification = Some((level, clip(&title.into(), MAX_TITLE), clip(body.as_ref(), MAX_BODY)));
    }

    fn finish(self, mut raw: Value) -> NormalizedHook {
        let source = detect_source(&raw);
        trim_raw(&mut raw, 0);
        let external_session_id = match self.session_id {
            Some(id) => id,
            None => format!("{}:{}", self.agent_id, self.cwd.as_deref().unwrap_or("unknown")),
        };
        NormalizedHook {
            external_session_id,
            agent_id: self.agent_id.to_string(),
            source,
            cwd: self.cwd,
            title: self.title,
            status: self.status,
            ended: self.ended,
            event: self.event,
            payload: HookPayload {
                source: self.source,
                hook_event: self.hook_event,
                message: self.message.map(|m| clip(&m, MAX_MESSAGE)),
                raw,
            },
            notification: self.notification,
        }
    }
}

/// Canonical (Claude-style PascalCase) event names, shared by all three agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ev {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    PostToolUseFailure,
    Notification,
    Stop,
    StopFailure,
    SubagentStart,
    SubagentStop,
    SessionEnd,
    Interrupt,
    Error,
    Other,
}

fn canonical(name: &str) -> Ev {
    let n: String = name.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase();
    match n.as_str() {
        "sessionstart" => Ev::SessionStart,
        "userpromptsubmit" | "userpromptsubmitted" => Ev::UserPromptSubmit,
        "pretooluse" => Ev::PreToolUse,
        "permissionrequest" => Ev::PermissionRequest,
        "posttooluse" => Ev::PostToolUse,
        "posttoolusefailure" => Ev::PostToolUseFailure,
        "notification" => Ev::Notification,
        "stop" | "agentstop" => Ev::Stop,
        "stopfailure" => Ev::StopFailure,
        "subagentstart" => Ev::SubagentStart,
        "subagentstop" => Ev::SubagentStop,
        "sessionend" => Ev::SessionEnd,
        "interrupt" => Ev::Interrupt,
        "erroroccurred" => Ev::Error,
        _ => Ev::Other,
    }
}

/// Shared lifecycle mapping for Claude-compatible hook events (Claude, Codex hooks, Copilot).
fn apply_lifecycle(d: &mut Draft, ev: Ev, raw: &Value) {
    let name = d.agent_name;
    let tool = s(raw, &["tool_name", "toolName"]);
    let tool_input = raw.get("tool_input").or_else(|| raw.get("toolArgs")).or_else(|| raw.get("toolInput"));
    match ev {
        Ev::SessionStart => {
            d.event = EventType::SessionStart;
            d.status = Some(SessionStatus::Idle);
            d.message = s(raw, &["source"]).map(|src| format!("session {src}"));
            if let Some(p) = s(raw, &["initial_prompt", "initialPrompt"]) {
                d.status = Some(SessionStatus::Thinking);
                d.title.get_or_insert_with(|| clip(p, MAX_TITLE));
            }
        }
        Ev::UserPromptSubmit => {
            d.status = Some(SessionStatus::Thinking);
            if let Some(p) = s(raw, &["prompt"]) {
                d.message = Some(p.to_string());
                d.title.get_or_insert_with(|| clip(p, MAX_TITLE));
            }
        }
        Ev::PreToolUse => {
            d.status = Some(SessionStatus::Working);
            d.message = tool_summary(tool, tool_input);
        }
        Ev::PermissionRequest => {
            d.status = Some(SessionStatus::WaitingPermission);
            let what = tool_summary(tool, tool_input)
                .or_else(|| tool_input.and_then(|i| i.get("description")).and_then(Value::as_str).map(str::to_string))
                .unwrap_or_else(|| "a tool call".to_string());
            d.message = Some(what.clone());
            d.notify(NotificationLevel::Alert, format!("{name} needs permission"), what);
        }
        Ev::PostToolUse => {
            d.status = Some(SessionStatus::Working);
            d.message = tool_summary(tool, tool_input);
        }
        Ev::PostToolUseFailure => {
            d.status = Some(SessionStatus::Working);
            let err = s(raw, &["error"]).map(|e| e.lines().next().unwrap_or(e).to_string());
            d.message = match (tool_summary(tool, tool_input), err) {
                (Some(t), Some(e)) => Some(format!("{t} failed: {e}")),
                (Some(t), None) => Some(format!("{t} failed")),
                (None, e) => e,
            };
        }
        Ev::Notification => {
            let ntype = s(raw, &["notification_type", "notificationType"]).unwrap_or("");
            let msg = s(raw, &["message"]).unwrap_or("").to_string();
            let title = s(raw, &["title"]);
            d.message = Some(if ntype.is_empty() { msg.clone() } else if msg.is_empty() { ntype.to_string() } else { format!("{ntype}: {msg}") });
            match ntype {
                "permission_prompt" => {
                    d.status = Some(SessionStatus::WaitingPermission);
                    d.notify(NotificationLevel::Alert, format!("{name} needs permission"), if msg.is_empty() { "Approval needed" } else { &msg });
                }
                "elicitation_dialog" | "elicitation_url_dialog" | "agent_needs_input" => {
                    d.status = Some(SessionStatus::WaitingPermission);
                    d.notify(NotificationLevel::Alert, format!("{name} needs your input"), if msg.is_empty() { "Input needed" } else { &msg });
                }
                "idle_prompt" | "agent_idle" => {
                    d.notify(NotificationLevel::Info, format!("{name} is waiting for you"), if msg.is_empty() { "Waiting for your next prompt" } else { &msg });
                }
                "auth_success" | "elicitation_complete" | "elicitation_response" | "shell_completed" | "shell_detached_completed" => {}
                "auth_storage_failure" | "quota_auto_resume_disabled" => {
                    d.notify(NotificationLevel::Warning, title.unwrap_or(name).to_string(), &msg);
                }
                _ if !msg.is_empty() => d.notify(NotificationLevel::Info, title.unwrap_or(name).to_string(), &msg),
                _ => {}
            }
        }
        Ev::Stop => {
            d.status = Some(SessionStatus::Done);
            let last = s(raw, &["last_assistant_message", "lastAssistantMessage"]);
            d.message = last.map(str::to_string).or_else(|| s(raw, &["stop_reason", "stopReason"]).map(str::to_string));
            d.notify(NotificationLevel::Success, format!("{name} finished"), last.unwrap_or("Turn complete"));
        }
        Ev::StopFailure => {
            d.status = Some(SessionStatus::Error);
            let err = s(raw, &["error"]).unwrap_or("unknown");
            let detail = s(raw, &["error_details", "last_assistant_message"]);
            let body = match detail {
                Some(x) => format!("{err}: {x}"),
                None => err.to_string(),
            };
            d.message = Some(body.clone());
            d.notify(NotificationLevel::Alert, format!("{name} stopped with an error"), body);
        }
        Ev::SubagentStart | Ev::SubagentStop => {
            d.status = Some(SessionStatus::Working);
            d.message = s(raw, &["agent_type", "agentType", "agentName", "agent_name"]).map(str::to_string);
        }
        Ev::SessionEnd => {
            d.event = EventType::SessionEnd;
            d.status = Some(SessionStatus::Done);
            d.ended = true;
            d.message = s(raw, &["reason"]).map(str::to_string);
        }
        Ev::Interrupt => {
            d.status = Some(SessionStatus::Idle);
            d.message = Some("turn interrupted".to_string());
        }
        Ev::Error => {
            let err = raw.get("error");
            let msg = err
                .and_then(|e| e.get("message").and_then(Value::as_str).or_else(|| e.as_str()))
                .unwrap_or("error")
                .to_string();
            d.message = Some(msg.clone());
            let recoverable = b(raw, &["recoverable"]).unwrap_or(false);
            if !recoverable {
                d.status = Some(SessionStatus::Error);
                d.notify(NotificationLevel::Alert, format!("{name} hit an error"), msg);
            }
        }
        Ev::Other => {
            d.message = s(raw, &["message", "reason", "trigger", "source"]).map(str::to_string);
        }
    }
}

// ---------- Claude Code ----------

pub(crate) fn claude(raw: Value) -> Option<NormalizedHook> {
    raw.as_object()?;
    let name = s(&raw, &["hook_event_name", "hookEventName"])?.to_string();
    let mut d = Draft::new("claude-code", "Claude Code", HookSource::Claude, name.clone(), &raw);
    apply_lifecycle(&mut d, canonical(&name), &raw);
    Some(d.finish(raw))
}

// ---------- Codex ----------

pub(crate) fn codex(raw: Value) -> Option<NormalizedHook> {
    raw.as_object()?;
    if let Some(name) = s(&raw, &["hook_event_name", "hookEventName"]).map(str::to_string) {
        // Codex lifecycle hooks (Claude-compatible schema).
        let mut d = Draft::new("codex", "Codex", HookSource::Codex, name.clone(), &raw);
        apply_lifecycle(&mut d, canonical(&name), &raw);
        return Some(d.finish(raw));
    }
    // `notify` program payload.
    let kind = s(&raw, &["type"])?.to_string();
    let mut d = Draft::new("codex", "Codex", HookSource::Codex, kind.clone(), &raw);
    let inputs: Vec<&str> = match raw.get("input-messages").or_else(|| raw.get("input_messages")) {
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect(),
        Some(Value::String(one)) => vec![one.as_str()],
        _ => Vec::new(),
    };
    if let Some(last_input) = inputs.iter().rev().map(|x| x.trim()).find(|x| !x.is_empty()) {
        d.title = Some(clip(last_input, MAX_TITLE));
    }
    let last = s(&raw, &["last-assistant-message", "last_assistant_message"]);
    match kind.as_str() {
        "agent-turn-complete" => {
            d.status = Some(SessionStatus::Done);
            d.message = last.map(str::to_string);
            d.notify(NotificationLevel::Success, "Codex finished", last.unwrap_or("Turn complete"));
        }
        k if k.contains("approval") || k.contains("permission") => {
            d.status = Some(SessionStatus::WaitingPermission);
            let what = s(&raw, &["message", "command", "reason"]).unwrap_or("Approval needed").to_string();
            d.message = Some(what.clone());
            d.notify(NotificationLevel::Alert, "Codex needs permission", what);
        }
        _ => {
            d.message = last.or_else(|| s(&raw, &["message"])).map(str::to_string);
        }
    }
    Some(d.finish(raw))
}

// ---------- Copilot ----------

/// Event name for a Copilot payload: explicit field, our env marker, else inferred from shape
/// (camelCase payloads carry no event name).
fn copilot_event(raw: &Value) -> Option<String> {
    if let Some(n) = s(raw, &["hook_event_name", "hookEventName", "hookEvent"]) {
        return Some(n.to_string());
    }
    if let Some(n) = meta(raw).and_then(|m| m.get("hook_event")).and_then(Value::as_str).filter(|v| !v.is_empty()) {
        return Some(n.to_string());
    }
    let has = |k: &str| raw.get(k).is_some();
    let inferred = if has("toolResult") || has("tool_response") {
        "postToolUse"
    } else if (has("toolName") || has("tool_name")) && has("error") {
        "postToolUseFailure"
    } else if has("toolName") || has("tool_name") {
        "preToolUse"
    } else if has("notification_type") || has("notificationType") {
        "notification"
    } else if has("stopReason") || has("stop_reason") || has("stop_hook_active") {
        "agentStop"
    } else if has("errorContext") || has("error_context") || raw.get("error").is_some_and(Value::is_object) {
        "errorOccurred"
    } else if has("prompt") {
        "userPromptSubmitted"
    } else if has("reason") {
        "sessionEnd"
    } else if has("source") || has("initialPrompt") || has("initial_prompt") {
        "sessionStart"
    } else {
        return None;
    };
    Some(inferred.to_string())
}

pub(crate) fn copilot(raw: Value) -> Option<NormalizedHook> {
    raw.as_object()?;
    let name = copilot_event(&raw)?;
    let mut d = Draft::new("copilot", "Copilot", HookSource::Copilot, name.clone(), &raw);
    apply_lifecycle(&mut d, canonical(&name), &raw);
    Some(d.finish(raw))
}
