use super::*;
use pawl_protocol::{PermissionOption, PermissionOptionKind, StopReason, ToolKind};

const MID: &str = "abcdef012345";
const CTX: &str = "abcdef012345:s1";

fn ev(kind: EventType, payload: Value) -> HubEvent {
    HubEvent { id: format!("{MID}:1"), seq: 1, machine_id: MID.into(), session_id: CTX.into(), kind, payload, ts: 0 }
}

fn req(id: &str) -> PermissionRequest {
    PermissionRequest {
        request_id: format!("{MID}:{id}"),
        machine_id: MID.into(),
        session_id: CTX.into(),
        agent_id: "codex".into(),
        tool_call: ToolCallPayload { tool_call_id: "t".into(), title: Some("cargo test".into()), kind: Some(ToolKind::Execute), ..Default::default() },
        options: vec![
            PermissionOption { option_id: "allow".into(), name: "Allow".into(), kind: PermissionOptionKind::AllowOnce },
            PermissionOption { option_id: "reject".into(), name: "Reject".into(), kind: PermissionOptionKind::RejectOnce },
        ],
        created_at: 0,
        voice_code: None,
    }
}

fn stop(r: StopReason) -> HubEvent {
    ev(EventType::Stop, serde_json::to_value(StopPayload { stop_reason: r }).unwrap())
}

fn started(reg: &TaskRegistry) -> Task {
    let t = reg.begin(CTX, "run the tests", &Caller::Local);
    reg.commit(&t.id);
    t
}

fn state(reg: &TaskRegistry, id: &str) -> TaskState {
    reg.get(id, None).unwrap().status.state
}

#[test]
fn a_prompt_turn_is_one_task_with_a_global_id_in_the_session_context() {
    let reg = TaskRegistry::new(MID);
    let t = started(&reg);
    assert!(t.id.starts_with(&format!("{MID}:")));
    assert_eq!(t.context_id, CTX);
    assert_eq!(t.status.state, TaskState::Submitted);
    assert_eq!(t.history.as_ref().unwrap()[0].text(), Some("run the tests"));
    assert_eq!(reg.active_task(CTX).unwrap().id, t.id);
    // The host's echo of the prompt does not start a second task.
    reg.on_event(&ev(EventType::UserMessage, json!({ "text": "run the tests" })));
    assert_eq!(reg.list(&Caller::Local, &ListTasksRequest::default_for_tests()).unwrap().tasks.len(), 1);
}

#[test]
fn acp_updates_map_to_working_artifacts_and_a_completed_status() {
    let reg = TaskRegistry::new(MID);
    let t = started(&reg);
    reg.on_event(&ev(EventType::AgentThoughtChunk, json!({ "text": "hmm" })));
    assert_eq!(state(&reg, &t.id), TaskState::Working);
    reg.on_event(&ev(EventType::AgentMessageChunk, json!({ "text": "All " })));
    reg.on_event(&ev(EventType::AgentMessageChunk, json!({ "text": "green." })));
    reg.on_event(&ev(EventType::ToolCall, json!({ "toolCallId": "e1", "title": "Edit lib.rs", "kind": "edit", "rawInput": { "old": "a", "new": "b" } })));
    let mid = reg.get(&t.id, None).unwrap();
    assert_eq!(mid.status.message.as_ref().unwrap().text(), Some("Editing: Edit lib.rs"));
    let names: Vec<_> = mid.artifacts.as_ref().unwrap().iter().map(|a| a.name.clone().unwrap()).collect();
    assert_eq!(names, vec!["response", "diff"]);
    reg.on_event(&stop(StopReason::EndTurn));
    let done = reg.get(&t.id, None).unwrap();
    assert_eq!(done.status.state, TaskState::Completed);
    assert_eq!(done.status.message.as_ref().unwrap().text(), Some("All green."));
    assert_eq!(done.metadata.as_ref().unwrap()[mapping::STOP_REASON_KEY], "end_turn");
    assert_eq!(done.history.as_ref().unwrap().len(), 2);
    assert!(reg.active_task(CTX).is_none());
}

