//! Deterministic session facts, summaries, event digests and the daily report.
//! These are the no-model fallback and the fact sheet handed to the LLM.

use std::collections::{BTreeSet, HashMap};

use pet_protocol::{
    Agent, EventType, HookPayload, HubEvent, PermissionOutcome, PermissionRequest, PermissionResolvedPayload,
    PlanEntryStatus, PlanPayload, Session, SessionStatus, StopPayload, StopReason, ToolCallPayload, ToolCallStatus,
    ToolKind, UnixMs,
};
use serde_json::Value;

use super::voice::Voice;
use crate::util::{agent_name, ago, basename, one_line, plural, truncate};

const DAY_MS: i64 = 86_400_000;
const MAX_BUF: usize = 4_000;

#[derive(Debug, Default, Clone)]
pub(crate) struct SessionFacts {
    pub tool_calls: usize,
    pub last_tool: Option<String>,
    pub last_message: Option<String>,
    pub last_thought: Option<String>,
    pub last_user: Option<String>,
    /// (completed, total, next entry)
    pub plan: Option<(usize, usize, Option<String>)>,
    pub last_error: Option<String>,
    pub last_stop: Option<StopReason>,
    pub permission_requests: usize,
    pub permission_decisions: usize,
    pub errors: usize,
    pub last_hook: Option<String>,
}

/// Events of one session, oldest first.
pub(crate) fn session_events<'a>(session_id: &str, events: &'a [HubEvent]) -> Vec<&'a HubEvent> {
    let mut v: Vec<&HubEvent> = events.iter().filter(|e| e.session_id == session_id).collect();
    v.sort_by_key(|e| (e.ts, e.id));
    v
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn text_of(p: &Value) -> String {
    p.get("text").and_then(Value::as_str).unwrap_or_default().to_string()
}

/// The tool call inside a `request_permission` payload (a PermissionRequest or a bare tool call).
fn tool_value(p: &Value) -> &Value {
    p.get("toolCall").unwrap_or(p)
}

fn tool_status(p: &Value) -> Option<ToolCallStatus> {
    p.get("status").and_then(|s| serde_json::from_value(s.clone()).ok())
}

fn push_capped(buf: &mut String, text: &str) {
    buf.push_str(text);
    if buf.len() > MAX_BUF {
        let mut cut = buf.len() - MAX_BUF;
        while !buf.is_char_boundary(cut) {
            cut += 1;
        }
        buf.drain(..cut);
    }
}

fn kind_label(kind: Option<ToolKind>) -> &'static str {
    match kind {
        Some(ToolKind::Read) => "read a file",
        Some(ToolKind::Edit) => "edit a file",
        Some(ToolKind::Delete) => "delete a file",
        Some(ToolKind::Move) => "move a file",
        Some(ToolKind::Search) => "search",
        Some(ToolKind::Execute) => "run a command",
        Some(ToolKind::Think) => "think",
        Some(ToolKind::Fetch) => "fetch from the network",
        Some(ToolKind::SwitchMode) => "switch mode",
        Some(ToolKind::Other) | None => "use a tool",
    }
}

/// Human label for a tool call: its title, else "<kind> <file>".
pub(crate) fn tool_label(tc: &ToolCallPayload) -> String {
    if let Some(t) = tc.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        return truncate(&one_line(t), 90);
    }
    let base = kind_label(tc.kind);
    match tc.locations.as_ref().and_then(|l| l.first()) {
        Some(loc) => format!("{base} ({})", basename(&loc.path)),
        None => base.to_string(),
    }
}

