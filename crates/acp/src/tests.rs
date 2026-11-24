//! In-process end-to-end tests of the host against a scripted fake ACP agent
//! (SDK `Agent` role over `Channel::duplex`, no subprocess, no paid agent).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::v1::schema as acp;
use agent_client_protocol::v1::{ConnectionTo, Responder};
use agent_client_protocol::{self as sdk, Agent as AgentRole, Channel, Client};
use krypto_protocol as pp;
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::{AcpHost, HostEvent};

fn j<T: serde::de::DeserializeOwned>(v: Value) -> T {
    serde_json::from_value(v).expect("valid ACP json")
}

struct TempDir(PathBuf);
impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("krypto-acp-host-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(p.join("root")).unwrap();
        std::fs::write(p.join("outside.txt"), "secret").unwrap();
        TempDir(p)
    }
    fn root(&self) -> PathBuf {
        self.0.join("root")
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The machine every test host runs on.
const ME: &str = "abcdef012345";

fn test_agent() -> pp::Agent {
    j(json!({"id": "fake", "machineId": ME, "name": "Fake", "command": ["fake-agent"], "installed": true, "source": "custom"}))
}

async fn run_turn(
    cx: ConnectionTo<Client>,
    req: acp::PromptRequest,
    responder: Responder<acp::PromptResponse>,
    root: PathBuf,
    outside: PathBuf,
) -> Result<(), sdk::Error> {
    let sid = req.session_id.to_string();
    let text = match req.prompt.first() {
        Some(acp::ContentBlock::Text(t)) => t.text.clone(),
        _ => String::new(),
    };
    let notify = |update: Value| cx.send_notification(j::<acp::SessionNotification>(json!({"sessionId": sid, "update": update})));
    let ask = || {
        j::<acp::RequestPermissionRequest>(json!({
            "sessionId": sid,
            "toolCall": {"toolCallId": "t1"},
            "options": [
                {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                {"optionId": "deny", "name": "Deny", "kind": "reject_once"}
            ]
        }))
    };
    let file = root.join("note.txt").to_string_lossy().into_owned();
    match text.as_str() {
        "work" => {
            notify(json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "pondering"}}))?;
            notify(json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "Write note", "kind": "edit",
                          "status": "in_progress", "locations": [{"path": file}]}))?;
            let perm = cx.send_request(ask()).block_task().await?;
            let allowed = matches!(&perm.outcome, acp::RequestPermissionOutcome::Selected(s) if s.option_id.to_string() == "allow");
            let mut report = format!("allowed={allowed}");
            if allowed {
                cx.send_request(j::<acp::WriteTextFileRequest>(json!({"sessionId": sid, "path": file, "content": "hello\nworld\n"})))
                    .block_task()
                    .await?;
                let read = cx
                    .send_request(j::<acp::ReadTextFileRequest>(json!({"sessionId": sid, "path": file, "line": 2, "limit": 1})))
                    .block_task()
                    .await?;
                report += &format!(" read={}", read.content.trim_end());
                let escaped = cx
                    .send_request(j::<acp::ReadTextFileRequest>(json!({"sessionId": sid, "path": outside.to_string_lossy()})))
                    .block_task()
                    .await;
                report += if escaped.is_err() { " outside=denied" } else { " outside=LEAKED" };
            }
            notify(json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed"}))?;
            notify(json!({"sessionUpdate": "plan", "entries": [{"content": "write note", "priority": "high", "status": "completed"}]}))?;
            notify(json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": report}}))?;
            responder.respond(j(json!({"stopReason": "end_turn"})))
        }
        "wait" => {
            let perm = cx.send_request(ask()).block_task().await?;
            let stop = if matches!(perm.outcome, acp::RequestPermissionOutcome::Cancelled) { "cancelled" } else { "end_turn" };
            responder.respond(j(json!({"stopReason": stop})))
        }
        _ => responder.respond(j(json!({"stopReason": "refusal"}))),
    }
}