#[test]
fn stop_reasons_and_errors_end_tasks_in_the_mapped_states() {
    for (event, want) in [
        (stop(StopReason::Cancelled), TaskState::Canceled),
        (stop(StopReason::Refusal), TaskState::Rejected),
        (stop(StopReason::MaxTokens), TaskState::Completed),
        (ev(EventType::Error, json!({ "message": "session/prompt failed: boom" })), TaskState::Failed),
        (ev(EventType::SessionEnd, json!({})), TaskState::Failed),
    ] {
        let reg = TaskRegistry::new(MID);
        let t = started(&reg);
        reg.on_event(&event);
        assert_eq!(state(&reg, &t.id), want, "{:?}", event.kind);
    }
    let reg = TaskRegistry::new(MID);
    let t = started(&reg);
    reg.on_event(&ev(EventType::Error, json!({ "message": "session/prompt failed: boom" })));
    assert_eq!(reg.get(&t.id, None).unwrap().status.message.unwrap().text(), Some("session/prompt failed: boom"));
}

#[test]
fn permission_requests_are_input_required_with_a_structured_input_schema() {
    let reg = TaskRegistry::new(MID);
    let t = started(&reg);
    reg.on_event(&ev(EventType::RequestPermission, serde_json::to_value(req("r1")).unwrap()));
    reg.on_event(&ev(EventType::RequestPermission, serde_json::to_value(req("r2")).unwrap()));
    let waiting = reg.get(&t.id, None).unwrap();
    assert_eq!(waiting.status.state, TaskState::InputRequired);
    let (schema, id) = structured_input::read_input_request(waiting.status.message.as_ref().unwrap()).unwrap();
    assert_eq!(id.as_deref(), Some("abcdef012345:r1"));
    assert_eq!(schema["properties"]["optionId"]["enum"], json!(["allow", "reject"]));
    assert_eq!(reg.pending_request(&t.id).unwrap().request_id, "abcdef012345:r1");
    // A tool call while waiting does not hide the question.
    reg.on_event(&ev(EventType::ToolCall, json!({ "toolCallId": "x", "kind": "read" })));
    assert_eq!(state(&reg, &t.id), TaskState::InputRequired);
    let resolved = |id: &str| ev(EventType::PermissionResolved, json!({ "requestId": format!("{MID}:{id}"), "outcome": { "kind": "cancelled" }, "by": "user" }));
    reg.on_event(&resolved("r1"));
    let next = reg.get(&t.id, None).unwrap();
    assert_eq!(next.status.state, TaskState::InputRequired);
    assert_eq!(structured_input::read_input_request(next.status.message.as_ref().unwrap()).unwrap().1.as_deref(), Some("abcdef012345:r2"));
    reg.on_event(&resolved("r2"));
    assert_eq!(state(&reg, &t.id), TaskState::Working);
    assert!(reg.pending_request(&t.id).is_none());
}

#[test]
fn a_rejected_answer_re_asks_with_validation_errors() {
    let reg = TaskRegistry::new(MID);
    let t = started(&reg);
    reg.on_event(&ev(EventType::RequestPermission, serde_json::to_value(req("r1")).unwrap()));
    reg.reject_input(&t.id, Some(vec![json!({ "keywordLocation": "/required", "instanceLocation": "", "error": "missing" })]));
    let m = reg.get(&t.id, None).unwrap().status.message.unwrap();
    assert_eq!(structured_input::read_input_request(&m).unwrap().1.as_deref(), Some("abcdef012345:r1"));
    assert!(serde_json::to_string(&m).unwrap().contains("validation_error"));
}

#[test]
fn abort_forgets_the_task_and_restores_the_previous_one() {
    let reg = TaskRegistry::new(MID);
    let first = started(&reg);
    let second = reg.begin(CTX, "again", &Caller::Local);
    reg.abort(&second.id);
    assert!(reg.get(&second.id, None).is_none());
    assert_eq!(reg.active_task(CTX).unwrap().id, first.id);
    // A committed new task closes a previous one that never saw its end.
    let third = reg.begin(CTX, "third", &Caller::Local);
    reg.commit(&third.id);
    assert_eq!(state(&reg, &first.id), TaskState::Failed);
    assert_eq!(reg.active_task(CTX).unwrap().id, third.id);
}

