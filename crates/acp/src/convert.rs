//! Pure conversions from ACP v1 schema types to `pawl-protocol` wire payloads.

use agent_client_protocol::v1::schema as acp;
use pawl_protocol as pp;

/// Render one content block as plain text for the event log.
///
/// Non-text blocks become short placeholders so the log still shows that
/// something was sent; an empty text block yields `None`.
pub(crate) fn content_text(block: &acp::ContentBlock) -> Option<String> {
    let text = match block {
        acp::ContentBlock::Text(t) => t.text.clone(),
        acp::ContentBlock::Image(_) => "[image]".to_string(),
        acp::ContentBlock::Audio(_) => "[audio]".to_string(),
        acp::ContentBlock::ResourceLink(link) => {
            if link.name.is_empty() {
                link.uri.clone()
            } else {
                format!("[{}]({})", link.name, link.uri)
            }
        }
        acp::ContentBlock::Resource(_) => "[resource]".to_string(),
        _ => return None,
    };
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

pub(crate) fn tool_kind(kind: &acp::ToolKind) -> pp::ToolKind {
    match kind {
        acp::ToolKind::Read => pp::ToolKind::Read,
        acp::ToolKind::Edit => pp::ToolKind::Edit,
        acp::ToolKind::Delete => pp::ToolKind::Delete,
        acp::ToolKind::Move => pp::ToolKind::Move,
        acp::ToolKind::Search => pp::ToolKind::Search,
        acp::ToolKind::Execute => pp::ToolKind::Execute,
        acp::ToolKind::Think => pp::ToolKind::Think,
        acp::ToolKind::Fetch => pp::ToolKind::Fetch,
        acp::ToolKind::SwitchMode => pp::ToolKind::SwitchMode,
        _ => pp::ToolKind::Other,
    }
}

pub(crate) fn tool_status(status: &acp::ToolCallStatus) -> pp::ToolCallStatus {
    match status {
        acp::ToolCallStatus::Pending => pp::ToolCallStatus::Pending,
        acp::ToolCallStatus::InProgress => pp::ToolCallStatus::InProgress,
        acp::ToolCallStatus::Completed => pp::ToolCallStatus::Completed,
        acp::ToolCallStatus::Failed => pp::ToolCallStatus::Failed,
        // Unknown future statuses: treat as still running so the pet keeps working.
        _ => pp::ToolCallStatus::InProgress,
    }
}

pub(crate) fn locations(locs: &[acp::ToolCallLocation]) -> Vec<pp::ToolLocation> {
    locs.iter()
        .map(|l| pp::ToolLocation { path: l.path.to_string_lossy().into_owned(), line: l.line })
        .collect()
}

/// Full payload for a `tool_call` update.
pub(crate) fn tool_call_payload(tc: &acp::ToolCall) -> pp::ToolCallPayload {
    pp::ToolCallPayload {
        tool_call_id: tc.tool_call_id.to_string(),
        title: if tc.title.is_empty() { None } else { Some(tc.title.clone()) },
        kind: Some(tool_kind(&tc.kind)),
        status: Some(tool_status(&tc.status)),
        locations: if tc.locations.is_empty() { None } else { Some(locations(&tc.locations)) },
        raw_input: tc.raw_input.clone(),
    }
}

/// Partial payload for a `tool_call_update` (only the fields the agent sent).
pub(crate) fn tool_update_payload(u: &acp::ToolCallUpdate) -> pp::ToolCallPayload {
    let f = &u.fields;
    pp::ToolCallPayload {
        tool_call_id: u.tool_call_id.to_string(),
        title: f.title.clone().filter(|t| !t.is_empty()),
        kind: f.kind.as_ref().map(tool_kind),
        status: f.status.as_ref().map(tool_status),
        locations: f.locations.as_ref().map(|l| locations(l)),
        raw_input: f.raw_input.clone(),
    }
}

/// Overlay the fields present in `update` on top of `base`.
pub(crate) fn merge_tool(base: &pp::ToolCallPayload, update: &pp::ToolCallPayload) -> pp::ToolCallPayload {
    pp::ToolCallPayload {
        tool_call_id: update.tool_call_id.clone(),
        title: update.title.clone().or_else(|| base.title.clone()),
        kind: update.kind.or(base.kind),
        status: update.status.or(base.status),
        locations: update.locations.clone().or_else(|| base.locations.clone()),
        raw_input: update.raw_input.clone().or_else(|| base.raw_input.clone()),
    }
}

pub(crate) fn plan_payload(entries: &[acp::PlanEntry]) -> pp::PlanPayload {
    pp::PlanPayload {
        entries: entries
            .iter()
            .map(|e| pp::PlanEntry {
                content: e.content.clone(),
                priority: match e.priority {
                    acp::PlanEntryPriority::High => pp::PlanPriority::High,
                    acp::PlanEntryPriority::Low => pp::PlanPriority::Low,
                    _ => pp::PlanPriority::Medium,
                },
                status: match e.status {
                    acp::PlanEntryStatus::InProgress => pp::PlanEntryStatus::InProgress,
                    acp::PlanEntryStatus::Completed => pp::PlanEntryStatus::Completed,
                    _ => pp::PlanEntryStatus::Pending,
                },
            })
            .collect(),
    }
}

pub(crate) fn stop_reason(r: &acp::StopReason) -> pp::StopReason {
    match r {
        acp::StopReason::EndTurn => pp::StopReason::EndTurn,
        acp::StopReason::MaxTokens => pp::StopReason::MaxTokens,
        acp::StopReason::MaxTurnRequests => pp::StopReason::MaxTurnRequests,
        acp::StopReason::Refusal => pp::StopReason::Refusal,
        acp::StopReason::Cancelled => pp::StopReason::Cancelled,
        _ => pp::StopReason::EndTurn,
    }
}

/// Session status after a turn ends with `reason` (CONTRACTS.md mapping).
pub(crate) fn status_after_stop(reason: pp::StopReason) -> pp::SessionStatus {
    match reason {
        pp::StopReason::EndTurn | pp::StopReason::MaxTokens | pp::StopReason::MaxTurnRequests => {
            pp::SessionStatus::Done
        }
        pp::StopReason::Cancelled => pp::SessionStatus::Idle,
        pp::StopReason::Refusal => pp::SessionStatus::Error,
    }
}

pub(crate) fn option_kind(k: &acp::PermissionOptionKind) -> pp::PermissionOptionKind {
    match k {
        acp::PermissionOptionKind::AllowOnce => pp::PermissionOptionKind::AllowOnce,
        acp::PermissionOptionKind::AllowAlways => pp::PermissionOptionKind::AllowAlways,
        acp::PermissionOptionKind::RejectAlways => pp::PermissionOptionKind::RejectAlways,
        // RejectOnce and unknown future kinds: the conservative choice.
        _ => pp::PermissionOptionKind::RejectOnce,
    }
}

pub(crate) fn permission_option(o: &acp::PermissionOption) -> pp::PermissionOption {
    pp::PermissionOption { option_id: o.option_id.to_string(), name: o.name.clone(), kind: option_kind(&o.kind) }
}

pub(crate) fn agent_capabilities(c: &acp::AgentCapabilities) -> pp::AgentCapabilities {
    pp::AgentCapabilities {
        load_session: c.load_session,
        list_sessions: c.session_capabilities.list.is_some(),
        resume_session: c.session_capabilities.resume.is_some(),
        prompt_image: c.prompt_capabilities.image,
        prompt_audio: c.prompt_capabilities.audio,
    }
}

/// Most elements kept from one snapshot, and the longest description kept.
const MAX_ELEMENTS: usize = 5_000;
const MAX_ELEMENT_CHARS: usize = 300;

/// The page elements in a tool result: every text line `uid=<id> <description>` (leading indentation
/// allowed). Empty when the result lists none.
pub(crate) fn snapshot_elements(content: &[acp::ToolCallContent]) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for c in content {
        let acp::ToolCallContent::Content(c) = c else { continue };
        let acp::ContentBlock::Text(t) = &c.content else { continue };
        for line in t.text.lines() {
            let Some(rest) = line.trim_start().strip_prefix("uid=") else { continue };
            let (uid, desc) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if uid.is_empty() || uid.len() > 64 || out.len() >= MAX_ELEMENTS {
                continue;
            }
            out.insert(uid.to_string(), desc.trim().chars().take(MAX_ELEMENT_CHARS).collect());
        }
    }
    out
}