/// A scripted ACP agent. Prompt "work" runs a tool with a permission request and fs
/// calls; "wait" blocks on a permission until cancelled; anything else is refused.
async fn fake_agent(transport: Channel, root: PathBuf, outside: PathBuf, closed: Arc<std::sync::Mutex<Vec<String>>>) -> Result<(), sdk::Error> {
    let counter = Arc::new(AtomicU32::new(0));
    let root_list = root.clone();
    AgentRole
        .v1()
        .name("fake-agent")
        .on_receive_request(
            async move |req: acp::InitializeRequest, responder: Responder<acp::InitializeResponse>, _cx: ConnectionTo<Client>| {
                assert!(req.client_capabilities.fs.read_text_file);
                assert!(req.client_capabilities.fs.write_text_file);
                assert!(!req.client_capabilities.terminal);
                responder.respond(j(json!({
                    "protocolVersion": 1,
                    "agentCapabilities": {"loadSession": true, "promptCapabilities": {"image": true}, "sessionCapabilities": {"list": {}, "close": {}}},
                    "agentInfo": {"name": "fake", "version": "1.0.0"}
                })))
            },
            sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: acp::NewSessionRequest, responder: Responder<acp::NewSessionResponse>, _cx: ConnectionTo<Client>| {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                responder.respond(j(json!({"sessionId": format!("sess-{n}")})))
            },
            sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: acp::ListSessionsRequest, responder: Responder<acp::ListSessionsResponse>, _cx: ConnectionTo<Client>| {
                responder.respond(j(json!({"sessions": [
                    {"sessionId": "old-1", "cwd": root_list.to_string_lossy(), "title": "Old work", "updatedAt": "2026-01-01T00:00:00Z"}
                ]})))
            },
            sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: acp::LoadSessionRequest, responder: Responder<acp::LoadSessionResponse>, cx: ConnectionTo<Client>| {
                cx.send_notification(j::<acp::SessionNotification>(json!({
                    "sessionId": req.session_id.to_string(),
                    "update": {"sessionUpdate": "user_message_chunk", "content": {"type": "text", "text": "earlier"}}
                })))?;
                responder.respond(j(json!({})))
            },
            sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: acp::CloseSessionRequest, responder: Responder<acp::CloseSessionResponse>, _cx: ConnectionTo<Client>| {
                closed.lock().unwrap().push(req.session_id.to_string());
                responder.respond(j(json!({})))
            },
            sdk::on_receive_request!(),
        )
        .on_receive_request(
            async move |req: acp::PromptRequest, responder: Responder<acp::PromptResponse>, cx: ConnectionTo<Client>| {
                let (root, outside, turn_cx) = (root.clone(), outside.clone(), cx.clone());
                cx.spawn(run_turn(turn_cx, req, responder, root, outside))
            },
            sdk::on_receive_request!(),
        )
        .connect_to(transport)
        .await
}

struct Setup {
    host: AcpHost,
    rx: UnboundedReceiver<HostEvent>,
    agent: tokio::task::JoinHandle<Result<(), sdk::Error>>,
    /// ACP session ids the fake agent received `session/close` for.
    closed: Arc<std::sync::Mutex<Vec<String>>>,
}

async fn setup(tmp: &TempDir) -> Setup {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let host = AcpHost::new(ME.into(), tx);
    let (client_side, agent_side) = Channel::duplex();
    let closed = Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = tokio::spawn(fake_agent(agent_side, tmp.root(), tmp.0.join("outside.txt"), closed.clone()));
    host.inner.attach_for_test(&test_agent(), &tmp.root(), client_side).await.expect("attach");
    Setup { host, rx, agent, closed }
}

async fn collect_until(rx: &mut UnboundedReceiver<HostEvent>, mut done: impl FnMut(&HostEvent) -> bool) -> Vec<HostEvent> {
    let mut out = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for host event; got so far: {out:#?}"))
            .expect("event channel closed");
        let stop = done(&ev);
        out.push(ev);
        if stop {
            return out;
        }
    }
}

fn events_of(evs: &[HostEvent], kind: pp::EventType) -> Vec<&Value> {
    evs.iter()
        .filter_map(|e| match e {
            HostEvent::Event { kind: k, payload, .. } if *k == kind => Some(payload),
            _ => None,
        })
        .collect()
}

