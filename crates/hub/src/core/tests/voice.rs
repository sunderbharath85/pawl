//! Approvals by voice (v0.2 item 3) over the hub: voice codes, the spoken rules checked again by the
//! hub, read-backs per chat, wrong-code pauses, the audit / intent log and `permission_resolved`.

use super::*;
use pawl_brain::chat::spoken::{is_voice_code, spell_code};

fn chat_msg(text: &str, via: Option<&str>) -> ClientMsg {
    ClientMsg::Chat { id: "v".into(), pet_id: None, text: text.into(), via: via.map(Into::into), images: Vec::new() }
}

/// Send `text` to the default pet (spoken or typed) and wait for its reply (actions have run by then).
async fn talk(hub: &Hub, rx: &mut broadcast::Receiver<ServerMsg>, text: &str, via: Option<&str>) -> ChatMessage {
    while rx.try_recv().is_ok() {}
    ack_result(hub.handle(chat_msg(text, via), &actor()).await);
    match wait_for(rx, "the pet's reply", |m| matches!(m, ServerMsg::Chat { message } if message.role == ChatRole::Pet)).await {
        ServerMsg::Chat { message } => message,
        _ => unreachable!(),
    }
}

async fn speak(f: &mut Fixture, text: &str) -> ChatMessage {
    talk(&f.hub, &mut f.rx, text, Some("voice")).await
}

/// A request of `kind` in a new session, and its voice code.
async fn ask_for(f: &Fixture, kind: ToolKind) -> (PermissionRequest, String) {
    let s = start(f).await;
    let req = permission(&s.id, kind);
    f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    let code = f.hub.st().pending.get(&req.request_id).and_then(|r| r.voice_code.clone()).expect("a voice code");
    (req, code)
}

fn resolved(f: &Fixture, req: &PermissionRequest) -> bool {
    f.fake.calls().iter().any(|c| c.starts_with(&format!("resolve {}", req.request_id)))
}

#[tokio::test]
async fn every_request_gets_its_own_voice_code_and_history_never_keeps_it() {
    let mut f = fixture(None).await;
    let s = start(&f).await;
    drain(&mut f.rx);
    let mut codes = HashSet::new();
    for _ in 0..40 {
        let mut req = permission(&s.id, ToolKind::Execute);
        req.voice_code = Some("00".into()); // whatever the agent side says is replaced
        f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
        let code = f.hub.st().pending.get(&req.request_id).and_then(|r| r.voice_code.clone()).unwrap();
        assert!(is_voice_code(&code) && code != "00" && !code.starts_with('0'), "{code}");
        assert!(codes.insert(code), "not reused among pending requests");
    }
    // Clients get the code with the request; the stored event history does not.
    let sent: Vec<PermissionRequest> = drain(&mut f.rx)
        .into_iter()
        .filter_map(|m| match m {
            ServerMsg::PermissionRequest { request } => Some(request),
            _ => None,
        })
        .collect();
    assert_eq!(sent.len(), 40);
    assert!(sent.iter().all(|r| r.voice_code.as_deref().is_some_and(is_voice_code)));
    assert!(f.hub.welcome().pending_permissions.iter().all(|r| r.voice_code.is_some()));
    let events = f.hub.store().events(&s.id, None, 500).unwrap();
    let asked: Vec<&HubEvent> = events.iter().filter(|e| e.kind == EventType::RequestPermission).collect();
    assert_eq!(asked.len(), 40);
    assert!(asked.iter().all(|e| e.payload.get("voiceCode").is_none() && e.payload.get("toolCall").is_some()), "{:?}", asked[0].payload);
}

