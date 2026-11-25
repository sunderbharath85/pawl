use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::Arc;

use super::*;
use crate::card::{pawl_card, CardInfo};
use crate::server::{self, A2aHandler, CallerAuth, EventStream, ServerState, ServiceParams};
use a2a::{
    A2AError, Artifact, GetExtendedAgentCardRequest, ListTasksRequest, ListTasksResponse, Message, Part, Role, SubscribeToTaskRequest, TaskArtifactUpdateEvent, TaskState,
    TaskStatus, TaskStatusUpdateEvent,
};

const TOKEN: &str = "agent-token";

/// An outside agent (Pawl's own server with a scripted handler).
struct Agent;

fn task(state: TaskState) -> Task {
    Task { id: "x:t1".into(), context_id: "x:c1".into(), status: TaskStatus { state, message: None, timestamp: None }, artifacts: None, history: None, metadata: None }
}

#[crate::async_trait]
impl A2aHandler for Agent {
    async fn send_message(&self, _p: &ServiceParams, req: SendMessageRequest) -> Result<SendMessageResponse, A2AError> {
        if req.message.text() == Some("fail") {
            return Err(A2AError::unsupported_operation("not today"));
        }
        Ok(SendMessageResponse::Task(task(TaskState::Completed)))
    }
    async fn send_streaming_message(&self, _p: &ServiceParams, _req: SendMessageRequest) -> Result<EventStream, A2AError> {
        let events = vec![
            Ok(StreamResponse::Task(task(TaskState::Working))),
            Ok(StreamResponse::ArtifactUpdate(TaskArtifactUpdateEvent {
                task_id: "x:t1".into(),
                context_id: "x:c1".into(),
                artifact: Artifact { artifact_id: "a".into(), name: None, description: None, parts: vec![Part::text("hello\nworld")], metadata: None, extensions: None },
                append: Some(false),
                last_chunk: Some(true),
                metadata: None,
            })),
            Ok(StreamResponse::StatusUpdate(TaskStatusUpdateEvent {
                task_id: "x:t1".into(),
                context_id: "x:c1".into(),
                status: TaskStatus { state: TaskState::Completed, message: Some(Message::new(Role::Agent, vec![Part::text("done")])), timestamp: None },
                metadata: None,
            })),
        ];
        Ok(futures::stream::iter(events).boxed())
    }
    async fn get_task(&self, _p: &ServiceParams, req: GetTaskRequest) -> Result<Task, A2AError> {
        if req.id == "missing" {
            return Err(A2AError::task_not_found(&req.id));
        }
        Ok(task(TaskState::Working))
    }
    async fn list_tasks(&self, _p: &ServiceParams, _r: ListTasksRequest) -> Result<ListTasksResponse, A2AError> {
        Ok(ListTasksResponse { tasks: vec![], next_page_token: String::new(), page_size: 0, total_size: 0 })
    }
    async fn cancel_task(&self, _p: &ServiceParams, _r: CancelTaskRequest) -> Result<Task, A2AError> {
        Ok(task(TaskState::Canceled))
    }
    async fn subscribe_to_task(&self, _p: &ServiceParams, _r: SubscribeToTaskRequest) -> Result<EventStream, A2AError> {
        Ok(futures::stream::empty().boxed())
    }
    async fn get_extended_agent_card(&self, _p: &ServiceParams, _r: GetExtendedAgentCardRequest) -> Result<AgentCard, A2AError> {
        Err(A2AError::extended_card_not_configured())
    }
}

/// A running agent on loopback; its base URL.
async fn agent() -> (server::RunningServer, String) {
    let port = Arc::new(AtomicU16::new(0));
    let p2 = port.clone();
    let state = ServerState::new(vec![CallerAuth { id: "pawl".into(), token: TOKEN.into() }], move || {
        pawl_card(&CardInfo { version: "9".into(), base_url: format!("http://127.0.0.1:{}", p2.load(Ordering::SeqCst)), ..Default::default() }, false)
    });
    let running = server::serve(Arc::new(Agent), state, 0).await.unwrap();
    port.store(running.addr.port(), Ordering::SeqCst);
    let url = format!("http://127.0.0.1:{}", running.addr.port());
    (running, url)
}

