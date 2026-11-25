use super::*;
use a2a::{
    DeleteTaskPushNotificationConfigRequest, GetTaskPushNotificationConfigRequest, ListTaskPushNotificationConfigsRequest,
    ListTaskPushNotificationConfigsResponse, ListTasksResponse, Message, Part, Role, SendMessageResponse, StreamResponse, Task, TaskPushNotificationConfig,
    TaskState, TaskStatus, TaskStatusUpdateEvent,
};
use crate::async_trait;
use axum::body::Body;
use axum::http::Request as HttpRequest;

const PORT: u16 = 47999;
const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

/// Records the caller each call saw; answers with fixed shapes.
#[derive(Default)]
struct Fake {
    seen: Mutex<Vec<(String, Option<String>, bool)>>,
}

fn task(state: TaskState) -> Task {
    Task {
        id: "m:t1".into(),
        context_id: "m:s1".into(),
        status: TaskStatus { state, message: Some(Message::new(Role::Agent, vec![Part::text("hi")])), timestamp: None },
        artifacts: None,
        history: None,
        metadata: None,
    }
}

impl Fake {
    fn record(&self, method: &str, params: &ServiceParams) {
        self.seen.lock().unwrap().push((method.into(), caller_of(params).map(str::to_string), params.contains_key("authorization")));
    }
}

#[async_trait]
impl RequestHandler for Fake {
    async fn send_message(&self, params: &ServiceParams, req: SendMessageRequest) -> Result<SendMessageResponse, A2AError> {
        self.record("send_message", params);
        let mut t = task(TaskState::Completed);
        t.history = Some(vec![req.message]);
        Ok(SendMessageResponse::Task(t))
    }
    async fn send_streaming_message(&self, params: &ServiceParams, _req: SendMessageRequest) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        self.record("send_streaming_message", params);
        let events = vec![
            Ok(StreamResponse::Task(task(TaskState::Working))),
            Ok(StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
                task_id: "m:t1".into(),
                context_id: "m:s1".into(),
                status: TaskStatus { state: TaskState::Completed, message: None, timestamp: None },
                metadata: None,
            })),
        ];
        Ok(futures::stream::iter(events).boxed())
    }
    async fn get_task(&self, params: &ServiceParams, req: GetTaskRequest) -> Result<Task, A2AError> {
        self.record("get_task", params);
        if req.id == "missing" {
            return Err(A2AError::task_not_found(&req.id));
        }
        Ok(task(TaskState::InputRequired))
    }
    async fn list_tasks(&self, params: &ServiceParams, _req: ListTasksRequest) -> Result<ListTasksResponse, A2AError> {
        self.record("list_tasks", params);
        Ok(ListTasksResponse { tasks: vec![task(TaskState::Working)], next_page_token: String::new(), page_size: 1, total_size: 1 })
    }
    async fn cancel_task(&self, params: &ServiceParams, _req: CancelTaskRequest) -> Result<Task, A2AError> {
        self.record("cancel_task", params);
        Ok(task(TaskState::Canceled))
    }
    async fn subscribe_to_task(&self, params: &ServiceParams, _req: SubscribeToTaskRequest) -> Result<BoxStream<'static, Result<StreamResponse, A2AError>>, A2AError> {
        self.record("subscribe_to_task", params);
        Ok(futures::stream::iter(vec![Ok(StreamResponse::Task(task(TaskState::Working)))]).boxed())
    }
    async fn create_push_config(&self, _p: &ServiceParams, _r: TaskPushNotificationConfig) -> Result<TaskPushNotificationConfig, A2AError> {
        Err(A2AError::push_notification_not_supported())
    }
    async fn get_push_config(&self, _p: &ServiceParams, _r: GetTaskPushNotificationConfigRequest) -> Result<TaskPushNotificationConfig, A2AError> {
        Err(A2AError::push_notification_not_supported())
    }
    async fn list_push_configs(&self, _p: &ServiceParams, _r: ListTaskPushNotificationConfigsRequest) -> Result<ListTaskPushNotificationConfigsResponse, A2AError> {
        Err(A2AError::push_notification_not_supported())
    }
    async fn delete_push_config(&self, _p: &ServiceParams, _r: DeleteTaskPushNotificationConfigRequest) -> Result<(), A2AError> {
        Err(A2AError::push_notification_not_supported())
    }
    async fn get_extended_agent_card(&self, params: &ServiceParams, _r: GetExtendedAgentCardRequest) -> Result<AgentCard, A2AError> {
        self.record("get_extended_agent_card", params);
        Ok(card::pawl_card(&info(), true))
    }
}

