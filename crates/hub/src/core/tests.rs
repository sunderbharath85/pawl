//! Message-handling tests against a fake ACP host (no agent processes).

use super::host::fake::FakeHost;
use super::*;
use krypto_protocol::{
    PermissionOption, PermissionOptionKind, RuleMatch, Species, ToolCallPayload, ToolKind, ToolLocation,
};

/// The test hub's machine id.
const MID: &str = "abcdef012345";
/// Another machine (ids with this prefix are never ours).
const OTHER: &str = "zyxwvu987654";

fn ident() -> MachineIdentity {
    MachineIdentity { id: MID.into(), name: "Test PC".into() }
}

struct Fixture {
    hub: Hub,
    fake: FakeHost,
    rx: broadcast::Receiver<ServerMsg>,
    dir: tempfile::TempDir,
}

fn agent(id: &str) -> Agent {
    Agent {
        id: id.into(),
        machine_id: MID.into(),
        name: pretty_agent_name(id),
        command: vec![format!("{id}-acp")],
        env: None,
        version: None,
        description: None,
        icon: None,
        installed: true,
        source: AgentSource::Registry,
        capabilities: None,
    }
}

async fn fixture(rules: Option<Vec<Rule>>) -> Fixture {
    fixture_with(rules, None).await
}

/// `store_rules` are written to hub.db (as if a client had called set_rules); `config_rules`
/// play the role of config.toml `rules`.
async fn fixture_with(store_rules: Option<Vec<Rule>>, config_rules: Option<Vec<Rule>>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    let store = Store::open(&paths.db, MID).unwrap();
    store.replace_agents(&[agent("codex"), agent("claude-code")]).unwrap();
    if let Some(r) = &store_rules {
        store.set_rules(r).unwrap();
    }
    let fake = FakeHost::default();
    let config = HubConfig { offline: true, rules: config_rules, ..Default::default() };
    let hub = Hub::build(
        HubOptions { paths, config, token: "t".repeat(64), machine: ident(), background: false },
        store,
        Host::Fake(fake.clone()),
    )
    .await
    .unwrap();
    let rx = hub.subscribe();
    Fixture { hub, fake, rx, dir }
}

fn actor() -> Actor {
    Actor::client(ClientKind::Desktop, 1)
}

fn ack_result(m: ServerMsg) -> Value {
    match m {
        ServerMsg::Ack { ok: true, result, .. } => result.unwrap_or(Value::Null),
        other => panic!("expected ok ack, got {other:?}"),
    }
}

fn ack_error(m: ServerMsg) -> String {
    match m {
        ServerMsg::Ack { ok: false, error, .. } => error.unwrap_or_default(),
        other => panic!("expected error ack, got {other:?}"),
    }
}

fn drain(rx: &mut broadcast::Receiver<ServerMsg>) -> Vec<ServerMsg> {
    let mut v = vec![];
    while let Ok(m) = rx.try_recv() {
        v.push(m);
    }
    v
}

fn permission(session_id: &str, kind: ToolKind) -> PermissionRequest {
    PermissionRequest {
        request_id: global_id(MID, format!("req-{}", uuid::Uuid::new_v4())),
        machine_id: MID.into(),
        session_id: session_id.into(),
        agent_id: "codex".into(),
        tool_call: ToolCallPayload {
            tool_call_id: "tc1".into(),
            title: Some("cargo test".into()),
            kind: Some(kind),
            status: None,
            locations: Some(vec![ToolLocation { path: "/repo/src/lib.rs".into(), line: None }]),
            raw_input: None,
        },
        options: vec![
            PermissionOption { option_id: "allow".into(), name: "Allow".into(), kind: PermissionOptionKind::AllowOnce },
            PermissionOption { option_id: "reject".into(), name: "Reject".into(), kind: PermissionOptionKind::RejectOnce },
        ],
        created_at: krypto_protocol::now_ms(),
    }
}

async fn start(f: &Fixture) -> Session {
    let cwd = f.dir.path().to_string_lossy().into_owned();
    let v = ack_result(
        f.hub
            .handle(ClientMsg::StartSession { id: "1".into(), agent_id: "codex".into(), cwd, prompt: Some("run the tests".into()) }, &actor())
            .await,
    );
    serde_json::from_value(v["session"].clone()).unwrap()
}

#[tokio::test]
async fn hello_returns_welcome_shape() {
    let f = fixture(None).await;
    let v = ack_result(
        f.hub
            .handle(
                ClientMsg::Hello { id: "h".into(), client: ClientKind::Cli, client_version: "0".into(), token: "x".into() },
                &actor(),
            )
            .await,
    );
    let w: Welcome = serde_json::from_value(v).unwrap();
    assert_eq!(w.protocol_version, PROTOCOL_VERSION);
    assert_eq!(w.agents.len(), 2);
    assert!(!w.rules.is_empty(), "default rules seeded");
    assert_eq!((w.machine.id.as_str(), w.machine.name.as_str()), (MID, "Test PC"));
    assert!(w.machine.online);
    assert_eq!(w.machine.os, krypto_protocol::MachineOs::current());
    assert_eq!(w.machine.hub_version.as_deref(), Some(crate::HUB_VERSION));
    assert!(krypto_protocol::now_ms() - w.machine.last_seen < 60_000);
    assert_eq!(w.machines, vec![w.machine.clone()]);
    assert!(w.devices.is_empty(), "nothing paired");
    assert_eq!(w.peer.listening, None, "the peer listener is off by default");
    assert_eq!(w.peer.public_key.len(), 44, "base64 of a 32-byte key");
}