fn msg(text: &str) -> SendMessageRequest {
    SendMessageRequest { message: Message::new(Role::User, vec![Part::text(text)]), configuration: None, metadata: None, tenant: None }
}

#[tokio::test]
async fn the_card_is_fetched_cached_and_its_endpoint_is_on_the_same_origin() {
    let (running, url) = agent().await;
    let http = http_client();
    let CardFetch::Fresh { card, etag, signed } = fetch_card(&http, &url, None, None).await.unwrap() else { panic!("fresh") };
    assert_eq!(card.name, "Pawl");
    assert!(!signed);
    let etag = etag.expect("an etag");
    assert!(matches!(fetch_card(&http, &url, Some(&etag), None).await.unwrap(), CardFetch::NotModified));
    let ep = endpoint(&card, &url, Some(TOKEN.into())).unwrap();
    assert_eq!(ep.wire, Wire::V1, "1.0 preferred");
    assert_eq!(ep.url.as_str(), format!("{url}/"));
    assert!(!format!("{ep:?}").contains(TOKEN));
    // A card whose endpoint is elsewhere cannot get the token.
    let mut other = (*card).clone();
    for i in other.supported_interfaces.iter_mut() {
        i.url = "https://collector.example/".into();
    }
    assert!(matches!(endpoint(&other, &url, None), Err(ClientError::OtherHost(_))));
    let mut legacy = (*card).clone();
    legacy.supported_interfaces.retain(|i| i.protocol_version == "0.3");
    assert_eq!(endpoint(&legacy, &url, None).unwrap().wire, Wire::V03);
    legacy.supported_interfaces.clear();
    assert_eq!(endpoint(&legacy, &url, None).unwrap_err(), ClientError::NoInterface);
    running.stop().await;
}

#[tokio::test]
async fn calls_and_streams_work_in_1_0_and_0_3() {
    let (running, url) = agent().await;
    let http = http_client();
    let base = Url::parse(&format!("{url}/")).unwrap();
    for wire in [Wire::V1, Wire::V03] {
        let ep = Endpoint::new(base.clone(), wire, Some(TOKEN.into()));
        let SendMessageResponse::Task(t) = ep.send_message(&http, &msg("hi")).await.unwrap() else { panic!() };
        assert_eq!(t.status.state, TaskState::Completed, "{wire:?}");
        let events: Vec<_> = ep.send_streaming(&http, &msg("go")).await.unwrap().collect().await;
        assert_eq!(events.len(), 3, "{wire:?}: {events:?}");
        assert!(matches!(&events[0], Ok(StreamResponse::Task(_))));
        match &events[1] {
            Ok(StreamResponse::ArtifactUpdate(a)) => assert_eq!(a.artifact.parts[0].as_text(), Some("hello\nworld")),
            other => panic!("{other:?}"),
        }
        match &events[2] {
            Ok(StreamResponse::StatusUpdate(s)) => assert_eq!(s.status.state, TaskState::Completed),
            other => panic!("{other:?}"),
        }
        assert_eq!(ep.cancel_task(&http, "x:t1").await.unwrap().status.state, TaskState::Canceled);
        assert_eq!(ep.get_task(&http, "missing").await.unwrap_err(), ClientError::Agent { code: -32001, message: "task not found: missing".into() });
        let e = ep.send_message(&http, &msg("fail")).await.unwrap_err();
        assert!(matches!(e, ClientError::Agent { code: -32004, .. }), "{e:?}");
    }
    // Without the token the agent refuses.
    let ep = Endpoint::new(base, Wire::V1, Some("wrong".into()));
    assert_eq!(ep.send_message(&http, &msg("hi")).await.unwrap_err(), ClientError::Status(401));
    running.stop().await;
}

