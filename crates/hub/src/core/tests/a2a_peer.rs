//! A4: machine-to-machine A2A inside the encrypted peer link (see `core/a2a_peer.rs`), with two
//! real in-process hubs paired over loopback.

use super::*;
use crate::store::IntentRole;
use pawl_a2a::Caller;
use pawl_a2a::spec::TaskState;
use pawl_peer::PeerEvent;
use pawl_protocol::{Presence, PROTOCOL_VERSION};

/// A (owner, listener on, fake ACP host) and B (origin), paired, B knowing A speaks A2A; a folder on
/// A that A already used (so remote starts may run there).
async fn two_hubs() -> (Hub, FakeHost, Hub, tempfile::TempDir, tempfile::TempDir, String) {
    let da = tempfile::tempdir().unwrap();
    let (a, a_fake) = peer_hub_with_fake(da.path(), MID, "Desk", Some("127.0.0.1:0")).await;
    let (b, db) = peer_hub(OTHER, "Laptop", None).await;
    pair(&a, &b).await;
    wait_until("b learns a's protocol", || {
        let b = b.clone();
        async move { b.peer_speaks_a2a(MID) }
    })
    .await;
    let cwd = da.path().to_string_lossy().into_owned();
    ack_result(a.handle(ClientMsg::StartSession { machine_id: None, confirmed: None, id: "s".into(), agent_id: "codex".into(), cwd: cwd.clone(), prompt: None }, &actor()).await);
    (a, a_fake, b, da, db, cwd)
}

fn start_on_a(cwd: &str, prompt: Option<&str>, confirmed: Option<bool>) -> ClientMsg {
    ClientMsg::StartSession { id: "rs".into(), agent_id: "codex".into(), cwd: cwd.into(), prompt: prompt.map(Into::into), machine_id: Some(MID.into()), confirmed }
}

#[tokio::test]
async fn start_prompt_and_cancel_between_v9_hubs_are_a2a() {
    let (a, a_fake, b, _da, _db, cwd) = two_hubs().await;
    assert_eq!(PROTOCOL_VERSION, 9);
    assert!(a.peer_speaks_a2a(OTHER), "both ways");

    // Start on A from B: over A2A, same ack shape as the intent.
    let v = ack_result(b.handle(start_on_a(&cwd, Some("run the tests"), Some(true)), &actor()).await);
    let s: Session = serde_json::from_value(v["session"].clone()).unwrap();
    assert_eq!(s.machine_id, MID);
    assert!(a_fake.calls().contains(&format!("prompt {} run the tests", s.id)), "{:?}", a_fake.calls());
    assert_eq!(a.tasks().owner_of(&s.id), Caller::Peer(OTHER.into()), "the context is bound to the peer");
    assert!(a.st().remote_driven.contains(&s.id), "rules never auto-approve it");
    let task = a.tasks().active_task(&s.id).unwrap();
    // Both sides logged the call, with the same id.
    let origin = &b.store().intent_log(1).unwrap()[0];
    let owner = &a.store().intent_log(1).unwrap()[0];
    assert_eq!((origin.role, origin.kind.as_str(), origin.status.as_str()), (IntentRole::Origin, "start", "applied"));
    assert_eq!((owner.role, owner.kind.as_str(), owner.status.as_str()), (IntentRole::Owner, "start", "applied"));
    assert_eq!(origin.intent_id, owner.intent_id);
    assert_eq!(owner.client, "desktop");
    // B's mirror gets the session as before.
    wait_until("b mirrors the start", || {
        let (b, id) = (b.clone(), s.id.clone());
        async move { b.welcome().sessions.iter().any(|x| x.id == id) }
    })
    .await;

    // Prompt from B: a new task on A.
    a.on_host_event(HostEvent::Event { session_id: s.id.clone(), kind: EventType::Stop, payload: json!({ "stopReason": "end_turn" }) }).await;
    assert_eq!(ack_result(b.handle(ClientMsg::Prompt { id: "p".into(), session_id: s.id.clone(), text: "and lint".into(), images: Vec::new() }, &actor()).await), json!({}));
    assert!(a_fake.calls().contains(&format!("prompt {} and lint", s.id)));
    let second = a.tasks().active_task(&s.id).unwrap();
    assert_ne!(second.id, task.id);
    assert_eq!(b.store().intent_log(1).unwrap()[0].kind, "prompt");

    // Cancel from B: CancelTask on the running task.
    assert_eq!(ack_result(b.handle(ClientMsg::Cancel { id: "c".into(), session_id: s.id.clone() }, &actor()).await), json!({}));
    assert!(a_fake.calls().contains(&format!("cancel {}", s.id)));
    assert_eq!(a.a2a_get(&second.id, None, &Caller::Local).unwrap().status.state, TaskState::Canceled);
    // Nothing running: a cancel has nothing to do.
    assert_eq!(ack_result(b.handle(ClientMsg::Cancel { id: "c2".into(), session_id: s.id.clone() }, &actor()).await), json!({}));
}