#[tokio::test]
async fn start_prompt_events_and_ack_shapes() {
    let mut f = fixture(None).await;
    let s = start(&f).await;
    assert_eq!(s.source, SessionSource::Owned);
    assert!(f.fake.calls().iter().any(|c| c.starts_with("start codex")));
    assert!(f.fake.calls().iter().any(|c| c == &format!("prompt {} run the tests", s.id)));

    let v = ack_result(f.hub.handle(ClientMsg::Prompt { id: "2".into(), session_id: s.id.clone(), text: "again".into() }, &actor()).await);
    assert_eq!(v, json!({}));
    // The real host emits one user_message per prompt; the hub must not add its own copy.
    for text in ["run the tests", "again"] {
        f.hub
            .on_host_event(HostEvent::Event { session_id: s.id.clone(), kind: EventType::UserMessage, payload: json!({ "text": text }) })
            .await;
    }

    let v = ack_result(
        f.hub.handle(ClientMsg::GetEvents { id: "3".into(), session_id: s.id.clone(), since_seq: None, limit: None }, &actor()).await,
    );
    let events: Vec<HubEvent> = serde_json::from_value(v["events"].clone()).unwrap();
    let kinds: Vec<EventType> = events.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, vec![EventType::SessionStart, EventType::UserMessage, EventType::UserMessage]);
    for e in &events {
        assert_eq!(e.id, global_id(MID, e.seq));
        assert_eq!((e.machine_id.as_str(), e.session_id.as_str()), (MID, s.id.as_str()));
    }
    let since = ack_result(
        f.hub
            .handle(ClientMsg::GetEvents { id: "4".into(), session_id: s.id.clone(), since_seq: Some(events[1].seq), limit: Some(10) }, &actor())
            .await,
    );
    assert_eq!(since["events"].as_array().unwrap().len(), 1);

    let v = ack_result(f.hub.handle(ClientMsg::ListSessions { id: "5".into(), include_ended: None }, &actor()).await);
    assert_eq!(v["sessions"][0]["id"], s.id.as_str());

    // a pet was created for the agent and broadcast
    let msgs = drain(&mut f.rx);
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Pet { pet } if pet.agent_id == "codex" && pet.species == Species::Dog)));
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Session { .. })));

    let err = ack_error(f.hub.handle(ClientMsg::Prompt { id: "6".into(), session_id: "nope".into(), text: "x".into() }, &actor()).await);
    assert_eq!(err, "not_found");
    let err = ack_error(
        f.hub.handle(ClientMsg::Prompt { id: "6b".into(), session_id: global_id(MID, "nope"), text: "x".into() }, &actor()).await,
    );
    assert_eq!(err, "not_found");
    let err = ack_error(
        f.hub.handle(ClientMsg::StartSession { id: "7".into(), agent_id: "codex".into(), cwd: "Z:/definitely/missing".into(), prompt: None }, &actor()).await,
    );
    assert!(err.contains("not an existing directory"), "{err}");
}

#[tokio::test]
async fn ask_rule_then_user_resolves_with_audit() {
    let mut f = fixture(None).await; // default rules: execute → ask
    let s = start(&f).await;
    let req = permission(&s.id, ToolKind::Execute);
    f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    assert_eq!(f.hub.pending_permissions().len(), 1);
    let msgs = drain(&mut f.rx);
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::PermissionRequest { request } if request.request_id == req.request_id)));
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Notification { notification } if notification.level == NotificationLevel::Alert)));

    let err = ack_error(
        f.hub
            .handle(ClientMsg::ResolvePermission { id: "r0".into(), request_id: req.request_id.clone(), option_id: Some("bogus".into()) }, &actor())
            .await,
    );
    assert!(err.contains("unknown optionId"));
    assert_eq!(f.hub.pending_permissions().len(), 1, "bad option keeps it pending");

    let v = ack_result(
        f.hub
            .handle(ClientMsg::ResolvePermission { id: "r1".into(), request_id: req.request_id.clone(), option_id: Some("allow".into()) }, &actor())
            .await,
    );
    assert_eq!(v, json!({}));
    assert!(f.hub.pending_permissions().is_empty());
    assert!(f.fake.calls().contains(&format!("resolve {} allow", req.request_id)));
    let msgs = drain(&mut f.rx);
    assert!(msgs.iter().any(|m| matches!(m,
        ServerMsg::PermissionResolved { request_id, by: ResolvedBy::User, outcome: PermissionOutcome::Selected { option_id }, .. }
        if request_id == &req.request_id && option_id == "allow")));

    let audit = f.hub.store().audit_log(10).unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].outcome, AuditOutcome::Allowed);
    assert_eq!(audit[0].decided_by, ResolvedBy::User);
    assert_eq!(audit[0].actor, "desktop#1");
    assert_eq!(audit[0].locations, vec!["/repo/src/lib.rs".to_string()]);

    assert_eq!(audit[0].machine_id, MID);

    let err = ack_error(
        f.hub.handle(ClientMsg::ResolvePermission { id: "r2".into(), request_id: req.request_id.clone(), option_id: None }, &actor()).await,
    );
    assert_eq!(err, "not_found");
}

#[tokio::test]
async fn auto_rules_resolve_without_asking() {
    let rules = vec![
        Rule { id: "allow-edit".into(), matcher: RuleMatch { tool_kind: Some(ToolKind::Edit), ..Default::default() }, action: RuleAction::AutoAllow },
        Rule { id: "no-delete".into(), matcher: RuleMatch { tool_kind: Some(ToolKind::Delete), ..Default::default() }, action: RuleAction::Deny },
    ];
    let mut f = fixture(Some(rules)).await;
    let s = start(&f).await;
    drain(&mut f.rx);

    let allow = permission(&s.id, ToolKind::Edit);
    f.hub.on_host_event(HostEvent::PermissionRequest(allow.clone())).await;
    let deny = permission(&s.id, ToolKind::Delete);
    f.hub.on_host_event(HostEvent::PermissionRequest(deny.clone())).await;

    assert!(f.hub.pending_permissions().is_empty());
    let msgs = drain(&mut f.rx);
    assert!(!msgs.iter().any(|m| matches!(m, ServerMsg::PermissionRequest { .. })), "auto decisions never ask");
    assert!(msgs.iter().any(|m| matches!(m,
        ServerMsg::PermissionResolved { request_id, by: ResolvedBy::Rule, outcome: PermissionOutcome::Selected { option_id }, .. }
        if request_id == &allow.request_id && option_id == "allow")));
    assert!(msgs.iter().any(|m| matches!(m,
        ServerMsg::PermissionResolved { request_id, by: ResolvedBy::Rule, outcome: PermissionOutcome::Selected { option_id }, .. }
        if request_id == &deny.request_id && option_id == "reject")));
    let audit = f.hub.store().audit_log(10).unwrap();
    assert_eq!(audit.len(), 2);
    assert!(audit.iter().all(|a| a.decided_by == ResolvedBy::Rule));
    assert!(audit.iter().any(|a| a.outcome == AuditOutcome::Rejected && a.actor == "rule:deny"));
}

#[tokio::test]
async fn set_rules_reevaluates_pending_and_validates() {
    let mut f = fixture(None).await;
    let s = start(&f).await;
    let req = permission(&s.id, ToolKind::Edit);
    f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    assert_eq!(f.hub.pending_permissions().len(), 1);
    drain(&mut f.rx);

    let dup = vec![
        Rule { id: "a".into(), matcher: RuleMatch::default(), action: RuleAction::Ask },
        Rule { id: "a".into(), matcher: RuleMatch::default(), action: RuleAction::Ask },
    ];
    assert!(ack_error(f.hub.handle(ClientMsg::SetRules { id: "1".into(), rules: dup }, &actor()).await).contains("duplicate"));

    let rules = vec![Rule {
        id: "edit-ok".into(),
        matcher: RuleMatch { tool_kind: Some(ToolKind::Edit), ..Default::default() },
        action: RuleAction::AutoAllow,
    }];
    let v = ack_result(f.hub.handle(ClientMsg::SetRules { id: "2".into(), rules: rules.clone() }, &actor()).await);
    assert_eq!(serde_json::from_value::<Vec<Rule>>(v["rules"].clone()).unwrap(), rules);
    assert!(f.hub.pending_permissions().is_empty());
    assert_eq!(f.hub.store().rules().unwrap(), Some(rules));
}

