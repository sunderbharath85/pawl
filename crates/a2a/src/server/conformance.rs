//! Conformance with the request / response examples of the A2A 1.0.1 specification
//! (https://github.com/a2aproject/A2A/blob/v1.0.1/docs/specification.md): section 6 (workflows, over
//! HTTP+JSON), 9.4 (JSON-RPC methods), 9.5 (JSON-RPC errors) and 11.6 (HTTP errors). Each request is
//! the spec's, sent through the real router; the handler answers with the spec's response objects
//! and the test checks what reaches the client.

use std::sync::Mutex;

use super::*;
use a2a::{Artifact, Message, Part, Role, TaskArtifactUpdateEvent, TaskState, TaskStatus, TaskStatusUpdateEvent};
use axum::body::Body;
use axum::http::Request as HttpRequest;
use serde_json::{json, Value};
use tower::ServiceExt;

const PORT: u16 = 47997;
const TOKEN: &str = "token";

/// Records what each operation received (as JSON) and answers with the spec's examples.
#[derive(Default)]
struct Spec {
    got: Mutex<Vec<(&'static str, Value)>>,
}

fn status(state: TaskState, text: Option<&str>) -> TaskStatus {
    TaskStatus {
        state,
        message: text.map(|t| {
            let mut m = Message::new(Role::Agent, vec![Part::text(t)]);
            m.message_id = "msg-status-update".into();
            m
        }),
        timestamp: None,
    }
}

fn task(id: &str, ctx: &str, s: TaskStatus) -> Task {
    Task { id: id.into(), context_id: ctx.into(), status: s, artifacts: None, history: None, metadata: None }
}

impl Spec {
    fn record<T: serde::Serialize>(&self, op: &'static str, req: &T) {
        self.got.lock().unwrap().push((op, serde_json::to_value(req).unwrap()));
    }

    fn last(&self) -> (&'static str, Value) {
        self.got.lock().unwrap().last().cloned().expect("a call")
    }
}

#[async_trait::async_trait]
impl A2aHandler for Spec {
    async fn send_message(&self, _p: &ServiceParams, req: SendMessageRequest) -> Result<SendMessageResponse, A2AError> {
        self.record("SendMessage", &req);
        // 6.1: the completed weather task; 6.3: the flight booking that needs input.
        if req.message.text() == Some("Book me a flight") {
            return Ok(SendMessageResponse::Task(task(
                "task-uuid",
                "context-uuid",
                status(TaskState::InputRequired, Some("I need more details. Where would you like to fly from and to?")),
            )));
        }
        let mut t = task("task-uuid", "context-uuid", status(TaskState::Completed, None));
        t.artifacts = Some(vec![Artifact {
            artifact_id: "artifact-uuid".into(),
            name: Some("Weather Report".into()),
            description: None,
            parts: vec![Part::text("Today will be sunny with a high of 75°F")],
            metadata: None,
            extensions: None,
        }]);
        Ok(SendMessageResponse::Task(t))
    }

    async fn send_streaming_message(&self, _p: &ServiceParams, req: SendMessageRequest) -> Result<EventStream, A2AError> {
        self.record("SendStreamingMessage", &req);
        Ok(Self::report_stream())
    }

    async fn get_task(&self, _p: &ServiceParams, req: GetTaskRequest) -> Result<Task, A2AError> {
        self.record("GetTask", &req);
        if req.id == "nonexistent-task-id" || req.id == "task-123" {
            return Err(A2AError::task_not_found(&req.id));
        }
        Ok(task(&req.id, "context-uuid", status(TaskState::Working, None)))
    }

    async fn list_tasks(&self, _p: &ServiceParams, req: ListTasksRequest) -> Result<ListTasksResponse, A2AError> {
        self.record("ListTasks", &req);
        let mut t = task("3f36680c-7f37-4a5f-945e-d78981fafd36", "c295ea44-7543-4f78-b524-7a38915ad6e4", status(TaskState::Completed, None));
        t.status.timestamp = Some(chrono::DateTime::parse_from_rfc3339("2024-03-15T10:15:00Z").unwrap().with_timezone(&chrono::Utc));
        Ok(ListTasksResponse { tasks: vec![t], next_page_token: String::new(), page_size: 10, total_size: 5 })
    }

    async fn cancel_task(&self, _p: &ServiceParams, req: CancelTaskRequest) -> Result<Task, A2AError> {
        self.record("CancelTask", &req);
        Ok(task(&req.id, "context-uuid", status(TaskState::Canceled, None)))
    }

