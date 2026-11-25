use super::*;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::post;
use axum::Json;
use std::sync::{Arc, Mutex};

const TOKEN: &str = "feedfacefeedfacefeedfacefeedface";

/// One call the fake server saw: Authorization, A2A-Version, body.
type Call = (Option<String>, Option<String>, Value);

#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<Call>>>);

impl Seen {
    fn calls(&self) -> Vec<Call> {
        self.0.lock().unwrap().clone()
    }
}

fn task(state: &str, text: &str) -> Value {
    json!({ "id": "t1", "contextId": "s1", "status": { "state": state, "message": { "messageId": "m", "role": "ROLE_AGENT", "parts": [{ "text": text }] } } })
}

/// A stand-in for Pawl's A2A JSON-RPC endpoint: records each call, answers by method.
async fn rpc(State(seen): State<Seen>, headers: HeaderMap, Json(body): Json<Value>) -> axum::response::Response {
    use axum::response::IntoResponse;
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(str::to_string);
    seen.0.lock().unwrap().push((h("authorization"), h("a2a-version"), body.clone()));
    if h("authorization").as_deref() != Some(&format!("Bearer {TOKEN}")) {
        return (axum::http::StatusCode::UNAUTHORIZED, "no").into_response();
    }
    let id = body["id"].clone();
    let result = match body["method"].as_str().unwrap_or("") {
        "ListTasks" => json!({ "tasks": [task("TASK_STATE_INPUT_REQUIRED", "Run cargo test?")], "nextPageToken": "", "pageSize": 20, "totalSize": 1 }),
        "GetExtendedAgentCard" => json!({
            "name": "Pawl", "description": "d", "version": "1", "supportedInterfaces": [], "capabilities": {},
            "defaultInputModes": [], "defaultOutputModes": [],
            "skills": [
                { "id": "status", "name": "Status", "description": "s", "tags": [] },
                { "id": "delegate:codex", "name": "Delegate to Codex", "description": "Start Codex. Repos: pawl.", "tags": [] }
            ]
        }),
        "SendMessage" if body["params"]["message"]["contextId"].is_null() => json!({ "task": task("TASK_STATE_WORKING", "Started Codex in /r.") }),
        "SendMessage" => json!({ "message": { "messageId": "x", "role": "ROLE_AGENT", "parts": [{ "text": "ok" }] } }),
        "GetTask" => json!(task("TASK_STATE_COMPLETED", "done")),
        "CancelTask" => return Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32002, "message": "task t1 has finished" } })).into_response(),
        _ => json!({}),
    };
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

