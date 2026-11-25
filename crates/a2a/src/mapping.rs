//! ACP (as normalized by `pawl-acp` into hub events) -> A2A.
//!
//! | ACP                          | A2A                                                        |
//! |------------------------------|------------------------------------------------------------|
//! | session                      | context (`contextId` = the session's global id)            |
//! | `session/prompt` turn        | Task (`TASK_STATE_SUBMITTED` until the agent starts)        |
//! | thought / tool call / plan   | `TASK_STATE_WORKING` status updates                         |
//! | agent message chunks         | the `response` artifact (appended), final status message    |
//! | edit / delete / move calls   | a `diff` artifact per tool call (path + tool input)         |
//! | `request_permission`         | `TASK_STATE_INPUT_REQUIRED` + Structured Input schema       |
//! | stop `end_turn`              | `TASK_STATE_COMPLETED`                                      |
//! | stop `cancelled`             | `TASK_STATE_CANCELED`                                       |
//! | stop `refusal`               | `TASK_STATE_REJECTED`                                       |
//! | stop `max_tokens` / `max_turn_requests` | `TASK_STATE_COMPLETED` (metadata `stopReason`)    |
//! | `session/prompt` error, process exit    | `TASK_STATE_FAILED`                               |

use std::collections::HashMap;

use a2a::{Artifact, Message, Part, Role, TaskState};
use pawl_protocol::{PlanPayload, StopReason, ToolCallPayload, ToolKind};
use serde_json::{json, Value};

/// Metadata key Pawl puts on terminal statuses and tasks: the ACP stop reason.
pub const STOP_REASON_KEY: &str = "pawl/stopReason";

/// Name of the artifact that collects the agent's reply text.
pub const RESPONSE_ARTIFACT: &str = "response";

/// The A2A state a turn ends in.
pub fn state_for_stop(reason: StopReason) -> TaskState {
    match reason {
        StopReason::EndTurn | StopReason::MaxTokens | StopReason::MaxTurnRequests => TaskState::Completed,
        StopReason::Cancelled => TaskState::Canceled,
        StopReason::Refusal => TaskState::Rejected,
    }
}

/// The wire name of a stop reason ("end_turn", ...).
pub fn stop_reason_name(reason: StopReason) -> String {
    serde_json::to_value(reason).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default()
}

/// Lower-case wire name of an A2A state without the prefix ("working", "input_required").
pub fn state_name(s: &TaskState) -> &'static str {
    match s {
        TaskState::Unspecified => "unspecified",
        TaskState::Submitted => "submitted",
        TaskState::Working => "working",
        TaskState::Completed => "completed",
        TaskState::Failed => "failed",
        TaskState::Canceled => "canceled",
        TaskState::InputRequired => "input_required",
        TaskState::Rejected => "rejected",
        TaskState::AuthRequired => "auth_required",
    }
}

/// An agent message in a task.
pub fn agent_message(text: impl Into<String>, context_id: &str, task_id: &str) -> Message {
    let mut m = Message::new(Role::Agent, vec![Part::text(text)]);
    m.context_id = Some(context_id.to_string());
    m.task_id = Some(task_id.to_string());
    m
}

/// A short status line for a tool call ("Running: cargo test").
pub fn tool_status_text(tc: &ToolCallPayload) -> String {
    let title = tc.title.as_deref().filter(|t| !t.trim().is_empty()).unwrap_or("a tool");
    let verb = match tc.kind {
        Some(ToolKind::Read) => "Reading",
        Some(ToolKind::Edit) => "Editing",
        Some(ToolKind::Delete) => "Deleting",
        Some(ToolKind::Move) => "Moving",
        Some(ToolKind::Search) => "Searching",
        Some(ToolKind::Execute) => "Running",
        Some(ToolKind::Think) => "Thinking",
        Some(ToolKind::Fetch) => "Fetching",
        Some(ToolKind::SwitchMode) => "Switching mode",
        Some(ToolKind::Other) | None => "Using",
    };
    format!("{verb}: {title}")
}

/// A `diff` artifact for a tool call that changes files (edit, delete, move); None otherwise.
/// The ACP host forwards the tool's locations and raw input (e.g. old/new strings), not a rendered
/// diff, so the artifact carries those as data.
pub fn diff_artifact(tc: &ToolCallPayload) -> Option<Artifact> {
    if !matches!(tc.kind, Some(ToolKind::Edit | ToolKind::Delete | ToolKind::Move)) {
        return None;
    }
    let data = json!({
        "toolCallId": tc.tool_call_id,
        "title": tc.title,
        "kind": tc.kind,
        "locations": tc.locations,
        "rawInput": tc.raw_input,
    });
    Some(Artifact {
        artifact_id: format!("diff-{}", tc.tool_call_id),
        name: Some("diff".into()),
        description: tc.title.clone(),
        parts: vec![Part::data(data).with_media_type("application/json")],
        metadata: None,
        extensions: None,
    })
}

/// The agent's plan as an artifact (replaced on every update).
pub fn plan_artifact(plan: &PlanPayload) -> Artifact {
    Artifact {
        artifact_id: "plan".into(),
        name: Some("plan".into()),
        description: None,
        parts: vec![Part::data(serde_json::to_value(plan).unwrap_or(Value::Null)).with_media_type("application/json")],
        metadata: None,
        extensions: None,
    }
}

/// A chunk of the agent's reply as a `response` artifact part.
pub fn response_chunk(task_id: &str, text: &str) -> Artifact {
    Artifact {
        artifact_id: format!("{task_id}/response"),
        name: Some(RESPONSE_ARTIFACT.into()),
        description: None,
        parts: vec![Part::text(text)],
        metadata: None,
        extensions: None,
    }
}

/// `{ STOP_REASON_KEY: reason }`.
pub fn stop_metadata(reason: StopReason) -> HashMap<String, Value> {
    HashMap::from([(STOP_REASON_KEY.to_string(), json!(stop_reason_name(reason)))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_reasons_map_to_the_planned_states() {
        assert_eq!(state_for_stop(StopReason::EndTurn), TaskState::Completed);
        assert_eq!(state_for_stop(StopReason::Cancelled), TaskState::Canceled);
        assert_eq!(state_for_stop(StopReason::Refusal), TaskState::Rejected);
        assert_eq!(state_for_stop(StopReason::MaxTokens), TaskState::Completed);
        assert_eq!(state_for_stop(StopReason::MaxTurnRequests), TaskState::Completed);
        assert_eq!(stop_reason_name(StopReason::MaxTurnRequests), "max_turn_requests");
        assert_eq!(stop_metadata(StopReason::EndTurn)[STOP_REASON_KEY], "end_turn");
    }

    #[test]
    fn only_file_changing_tools_become_diff_artifacts() {
        let mut tc = ToolCallPayload { tool_call_id: "t1".into(), title: Some("Edit lib.rs".into()), kind: Some(ToolKind::Edit), ..Default::default() };
        let a = diff_artifact(&tc).unwrap();
        assert_eq!(a.artifact_id, "diff-t1");
        assert_eq!(a.name.as_deref(), Some("diff"));
        tc.kind = Some(ToolKind::Execute);
        assert!(diff_artifact(&tc).is_none());
        tc.kind = None;
        assert!(diff_artifact(&tc).is_none());
        assert_eq!(tool_status_text(&ToolCallPayload { kind: Some(ToolKind::Execute), title: Some("cargo test".into()), ..Default::default() }), "Running: cargo test");
    }
}