#[test]
fn a_user_message_without_a_task_starts_one() {
    let reg = TaskRegistry::new(MID);
    reg.on_event(&ev(EventType::UserMessage, json!({ "text": "typed elsewhere" })));
    let t = reg.active_task(CTX).unwrap();
    assert_eq!(t.history.unwrap()[0].text(), Some("typed elsewhere"));
    // Events of a context with no task are ignored.
    let reg = TaskRegistry::new(MID);
    reg.on_event(&stop(StopReason::EndTurn));
    assert!(reg.latest_per_context().is_empty());
}

#[test]
fn cancel_is_terminal_and_idempotent() {
    let reg = TaskRegistry::new(MID);
    let t = started(&reg);
    assert_eq!(reg.cancel(&t.id).unwrap().status.state, TaskState::Canceled);
    reg.on_event(&stop(StopReason::EndTurn));
    assert_eq!(state(&reg, &t.id), TaskState::Canceled);
    assert_eq!(reg.cancel(&t.id).unwrap().status.state, TaskState::Canceled);
    assert!(reg.cancel("nope").is_none());
    let t2 = started(&reg);
    reg.on_session_ended(CTX);
    assert_eq!(state(&reg, &t2.id), TaskState::Failed);
}

#[test]
fn outside_callers_see_only_their_own_contexts() {
    let reg = TaskRegistry::new(MID);
    let mine = Caller::Outside("ci-bot".into());
    reg.bind_context("abcdef012345:theirs", mine.clone());
    let a = reg.begin("abcdef012345:theirs", "x", &mine);
    reg.commit(&a.id);
    started(&reg); // a local context
    assert!(reg.can_access(&mine, "abcdef012345:theirs"));
    assert!(!reg.can_access(&mine, CTX));
    assert!(!reg.can_access(&Caller::Outside("other".into()), "abcdef012345:theirs"));
    assert!(reg.can_access(&Caller::Local, "abcdef012345:theirs"));
    assert!(reg.can_access(&Caller::Peer("zyxwvu987654".into()), CTX));
    let req = ListTasksRequest::default_for_tests();
    assert_eq!(reg.list(&mine, &req).unwrap().tasks.len(), 1);
    assert_eq!(reg.list(&Caller::Outside("other".into()), &req).unwrap().tasks.len(), 0);
    assert_eq!(reg.list(&Caller::Local, &req).unwrap().tasks.len(), 2);
    // First binding wins.
    reg.bind_context("abcdef012345:theirs", Caller::Local);
    assert_eq!(reg.owner_of("abcdef012345:theirs"), mine);
    assert_eq!(mine.label(), "a2a:ci-bot");
    assert_eq!(reg.contexts_owned_by(&mine), vec!["abcdef012345:theirs".to_string()]);
    // A new generation of the same caller id is someone else.
    let again = Caller::Outside("ci-bot~2".into());
    assert_eq!(again.label(), "a2a:ci-bot");
    assert!(!reg.can_access(&again, "abcdef012345:theirs"));
    assert!(reg.contexts_owned_by(&again).is_empty());
}