fn info() -> card::CardInfo {
    card::CardInfo { version: "0.1.1".into(), base_url: format!("http://127.0.0.1:{PORT}"), agents: vec![("codex".into(), "Codex".into())], repos: vec![] }
}

fn app() -> (Router, Arc<Fake>, Arc<ServerState>) {
    let fake = Arc::new(Fake::default());
    let state = ServerState::new(vec![CallerAuth { id: "ci".into(), token: TOKEN.into() }], || card::pawl_card(&info(), false));
    (router_for_port(fake.clone(), state.clone(), PORT), fake, state)
}

fn req(method: &str, path: &str) -> axum::http::request::Builder {
    HttpRequest::builder().method(method).uri(path).header("host", format!("127.0.0.1:{PORT}"))
}

fn rpc(method: &str, params: Value) -> Body {
    Body::from(json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params }).to_string())
}

async fn send(app: &Router, r: HttpRequest<Body>) -> (StatusCode, HeaderMap, String) {
    let resp = app.clone().oneshot(r).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let body = axum::body::to_bytes(resp.into_body(), 4 * BODY_MAX_BYTES).await.unwrap();
    (status, headers, String::from_utf8_lossy(&body).into_owned())
}

fn authed(b: axum::http::request::Builder) -> axum::http::request::Builder {
    b.header("authorization", format!("Bearer {TOKEN}")).header("content-type", "application/json")
}

fn v1_message() -> Value {
    json!({ "message": { "messageId": "m1", "role": "ROLE_USER", "parts": [{ "text": "hello" }] } })
}

fn v03_message() -> Value {
    json!({ "message": { "kind": "message", "messageId": "m1", "role": "user", "parts": [{ "kind": "text", "text": "hello" }] } })
}

#[tokio::test]
async fn a_dns_rebinding_host_is_refused_even_with_a_token() {
    let (app, fake, _) = app();
    for host in ["evil.example:47999", "attacker.test", "127.0.0.1:1", "localhost", "127.0.0.1.nip.io:47999", "[::2]:47999"] {
        let r = authed(HttpRequest::builder().method("POST").uri("/").header("host", host)).body(rpc("SendMessage", v1_message())).unwrap();
        let (status, _, _) = send(&app, r).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{host}");
    }
    let r = HttpRequest::builder().method("GET").uri(WELL_KNOWN_CARD).header("host", "rebind.example:47999").body(Body::empty()).unwrap();
    assert_eq!(send(&app, r).await.0, StatusCode::FORBIDDEN, "the card too");
    assert!(fake.seen.lock().unwrap().is_empty());
    // The loopback names with the right port pass.
    for host in ["127.0.0.1:47999", "localhost:47999", "LOCALHOST:47999", "[::1]:47999"] {
        let r = authed(HttpRequest::builder().method("POST").uri("/").header("host", host).header("a2a-version", "1.0")).body(rpc("SendMessage", v1_message())).unwrap();
        assert_eq!(send(&app, r).await.0, StatusCode::OK, "{host}");
    }
}