fn rule(id: &str, kind: Option<ToolKind>, action: RuleAction) -> Rule {
    Rule { id: id.into(), matcher: RuleMatch { tool_kind: kind, ..Default::default() }, action }
}

#[tokio::test]
async fn set_rules_rejects_unsafe_rules_and_changes_nothing() {
    let f = fixture(None).await;
    let before = f.hub.rules();
    for (n, unsafe_rule) in [
        rule("allow-exec", Some(ToolKind::Execute), RuleAction::AutoAllow),
        rule("allow-delete", Some(ToolKind::Delete), RuleAction::AutoAllow),
        rule("allow-fetch", Some(ToolKind::Fetch), RuleAction::AutoAllow),
        rule("allow-other", Some(ToolKind::Other), RuleAction::AutoAllow),
        rule("allow-anything", None, RuleAction::AutoAllow),
    ]
    .into_iter()
    .enumerate()
    {
        let id = unsafe_rule.id.clone();
        let rules = vec![rule("read-ok", Some(ToolKind::Read), RuleAction::AutoAllow), unsafe_rule];
        let err = ack_error(f.hub.handle(ClientMsg::SetRules { id: format!("s{n}"), rules }, &actor()).await);
        assert!(err.starts_with("invalid_rule: "), "{err}");
        assert!(err.contains(&id), "{err}");
        assert_eq!(f.hub.rules(), before, "rejected set_rules must not change the live rules");
        assert_eq!(f.hub.store().rules().unwrap(), None, "rejected set_rules must not persist anything");
    }
    let err = ack_error(
        f.hub
            .handle(ClientMsg::SetRules { id: "dup".into(), rules: vec![rule("a", None, RuleAction::Ask), rule("a", None, RuleAction::Deny)] }, &actor())
            .await,
    );
    assert!(err.starts_with("invalid_rule: duplicate"), "{err}");
    // ask/deny rules may cover any kind
    let ok = vec![rule("exec-ask", Some(ToolKind::Execute), RuleAction::Ask), rule("no-fetch", Some(ToolKind::Fetch), RuleAction::Deny)];
    ack_result(f.hub.handle(ClientMsg::SetRules { id: "ok".into(), rules: ok.clone() }, &actor()).await);
    assert_eq!(f.hub.rules(), ok);
}

#[tokio::test]
async fn startup_drops_invalid_rules_from_the_store() {
    let stored = vec![
        rule("read-ok", Some(ToolKind::Read), RuleAction::AutoAllow),
        rule("allow-exec", Some(ToolKind::Execute), RuleAction::AutoAllow),
        rule("allow-anything", None, RuleAction::AutoAllow),
        rule("read-ok", Some(ToolKind::Search), RuleAction::AutoAllow),
        rule("exec-ask", Some(ToolKind::Execute), RuleAction::Ask),
    ];
    let mut f = fixture(Some(stored)).await;
    let ids: Vec<String> = f.hub.rules().into_iter().map(|r| r.id).collect();
    assert_eq!(ids, vec!["read-ok", "exec-ask"]);
    let notices = f.hub.startup_notices();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].level, NotificationLevel::Warning);
    for id in ["allow-exec", "allow-anything", "read-ok"] {
        assert!(notices[0].body.contains(id), "{}", notices[0].body);
    }
    assert!(notices[0].body.contains("hub.db"));

    // an execute request still asks the user: the dropped rule never reached the engine
    let s = start(&f).await;
    f.hub.on_host_event(HostEvent::PermissionRequest(permission(&s.id, ToolKind::Execute))).await;
    assert_eq!(f.hub.pending_permissions().len(), 1);
    assert!(drain(&mut f.rx).iter().any(|m| matches!(m, ServerMsg::PermissionRequest { .. })));

    // fixing the rules clears the replayed warning
    ack_result(f.hub.handle(ClientMsg::SetRules { id: "fix".into(), rules: vec![] }, &actor()).await);
    assert!(f.hub.startup_notices().is_empty());
}

#[tokio::test]
async fn startup_drops_invalid_rules_from_config() {
    let from_config = vec![
        rule("allow-delete", Some(ToolKind::Delete), RuleAction::AutoAllow),
        rule("edit-ok", Some(ToolKind::Edit), RuleAction::AutoAllow),
    ];
    let f = fixture_with(None, Some(from_config)).await;
    assert_eq!(f.hub.rules(), vec![rule("edit-ok", Some(ToolKind::Edit), RuleAction::AutoAllow)]);
    let notices = f.hub.startup_notices();
    assert_eq!(notices.len(), 1);
    assert!(notices[0].body.contains("allow-delete") && notices[0].body.contains("config.toml"), "{}", notices[0].body);

    // valid config and the built-in defaults raise no warning
    let f = fixture_with(None, Some(vec![rule("edit-ok", Some(ToolKind::Edit), RuleAction::AutoAllow)])).await;
    assert!(f.hub.startup_notices().is_empty());
    let f = fixture(None).await;
    assert!(f.hub.startup_notices().is_empty());
    assert_eq!(f.hub.rules(), krypto_brain::rules::default_rules());
}

#[tokio::test]
async fn cancel_and_session_end_clear_pending() {
    let mut f = fixture(None).await;
    let s = start(&f).await;
    let req = permission(&s.id, ToolKind::Execute);
    f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    drain(&mut f.rx);
    ack_result(f.hub.handle(ClientMsg::Cancel { id: "c".into(), session_id: s.id.clone() }, &actor()).await);
    assert!(f.hub.pending_permissions().is_empty());
    assert!(f.fake.calls().contains(&format!("cancel {}", s.id)));
    let msgs = drain(&mut f.rx);
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::PermissionResolved { outcome: PermissionOutcome::Cancelled, .. })));

    let req2 = permission(&s.id, ToolKind::Execute);
    f.hub.on_host_event(HostEvent::PermissionRequest(req2)).await;
    let mut ended = s.clone();
    ended.status = SessionStatus::Done;
    ended.ended_at = Some(krypto_protocol::now_ms());
    f.hub.on_host_event(HostEvent::Session(ended)).await;
    assert!(f.hub.pending_permissions().is_empty());
    let err = ack_error(f.hub.handle(ClientMsg::Prompt { id: "p".into(), session_id: s.id.clone(), text: "x".into() }, &actor()).await);
    assert!(err.contains("ended"));
}

#[tokio::test]
async fn host_initiated_cancel_clears_pending_once() {
    let mut f = fixture(None).await;
    let s = start(&f).await;
    let req = permission(&s.id, ToolKind::Execute);
    f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    drain(&mut f.rx);
    let payload = json!({ "requestId": req.request_id, "outcome": { "kind": "cancelled" }, "by": "system" });
    f.hub
        .on_host_event(HostEvent::Event { session_id: s.id.clone(), kind: EventType::PermissionResolved, payload: payload.clone() })
        .await;
    assert!(f.hub.pending_permissions().is_empty());
    let msgs = drain(&mut f.rx);
    let resolved = msgs
        .iter()
        .filter(|m| matches!(m, ServerMsg::PermissionResolved { by: ResolvedBy::System, outcome: PermissionOutcome::Cancelled, .. }))
        .count();
    assert_eq!(resolved, 1);
    // a second echo for the same request is dropped
    f.hub.on_host_event(HostEvent::Event { session_id: s.id.clone(), kind: EventType::PermissionResolved, payload }).await;
    assert!(!drain(&mut f.rx).iter().any(|m| matches!(m, ServerMsg::PermissionResolved { .. })));
    let audit = f.hub.store().audit_log(10).unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].decided_by, ResolvedBy::System);
    assert_eq!(audit[0].outcome, AuditOutcome::Cancelled);
}