    async fn subscribe_to_task(&self, _p: &ServiceParams, req: SubscribeToTaskRequest) -> Result<EventStream, A2AError> {
        self.record("SubscribeToTask", &req);
        if req.id == "finished-task" {
            return Err(A2AError::unsupported_operation("the task has finished"));
        }
        Ok(Self::report_stream())
    }

    async fn get_extended_agent_card(&self, _p: &ServiceParams, req: GetExtendedAgentCardRequest) -> Result<AgentCard, A2AError> {
        self.record("GetExtendedAgentCard", &req);
        let mut c = card::pawl_card(&card::CardInfo { version: "1".into(), base_url: format!("http://127.0.0.1:{PORT}"), ..Default::default() }, true);
        c.name = "Extended Agent with Additional Skills".into();
        Ok(c)
    }
}

impl Spec {
    /// 6.2: task working, a report chunk, completed.
    fn report_stream() -> EventStream {
        let events = vec![
            Ok(StreamResponse::Task(task("task-uuid", "context-uuid", status(TaskState::Working, None)))),
            Ok(StreamResponse::ArtifactUpdate(TaskArtifactUpdateEvent {
                task_id: "task-uuid".into(),
                context_id: "context-uuid".into(),
                artifact: Artifact { artifact_id: "a".into(), name: None, description: None, parts: vec![Part::text("# Climate Change Report\n\n")], metadata: None, extensions: None },
                append: None,
                last_chunk: None,
                metadata: None,
            })),
            Ok(StreamResponse::StatusUpdate(TaskStatusUpdateEvent { task_id: "task-uuid".into(), context_id: "context-uuid".into(), status: status(TaskState::Completed, None), metadata: None })),
        ];
        futures::stream::iter(events).boxed()
    }
}

fn app() -> (Router, Arc<Spec>) {
    let spec = Arc::new(Spec::default());
    let state = ServerState::new(vec![CallerAuth { id: "c".into(), token: TOKEN.into() }], || card::pawl_card(&card::CardInfo::default(), false));
    (router_for_port(spec.clone(), state, PORT), spec)
}

struct Reply {
    status: StatusCode,
    content_type: String,
    body: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or_else(|e| panic!("{e}: {}", self.body))
    }

    /// The `data:` payloads of an SSE body.
    fn events(&self) -> Vec<Value> {
        self.body.lines().filter_map(|l| l.strip_prefix("data:")).map(|d| serde_json::from_str(d.trim()).unwrap()).collect()
    }
}

