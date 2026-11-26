//! A3: outside A2A agents as sub-agents (see `core/a2a_agents.rs`), against a scripted agent served
//! by Pawl's own A2A server on loopback.

use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};

use super::*;
use crate::config::A2aAgentConfig;
use futures::StreamExt;
use pawl_a2a::server::{self as srv, A2aHandler, CallerAuth, EventStream, ServerState, ServiceParams};
use pawl_a2a::spec::{
    A2AError, AgentCard, Artifact, CancelTaskRequest, GetExtendedAgentCardRequest, GetTaskRequest, ListTasksRequest, ListTasksResponse, Message, Part, PartContent,
    Role, SendMessageRequest, SendMessageResponse, StreamResponse, SubscribeToTaskRequest, Task, TaskArtifactUpdateEvent, TaskState, TaskStatus,
    TaskStatusUpdateEvent,
};
use pawl_protocol::{A2aAgentPatch, HubSettingsA2aPatch, HubSettingsPatch};
use tokio::sync::mpsc;

const AGENT_TOKEN: &str = "agent-secret-token";
const TASK: &str = "r:t1";
const CTX: &str = "r:c1";

/// (contextId, taskId, text) of a prompt the agent got.
type Prompted = (Option<String>, Option<String>, String);

/// A scripted outside agent: "deploy" asks approve / deny (structured input), "wait" works until
/// canceled, anything else answers "Hi there".
#[derive(Default)]
struct Script {
    tx: Mutex<Option<mpsc::UnboundedSender<StreamResponse>>>,
    answers: Mutex<Vec<Value>>,
    cancels: Mutex<Vec<String>>,
    /// (contextId, taskId, text) of each new turn.
    prompts: Mutex<Vec<Prompted>>,
}

fn status(state: TaskState, text: Option<&str>) -> StreamResponse {
    StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
        task_id: TASK.into(),
        context_id: CTX.into(),
        status: TaskStatus { state, message: text.map(|t| Message::new(Role::Agent, vec![Part::text(t)])), timestamp: None },
        metadata: None,
    })
}

fn task(state: TaskState) -> Task {
    Task { id: TASK.into(), context_id: CTX.into(), status: TaskStatus { state, message: None, timestamp: None }, artifacts: None, history: None, metadata: None }
}

fn artifact(text: &str) -> StreamResponse {
    StreamResponse::ArtifactUpdate(TaskArtifactUpdateEvent {
        task_id: TASK.into(),
        context_id: CTX.into(),
        artifact: Artifact { artifact_id: "out".into(), name: None, description: None, parts: vec![Part::text(text)], metadata: None, extensions: None },
        append: None,
        last_chunk: None,
        metadata: None,
    })
}

/// A question with a schema picking one of `values`, tagged `id`.
fn ask_with(text: &str, id: Option<&str>, values: &[&str]) -> StreamResponse {
    let schema = json!({ "type": "object", "properties": { "pick": { "type": "string", "enum": values } } });
    let mut sp = Part::data(schema);
    let meta = match id {
        Some(i) => json!({ "type": "input_schema", "id": i }),
        None => json!({ "type": "input_schema" }),
    };
    sp.metadata = Some(HashMap::from([(pawl_a2a::STRUCTURED_INPUT_EXT.to_string(), meta)]));
    StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
        task_id: TASK.into(),
        context_id: CTX.into(),
        status: TaskStatus { state: TaskState::InputRequired, message: Some(Message::new(Role::Agent, vec![Part::text(text), sp])), timestamp: None },
        metadata: None,
    })
}