#[tokio::test]
async fn close_session_ends_and_clears_pending() {
    let mut f = fixture(None).await;
    let s = start(&f).await;
    f.hub.on_host_event(HostEvent::PermissionRequest(permission(&s.id, ToolKind::Execute))).await;
    drain(&mut f.rx);
    f.hub.close_session(&s.id).await.unwrap();
    assert!(f.fake.calls().contains(&format!("close {}", s.id)));
    let closed = f.hub.session(&s.id).unwrap();
    assert!(closed.ended_at.is_some());
    assert_eq!(closed.status, SessionStatus::Done);
    assert!(f.hub.pending_permissions().is_empty());
    let msgs = drain(&mut f.rx);
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::PermissionResolved { by: ResolvedBy::System, .. })));
    assert!(f.hub.close_session(&s.id).await.is_err(), "already ended");
}

#[tokio::test]
async fn acp_stop_event_notifies_and_updates_session() {
    let mut f = fixture(None).await;
    let s = start(&f).await;
    drain(&mut f.rx);
    f.hub
        .on_host_event(HostEvent::Event { session_id: s.id.clone(), kind: EventType::Stop, payload: json!({ "stopReason": "end_turn" }) })
        .await;
    let msgs = drain(&mut f.rx);
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Event { event } if event.kind == EventType::Stop)));
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Notification { notification } if notification.level == NotificationLevel::Success)));
}

#[tokio::test]
async fn update_pet_validates_and_broadcasts() {
    let mut f = fixture(None).await;
    start(&f).await;
    let pet = f.hub.pets().into_iter().find(|p| p.agent_id == "codex").unwrap();
    drain(&mut f.rx);
    let v = ack_result(
        f.hub
            .handle(
                ClientMsg::UpdatePet {
                    id: "u".into(),
                    pet_id: pet.id.clone(),
                    patch: PetPatch { species: Some(Species::Robot), name: Some("  Bolt ".into()), skin: None, personality: None },
                },
                &actor(),
            )
            .await,
    );
    let updated: Pet = serde_json::from_value(v["pet"].clone()).unwrap();
    assert_eq!(updated.species, Species::Robot);
    assert_eq!(updated.name, "Bolt");
    assert!(drain(&mut f.rx).iter().any(|m| matches!(m, ServerMsg::Pet { pet } if pet.name == "Bolt")));
    let err = ack_error(
        f.hub
            .handle(ClientMsg::UpdatePet { id: "u2".into(), pet_id: pet.id.clone(), patch: PetPatch { name: Some(" ".into()), ..Default::default() } }, &actor())
            .await,
    );
    assert_eq!(err, "invalid");
    assert_eq!(
        ack_error(f.hub.handle(ClientMsg::UpdatePet { id: "u3".into(), pet_id: "nope".into(), patch: PetPatch::default() }, &actor()).await),
        "not_found"
    );
    assert_eq!(
        ack_error(
            f.hub
                .handle(ClientMsg::UpdatePet { id: "u4".into(), pet_id: global_id(MID, "pet-nope"), patch: PetPatch::default() }, &actor())
                .await
        ),
        "not_found"
    );
}

#[tokio::test]
async fn chat_ack_is_the_stored_user_message() {
    let f = fixture(None).await;
    start(&f).await;
    let v = ack_result(f.hub.handle(ClientMsg::Chat { id: "c".into(), pet_id: None, text: " hi doggo ".into() }, &actor()).await);
    let m: ChatMessage = serde_json::from_value(v["message"].clone()).unwrap();
    assert_eq!(m.role, ChatRole::User);
    assert_eq!(m.text, "hi doggo");
    assert!(m.pet_id.is_some());
    assert!(ack_error(f.hub.handle(ClientMsg::Chat { id: "c2".into(), pet_id: None, text: "  ".into() }, &actor()).await).contains("empty"));
}

#[tokio::test]
async fn resume_reuses_open_session() {
    let f = fixture(None).await;
    let cwd = f.dir.path().to_string_lossy().into_owned();
    let v = ack_result(
        f.hub
            .handle(ClientMsg::ResumeSession { id: "r".into(), agent_id: "codex".into(), acp_session_id: "acp-1".into(), cwd: cwd.clone() }, &actor())
            .await,
    );
    let s: Session = serde_json::from_value(v["session"].clone()).unwrap();
    assert_eq!(s.acp_session_id.as_deref(), Some("acp-1"));
    let v = ack_result(
        f.hub.handle(ClientMsg::ResumeSession { id: "r2".into(), agent_id: "codex".into(), acp_session_id: "acp-1".into(), cwd }, &actor()).await,
    );
    assert_eq!(v["session"]["id"], s.id.as_str());
    let v = ack_result(f.hub.handle(ClientMsg::ListHistory { id: "h".into(), agent_id: "codex".into(), cwd: None }, &actor()).await);
    assert_eq!(v, json!({ "sessions": [] }));
}

#[tokio::test]
async fn restart_closes_owned_sessions() {
    let f = fixture(None).await;
    let s = start(&f).await;
    let paths = f.hub.paths().clone();
    drop(f.hub);
    let store = Store::open(&paths.db, MID).unwrap();
    let hub = Hub::build(
        HubOptions {
            paths,
            config: HubConfig { offline: true, ..Default::default() },
            token: "t".into(),
            machine: ident(),
            background: false,
        },
        store,
        Host::Fake(FakeHost::default()),
    )
    .await
    .unwrap();
    let again = hub.session(&s.id).unwrap();
    assert!(again.ended_at.is_some());
}

#[test]
fn names_and_cwd() {
    assert_eq!(pretty_agent_name("claude-code"), "Claude Code");
    assert_eq!(pretty_agent_name("codex"), "Codex");
    assert!(resolve_cwd("").is_err());
    let d = tempfile::tempdir().unwrap();
    assert!(resolve_cwd(&d.path().to_string_lossy()).unwrap().is_absolute());
}