#[tokio::test]
async fn a_spoken_approval_of_a_command_needs_its_code_and_is_audited_as_voice() {
    let mut f = fixture(None).await;
    let (req, code) = ask_for(&f, ToolKind::Execute).await;
    let reply = speak(&mut f, "approve").await;
    assert!(reply.text.contains("Codex on") && reply.text.contains("wants to run a command: `cargo test`"), "{}", reply.text);
    assert!(reply.text.contains(&format!("Say: “approve {}”", spell_code(&code))), "{}", reply.text);
    assert!(!resolved(&f, &req));
    // A wrong code: nothing happens.
    let wrong = if code == "10" { "11" } else { "10" };
    let reply = speak(&mut f, &format!("approve {}", spell_code(wrong))).await;
    assert!(reply.text.contains("That code doesn't match"), "{}", reply.text);
    assert!(!resolved(&f, &req));
    assert!(f.hub.store().audit_log(5).unwrap().is_empty());
    // The right code, said as one number: approved once.
    let said = format!("Approve {code}, please.");
    let reply = speak(&mut f, &said).await;
    assert!(reply.text.contains("Approved once"), "{}", reply.text);
    assert!(f.fake.calls().contains(&format!("resolve {} allow", req.request_id)), "{:?}", f.fake.calls());
    let audit = &f.hub.store().audit_log(5).unwrap()[0];
    assert_eq!((audit.via.as_deref(), audit.transcript.as_deref()), (Some("voice"), Some(said.as_str())));
    assert!(audit.actor.starts_with("voice:chat:desktop#"), "{}", audit.actor);
    assert_eq!(audit.outcome, AuditOutcome::Allowed);
    // permission_resolved (history) says it was spoken.
    let events = f.hub.store().events(&req.session_id, None, 100).unwrap();
    let done = events.iter().find(|e| e.kind == EventType::PermissionResolved).expect("resolved event");
    assert_eq!(done.payload["via"], "voice", "{}", done.payload);
    // A typed decision is not marked as spoken.
    let (req2, _) = ask_for(&f, ToolKind::Execute).await;
    ack_result(f.hub.handle(ClientMsg::ResolvePermission { id: "r".into(), request_id: req2.request_id.clone(), option_id: Some("reject".into()), confirmed: None }, &actor()).await);
    let audit = &f.hub.store().audit_log(5).unwrap()[0];
    assert_eq!((audit.via.as_deref(), audit.transcript.as_deref()), (None, None));
}

#[tokio::test]
async fn the_transcript_is_cut_to_300_characters_in_the_audit_log() {
    let mut f = fixture(None).await;
    let (req, code) = ask_for(&f, ToolKind::Execute).await;
    let said = format!("approve {} {}", spell_code(&code), "and that is all I wanted to say ".repeat(20));
    speak(&mut f, &said).await;
    assert!(resolved(&f, &req));
    let t = f.hub.store().audit_log(1).unwrap()[0].transcript.clone().unwrap();
    assert!((290..=300).contains(&t.chars().count()), "{}", t.chars().count());
    assert!(said.starts_with(&t));
}

#[tokio::test]
async fn a_local_read_is_read_back_then_approved_and_the_read_back_lasts_one_message() {
    let mut f = fixture(None).await;
    let (req, _) = ask_for(&f, ToolKind::Read).await;
    let reply = speak(&mut f, "approve").await;
    assert!(reply.text.contains("wants to read a file: /repo/src/lib.rs") && reply.text.contains("Say “approve”"), "{}", reply.text);
    assert!(!resolved(&f, &req));
    // A typed message in between: the read-back is gone, the next "approve" reads back again.
    talk(&f.hub, &mut f.rx, "help", None).await;
    speak(&mut f, "approve").await;
    assert!(!resolved(&f, &req));
    // A bare "yes" is never enough by voice.
    let reply = speak(&mut f, "yes").await;
    assert!(reply.text.contains("isn't enough by voice"), "{}", reply.text);
    assert!(!resolved(&f, &req));
    let reply = speak(&mut f, "yes, approve").await;
    assert!(reply.text.contains("Approved once"), "{}", reply.text);
    assert!(resolved(&f, &req));
    assert_eq!(f.hub.store().audit_log(1).unwrap()[0].via.as_deref(), Some("voice"));
}