/// "Deploy v2 to production?" with a schema picking approve / deny (input id "q1").
fn ask_deploy(details_kind: Option<&str>) -> StreamResponse {
    let schema = json!({ "type": "object", "properties": { "decision": { "type": "string", "enum": ["approve", "deny"] } }, "required": ["decision"] });
    let mut sp = Part::data(schema);
    sp.metadata = Some(HashMap::from([(pawl_a2a::STRUCTURED_INPUT_EXT.to_string(), json!({ "type": "input_schema", "id": "q1" }))]));
    let mut parts = vec![Part::text("Deploy v2 to production?")];
    if let Some(k) = details_kind {
        parts.push(Part::data(json!({ "toolCall": { "kind": k, "title": "Read the release notes" }, "options": [{ "optionId": "approve", "name": "Yes, go", "kind": "allow_once" }] })));
    }
    parts.push(sp);
    StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
        task_id: TASK.into(),
        context_id: CTX.into(),
        status: TaskStatus { state: TaskState::InputRequired, message: Some(Message::new(Role::Agent, parts)), timestamp: None },
        metadata: None,
    })
}

impl Script {
    fn push(&self, ev: StreamResponse) {
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            let _ = tx.send(ev);
        }
    }
    fn end(&self) {
        self.tx.lock().unwrap().take();
    }
}

#[pawl_a2a::async_trait]
impl A2aHandler for Script {
    async fn send_message(&self, _p: &ServiceParams, req: SendMessageRequest) -> Result<SendMessageResponse, A2AError> {
        let m = req.message;
        if m.task_id.is_some() {
            // A continuation: the structured answer.
            let data = m.parts.iter().find_map(|p| match &p.content {
                PartContent::Data(d) => Some(d.clone()),
                _ => None,
            });
            let data = data.unwrap_or(Value::Null);
            self.answers.lock().unwrap().push(json!({ "data": data, "meta": m.parts[0].metadata }));
            match data["decision"].as_str() {
                Some("approve") => {
                    self.push(status(TaskState::Working, None));
                    self.push(status(TaskState::Completed, Some("Deployed.")));
                }
                _ => self.push(status(TaskState::Rejected, Some("Not deployed."))),
            }
            self.end();
            return Ok(SendMessageResponse::Task(task(TaskState::Working)));
        }
        self.prompts.lock().unwrap().push((m.context_id.clone(), m.task_id.clone(), m.text().unwrap_or_default().to_string()));
        let mut t = task(TaskState::Completed);
        t.artifacts = Some(vec![Artifact { artifact_id: "out".into(), name: None, description: None, parts: vec![Part::text("Hi there")], metadata: None, extensions: None }]);
        Ok(SendMessageResponse::Task(t))
    }

    async fn send_streaming_message(&self, _p: &ServiceParams, req: SendMessageRequest) -> Result<EventStream, A2AError> {
        let m = req.message;
        let text = m.text().unwrap_or_default().to_string();
        self.prompts.lock().unwrap().push((m.context_id.clone(), m.task_id.clone(), text.clone()));
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = tx.send(StreamResponse::Task(task(TaskState::Working)));
        match text.as_str() {
            "deploy" | "deploy-read" => {
                let _ = tx.send(artifact("Checking the release..."));
                let _ = tx.send(ask_deploy((text == "deploy-read").then_some("read")));
                *self.tx.lock().unwrap() = Some(tx);
            }
            "wait" => *self.tx.lock().unwrap() = Some(tx),
            // A hostile agent: question after question, fresh ids and none.
            "flood" => {
                let _ = tx.send(ask_with("First?", Some("a1"), &["yes", "no"]));
                let _ = tx.send(ask_with("Second?", Some("a2"), &["yes", "no"]));
                let _ = tx.send(ask_with("Third?", None, &["yes", "no"]));
                *self.tx.lock().unwrap() = Some(tx);
            }
            "many" => {
                let nine = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
                let _ = tx.send(ask_with("Pick one", Some("m1"), &nine));
                *self.tx.lock().unwrap() = Some(tx);
            }
            _ => {
                let _ = tx.send(artifact("Hi there"));
                let _ = tx.send(status(TaskState::Completed, None));
            }
        }
        Ok(tokio_stream_from(rx))
    }