pub(crate) fn facts(events: &[&HubEvent]) -> SessionFacts {
    let mut f = SessionFacts::default();
    let mut titles: HashMap<String, String> = HashMap::new();
    let mut prev: Option<EventType> = None;
    let mut msg = String::new();
    let mut thought = String::new();
    for e in events {
        let p = &e.payload;
        match e.kind {
            EventType::AgentMessageChunk => {
                if prev != Some(EventType::AgentMessageChunk) {
                    msg.clear();
                }
                push_capped(&mut msg, &text_of(p));
            }
            EventType::AgentThoughtChunk => {
                if prev != Some(EventType::AgentThoughtChunk) {
                    thought.clear();
                }
                push_capped(&mut thought, &text_of(p));
            }
            EventType::UserMessage => {
                f.last_user = str_field(p, "text");
            }
            EventType::ToolCall => {
                f.tool_calls += 1;
                let title = str_field(p, "title");
                if let (Some(id), Some(t)) = (str_field(p, "toolCallId"), title.clone()) {
                    titles.insert(id, t);
                }
                if title.is_some() {
                    f.last_tool = title;
                }
            }
            EventType::ToolCallUpdate => {
                let title = str_field(p, "title")
                    .or_else(|| str_field(p, "toolCallId").and_then(|id| titles.get(&id).cloned()));
                if let (Some(id), Some(t)) = (str_field(p, "toolCallId"), str_field(p, "title")) {
                    titles.insert(id, t);
                }
                if title.is_some() {
                    f.last_tool = title;
                }
                if tool_status(p) == Some(ToolCallStatus::Failed) {
                    f.errors += 1;
                }
            }
            EventType::Plan => {
                if let Ok(plan) = serde_json::from_value::<PlanPayload>(p.clone()) {
                    let total = plan.entries.len();
                    let done = plan.entries.iter().filter(|x| x.status == PlanEntryStatus::Completed).count();
                    let next = plan
                        .entries
                        .iter()
                        .find(|x| x.status == PlanEntryStatus::InProgress)
                        .or_else(|| plan.entries.iter().find(|x| x.status == PlanEntryStatus::Pending))
                        .map(|x| truncate(&one_line(&x.content), 70));
                    f.plan = Some((done, total, next));
                }
            }
            EventType::RequestPermission => {
                f.permission_requests += 1;
                if let Some(t) = str_field(tool_value(p), "title") {
                    f.last_tool = Some(t);
                }
            }
            EventType::PermissionResolved => f.permission_decisions += 1,
            EventType::Stop => {
                if let Ok(s) = serde_json::from_value::<StopPayload>(p.clone()) {
                    f.last_stop = Some(s.stop_reason);
                }
            }
            EventType::Error => {
                f.errors += 1;
                f.last_error = str_field(p, "message").or_else(|| Some("unknown error".into()));
            }
            EventType::Hook => {
                if let Ok(h) = serde_json::from_value::<HookPayload>(p.clone()) {
                    f.last_hook = Some(match h.message.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
                        Some(m) => format!("{}: {}", h.hook_event, truncate(&one_line(m), 100)),
                        None => h.hook_event,
                    });
                }
            }
            EventType::SessionStart | EventType::SessionEnd | EventType::Notification => {}
        }
        prev = Some(e.kind);
    }
    if !msg.trim().is_empty() {
        f.last_message = Some(msg.trim().to_string());
    }
    if !thought.trim().is_empty() {
        f.last_thought = Some(thought.trim().to_string());
    }
    f
}

fn pending_title(session: &Session, pending: &[PermissionRequest]) -> Option<String> {
    pending
        .iter()
        .filter(|r| r.session_id == session.id)
        .min_by_key(|r| r.created_at)
        .map(|r| tool_label(&r.tool_call))
}

/// One sentence-ish status for overview lines: "working: Run cargo test".
pub(crate) fn short_status(session: &Session, facts: &SessionFacts, pending: &[PermissionRequest], now: UnixMs) -> String {
    if let Some(end) = session.ended_at {
        return format!("ended {}", ago(end, now));
    }
    match session.status {
        SessionStatus::Idle => "idle".into(),
        SessionStatus::Thinking => "thinking".into(),
        SessionStatus::Working => match &facts.last_tool {
            Some(t) => format!("working: {}", truncate(t, 50)),
            None => "working".into(),
        },
        SessionStatus::WaitingPermission => match pending_title(session, pending).or_else(|| facts.last_tool.clone()) {
            Some(t) => format!("waiting for your OK: {}", truncate(&t, 50)),
            None => "waiting for your OK".into(),
        },
        SessionStatus::Done => format!("done {}", ago(session.last_event_at, now)),
        SessionStatus::Error => match &facts.last_error {
            Some(e) => format!("error: {}", truncate(e, 60)),
            None => "error".into(),
        },
    }
}