/// A tiny HTTP server with fixed answers per path.
async fn fixed(routes: Vec<(&'static str, axum::response::Response)>) -> String {
    use axum::routing::get;
    let mut app = axum::Router::new();
    for (path, resp) in routes {
        let resp = std::sync::Arc::new(std::sync::Mutex::new(Some(resp)));
        app = app.route(
            path,
            get(move || {
                let r = resp.clone();
                async move { r.lock().unwrap().take().unwrap_or_else(|| axum::http::StatusCode::GONE.into_response()) }
            }),
        );
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

use axum::response::IntoResponse;

#[tokio::test]
async fn oversized_cards_and_redirects_are_refused() {
    let big = format!("{{\"name\":\"{}\"}}", "x".repeat(CARD_MAX_BYTES));
    let url = fixed(vec![
        ("/big/.well-known/agent-card.json", big.into_response()),
        ("/moved/.well-known/agent-card.json", (axum::http::StatusCode::FOUND, [("location", "https://evil.example/card.json")]).into_response()),
        ("/junk/.well-known/agent-card.json", "not json".into_response()),
        ("/legacy/.well-known/agent-card.json", axum::Json(json!({
            "name": "Old agent", "description": "d", "url": "http://127.0.0.1:1/", "version": "1", "protocolVersion": "0.3.0",
            "capabilities": { "streaming": true }, "defaultInputModes": ["text"], "defaultOutputModes": ["text"], "skills": [],
            "signatures": [{ "protected": "e30", "signature": "c2ln" }]
        })).into_response()),
    ])
    .await;
    let http = http_client();
    assert_eq!(fetch_card(&http, &format!("{url}/big"), None, None).await.unwrap_err(), ClientError::TooLarge);
    assert_eq!(fetch_card(&http, &format!("{url}/moved"), None, None).await.unwrap_err(), ClientError::Status(302), "redirects are not followed");
    assert!(matches!(fetch_card(&http, &format!("{url}/junk"), None, None).await.unwrap_err(), ClientError::Invalid(_)));
    let CardFetch::Fresh { card, signed, .. } = fetch_card(&http, &format!("{url}/legacy"), None, None).await.unwrap() else { panic!() };
    assert!(signed, "signatures are reported (not verified)");
    assert_eq!(card.supported_interfaces[0].protocol_version, "0.3");
}

#[test]
fn agent_urls_are_https_or_loopback_http() {
    assert!(check_agent_url("https://agent.example.com/a2a").is_ok());
    assert!(check_agent_url("http://localhost:41242").is_ok());
    assert!(check_agent_url("http://127.0.0.1:8080/").is_ok());
    assert!(check_agent_url("http://[::1]:8080/").is_ok());
    assert!(check_agent_url("http://agent.example.com").is_err(), "plain http to another host");
    assert!(check_agent_url("http://192.168.1.5:8080").is_err());
    assert!(check_agent_url("https://user:pw@agent.example.com").is_err());
    assert!(check_agent_url("ftp://x").is_err());
    assert!(check_agent_url("agent.example.com").is_err());
    assert_eq!(card_url("https://a.example/api/").unwrap().as_str(), "https://a.example/api/.well-known/agent-card.json");
    assert_eq!(card_url("https://a.example/card.json").unwrap().as_str(), "https://a.example/card.json");
}

#[test]
fn the_sse_parser_handles_crlf_multiline_data_comments_and_caps() {
    let mut p = SseParser { buf: Vec::new() };
    assert!(p.push(b": keep-alive\n\n").unwrap().is_empty());
    assert_eq!(p.push(b"id: 1\ndata: {\"a\":\ndata: 1}\n\nda").unwrap(), vec!["{\"a\":\n1}".to_string()]);
    assert_eq!(p.push(b"ta: x\r\n\r\n").unwrap(), vec!["x".to_string()]);
    let mut p = SseParser { buf: Vec::new() };
    assert_eq!(p.push(&vec![b'a'; EVENT_MAX_BYTES + 1]).unwrap_err(), ClientError::TooLarge);
}