    async fn get_task(&self, _p: &ServiceParams, _r: GetTaskRequest) -> Result<Task, A2AError> {
        Ok(task(TaskState::Working))
    }
    async fn list_tasks(&self, _p: &ServiceParams, _r: ListTasksRequest) -> Result<ListTasksResponse, A2AError> {
        Ok(ListTasksResponse { tasks: vec![], next_page_token: String::new(), page_size: 0, total_size: 0 })
    }
    async fn cancel_task(&self, _p: &ServiceParams, r: CancelTaskRequest) -> Result<Task, A2AError> {
        self.cancels.lock().unwrap().push(r.id);
        self.push(status(TaskState::Canceled, None));
        self.end();
        Ok(task(TaskState::Canceled))
    }
    async fn subscribe_to_task(&self, _p: &ServiceParams, _r: SubscribeToTaskRequest) -> Result<EventStream, A2AError> {
        Ok(futures::stream::empty().boxed())
    }
    async fn get_extended_agent_card(&self, _p: &ServiceParams, _r: GetExtendedAgentCardRequest) -> Result<AgentCard, A2AError> {
        Err(A2AError::extended_card_not_configured())
    }
}

fn tokio_stream_from(mut rx: mpsc::UnboundedReceiver<StreamResponse>) -> EventStream {
    futures::stream::poll_fn(move |cx| rx.poll_recv(cx)).map(Ok).boxed()
}

struct Outside {
    script: Arc<Script>,
    url: String,
    _server: srv::RunningServer,
}

async fn outside(streaming: bool) -> Outside {
    let script = Arc::new(Script::default());
    let port = Arc::new(AtomicU16::new(0));
    let p2 = port.clone();
    let stream_flag = Arc::new(AtomicBool::new(streaming));
    let state = ServerState::new(vec![CallerAuth { id: "pawl".into(), token: AGENT_TOKEN.into() }], move || {
        let mut c = pawl_a2a::card::pawl_card(&pawl_a2a::card::CardInfo { version: "2".into(), base_url: format!("http://127.0.0.1:{}", p2.load(Ordering::SeqCst)), ..Default::default() }, false);
        c.name = "Release bot".into();
        c.capabilities.streaming = Some(stream_flag.load(Ordering::SeqCst));
        c
    });
    let server = srv::serve(script.clone(), state, 0).await.unwrap();
    port.store(server.addr.port(), Ordering::SeqCst);
    Outside { script, url: format!("http://127.0.0.1:{}", server.addr.port()), _server: server }
}

async fn agent_fixture(o: &Outside, rules: Option<Vec<Rule>>) -> Fixture {
    let url = o.url.clone();
    let f = fixture_config(move |c| {
        c.a2a.agents = vec![A2aAgentConfig { id: "helper".into(), name: None, url, token: Some(AGENT_TOKEN.into()) }];
        c.rules = rules;
    })
    .await;
    f.hub.refresh_a2a_cards().await;
    f
}