#[test]
fn list_pages_newest_first_and_filters() {
    let reg = TaskRegistry::new(MID);
    let mut ids = vec![];
    for i in 0..5 {
        let t = reg.begin(CTX, &format!("p{i}"), &Caller::Local);
        reg.commit(&t.id);
        reg.on_event(&stop(StopReason::EndTurn));
        ids.push(t.id);
    }
    let mut req = ListTasksRequest::default_for_tests();
    req.page_size = Some(2);
    let p1 = reg.list(&Caller::Local, &req).unwrap();
    assert_eq!(p1.tasks.iter().map(|t| t.id.clone()).collect::<Vec<_>>(), vec![ids[4].clone(), ids[3].clone()]);
    assert_eq!(p1.total_size, 5);
    assert!(p1.tasks[0].artifacts.is_none());
    req.page_token = Some(p1.next_page_token.clone());
    let p2 = reg.list(&Caller::Local, &req).unwrap();
    assert_eq!(p2.tasks[0].id, ids[2]);
    req.page_token = Some("x".into());
    assert!(reg.list(&Caller::Local, &req).is_err());
    let mut by_state = ListTasksRequest::default_for_tests();
    by_state.status = Some(TaskState::Working);
    assert!(reg.list(&Caller::Local, &by_state).unwrap().tasks.is_empty());
    let mut hist = ListTasksRequest::default_for_tests();
    hist.history_length = Some(1);
    assert_eq!(reg.list(&Caller::Local, &hist).unwrap().tasks[0].history.as_ref().unwrap().len(), 1);
    hist.history_length = Some(0);
    assert!(reg.list(&Caller::Local, &hist).unwrap().tasks[0].history.is_none());
}

#[test]
fn finished_tasks_are_evicted_per_context() {
    let reg = TaskRegistry::new(MID);
    let first = started(&reg);
    reg.on_event(&stop(StopReason::EndTurn));
    for _ in 0..TASKS_PER_CONTEXT {
        started(&reg);
        reg.on_event(&stop(StopReason::EndTurn));
    }
    assert!(reg.get(&first.id, None).is_none());
    let mut req = ListTasksRequest::default_for_tests();
    req.page_size = Some(100);
    assert_eq!(reg.list(&Caller::Local, &req).unwrap().total_size as usize, TASKS_PER_CONTEXT);
}

#[test]
fn reply_text_is_capped() {
    let reg = TaskRegistry::new(MID);
    let t = started(&reg);
    let big = "é".repeat(RESPONSE_MAX_BYTES); // 2 bytes each
    reg.on_event(&ev(EventType::AgentMessageChunk, json!({ "text": big })));
    reg.on_event(&ev(EventType::AgentMessageChunk, json!({ "text": "more" })));
    let got = reg.get(&t.id, None).unwrap();
    let text = got.artifacts.unwrap()[0].parts[0].as_text().unwrap().to_string();
    assert!(text.len() <= RESPONSE_MAX_BYTES);
    assert!(!text.contains("more"));
    assert_eq!(got.metadata.unwrap()[TRUNCATED_KEY], true);
}

#[tokio::test]
async fn subscribers_get_status_and_artifact_updates_until_the_end() {
    let reg = TaskRegistry::new(MID);
    let t = started(&reg);
    let (snap, rx) = reg.subscribe(&t.id).unwrap();
    assert_eq!(snap.id, t.id);
    let mut rx = rx.unwrap();
    reg.on_event(&ev(EventType::AgentMessageChunk, json!({ "text": "hi" })));
    reg.on_event(&stop(StopReason::EndTurn));
    let mut kinds = vec![];
    while let Ok(u) = rx.try_recv() {
        kinds.push(match u {
            StreamResponse::StatusUpdate(s) => format!("status:{}", mapping::state_name(&s.status.state)),
            StreamResponse::ArtifactUpdate(a) => format!("artifact:{}", a.last_chunk.unwrap_or(false)),
            _ => "other".into(),
        });
    }
    assert_eq!(kinds, vec!["status:working", "artifact:false", "artifact:true", "status:completed"]);
    let (_, none) = reg.subscribe(&t.id).unwrap();
    assert!(none.is_none());
    assert!(reg.subscribe("missing").is_none());
}

trait DefaultForTests {
    fn default_for_tests() -> Self;
}

impl DefaultForTests for ListTasksRequest {
    fn default_for_tests() -> Self {
        ListTasksRequest {
            context_id: None,
            status: None,
            page_size: None,
            page_token: None,
            history_length: None,
            status_timestamp_after: None,
            include_artifacts: None,
            tenant: None,
        }
    }
}