/// Send `method path` with the spec's headers (plus the loopback Host this server needs).
async fn http(app: &Router, method: &str, path: &str, version: Option<&str>, body: Option<Value>) -> Reply {
    let mut b = HttpRequest::builder().method(method).uri(path).header("host", format!("127.0.0.1:{PORT}")).header("authorization", format!("Bearer {TOKEN}"));
    if let Some(v) = version {
        b = b.header("a2a-version", v);
    }
    let body = match body {
        Some(v) => {
            b = b.header("content-type", "application/a2a+json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let resp = app.clone().oneshot(b.body(body).unwrap()).await.unwrap();
    let status = resp.status();
    let content_type = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 22).await.unwrap();
    Reply { status, content_type, body: String::from_utf8_lossy(&bytes).into_owned() }
}

async fn rpc(app: &Router, request: Value) -> Reply {
    http(app, "POST", "/", Some("1.0"), Some(request)).await
}

// ---------------------------------------------------------------- section 6, HTTP+JSON

#[tokio::test]
async fn spec_6_1_basic_task_execution() {
    let (app, spec) = app();
    let request = json!({ "message": { "role": "ROLE_USER", "parts": [{ "text": "What is the weather today?" }], "messageId": "msg-uuid" } });
    let r = http(&app, "POST", "/message:send", None, Some(request.clone())).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.content_type, "application/a2a+json");
    assert_eq!(
        r.json(),
        json!({ "task": {
            "id": "task-uuid",
            "contextId": "context-uuid",
            "status": { "state": "TASK_STATE_COMPLETED" },
            "artifacts": [{ "artifactId": "artifact-uuid", "name": "Weather Report", "parts": [{ "text": "Today will be sunny with a high of 75°F" }] }]
        }})
    );
    assert_eq!(spec.last(), ("SendMessage", request));
}

#[tokio::test]
async fn spec_6_2_streaming_task_execution() {
    let (app, spec) = app();
    let request = json!({ "message": { "role": "ROLE_USER", "parts": [{ "text": "Write a detailed report on climate change" }], "messageId": "msg-uuid" } });
    let r = http(&app, "POST", "/message:stream", None, Some(request)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.content_type.starts_with("text/event-stream"), "{}", r.content_type);
    let ev = r.events();
    assert_eq!(ev.len(), 3);
    assert_eq!(ev[0]["task"]["status"]["state"], "TASK_STATE_WORKING");
    assert_eq!(ev[1]["artifactUpdate"]["taskId"], "task-uuid");
    assert_eq!(ev[1]["artifactUpdate"]["artifact"]["parts"][0]["text"], "# Climate Change Report\n\n");
    assert_eq!(ev[2]["statusUpdate"]["status"]["state"], "TASK_STATE_COMPLETED");
    assert_eq!(spec.last().0, "SendStreamingMessage");
}

#[tokio::test]
async fn spec_6_3_multi_turn_interaction() {
    let (app, spec) = app();
    let first = json!({ "message": { "role": "ROLE_USER", "parts": [{ "text": "Book me a flight" }], "messageId": "msg-1" } });
    let r = http(&app, "POST", "/message:send", None, Some(first)).await;
    let t = r.json();
    assert_eq!(t["task"]["status"]["state"], "TASK_STATE_INPUT_REQUIRED");
    assert_eq!(t["task"]["status"]["message"]["role"], "ROLE_AGENT");
    assert_eq!(t["task"]["status"]["message"]["parts"][0]["text"], "I need more details. Where would you like to fly from and to?");
    let follow = json!({ "message": { "taskId": "task-uuid", "role": "ROLE_USER", "parts": [{ "text": "From San Francisco to New York" }], "messageId": "msg-2" } });
    http(&app, "POST", "/message:send", None, Some(follow.clone())).await;
    assert_eq!(spec.last(), ("SendMessage", follow), "the follow-up reaches the agent with its taskId");
}

#[tokio::test]
async fn spec_6_4_version_negotiation_error() {
    let (app, spec) = app();
    let request = json!({ "message": { "role": "ROLE_USER", "parts": [{ "text": "Hello" }], "messageId": "msg-uuid" } });
    let r = http(&app, "POST", "/message:send", Some("0.5"), Some(request)).await;
    // The example shows a problem+json body; section 11.6 (normative for the binding) has the
    // google.rpc.Status shape, which is what Pawl sends: 400 with reason VERSION_NOT_SUPPORTED.
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let e = r.json();
    assert_eq!(e["error"]["code"], 400);
    assert_eq!(e["error"]["details"][0]["reason"], "VERSION_NOT_SUPPORTED");
    assert!(e["error"]["message"].as_str().unwrap().contains("0.5"));
    // JSON-RPC: -32009.
    let r = http(&app, "POST", "/", Some("0.5"), Some(json!({ "jsonrpc": "2.0", "id": 1, "method": "SendMessage", "params": {} }))).await;
    assert_eq!(r.json()["error"]["code"], -32009);
    assert!(spec.got.lock().unwrap().is_empty());
}

#[tokio::test]
async fn spec_6_5_task_listing() {
    let (app, spec) = app();
    let r = http(&app, "GET", "/tasks?contextId=c295ea44-7543-4f78-b524-7a38915ad6e4&pageSize=10&historyLength=3", None, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json(),
        json!({
            "tasks": [{ "id": "3f36680c-7f37-4a5f-945e-d78981fafd36", "contextId": "c295ea44-7543-4f78-b524-7a38915ad6e4", "status": { "state": "TASK_STATE_COMPLETED", "timestamp": "2024-03-15T10:15:00Z" } }],
            "totalSize": 5,
            "pageSize": 10,
            "nextPageToken": ""
        })
    );
    assert_eq!(spec.last(), ("ListTasks", json!({ "contextId": "c295ea44-7543-4f78-b524-7a38915ad6e4", "pageSize": 10, "historyLength": 3 })));
    http(&app, "GET", "/tasks?status=TASK_STATE_WORKING&pageSize=20", None, None).await;
    assert_eq!(spec.last().1, json!({ "status": "TASK_STATE_WORKING", "pageSize": 20 }));
    http(&app, "GET", "/tasks?contextId=c295ea44-7543-4f78-b524-7a38915ad6e4&pageSize=10&pageToken=base64-encoded-cursor-token", None, None).await;
    assert_eq!(spec.last().1["pageToken"], "base64-encoded-cursor-token");
    // The validation example: an unknown status is refused before the agent sees it.
    let calls = spec.got.lock().unwrap().len();
    let r = http(&app, "GET", "/tasks?pageSize=150&historyLength=-5&status=TASK_STATE_RUNNING", None, None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["error"]["status"], "INVALID_ARGUMENT");
    assert_eq!(spec.got.lock().unwrap().len(), calls);
}

#[tokio::test]
async fn spec_6_9_extended_agent_card() {
    let (app, _) = app();
    let r = http(&app, "GET", "/extendedAgentCard", None, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.content_type, "application/a2a+json");
    assert_eq!(r.json()["name"], "Extended Agent with Additional Skills");
    assert!(r.json()["skills"].as_array().is_some());
    // The public card says an extended card exists.
    let r = http(&app, "GET", WELL_KNOWN_CARD, Some("1.0"), None).await;
    assert_eq!(r.json()["capabilities"]["extendedAgentCard"], true);
}

// ---------------------------------------------------------------- 9.4 JSON-RPC methods

#[tokio::test]
async fn spec_9_4_json_rpc_methods() {
    let (app, spec) = app();
    // 9.4.1 SendMessage
    let r = rpc(&app, json!({ "jsonrpc": "2.0", "id": 1, "method": "SendMessage", "params": { "message": { "role": "ROLE_USER", "parts": [{ "text": "What is the weather today?" }], "messageId": "msg-uuid" } } })).await;
    let v = r.json();
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["id"], 1);
    assert_eq!(v["result"]["task"]["status"]["state"], "TASK_STATE_COMPLETED");
    // 9.4.2 SendStreamingMessage: SSE of JSON-RPC responses with the request id.
    let r = rpc(&app, json!({ "jsonrpc": "2.0", "id": 1, "method": "SendStreamingMessage", "params": { "message": { "role": "ROLE_USER", "parts": [{ "text": "x" }], "messageId": "m" } } })).await;
    assert!(r.content_type.starts_with("text/event-stream"));
    let ev = r.events();
    assert_eq!(ev.len(), 3);
    assert!(ev.iter().all(|e| e["jsonrpc"] == "2.0" && e["id"] == 1));
    assert!(ev[0]["result"].get("task").is_some());
    assert!(ev[2]["result"].get("statusUpdate").is_some());
    // 9.4.3 GetTask
    let v = rpc(&app, json!({ "jsonrpc": "2.0", "id": 2, "method": "GetTask", "params": { "id": "task-uuid", "historyLength": 10 } })).await.json();
    assert_eq!(v["id"], 2);
    assert_eq!(v["result"]["id"], "task-uuid");
    assert_eq!(spec.last(), ("GetTask", json!({ "id": "task-uuid", "historyLength": 10 })));
    // 9.4.4 ListTasks
    rpc(&app, json!({ "jsonrpc": "2.0", "id": 3, "method": "ListTasks", "params": { "contextId": "context-uuid", "status": "TASK_STATE_WORKING", "pageSize": 50, "pageToken": "cursor-token" } })).await;
    assert_eq!(spec.last(), ("ListTasks", json!({ "contextId": "context-uuid", "status": "TASK_STATE_WORKING", "pageSize": 50, "pageToken": "cursor-token" })));
    // 9.4.5 CancelTask
    let v = rpc(&app, json!({ "jsonrpc": "2.0", "id": 4, "method": "CancelTask", "params": { "id": "task-uuid" } })).await.json();
    assert_eq!(v["result"]["status"]["state"], "TASK_STATE_CANCELED");
    // 9.4.6 SubscribeToTask; a terminal task is UnsupportedOperation.
    let r = rpc(&app, json!({ "jsonrpc": "2.0", "id": 5, "method": "SubscribeToTask", "params": { "id": "task-uuid" } })).await;
    assert_eq!(r.events().len(), 3);
    let v = rpc(&app, json!({ "jsonrpc": "2.0", "id": 5, "method": "SubscribeToTask", "params": { "id": "finished-task" } })).await.json();
    assert_eq!(v["error"]["code"], -32004);
    // 9.4.7 push configuration: not supported.
    for method in ["CreateTaskPushNotificationConfig", "GetTaskPushNotificationConfig", "ListTaskPushNotificationConfigs", "DeleteTaskPushNotificationConfig"] {
        let v = rpc(&app, json!({ "jsonrpc": "2.0", "id": 9, "method": method, "params": { "taskId": "t", "id": "c" } })).await.json();
        assert_eq!(v["error"]["code"], -32003, "{method}");
    }
    // 9.4.8 GetExtendedAgentCard (no params).
    let v = rpc(&app, json!({ "jsonrpc": "2.0", "id": 6, "method": "GetExtendedAgentCard" })).await.json();
    assert_eq!(v["result"]["name"], "Extended Agent with Additional Skills");
}