fn statuses(evs: &[HostEvent], session: &str) -> Vec<pp::SessionStatus> {
    evs.iter()
        .filter_map(|e| match e {
            HostEvent::Session(s) if s.id == session => Some(s.status),
            _ => None,
        })
        .collect()
}

fn is_event(e: &HostEvent, kind: pp::EventType) -> bool {
    matches!(e, HostEvent::Event { kind: k, .. } if *k == kind)
}

#[tokio::test]
async fn full_turn_with_permission_and_fs() {
    let tmp = TempDir::new();
    let Setup { host, mut rx, .. } = setup(&tmp).await;

    let caps = collect_until(&mut rx, |e| matches!(e, HostEvent::AgentCapabilities { .. })).await;
    let Some(HostEvent::AgentCapabilities { agent_id, capabilities }) = caps.last() else { unreachable!() };
    assert_eq!(agent_id, "fake");
    assert_eq!(
        capabilities,
        &pp::AgentCapabilities { load_session: true, list_sessions: true, resume_session: false, prompt_image: true, prompt_audio: false }
    );

    let session = host.start_session(&test_agent(), tmp.root(), "hub-1".into(), Some("work".into())).await.unwrap();
    assert_eq!(session.id, "hub-1");
    assert_eq!(session.machine_id, ME);
    assert_eq!(session.acp_session_id.as_deref(), Some("sess-1"));
    assert_eq!(session.source, pp::SessionSource::Owned);
    assert_eq!(session.title.as_deref(), Some("work"));

    let evs = collect_until(&mut rx, |e| matches!(e, HostEvent::PermissionRequest(_))).await;
    let Some(HostEvent::PermissionRequest(req)) = evs.last() else { unreachable!() };
    assert_eq!(req.session_id, "hub-1");
    assert_eq!(req.agent_id, "fake");
    assert_eq!(req.machine_id, ME);
    // Request ids are global: `${machine_id}:${uuid}`.
    let (m, local) = pp::parse_global_id(&req.request_id).expect("global request id");
    assert_eq!(m, ME);
    assert!(uuid::Uuid::parse_str(local).is_ok(), "{}", req.request_id);
    assert!(evs.iter().all(|e| !matches!(e, HostEvent::Session(s) if s.machine_id != ME)));
    // The permission's toolCall only carried an id; kind/title come from the earlier tool_call.
    assert_eq!(req.tool_call.kind, Some(pp::ToolKind::Edit));
    assert_eq!(req.tool_call.title.as_deref(), Some("Write note"));
    assert_eq!(req.options.len(), 2);
    assert_eq!(events_of(&evs, pp::EventType::UserMessage), vec![&json!({"text": "work"})]);
    assert_eq!(events_of(&evs, pp::EventType::AgentThoughtChunk), vec![&json!({"text": "pondering"})]);
    let tool = events_of(&evs, pp::EventType::ToolCall);
    assert_eq!(tool[0]["kind"], "edit");
    assert_eq!(tool[0]["status"], "in_progress");
    let st = statuses(&evs, "hub-1");
    assert_eq!(st.first(), Some(&pp::SessionStatus::Idle));
    assert!(st.contains(&pp::SessionStatus::Thinking));
    assert!(st.contains(&pp::SessionStatus::Working));

    // Bad option is rejected and leaves the request pending.
    assert!(host.resolve_permission(&req.request_id, Some("nope".into())).await.is_err());
    // A second prompt while the turn runs is refused.
    assert!(host.prompt("hub-1", "again".into()).await.is_err());

    let request_id = req.request_id.clone();
    // waiting_permission is announced before the request, so an immediate (rule) resolution wins.
    assert_eq!(st.last(), Some(&pp::SessionStatus::WaitingPermission));
    host.resolve_permission(&request_id, Some("allow".into())).await.unwrap();
    assert!(host.resolve_permission(&request_id, Some("allow".into())).await.is_err(), "already resolved");

    let evs = collect_until(&mut rx, |e| matches!(e, HostEvent::Session(s) if s.status == pp::SessionStatus::Done)).await;
    let update = events_of(&evs, pp::EventType::ToolCallUpdate);
    assert_eq!(update[0]["status"], "completed");
    assert_eq!(update[0]["kind"], "edit", "updates are merged with the original tool call");
    assert_eq!(
        events_of(&evs, pp::EventType::Plan),
        vec![&json!({"entries": [{"content": "write note", "priority": "high", "status": "completed"}]})]
    );
    assert_eq!(
        events_of(&evs, pp::EventType::AgentMessageChunk),
        vec![&json!({"text": "allowed=true read=world outside=denied"})]
    );
    assert_eq!(events_of(&evs, pp::EventType::Stop), vec![&json!({"stopReason": "end_turn"})]);
    let stop_idx = evs.iter().position(|e| is_event(e, pp::EventType::Stop)).unwrap();
    assert!(matches!(evs.last(), Some(HostEvent::Session(_))) && stop_idx < evs.len() - 1, "Stop precedes the done upsert");
    assert_eq!(std::fs::read_to_string(tmp.root().join("note.txt")).unwrap(), "hello\nworld\n");

    // A refused turn ends in error.
    host.prompt("hub-1", "something else".into()).await.unwrap();
    let evs = collect_until(&mut rx, |e| matches!(e, HostEvent::Session(s) if s.status == pp::SessionStatus::Error)).await;
    assert_eq!(events_of(&evs, pp::EventType::Stop), vec![&json!({"stopReason": "refusal"})]);

    host.shutdown().await;
    let evs = collect_until(&mut rx, |e| matches!(e, HostEvent::Session(s) if s.ended_at.is_some())).await;
    assert!(evs.iter().all(|e| !is_event(e, pp::EventType::Error)), "clean shutdown emits no errors");
    assert!(host.prompt("hub-1", "x".into()).await.is_err());
    assert!(host.start_session(&test_agent(), tmp.root(), "hub-9".into(), None).await.is_err());
}