async fn wait_for(what: &str, mut cond: impl FnMut() -> bool) {
    for _ in 0..250 {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn start_helper(f: &Fixture, prompt: &str) -> Session {
    let v = ack_result(
        f.hub
            .handle(
                ClientMsg::StartSession { id: "s".into(), agent_id: "a2a:helper".into(), cwd: String::new(), prompt: Some(prompt.into()), machine_id: None, confirmed: None },
                &actor(),
            )
            .await,
    );
    serde_json::from_value(v["session"].clone()).unwrap()
}

fn events_of(f: &Fixture, sid: &str) -> Vec<HubEvent> {
    f.hub.store().events(sid, None, 500).unwrap()
}

#[tokio::test]
async fn an_outside_agent_is_listed_with_its_card_and_a_write_only_token() {
    let o = outside(true).await;
    let f = agent_fixture(&o, None).await;
    let a = f.hub.agents().into_iter().find(|a| a.id == "a2a:helper").expect("listed");
    assert!(a.installed);
    assert_eq!(a.name, "Release bot");
    assert_eq!(a.source, AgentSource::A2a);
    let s = f.hub.settings();
    let helper = &s.a2a.agents[0];
    assert!(helper.token_set);
    let card = helper.card.as_ref().expect("a card");
    assert_eq!(card.signature, "unsigned");
    assert_eq!(card.protocol_version, "1.0");
    assert!(card.skills.iter().any(|k| k.id == "delegate"));
    assert!(!serde_json::to_string(&f.hub.get_settings()).unwrap().contains(AGENT_TOKEN), "never read back");
    assert!(f.hub.pets().iter().any(|p| p.agent_id == "a2a:helper"));
}

#[tokio::test]
async fn a_streaming_turn_with_an_approval_card_that_is_answered() {
    let o = outside(true).await;
    let f = agent_fixture(&o, None).await;
    let s = start_helper(&f, "deploy").await;
    assert_eq!(s.source, SessionSource::Owned);
    assert_eq!(s.cwd, o.url);
    wait_for("the approval card", || !f.hub.pending_permissions().is_empty()).await;
    let req = f.hub.pending_permissions().pop().unwrap();
    assert_eq!(req.session_id, s.id);
    assert_eq!(req.tool_call.kind, Some(ToolKind::Other), "other unless the agent says otherwise");
    assert_eq!(req.tool_call.title.as_deref(), Some("Deploy v2 to production?"));
    let opts: Vec<(&str, PermissionOptionKind)> = req.options.iter().map(|o| (o.option_id.as_str(), o.kind)).collect();
    assert_eq!(opts, vec![("approve", PermissionOptionKind::AllowOnce), ("deny", PermissionOptionKind::RejectOnce)]);
    assert_eq!(f.hub.session(&s.id).unwrap().status, SessionStatus::WaitingPermission);
    // The agent's text so far is in the session.
    assert!(events_of(&f, &s.id).iter().any(|e| e.kind == EventType::AgentMessageChunk && e.payload["text"] == "Checking the release..."));
    // The A2A view of our own delegation is waiting too.
    let t = f.hub.tasks().active_task(&s.id).unwrap();
    assert_eq!(t.status.state, TaskState::InputRequired);

    ack_result(f.hub.handle(ClientMsg::ResolvePermission { id: "r".into(), request_id: req.request_id.clone(), option_id: Some("approve".into()), confirmed: None }, &actor()).await);
    wait_for("the end of the turn", || events_of(&f, &s.id).iter().any(|e| e.kind == EventType::Stop)).await;
    let answers = o.script.answers.lock().unwrap().clone();
    assert_eq!(answers[0]["data"], json!({ "decision": "approve" }));
    assert_eq!(answers[0]["meta"][pawl_a2a::STRUCTURED_INPUT_EXT]["id"], "q1", "tagged with the input request id");
    let ev = events_of(&f, &s.id);
    assert!(ev.iter().any(|e| e.kind == EventType::AgentMessageChunk && e.payload["text"] == "Deployed."));
    assert_eq!(ev.iter().find(|e| e.kind == EventType::Stop).unwrap().payload["stopReason"], "end_turn");
    wait_for("done", || f.hub.session(&s.id).unwrap().status == SessionStatus::Done).await;
    assert_eq!(f.hub.a2a_get(&t.id, None, &pawl_a2a::Caller::Local).unwrap().status.state, TaskState::Completed);
    let audit = f.hub.store().audit_log(5).unwrap();
    assert_eq!(audit[0].request_id, req.request_id);
    // The next prompt continues the same remote context.
    f.hub.prompt(&s.id, "hello".into()).await.unwrap();
    wait_for("the second prompt", || o.script.prompts.lock().unwrap().len() == 2).await;
    assert_eq!(o.script.prompts.lock().unwrap()[1].0.as_deref(), Some(CTX));
}

#[tokio::test]
async fn deny_sends_the_reject_value_and_the_agent_refuses() {
    let o = outside(true).await;
    let f = agent_fixture(&o, None).await;
    let s = start_helper(&f, "deploy").await;
    wait_for("the approval card", || !f.hub.pending_permissions().is_empty()).await;
    let req = f.hub.pending_permissions().pop().unwrap();
    ack_result(f.hub.handle(ClientMsg::ResolvePermission { id: "r".into(), request_id: req.request_id.clone(), option_id: Some("deny".into()), confirmed: None }, &actor()).await);
    wait_for("the refusal", || events_of(&f, &s.id).iter().any(|e| e.kind == EventType::Stop)).await;
    assert_eq!(o.script.answers.lock().unwrap()[0]["data"], json!({ "decision": "deny" }));
    assert_eq!(events_of(&f, &s.id).iter().find(|e| e.kind == EventType::Stop).unwrap().payload["stopReason"], "refusal");
}

#[tokio::test]
async fn questions_from_outside_agents_are_never_auto_approved() {
    let o = outside(true).await;
    // Even a rule that auto-allows reads does not answer an outside agent's read question.
    let f = agent_fixture(&o, Some(vec![rule("reads", Some(ToolKind::Read), RuleAction::AutoAllow)])).await;
    let s = start_helper(&f, "deploy-read").await;
    wait_for("the approval card", || !f.hub.pending_permissions().is_empty()).await;
    let req = f.hub.pending_permissions().pop().unwrap();
    assert_eq!(req.tool_call.kind, Some(ToolKind::Read), "the agent's own details name the kind");
    assert_eq!(req.tool_call.title.as_deref(), Some("Read the release notes"));
    assert_eq!(req.options[0].name, "Yes, go");
    assert!(o.script.answers.lock().unwrap().is_empty(), "nothing was answered for the user");
    assert!(f.hub.st().remote_driven.contains(&s.id));
}

#[tokio::test]
async fn cancel_cancels_the_agents_task() {
    let o = outside(true).await;
    let f = agent_fixture(&o, None).await;
    let s = start_helper(&f, "wait").await;
    wait_for("working", || f.hub.session(&s.id).unwrap().status == SessionStatus::Working).await;
    ack_result(f.hub.handle(ClientMsg::Cancel { id: "c".into(), session_id: s.id.clone() }, &actor()).await);
    assert_eq!(o.script.cancels.lock().unwrap().clone(), vec![TASK.to_string()]);
    wait_for("the cancelled stop", || events_of(&f, &s.id).iter().any(|e| e.kind == EventType::Stop && e.payload["stopReason"] == "cancelled")).await;
    // A prompt while a turn runs is refused like ACP's.
    f.hub.prompt(&s.id, "wait".into()).await.unwrap();
    wait_for("working again", || f.hub.session(&s.id).unwrap().status == SessionStatus::Working).await;
    let e = f.hub.prompt(&s.id, "again".into()).await.unwrap_err();
    assert!(format!("{e:#}").contains("already running a prompt"), "{e:#}");
    // Closing the session ends it.
    f.hub.close_session(&s.id).await.unwrap();
    assert!(f.hub.session(&s.id).unwrap().ended_at.is_some());
}

#[tokio::test]
async fn a_non_streaming_agent_answers_in_one_go() {
    let o = outside(false).await;
    let f = agent_fixture(&o, None).await;
    let s = start_helper(&f, "hello").await;
    wait_for("the answer", || events_of(&f, &s.id).iter().any(|e| e.kind == EventType::Stop)).await;
    let ev = events_of(&f, &s.id);
    assert!(ev.iter().any(|e| e.kind == EventType::AgentMessageChunk && e.payload["text"] == "Hi there"), "{ev:?}");
    assert!(ev.iter().any(|e| e.kind == EventType::UserMessage && e.payload["text"] == "hello"));
}

#[tokio::test]
async fn agents_are_validated_and_an_unreachable_one_shows_its_error() {
    let f = fixture(None).await;
    let me = actor();
    let patch = |agents: Vec<A2aAgentPatch>| HubSettingsPatch { a2a: Some(HubSettingsA2aPatch { agents: Some(agents), ..Default::default() }), ..Default::default() };
    let agent = |id: &str, url: &str| A2aAgentPatch { id: id.into(), name: None, url: url.into(), token: None };
    let err = |r: Result<Value>| format!("{:#}", r.unwrap_err());
    assert!(err(f.hub.update_settings(&patch(vec![agent("x", "http://example.com")]), &me).await).starts_with("invalid:a2a.agents[0].url"));
    assert!(err(f.hub.update_settings(&patch(vec![agent("X Y", "https://a.example")]), &me).await).starts_with("invalid:a2a.agents[0].id"));
    assert!(err(f.hub.update_settings(&patch(vec![agent("a", "https://a.example"), agent("a", "https://b.example")]), &me).await).starts_with("invalid:a2a.agents[1].id"));
    let mut bad_token = agent("a", "https://a.example");
    bad_token.token = Some(Some("has space".into()));
    assert!(err(f.hub.update_settings(&patch(vec![bad_token]), &me).await).starts_with("invalid:a2a.agents[0].token"));
    // A port nobody listens on. Port 1 (tcpmux) is never served on a dev or CI machine; a port picked with bind(0)
    // and released can be taken by another test running in parallel, which made this test flaky.
    let port = 1;
    let mut down = agent("down", &format!("http://127.0.0.1:{port}"));
    down.token = Some(Some("t0ken".into()));
    f.hub.update_settings(&patch(vec![down]), &me).await.unwrap();
    f.hub.refresh_a2a_cards().await;
    let s = f.hub.settings();
    assert!(s.a2a.agents[0].token_set);
    assert!(s.a2a.agents[0].error.as_deref().unwrap_or_default().contains("could not reach"), "{:?}", s.a2a.agents[0]);
    assert!(!f.hub.agents().iter().find(|a| a.id == "a2a:down").unwrap().installed);
    let toml = std::fs::read_to_string(f.hub.paths().config.clone()).unwrap();
    assert!(toml.contains("[[a2a.agents]]") && toml.contains("t0ken"));
    // Starting it says why not.
    let e = ack_error(
        f.hub.handle(ClientMsg::StartSession { id: "s".into(), agent_id: "a2a:down".into(), cwd: String::new(), prompt: None, machine_id: None, confirmed: None }, &me).await,
    );
    assert!(e.contains("has no Agent Card"), "{e}");
    // Removing it drops it from the list.
    f.hub.update_settings(&patch(vec![]), &me).await.unwrap();
    f.hub.refresh_a2a_cards().await;
    assert!(!f.hub.agents().iter().any(|a| a.id == "a2a:down"));
}


// ---------------------------------------------------------------- security review fixes

#[tokio::test]
async fn a_hostile_agent_gets_one_card_per_session_and_few_options() {
    let o = outside(true).await;
    let f = agent_fixture(&o, None).await;
    let s = start_helper(&f, "flood").await;
    wait_for("the third question as text", || events_of(&f, &s.id).iter().any(|e| e.kind == EventType::AgentMessageChunk && e.payload["text"] == "Third?")).await;
    let cards: Vec<_> = f.hub.pending_permissions().into_iter().filter(|r| r.session_id == s.id).collect();
    assert_eq!(cards.len(), 1, "one card at a time per session");
    assert_eq!(cards[0].tool_call.title.as_deref(), Some("First?"));
    assert!(events_of(&f, &s.id).iter().any(|e| e.kind == EventType::AgentMessageChunk && e.payload["text"] == "Second?"), "later questions are text");
    // Too many choices: a question, no card.
    let s2 = start_helper(&f, "many").await;
    wait_for("the question", || events_of(&f, &s2.id).iter().any(|e| e.kind == EventType::AgentMessageChunk && e.payload["text"].as_str().unwrap_or("").contains("9 choices"))).await;
    assert!(!f.hub.pending_permissions().iter().any(|r| r.session_id == s2.id));
}

#[tokio::test]
async fn an_agent_that_moves_host_needs_its_token_again() {
    let f = fixture(None).await;
    let me = actor();
    let patch = |url: &str, token: Option<Option<&str>>| HubSettingsPatch {
        a2a: Some(HubSettingsA2aPatch {
            agents: Some(vec![A2aAgentPatch { id: "bot".into(), name: None, url: url.into(), token: token.map(|t| t.map(str::to_string)) }]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let token = |f: &Fixture| f.hub.config().a2a.agents[0].token.clone();
    f.hub.update_settings(&patch("https://agent.example/a2a", Some(Some("s3cret"))), &me).await.unwrap();
    // Same origin, another path: the token stays.
    f.hub.update_settings(&patch("https://agent.example/v2/", None), &me).await.unwrap();
    assert_eq!(token(&f).as_deref(), Some("s3cret"));
    // Another host (or port or scheme) without a new token: refused, nothing changed.
    for moved in ["https://other.example/a2a", "https://agent.example:8443/a2a"] {
        let e = format!("{:#}", f.hub.update_settings(&patch(moved, None), &me).await.unwrap_err());
        assert!(e.starts_with("invalid:a2a.agents[0].token") && e.contains("another host"), "{e}");
        assert_eq!(f.hub.config().a2a.agents[0].url, "https://agent.example/v2/");
    }
    // With a new token, or an explicit clear, it moves.
    f.hub.update_settings(&patch("https://other.example/a2a", Some(Some("n3w"))), &me).await.unwrap();
    assert_eq!(token(&f).as_deref(), Some("n3w"));
    f.hub.update_settings(&patch("https://third.example/", Some(None)), &me).await.unwrap();
    assert_eq!(token(&f), None);
    // No token before: moving needs nothing.
    f.hub.update_settings(&patch("https://fourth.example/", None), &me).await.unwrap();
}

#[tokio::test]
async fn settings_are_pushed_after_a_card_fetch() {
    let o = outside(true).await;
    let mut f = agent_fixture(&o, None).await;
    drain(&mut f.rx);
    f.hub.refresh_a2a_cards().await;
    let pushed = drain(&mut f.rx).into_iter().find_map(|m| match m {
        ServerMsg::Settings { settings, .. } => Some(settings),
        _ => None,
    });
    let settings = pushed.expect("a settings message");
    assert!(settings.a2a.agents[0].card.is_some());
}

#[tokio::test]
async fn pawl_added_as_its_own_outside_agent_answers_a_plain_prompt() {
    let f = fixture(None).await;
    let me = actor();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let callers = vec![pawl_protocol::A2aCallerPatch { id: "me".into(), name: "Myself".into(), allow_approvals: false }];
    f.hub.update_settings(&HubSettingsPatch { a2a: Some(HubSettingsA2aPatch { server: Some(true), port: Some(port), callers: Some(callers), agents: None }), ..Default::default() }, &me).await.unwrap();
    let token = f.hub.a2a_rotate_token("me", &me).await.unwrap()["token"].as_str().unwrap().to_string();
    let agents = vec![A2aAgentPatch { id: "self".into(), name: None, url: format!("http://127.0.0.1:{port}"), token: Some(Some(token)) }];
    f.hub.update_settings(&HubSettingsPatch { a2a: Some(HubSettingsA2aPatch { agents: Some(agents), ..Default::default() }), ..Default::default() }, &me).await.unwrap();
    f.hub.refresh_a2a_cards().await;
    let v = ack_result(
        f.hub
            .handle(ClientMsg::StartSession { id: "s".into(), agent_id: "a2a:self".into(), cwd: String::new(), prompt: Some("What's everyone doing?".into()), machine_id: None, confirmed: None }, &me)
            .await,
    );
    let s: Session = serde_json::from_value(v["session"].clone()).unwrap();
    wait_for("the answer", || events_of(&f, &s.id).iter().any(|e| e.kind == EventType::Stop)).await;
    let ev = events_of(&f, &s.id);
    assert!(ev.iter().any(|e| e.kind == EventType::AgentMessageChunk && e.payload["text"].as_str().unwrap_or("").contains("delegate metadata")), "{ev:?}");
    assert!(!ev.iter().any(|e| e.kind == EventType::Error), "{ev:?}");
    assert_eq!(f.hub.a2a_get(&f.hub.tasks().latest_per_context()[0].id, None, &pawl_a2a::Caller::Local).unwrap().status.state, TaskState::Completed);
    f.hub.stop_a2a_server().await;
}