#[tokio::test]
async fn ids_on_the_wire_are_global() {
    let mut f = fixture(None).await;
    let s = start(&f).await;
    assert_eq!(s.machine_id, MID);
    assert_eq!(parse_global_id(&s.id).map(|(m, _)| m), Some(MID));

    let pets = f.hub.pets();
    let codex = pets.iter().find(|p| p.agent_id == "codex").unwrap();
    assert_eq!(codex.id, global_id(MID, "pet-codex"));
    assert_eq!(codex.machine_id, MID);

    let v = ack_result(f.hub.handle(ClientMsg::Chat { id: "c".into(), pet_id: Some(codex.id.clone()), text: "hi".into() }, &actor()).await);
    let m: ChatMessage = serde_json::from_value(v["message"].clone()).unwrap();
    assert_eq!(m.machine_id, MID);
    assert_eq!(parse_global_id(&m.id).map(|(m, _)| m), Some(MID));

    f.hub
        .on_host_event(HostEvent::Event { session_id: s.id.clone(), kind: EventType::Stop, payload: json!({ "stopReason": "end_turn" }) })
        .await;
    let msgs = drain(&mut f.rx);
    let note = msgs
        .iter()
        .find_map(|m| match m {
            ServerMsg::Notification { notification } => Some(notification.clone()),
            _ => None,
        })
        .expect("a stop notification");
    assert_eq!(note.machine_id, MID);
    assert_eq!(parse_global_id(&note.id).map(|(m, _)| m), Some(MID));
    let ev = msgs
        .iter()
        .find_map(|m| match m {
            ServerMsg::Event { event } if event.kind == EventType::Stop => Some(event.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(ev.id, global_id(MID, ev.seq));

    // hook sessions too
    f.hub
        .ingest_hook(HookSource::Claude, json!({ "hook_event_name": "SessionStart", "session_id": "ext-1", "cwd": "/repo" }))
        .await
        .unwrap();
    let hooked = f.hub.list_sessions(false).await.unwrap().into_iter().find(|x| x.agent_id == "claude-code").expect("hook session");
    assert_eq!(hooked.machine_id, MID);
    assert_eq!(parse_global_id(&hooked.id).map(|(m, _)| m), Some(MID));
}

#[tokio::test]
async fn foreign_and_legacy_ids_are_refused() {
    let f = fixture(None).await;
    let s = start(&f).await;
    let local = parse_global_id(&s.id).unwrap().1.to_string();
    let theirs = global_id(OTHER, &local);
    let cases: Vec<(ClientMsg, &str)> = vec![
        (ClientMsg::Prompt { id: "1".into(), session_id: theirs.clone(), text: "x".into() }, "not_owner"),
        (ClientMsg::Prompt { id: "2".into(), session_id: local.clone(), text: "x".into() }, "not_found"),
        (ClientMsg::Cancel { id: "3".into(), session_id: theirs.clone() }, "not_owner"),
        (ClientMsg::Cancel { id: "4".into(), session_id: local.clone() }, "not_found"),
        (ClientMsg::GetEvents { id: "5".into(), session_id: theirs.clone(), since_seq: None, limit: None }, "not_owner"),
        (ClientMsg::GetEvents { id: "6".into(), session_id: local.clone(), since_seq: None, limit: None }, "not_found"),
        (ClientMsg::GetEvents { id: "6b".into(), session_id: global_id(MID, "missing"), since_seq: None, limit: None }, "not_found"),
        (ClientMsg::ResolvePermission { id: "7".into(), request_id: global_id(OTHER, "r"), option_id: None }, "not_owner"),
        (ClientMsg::ResolvePermission { id: "8".into(), request_id: "r".into(), option_id: None }, "not_found"),
        (ClientMsg::UpdatePet { id: "9".into(), pet_id: global_id(OTHER, "pet-codex"), patch: PetPatch::default() }, "not_owner"),
        (ClientMsg::UpdatePet { id: "10".into(), pet_id: "pet-codex".into(), patch: PetPatch::default() }, "not_found"),
        (ClientMsg::Chat { id: "11".into(), pet_id: Some(global_id(OTHER, "pet-codex")), text: "hi".into() }, "not_owner"),
        (ClientMsg::Chat { id: "12".into(), pet_id: Some("pet-codex".into()), text: "hi".into() }, "not_found"),
        (ClientMsg::UpdateMachine { id: "13".into(), machine_id: OTHER.into(), name: "Mac mini".into() }, "not_owner"),
        (ClientMsg::UpdateMachine { id: "14".into(), machine_id: "nope".into(), name: "Mac mini".into() }, "invalid"),
        (ClientMsg::UpdateMachine { id: "15".into(), machine_id: "ABCDEF012345".into(), name: "Mac mini".into() }, "invalid"),
        (ClientMsg::UpdateMachine { id: "16".into(), machine_id: format!("{MID}:x"), name: "Mac mini".into() }, "invalid"),
        (ClientMsg::UpdateMachine { id: "17".into(), machine_id: String::new(), name: "Mac mini".into() }, "invalid"),
    ];
    for (msg, want) in cases {
        let id = msg.id().to_string();
        let got = ack_error(f.hub.handle(msg, &actor()).await);
        assert_eq!(got, want, "message {id}");
    }
    // our own prefix still works
    ack_result(f.hub.handle(ClientMsg::Prompt { id: "ok".into(), session_id: s.id.clone(), text: "x".into() }, &actor()).await);
    assert!(f.hub.store().audit_log(10).unwrap().is_empty(), "refused requests decide nothing");
}

#[tokio::test]
async fn update_machine_persists_and_broadcasts() {
    let mut f = fixture(None).await;
    drain(&mut f.rx);
    let v = ack_result(
        f.hub.handle(ClientMsg::UpdateMachine { id: "m".into(), machine_id: MID.into(), name: "  Windows PC  ".into() }, &actor()).await,
    );
    let m: Machine = serde_json::from_value(v["machine"].clone()).unwrap();
    assert_eq!((m.id.as_str(), m.name.as_str()), (MID, "Windows PC"));
    assert!(m.online);
    assert_eq!(f.hub.machine().name, "Windows PC");
    assert_eq!(f.hub.welcome().machine.name, "Windows PC");
    assert_eq!(f.hub.welcome().machines[0].name, "Windows PC");
    let saved = machine::load(f.hub.paths()).unwrap().unwrap();
    assert_eq!(saved, MachineIdentity { id: MID.into(), name: "Windows PC".into() });
    let msgs = drain(&mut f.rx);
    assert!(msgs.iter().any(|x| matches!(x, ServerMsg::Machine { machine } if machine.name == "Windows PC" && machine.id == MID)));

    // 40 chars (counted as chars, not bytes) is fine; 41 or blank is "invalid" and changes nothing
    let forty = "ü".repeat(40);
    ack_result(f.hub.handle(ClientMsg::UpdateMachine { id: "m2".into(), machine_id: MID.into(), name: forty.clone() }, &actor()).await);
    assert_eq!(f.hub.machine().name, forty);
    drain(&mut f.rx);
    for bad in ["".to_string(), "   ".to_string(), "ü".repeat(41), "a\tb".to_string()] {
        let err = ack_error(f.hub.handle(ClientMsg::UpdateMachine { id: "bad".into(), machine_id: MID.into(), name: bad.clone() }, &actor()).await);
        assert_eq!(err, "invalid", "{bad:?}");
    }
    assert_eq!(f.hub.machine().name, forty);
    assert_eq!(machine::load(f.hub.paths()).unwrap().unwrap().name, forty);
    assert!(!drain(&mut f.rx).iter().any(|x| matches!(x, ServerMsg::Machine { .. })));
}

#[tokio::test]
async fn default_pets_get_distinct_names() {
    let f = fixture(None).await;
    let cwd = f.dir.path().to_string_lossy().into_owned();
    for (i, agent_id) in ["codex", "claude-code"].into_iter().enumerate() {
        ack_result(
            f.hub
                .handle(ClientMsg::StartSession { id: format!("s{i}"), agent_id: agent_id.into(), cwd: cwd.clone(), prompt: None }, &actor())
                .await,
        );
    }
    let pets = f.hub.pets();
    assert_eq!(pets.len(), 2);
    assert_ne!(pets[0].name.to_lowercase(), pets[1].name.to_lowercase());
    for p in &pets {
        assert_eq!(p.id, global_id(MID, format!("pet-{}", p.agent_id)));
        assert_eq!(p.species, Species::Dog);
    }
}

#[tokio::test]
async fn store_of_another_machine_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    let store = Store::open(&paths.db, OTHER).unwrap();
    let r = Hub::build(
        HubOptions { paths, config: HubConfig { offline: true, ..Default::default() }, token: "t".into(), machine: ident(), background: false },
        store,
        Host::Fake(FakeHost::default()),
    )
    .await;
    assert!(r.is_err());
}

fn pet(machine_id: &str, agent_id: &str, name: &str) -> Pet {
    Pet {
        id: global_id(machine_id, format!("pet-{agent_id}")),
        machine_id: machine_id.into(),
        agent_id: agent_id.into(),
        species: Species::Dog,
        skin: "default".into(),
        name: name.into(),
        personality: "eager".into(),
        mood: 70,
        xp: 0,
    }
}

#[test]
fn dedupe_keeps_the_oldest_name_and_renames_later_duplicates() {
    let mut pets = vec![
        pet(MID, "claude-code", "Mochi"),
        pet(MID, "copilot", " mochi "),
        pet(MID, "codex", "Biscuit"),
        pet(MID, "opencode-v2", "MOCHI"),
        pet(MID, "gemini", "  "),
    ];
    let renamed = dedupe_pet_names(&mut pets, MID);
    assert_eq!(renamed, vec![1, 3, 4]);
    assert_eq!(pets[0].name, "Mochi");
    assert_eq!(pets[2].name, "Biscuit");
    let keys: HashSet<String> = pets.iter().map(|p| p.name.trim().to_lowercase()).collect();
    assert_eq!(keys.len(), pets.len(), "{:?}", pets.iter().map(|p| &p.name).collect::<Vec<_>>());
    assert!(pets.iter().all(|p| !p.name.trim().is_empty()));
    // already unique: nothing changes
    let before = pets.clone();
    assert!(dedupe_pet_names(&mut pets, MID).is_empty());
    assert_eq!(pets, before);
}

#[test]
fn dedupe_never_renames_other_machines_pets() {
    let mut pets = vec![pet(MID, "codex", "Mochi"), pet(OTHER, "codex", "Mochi")];
    assert_eq!(dedupe_pet_names(&mut pets, MID), vec![0]);
    assert_eq!(pets[1].name, "Mochi");
    assert_ne!(pets[0].name.to_lowercase(), "mochi");
}

/// The live-test bug: a v1 hub.db where claude-code, copilot and opencode-v2 are all "Mochi".
#[tokio::test]
async fn migrated_duplicate_pet_names_are_fixed_at_startup() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    crate::store::create_v1_db_for_tests(
        &paths.db,
        &[("pet-claude-code", "claude-code", "Mochi"), ("pet-copilot", "copilot", "Mochi"), ("pet-x", "opencode-v2", "mochi")],
    );
    let build = |paths: Paths| async move {
        let store = Store::open(&paths.db, MID).unwrap();
        Hub::build(
            HubOptions { paths, config: HubConfig { offline: true, ..Default::default() }, token: "t".into(), machine: ident(), background: false },
            store,
            Host::Fake(FakeHost::default()),
        )
        .await
        .unwrap()
    };
    let hub = build(paths.clone()).await;
    let pets = hub.pets();
    assert_eq!(pets.len(), 3);
    let by_agent = |a: &str| pets.iter().find(|p| p.agent_id == a).unwrap().clone();
    assert_eq!(by_agent("claude-code").name, "Mochi", "the oldest pet keeps its name");
    assert_eq!(by_agent("opencode-v2").id, global_id(MID, "pet-opencode-v2"));
    let keys: HashSet<String> = pets.iter().map(|p| p.name.to_lowercase()).collect();
    assert_eq!(keys.len(), 3, "{pets:?}");
    // persisted: a restart sees the same names and renames nothing
    let mut stored = hub.store().pets().unwrap();
    stored.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    let mut live = pets.clone();
    live.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
    assert_eq!(stored, live);
    drop(hub);
    let again = build(paths).await;
    assert_eq!(again.pets(), pets);
}

#[tokio::test]
async fn update_pet_names_are_validated_and_unique() {
    let f = fixture(None).await;
    let cwd = f.dir.path().to_string_lossy().into_owned();
    for (i, agent_id) in ["codex", "claude-code"].into_iter().enumerate() {
        ack_result(
            f.hub
                .handle(ClientMsg::StartSession { id: format!("s{i}"), agent_id: agent_id.into(), cwd: cwd.clone(), prompt: None }, &actor())
                .await,
        );
    }
    let codex = f.hub.pets().into_iter().find(|p| p.agent_id == "codex").unwrap();
    let claude = f.hub.pets().into_iter().find(|p| p.agent_id == "claude-code").unwrap();
    let rename = |pet_id: String, name: String| ClientMsg::UpdatePet {
        id: "n".into(),
        pet_id,
        patch: PetPatch { name: Some(name), ..Default::default() },
    };

    // another pet's name, any case / surrounding whitespace: name_taken, nothing changes
    let taken = format!("  {}  ", claude.name.to_uppercase());
    assert_eq!(ack_error(f.hub.handle(rename(codex.id.clone(), taken), &actor()).await), "name_taken");
    assert_eq!(f.hub.pets().into_iter().find(|p| p.id == codex.id).unwrap().name, codex.name);

    // re-casing its own name is fine
    let v = ack_result(f.hub.handle(rename(codex.id.clone(), codex.name.to_uppercase()), &actor()).await);
    assert_eq!(v["pet"]["name"], codex.name.to_uppercase().as_str());

    // 1..=40 chars (as chars) after trimming
    let forty = "ö".repeat(40);
    let v = ack_result(f.hub.handle(rename(codex.id.clone(), format!(" {forty} ")), &actor()).await);
    assert_eq!(v["pet"]["name"], forty.as_str());
    for bad in [String::new(), "   ".into(), "ö".repeat(41), "a\nb".into()] {
        assert_eq!(ack_error(f.hub.handle(rename(codex.id.clone(), bad.clone()), &actor()).await), "invalid", "{bad:?}");
    }
    assert_eq!(f.hub.store().pets().unwrap().into_iter().find(|p| p.id == codex.id).unwrap().name, forty);
}

// ---------------------------------------------------------------- pairing + presence (step 2)

use crate::store::{PairingAuditEntry, PairingAuditKind};
use krypto_protocol::{Device, DeviceKind};

/// A real hub (fake ACP host) in `dir` with its own machine id; `listen` = config.toml [peer] listen.
async fn peer_hub_in(dir: &std::path::Path, mid: &str, name: &str, listen: Option<&str>) -> Hub {
    let paths = Paths::in_dir(dir);
    let store = Store::open(&paths.db, mid).unwrap();
    let mut config = HubConfig { offline: true, ..Default::default() };
    config.peer.listen = listen.map(Into::into);
    let machine = MachineIdentity { id: mid.into(), name: name.into() };
    let hub = Hub::build(
        HubOptions { paths, config, token: "t".repeat(64), machine, background: false },
        store,
        Host::Fake(FakeHost::default()),
    )
    .await
    .unwrap();
    hub.start_peer_links().await.unwrap();
    hub
}

async fn peer_hub(mid: &str, name: &str, listen: Option<&str>) -> (Hub, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let hub = peer_hub_in(dir.path(), mid, name, listen).await;
    (hub, dir)
}

/// The next broadcast matching `pred` (10 s cap, far below the 45 s offline timeout).
async fn wait_for(rx: &mut broadcast::Receiver<ServerMsg>, what: &str, pred: impl Fn(&ServerMsg) -> bool) -> ServerMsg {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match rx.recv().await {
                Ok(m) if pred(&m) => return m,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("bus closed while waiting for {what}: {e}"),
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// Poll the pairing audit log until an entry of `kind` shows up (peer events are handled async).
async fn wait_audit(hub: &Hub, kind: PairingAuditKind) -> PairingAuditEntry {
    for _ in 0..100 {
        if let Some(e) = hub.store().pairing_audit(50).unwrap().into_iter().find(|e| e.kind == kind) {
            return e;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no {kind:?} pairing audit entry");
}

fn is_machine(m: &ServerMsg, id: &str, online: bool) -> bool {
    matches!(m, ServerMsg::Machine { machine } if machine.id == id && machine.online == online)
}

async fn invite(hub: &Hub, ttl_secs: Option<u32>, addr: Option<&str>) -> ServerMsg {
    hub.handle(ClientMsg::CreateInvite { id: "i".into(), ttl_secs, addr: addr.map(Into::into) }, &actor()).await
}

async fn join(hub: &Hub, uri: &str) -> ServerMsg {
    hub.handle(ClientMsg::JoinPeer { id: "j".into(), uri: uri.into() }, &actor()).await
}

/// Pair `b` (no listener) with `a` (listener on); returns once both see the other online.
async fn pair(a: &Hub, b: &Hub) {
    let mut arx = a.subscribe();
    let mut brx = b.subscribe();
    let uri = ack_result(invite(a, None, None).await)["uri"].as_str().unwrap().to_string();
    ack_result(join(b, &uri).await);
    let (a_id, b_id) = (a.machine_id().to_string(), b.machine_id().to_string());
    wait_for(&mut arx, "a sees b online", |m| is_machine(m, &b_id, true)).await;
    wait_for(&mut brx, "b sees a online", |m| is_machine(m, &a_id, true)).await;
}

#[tokio::test]
async fn pairing_requests_answer_bare_codes() {
    let (a, _da) = peer_hub(MID, "Desk", Some("127.0.0.1:0")).await;
    let (b, _db) = peer_hub(OTHER, "Laptop", None).await;

    // Welcome carries the peer status; the key is base64 of 32 bytes.
    let wa = a.welcome();
    let listening = wa.peer.listening.clone().expect("listener on");
    assert!(listening.starts_with("127.0.0.1:") && !listening.ends_with(":0"), "{listening}");
    assert_eq!(wa.peer.public_key.len(), 44);
    assert!(wa.devices.is_empty());
    assert_eq!(b.welcome().peer.listening, None);
    assert_ne!(b.welcome().peer.public_key, wa.peer.public_key);

    assert_eq!(ack_error(invite(&b, None, None).await), "peer_listener_off");
    for ttl in [0, 59, 3601] {
        assert_eq!(ack_error(invite(&a, Some(ttl), None).await), "invalid", "ttl {ttl}");
    }
    let v = ack_result(invite(&a, Some(60), None).await);
    let expires = v["expiresAt"].as_i64().unwrap();
    assert!((expires - krypto_protocol::now_ms() - 60_000).abs() < 5_000);
    let uri = v["uri"].as_str().unwrap().to_string();
    assert!(uri.starts_with("krypto://peer?"), "{uri}");
    assert_eq!(krypto_peer::Invite::parse(&uri).unwrap().addr, listening);

    assert_eq!(ack_error(join(&b, "not an invite").await), "bad_uri");
    assert_eq!(ack_error(join(&a, &uri).await), "self_invite");

    // A wrong code is refused (and audited on the inviter) without pairing anything.
    let mut forged = krypto_peer::Invite::parse(&uri).unwrap();
    forged.code = krypto_peer::b64::encode_url(&[7u8; 16]);
    assert_eq!(ack_error(join(&b, &forged.to_uri()).await), "invite_rejected");
    let rejected = wait_audit(&a, PairingAuditKind::PairRejected).await;
    assert_eq!(rejected.detail.as_deref(), Some("invalid_invite"));
    assert!(rejected.remote_addr.is_some());
    assert!(a.devices().is_empty() && b.devices().is_empty());

    assert_eq!(
        ack_error(b.handle(ClientMsg::RevokeDevice { id: "r".into(), device_id: MID.into() }, &actor()).await),
        "not_found"
    );
}

#[tokio::test]
async fn two_hubs_pair_see_each_other_and_revoke() {
    let (a, _da) = peer_hub(MID, "Desk", Some("127.0.0.1:0")).await;
    let (b, _db) = peer_hub(OTHER, "Laptop", None).await;
    let mut arx = a.subscribe();
    let mut brx = b.subscribe();

    let uri = ack_result(invite(&a, Some(120), None).await)["uri"].as_str().unwrap().to_string();
    let v = ack_result(join(&b, &uri).await);
    let device: Device = serde_json::from_value(v["device"].clone()).unwrap();
    let machine: Machine = serde_json::from_value(v["machine"].clone()).unwrap();
    assert_eq!((device.id.as_str(), device.kind), (MID, DeviceKind::Hub));
    assert_eq!(device.addr, a.peer_status().listening);
    assert_eq!(device.public_key, a.peer_status().public_key);
    assert_eq!((machine.id.as_str(), machine.name.as_str(), machine.online), (MID, "Desk", true));

    // Both sides: the roster, then the other machine online.
    wait_for(&mut arx, "a: devices", |m| matches!(m, ServerMsg::Devices { devices } if devices.iter().any(|d| d.id == OTHER)))
        .await;
    wait_for(&mut arx, "a: laptop online", |m| is_machine(m, OTHER, true)).await;
    wait_for(&mut brx, "b: devices", |m| matches!(m, ServerMsg::Devices { devices } if devices.iter().any(|d| d.id == MID)))
        .await;
    wait_for(&mut brx, "b: desk online", |m| is_machine(m, MID, true)).await;

    let wa = a.welcome();
    assert_eq!(wa.machines.iter().map(|m| (m.id.as_str(), m.online)).collect::<Vec<_>>(), [(MID, true), (OTHER, true)]);
    assert_eq!(wa.machines[1].name, "Laptop");
    assert_eq!(wa.devices.len(), 1);
    assert_eq!(wa.devices[0].public_key, b.peer_status().public_key);
    let wb = b.welcome();
    assert_eq!(wb.machines.iter().map(|m| (m.id.as_str(), m.online)).collect::<Vec<_>>(), [(OTHER, true), (MID, true)]);
    assert_eq!(wa.machines, a.machines(), "BrainContext and Welcome share the list");

    let paired = wait_audit(&a, PairingAuditKind::Paired).await;
    assert_eq!((paired.device_id.as_deref(), paired.detail.as_deref()), (Some(OTHER), Some("accepted our invite")));
    let joined = wait_audit(&b, PairingAuditKind::Paired).await;
    assert_eq!((joined.device_id.as_deref(), joined.detail.as_deref()), (Some(MID), Some("joined their invite")));

    // The invite was single use.
    let (c, _dc) = peer_hub("cccccc333333", "Third", None).await;
    assert_eq!(ack_error(join(&c, &uri).await), "invite_rejected");

    // A rename travels by presence and is announced once.
    ack_result(b.handle(ClientMsg::UpdateMachine { id: "m".into(), machine_id: OTHER.into(), name: "Laptop 2".into() }, &actor()).await);
    wait_for(&mut arx, "a: rename", |m| matches!(m, ServerMsg::Machine { machine } if machine.id == OTHER && machine.name == "Laptop 2" && machine.online)).await;
    assert_eq!(a.welcome().machines[1].name, "Laptop 2");
    assert_eq!(a.devices()[0].name, "Laptop 2");

    // Peer machine ids are never ours: requests naming them stay not_owner.
    let err = ack_error(a.handle(ClientMsg::UpdateMachine { id: "m".into(), machine_id: OTHER.into(), name: "x".into() }, &actor()).await);
    assert_eq!(err, "not_owner");

    // B revokes A: both drop the other, both audit it, neither can reconnect.
    ack_result(b.handle(ClientMsg::RevokeDevice { id: "r".into(), device_id: MID.into() }, &actor()).await);
    wait_for(&mut brx, "b: desk removed", |m| matches!(m, ServerMsg::MachineRemoved { machine_id } if machine_id == MID)).await;
    wait_for(&mut arx, "a: laptop removed", |m| matches!(m, ServerMsg::MachineRemoved { machine_id } if machine_id == OTHER)).await;
    assert_eq!(b.welcome().machines.len(), 1);
    assert!(b.welcome().devices.is_empty());
    let revoked = wait_audit(&b, PairingAuditKind::Revoked).await;
    assert_eq!((revoked.device_id.as_deref(), revoked.actor.as_str()), (Some(MID), actor().label.as_str()));
    let by_peer = wait_audit(&a, PairingAuditKind::RevokedByPeer).await;
    assert_eq!(by_peer.device_id.as_deref(), Some(OTHER));
    for _ in 0..100 {
        if a.welcome().machines.len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(a.welcome().machines.len(), 1);
    assert!(a.store().roster().unwrap().is_empty() && b.store().roster().unwrap().is_empty());
    assert_eq!(b.store().pairing_audit(50).unwrap().iter().filter(|e| e.kind == PairingAuditKind::Revoked).count(), 1, "audited once");
    assert_eq!(
        ack_error(b.handle(ClientMsg::RevokeDevice { id: "r2".into(), device_id: MID.into() }, &actor()).await),
        "not_found"
    );
}

#[tokio::test]
async fn shutdown_says_bye_and_restart_reconnects() {
    let (a, _da) = peer_hub(MID, "Desk", Some("127.0.0.1:0")).await;
    let db = tempfile::tempdir().unwrap();
    let b = peer_hub_in(db.path(), OTHER, "Laptop", None).await;
    pair(&a, &b).await;
    let mut arx = a.subscribe();

    // bye{shutdown}: offline at once, still paired.
    b.shutdown().await;
    drop(b);
    wait_for(&mut arx, "a: laptop offline", |m| is_machine(m, OTHER, false)).await;
    let w = a.welcome();
    assert_eq!(w.machines.iter().map(|m| (m.id.as_str(), m.online)).collect::<Vec<_>>(), [(MID, true), (OTHER, false)]);
    assert_eq!(w.devices.len(), 1);
    assert!(a.store().pairing_audit(50).unwrap().iter().all(|e| e.kind == PairingAuditKind::Paired));

    // A restarted B dials every known hub (connect_known) and both are online again.
    let b = peer_hub_in(db.path(), OTHER, "Laptop", None).await;
    let mut brx = b.subscribe();
    assert_eq!(b.welcome().machines.len(), 2, "the roster survives a restart");
    wait_for(&mut arx, "a: laptop back", |m| is_machine(m, OTHER, true)).await;
    if !b.welcome().machines[1].online {
        wait_for(&mut brx, "b: desk back", |m| is_machine(m, MID, true)).await;
    }
}

/// Presence carries this hub's own pets; their names count as taken on the paired hub.
#[tokio::test]
async fn presence_pets_are_taken_names() {
    let (a, _da) = peer_hub(MID, "Desk", Some("127.0.0.1:0")).await;
    let (b, _db) = peer_hub(OTHER, "Laptop", None).await;
    pair(&a, &b).await;
    b.ensure_pet("codex").await;
    let b_pet = b.pets().into_iter().find(|p| p.agent_id == "codex").unwrap();
    assert_eq!(b_pet.machine_id, OTHER);
    for _ in 0..200 {
        if a.remote_pet_names().contains(&b_pet.name) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(a.remote_pet_names(), vec![b_pet.name.clone()]);
    a.ensure_pet("codex").await;
    let a_pet = a.pets().into_iter().find(|p| p.agent_id == "codex").unwrap();
    assert_ne!(name_key(&a_pet.name), name_key(&b_pet.name), "default names avoid the peer's pets");
    let err = ack_error(
        a.handle(
            ClientMsg::UpdatePet { id: "n".into(), pet_id: a_pet.id.clone(), patch: PetPatch { name: Some(b_pet.name.to_lowercase()), ..Default::default() } },
            &actor(),
        )
        .await,
    );
    assert_eq!(err, "name_taken");
    assert!(a.pets().iter().all(|p| p.machine_id == MID), "remote pets are not shown before step 3");
}