// ---------------------------------------------------------------- 9.5 / 11.6 errors

async fn raw_rpc(app: &Router, body: &'static str) -> Value {
    let b = HttpRequest::builder().method("POST").uri("/").header("host", format!("127.0.0.1:{PORT}")).header("authorization", format!("Bearer {TOKEN}")).header("a2a-version", "1.0");
    let resp = app.clone().oneshot(b.body(Body::from(body)).unwrap()).await.unwrap();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    serde_json::from_slice::<Value>(&bytes).unwrap()
}

#[tokio::test]
async fn spec_9_5_json_rpc_errors() {
    let (app, _) = app();
    let raw = |body: &'static str| raw_rpc(&app, body);
    assert_eq!(raw("{not json").await["error"]["code"], -32700);
    assert_eq!(raw(r#"{"jsonrpc":"1.0","id":1,"method":"GetTask","params":{"id":"x"}}"#).await["error"]["code"], -32600);
    assert_eq!(raw(r#"{"jsonrpc":"2.0","method":"GetTask","params":{"id":"x"}}"#).await["error"]["code"], -32600, "an id is required");
    assert_eq!(raw(r#"[1,2]"#).await["error"]["code"], -32600);
    assert_eq!(raw(r#"{"jsonrpc":"2.0","id":1,"method":"tasks/get","params":{"id":"x"}}"#).await["error"]["code"], -32601, "0.3 names are not 1.0 methods");
    assert_eq!(raw(r#"{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{}}"#).await["error"]["code"], -32602, "a message is required");
    assert_eq!(raw(r#"{"jsonrpc":"2.0","id":1,"method":"GetTask","params":[1]}"#).await["error"]["code"], -32602);
    // The A2A-specific example: TaskNotFound with an ErrorInfo detail.
    let v = raw(r#"{"jsonrpc":"2.0","id":2,"method":"GetTask","params":{"id":"nonexistent-task-id"}}"#).await;
    assert_eq!(v["id"], 2);
    assert_eq!(v["error"]["code"], -32001);
    let info = &v["error"]["data"][0];
    assert_eq!(info["@type"], "type.googleapis.com/google.rpc.ErrorInfo");
    assert_eq!(info["reason"], "TASK_NOT_FOUND");
    assert_eq!(info["domain"], "a2a-protocol.org");
    assert!(info["metadata"]["timestamp"].is_string());
}

#[tokio::test]
async fn spec_11_6_http_error_shape() {
    let (app, _) = app();
    let r = http(&app, "GET", "/tasks/task-123", Some("1.0"), None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.content_type, "application/a2a+json");
    let e = r.json();
    assert_eq!(e["error"]["code"], 404);
    assert_eq!(e["error"]["status"], "NOT_FOUND");
    assert_eq!(e["error"]["details"][0]["@type"], "type.googleapis.com/google.rpc.ErrorInfo");
    assert_eq!(e["error"]["details"][0]["reason"], "TASK_NOT_FOUND");
    assert_eq!(e["error"]["details"][0]["domain"], "a2a-protocol.org");
    // Cancel and subscribe actions on global ids (with their own ':').
    let r = http(&app, "POST", "/tasks/m:t1:cancel", None, None).await;
    assert_eq!(r.json()["id"], "m:t1");
    let r = http(&app, "POST", "/tasks/m:t1:subscribe", None, None).await;
    assert_eq!(r.events().len(), 3);
    let r = http(&app, "POST", "/tasks/m:t1:pause", None, None).await;
    assert_eq!(r.status, StatusCode::NOT_IMPLEMENTED);
    let r = http(&app, "POST", "/tasks/t/pushNotificationConfigs", None, Some(json!({ "url": "https://x" }))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["error"]["details"][0]["reason"], "PUSH_NOTIFICATION_NOT_SUPPORTED");
    let r = http(&app, "POST", "/message:send", None, Some(json!({ "nope": 1 }))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(r.json()["error"]["status"], "INVALID_ARGUMENT");
}