#[tokio::test]
async fn browser_origins_and_preflights_are_refused() {
    let (app, _, _) = app();
    for origin in ["https://evil.example", "http://localhost:3000", "null", "http://127.0.0.1:47999.evil"] {
        let r = authed(req("POST", "/").header("origin", origin)).body(rpc("SendMessage", v1_message())).unwrap();
        assert_eq!(send(&app, r).await.0, StatusCode::FORBIDDEN, "{origin}");
    }
    let r = req("OPTIONS", "/").header("origin", "https://evil.example").body(Body::empty()).unwrap();
    let (status, headers, _) = send(&app, r).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(headers.get("access-control-allow-origin").is_none());
    // Its own origin is fine.
    let r = authed(req("POST", "/").header("origin", "http://127.0.0.1:47999").header("a2a-version", "1.0")).body(rpc("SendMessage", v1_message())).unwrap();
    assert_eq!(send(&app, r).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_missing_or_wrong_token_is_401_and_never_reaches_the_handler() {
    let (app, fake, _) = app();
    let none = req("POST", "/").header("a2a-version", "1.0").body(rpc("SendMessage", v1_message())).unwrap();
    let (status, headers, _) = send(&app, none).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers.get("www-authenticate").unwrap(), "Bearer realm=\"pawl\"");
    let wrong = req("POST", "/").header("authorization", format!("Bearer {}", "f".repeat(64))).body(rpc("SendMessage", v1_message())).unwrap();
    assert_eq!(send(&app, wrong).await.0, StatusCode::UNAUTHORIZED);
    let prefix = req("POST", "/").header("authorization", format!("Bearer {}", &TOKEN[..32])).body(rpc("SendMessage", v1_message())).unwrap();
    assert_eq!(send(&app, prefix).await.0, StatusCode::UNAUTHORIZED, "a prefix of the token is wrong");
    let rest = req("GET", "/tasks/m:t1").body(Body::empty()).unwrap();
    assert_eq!(send(&app, rest).await.0, StatusCode::UNAUTHORIZED, "REST too");
    assert!(fake.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_handler_learns_the_caller_from_the_guard_only() {
    let (app, fake, state) = app();
    let r = authed(req("POST", "/").header("a2a-version", "1.0").header(CALLER_HEADER, "admin")).body(rpc("SendMessage", v1_message())).unwrap();
    let (status, _, body) = send(&app, r).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let seen = fake.seen.lock().unwrap().clone();
    assert_eq!(seen, vec![("send_message".to_string(), Some("ci".to_string()), false)], "spoofed caller dropped, token removed");
    // A rotated token stops working at once.
    state.set_callers(vec![CallerAuth { id: "ci".into(), token: "new-token".into() }]);
    let r = authed(req("POST", "/").header("a2a-version", "1.0")).body(rpc("SendMessage", v1_message())).unwrap();
    assert_eq!(send(&app, r).await.0, StatusCode::UNAUTHORIZED);
    // A caller without a token can never be matched by an empty bearer.
    state.set_callers(vec![CallerAuth { id: "ci".into(), token: String::new() }]);
    let r = req("POST", "/").header("authorization", "Bearer ").body(rpc("SendMessage", v1_message())).unwrap();
    assert_eq!(send(&app, r).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_1_0_client_gets_1_0_shapes() {
    let (app, _, _) = app();
    let r = authed(req("POST", "/").header("a2a-version", "1.0")).body(rpc("SendMessage", v1_message())).unwrap();
    let (_, _, body) = send(&app, r).await;
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["id"], 7);
    assert_eq!(v["result"]["task"]["status"]["state"], "TASK_STATE_COMPLETED", "{body}");
    // The version may come as a query parameter (spec 3.6.1).
    let r = authed(req("POST", "/?A2A-Version=1.0")).body(rpc("GetTask", json!({ "id": "m:t1" }))).unwrap();
    let v: Value = serde_json::from_str(&send(&app, r).await.2).unwrap();
    assert_eq!(v["result"]["status"]["state"], "TASK_STATE_INPUT_REQUIRED");
    // REST (HTTP+JSON) is 1.0.
    let r = authed(req("GET", "/tasks/m:t1")).body(Body::empty()).unwrap();
    let (status, _, body) = send(&app, r).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("TASK_STATE_INPUT_REQUIRED"), "{body}");
}

#[tokio::test]
async fn a_0_3_client_without_a_version_header_gets_0_3_shapes() {
    let (app, fake, _) = app();
    let r = authed(req("POST", "/")).body(rpc("message/send", v03_message())).unwrap();
    let (status, _, body) = send(&app, r).await;
    assert_eq!(status, StatusCode::OK);
    let v: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["result"]["kind"], "task", "{body}");
    assert_eq!(v["result"]["status"]["state"], "completed");
    assert_eq!(v["result"]["history"][0]["parts"][0], json!({ "kind": "text", "text": "hello" }), "the 0.3 message reached the handler as 1.0");
    let r = authed(req("POST", "/").header("a2a-version", "0.3")).body(rpc("tasks/get", json!({ "id": "m:t1" }))).unwrap();
    let v: Value = serde_json::from_str(&send(&app, r).await.2).unwrap();
    assert_eq!(v["result"]["status"]["state"], "input-required");
    let r = authed(req("POST", "/")).body(rpc("tasks/get", json!({ "id": "missing" }))).unwrap();
    let v: Value = serde_json::from_str(&send(&app, r).await.2).unwrap();
    assert_eq!(v["error"]["code"], -32001);
    let r = authed(req("POST", "/")).body(rpc("agent/getAuthenticatedExtendedCard", Value::Null)).unwrap();
    let v: Value = serde_json::from_str(&send(&app, r).await.2).unwrap();
    assert_eq!(v["result"]["protocolVersion"], "0.3.0");
    assert_eq!(v["result"]["securitySchemes"]["bearer"]["type"], "http");
    assert!(v["result"]["skills"].as_array().unwrap().iter().any(|s| s["id"] == "delegate:codex"));
    let r = authed(req("POST", "/")).body(rpc("tasks/pushNotificationConfig/set", json!({}))).unwrap();
    let v: Value = serde_json::from_str(&send(&app, r).await.2).unwrap();
    assert_eq!(v["error"]["code"], -32003);
    // 1.0 method names are not 0.3 methods.
    let r = authed(req("POST", "/")).body(rpc("SendMessage", v1_message())).unwrap();
    let v: Value = serde_json::from_str(&send(&app, r).await.2).unwrap();
    assert_eq!(v["error"]["code"], -32601);
    assert_eq!(fake.seen.lock().unwrap().iter().filter(|s| s.1.as_deref() == Some("ci")).count(), 4);
}

#[tokio::test]
async fn a_0_3_stream_is_sse_with_0_3_events() {
    let (app, _, _) = app();
    let r = authed(req("POST", "/")).body(rpc("message/stream", v03_message())).unwrap();
    let (status, headers, body) = send(&app, r).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("content-type").unwrap().to_str().unwrap().starts_with("text/event-stream"));
    let events: Vec<Value> = body.lines().filter_map(|l| l.strip_prefix("data:")).map(|d| serde_json::from_str(d.trim()).unwrap()).collect();
    assert_eq!(events.len(), 2, "{body}");
    assert_eq!(events[0]["result"]["kind"], "task");
    assert_eq!(events[1]["result"]["kind"], "status-update");
    assert_eq!(events[1]["result"]["final"], true);
    // And a 1.0 stream keeps 1.0 shapes.
    let r = authed(req("POST", "/").header("a2a-version", "1.0")).body(rpc("SendStreamingMessage", v1_message())).unwrap();
    let body = send(&app, r).await.2;
    assert!(body.contains("\"statusUpdate\""), "{body}");
}

#[tokio::test]
async fn other_versions_are_not_supported() {
    let (app, _, _) = app();
    for v in ["2.0", "0.2", "x"] {
        let r = authed(req("POST", "/").header("a2a-version", v)).body(rpc("SendMessage", v1_message())).unwrap();
        let body: Value = serde_json::from_str(&send(&app, r).await.2).unwrap();
        assert_eq!(body["error"]["code"], -32009, "{v}");
    }
}

#[tokio::test]
async fn the_card_is_public_and_shaped_for_the_readers_version() {
    let (app, _, _) = app();
    let r = req("GET", WELL_KNOWN_CARD).body(Body::empty()).unwrap();
    let (status, headers, body) = send(&app, r).await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get("access-control-allow-origin").is_none());
    let dual: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(dual["url"], format!("http://127.0.0.1:{PORT}/"));
    assert!(dual.get("securitySchemes").is_none());
    serde_json::from_value::<AgentCard>(dual).expect("1.0 readers parse it");
    let etag = headers.get("etag").unwrap().clone();
    let r = req("GET", WELL_KNOWN_CARD).header("if-none-match", etag).body(Body::empty()).unwrap();
    assert_eq!(send(&app, r).await.0, StatusCode::NOT_MODIFIED);
    let r = req("GET", WELL_KNOWN_CARD).header("a2a-version", "1.0").body(Body::empty()).unwrap();
    let strict: Value = serde_json::from_str(&send(&app, r).await.2).unwrap();
    assert!(strict.get("securitySchemes").is_some());
    assert!(strict.get("url").is_none());
    // The public card never lists this machine's agents.
    assert!(!body.contains("codex"));
}