/// Deterministic summary of one session (no voice decoration).
pub(crate) fn deterministic_summary(
    session: &Session,
    facts: &SessionFacts,
    agents: &[Agent],
    pending: &[PermissionRequest],
    now: UnixMs,
) -> String {
    let agent = agent_name(&session.agent_id, agents);
    let mut out = format!("{agent} in {}", basename(&session.cwd));
    if let Some(t) = session.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        out.push_str(&format!(" (“{}”)", truncate(&one_line(t), 48)));
    }
    let phrase = if let Some(end) = session.ended_at {
        format!("finished; the session ended {}", ago(end, now))
    } else {
        match session.status {
            SessionStatus::Idle => "idle and ready for a task".to_string(),
            SessionStatus::Thinking => "thinking it over".to_string(),
            SessionStatus::Working => match &facts.last_tool {
                Some(t) => format!("working on: {}", truncate(t, 70)),
                None => "working".to_string(),
            },
            SessionStatus::WaitingPermission => {
                match pending_title(session, pending).or_else(|| facts.last_tool.clone()) {
                    Some(t) => format!("waiting for your OK to: {}", truncate(&t, 70)),
                    None => "waiting for your OK".to_string(),
                }
            }
            SessionStatus::Done => {
                let how = match facts.last_stop {
                    Some(StopReason::Refusal) => " (it refused)",
                    Some(StopReason::MaxTokens) => " (ran out of tokens)",
                    Some(StopReason::MaxTurnRequests) => " (hit its turn limit)",
                    Some(StopReason::Cancelled) => " (cancelled)",
                    _ => "",
                };
                format!("done with its turn{how}, {}", ago(session.last_event_at, now))
            }
            SessionStatus::Error => match &facts.last_error {
                Some(e) => format!("stuck on an error: {}", truncate(&one_line(e), 80)),
                None => "stuck on an error".to_string(),
            },
        }
    };
    out.push_str(" is ");
    out.push_str(&phrase);
    out.push('.');
    if let Some(m) = &facts.last_message {
        out.push_str(&format!(" Last said: “{}”", truncate(&one_line(m), 110)));
    } else if session.status == SessionStatus::Thinking {
        if let Some(t) = &facts.last_thought {
            out.push_str(&format!(" Thinking: “{}”", truncate(&one_line(t), 90)));
        }
    } else if let Some(h) = &facts.last_hook {
        out.push_str(&format!(" Last hook: {h}."));
    }
    if let Some((done, total, next)) = &facts.plan {
        if *total > 0 {
            out.push_str(&format!(" Plan: {done}/{total} done"));
            if let Some(n) = next.as_ref().filter(|_| done < total) {
                out.push_str(&format!(", next: {n}"));
            }
            out.push('.');
        }
    }
    if facts.tool_calls > 0 && facts.last_message.is_none() {
        out.push_str(&format!(" {} so far.", plural(facts.tool_calls, "tool call")));
    }
    out
}

