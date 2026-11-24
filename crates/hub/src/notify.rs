//! Notifications. Desktop/phone notifications are a client concern: the hub only emits
//! `notification` messages on the bus; clients decide how to surface them.

use krypto_protocol::{
    global_id, AgentId, HubNotification, NotificationLevel, PermissionRequest, Session, SessionId, StopPayload,
    StopReason,
};

/// A notification of `machine_id` (its id is global: `machine:uuid`).
pub fn make(
    machine_id: &str,
    level: NotificationLevel,
    title: impl Into<String>,
    body: impl Into<String>,
    session_id: Option<SessionId>,
    agent_id: Option<AgentId>,
) -> HubNotification {
    HubNotification {
        id: global_id(machine_id, uuid::Uuid::new_v4()),
        machine_id: machine_id.to_string(),
        level,
        title: title.into(),
        body: body.into(),
        session_id,
        agent_id,
        ts: krypto_protocol::now_ms(),
    }
}

/// Short human label for a session: title, else last path component of cwd.
pub fn session_label(s: &Session) -> String {
    if let Some(t) = s.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        return truncate(t, 80);
    }
    let cwd = s.cwd.trim_end_matches(['/', '\\']);
    let leaf = cwd.rsplit(['/', '\\']).next().unwrap_or(cwd);
    if leaf.is_empty() { "a session".to_string() } else { leaf.to_string() }
}

pub fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Notification for a finished turn of an owned session (None for user cancellations).
pub fn for_stop(agent_name: &str, session: &Session, stop: &StopPayload) -> Option<HubNotification> {
    let label = session_label(session);
    let (level, title) = match stop.stop_reason {
        StopReason::EndTurn => (NotificationLevel::Success, format!("{agent_name} finished")),
        StopReason::Cancelled => return None,
        StopReason::Refusal => (NotificationLevel::Warning, format!("{agent_name} refused")),
        StopReason::MaxTokens => (NotificationLevel::Warning, format!("{agent_name} hit the token limit")),
        StopReason::MaxTurnRequests => (NotificationLevel::Warning, format!("{agent_name} hit the turn limit")),
    };
    Some(make(&session.machine_id, level, title, label, Some(session.id.clone()), Some(session.agent_id.clone())))
}

pub fn for_error(agent_name: &str, session: &Session, message: &str) -> HubNotification {
    make(
        &session.machine_id,
        NotificationLevel::Alert,
        format!("{agent_name} hit an error"),
        format!("{}: {}", session_label(session), truncate(message, 200)),
        Some(session.id.clone()),
        Some(session.agent_id.clone()),
    )
}

pub fn for_permission(agent_name: &str, pet_name: Option<&str>, req: &PermissionRequest) -> HubNotification {
    let who = match pet_name {
        Some(p) => format!("{p} ({agent_name})"),
        None => agent_name.to_string(),
    };
    let title = req.tool_call.title.as_deref().unwrap_or("a tool call");
    make(
        &req.machine_id,
        NotificationLevel::Alert,
        format!("{who} needs your OK"),
        truncate(title, 200),
        Some(req.session_id.clone()),
        Some(req.agent_id.clone()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use krypto_protocol::{SessionSource, SessionStatus};

    #[test]
    fn labels() {
        let mut s = Session {
            id: "abcdef012345:s".into(),
            machine_id: "abcdef012345".into(),
            agent_id: "codex".into(),
            acp_session_id: None,
            external_id: None,
            cwd: "C:\\code\\my-repo\\".into(),
            title: None,
            source: SessionSource::Owned,
            status: SessionStatus::Done,
            started_at: 0,
            last_event_at: 0,
            ended_at: None,
        };
        assert_eq!(session_label(&s), "my-repo");
        s.title = Some("Fix the tests".into());
        assert_eq!(session_label(&s), "Fix the tests");
        assert_eq!(truncate("abcdef", 4), "abc…");
        let n = for_stop("Codex", &s, &StopPayload { stop_reason: StopReason::EndTurn }).unwrap();
        assert_eq!(n.level, NotificationLevel::Success);
        assert_eq!(n.machine_id, "abcdef012345");
        assert_eq!(krypto_protocol::parse_global_id(&n.id).map(|(m, _)| m), Some("abcdef012345"));
        assert_eq!(n.session_id.as_deref(), Some("abcdef012345:s"));
        assert!(for_stop("Codex", &s, &StopPayload { stop_reason: StopReason::Cancelled }).is_none());
    }
}