#[tokio::test]
async fn always_allow_and_unchecked_codes_are_refused_by_the_hub_too() {
    let mut f = fixture(None).await;
    let (mut req, code) = ask_for(&f, ToolKind::Execute).await;
    req.options.push(PermissionOption { option_id: "always".into(), name: "Always".into(), kind: PermissionOptionKind::AllowAlways });
    f.hub.st().pending.get_mut(&req.request_id).unwrap().options = req.options.clone();
    let reply = speak(&mut f, &format!("always allow {}", spell_code(&code))).await;
    assert!(reply.text.contains("I can't “always allow” by voice"), "{}", reply.text);
    assert!(!resolved(&f, &req));
    // The hub's own check, whatever the brain said.
    let a = actor();
    let said = format!("approve {}", spell_code(&code));
    let e = f.hub.resolve_by_voice(&req.request_id, Some("always".into()), &a, &said).await.unwrap_err();
    assert!(format!("{e:#}").contains("always allow"), "{e:#}");
    let e = f.hub.resolve_by_voice(&req.request_id, Some("allow".into()), &a, "approve").await.unwrap_err();
    assert!(format!("{e:#}").contains("code does not match"), "{e:#}");
    assert!(!resolved(&f, &req));
    // Deny needs no code.
    f.hub.resolve_by_voice(&req.request_id, Some("reject".into()), &a, "deny").await.unwrap();
    assert!(f.fake.calls().contains(&format!("resolve {} reject", req.request_id)));
    // A local edit: the hub leaves the read-back to the brain, no code needed.
    let (edit, _) = ask_for(&f, ToolKind::Edit).await;
    f.hub.resolve_by_voice(&edit.request_id, Some("allow".into()), &a, "approve").await.unwrap();
    assert!(resolved(&f, &edit));
}

#[tokio::test]
async fn three_wrong_codes_pause_approving_by_voice() {
    let mut f = fixture(None).await;
    let (req, code) = ask_for(&f, ToolKind::Execute).await;
    let wrong: Vec<String> = (10..=99).map(|n: u8| n.to_string()).filter(|c| *c != code).take(3).collect();
    for w in &wrong {
        assert!(speak(&mut f, &format!("approve {w}")).await.text.contains("doesn't match"));
    }
    let reply = speak(&mut f, &format!("approve {}", spell_code(&code))).await;
    assert!(reply.text.contains("Too many wrong codes"), "{}", reply.text);
    assert!(!resolved(&f, &req));
    let e = f.hub.resolve_by_voice(&req.request_id, Some("allow".into()), &actor(), &format!("approve {code}")).await.unwrap_err();
    assert!(format!("{e:#}").contains("paused"), "{e:#}");
    // Deny still works.
    let reply = speak(&mut f, &format!("deny {}", spell_code(&code))).await;
    assert!(reply.text.contains("Denied"), "{}", reply.text);
    assert!(f.fake.calls().contains(&format!("resolve {} reject", req.request_id)));
}

#[tokio::test]
async fn chat_via_is_voice_or_nothing() {
    let f = fixture(None).await;
    let e = ack_error(f.hub.handle(chat_msg("approve", Some("telepathy")), &actor()).await);
    assert_eq!(e, "invalid");
    ack_result(f.hub.handle(chat_msg("hi", Some("voice")), &actor()).await);
}

