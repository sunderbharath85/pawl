//! A2: the localhost A2A server over the hub (see `core/a2a_server.rs`).

use super::*;
use crate::config::A2aCaller;
use crate::core::a2a_server::test_router;
use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use pawl_a2a::Caller;
use pawl_protocol::{A2aCallerPatch, HubSettingsA2aPatch, HubSettingsPatch};
use tower::ServiceExt;

const PORT: u16 = 47998;
const CI: &str = "c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1";
const OPS: &str = "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";

async fn a2a_fixture() -> Fixture {
    fixture_config(|c| {
        c.a2a.callers = vec![
            A2aCaller { id: "ci".into(), name: "CI bot".into(), token: Some(CI.into()), allow_approvals: false },
            A2aCaller { id: "ops".into(), name: "Ops".into(), token: Some(OPS.into()), allow_approvals: true },
        ];
    })
    .await
}

async fn call(app: &axum::Router, token: &str, version: Option<&str>, method: &str, params: Value) -> Value {
    let mut b = HttpRequest::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("127.0.0.1:{PORT}"))
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if let Some(v) = version {
        b = b.header("a2a-version", v);
    }
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }).to_string();
    let resp = app.clone().oneshot(b.body(Body::from(body)).unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(resp.into_body(), 8 << 20).await.unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if text.starts_with("data:") || text.contains("\ndata:") {
        let events: Vec<Value> = text.lines().filter_map(|l| l.strip_prefix("data:")).map(|d| serde_json::from_str(d.trim()).unwrap()).collect();
        return Value::Array(events);
    }
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

fn start_params(cwd: &str, text: &str, immediate: bool) -> Value {
    json!({
        "message": {
            "messageId": uuid::Uuid::new_v4().to_string(),
            "role": "ROLE_USER",
            "parts": [{ "text": text }],
            "metadata": { pawl_a2a::DELEGATE_EXT: { "agentId": "codex", "cwd": cwd } }
        },
        "configuration": { "returnImmediately": immediate }
    })
}

fn answer_params(task_id: &str, request_id: &str, option: &str) -> Value {
    json!({
        "message": {
            "messageId": uuid::Uuid::new_v4().to_string(),
            "role": "ROLE_USER",
            "taskId": task_id,
            "parts": [{ "data": { "optionId": option }, "metadata": { pawl_a2a::STRUCTURED_INPUT_EXT: { "type": "structured_input", "id": request_id } } }]
        },
        "configuration": { "returnImmediately": true }
    })
}

/// A known folder (a local session ran there) and an outside start in it; returns (task id, session id).
async fn outside_start(f: &Fixture, app: &axum::Router, token: &str) -> (String, String) {
    let cwd = f.dir.path().to_string_lossy().into_owned();
    start(f).await; // the folder is known now
    let v = call(app, token, Some("1.0"), "SendMessage", start_params(&cwd, "run the tests", true)).await;
    let task = &v["result"]["task"];
    assert_eq!(task["status"]["state"], "TASK_STATE_SUBMITTED", "{v}");
    (task["id"].as_str().unwrap().to_string(), task["contextId"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn an_outside_caller_delegates_a_task_bound_to_itself_and_audited() {
    let f = a2a_fixture().await;
    let app = test_router(&f.hub, PORT);
    let (task, ctx) = outside_start(&f, &app, CI).await;
    assert!(task.starts_with(&format!("{MID}:")));
    assert_eq!(f.hub.tasks().owner_of(&ctx), Caller::Outside("ci".into()));
    assert!(f.hub.st().remote_driven.contains(&ctx), "rules never auto-approve its requests");
    assert!(f.fake.calls().iter().any(|c| c == &format!("prompt {ctx} run the tests")), "{:?}", f.fake.calls());
    // Its own task: visible. The other caller: not.
    let v = call(&app, CI, Some("1.0"), "GetTask", json!({ "id": task })).await;
    assert_eq!(v["result"]["id"], task.as_str());
    let v = call(&app, OPS, Some("1.0"), "GetTask", json!({ "id": task })).await;
    assert_eq!(v["error"]["code"], -32001, "{v}");
    let v = call(&app, OPS, Some("1.0"), "ListTasks", json!({})).await;
    // ProtoJSON leaves out empty lists.
    assert_eq!(v["result"]["tasks"].as_array().map_or(0, Vec::len), 0, "{v}");
    // A follow-up in its context is a new task.
    let v = call(&app, CI, Some("1.0"), "SendMessage", json!({ "message": { "messageId": "m2", "role": "ROLE_USER", "contextId": ctx, "parts": [{ "text": "and lint" }] }, "configuration": { "returnImmediately": true } })).await;
    assert_ne!(v["result"]["task"]["id"], task.as_str(), "{v}");
    // Every call is in the A2A log as the caller.
    let log = f.hub.store().a2a_log(10).unwrap();
    assert!(log.iter().all(|e| e.caller.starts_with("a2a:")));
    let methods: Vec<(&str, &str, &str)> = log.iter().rev().map(|e| (e.caller.as_str(), e.method.as_str(), e.status.as_str())).collect();
    assert_eq!(
        methods,
        vec![
            ("a2a:ci", "SendMessage", "ok"),
            ("a2a:ci", "GetTask", "ok"),
            ("a2a:ops", "GetTask", "task_not_found"),
            ("a2a:ops", "ListTasks", "ok"),
            ("a2a:ci", "SendMessage", "ok"),
        ]
    );
    // Nothing of the prompts in the log.
    assert!(!format!("{log:?}").contains("run the tests"));
}

#[tokio::test]
async fn outside_callers_start_only_installed_agents_in_known_folders() {
    let f = a2a_fixture().await;
    let app = test_router(&f.hub, PORT);
    let elsewhere = tempfile::tempdir().unwrap();
    let v = call(&app, CI, Some("1.0"), "SendMessage", start_params(&elsewhere.path().to_string_lossy(), "x", true)).await;
    assert_eq!(v["error"]["code"], -32004, "an unknown folder is forbidden: {v}");
    let mut p = start_params(&f.dir.path().to_string_lossy(), "x", true);
    p["message"]["metadata"][pawl_a2a::DELEGATE_EXT]["agentId"] = json!("goose");
    let v = call(&app, CI, Some("1.0"), "SendMessage", p).await;
    assert_eq!(v["error"]["code"], -32602, "{v}");
    let v = call(&app, CI, Some("1.0"), "SendMessage", json!({ "message": { "messageId": "m", "role": "ROLE_USER", "parts": [{ "text": "x" }] } })).await;
    assert_eq!(v["error"]["code"], -32602, "a start needs delegate metadata: {v}");
    assert!(!f.fake.calls().iter().any(|c| c.starts_with("start")), "nothing started");
    // A local session is not theirs to prompt.
    let local = start(&f).await;
    let v = call(&app, CI, Some("1.0"), "SendMessage", json!({ "message": { "messageId": "m", "role": "ROLE_USER", "contextId": local.id, "parts": [{ "text": "x" }] } })).await;
    assert_eq!(v["error"]["code"], -32602, "{v}");
    assert!(!f.fake.calls().iter().any(|c| c == &format!("prompt {} x", local.id)));
}

#[tokio::test]
async fn approvals_need_the_callers_permission_and_respect_the_hard_floor() {
    let f = a2a_fixture().await;
    let app = test_router(&f.hub, PORT);
    // ci may never answer.
    let (task, ctx) = outside_start(&f, &app, CI).await;
    let req = permission(&ctx, ToolKind::Read);
    f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    let v = call(&app, CI, Some("1.0"), "GetTask", json!({ "id": task })).await;
    assert_eq!(v["result"]["status"]["state"], "TASK_STATE_INPUT_REQUIRED", "{v}");
    let v = call(&app, CI, Some("1.0"), "SendMessage", answer_params(&task, &req.request_id, "allow")).await;
    assert_eq!(v["error"]["code"], -32004, "{v}");
    assert!(!f.fake.calls().iter().any(|c| c.starts_with("resolve")));

    // ops may: allow once on a read, never on execute; deny always works.
    let (task, ctx) = outside_start(&f, &app, OPS).await;
    let read = permission(&ctx, ToolKind::Read);
    f.hub.on_host_event(HostEvent::PermissionRequest(read.clone())).await;
    let v = call(&app, OPS, Some("1.0"), "SendMessage", answer_params(&task, &read.request_id, "allow")).await;
    assert_eq!(v["result"]["task"]["status"]["state"], "TASK_STATE_WORKING", "{v}");
    assert!(f.fake.calls().contains(&format!("resolve {} allow", read.request_id)));
    let audit = f.hub.store().audit_log(5).unwrap();
    assert_eq!(audit[0].actor, "a2a:ops");
    let exec = permission(&ctx, ToolKind::Execute);
    f.hub.on_host_event(HostEvent::PermissionRequest(exec.clone())).await;
    let v = call(&app, OPS, Some("1.0"), "SendMessage", answer_params(&task, &exec.request_id, "allow")).await;
    assert_eq!(v["error"]["code"], -32004, "{v}");
    let v = call(&app, OPS, Some("1.0"), "SendMessage", answer_params(&task, &exec.request_id, "reject")).await;
    assert_eq!(v["result"]["task"]["status"]["state"], "TASK_STATE_WORKING", "{v}");
    // A local session's request is out of reach.
    let local = start(&f).await;
    let lr = permission(&local.id, ToolKind::Read);
    f.hub.on_host_event(HostEvent::PermissionRequest(lr.clone())).await;
    let lt = f.hub.tasks().active_task(&local.id).unwrap();
    let v = call(&app, OPS, Some("1.0"), "SendMessage", answer_params(&lt.id, &lr.request_id, "reject")).await;
    assert_eq!(v["error"]["code"], -32001, "{v}");
}

#[tokio::test]
async fn a_0_3_client_delegates_and_gets_0_3_shapes() {
    let f = a2a_fixture().await;
    let app = test_router(&f.hub, PORT);
    start(&f).await;
    let cwd = f.dir.path().to_string_lossy().into_owned();
    let params = json!({
        "message": { "kind": "message", "messageId": "m1", "role": "user", "parts": [{ "kind": "text", "text": "build it" }],
                     "metadata": { pawl_a2a::DELEGATE_EXT: { "agentId": "codex", "cwd": cwd } } },
        "configuration": { "blocking": false }
    });
    let v = call(&app, CI, None, "message/send", params).await;
    assert_eq!(v["result"]["kind"], "task", "{v}");
    assert_eq!(v["result"]["status"]["state"], "submitted");
    let id = v["result"]["id"].as_str().unwrap().to_string();
    let v = call(&app, CI, Some("0.3"), "tasks/cancel", json!({ "id": id })).await;
    assert_eq!(v["result"]["status"]["state"], "canceled", "{v}");
    let v = call(&app, CI, None, "tasks/cancel", json!({ "id": id })).await;
    assert_eq!(v["error"]["code"], -32002, "a finished task is not cancelable: {v}");
}

#[tokio::test]
async fn send_message_blocks_until_the_task_finishes_or_asks() {
    let f = a2a_fixture().await;
    let app = test_router(&f.hub, PORT);
    start(&f).await;
    let cwd = f.dir.path().to_string_lossy().into_owned();
    let hub = f.hub.clone();
    let finisher = tokio::spawn(async move {
        // Wait for the delegated session's task, then end its turn.
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let mine = hub.tasks().latest_per_context().into_iter().find(|t| hub.tasks().owner_of(&t.context_id) == Caller::Outside("ci".into()));
            if let Some(t) = mine {
                hub.on_host_event(HostEvent::Event { session_id: t.context_id.clone(), kind: EventType::AgentMessageChunk, payload: json!({ "text": "Done." }) }).await;
                hub.on_host_event(HostEvent::Event { session_id: t.context_id, kind: EventType::Stop, payload: json!({ "stopReason": "end_turn" }) }).await;
                return;
            }
        }
    });
    let v = call(&app, CI, Some("1.0"), "SendMessage", start_params(&cwd, "go", false)).await;
    finisher.await.unwrap();
    assert_eq!(v["result"]["task"]["status"]["state"], "TASK_STATE_COMPLETED", "{v}");
    assert_eq!(v["result"]["task"]["status"]["message"]["parts"][0]["text"], "Done.");
}

#[tokio::test]
async fn a_stream_follows_the_task_until_it_ends() {
    let f = a2a_fixture().await;
    let app = test_router(&f.hub, PORT);
    start(&f).await;
    let cwd = f.dir.path().to_string_lossy().into_owned();
    let hub = f.hub.clone();
    let driver = tokio::spawn(async move {
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let mine = hub.tasks().latest_per_context().into_iter().find(|t| hub.tasks().owner_of(&t.context_id) == Caller::Outside("ci".into()));
            if let Some(t) = mine {
                tokio::time::sleep(Duration::from_millis(50)).await; // the stream is subscribed by now
                let ev = |kind, payload| HostEvent::Event { session_id: t.context_id.clone(), kind, payload };
                hub.on_host_event(ev(EventType::ToolCall, json!({ "toolCallId": "e", "title": "Edit a.rs", "kind": "edit" }))).await;
                hub.on_host_event(ev(EventType::Stop, json!({ "stopReason": "end_turn" }))).await;
                return;
            }
        }
    });
    let events = call(&app, CI, Some("1.0"), "SendStreamingMessage", start_params(&cwd, "go", false)).await;
    driver.await.unwrap();
    let events = events.as_array().unwrap();
    assert!(events[0]["result"].get("task").is_some(), "{events:?}");
    assert!(events.iter().any(|e| e["result"]["artifactUpdate"]["artifact"]["name"] == "diff"), "{events:?}");
    let last = &events.last().unwrap()["result"]["statusUpdate"];
    assert_eq!(last["status"]["state"], "TASK_STATE_COMPLETED", "{events:?}");
}

#[tokio::test]
async fn the_extended_card_needs_the_token_and_lists_agents() {
    let f = a2a_fixture().await;
    let app = test_router(&f.hub, PORT);
    let v = call(&app, CI, Some("1.0"), "GetExtendedAgentCard", json!({})).await;
    let skills: Vec<&str> = v["result"]["skills"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert!(skills.contains(&"delegate:codex"), "{skills:?}");
    let r = HttpRequest::builder().method("GET").uri("/.well-known/agent-card.json").header("host", format!("127.0.0.1:{PORT}")).body(Body::empty()).unwrap();
    let resp = app.clone().oneshot(r).await.unwrap();
    let body = String::from_utf8_lossy(&axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap()).into_owned();
    assert!(!body.contains("delegate:codex") && !body.contains(&f.dir.path().to_string_lossy().into_owned()), "{body}");
    // DNS rebinding through the hub's server too.
    let r = HttpRequest::builder().method("POST").uri("/").header("host", "pawl.attacker.example:47998").header("authorization", format!("Bearer {CI}")).body(Body::from("{}")).unwrap();
    assert_eq!(app.clone().oneshot(r).await.unwrap().status(), StatusCode::FORBIDDEN);
}

fn a2a_patch(a: HubSettingsA2aPatch) -> HubSettingsPatch {
    HubSettingsPatch { a2a: Some(a), ..Default::default() }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn raw_get(port: u16, path: &str, auth: Option<&str>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let auth = auth.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    s.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n{auth}Connection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    out
}

#[tokio::test]
async fn the_server_is_off_by_default_and_follows_its_settings() {
    let f = fixture(None).await;
    let s = f.hub.settings();
    assert!(!s.a2a.server && !s.a2a.running);
    assert_eq!(s.a2a.port, pawl_protocol::A2A_DEFAULT_PORT);
    let me = actor();
    let port = free_port();
    let patch = a2a_patch(HubSettingsA2aPatch {
        server: Some(true),
        port: Some(port),
        callers: Some(vec![A2aCallerPatch { id: "ci".into(), name: "CI bot".into(), allow_approvals: false }]),
        agents: None,
    });
    f.hub.update_settings(&patch, &me).await.unwrap();
    let s = f.hub.settings();
    assert!(s.a2a.running, "{:?}", s.a2a);
    assert_eq!(s.a2a.url.as_deref(), Some(format!("http://127.0.0.1:{port}").as_str()));
    assert!(!s.a2a.callers[0].token_set, "a new caller has no token yet");
    let card = raw_get(port, "/.well-known/agent-card.json", None).await;
    assert!(card.starts_with("HTTP/1.1 200"), "{card}");
    // No token yet: nothing gets in.
    let calls = raw_get(port, "/tasks", None).await;
    assert!(calls.starts_with("HTTP/1.1 401"), "{calls}");

    // A token is shown once, saved write-only, audited without its value.
    let v = f.hub.a2a_rotate_token("ci", &me).await.unwrap();
    let token = v["token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 64);
    assert!(f.hub.settings().a2a.callers[0].token_set);
    assert!(!serde_json::to_string(&f.hub.get_settings()).unwrap().contains(&token), "never read back");
    let toml = std::fs::read_to_string(f.hub.paths().config.clone()).unwrap();
    assert!(toml.contains("[[a2a.callers]]") && toml.contains(&token), "{toml}");
    let audit = f.hub.store().settings_audit(5).unwrap();
    assert_eq!(audit[0].keys, vec![crate::settings::A2A_TOKENS.to_string()]);
    assert!(!format!("{audit:?}").contains(&token));
    let ok = raw_get(port, "/tasks", Some(&token)).await;
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
    // Rotating again retires the old token at once.
    let newer = f.hub.a2a_rotate_token("ci", &me).await.unwrap()["token"].as_str().unwrap().to_string();
    assert!(raw_get(port, "/tasks", Some(&token)).await.starts_with("HTTP/1.1 401"));
    assert!(raw_get(port, "/tasks", Some(&newer)).await.starts_with("HTTP/1.1 200"));
    assert!(f.hub.a2a_rotate_token("nobody", &me).await.is_err());
    // A removed caller is out at once.
    f.hub.update_settings(&a2a_patch(HubSettingsA2aPatch { callers: Some(vec![]), ..Default::default() }), &me).await.unwrap();
    assert!(raw_get(port, "/tasks", Some(&newer)).await.starts_with("HTTP/1.1 401"));
    // Off again: the port closes.
    f.hub.update_settings(&a2a_patch(HubSettingsA2aPatch { server: Some(false), ..Default::default() }), &me).await.unwrap();
    assert!(!f.hub.settings().a2a.running);
    assert!(tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_err());
}

#[tokio::test]
async fn a2a_settings_are_validated() {
    let f = fixture(None).await;
    let me = actor();
    let bad = |a: HubSettingsA2aPatch| a2a_patch(a);
    let err = |r: Result<Value>| format!("{:#}", r.unwrap_err());
    assert!(err(f.hub.update_settings(&bad(HubSettingsA2aPatch { port: Some(80), ..Default::default() }), &me).await).starts_with("invalid:a2a.port"));
    let hub_port = f.hub.config().port;
    assert!(err(f.hub.update_settings(&bad(HubSettingsA2aPatch { port: Some(hub_port), ..Default::default() }), &me).await).contains("share a port"));
    let caller = |id: &str, name: &str| A2aCallerPatch { id: id.into(), name: name.into(), allow_approvals: false };
    for (list, field) in [
        (vec![caller("Bad Id", "x")], "invalid:a2a.callers[0].id"),
        (vec![caller("a", "x"), caller("a", "y")], "invalid:a2a.callers[1].id"),
        (vec![caller("a", "")], "invalid:a2a.callers[0].name"),
        (vec![caller("a", "evil\u{202E}name")], "invalid:a2a.callers[0].name"),
    ] {
        let e = err(f.hub.update_settings(&bad(HubSettingsA2aPatch { callers: Some(list), ..Default::default() }), &me).await);
        assert!(e.starts_with(field), "{e}");
    }
    // A port in use is refused before anything is written.
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let p = busy.local_addr().unwrap().port();
    let e = err(f.hub.update_settings(&bad(HubSettingsA2aPatch { server: Some(true), port: Some(p), ..Default::default() }), &me).await);
    assert!(e.contains("invalid:a2a.port") && e.contains("in use"), "{e}");
    assert!(!f.hub.config().a2a.server);
}

#[tokio::test]
async fn list_tasks_shows_every_delegation_with_its_caller() {
    let f = a2a_fixture().await;
    let app = test_router(&f.hub, PORT);
    let (task, ctx) = outside_start(&f, &app, CI).await;
    let v = ack_result(f.hub.handle(ClientMsg::ListTasks { id: "l".into(), session_id: None }, &actor()).await);
    let tasks = v["tasks"].as_array().unwrap();
    assert_eq!(tasks.len(), 2, "the local start and the outside one: {v}");
    let theirs = tasks.iter().find(|t| t["taskId"] == task.as_str()).unwrap();
    assert_eq!(theirs["caller"], "a2a:ci");
    assert_eq!(theirs["sessionId"], ctx.as_str());
    assert_eq!(theirs["state"], "submitted");
    let v = ack_result(f.hub.handle(ClientMsg::ListTasks { id: "l".into(), session_id: Some(ctx.clone()) }, &actor()).await);
    assert_eq!(v["tasks"].as_array().unwrap().len(), 1);
    let v = ack_result(f.hub.handle(ClientMsg::A2aRotateToken { id: "r".into(), caller_id: "ci".into() }, &actor()).await);
    assert_eq!(v["token"].as_str().unwrap().len(), 64);
}