#[tokio::test]
async fn cancel_history_resume_and_process_exit() {
    let tmp = TempDir::new();
    let Setup { host, mut rx, agent, .. } = setup(&tmp).await;
    collect_until(&mut rx, |e| matches!(e, HostEvent::AgentCapabilities { .. })).await;

    let s = host.start_session(&test_agent(), tmp.root(), "hub-1".into(), None).await.unwrap();
    assert_eq!(s.status, pp::SessionStatus::Idle);
    assert!(host.start_session(&test_agent(), tmp.root(), "hub-1".into(), None).await.is_err(), "duplicate hub id");

    host.prompt("hub-1", "wait".into()).await.unwrap();
    let evs = collect_until(&mut rx, |e| matches!(e, HostEvent::PermissionRequest(_))).await;
    let Some(HostEvent::PermissionRequest(req)) = evs.last() else { unreachable!() };
    host.cancel("hub-1").await.unwrap();
    let evs = collect_until(&mut rx, |e| matches!(e, HostEvent::Session(s) if s.status == pp::SessionStatus::Idle)).await;
    let resolved = events_of(&evs, pp::EventType::PermissionResolved);
    assert_eq!(resolved, vec![&json!({"requestId": req.request_id, "outcome": {"kind": "cancelled"}, "by": "user"})]);
    assert_eq!(events_of(&evs, pp::EventType::Stop), vec![&json!({"stopReason": "cancelled"})]);
    assert!(host.resolve_permission(&req.request_id, None).await.is_err(), "cancel cleared the request");

    let history = host.list_history(&test_agent(), Some(tmp.root())).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].acp_session_id, "old-1");
    assert_eq!(history[0].machine_id, ME);
    assert_eq!(history[0].title.as_deref(), Some("Old work"));
    assert_eq!(history[0].updated_at, Some(1_767_225_600_000));
    // No cwd: reuses the live connection for the agent.
    assert_eq!(host.list_history(&test_agent(), None).await.unwrap().len(), 1);

    let resumed = host.resume_session(&test_agent(), "old-1".into(), tmp.root(), "hub-2".into()).await.unwrap();
    assert_eq!(resumed.acp_session_id.as_deref(), Some("old-1"));
    assert_eq!(resumed.machine_id, ME);
    let evs = collect_until(&mut rx, |e| is_event(e, pp::EventType::UserMessage)).await;
    let upsert = evs.iter().position(|e| matches!(e, HostEvent::Session(s) if s.id == "hub-2")).expect("hub-2 upsert");
    assert!(upsert < evs.len() - 1, "session upsert is emitted before the replayed history");
    assert_eq!(events_of(&evs, pp::EventType::UserMessage), vec![&json!({"text": "earlier"})]);
    assert!(
        host.resume_session(&test_agent(), "old-1".into(), tmp.root(), "hub-3".into()).await.is_err(),
        "the same ACP session cannot be opened twice"
    );

    // Agent process dies: both sessions end in error.
    agent.abort();
    let mut ended = std::collections::HashSet::new();
    let evs = collect_until(&mut rx, |e| {
        if let HostEvent::Session(s) = e {
            if s.ended_at.is_some() {
                assert_eq!(s.status, pp::SessionStatus::Error);
                ended.insert(s.id.clone());
            }
        }
        ended.len() == 2
    })
    .await;
    assert_eq!(events_of(&evs, pp::EventType::Error).len(), 2);
    assert!(host.prompt("hub-1", "x".into()).await.is_err());
}