/// Compact oldest-first event lines for the LLM (chunk runs merged, text truncated).
pub(crate) fn event_digest(events: &[&HubEvent], max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut titles: HashMap<String, String> = HashMap::new();
    let mut prev: Option<EventType> = None;
    let mut run = String::new();
    let flush = |lines: &mut Vec<String>, prev: Option<EventType>, run: &mut String| {
        if run.trim().is_empty() {
            run.clear();
            return;
        }
        let who = if prev == Some(EventType::AgentThoughtChunk) { "agent thinks" } else { "agent says" };
        lines.push(format!("{who}: {}", truncate(&one_line(run), 220)));
        run.clear();
    };
    for e in events {
        let p = &e.payload;
        let chunk = matches!(e.kind, EventType::AgentMessageChunk | EventType::AgentThoughtChunk);
        if prev != Some(e.kind) && matches!(prev, Some(EventType::AgentMessageChunk | EventType::AgentThoughtChunk)) {
            flush(&mut lines, prev, &mut run);
        }
        if chunk {
            push_capped(&mut run, &text_of(p));
            prev = Some(e.kind);
            continue;
        }
        let line = match e.kind {
            EventType::SessionStart => Some("session started".to_string()),
            EventType::SessionEnd => Some("session ended".to_string()),
            EventType::UserMessage => Some(format!("user: {}", truncate(&one_line(&text_of(p)), 200))),
            EventType::ToolCall => {
                let title = str_field(p, "title").unwrap_or_else(|| "tool".into());
                if let Some(id) = str_field(p, "toolCallId") {
                    titles.insert(id, title.clone());
                }
                let kind = str_field(p, "kind").map(|k| format!(" [{k}]")).unwrap_or_default();
                Some(format!("tool call{kind}: {}", truncate(&one_line(&title), 120)))
            }
            EventType::ToolCallUpdate => {
                let status = tool_status(p);
                if matches!(status, Some(ToolCallStatus::Completed | ToolCallStatus::Failed)) {
                    let title = str_field(p, "title")
                        .or_else(|| str_field(p, "toolCallId").and_then(|id| titles.get(&id).cloned()))
                        .unwrap_or_else(|| "tool".into());
                    let s = if status == Some(ToolCallStatus::Failed) { "failed" } else { "completed" };
                    Some(format!("tool {s}: {}", truncate(&one_line(&title), 120)))
                } else {
                    None
                }
            }
            EventType::Plan => serde_json::from_value::<PlanPayload>(p.clone()).ok().map(|plan| {
                let done = plan.entries.iter().filter(|x| x.status == PlanEntryStatus::Completed).count();
                let items: Vec<String> = plan.entries.iter().take(6).map(|x| truncate(&one_line(&x.content), 50)).collect();
                format!("plan {done}/{} done: {}", plan.entries.len(), items.join("; "))
            }),
            EventType::RequestPermission => {
                let title = str_field(tool_value(p), "title").unwrap_or_else(|| "a tool call".into());
                Some(format!("asked the user for permission: {}", truncate(&one_line(&title), 120)))
            }
            EventType::PermissionResolved => {
                serde_json::from_value::<PermissionResolvedPayload>(p.clone()).ok().map(|r| {
                    let outcome = match r.outcome {
                        PermissionOutcome::Selected { option_id } => format!("option {option_id}"),
                        PermissionOutcome::Cancelled => "cancelled".into(),
                    };
                    format!("permission resolved ({outcome}) by {:?}", r.by).to_lowercase()
                })
            }
            EventType::Stop => serde_json::from_value::<StopPayload>(p.clone())
                .ok()
                .map(|s| format!("turn ended: {}", serde_json::to_value(s.stop_reason).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default())),
            EventType::Error => Some(format!("error: {}", truncate(&one_line(&str_field(p, "message").unwrap_or_default()), 160))),
            EventType::Hook => serde_json::from_value::<HookPayload>(p.clone()).ok().map(|h| match h.message {
                Some(m) if !m.trim().is_empty() => format!("hook {}: {}", h.hook_event, truncate(&one_line(&m), 160)),
                _ => format!("hook {}", h.hook_event),
            }),
            EventType::Notification => {
                let t = str_field(p, "title").or_else(|| str_field(p, "body")).or_else(|| str_field(p, "text"));
                t.map(|t| format!("notification: {}", truncate(&one_line(&t), 120)))
            }
            EventType::AgentMessageChunk | EventType::AgentThoughtChunk => None,
        };
        if let Some(l) = line {
            lines.push(l);
        }
        prev = Some(e.kind);
    }
    if matches!(prev, Some(EventType::AgentMessageChunk | EventType::AgentThoughtChunk)) {
        flush(&mut lines, prev, &mut run);
    }
    if lines.len() > max_lines {
        lines.drain(..lines.len() - max_lines);
    }
    lines
}