#[tokio::test]
async fn failed_logins_are_throttled_for_everyone() {
    let (app, _, _) = app();
    for _ in 0..AUTH_FAILURES_PER_MIN {
        let r = req("POST", "/").header("authorization", "Bearer guess").body(rpc("GetTask", json!({ "id": "x" }))).unwrap();
        assert_eq!(send(&app, r).await.0, StatusCode::UNAUTHORIZED);
    }
    let r = authed(req("POST", "/").header("a2a-version", "1.0")).body(rpc("GetTask", json!({ "id": "m:t1" }))).unwrap();
    assert_eq!(send(&app, r).await.0, StatusCode::TOO_MANY_REQUESTS, "even the right token waits");
}

#[tokio::test]
async fn callers_are_rate_limited() {
    let (app, _, _) = app();
    let mut limited = false;
    for _ in 0..(CALLER_RATE_PER_SEC as usize * 3) {
        let r = authed(req("POST", "/").header("a2a-version", "1.0")).body(rpc("GetTask", json!({ "id": "m:t1" }))).unwrap();
        if send(&app, r).await.0 == StatusCode::TOO_MANY_REQUESTS {
            limited = true;
            break;
        }
    }
    assert!(limited);
}

#[tokio::test]
async fn bodies_over_the_cap_are_refused() {
    let (app, fake, _) = app();
    let big = json!({ "message": { "messageId": "m", "role": "ROLE_USER", "parts": [{ "text": "x".repeat(BODY_MAX_BYTES) }] } });
    let r = authed(req("POST", "/")).body(rpc("message/send", big.clone())).unwrap();
    assert_eq!(send(&app, r).await.0, StatusCode::PAYLOAD_TOO_LARGE);
    let r = authed(req("POST", "/").header("a2a-version", "1.0")).body(rpc("SendMessage", big)).unwrap();
    assert_eq!(send(&app, r).await.0, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(fake.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn serve_binds_loopback_only_and_stops() {
    let fake = Arc::new(Fake::default());
    let state = ServerState::new(vec![CallerAuth { id: "ci".into(), token: TOKEN.into() }], || card::pawl_card(&info(), false));
    let running = serve(fake, state.clone(), 0).await.unwrap();
    assert!(running.addr.ip().is_loopback());
    assert_eq!(state.port(), running.addr.port());
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(running.addr).await.unwrap();
    let get = format!("GET {WELL_KNOWN_CARD} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n", running.addr.port());
    s.write_all(get.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    let addr = running.addr;
    running.stop().await;
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());
}

#[test]
fn host_and_origin_parsing() {
    let h = |v: &str| {
        let mut m = HeaderMap::new();
        m.insert(header::HOST, HeaderValue::from_str(v).unwrap());
        m
    };
    assert!(host_ok(&h("127.0.0.1:80"), 80));
    assert!(!host_ok(&h("127.0.0.1"), 80), "the port must be explicit");
    assert!(!host_ok(&h("127.0.0.1:80:80"), 80));
    assert!(!host_ok(&h("[::1"), 80));
    assert!(!host_ok(&HeaderMap::new(), 80), "Host is required");
    let o = |v: &str| {
        let mut m = HeaderMap::new();
        m.insert(header::ORIGIN, HeaderValue::from_str(v).unwrap());
        m
    };
    assert!(origin_ok(&HeaderMap::new(), 80));
    assert!(origin_ok(&o("http://localhost:80"), 80));
    assert!(!origin_ok(&o("https://localhost:80"), 80), "never https from a page we did not serve");
    assert!(!origin_ok(&o("http://localhost:81"), 80));
    assert_eq!(requested_version(&HeaderMap::new(), Some("a=b&a2a-version=1.0")).as_deref(), Some("1.0"));
    assert_eq!(requested_version(&HeaderMap::new(), None), None);
}

#[test]
fn caller_auth_debug_hides_the_token() {
    let c = CallerAuth { id: "ci".into(), token: TOKEN.into() };
    assert!(!format!("{c:?}").contains(TOKEN));
}