#[test]
fn other_machines_agents_are_refused() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let host = AcpHost::new(ME.into(), tx);
        assert_eq!(host.machine_id(), ME);
        let mut remote = test_agent();
        remote.machine_id = "zz9988776655".into();
        let here = std::env::current_dir().unwrap();
        let err = host.start_session(&remote, here.clone(), "h".into(), None).await.unwrap_err();
        assert!(err.to_string().contains("belongs to machine `zz9988776655`"), "{err:#}");
        assert!(host.list_history(&remote, None).await.is_err());
        assert!(host.resume_session(&remote, "x".into(), here, "h2".into()).await.is_err());
        assert!(rx.try_recv().is_err(), "nothing was spawned or emitted");
    });
}

#[test]
fn unknown_cwd_is_rejected() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let host = AcpHost::new(ME.into(), tx);
        let missing = Path::new("definitely/not/a/real/dir/xyz");
        let err = host.start_session(&test_agent(), missing.to_path_buf(), "h".into(), None).await.unwrap_err();
        assert!(err.to_string().contains("invalid session cwd"), "{err:#}");
    });
}

const NODE_AGENT: &str = r#"
const rl = require('readline').createInterface({ input: process.stdin });
const send = (m) => process.stdout.write(JSON.stringify(m) + '\n');
let sessions = 0;
rl.on('line', (line) => {
  let m; try { m = JSON.parse(line); } catch { return; }
  if (m.method === 'initialize') {
    send({ jsonrpc: '2.0', id: m.id, result: { protocolVersion: 1, agentCapabilities: {} } });
  } else if (m.method === 'session/new') {
    send({ jsonrpc: '2.0', id: m.id, result: { sessionId: 'node-' + (++sessions) } });
  } else if (m.method === 'session/prompt') {
    const text = m.params.prompt[0].text;
    if (text === 'spawn') {
      const kid = require('child_process').spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'],
        // Windows: libuv kills non-detached children with node (its own job), so detach to get
        // a real orphan. Unix: stay in the process group (detached would setsid out of it).
        { stdio: 'ignore', detached: process.platform === 'win32' });
      send({ jsonrpc: '2.0', method: 'session/update', params: { sessionId: m.params.sessionId,
        update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: 'pid=' + kid.pid } } } });
      send({ jsonrpc: '2.0', id: m.id, result: { stopReason: 'end_turn' } });
      return;
    }
    if (text === 'crash') {
      process.stderr.write('fatal: simulated crash\n', () => process.exit(3));
      return;
    }
    send({ jsonrpc: '2.0', method: 'session/update', params: { sessionId: m.params.sessionId,
      update: { sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: 'hi from node' } } } });
    send({ jsonrpc: '2.0', id: m.id, result: { stopReason: 'end_turn' } });
  } else if (m.id !== undefined && m.method) {
    send({ jsonrpc: '2.0', id: m.id, error: { code: -32601, message: 'Method not found' } });
  }
});
rl.on('close', () => process.exit(0));
"#;