#[tokio::test]
async fn refusals_keep_the_intent_codes() {
    let (a, a_fake, b, _da, _db, cwd) = two_hubs().await;
    // Unconfirmed: refused on B before anything is sent.
    assert_eq!(ack_error(b.handle(start_on_a(&cwd, None, None), &actor()).await), "unconfirmed");
    // A folder A does not know: forbidden by A.
    let elsewhere = tempfile::tempdir().unwrap();
    assert_eq!(ack_error(b.handle(start_on_a(&elsewhere.path().to_string_lossy(), None, Some(true)), &actor()).await), "forbidden");
    let owner = &a.store().intent_log(1).unwrap()[0];
    assert_eq!((owner.kind.as_str(), owner.status.as_str(), owner.reason.as_deref()), ("start", "rejected", Some("forbidden")));
    // An agent A does not have: invalid.
    let mut msg = start_on_a(&cwd, None, Some(true));
    if let ClientMsg::StartSession { agent_id, .. } = &mut msg {
        *agent_id = "goose".into();
    }
    assert_eq!(ack_error(b.handle(msg, &actor()).await), "invalid");
    // An unknown session: not_found, for prompt and cancel.
    let ghost = global_id(MID, "ghost");
    assert_eq!(ack_error(b.handle(ClientMsg::Prompt { id: "p".into(), session_id: ghost.clone(), text: "hi".into(), images: Vec::new() }, &actor()).await), "not_found");
    assert_eq!(ack_error(b.handle(ClientMsg::Cancel { id: "c".into(), session_id: ghost }, &actor()).await), "not_found");
    assert!(!a_fake.calls().iter().any(|c| c.contains("goose") || c.contains("ghost")));
    // Remote control off on A: forbidden.
    a.update_settings(&pawl_protocol::HubSettingsPatch { peer: Some(pawl_protocol::HubSettingsPeerPatch { accept_intents: Some(false), ..Default::default() }), ..Default::default() }, &actor())
        .await
        .unwrap();
    assert_eq!(ack_error(b.handle(start_on_a(&cwd, None, Some(true)), &actor()).await), "forbidden");
}

#[tokio::test]
async fn approvals_between_v9_hubs_stay_intents() {
    let (a, a_fake, b, _da, _db, cwd) = two_hubs().await;
    let v = ack_result(a.handle(ClientMsg::StartSession { machine_id: None, confirmed: None, id: "s2".into(), agent_id: "codex".into(), cwd, prompt: Some("go".into()) }, &actor()).await);
    let s: Session = serde_json::from_value(v["session"].clone()).unwrap();
    let req = permission(&s.id, ToolKind::Execute);
    a.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    wait_until("b mirrors the request", || {
        let (b, rid) = (b.clone(), req.request_id.clone());
        async move { b.welcome().pending_permissions.iter().any(|r| r.request_id == rid) }
    })
    .await;
    let approve = ClientMsg::ResolvePermission { id: "ok".into(), request_id: req.request_id.clone(), option_id: Some("allow".into()), confirmed: Some(true) };
    assert_eq!(ack_result(b.handle(approve, &actor()).await), json!({}));
    assert!(a_fake.calls().contains(&format!("resolve {} allow", req.request_id)));
    let origin = &b.store().intent_log(1).unwrap()[0];
    assert_eq!((origin.kind.as_str(), origin.status.as_str()), ("approve", "applied"));
    let audit = &a.store().audit_log(1).unwrap()[0];
    assert_eq!(audit.origin.as_ref().map(|o| o.intent_id.clone()), Some(origin.intent_id.clone()), "the intent path, with its audit origin");
}