#[tokio::test]
async fn a_mirrored_request_keeps_only_a_well_formed_code_and_voice_goes_to_its_owner() {
    let mut f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    let sid = global_id(OTHER, "s1");
    feed(&f.hub, OTHER, MirrorUpdate::Session { session: remote_session(OTHER, "s1") }).await;
    let mut bad = remote_request(OTHER, "r-bad", &sid);
    bad.voice_code = Some("4x7".into());
    feed(&f.hub, OTHER, MirrorUpdate::PermissionRequest { request: bad.clone() }).await;
    assert_eq!(f.hub.mirror_pending().into_iter().find(|r| r.request_id == bad.request_id).map(|r| r.voice_code), Some(None));
    let mut good = remote_request(OTHER, "r1", &sid);
    good.voice_code = Some("62".into());
    feed(&f.hub, OTHER, MirrorUpdate::PermissionRequest { request: good.clone() }).await;
    // A read on another machine: the read-back alone is not enough.
    speak(&mut f, "approve").await; // "more than one request": both are the other machine's
    let reply = speak(&mut f, "approve 6 2").await;
    // The peer is not linked in this fixture: the confirmed intent is sent and logged as offline.
    assert!(reply.text.contains("Approved once") && reply.text.contains("offline"), "{}", reply.text);
    let log = &f.hub.store().intent_log(1).unwrap()[0];
    assert_eq!((log.kind.as_str(), log.status.as_str(), log.via.as_deref(), log.transcript.as_deref()), ("approve", "offline", Some("voice"), Some("approve 6 2")));
    // The request without a code cannot be approved by voice.
    let e = f.hub.resolve_by_voice(&bad.request_id, Some("allow".into()), &actor(), "approve 4 7").await.unwrap_err();
    assert!(format!("{e:#}").contains("code does not match"), "{e:#}");
}

/// End to end over real peer links: B's spoken "approve <code>" for A's request is the confirmation
/// on B; A applies it under its own rules. Typed chat on B still never decides it.
#[tokio::test]
async fn two_hubs_a_spoken_code_approves_another_machines_request() {
    let da = tempfile::tempdir().unwrap();
    let (a, a_fake) = peer_hub_with_fake(da.path(), MID, "Desk", Some("127.0.0.1:0")).await;
    let (b, _db) = peer_hub(OTHER, "Laptop", None).await;
    pair(&a, &b).await;
    let cwd = da.path().to_string_lossy().into_owned();
    let v = ack_result(a.handle(ClientMsg::StartSession { machine_id: None, confirmed: None, id: "s".into(), agent_id: "codex".into(), cwd, prompt: None }, &actor()).await);
    let s: Session = serde_json::from_value(v["session"].clone()).unwrap();
    let req = permission(&s.id, ToolKind::Execute);
    a.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    let code = a.st().pending.get(&req.request_id).and_then(|r| r.voice_code.clone()).unwrap();
    wait_until("b mirrors the request with its code", || {
        let (b, rid, code) = (b.clone(), req.request_id.clone(), code.clone());
        async move { b.welcome().pending_permissions.iter().any(|r| r.request_id == rid && r.voice_code.as_deref() == Some(code.as_str())) }
    })
    .await;
    let mut brx = b.subscribe();
    let said = format!("approve {}", spell_code(&code));
    // Typed: unchanged, chat never decides another machine's request.
    let reply = talk(&b, &mut brx, "approve", None).await;
    assert!(reply.text.contains("won't approve it from chat"), "{}", reply.text);
    talk(&b, &mut brx, &said, None).await;
    assert!(b.store().intent_log(5).unwrap().is_empty() && !a_fake.calls().iter().any(|c| c.starts_with("resolve")));
    // Spoken without the code: read back with the machine, nothing sent.
    let reply = talk(&b, &mut brx, "approve", Some("voice")).await;
    assert!(reply.text.contains("Codex on Desk wants to run a command") && reply.text.contains(&format!("Say: “{said}”")), "{}", reply.text);
    assert!(b.store().intent_log(5).unwrap().is_empty());
    // Spoken with the code: approved on A.
    let reply = talk(&b, &mut brx, &said, Some("voice")).await;
    assert!(reply.text.contains("Approved once"), "{}", reply.text);
    assert!(a_fake.calls().contains(&format!("resolve {} allow", req.request_id)), "{:?}", a_fake.calls());
    let owner_audit = &a.store().audit_log(5).unwrap()[0];
    assert_eq!(owner_audit.origin.as_ref().map(|o| o.machine_id.as_str()), Some(OTHER));
    let origin_log = &b.store().intent_log(5).unwrap()[0];
    assert_eq!(
        (origin_log.kind.as_str(), origin_log.status.as_str(), origin_log.via.as_deref(), origin_log.transcript.as_deref()),
        ("approve", "applied", Some("voice"), Some(said.as_str()))
    );
}