/// Real subprocess over stdio: spawn + PATH resolution + turn + crash detection.
#[tokio::test]
async fn real_subprocess_agent() {
    if which::which("node").is_err() {
        eprintln!("node not on PATH; skipping real_subprocess_agent");
        return;
    }
    let tmp = TempDir::new();
    let script = tmp.0.join("agent.js");
    std::fs::write(&script, NODE_AGENT).unwrap();
    let agent: pp::Agent = j(json!({
        "id": "node-agent", "machineId": ME, "name": "Node", "command": ["node", script.to_string_lossy()],
        "installed": true, "source": "custom"
    }));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let host = AcpHost::new(ME.into(), tx);

    let s = host.start_session(&agent, tmp.root(), "p1".into(), Some("hello".into())).await.unwrap();
    assert_eq!(s.acp_session_id.as_deref(), Some("node-1"));
    let evs = collect_until(&mut rx, |e| matches!(e, HostEvent::Session(s) if s.status == pp::SessionStatus::Done)).await;
    assert!(evs.iter().any(|e| matches!(e, HostEvent::AgentCapabilities { agent_id, .. } if agent_id == "node-agent")));
    assert_eq!(events_of(&evs, pp::EventType::AgentMessageChunk), vec![&json!({"text": "hi from node"})]);
    assert_eq!(events_of(&evs, pp::EventType::Stop), vec![&json!({"stopReason": "end_turn"})]);

    // A second session in the same cwd reuses the process (its counter continues).
    let s2 = host.start_session(&agent, tmp.root(), "p2".into(), None).await.unwrap();
    assert_eq!(s2.acp_session_id.as_deref(), Some("node-2"));

    host.prompt("p1", "crash".into()).await.unwrap();
    // The crash ends every session served by that process.
    let mut ended = std::collections::HashSet::new();
    let evs = collect_until(&mut rx, |e| {
        if let HostEvent::Session(s) = e {
            if s.ended_at.is_some() {
                assert_eq!(s.status, pp::SessionStatus::Error);
                ended.insert(s.id.clone());
            }
        }
        ended.len() == 2
    })
    .await;
    let errors = events_of(&evs, pp::EventType::Error);
    let msg = errors.iter().filter_map(|p| p["message"].as_str()).find(|m| m.contains("simulated crash"));
    assert!(msg.is_some(), "process exit reports stderr: {errors:?}");
    assert!(msg.unwrap().contains('3'), "exit code reported: {msg:?}");
    host.shutdown().await;
}

#[tokio::test]
async fn non_acp_program_fails_initialize() {
    let tmp = TempDir::new();
    let agent: pp::Agent = j(json!({
        "id": "not-acp", "machineId": ME, "name": "cargo", "command": ["cargo", "--version"], "installed": true, "source": "custom"
    }));
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let host = AcpHost::new(ME.into(), tx);
    let err = host.start_session(&agent, tmp.root(), "x".into(), None).await.unwrap_err();
    assert!(err.to_string().contains("initialize failed"), "{err:#}");

    let missing: pp::Agent = j(json!({
        "id": "missing", "machineId": ME, "name": "missing", "command": ["no-such-agent-binary-xyz"], "installed": false, "source": "custom"
    }));
    let err = host.start_session(&missing, tmp.root(), "y".into(), None).await.unwrap_err();
    assert!(err.to_string().contains("could not find agent executable"), "{err:#}");
}