/// Parse an ISO 8601 / RFC 3339 timestamp into unix milliseconds.
pub(crate) fn parse_timestamp_ms(s: &str) -> Option<pp::UnixMs> {
    chrono::DateTime::parse_from_rfc3339(s.trim()).ok().map(|d| d.timestamp_millis())
}

pub(crate) fn history_session(machine_id: &str, agent_id: &str, info: &acp::SessionInfo) -> pp::HistorySession {
    pp::HistorySession {
        machine_id: machine_id.to_string(),
        agent_id: agent_id.to_string(),
        acp_session_id: info.session_id.to_string(),
        cwd: info.cwd.to_string_lossy().into_owned(),
        title: info.title.clone().filter(|t| !t.trim().is_empty()),
        updated_at: info.updated_at.as_deref().and_then(parse_timestamp_ms),
    }
}

/// A short single-line title derived from the first prompt.
pub(crate) fn title_from_prompt(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    const MAX: usize = 80;
    if line.chars().count() <= MAX {
        Some(line.to_string())
    } else {
        let mut t: String = line.chars().take(MAX - 1).collect();
        t.push('…');
        Some(t)
    }
}

pub(crate) fn to_json<T: serde::Serialize>(v: &T) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> T {
        serde_json::from_value(v).expect("valid ACP json")
    }

    #[test]
    fn maps_tool_call_from_wire_json() {
        let tc: acp::ToolCall = parse(json!({
            "toolCallId": "t1", "title": "Edit main.rs", "kind": "edit", "status": "in_progress",
            "locations": [{"path": "/r/src/main.rs", "line": 3}], "rawInput": {"a": 1}
        }));
        let p = tool_call_payload(&tc);
        assert_eq!(
            to_json(&p),
            json!({"toolCallId":"t1","title":"Edit main.rs","kind":"edit","status":"in_progress",
                   "locations":[{"path":"/r/src/main.rs","line":3}],"rawInput":{"a":1}})
        );
    }

    #[test]
    fn merges_partial_tool_update() {
        let tc: acp::ToolCall = parse(json!({"toolCallId": "t1", "title": "Run tests", "kind": "execute", "status": "pending"}));
        let base = tool_call_payload(&tc);
        let u: acp::ToolCallUpdate = parse(json!({"toolCallId": "t1", "status": "completed"}));
        let partial = tool_update_payload(&u);
        assert_eq!(partial.kind, None);
        let merged = merge_tool(&base, &partial);
        assert_eq!(merged.kind, Some(pp::ToolKind::Execute));
        assert_eq!(merged.status, Some(pp::ToolCallStatus::Completed));
        assert_eq!(merged.title.as_deref(), Some("Run tests"));
    }

    #[test]
    fn maps_session_updates_and_plan() {
        let n: acp::SessionNotification = parse(json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "plan", "entries": [
                {"content": "write code", "priority": "high", "status": "in_progress"},
                {"content": "test", "priority": "low", "status": "pending"}
            ]}
        }));
        let acp::SessionUpdate::Plan(plan) = &n.update else { panic!("expected plan") };
        assert_eq!(
            to_json(&plan_payload(&plan.entries)),
            json!({"entries":[{"content":"write code","priority":"high","status":"in_progress"},
                               {"content":"test","priority":"low","status":"pending"}]})
        );
        let n: acp::SessionNotification = parse(json!({
            "sessionId": "s1",
            "update": {"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "hmm"}}
        }));
        let acp::SessionUpdate::AgentThoughtChunk(chunk) = &n.update else { panic!("expected thought") };
        assert_eq!(content_text(&chunk.content).as_deref(), Some("hmm"));
    }

    #[test]
    fn maps_capabilities() {
        let c: acp::AgentCapabilities = parse(json!({
            "loadSession": true,
            "promptCapabilities": {"image": true},
            "sessionCapabilities": {"list": {}, "resume": {}}
        }));
        assert_eq!(
            agent_capabilities(&c),
            pp::AgentCapabilities { load_session: true, list_sessions: true, resume_session: true, prompt_image: true, prompt_audio: false }
        );
        let c: acp::AgentCapabilities = parse(json!({}));
        assert_eq!(agent_capabilities(&c), pp::AgentCapabilities::default());
    }

    #[test]
    fn stop_status_mapping() {
        assert_eq!(status_after_stop(pp::StopReason::EndTurn), pp::SessionStatus::Done);
        assert_eq!(status_after_stop(pp::StopReason::Refusal), pp::SessionStatus::Error);
        assert_eq!(status_after_stop(pp::StopReason::Cancelled), pp::SessionStatus::Idle);
    }

    #[test]
    fn timestamps_and_titles() {
        assert_eq!(parse_timestamp_ms("1970-01-01T00:00:01Z"), Some(1000));
        assert_eq!(parse_timestamp_ms("2026-01-01T00:00:00.500+00:00"), Some(1_767_225_600_500));
        assert_eq!(parse_timestamp_ms("yesterday"), None);
        assert_eq!(title_from_prompt("\n  fix the build \nmore"), Some("fix the build".into()));
        assert_eq!(title_from_prompt("   "), None);
        let long = "x".repeat(200);
        assert_eq!(title_from_prompt(&long).unwrap().chars().count(), 80);
    }

    #[test]
    fn permission_options() {
        let o: acp::PermissionOption = parse(json!({"optionId": "allow", "name": "Allow", "kind": "allow_once"}));
        assert_eq!(
            permission_option(&o),
            pp::PermissionOption { option_id: "allow".into(), name: "Allow".into(), kind: pp::PermissionOptionKind::AllowOnce }
        );
    }
}