#[tokio::test]
async fn a_v8_peer_keeps_getting_intents() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    let presence = |version: Option<u32>| Presence {
        machine: Machine { id: OTHER.into(), name: "Mac mini".into(), os: pawl_protocol::MachineOs::Macos, last_seen: 1, online: true, hub_version: None, user_active_at: None },
        agents: vec![],
        repos: vec![],
        pets: vec![],
        user_active_at: None,
        ts: pawl_protocol::now_ms(),
        protocol_version: version,
    };
    assert!(!f.hub.peer_speaks_a2a(OTHER), "no presence yet");
    f.hub.on_peer_event(PeerEvent::LinkUp { device_id: OTHER.into(), machine: presence(None).machine }).await;
    f.hub.on_peer_event(PeerEvent::Presence { device_id: OTHER.into(), presence: presence(None) }).await;
    assert!(!f.hub.peer_speaks_a2a(OTHER), "a v8 presence");
    f.hub.on_peer_event(PeerEvent::Presence { device_id: OTHER.into(), presence: presence(Some(9)) }).await;
    assert!(f.hub.peer_speaks_a2a(OTHER));
    f.hub.on_peer_event(PeerEvent::Presence { device_id: OTHER.into(), presence: presence(Some(8)) }).await;
    assert!(!f.hub.peer_speaks_a2a(OTHER));
    // This hub tells peers its own version.
    assert_eq!(f.hub.presence_snapshot().protocol_version, Some(PROTOCOL_VERSION));
}

#[tokio::test]
async fn an_owner_that_does_not_answer_times_out_over_a2a_too() {
    let (a, _a_fake, b, _da, _db, cwd) = two_hubs().await;
    a.drop_intents(true);
    assert_eq!(ack_error(b.handle(start_on_a(&cwd, None, Some(true)), &actor()).await), "timeout");
    assert_eq!(b.store().intent_log(1).unwrap()[0].status, "timeout");
}


// ---------------------------------------------------------------- security review fixes

use crate::core::a2a_peer::STREAMS_PER_PEER;

fn start_rpc(call: &str, method: &str, cwd: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "id": call, "method": method,
        "params": {
            "message": { "messageId": call, "role": "ROLE_USER", "parts": [{ "text": "go" }],
                         "metadata": { pawl_a2a::DELEGATE_EXT: { "agentId": "codex", "cwd": cwd } } },
            "configuration": { "returnImmediately": true }
        }
    })
}

fn starts(fake: &FakeHost) -> usize {
    fake.calls().iter().filter(|c| c.starts_with("start codex")).count()
}

#[tokio::test]
async fn a_repeated_call_id_never_runs_twice_and_streams_stop_when_the_link_is_gone() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    start(&f).await; // the folder is known
    let cwd = f.dir.path().to_string_lossy().into_owned();
    let before = starts(&f.fake);
    // A unary start, twice with one id: one start.
    f.hub.answer_a2a(OTHER, "c1", start_rpc("c1", "SendMessage", &cwd), ClientKind::Desktop, true).await;
    f.hub.answer_a2a(OTHER, "c1", start_rpc("c1", "SendMessage", &cwd), ClientKind::Desktop, true).await;
    assert_eq!(starts(&f.fake), before + 1);
    // A streaming start toward a peer that is not linked: forwarding stops at once (no hang), the
    // stream slot is given back, and a repeat of the id does not start again.
    tokio::time::timeout(Duration::from_secs(5), f.hub.answer_a2a(OTHER, "c2", start_rpc("c2", "SendStreamingMessage", &cwd), ClientKind::Desktop, true))
        .await
        .expect("stops when the link is gone");
    assert_eq!(starts(&f.fake), before + 2);
    assert_eq!(f.hub.a2a_peer_streams(OTHER, None), 0);
    f.hub.answer_a2a(OTHER, "c2", start_rpc("c2", "SendStreamingMessage", &cwd), ClientKind::Desktop, true).await;
    assert_eq!(starts(&f.fake), before + 2, "a stream call is remembered too");
    // A peer with all its stream slots in use is refused.
    f.hub.a2a_peer_streams(OTHER, Some(STREAMS_PER_PEER));
    f.hub.answer_a2a(OTHER, "c3", start_rpc("c3", "SendStreamingMessage", &cwd), ClientKind::Desktop, true).await;
    assert_eq!(starts(&f.fake), before + 2);
    let owner = &f.hub.store().intent_log(1).unwrap()[0];
    assert_eq!((owner.intent_id.as_str(), owner.reason.as_deref()), ("c3", Some("rate_limited")));
}

#[tokio::test]
async fn a_dropped_call_leaves_no_waiter() {
    let (a, _a_fake, b, _da, _db, _cwd) = two_hubs().await;
    a.drop_intents(true);
    let b2 = b.clone();
    let call = tokio::spawn(async move { b2.peer_call(MID, "dropped", ClientKind::Desktop, false, "ListTasks", json!({})).await });
    wait_until("the call waits", || {
        let b = b.clone();
        async move { b.a2a_waiting() == 1 }
    })
    .await;
    call.abort();
    let _ = call.await;
    assert_eq!(b.a2a_waiting(), 0, "the waiter went with the call");
}