#[tokio::test]
async fn close_session_cancels_and_stops_process_with_last_session() {
    let tmp = TempDir::new();
    let Setup { host, mut rx, agent, closed } = setup(&tmp).await;
    collect_until(&mut rx, |e| matches!(e, HostEvent::AgentCapabilities { .. })).await;
    host.start_session(&test_agent(), tmp.root(), "hub-1".into(), None).await.unwrap();
    host.start_session(&test_agent(), tmp.root(), "hub-2".into(), None).await.unwrap();

    host.prompt("hub-1", "wait".into()).await.unwrap();
    let evs = collect_until(&mut rx, |e| matches!(e, HostEvent::PermissionRequest(_))).await;
    let Some(HostEvent::PermissionRequest(req)) = evs.last() else { unreachable!() };
    host.close_session("hub-1").await.unwrap();
    let evs = collect_until(&mut rx, |e| matches!(e, HostEvent::Session(s) if s.id == "hub-1" && s.ended_at.is_some())).await;
    assert_eq!(
        events_of(&evs, pp::EventType::PermissionResolved),
        vec![&json!({"requestId": req.request_id, "outcome": {"kind": "cancelled"}, "by": "system"})]
    );
    let Some(HostEvent::Session(s)) = evs.last() else { unreachable!() };
    assert_eq!(s.status, pp::SessionStatus::Done);
    assert_eq!(closed.lock().unwrap().as_slice(), ["sess-1"], "agent got session/close");
    host.close_session("hub-1").await.expect("closing twice is a no-op");
    assert!(host.prompt("hub-1", "x".into()).await.is_err());

    // The process still serves hub-2.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!agent.is_finished(), "agent must stay up while hub-2 is open");
    host.prompt("hub-2", "anything".into()).await.unwrap();
    let evs = collect_until(&mut rx, |e| is_event(e, pp::EventType::Stop)).await;
    assert_eq!(events_of(&evs, pp::EventType::Stop), vec![&json!({"stopReason": "refusal"})]);

    // Closing the last session stops the agent connection.
    host.close_session("hub-2").await.unwrap();
    collect_until(&mut rx, |e| matches!(e, HostEvent::Session(s) if s.id == "hub-2" && s.ended_at.is_some())).await;
    let finished = tokio::time::timeout(Duration::from_secs(5), agent).await;
    assert!(finished.is_ok(), "agent connection closed after its last session");
    assert!(host.inner.find_live_conn_for_test(&test_agent()).is_none(), "stopped connection is not reused");
}

fn pid_alive(pid: u32) -> bool {
    std::process::Command::new(which::which("node").expect("node"))
        .args(["-e", &format!("try {{ process.kill({pid}, 0); process.exit(0) }} catch (e) {{ process.exit(1) }}")])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// The agent's own children die with it (Job Object / process group), even though
/// the agent exits cleanly and would leave them orphaned.
#[tokio::test]
async fn closing_last_session_kills_process_tree() {
    if which::which("node").is_err() {
        eprintln!("node not on PATH; skipping closing_last_session_kills_process_tree");
        return;
    }
    let tmp = TempDir::new();
    let script = tmp.0.join("agent.js");
    std::fs::write(&script, NODE_AGENT).unwrap();
    let agent: pp::Agent = j(json!({
        "id": "node-tree", "machineId": ME, "name": "Node", "command": ["node", script.to_string_lossy()],
        "installed": true, "source": "custom"
    }));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let host = AcpHost::new(ME.into(), tx);
    host.start_session(&agent, tmp.root(), "t1".into(), Some("spawn".into())).await.unwrap();
    let evs = collect_until(&mut rx, |e| is_event(e, pp::EventType::Stop)).await;
    let text = events_of(&evs, pp::EventType::AgentMessageChunk)[0]["text"].as_str().unwrap().to_string();
    let pid: u32 = text.strip_prefix("pid=").unwrap().parse().unwrap();
    assert!(pid_alive(pid), "grandchild should be running");

    host.close_session("t1").await.unwrap();
    let mut dead = false;
    for _ in 0..80 {
        if !pid_alive(pid) {
            dead = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    if !dead {
        // Clean up before failing so the test never leaks a process.
        let _ = std::process::Command::new(which::which("node").unwrap())
            .args(["-e", &format!("try {{ process.kill({pid}) }} catch (e) {{}}")])
            .status();
    }
    assert!(dead, "agent's child process {pid} survived closing the last session");
    host.shutdown().await;
}