async fn fake_pawl() -> (String, Seen) {
    let seen = Seen::default();
    let app = axum::Router::new().route("/", post(rpc)).with_state(seen.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (url, seen)
}

fn req(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn call(id: u64, name: &str, args: Value) -> Value {
    req(id, "tools/call", json!({ "name": name, "arguments": args }))
}

async fn bridge() -> (Bridge, Seen) {
    let (url, seen) = fake_pawl().await;
    (Bridge::new(BridgeConfig::new(&url, TOKEN).unwrap()).unwrap(), seen)
}

fn text(r: &Value) -> String {
    r["result"]["content"][0]["text"].as_str().unwrap_or("").to_string()
}

#[test]
fn only_loopback_http_is_accepted() {
    for ok in ["http://127.0.0.1:8941", "http://127.0.0.1:8941/", "http://[::1]:8941", "http://LOCALHOST:8941"] {
        let c = BridgeConfig::new(ok, TOKEN).unwrap_or_else(|e| panic!("{ok}: {e}"));
        assert!(c.url().starts_with("http://127.0.0.1:") || c.url().starts_with("http://[::1]:"), "{}", c.url());
    }
    for bad in [
        "https://127.0.0.1:8941",
        "http://10.0.0.5:8941",
        "http://example.com:8941",
        "http://localhost.example.com:8941",
        "http://127.0.0.1",
        "http://user:pw@127.0.0.1:8941",
        "http://127.0.0.1:8941/a2a",
        "http://127.0.0.1:8941/?x=1",
        "file:///etc/passwd",
        "not a url",
    ] {
        assert!(BridgeConfig::new(bad, TOKEN).unwrap_err().contains("this machine only"), "{bad}");
    }
    assert!(BridgeConfig::new("http://127.0.0.1:1", " ").unwrap_err().contains("PAWL_A2A_TOKEN"));
    assert!(!format!("{:?}", BridgeConfig::new("http://127.0.0.1:1", TOKEN).unwrap()).contains(TOKEN));
}

#[tokio::test]
async fn initialize_agrees_on_a_version() {
    let (mut b, _) = bridge().await;
    let r = b.handle(req(1, "initialize", json!({ "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "c", "version": "1" } }))).await.unwrap();
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(r["result"]["capabilities"]["tools"], json!({ "listChanged": false }));
    assert_eq!(r["result"]["serverInfo"]["name"], "pawl");
    assert!(r["result"]["instructions"].as_str().unwrap().contains("cannot approve"));
    let r = b.handle(req(2, "initialize", json!({ "protocolVersion": "2099-01-01" }))).await.unwrap();
    assert_eq!(r["result"]["protocolVersion"], LEGACY_VERSIONS[0]);
    assert_eq!(b.legacy_version(), Some(LEGACY_VERSIONS[0]));
    assert!(b.handle(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await.is_none());
    assert_eq!(b.handle(req(3, "ping", json!({}))).await.unwrap()["result"]["resultType"], "complete");
}

#[tokio::test]
async fn the_modern_era_discovers_and_checks_versions() {
    let (mut b, _) = bridge().await;
    let meta = json!({ "_meta": { VERSION_META: "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {} } });
    let r = b.handle(req(1, "server/discover", meta.clone())).await.unwrap();
    let res = &r["result"];
    assert_eq!(res["resultType"], "complete");
    assert_eq!(res["supportedVersions"], json!(["2026-07-28"]));
    assert_eq!(res["_meta"][SERVER_INFO_META]["name"], "pawl");
    assert!(res["ttlMs"].is_u64() && res["cacheScope"].is_string());
    let r = b.handle(req(2, "server/discover", json!({}))).await.unwrap();
    assert_eq!(r["error"]["code"], INVALID_PARAMS);
    let r = b.handle(req(3, "tools/list", json!({ "_meta": { VERSION_META: "1900-01-01" } }))).await.unwrap();
    assert_eq!(r["error"]["code"], UNSUPPORTED_VERSION);
    assert_eq!(r["error"]["data"]["requested"], "1900-01-01");
    assert!(r["error"]["data"]["supported"].as_array().unwrap().contains(&json!("2026-07-28")));
    let r = b.handle(req(4, "tools/list", meta)).await.unwrap();
    assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 6);
    assert_eq!(r["result"]["_meta"][SERVER_INFO_META]["name"], "pawl");
    let r = b.handle(req(5, "resources/list", json!({}))).await.unwrap();
    assert_eq!(r["error"]["code"], METHOD_NOT_FOUND);
    let r = b.handle(json!([req(6, "ping", json!({}))])).await.unwrap();
    assert_eq!(r["error"]["code"], INVALID_REQUEST);
}

#[tokio::test]
async fn tools_are_listed_with_plain_descriptions_and_no_approval_tool() {
    let (mut b, _) = bridge().await;
    let r = b.handle(req(1, "tools/list", json!({}))).await.unwrap();
    let tools = r["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["pawl_status", "pawl_agents", "pawl_start", "pawl_prompt", "pawl_task", "pawl_cancel"]);
    for t in tools {
        assert!(t["description"].as_str().unwrap().len() < 140, "{t}");
        assert_eq!(t["inputSchema"]["type"], "object");
        assert_eq!(t["annotations"]["destructiveHint"], false);
        assert!(!t["name"].as_str().unwrap().contains("approv"));
    }
    let start = &tools[2];
    assert_eq!(start["inputSchema"]["required"], json!(["agent", "folder", "prompt"]));
    assert_eq!(start["annotations"]["readOnlyHint"], false);
    assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);
}

#[tokio::test]
async fn tool_calls_go_to_pawls_a2a_server_as_the_caller() {
    let (mut b, seen) = bridge().await;
    let r = b.handle(call(1, "pawl_status", json!({}))).await.unwrap();
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert!(text(&r).contains("task t1 (session s1): input_required, waiting for the user on this machine: Run cargo test?"), "{}", text(&r));
    assert_eq!(r["result"]["structuredContent"]["tasks"][0]["waitsForUser"], true);

    let r = b.handle(call(2, "pawl_agents", json!({}))).await.unwrap();
    assert_eq!(r["result"]["structuredContent"]["agents"], json!([{ "agent": "codex", "name": "Codex", "description": "Start Codex. Repos: pawl." }]));

    let r = b.handle(call(3, "pawl_start", json!({ "agent": "codex", "folder": "/r", "prompt": "run the tests" }))).await.unwrap();
    assert!(text(&r).starts_with("Started. task t1 (session s1): working"), "{}", text(&r));
    let r = b.handle(call(4, "pawl_prompt", json!({ "session": "s1", "prompt": "and fix them" }))).await.unwrap();
    assert_eq!(text(&r), "Pawl answered: ok");
    let r = b.handle(call(5, "pawl_task", json!({ "task": "t1" }))).await.unwrap();
    assert_eq!(r["result"]["structuredContent"]["state"], "completed");
    // A2A refusals come back as tool errors with Pawl's sentence.
    let r = b.handle(call(6, "pawl_cancel", json!({ "task": "t1" }))).await.unwrap();
    assert_eq!(r["result"]["isError"], true);
    assert_eq!(text(&r), "Pawl said: task t1 has finished");

    let calls = seen.calls();
    assert_eq!(calls.len(), 6);
    for (auth, version, body) in &calls {
        assert_eq!(auth.as_deref(), Some(format!("Bearer {TOKEN}").as_str()));
        assert_eq!(version.as_deref(), Some(crate::A2A_VERSION));
        assert_eq!(body["jsonrpc"], "2.0");
    }
    let methods: Vec<&str> = calls.iter().map(|c| c.2["method"].as_str().unwrap()).collect();
    assert_eq!(methods, ["ListTasks", "GetExtendedAgentCard", "SendMessage", "SendMessage", "GetTask", "CancelTask"]);
    let start = &calls[2].2["params"];
    assert_eq!(start["message"]["metadata"][crate::DELEGATE_EXT], json!({ "agentId": "codex", "cwd": "/r" }));
    assert_eq!(start["message"]["parts"][0]["text"], "run the tests");
    assert_eq!(start["configuration"]["returnImmediately"], true);
    assert_eq!(calls[3].2["params"]["message"]["contextId"], "s1");
}

#[tokio::test]
async fn bad_calls_and_a_wrong_token_or_a_stopped_pawl_say_so() {
    let (mut b, _) = bridge().await;
    let r = b.handle(call(1, "pawl_start", json!({ "agent": "codex", "prompt": "x" }))).await.unwrap();
    assert_eq!((r["result"]["isError"].clone(), text(&r)), (json!(true), "folder is required".to_string()));
    let r = b.handle(call(2, "pawl_answer", json!({}))).await.unwrap();
    assert_eq!(r["error"]["code"], INVALID_PARAMS);
    let r = b.handle(req(3, "tools/call", json!({ "name": "pawl_task", "arguments": [1] }))).await.unwrap();
    assert_eq!(r["result"]["isError"], true);

    let (url, _) = fake_pawl().await;
    let mut wrong = Bridge::new(BridgeConfig::new(&url, "0000").unwrap()).unwrap();
    let r = wrong.handle(call(4, "pawl_status", json!({}))).await.unwrap();
    assert!(text(&r).contains("refused this agent's token") && !text(&r).contains("0000"), "{}", text(&r));

    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut down = Bridge::new(BridgeConfig::new(&format!("http://127.0.0.1:{port}"), TOKEN).unwrap()).unwrap();
    let r = down.handle(call(5, "pawl_status", json!({}))).await.unwrap();
    assert!(text(&r).contains("not answering") && !text(&r).contains(TOKEN), "{}", text(&r));
}

#[tokio::test]
async fn serve_speaks_newline_delimited_json_rpc() {
    let (url, _) = fake_pawl().await;
    let mut input = Vec::new();
    for m in [
        req(1, "initialize", json!({ "protocolVersion": "2025-11-25" })),
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        call(2, "pawl_status", json!({})),
    ] {
        input.extend(serde_json::to_vec(&m).unwrap());
        input.push(b'\n');
    }
    input.extend(b"{not json\n\n");
    input.extend(vec![b' '; MAX_LINE_BYTES + 10]);
    input.extend(b"\n");
    input.extend(serde_json::to_vec(&req(3, "tools/list", json!({}))).unwrap());
    let mut out = Vec::new();
    serve(BridgeConfig::new(&url, TOKEN).unwrap(), tokio::io::BufReader::new(&input[..]), &mut out).await.unwrap();
    let text = String::from_utf8(out).unwrap();
    let lines: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 5, "{text}");
    assert_eq!(lines[0]["id"], 1);
    assert_eq!(lines[1]["id"], 2);
    assert_eq!(lines[2]["error"]["code"], PARSE_ERROR);
    assert_eq!(lines[3]["error"]["message"], "message too large");
    assert_eq!(lines[4]["result"]["tools"].as_array().unwrap().len(), 6, "the last line needs no newline");
    assert!(!text.contains(TOKEN));
}