fn report_state(s: &Session, now: UnixMs) -> String {
    if s.ended_at.is_some() {
        return "done (ended)".into();
    }
    match s.status {
        SessionStatus::Idle => "idle".into(),
        SessionStatus::Thinking => "thinking".into(),
        SessionStatus::Working => "working".into(),
        SessionStatus::WaitingPermission => "waiting for you".into(),
        SessionStatus::Done => format!("done {}", ago(s.last_event_at, now)),
        SessionStatus::Error => "error".into(),
    }
}

/// Deterministic end-of-day report for sessions active in the last 24 h.
/// `voice` adds the pet's sound to the header.
pub(crate) fn daily_report_text(
    sessions: &[Session],
    events: &[HubEvent],
    agents: &[Agent],
    voice: Option<(&Voice, u64)>,
    now: UnixMs,
) -> String {
    let since = now - DAY_MS;
    let mut todays: Vec<&Session> = sessions
        .iter()
        .filter(|s| s.last_event_at >= since || s.started_at >= since || s.ended_at.is_some_and(|e| e >= since))
        .collect();
    todays.sort_by_key(|s| (s.started_at, s.id.clone()));
    let sound = voice.map(|(v, seed)| format!("{} ", v.sound(seed))).unwrap_or_default();
    if todays.is_empty() {
        let nap = voice.map(|(v, seed)| format!(" {}", v.sleepy(seed))).unwrap_or_default();
        return format!("{sound}Quiet day: no agent sessions in the last 24 h.{nap}");
    }
    let ids: BTreeSet<&str> = todays.iter().map(|s| s.id.as_str()).collect();
    let mut per: HashMap<&str, Vec<&HubEvent>> = HashMap::new();
    for e in events.iter().filter(|e| e.ts >= since && ids.contains(e.session_id.as_str())) {
        per.entry(e.session_id.as_str()).or_default().push(e);
    }
    for v in per.values_mut() {
        v.sort_by_key(|e| (e.ts, e.id));
    }
    let empty: Vec<&HubEvent> = Vec::new();
    let all_facts: Vec<SessionFacts> = todays.iter().map(|s| facts(per.get(s.id.as_str()).unwrap_or(&empty))).collect();
    let agent_set: BTreeSet<&str> = todays.iter().map(|s| s.agent_id.as_str()).collect();
    let repo_set: BTreeSet<String> = todays.iter().map(|s| basename(&s.cwd).to_lowercase()).collect();
    let tools: usize = all_facts.iter().map(|f| f.tool_calls).sum();
    let asks: usize = all_facts.iter().map(|f| f.permission_requests).sum();
    let errors: usize = all_facts.iter().map(|f| f.errors).sum::<usize>()
        + todays.iter().zip(&all_facts).filter(|(s, f)| s.status == SessionStatus::Error && f.errors == 0).count();

    let mut out = format!(
        "{sound}Daily report: {} across {} in {} ({}, {}, {}).",
        plural(todays.len(), "session"),
        plural(agent_set.len(), "agent"),
        plural(repo_set.len(), "repo"),
        plural(tools, "tool call"),
        plural(asks, "approval request"),
        plural(errors, "error"),
    );
    const MAX_LINES: usize = 10;
    for (s, f) in todays.iter().zip(&all_facts).take(MAX_LINES) {
        let mut line = format!("\n• {} · {}: {}", agent_name(&s.agent_id, agents), basename(&s.cwd), report_state(s, now));
        if f.tool_calls > 0 {
            line.push_str(&format!(", {}", plural(f.tool_calls, "tool")));
        }
        if let Some(t) = s.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            line.push_str(&format!(" · “{}”", truncate(&one_line(t), 40)));
        }
        out.push_str(&line);
    }
    if todays.len() > MAX_LINES {
        out.push_str(&format!("\n• …and {} more", todays.len() - MAX_LINES));
    }
    let needs: Vec<String> = todays
        .iter()
        .filter(|s| s.ended_at.is_none() && matches!(s.status, SessionStatus::WaitingPermission | SessionStatus::Error))
        .map(|s| {
            let why = if s.status == SessionStatus::Error { "error" } else { "waiting for approval" };
            format!("{} in {} ({why})", agent_name(&s.agent_id, agents), basename(&s.cwd))
        })
        .collect();
    if !needs.is_empty() {
        out.push_str(&format!("\nNeeds you: {}.", needs.join(", ")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pet_protocol::SessionSource;
    use serde_json::json;

    fn ev(id: i64, kind: EventType, payload: Value) -> HubEvent {
        HubEvent { id, session_id: "s1".into(), kind, payload, ts: id }
    }

    fn sess(status: SessionStatus) -> Session {
        Session {
            id: "s1".into(),
            agent_id: "codex".into(),
            acp_session_id: None,
            external_id: None,
            cwd: "E:\\work\\ai-pet".into(),
            title: Some("Fix tests".into()),
            source: SessionSource::Owned,
            status,
            started_at: 0,
            last_event_at: 0,
            ended_at: None,
        }
    }

    #[test]
    fn facts_and_summary_from_events() {
        let events = vec![
            ev(1, EventType::UserMessage, json!({"text": "run the tests"})),
            ev(2, EventType::AgentMessageChunk, json!({"text": "Running "})),
            ev(3, EventType::AgentMessageChunk, json!({"text": "the tests now."})),
            ev(4, EventType::ToolCall, json!({"toolCallId": "t1", "title": "Run cargo test", "kind": "execute", "status": "in_progress"})),
            ev(5, EventType::Plan, json!({"entries": [
                {"content": "run tests", "priority": "high", "status": "completed"},
                {"content": "fix failures", "priority": "high", "status": "in_progress"}
            ]})),
        ];
        let refs = session_events("s1", &events);
        let f = facts(&refs);
        assert_eq!(f.tool_calls, 1);
        assert_eq!(f.last_tool.as_deref(), Some("Run cargo test"));
        assert_eq!(f.last_message.as_deref(), Some("Running the tests now."));
        assert_eq!(f.plan, Some((1, 2, Some("fix failures".into()))));
        let s = deterministic_summary(&sess(SessionStatus::Working), &f, &[], &[], 10);
        assert!(s.starts_with("Codex in ai-pet (“Fix tests”) is working on: Run cargo test."), "{s}");
        assert!(s.contains("Plan: 1/2 done, next: fix failures."), "{s}");
        let digest = event_digest(&refs, 10);
        assert_eq!(digest[0], "user: run the tests");
        assert_eq!(digest[1], "agent says: Running the tests now.");
        assert!(digest[2].starts_with("tool call [execute]: Run cargo test"));
    }

    #[test]
    fn daily_report_counts() {
        let now = 10 * DAY_MS;
        let mut a = sess(SessionStatus::Done);
        a.last_event_at = now - 1000;
        let mut b = sess(SessionStatus::WaitingPermission);
        b.id = "s2".into();
        b.agent_id = "claude-code".into();
        b.cwd = "/w/web".into();
        b.last_event_at = now - 500;
        let mut old = sess(SessionStatus::Done);
        old.id = "s0".into();
        old.last_event_at = now - 3 * DAY_MS;
        let events = vec![
            HubEvent { id: 1, session_id: "s1".into(), kind: EventType::ToolCall, payload: json!({"toolCallId": "x"}), ts: now - 2000 },
            HubEvent { id: 2, session_id: "s2".into(), kind: EventType::RequestPermission, payload: json!({"toolCall": {"title": "rm"}}), ts: now - 600 },
        ];
        let r = daily_report_text(&[a, b, old], &events, &[], None, now);
        assert!(r.starts_with("Daily report: 2 sessions across 2 agents in 2 repos (1 tool call, 1 approval request, 0 errors)."), "{r}");
        assert!(r.contains("• Claude Code · web: waiting for you"), "{r}");
        assert!(r.contains("Needs you: Claude Code in web (waiting for approval)."), "{r}");
        assert!(daily_report_text(&[], &[], &[], None, now).starts_with("Quiet day"));
    }
}
