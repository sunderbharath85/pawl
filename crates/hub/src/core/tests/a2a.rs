//! A1: the orchestrator's A2A facade over this machine's ACP sessions (see `core/a2a.rs`).

use super::*;
use crate::core::a2a::{self as facade, A2aError, Answers};
use pawl_a2a::spec::{ListTasksRequest, Message, Part, Role, SendMessageResponse, TaskState};
use pawl_a2a::structured_input::{self as si, read_input_request};
use pawl_a2a::Caller;
use pawl_protocol::StopReason;

fn list_all() -> ListTasksRequest {
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

async fn host_event(f: &Fixture, session_id: &str, kind: EventType, payload: Value) {
    f.hub.on_host_event(HostEvent::Event { session_id: session_id.into(), kind, payload }).await;
}

fn latest(f: &Fixture, session_id: &str) -> pawl_a2a::spec::Task {
    let mut req = list_all();
    req.context_id = Some(session_id.into());
    f.hub.a2a_list(&req, &Caller::Local).unwrap().tasks.into_iter().next().expect("a task")
}

fn answer(task_id: &str, request_id: &str, option: &str) -> pawl_a2a::spec::SendMessageRequest {
    let mut m = Message::new(Role::User, vec![si::structured_input_part(Some(request_id), json!({ "optionId": option }))]);
    m.task_id = Some(task_id.into());
    pawl_a2a::spec::SendMessageRequest { message: m, configuration: None, metadata: None, tenant: None }
}

/// A session started with a prompt, plus the task of that first turn.
async fn turn(f: &Fixture) -> (Session, pawl_a2a::spec::Task) {
    let s = start(f).await;
    let t = latest(f, &s.id);
    (s, t)
}

#[tokio::test]
async fn a_start_with_a_prompt_is_one_task_in_the_sessions_context() {
    let f = fixture(None).await;
    let (s, t) = turn(&f).await;
    assert_eq!(t.context_id, s.id);
    assert!(t.id.starts_with(&format!("{MID}:")), "task ids are global: {}", t.id);
    assert_eq!(t.status.state, TaskState::Submitted);
    assert_eq!(t.history.as_ref().unwrap()[0].text(), Some("run the tests"));
    // The ACP executor got the turn.
    assert!(f.fake.calls().iter().any(|c| c == &format!("prompt {} run the tests", s.id)), "{:?}", f.fake.calls());
    // The turn's ACP updates drive the task.
    host_event(&f, &s.id, EventType::UserMessage, json!({ "text": "run the tests" })).await;
    host_event(&f, &s.id, EventType::AgentMessageChunk, json!({ "text": "All tests pass." })).await;
    assert_eq!(f.hub.a2a_get(&t.id, None, &Caller::Local).unwrap().status.state, TaskState::Working);
    host_event(&f, &s.id, EventType::Stop, json!({ "stopReason": "end_turn" })).await;
    let done = f.hub.a2a_get(&t.id, None, &Caller::Local).unwrap();
    assert_eq!(done.status.state, TaskState::Completed);
    assert_eq!(done.status.message.unwrap().text(), Some("All tests pass."));
    // Each prompt is a new task in the same context.
    ack_result(f.hub.handle(ClientMsg::Prompt { id: "p".into(), session_id: s.id.clone(), text: "and lint".into(), images: Vec::new() }, &actor()).await);
    let tasks = f.hub.a2a_list(&list_all(), &Caller::Local).unwrap().tasks;
    assert_eq!(tasks.len(), 2);
    assert_eq!(tasks[0].context_id, s.id);
    assert_eq!(tasks[0].status.state, TaskState::Submitted);
}

#[tokio::test]
async fn a_start_without_a_prompt_answers_with_a_message_and_no_task() {
    let f = fixture(None).await;
    let cwd = f.dir.path().to_string_lossy().into_owned();
    let d = f.hub.a2a_send(facade::start_request("codex", &cwd, None), &Caller::Local, &actor(), Answers::Any).await.unwrap();
    let SendMessageResponse::Message(m) = d.response else { panic!("expected a message") };
    let s = d.session.unwrap();
    assert_eq!(m.context_id.as_deref(), Some(s.id.as_str()));
    assert!(m.text().unwrap().starts_with("Started Codex in"));
    assert!(f.hub.a2a_list(&list_all(), &Caller::Local).unwrap().tasks.is_empty());
    // Delegate metadata is required without a context.
    let bare = pawl_a2a::spec::SendMessageRequest { message: Message::new(Role::User, vec![Part::text("hi")]), configuration: None, metadata: None, tenant: None };
    let e = f.hub.a2a_send(bare, &Caller::Local, &actor(), Answers::Any).await.unwrap_err();
    assert!(matches!(A2aError::find(&e), Some(A2aError::InvalidParams(_))), "{e:#}");
}

#[tokio::test]
async fn a_prompt_the_agent_refuses_fails_as_before_and_leaves_no_task() {
    let f = fixture(None).await;
    let (s, first) = turn(&f).await;
    let e = ack_error(f.hub.handle(ClientMsg::Prompt { id: "p".into(), session_id: s.id.clone(), text: "fail".into(), images: Vec::new() }, &actor()).await);
    assert!(e.contains("refused"), "{e}");
    let tasks = f.hub.a2a_list(&list_all(), &Caller::Local).unwrap().tasks;
    assert_eq!(tasks.len(), 1);
    assert_eq!(f.hub.tasks().active_task(&s.id).unwrap().id, first.id);
    // Unknown and foreign sessions keep their ack codes.
    assert_eq!(ack_error(f.hub.handle(ClientMsg::Prompt { id: "q".into(), session_id: global_id(MID, "nope"), text: "x".into(), images: Vec::new() }, &actor()).await), "not_found");
}

#[tokio::test]
async fn permission_requests_are_input_required_and_answers_go_through_the_audited_decision() {
    let f = fixture(None).await;
    let (s, t) = turn(&f).await;
    let req = permission(&s.id, ToolKind::Execute);
    f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    let waiting = f.hub.a2a_get(&t.id, None, &Caller::Local).unwrap();
    assert_eq!(waiting.status.state, TaskState::InputRequired);
    let (schema, id) = read_input_request(waiting.status.message.as_ref().unwrap()).unwrap();
    assert_eq!(id.as_deref(), Some(req.request_id.as_str()));
    assert_eq!(schema["properties"]["optionId"]["enum"], json!(["allow", "reject"]));

    // An answer that does not validate re-asks (no decision).
    let d = f.hub.a2a_send(answer(&t.id, &req.request_id, "maybe"), &Caller::Local, &actor(), Answers::Any).await.unwrap();
    let SendMessageResponse::Task(again) = d.response else { panic!() };
    assert_eq!(again.status.state, TaskState::InputRequired);
    assert!(serde_json::to_string(&again.status.message).unwrap().contains("validation_error"));
    assert!(!f.fake.calls().iter().any(|c| c.starts_with("resolve")));

    // A valid answer is decided like a click: the agent is told, the audit log has it.
    let d = f.hub.a2a_send(answer(&t.id, &req.request_id, "allow"), &Caller::Local, &actor(), Answers::Any).await.unwrap();
    let SendMessageResponse::Task(after) = d.response else { panic!() };
    assert_eq!(after.status.state, TaskState::Working);
    assert!(f.fake.calls().contains(&format!("resolve {} allow", req.request_id)));
    let audit = f.hub.store().audit_log(10).unwrap();
    assert_eq!(audit[0].request_id, req.request_id);
    assert_eq!(audit[0].option_id.as_deref(), Some("allow"));
    assert_eq!(audit[0].actor, "desktop#1");
    // First valid decision wins.
    f.hub.on_host_event(HostEvent::PermissionRequest(permission(&s.id, ToolKind::Execute))).await;
    let e = f.hub.a2a_send(answer(&t.id, &req.request_id, "allow"), &Caller::Local, &actor(), Answers::Any).await;
    let SendMessageResponse::Task(t2) = e.unwrap().response else { panic!() };
    assert_eq!(t2.status.state, TaskState::InputRequired, "an answer for an older request re-asks the new one");
}

#[tokio::test]
async fn outside_answers_are_limited_by_the_hard_floor_and_never_always() {
    let f = fixture(None).await;
    let (s, t) = turn(&f).await;
    let ci = Caller::Outside("ci".into());
    // Outside callers only see their own contexts, so bind this one to it for the test.
    let exec = permission(&s.id, ToolKind::Execute);
    f.hub.on_host_event(HostEvent::PermissionRequest(exec.clone())).await;
    let e = f.hub.a2a_send(answer(&t.id, &exec.request_id, "allow"), &ci, &actor(), Answers::Limited).await.unwrap_err();
    assert!(matches!(A2aError::find(&e), Some(A2aError::TaskNotFound)), "not its context: {e:#}");

    let cwd = f.dir.path().to_string_lossy().into_owned();
    let d = f.hub.a2a_send(facade::start_request("codex", &cwd, Some("build")), &ci, &actor(), Answers::Limited).await.unwrap();
    let SendMessageResponse::Task(ot) = d.response else { panic!() };
    let os = d.session.unwrap();
    let exec = permission(&os.id, ToolKind::Execute);
    f.hub.on_host_event(HostEvent::PermissionRequest(exec.clone())).await;
    let e = f.hub.a2a_send(answer(&ot.id, &exec.request_id, "allow"), &ci, &actor(), Answers::Limited).await.unwrap_err();
    assert_eq!(AckCode::find(&e), Some(AckCode::Forbidden), "execute needs a person: {e:#}");
    let e = f.hub.a2a_send(answer(&ot.id, &exec.request_id, "allow"), &ci, &actor(), Answers::Never).await.unwrap_err();
    assert_eq!(AckCode::find(&e), Some(AckCode::Forbidden));
    // Deny is fine.
    f.hub.a2a_send(answer(&ot.id, &exec.request_id, "reject"), &ci, &actor(), Answers::Limited).await.unwrap();
    assert!(f.fake.calls().contains(&format!("resolve {} reject", exec.request_id)));
    // A read may be allowed once, never always.
    let mut read = permission(&os.id, ToolKind::Read);
    read.options.push(PermissionOption { option_id: "always".into(), name: "Always".into(), kind: PermissionOptionKind::AllowAlways });
    f.hub.on_host_event(HostEvent::PermissionRequest(read.clone())).await;
    let e = f.hub.a2a_send(answer(&ot.id, &read.request_id, "always"), &ci, &actor(), Answers::Limited).await.unwrap_err();
    assert_eq!(AckCode::find(&e), Some(AckCode::Forbidden));
    f.hub.a2a_send(answer(&ot.id, &read.request_id, "allow"), &ci, &actor(), Answers::Limited).await.unwrap();
    assert!(f.fake.calls().contains(&format!("resolve {} allow", read.request_id)));
}

#[tokio::test]
async fn outside_callers_are_bound_to_their_contexts_and_never_auto_approved() {
    let f = fixture_with(Some(vec![rule("reads", Some(ToolKind::Read), RuleAction::AutoAllow)]), None).await;
    let (local, local_task) = turn(&f).await;
    let ci = Caller::Outside("ci".into());
    let e = f.hub.a2a_send(facade::prompt_request(&local.id, "hi"), &ci, &actor(), Answers::Never).await.unwrap_err();
    assert_eq!(AckCode::find(&e), Some(AckCode::NotFound));
    assert!(matches!(A2aError::find(&f.hub.a2a_get(&local_task.id, None, &ci).unwrap_err()), Some(A2aError::TaskNotFound)));
    assert!(matches!(A2aError::find(&f.hub.a2a_cancel(&local_task.id, &ci, &actor()).await.unwrap_err()), Some(A2aError::TaskNotFound)));
    assert!(f.hub.a2a_list(&list_all(), &ci).unwrap().tasks.is_empty());

    let cwd = f.dir.path().to_string_lossy().into_owned();
    let d = f.hub.a2a_send(facade::start_request("codex", &cwd, Some("look around")), &ci, &actor(), Answers::Never).await.unwrap();
    let theirs = d.session.unwrap();
    assert_eq!(f.hub.a2a_list(&list_all(), &ci).unwrap().tasks.len(), 1);
    assert_eq!(f.hub.a2a_list(&list_all(), &Caller::Local).unwrap().tasks.len(), 2, "the user sees everything");
    // A read the rules would allow still asks a person.
    let read = permission(&theirs.id, ToolKind::Read);
    f.hub.on_host_event(HostEvent::PermissionRequest(read.clone())).await;
    assert!(f.hub.pending_permissions().iter().any(|r| r.request_id == read.request_id));
    // The same rule on a local session auto-allows.
    let local_read = permission(&local.id, ToolKind::Read);
    f.hub.on_host_event(HostEvent::PermissionRequest(local_read.clone())).await;
    assert!(!f.hub.pending_permissions().iter().any(|r| r.request_id == local_read.request_id));
}

#[tokio::test]
async fn cancel_is_cancel_task_on_the_running_turn() {
    let f = fixture(None).await;
    let (s, t) = turn(&f).await;
    let req = permission(&s.id, ToolKind::Execute);
    f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    ack_result(f.hub.handle(ClientMsg::Cancel { id: "c".into(), session_id: s.id.clone() }, &actor()).await);
    assert!(f.fake.calls().contains(&format!("cancel {}", s.id)));
    let t = f.hub.a2a_get(&t.id, None, &Caller::Local).unwrap();
    assert_eq!(t.status.state, TaskState::Canceled);
    assert!(f.hub.pending_permissions().is_empty(), "pending requests close as before");
    // The agent's own stop afterwards changes nothing; a finished task is not cancelable.
    host_event(&f, &s.id, EventType::Stop, json!({ "stopReason": "cancelled" })).await;
    assert_eq!(f.hub.a2a_get(&t.id, None, &Caller::Local).unwrap().status.state, TaskState::Canceled);
    let e = f.hub.a2a_cancel(&t.id, &Caller::Local, &actor()).await.unwrap_err();
    assert_eq!(A2aError::find(&e), Some(A2aError::TaskNotCancelable));
    // With no turn running, cancel still reaches the session.
    ack_result(f.hub.handle(ClientMsg::Cancel { id: "c2".into(), session_id: s.id.clone() }, &actor()).await);
    assert_eq!(f.fake.calls().iter().filter(|c| *c == &format!("cancel {}", s.id)).count(), 2);
}

#[tokio::test]
async fn stop_reasons_end_tasks_and_a_session_exit_fails_the_turn() {
    let f = fixture(None).await;
    let (s, t) = turn(&f).await;
    host_event(&f, &s.id, EventType::Stop, json!({ "stopReason": "refusal" })).await;
    assert_eq!(f.hub.a2a_get(&t.id, None, &Caller::Local).unwrap().status.state, TaskState::Rejected);
    f.hub.prompt(&s.id, "again".into()).await.unwrap();
    let t2 = latest(&f, &s.id);
    let mut ended = f.hub.session(&s.id).unwrap();
    ended.ended_at = Some(pawl_protocol::now_ms());
    ended.status = SessionStatus::Error;
    f.hub.on_host_event(HostEvent::Session(ended)).await;
    assert_eq!(f.hub.a2a_get(&t2.id, None, &Caller::Local).unwrap().status.state, TaskState::Failed);
    let _ = StopReason::EndTurn;
}

#[tokio::test]
async fn hook_sessions_have_no_tasks() {
    let f = fixture(None).await;
    f.hub
        .ingest_hook(HookSource::Claude, json!({ "hook_event_name": "UserPromptSubmit", "session_id": "ext-1", "cwd": f.dir.path(), "prompt": "hello" }))
        .await
        .unwrap();
    assert!(f.hub.a2a_list(&list_all(), &Caller::Local).unwrap().tasks.is_empty());
}

#[tokio::test]
async fn remote_prompts_and_starts_are_a2a_delegations_of_the_peer() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    let s = start(&f).await;
    let a = intent(&f.hub, OTHER, "p1", false, Intent::Prompt { session_id: s.id.clone(), text: "from the mac".into() }).await;
    assert_eq!(a.status, IntentStatus::Applied, "{a:?}");
    let t = latest(&f, &s.id);
    assert_eq!(t.history.as_ref().unwrap()[0].text(), Some("from the mac"));
    assert!(f.hub.st().remote_driven.contains(&s.id));
    let a = intent(&f.hub, OTHER, "c1", false, Intent::Cancel { session_id: s.id.clone() }).await;
    assert_eq!(a.status, IntentStatus::Applied, "{a:?}");
    assert_eq!(f.hub.a2a_get(&t.id, None, &Caller::Local).unwrap().status.state, TaskState::Canceled);
    let cwd = f.dir.path().to_string_lossy().into_owned();
    let a = intent(&f.hub, OTHER, "s1", true, Intent::Start { agent_id: "codex".into(), cwd, prompt: Some("go".into()) }).await;
    assert_eq!(a.status, IntentStatus::Applied, "{a:?}");
    let sid = a.result.unwrap()["session"]["id"].as_str().unwrap().to_string();
    assert_eq!(f.hub.tasks().owner_of(&sid), Caller::Peer(OTHER.into()));
    assert!(f.hub.st().remote_driven.contains(&sid));
}
