//! Message-handling tests against a fake ACP host (no agent processes).

use super::host::fake::FakeHost;
use super::*;
use pawl_protocol::{
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
/// The standard fixture with `edit` applied to its config.toml.
async fn fixture_config(edit: impl FnOnce(&mut HubConfig)) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    let store = Store::open(&paths.db, MID).unwrap();
    store.replace_agents(&[agent("codex"), agent("claude-code")]).unwrap();
    let fake = FakeHost::default();
    let mut config = HubConfig { offline: true, ..Default::default() };
    edit(&mut config);
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
        created_at: pawl_protocol::now_ms(),
    }
}

async fn start(f: &Fixture) -> Session {
    let cwd = f.dir.path().to_string_lossy().into_owned();
    let v = ack_result(
        f.hub
            .handle(ClientMsg::StartSession { machine_id: None, confirmed: None, id: "1".into(), agent_id: "codex".into(), cwd, prompt: Some("run the tests".into()) }, &actor())
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
    assert_eq!(w.machine.os, pawl_protocol::MachineOs::current());
    assert_eq!(w.machine.hub_version.as_deref(), Some(crate::HUB_VERSION));
    assert!(pawl_protocol::now_ms() - w.machine.last_seen < 60_000);
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
        f.hub.handle(ClientMsg::StartSession { machine_id: None, confirmed: None, id: "7".into(), agent_id: "codex".into(), cwd: "Z:/definitely/missing".into(), prompt: None }, &actor()).await,
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
            .handle(ClientMsg::ResolvePermission { confirmed: None, id: "r0".into(), request_id: req.request_id.clone(), option_id: Some("bogus".into()) }, &actor())
            .await,
    );
    assert_eq!(err, "invalid", "an option the request does not offer");
    assert_eq!(f.hub.pending_permissions().len(), 1, "bad option keeps it pending");

    let v = ack_result(
        f.hub
            .handle(ClientMsg::ResolvePermission { confirmed: None, id: "r1".into(), request_id: req.request_id.clone(), option_id: Some("allow".into()) }, &actor())
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
        f.hub.handle(ClientMsg::ResolvePermission { confirmed: None, id: "r2".into(), request_id: req.request_id.clone(), option_id: None }, &actor()).await,
    );
    assert_eq!(err, "already_decided", "a decided request is never decided twice");
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
    assert_eq!(f.hub.rules(), pawl_brain::rules::default_rules());
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
    ended.ended_at = Some(pawl_protocol::now_ms());
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
    assert_eq!(m.pet_id, None, "no pet attached: the default pet's chat");
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
        // Actions on an unpaired machine's ids: nowhere to forward them (step 4).
        (ClientMsg::Prompt { id: "1".into(), session_id: theirs.clone(), text: "x".into() }, "not_found"),
        (ClientMsg::Prompt { id: "2".into(), session_id: local.clone(), text: "x".into() }, "not_found"),
        (ClientMsg::Cancel { id: "3".into(), session_id: theirs.clone() }, "not_found"),
        (ClientMsg::Cancel { id: "4".into(), session_id: local.clone() }, "not_found"),
        (ClientMsg::GetEvents { id: "5".into(), session_id: theirs.clone(), since_seq: None, limit: None }, "not_owner"),
        (ClientMsg::GetEvents { id: "6".into(), session_id: local.clone(), since_seq: None, limit: None }, "not_found"),
        (ClientMsg::GetEvents { id: "6b".into(), session_id: global_id(MID, "missing"), since_seq: None, limit: None }, "not_found"),
        (ClientMsg::ResolvePermission { confirmed: None, id: "7".into(), request_id: global_id(OTHER, "r"), option_id: None }, "not_found"),
        (ClientMsg::ResolvePermission { confirmed: None, id: "8".into(), request_id: "r".into(), option_id: None }, "not_found"),
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
                .handle(ClientMsg::StartSession { machine_id: None, confirmed: None, id: format!("s{i}"), agent_id: agent_id.into(), cwd: cwd.clone(), prompt: None }, &actor())
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
    // MID sorts before OTHER: on MID's hub nothing changes, on OTHER's hub its own pet yields.
    let pets = vec![pet(MID, "codex", "Mochi"), pet(OTHER, "codex", "mochi")];
    let mut on_mid = pets.clone();
    assert!(dedupe_pet_names(&mut on_mid, MID).is_empty());
    assert_eq!(on_mid, pets);
    let mut on_other = pets.clone();
    assert_eq!(dedupe_pet_names(&mut on_other, OTHER), vec![1]);
    assert_eq!(on_other[0].name, "Mochi", "never renames another machine's pet");
    assert_ne!(on_other[1].name.to_lowercase(), "mochi");
    assert_ne!(on_other[1].name, "");
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
                .handle(ClientMsg::StartSession { machine_id: None, confirmed: None, id: format!("s{i}"), agent_id: agent_id.into(), cwd: cwd.clone(), prompt: None }, &actor())
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
use pawl_protocol::{Device, DeviceKind};

/// A real hub (fake ACP host) in `dir` with its own machine id; `listen` = config.toml [peer] listen.
async fn peer_hub_in(dir: &std::path::Path, mid: &str, name: &str, listen: Option<&str>) -> Hub {
    peer_hub_with_fake(dir, mid, name, listen).await.0
}

/// `peer_hub_in`, also returning the fake ACP host (to see what the hub asked of the agent).
async fn peer_hub_with_fake(dir: &std::path::Path, mid: &str, name: &str, listen: Option<&str>) -> (Hub, FakeHost) {
    let paths = Paths::in_dir(dir);
    let store = Store::open(&paths.db, mid).unwrap();
    store.replace_agents(&[Agent { machine_id: mid.into(), ..agent("codex") }]).unwrap();
    let mut config = HubConfig { offline: true, ..Default::default() };
    config.peer.listen = listen.map(Into::into);
    let machine = MachineIdentity { id: mid.into(), name: name.into() };
    let fake = FakeHost::default();
    let hub = Hub::build(
        HubOptions { paths, config, token: "t".repeat(64), machine, background: false },
        store,
        Host::Fake(fake.clone()),
    )
    .await
    .unwrap();
    hub.start_peer_links().await.unwrap();
    (hub, fake)
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
    hub.handle(ClientMsg::CreateInvite { id: "i".into(), ttl_secs, addr: addr.map(Into::into), for_phone: None }, &actor()).await
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
    assert!((expires - pawl_protocol::now_ms() - 60_000).abs() < 5_000);
    let uri = v["uri"].as_str().unwrap().to_string();
    assert!(uri.starts_with("pawl://peer?"), "{uri}");
    assert_eq!(pawl_peer::Invite::parse(&uri).unwrap().addr.as_deref(), Some(listening.as_str()));

    assert_eq!(ack_error(join(&b, "not an invite").await), "bad_uri");
    assert_eq!(ack_error(join(&a, &uri).await), "self_invite");

    // A wrong code is refused (and audited on the inviter) without pairing anything.
    let mut forged = pawl_peer::Invite::parse(&uri).unwrap();
    forged.code = pawl_peer::b64::encode_url(&[7u8; 16]);
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
    let ids = |ms: &[Machine]| ms.iter().map(|m| (m.id.clone(), m.online)).collect::<Vec<_>>();
    assert_eq!(ids(&wa.machines), ids(&a.machines()), "BrainContext and Welcome share the list");

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

// ---------------------------------------------------------------- mirror + visitors (step 3)

use pawl_peer::RosterEntry;
use pawl_protocol::{MirrorUpdate, PeerBody, MIRROR_PAYLOAD_MAX_BYTES};

/// A machine that is neither this hub nor its peer.
const THIRD: &str = "cccccc333333";

/// Put `peer` in `hub`'s roster as a paired hub, without a link (inbound tests feed bodies directly).
async fn roster_peer(hub: &Hub, peer: &str) {
    let device = Device {
        id: peer.into(),
        kind: DeviceKind::Hub,
        name: "Mac mini".into(),
        public_key: "pk".into(),
        paired_at: 1,
        last_seen: None,
        addr: None,
        path: None,
        introduced_by: None,
        os: None,
    };
    let machine = Machine {
        id: peer.into(),
        name: "Mac mini".into(),
        os: pawl_protocol::MachineOs::Macos,
        last_seen: 1,
        online: false,
        hub_version: None,
        user_active_at: None,
    };
    hub.store().upsert_device(&RosterEntry { device, machine }).unwrap();
    hub.load_peers().await.unwrap();
}

fn remote_session(mid: &str, local: &str) -> Session {
    let now = pawl_protocol::now_ms();
    Session {
        id: global_id(mid, local),
        machine_id: mid.into(),
        agent_id: "codex".into(),
        acp_session_id: None,
        external_id: None,
        cwd: "/srv/api".into(),
        title: Some("remote".into()),
        source: SessionSource::Owned,
        status: SessionStatus::Working,
        started_at: now,
        last_event_at: now,
        ended_at: None,
    }
}

fn remote_event(mid: &str, seq: i64, session_id: &str, payload: Value) -> HubEvent {
    HubEvent {
        id: global_id(mid, seq),
        seq,
        machine_id: mid.into(),
        session_id: session_id.into(),
        kind: EventType::AgentMessageChunk,
        payload,
        ts: pawl_protocol::now_ms(),
    }
}

fn remote_pet(mid: &str, agent: &str, name: &str) -> Pet {
    Pet {
        id: global_id(mid, format!("pet-{agent}")),
        machine_id: mid.into(),
        agent_id: agent.into(),
        species: Species::Dog,
        skin: "default".into(),
        name: name.into(),
        personality: String::new(),
        mood: 60,
        xp: 1,
    }
}

fn remote_request(mid: &str, request_local: &str, session_id: &str) -> PermissionRequest {
    let mut r = permission(session_id, ToolKind::Read);
    r.request_id = global_id(mid, request_local);
    r.machine_id = mid.into();
    r
}

async fn feed(hub: &Hub, from: &str, update: MirrorUpdate) {
    hub.on_peer_body(from, PeerBody::Mirror { update }).await;
}

/// Every row of the store that belongs to `mid` (sessions, events, pets, mirrored pending).
fn rows_of(hub: &Hub, mid: &str) -> usize {
    let s = hub.store();
    let sessions = s.recent_sessions(10_000).unwrap().into_iter().filter(|x| x.machine_id == mid).count();
    let events = s.recent_events(10_000, false).unwrap().into_iter().filter(|e| e.machine_id == mid).count();
    let pets = s.pets().unwrap().into_iter().filter(|p| p.machine_id == mid).count();
    let pending = s.mirror_pending().unwrap().into_iter().filter(|r| r.machine_id == mid).count();
    sessions + events + pets + pending
}

/// The security floor of the mirror: a peer can only ever write rows of its own machine.
#[tokio::test]
async fn inbound_mirror_never_stores_items_of_this_hub_or_a_third_machine() {
    let mut f = fixture(None).await;
    let local = start(&f).await;
    let local_pet = f.hub.pets().into_iter().find(|p| p.agent_id == "codex").unwrap();
    let local_events_before = f.hub.store().events(&local.id, None, 100).unwrap();
    roster_peer(&f.hub, OTHER).await;
    drain(&mut f.rx);
    let sid = global_id(OTHER, "s1");

    // Baseline: what a well-behaved peer sends is stored.
    feed(&f.hub, OTHER, MirrorUpdate::Session { session: remote_session(OTHER, "s1") }).await;
    feed(&f.hub, OTHER, MirrorUpdate::Event { event: remote_event(OTHER, 5, &sid, json!({"text": "hi"})) }).await;

    // Sessions: this hub's machine id, this hub's id with the peer's machineId, a third machine.
    let mut hijack = local.clone();
    hijack.status = SessionStatus::Error;
    hijack.title = Some("pwned".into());
    feed(&f.hub, OTHER, MirrorUpdate::Session { session: hijack.clone() }).await;
    let mut hijack2 = hijack.clone();
    hijack2.machine_id = OTHER.into();
    feed(&f.hub, OTHER, MirrorUpdate::Session { session: hijack2.clone() }).await;
    feed(&f.hub, OTHER, MirrorUpdate::Session { session: remote_session(THIRD, "s9") }).await;
    let mut spoofed = remote_session(OTHER, "s3");
    spoofed.id = global_id(THIRD, "s3");
    feed(&f.hub, OTHER, MirrorUpdate::Session { session: spoofed }).await;

    // Events: this hub's machine, a mismatched id, into this hub's session, a third machine,
    // an oversize payload, a session that is not mirrored.
    feed(&f.hub, OTHER, MirrorUpdate::Event { event: remote_event(MID, 99, &local.id, json!({})) }).await;
    let mut mismatched = remote_event(OTHER, 7, &sid, json!({}));
    mismatched.id = global_id(OTHER, 6);
    feed(&f.hub, OTHER, MirrorUpdate::Event { event: mismatched.clone() }).await;
    feed(&f.hub, OTHER, MirrorUpdate::Event { event: remote_event(OTHER, 8, &local.id, json!({})) }).await;
    feed(&f.hub, OTHER, MirrorUpdate::Event { event: remote_event(THIRD, 9, &global_id(THIRD, "s9"), json!({})) }).await;
    let huge = json!({ "text": "x".repeat(MIRROR_PAYLOAD_MAX_BYTES) });
    feed(&f.hub, OTHER, MirrorUpdate::Event { event: remote_event(OTHER, 10, &sid, huge.clone()) }).await;
    feed(&f.hub, OTHER, MirrorUpdate::Event { event: remote_event(OTHER, 11, &global_id(OTHER, "nope"), json!({})) }).await;

    // Pets: overwrite this hub's pet (its machine id, or its id under the peer's machineId), a third machine.
    let mut pet_hijack = local_pet.clone();
    pet_hijack.name = "Pwned".into();
    feed(&f.hub, OTHER, MirrorUpdate::Pet { pet: pet_hijack.clone() }).await;
    let mut pet_hijack2 = pet_hijack.clone();
    pet_hijack2.machine_id = OTHER.into();
    feed(&f.hub, OTHER, MirrorUpdate::Pet { pet: pet_hijack2.clone() }).await;
    feed(&f.hub, OTHER, MirrorUpdate::Pet { pet: remote_pet(THIRD, "codex", "Third") }).await;

    // Requests and resolutions naming this hub or a third machine.
    feed(&f.hub, OTHER, MirrorUpdate::PermissionRequest { request: permission(&local.id, ToolKind::Read) }).await;
    let mut r = remote_request(OTHER, "r1", &sid);
    r.request_id = global_id(MID, "r1");
    feed(&f.hub, OTHER, MirrorUpdate::PermissionRequest { request: r }).await;
    feed(&f.hub, OTHER, MirrorUpdate::PermissionRequest { request: remote_request(OTHER, "r2", &global_id(THIRD, "s")) })
        .await;
    feed(
        &f.hub,
        OTHER,
        MirrorUpdate::Notification {
            notification: notify::make(MID, NotificationLevel::Alert, "spoof", "x", Some(local.id.clone()), None),
        },
    )
    .await;

    // A body from a device that is not paired at all is ignored wholesale.
    f.hub.on_peer_body(THIRD, PeerBody::Mirror { update: MirrorUpdate::Session { session: remote_session(THIRD, "s7") } }).await;

    // A snapshot and an event batch mixing good and bad items.
    f.hub
        .on_peer_body(
            OTHER,
            PeerBody::MirrorSnapshot {
                sessions: vec![remote_session(OTHER, "s1"), hijack, hijack2, remote_session(THIRD, "s8")],
                pets: vec![pet_hijack2, remote_pet(OTHER, "codex", "Biscuit"), remote_pet(THIRD, "goose", "T")],
                pending_permissions: vec![permission(&local.id, ToolKind::Read)],
            },
        )
        .await;
    f.hub
        .on_peer_body(
            OTHER,
            PeerBody::MirrorEvents {
                events: vec![
                    remote_event(OTHER, 12, &sid, json!({"text": "ok"})),
                    remote_event(MID, 13, &local.id, json!({})),
                    mismatched,
                    remote_event(OTHER, 14, &sid, huge),
                ],
            },
        )
        .await;

    // This hub's session, events and pet are untouched.
    let stored = f.hub.store().session(&local.id).unwrap().unwrap();
    assert_eq!((stored.machine_id.as_str(), &stored.title, stored.status), (MID, &local.title, local.status));
    assert_eq!(f.hub.session(&local.id).unwrap().status, local.status);
    assert_eq!(f.hub.store().events(&local.id, None, 100).unwrap(), local_events_before, "no event slipped into our session");
    let pet_now = f.hub.store().pets().unwrap().into_iter().find(|p| p.id == local_pet.id).unwrap();
    assert_eq!((pet_now.machine_id.as_str(), pet_now.name.as_str()), (MID, local_pet.name.as_str()));
    assert!(f.hub.pending_permissions().is_empty(), "nothing reached this hub's own pending set");

    // Nothing of a third machine anywhere; the peer's rows are exactly the valid ones.
    assert_eq!(rows_of(&f.hub, THIRD), 0);
    let mirrored: Vec<String> = f.hub.store().mirror_sessions(0).unwrap().into_iter().map(|s| s.id).collect();
    assert_eq!(mirrored, vec![sid.clone()]);
    let seqs: Vec<i64> = f.hub.events(&sid, None, None).await.unwrap().iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![5, 12], "mismatched, oversize and foreign events were dropped");
    let pets: Vec<(String, String)> =
        f.hub.store().pets().unwrap().into_iter().filter(|p| p.machine_id != MID).map(|p| (p.id, p.name)).collect();
    assert_eq!(pets, vec![(global_id(OTHER, "pet-codex"), "Biscuit".to_string())]);
    assert!(f.hub.store().mirror_pending().unwrap().is_empty());

    // Clients see the same: this hub's items plus the peer's valid ones; no spoofed alert.
    let w = f.hub.welcome();
    let mut ids: Vec<&str> = w.sessions.iter().map(|s| s.id.as_str()).collect();
    ids.sort();
    let mut want = vec![local.id.as_str(), sid.as_str()];
    want.sort();
    assert_eq!(ids, want);
    assert!(w.pets.iter().all(|p| p.name != "Pwned" && p.machine_id != THIRD));
    assert!(w.pending_permissions.is_empty());
    let msgs = drain(&mut f.rx);
    assert!(!msgs.iter().any(|m| matches!(m, ServerMsg::Notification { notification } if notification.title == "spoof")));
    assert!(!msgs.iter().any(|m| matches!(m, ServerMsg::Session { session } if session.title.as_deref() == Some("pwned"))));
}

/// Mirrored items are shown to clients but stay read-only; rules never run on them; revoke deletes them.
#[tokio::test]
async fn mirrored_items_are_read_only_and_revoke_deletes_them() {
    let mut f = fixture(Some(vec![rule("read", Some(ToolKind::Read), RuleAction::AutoAllow)])).await;
    roster_peer(&f.hub, OTHER).await;
    drain(&mut f.rx);
    let sid = global_id(OTHER, "s1");
    let rid = global_id(OTHER, "r1");
    let pet = remote_pet(OTHER, "codex", "Biscuit");
    feed(&f.hub, OTHER, MirrorUpdate::Session { session: remote_session(OTHER, "s1") }).await;
    feed(&f.hub, OTHER, MirrorUpdate::Event { event: remote_event(OTHER, 3, &sid, json!({"text": "hi"})) }).await;
    feed(&f.hub, OTHER, MirrorUpdate::Event { event: remote_event(OTHER, 3, &sid, json!({"text": "hi"})) }).await; // replay
    feed(&f.hub, OTHER, MirrorUpdate::Pet { pet: pet.clone() }).await;
    feed(&f.hub, OTHER, MirrorUpdate::PermissionRequest { request: remote_request(OTHER, "r1", &sid) }).await;

    let msgs = drain(&mut f.rx);
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Session { session } if session.id == sid)));
    assert_eq!(msgs.iter().filter(|m| matches!(m, ServerMsg::Event { event } if event.session_id == sid)).count(), 1, "replays are not rebroadcast");
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Pet { pet: p } if p.id == pet.id)));
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::PermissionRequest { request } if request.request_id == rid)));

    // Served to clients...
    let v = ack_result(f.hub.handle(ClientMsg::GetEvents { id: "e".into(), session_id: sid.clone(), since_seq: None, limit: None }, &actor()).await);
    assert_eq!(v["events"].as_array().unwrap().len(), 1);
    assert_eq!(v["events"][0]["id"], global_id(OTHER, 3).as_str());
    let v = ack_result(f.hub.handle(ClientMsg::ListSessions { id: "l".into(), include_ended: Some(false) }, &actor()).await);
    assert!(v["sessions"].as_array().unwrap().iter().any(|s| s["id"] == sid.as_str()));
    let w = f.hub.welcome();
    assert!(w.pending_permissions.iter().any(|r| r.request_id == rid));
    assert!(w.pets.iter().any(|p| p.id == pet.id));

    // ...but never acted on here: no auto-allow despite the read rule. Actions are forwarded to the
    // owner (offline: the peer is paired but not linked); pet and chat edits stay not_owner.
    assert!(!f.fake.calls().iter().any(|c| c.starts_with("resolve")), "rules never run on mirrored requests");
    assert!(f.hub.pending_permissions().is_empty());
    for (msg, want) in [
        (ClientMsg::Prompt { id: "p".into(), session_id: sid.clone(), text: "x".into() }, "offline"),
        (ClientMsg::Cancel { id: "c".into(), session_id: sid.clone() }, "offline"),
        (ClientMsg::ResolvePermission { confirmed: Some(true), id: "r".into(), request_id: rid.clone(), option_id: Some("allow".into()) }, "offline"),
        (ClientMsg::ResolvePermission { confirmed: None, id: "r2".into(), request_id: rid.clone(), option_id: Some("allow".into()) }, "unconfirmed"),
        (ClientMsg::UpdatePet { id: "u".into(), pet_id: pet.id.clone(), patch: PetPatch { name: Some("Rex".into()), ..Default::default() } }, "not_owner"),
        (ClientMsg::Chat { id: "h".into(), pet_id: Some(pet.id.clone()), text: "hi".into() }, "not_owner"),
    ] {
        assert_eq!(ack_error(f.hub.handle(msg.clone(), &actor()).await), want, "{msg:?}");
    }
    // A peer's pet name is taken here too.
    f.hub.ensure_pet("codex").await;
    let mine = f.hub.pets().into_iter().find(|p| p.agent_id == "codex").unwrap();
    assert_ne!(mine.name, "Biscuit");

    // The owner resolved it: gone here as well.
    feed(
        &f.hub,
        OTHER,
        MirrorUpdate::PermissionResolved {
            request_id: rid.clone(),
            session_id: sid.clone(),
            outcome: PermissionOutcome::Selected { option_id: "allow".into() },
            by: ResolvedBy::User,
            by_machine_id: None,
        },
    )
    .await;
    assert!(f.hub.welcome().pending_permissions.is_empty());
    assert!(f.hub.store().mirror_pending().unwrap().is_empty());

    // Revoke deletes every mirrored row of that machine (and nothing of this one).
    ack_result(f.hub.handle(ClientMsg::RevokeDevice { id: "rv".into(), device_id: OTHER.into() }, &actor()).await);
    assert_eq!(rows_of(&f.hub, OTHER), 0);
    assert!(f.hub.welcome().sessions.iter().all(|s| s.machine_id == MID));
    assert!(f.hub.welcome().pets.iter().all(|p| p.machine_id == MID));
    assert_eq!(f.hub.pets().len(), 1, "this hub's pet stays");
    let err = ack_error(f.hub.handle(ClientMsg::GetEvents { id: "e2".into(), session_id: sid, since_seq: None, limit: None }, &actor()).await);
    assert_eq!(err, "not_owner");
}

/// Only this machine's items go out; big payloads go out truncated.
#[tokio::test]
async fn only_this_machines_items_are_mirrored_out() {
    let f = fixture(None).await;
    let local = start(&f).await;
    let remote = remote_session(OTHER, "s1");
    assert!(f.hub.outbound_update(ServerMsg::Session { session: remote.clone() }).is_none(), "no relaying");
    assert!(matches!(f.hub.outbound_update(ServerMsg::Session { session: local.clone() }), Some(MirrorUpdate::Session { .. })));
    let ev = remote_event(OTHER, 1, &remote.id, json!({}));
    assert!(f.hub.outbound_update(ServerMsg::Event { event: ev }).is_none());
    let mut big = remote_event(MID, 1, &local.id, json!({ "text": "x".repeat(MIRROR_PAYLOAD_MAX_BYTES) }));
    big.machine_id = MID.into();
    let Some(MirrorUpdate::Event { event }) = f.hub.outbound_update(ServerMsg::Event { event: big }) else { panic!("local event") };
    assert_eq!(event.payload["truncated"], true);
    assert!(f.hub.outbound_update(ServerMsg::Pet { pet: remote_pet(OTHER, "codex", "B") }).is_none());
    let resolved = |rid: String| ServerMsg::PermissionResolved {
        request_id: rid,
        session_id: local.id.clone(),
        outcome: PermissionOutcome::Cancelled,
        by: ResolvedBy::System,
        by_machine_id: None,
    };
    assert!(f.hub.outbound_update(resolved(global_id(OTHER, "r"))).is_none());
    assert!(f.hub.outbound_update(resolved(global_id(MID, "r"))).is_some());
    let n = notify::make(OTHER, NotificationLevel::Info, "t", "b", None, None);
    assert!(f.hub.outbound_update(ServerMsg::Notification { notification: n }).is_none());
    assert!(f.hub.outbound_update(ServerMsg::Agents { agents: vec![] }).is_none());
}

/// Poll `cond` until true (10 s cap).
async fn wait_until<F, Fut>(what: &str, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    for _ in 0..200 {
        if cond().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn event_ids(hub: &Hub, sid: &str) -> Vec<String> {
    hub.events(sid, None, Some(5000)).await.map(|v| v.into_iter().map(|e| e.id).collect()).unwrap_or_default()
}

async fn say(hub: &Hub, sid: &str, text: &str) {
    hub.on_host_event(HostEvent::Event {
        session_id: sid.into(),
        kind: EventType::AgentMessageChunk,
        payload: json!({ "text": text }),
    })
    .await;
}

/// End to end over real peer links: live mirroring, a sync that fills an offline gap, activity
/// making the peer the active machine, and revoke deleting the mirror.
#[tokio::test]
async fn two_hubs_mirror_live_resync_and_revoke() {
    let (a, da) = peer_hub(MID, "Desk", Some("127.0.0.1:0")).await;
    let db = tempfile::tempdir().unwrap();
    let b = peer_hub_in(db.path(), OTHER, "Laptop", None).await;
    pair(&a, &b).await;

    // Live: a session, its events (one oversize), its pet and a pending request on A appear on B.
    let cwd = da.path().to_string_lossy().into_owned();
    let v = ack_result(a.handle(ClientMsg::StartSession { machine_id: None, confirmed: None, id: "s".into(), agent_id: "codex".into(), cwd, prompt: None }, &actor()).await);
    let s: Session = serde_json::from_value(v["session"].clone()).unwrap();
    say(&a, &s.id, "hello").await;
    say(&a, &s.id, &"x".repeat(MIRROR_PAYLOAD_MAX_BYTES)).await;
    let req = permission(&s.id, ToolKind::Execute);
    a.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    let a_ids = event_ids(&a, &s.id).await;
    assert!(a_ids.len() >= 2);
    wait_until("b mirrors the session and events", || {
        let (b, sid, want) = (b.clone(), s.id.clone(), a_ids.clone());
        async move { event_ids(&b, &sid).await == want }
    })
    .await;
    let wb = b.welcome();
    assert!(wb.sessions.iter().any(|x| x.id == s.id && x.machine_id == MID));
    assert!(wb.pets.iter().any(|p| p.machine_id == MID && p.agent_id == "codex"), "A's pet visits B");
    wait_until("b sees the pending request", || {
        let b = b.clone();
        let rid = req.request_id.clone();
        async move { b.welcome().pending_permissions.iter().any(|r| r.request_id == rid) }
    })
    .await;
    let b_events = b.events(&s.id, None, None).await.unwrap();
    assert!(b_events.iter().any(|e| e.payload["truncated"] == true), "the oversize payload arrived truncated");
    assert!(a.events(&s.id, None, None).await.unwrap().iter().all(|e| e.payload.get("truncated").is_none()), "the owner keeps it whole");
    // Step 4: B can act on A's session; the prompt is forwarded and applied on A.
    ack_result(b.handle(ClientMsg::Prompt { id: "p".into(), session_id: s.id.clone(), text: "x".into() }, &actor()).await);

    // Resolved on A: gone on B.
    ack_result(a.handle(ClientMsg::ResolvePermission { confirmed: None, id: "r".into(), request_id: req.request_id.clone(), option_id: Some("allow".into()) }, &actor()).await);
    wait_until("b drops the resolved request", || {
        let b = b.clone();
        async move { b.welcome().pending_permissions.is_empty() }
    })
    .await;

    // B goes away; A keeps working; B comes back and the sync fills the gap.
    b.shutdown().await;
    drop(b);
    for i in 0..5 {
        say(&a, &s.id, &format!("while you were away {i}")).await;
    }
    let a_ids = event_ids(&a, &s.id).await;
    let b = peer_hub_in(db.path(), OTHER, "Laptop", None).await;
    assert!(b.welcome().sessions.iter().any(|x| x.id == s.id), "the mirror survives a restart");
    wait_until("the sync fills the gap", || {
        let (b, sid, want) = (b.clone(), s.id.clone(), a_ids.clone());
        async move { event_ids(&b, &sid).await == want }
    })
    .await;
    assert!(b.store().mirror_cursor(MID).unwrap() >= a.events(&s.id, None, None).await.unwrap().last().unwrap().seq);

    // The user is on B: A's Welcome makes B the active machine.
    assert_eq!(a.active_machine_id(), MID);
    ack_result(b.handle(ClientMsg::UserActivity { id: "ua".into() }, &actor()).await);
    assert!(b.machine().user_active_at.is_some());
    wait_until("a sees b active", || {
        let a = a.clone();
        async move { a.active_machine_id() == OTHER }
    })
    .await;
    let wa = a.welcome();
    let bm = wa.machines.iter().find(|m| m.id == OTHER).unwrap();
    assert!(bm.online && bm.user_active_at.is_some());
    assert_eq!(active_machine(&wa.machines, MID, pawl_protocol::now_ms()), OTHER);

    // Revoke on A: B deletes its mirror of A.
    ack_result(a.handle(ClientMsg::RevokeDevice { id: "rv".into(), device_id: OTHER.into() }, &actor()).await);
    wait_until("b deletes the mirror of a", || {
        let b = b.clone();
        async move { rows_of(&b, MID) == 0 && b.welcome().sessions.iter().all(|x| x.machine_id == OTHER) }
    })
    .await;
    assert!(b.welcome().sessions.iter().all(|x| x.machine_id == OTHER));
    assert!(b.welcome().pets.iter().all(|p| p.machine_id == OTHER));
    assert_eq!(rows_of(&a, OTHER), 0);
}

/// Cross-machine pet name clashes settle deterministically: the machine whose id sorts first keeps
/// the name; the other owner renames its own pet as soon as it sees the clash.
#[tokio::test]
async fn pet_name_clashes_with_paired_machines_settle_on_one_side() {
    const FIRST: &str = "aaaaaa000000"; // sorts before MID
    let mut f = fixture(None).await;
    f.hub.ensure_pet("codex").await;
    let mine = f.hub.pets().into_iter().find(|p| p.agent_id == "codex").unwrap();
    roster_peer(&f.hub, OTHER).await;
    roster_peer(&f.hub, FIRST).await;
    drain(&mut f.rx);

    // OTHER sorts after MID: its clashing pet changes nothing here (OTHER's hub renames).
    feed(&f.hub, OTHER, MirrorUpdate::Pet { pet: remote_pet(OTHER, "gemini", &mine.name.to_uppercase()) }).await;
    assert_eq!(f.hub.pets().into_iter().find(|p| p.id == mine.id).unwrap().name, mine.name);

    // FIRST sorts before MID: ours yields, is saved and announced.
    feed(&f.hub, FIRST, MirrorUpdate::Pet { pet: remote_pet(FIRST, "goose", &mine.name) }).await;
    let now = f.hub.pets().into_iter().find(|p| p.id == mine.id).unwrap();
    assert_ne!(name_key(&now.name), name_key(&mine.name));
    assert_eq!(f.hub.store().pets().unwrap().into_iter().find(|p| p.id == mine.id).unwrap().name, now.name);
    assert!(drain(&mut f.rx).iter().any(|m| matches!(m, ServerMsg::Pet { pet } if pet.id == mine.id && pet.name == now.name)));
    let all: Vec<String> = f.hub.welcome().pets.iter().map(|p| name_key(&p.name)).collect();
    assert_eq!(all.iter().filter(|n| **n == name_key(&mine.name)).count(), 2, "the two other machines keep theirs");
    assert!(!all.contains(&name_key(&now.name)) || all.iter().filter(|n| **n == name_key(&now.name)).count() == 1);
}

// ---------------------------------------------------------------- review fixes (step 3)

async fn body(hub: &Hub, from: &str, b: PeerBody) {
    hub.on_peer_body(from, b).await;
}

fn cursor(hub: &Hub, peer: &str) -> i64 {
    hub.store().mirror_cursor(peer).unwrap()
}

/// Fix 1: the in-memory mirror never holds more sessions than the store keeps; evicted sessions
/// disappear from every view.
#[tokio::test]
async fn mirror_cache_never_outgrows_the_store() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    let max = crate::store::MIRROR_MAX_SESSIONS;
    let now = pawl_protocol::now_ms();
    for i in 0..(max + 5) {
        let mut s = remote_session(OTHER, &format!("s{i:04}"));
        s.last_event_at = now - (max + 5 - i) * 1000;
        feed(&f.hub, OTHER, MirrorUpdate::Session { session: s }).await;
    }
    assert_eq!(f.hub.mirror_sessions(|_| true).len() as i64, max);
    let w = f.hub.welcome();
    for i in 0..5 {
        let id = global_id(OTHER, format!("s{i:04}"));
        assert!(!f.hub.is_mirrored_session(&id).await.unwrap(), "{id} was evicted");
        assert!(w.sessions.iter().all(|s| s.id != id));
    }
    assert_eq!(w.sessions.len() as i64, max);
    // A session older than everything kept is evicted at once and never shown.
    let mut ancient = remote_session(OTHER, "ancient");
    ancient.last_event_at = 1;
    feed(&f.hub, OTHER, MirrorUpdate::Session { session: ancient.clone() }).await;
    assert!(!f.hub.is_mirrored_session(&ancient.id).await.unwrap());
    assert_eq!(f.hub.mirror_sessions(|_| true).len() as i64, max);
}

/// Fixes 2 and 3: sync_done counts only for a sync we asked for on this link; the cursor is the
/// highest seq actually stored; live events before the sync never move it; a seq that went back
/// means the peer's hub.db was reset.
#[tokio::test]
async fn sync_cursor_comes_only_from_stored_events_of_a_requested_sync() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    let sid = global_id(OTHER, "s1");
    feed(&f.hub, OTHER, MirrorUpdate::Session { session: remote_session(OTHER, "s1") }).await;
    let ev = |seq: i64| remote_event(OTHER, seq, &sid, json!({ "n": seq }));

    body(&f.hub, OTHER, PeerBody::SyncDone { seq: 1_000_000, truncated: false }).await;
    assert_eq!(cursor(&f.hub, OTHER), 0, "an unsolicited sync_done is ignored");
    assert!(!f.hub.is_synced(OTHER));
    feed(&f.hub, OTHER, MirrorUpdate::Event { event: ev(3) }).await;
    assert_eq!(cursor(&f.hub, OTHER), 0, "live events before a sync never move the cursor");

    f.hub.expect_sync(OTHER);
    body(&f.hub, OTHER, PeerBody::MirrorEvents { events: vec![ev(4), ev(5), remote_event(OTHER, 900, &global_id(OTHER, "gone"), json!({}))] })
        .await;
    body(&f.hub, OTHER, PeerBody::SyncDone { seq: 1_000_000, truncated: false }).await;
    assert_eq!(cursor(&f.hub, OTHER), 5, "the claimed seq and skipped events never set the cursor");
    assert!(f.hub.is_synced(OTHER));
    feed(&f.hub, OTHER, MirrorUpdate::Event { event: ev(6) }).await;
    assert_eq!(cursor(&f.hub, OTHER), 6, "in sync: live events advance it");
    body(&f.hub, OTHER, PeerBody::SyncDone { seq: 2, truncated: false }).await;
    assert_eq!(cursor(&f.hub, OTHER), 6, "a second sync_done without a new request is ignored");

    // The peer's seq space restarted: its mirrored events go, the cursor resets, the session stays.
    f.hub.expect_sync(OTHER);
    body(&f.hub, OTHER, PeerBody::SyncDone { seq: 2, truncated: false }).await;
    assert_eq!(cursor(&f.hub, OTHER), 0);
    assert!(f.hub.events(&sid, None, None).await.unwrap().is_empty());
    assert!(f.hub.is_mirrored_session(&sid).await.unwrap());
    assert!(!f.hub.is_synced(OTHER));
}

/// Fix 4: more than MIRROR_FLOOD_FRAMES bodies in a second: the rest of that second is dropped and
/// the link loses its sync state (and is disconnected for MIRROR_FLOOD_COOLDOWN).
#[tokio::test]
async fn a_flooding_peer_is_cut_off() {
    let mut f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    let sid = global_id(OTHER, "s1");
    feed(&f.hub, OTHER, MirrorUpdate::Session { session: remote_session(OTHER, "s1") }).await;
    f.hub.expect_sync(OTHER);
    body(&f.hub, OTHER, PeerBody::SyncDone { seq: 0, truncated: false }).await;
    assert!(f.hub.is_synced(OTHER));
    drain(&mut f.rx);
    let note = || MirrorUpdate::Notification { notification: notify::make(OTHER, NotificationLevel::Info, "n", "b", None, None) };
    let started = std::time::Instant::now();
    for _ in 0..(mirror::MIRROR_FLOOD_FRAMES - 3) {
        feed(&f.hub, OTHER, note()).await;
    }
    assert!(started.elapsed() < Duration::from_millis(900), "the flood must fit in one window");
    feed(&f.hub, OTHER, MirrorUpdate::Event { event: remote_event(OTHER, 1, &sid, json!({})) }).await; // frame 1000: kept
    feed(&f.hub, OTHER, MirrorUpdate::Event { event: remote_event(OTHER, 2, &sid, json!({})) }).await; // frame 1001: dropped
    let seqs: Vec<i64> = f.hub.events(&sid, None, None).await.unwrap().iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![1]);
    assert!(!f.hub.is_synced(OTHER), "a cut-off link resyncs when it comes back");
}

/// Fix 5: a sync_request storm costs at most one answer in flight and one per SYNC_COOLDOWN.
#[tokio::test]
async fn sync_requests_are_answered_at_most_once_per_cooldown() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    for _ in 0..20 {
        body(&f.hub, OTHER, PeerBody::SyncRequest { since_seq: 0 }).await;
    }
    tokio::time::sleep(mirror::SYNC_COOLDOWN / 3).await;
    assert_eq!(f.hub.syncs_answered(), 1);
    tokio::time::sleep(mirror::SYNC_COOLDOWN * 3).await;
    assert_eq!(f.hub.syncs_answered(), 2, "the extras were coalesced into one deferred answer");
    tokio::time::sleep(mirror::SYNC_COOLDOWN * 2).await;
    assert_eq!(f.hub.syncs_answered(), 2);
}

/// Fix 6: mirrored pets follow the local name rules.
#[tokio::test]
async fn mirrored_pets_are_validated_like_local_ones() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    for (agent, name) in [("a", ""), ("b", "   "), ("c", &*"x".repeat(41)), ("d", "Rex\u{7}"), ("e", "Bad\nName")] {
        feed(&f.hub, OTHER, MirrorUpdate::Pet { pet: remote_pet(OTHER, agent, name) }).await;
    }
    let mut no_skin = remote_pet(OTHER, "f", "Fine");
    no_skin.skin = String::new();
    feed(&f.hub, OTHER, MirrorUpdate::Pet { pet: no_skin }).await;
    let mut long_agent = remote_pet(OTHER, "g", "Fine");
    long_agent.agent_id = "g".repeat(200);
    feed(&f.hub, OTHER, MirrorUpdate::Pet { pet: long_agent }).await;
    assert!(f.hub.store().pets().unwrap().iter().all(|p| p.machine_id != OTHER), "every invalid pet was dropped");
    feed(&f.hub, OTHER, MirrorUpdate::Pet { pet: remote_pet(OTHER, "h", "  Rex  ") }).await;
    let names: Vec<String> = f.hub.store().pets().unwrap().into_iter().filter(|p| p.machine_id == OTHER).map(|p| p.name).collect();
    assert_eq!(names, vec!["Rex".to_string()], "stored trimmed");
}

/// Fix 6: one peer can cause at most MAX_PEER_RENAMES_PER_HOUR renames of this hub's pets, and a
/// clash set that did not change is not re-settled.
#[tokio::test]
async fn a_peer_can_force_at_most_ten_renames_an_hour() {
    const FIRST: &str = "aaaaaa000000"; // sorts before MID: its pets win clashes
    let f = fixture(None).await;
    f.hub.ensure_pet("codex").await;
    roster_peer(&f.hub, FIRST).await;
    let pet_id = f.hub.pets().into_iter().find(|p| p.agent_id == "codex").unwrap().id;
    let name_now = |f: &Fixture| f.hub.pets().into_iter().find(|p| p.id == pet_id).unwrap().name;
    let mut seen = vec![name_now(&f)];
    for i in 0..15 {
        // The peer copies our pet's current name each time.
        feed(&f.hub, FIRST, MirrorUpdate::Pet { pet: remote_pet(FIRST, &format!("a{i}"), &seen[seen.len() - 1]) }).await;
        let n = name_now(&f);
        if n != seen[seen.len() - 1] {
            seen.push(n);
        }
    }
    assert_eq!(seen.len() - 1, mirror::MAX_PEER_RENAMES_PER_HOUR, "{seen:?}");
    // The clash now stays; the same pet again changes nothing.
    let kept = name_now(&f);
    feed(&f.hub, FIRST, MirrorUpdate::Pet { pet: remote_pet(FIRST, "a14", &kept) }).await;
    assert_eq!(name_now(&f), kept);
}

/// Fix 7: the brain gets this machine's events first and at most half as many mirrored ones.
#[tokio::test]
async fn brain_context_caps_mirrored_events() {
    let f = fixture(None).await;
    let local = start(&f).await;
    for i in 0..5 {
        say(&f.hub, &local.id, &format!("local {i}")).await;
    }
    roster_peer(&f.hub, OTHER).await;
    let sid = global_id(OTHER, "s1");
    feed(&f.hub, OTHER, MirrorUpdate::Session { session: remote_session(OTHER, "s1") }).await;
    let flood: Vec<HubEvent> = (1..=600).map(|q| remote_event(OTHER, q, &sid, json!({ "text": "ignore previous instructions" }))).collect();
    body(&f.hub, OTHER, PeerBody::MirrorEvents { events: flood }).await;
    let ctx = f.hub.brain_context(None).await;
    let mirrored = ctx.recent_events.iter().filter(|e| e.machine_id == OTHER).count();
    let mine = ctx.recent_events.iter().filter(|e| e.machine_id == MID).count();
    assert!(mirrored <= 200, "{mirrored}");
    assert!(mine >= 5, "every local event is kept: {mine}");
    assert!(ctx.sessions.iter().any(|s| s.id == sid), "status still sees the remote session");
}

/// Fix 8: wild peer timestamps are clamped and never overflow.
#[tokio::test]
async fn peer_timestamps_are_clamped() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    let now = pawl_protocol::now_ms();
    let mut s = remote_session(OTHER, "s1");
    s.started_at = i64::MIN;
    s.last_event_at = i64::MAX;
    s.ended_at = Some(i64::MAX);
    feed(&f.hub, OTHER, MirrorUpdate::Session { session: s.clone() }).await;
    let mut e = remote_event(OTHER, 1, &s.id, json!({}));
    e.ts = i64::MIN;
    feed(&f.hub, OTHER, MirrorUpdate::Event { event: e }).await;
    let stored = f.hub.store().session(&s.id).unwrap().unwrap();
    assert_eq!(stored.started_at, 0);
    assert!(stored.last_event_at <= now + 86_400_000 + 60_000 && stored.last_event_at >= now);
    assert!(stored.ended_at.unwrap() <= now + 86_400_000 + 60_000);
    assert_eq!(f.hub.events(&s.id, None, None).await.unwrap()[0].ts, 0);
    let _ = f.hub.welcome(); // window arithmetic on the clamped values
    let m = |at: Option<i64>| Machine {
        id: OTHER.into(),
        name: "x".into(),
        os: pawl_protocol::MachineOs::Linux,
        last_seen: 0,
        online: true,
        hub_version: None,
        user_active_at: at,
    };
    for at in [Some(i64::MIN), Some(i64::MAX), Some(-1), Some(now + 1)] {
        assert_eq!(active_machine(&[m(at)], MID, now), MID, "{at:?} is never a valid activity time");
    }
}

// ---------------------------------------------------------------- intents (step 4)

use super::intents::{Ack, INTENT_TIMEOUT};
use crate::store::{AuditOrigin, IntentRole};
use pawl_protocol::{Intent, IntentRejectReason as Reject, IntentStatus};

/// The owner's answer to an intent from `from` (sent by a desktop client).
async fn intent(hub: &Hub, from: &str, id: &str, confirmed: bool, i: Intent) -> Ack {
    hub.handle_intent(from, id, ClientKind::Desktop, confirmed, &i).await.expect("answered")
}

fn reason(a: &Ack) -> Option<Reject> {
    (a.status == IntentStatus::Rejected).then_some(a.reason).flatten()
}

fn approve(request_id: &str, option: &str) -> Intent {
    Intent::Approve { request_id: request_id.into(), option_id: option.into() }
}

/// A session of this hub with one pending request (options "allow" / "reject"; execute = ask).
async fn pending_request(f: &Fixture) -> PermissionRequest {
    let s = start(f).await;
    let req = permission(&s.id, ToolKind::Execute);
    f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    assert!(f.hub.pending_permissions().iter().any(|r| r.request_id == req.request_id));
    req
}

fn resolves(f: &Fixture, request_id: &str) -> usize {
    f.fake.calls().iter().filter(|c| c.starts_with(&format!("resolve {request_id}"))).count()
}

/// A remote approval is applied through the normal path: the agent gets the answer, the audit log
/// names the origin, clients see byMachineId, and this machine is told.
#[tokio::test]
async fn remote_approve_is_applied_audited_and_announced() {
    let mut f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    let req = pending_request(&f).await;
    drain(&mut f.rx);
    let a = intent(&f.hub, OTHER, "i-1", true, approve(&req.request_id, "allow")).await;
    assert_eq!(a.status, IntentStatus::Applied, "{a:?}");
    assert_eq!(a.result, Some(json!({})));
    assert_eq!(resolves(&f, &req.request_id), 1);
    assert!(f.hub.pending_permissions().is_empty());

    let audit = &f.hub.store().audit_log(5).unwrap()[0];
    assert_eq!((audit.request_id.as_str(), audit.outcome), (req.request_id.as_str(), AuditOutcome::Allowed));
    assert_eq!(audit.decided_by, ResolvedBy::User, "the user decided; rules never re-ran");
    assert_eq!(audit.actor, "desktop@Mac mini");
    assert_eq!(
        audit.origin,
        Some(AuditOrigin { machine_id: OTHER.into(), name: Some("Mac mini".into()), intent_id: "i-1".into(), client: "desktop".into() })
    );
    let log = &f.hub.store().intent_log(5).unwrap()[0];
    assert_eq!((log.role, log.kind.as_str(), log.peer_machine_id.as_str()), (IntentRole::Owner, "approve", OTHER));
    assert_eq!((log.status.as_str(), log.option_id.as_deref()), ("applied", Some("allow")));

    let msgs = drain(&mut f.rx);
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::PermissionResolved { request_id, by: ResolvedBy::User, by_machine_id: Some(by), .. }
        if *request_id == req.request_id && by == OTHER)));
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Notification { notification }
        if notification.title == "Approved from Mac mini" && notification.body == "cargo test")));
    let ev = f.hub.store().events(&req.session_id, None, 100).unwrap().into_iter().rev().find(|e| e.kind == EventType::PermissionResolved).unwrap();
    assert_eq!((ev.payload["byMachineId"].as_str(), ev.payload["optionKind"].as_str()), (Some(OTHER), Some("allow_once")));
}

/// First valid decision wins: two intents, or a local click and an intent, for one request give
/// exactly one `applied`; the other is `already_decided` and the agent is answered once.
#[tokio::test]
async fn first_decision_wins_between_intents_and_local_clicks() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;

    // Two intents racing.
    let r1 = pending_request(&f).await;
    let (a, b) = tokio::join!(
        intent(&f.hub, OTHER, "race-a", true, approve(&r1.request_id, "allow")),
        intent(&f.hub, OTHER, "race-b", true, approve(&r1.request_id, "allow")),
    );
    let mut got = [a.status == IntentStatus::Applied, b.status == IntentStatus::Applied];
    got.sort();
    assert_eq!(got, [false, true], "{a:?} {b:?}");
    assert!([reason(&a), reason(&b)].contains(&Some(Reject::AlreadyDecided)));
    assert_eq!(resolves(&f, &r1.request_id), 1);

    // A local click and an intent racing.
    let r2 = pending_request(&f).await;
    let local = ClientMsg::ResolvePermission { confirmed: None, id: "l".into(), request_id: r2.request_id.clone(), option_id: Some("reject".into()) };
    let me = actor();
    let (click, remote) = tokio::join!(f.hub.handle(local, &me), intent(&f.hub, OTHER, "race-c", true, approve(&r2.request_id, "allow")));
    let click_ok = matches!(click, ServerMsg::Ack { ok: true, .. });
    assert_ne!(click_ok, remote.status == IntentStatus::Applied, "exactly one wins: {click:?} {remote:?}");
    if click_ok {
        assert_eq!(reason(&remote), Some(Reject::AlreadyDecided));
    } else {
        assert_eq!(ack_error(click), "already_decided", "the local path answers already_decided too");
    }
    assert_eq!(resolves(&f, &r2.request_id), 1);

    // Later answers, either way, are already_decided (also after the in-memory record is gone).
    assert_eq!(reason(&intent(&f.hub, OTHER, "late", true, approve(&r1.request_id, "allow")).await), Some(Reject::AlreadyDecided));
    let late = ClientMsg::ResolvePermission { confirmed: None, id: "l2".into(), request_id: r1.request_id.clone(), option_id: None };
    assert_eq!(ack_error(f.hub.handle(late.clone(), &actor()).await), "already_decided");
    f.hub.st().decided.clear();
    assert_eq!(ack_error(f.hub.handle(late, &actor()).await), "already_decided", "the audit log remembers");
}

/// The owner checks every intent against its own state.
#[tokio::test]
async fn the_owner_validates_intents() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    let req = pending_request(&f).await;
    let rid = req.request_id.clone();
    let cases = [
        ("unconfirmed approve", false, approve(&rid, "allow"), Reject::Unconfirmed),
        ("approve with a reject option", true, approve(&rid, "reject"), Reject::Invalid),
        ("deny with an allow option", false, Intent::Deny { request_id: rid.clone(), option_id: Some("allow".into()) }, Reject::Invalid),
        ("unknown option", true, approve(&rid, "bogus"), Reject::Invalid),
        ("a third machine's request", true, approve(&global_id(THIRD, "r"), "allow"), Reject::NotOwner),
        ("the origin's own request", true, approve(&global_id(OTHER, "r"), "allow"), Reject::NotOwner),
        ("a third machine's session", false, Intent::Prompt { session_id: global_id(THIRD, "s"), text: "hi".into() }, Reject::NotOwner),
        ("cancel a third machine's session", false, Intent::Cancel { session_id: global_id(THIRD, "s") }, Reject::NotOwner),
        ("unknown request", true, approve(&global_id(MID, "nope"), "allow"), Reject::NotFound),
        ("unprefixed id", true, approve("nope", "allow"), Reject::NotFound),
        ("unknown session", false, Intent::Prompt { session_id: global_id(MID, "nope"), text: "hi".into() }, Reject::NotFound),
        ("empty prompt", false, Intent::Prompt { session_id: req.session_id.clone(), text: "  ".into() }, Reject::Invalid),
        ("oversize prompt", false, Intent::Prompt { session_id: req.session_id.clone(), text: "x".repeat(intents::MAX_PROMPT_CHARS + 1) }, Reject::Invalid),
        ("unconfirmed start", false, Intent::Start { agent_id: "codex".into(), cwd: f.dir.path().to_string_lossy().into(), prompt: None }, Reject::Unconfirmed),
    ];
    for (i, (what, confirmed, intent_body, want)) in cases.into_iter().enumerate() {
        let a = intent(&f.hub, OTHER, &format!("v{i}"), confirmed, intent_body).await;
        assert_eq!(reason(&a), Some(want), "{what}: {a:?}");
    }
    assert_eq!(f.hub.pending_permissions().len(), 1, "nothing was decided");
    assert_eq!(resolves(&f, &rid), 0);
    assert!(f.hub.store().audit_log(10).unwrap().is_empty());
    // A deny with a reject option (or none) needs no confirmation.
    assert_eq!(intent(&f.hub, OTHER, "deny", false, Intent::Deny { request_id: rid.clone(), option_id: None }).await.status, IntentStatus::Applied);
    assert_eq!(f.hub.store().audit_log(1).unwrap()[0].outcome, AuditOutcome::Cancelled);
}

/// M3: a remote start runs the agent on the owner only, in a folder it already knows: a previous
/// local session's folder (or inside it), or inside a `[peer] start_roots` entry. Anything else,
/// and share/device paths, are `forbidden`; relative or missing paths are `invalid`.
#[tokio::test]
async fn remote_start_runs_only_in_folders_this_machine_knows() {
    let roots = tempfile::tempdir().unwrap();
    let root_str = roots.path().to_string_lossy().into_owned();
    let f = fixture_config(move |c| c.peer.start_roots = vec![root_str]).await;
    roster_peer(&f.hub, OTHER).await;
    let known = f.dir.path().join("repo");
    let inside = known.join("crates");
    let unknown = f.dir.path().join("elsewhere");
    let in_root = roots.path().join("project");
    for d in [&inside, &unknown, &in_root] {
        std::fs::create_dir_all(d).unwrap();
    }
    let s = |p: &std::path::Path| p.to_string_lossy().into_owned();
    let start = |agent: &str, cwd: String| Intent::Start { agent_id: agent.into(), cwd, prompt: None };
    // Before any local session there, its folder is unknown.
    assert_eq!(reason(&intent(&f.hub, OTHER, "s0", true, start("codex", s(&known))).await), Some(Reject::Forbidden));
    // A local session in `known` makes it (and what is inside it) a known folder.
    ack_result(f.hub.handle(ClientMsg::StartSession { machine_id: None, confirmed: None, id: "l".into(), agent_id: "codex".into(), cwd: s(&known), prompt: None }, &actor()).await);
    let missing = f.dir.path().join("missing");
    let cases: Vec<(&str, Intent, Option<Reject>)> = vec![
        ("relative", start("codex", "src".into()), Some(Reject::Invalid)),
        ("home shorthand", start("codex", "~".into()), Some(Reject::Invalid)),
        ("missing", start("codex", s(&missing)), Some(Reject::Invalid)),
        ("unknown agent", start("goose", s(&known)), Some(Reject::Invalid)),
        ("unknown folder", start("codex", s(&unknown)), Some(Reject::Forbidden)),
        ("escape with ..", start("codex", s(&known.join("..").join("elsewhere"))), Some(Reject::Forbidden)),
        ("a share", start("codex", r"\\server\share\repo".into()), Some(Reject::Forbidden)),
        ("a forward-slash share", start("codex", "//server/share/repo".into()), Some(Reject::Forbidden)),
        ("a device path", start("codex", r"\\.\C:\repo".into()), Some(Reject::Forbidden)),
        ("a verbatim share", start("codex", r"\\?\UNC\server\share".into()), Some(Reject::Forbidden)),
        ("the known folder", start("codex", s(&known)), None),
        ("inside it", start("codex", s(&inside)), None),
        ("inside a start root", start("codex", s(&in_root)), None),
    ];
    for (i, (what, body, want)) in cases.into_iter().enumerate() {
        let a = intent(&f.hub, OTHER, &format!("c{i}"), true, body).await;
        assert_eq!(reason(&a), want, "{what}: {a:?}");
        if want.is_none() {
            let session: Session = serde_json::from_value(a.result.unwrap()["session"].clone()).unwrap();
            assert_eq!(session.machine_id, MID, "{what}: runs on the owner");
            assert!(!session.cwd.starts_with(r"\\?\"), "{what}: the plain canonical path is used: {}", session.cwd);
        }
    }
    #[cfg(windows)]
    {
        // Case-insensitive, and the verbatim form of a known drive path is the same folder.
        let upper = s(&inside).to_uppercase();
        assert_eq!(reason(&intent(&f.hub, OTHER, "w1", true, start("codex", upper)).await), None);
        let verbatim = format!(r"\\?\{}", s(&std::fs::canonicalize(&inside).map(|p| crate::repos::plain_path(&p).unwrap()).unwrap()));
        assert_eq!(reason(&intent(&f.hub, OTHER, "w2", true, start("codex", verbatim)).await), Some(Reject::Forbidden), "raw \\\\ paths are refused outright");
    }
}

#[test]
fn plain_paths_drop_the_verbatim_prefix_and_refuse_shares() {
    use std::path::Path;
    let w = |p: &str| crate::repos::plain_path(Path::new(p)).map(|x| x.to_string_lossy().into_owned());
    if cfg!(windows) {
        assert_eq!(w(r"\\?\C:\Users\me\repo").as_deref(), Some(r"C:\Users\me\repo"));
        assert_eq!(w(r"C:\Users\me").as_deref(), Some(r"C:\Users\me"));
        assert_eq!(w(r"\\?\UNC\server\share"), None);
        assert_eq!(w(r"\\server\share\x"), None);
        assert_eq!(w(r"\\.\PhysicalDrive0"), None);
        assert_eq!(w(r"\\?\GLOBALROOT\Device"), None);
        assert!(crate::repos::is_within(Path::new(r"c:\USERS\me\repo\x"), Path::new(r"C:\Users\me\repo")));
        assert!(!crate::repos::is_within(Path::new(r"C:\Users\me\repo2"), Path::new(r"C:\Users\me\repo")));
    } else {
        assert_eq!(w("/home/me/repo").as_deref(), Some("/home/me/repo"));
        assert!(crate::repos::is_within(Path::new("/home/me/repo/x"), Path::new("/home/me/repo")));
        assert!(!crate::repos::is_within(Path::new("/home/me/repo2"), Path::new("/home/me/repo")));
        assert!(!crate::repos::is_within(Path::new("/home/me/REPO"), Path::new("/home/me/repo")));
    }
}

/// H1: a remote approval may only choose allow_once; allow_always is chosen on the owner.
#[tokio::test]
async fn remote_approve_never_chooses_allow_always() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    let s = start(&f).await;
    let mut req = permission(&s.id, ToolKind::Execute);
    req.options.insert(1, PermissionOption { option_id: "always".into(), name: "Always".into(), kind: PermissionOptionKind::AllowAlways });
    f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    let a = intent(&f.hub, OTHER, "aa", true, approve(&req.request_id, "always")).await;
    assert_eq!(reason(&a), Some(Reject::Forbidden));
    assert_eq!(resolves(&f, &req.request_id), 0);
    assert_eq!(f.hub.pending_permissions().len(), 1, "still waiting for a human here");
    // allow_once from the same peer works; allow_always stays a local choice.
    let local = ClientMsg::ResolvePermission { confirmed: None, id: "l".into(), request_id: req.request_id.clone(), option_id: Some("always".into()) };
    let r2 = {
        let mut r = permission(&s.id, ToolKind::Execute);
        r.options = req.options.clone();
        r
    };
    f.hub.on_host_event(HostEvent::PermissionRequest(r2.clone())).await;
    assert_eq!(intent(&f.hub, OTHER, "ao", true, approve(&r2.request_id, "allow")).await.status, IntentStatus::Applied);
    ack_result(f.hub.handle(local, &actor()).await);
}

/// H2: a session started or prompted from another machine never gets rule auto-approval (deny
/// rules still deny); its own sessions keep auto-approval; the mark survives a restart.
#[tokio::test]
async fn remote_driven_sessions_always_ask_a_human() {
    let read_rule = rule("read", Some(ToolKind::Read), RuleAction::AutoAllow);
    let f = fixture(Some(vec![read_rule.clone()])).await;
    roster_peer(&f.hub, OTHER).await;
    let cwd = f.dir.path().to_string_lossy().into_owned();
    let read_request = |sid: &str| permission(sid, ToolKind::Read);

    // A local session: rules auto-approve the read.
    let local = start(&f).await;
    let r0 = read_request(&local.id);
    f.hub.on_host_event(HostEvent::PermissionRequest(r0.clone())).await;
    assert_eq!(resolves(&f, &r0.request_id), 1, "local sessions keep auto-approval");

    // Started by another machine (its folder is known now): the same read asks.
    let a = intent(&f.hub, OTHER, "st", true, Intent::Start { agent_id: "codex".into(), cwd: cwd.clone(), prompt: None }).await;
    let remote: Session = serde_json::from_value(a.result.unwrap()["session"].clone()).unwrap();
    let r1 = read_request(&remote.id);
    f.hub.on_host_event(HostEvent::PermissionRequest(r1.clone())).await;
    assert_eq!(resolves(&f, &r1.request_id), 0, "a remote-started session asks");
    assert!(f.hub.pending_permissions().iter().any(|r| r.request_id == r1.request_id));

    // A local session prompted once by another machine asks from then on.
    let prompted = start(&f).await;
    assert_eq!(intent(&f.hub, OTHER, "pr", false, Intent::Prompt { session_id: prompted.id.clone(), text: "go on".into() }).await.status, IntentStatus::Applied);
    let r2 = read_request(&prompted.id);
    f.hub.on_host_event(HostEvent::PermissionRequest(r2.clone())).await;
    assert_eq!(resolves(&f, &r2.request_id), 0, "a remote-prompted session asks");

    // Deny rules still apply to remote-driven sessions.
    ack_result(f.hub.handle(ClientMsg::SetRules { id: "r".into(), rules: vec![rule("no-fetch", Some(ToolKind::Fetch), RuleAction::Deny)] }, &actor()).await);
    let r3 = permission(&remote.id, ToolKind::Fetch);
    f.hub.on_host_event(HostEvent::PermissionRequest(r3.clone())).await;
    assert_eq!(resolves(&f, &r3.request_id), 1, "deny rules still deny");

    // Persisted: a restarted hub still treats them as remote-driven.
    let mut marked = f.hub.store().remote_driven_sessions().unwrap();
    marked.sort();
    let mut want = vec![remote.id.clone(), prompted.id.clone()];
    want.sort();
    assert_eq!(marked, want);
    f.hub.shutdown().await;
    let paths = Paths::in_dir(f.dir.path());
    let store = Store::open(&paths.db, MID).unwrap();
    let fake = FakeHost::default();
    let config = HubConfig { offline: true, rules: Some(vec![read_rule]), ..Default::default() };
    let hub = Hub::build(HubOptions { paths, config, token: "t".repeat(64), machine: ident(), background: false }, store, Host::Fake(fake.clone()))
        .await
        .unwrap();
    assert!(hub.st().remote_driven.contains(&prompted.id));
}

/// L8: a remote start whose first prompt fails still returns the session, with a warning.
#[tokio::test]
async fn a_start_whose_first_prompt_fails_returns_the_session_and_a_warning() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    let cwd = f.dir.path().to_string_lossy().into_owned();
    start(&f).await; // the folder is known
    let a = intent(&f.hub, OTHER, "sw", true, Intent::Start { agent_id: "codex".into(), cwd: cwd.clone(), prompt: Some("fail".into()) }).await;
    assert_eq!(a.status, IntentStatus::Applied, "{a:?}");
    let v = a.result.unwrap();
    assert!(v["session"]["id"].as_str().unwrap().starts_with(MID));
    assert!(v["warning"].as_str().unwrap().contains("first prompt was not sent"), "{v}");
    // The local ack has the same shape.
    let v = ack_result(f.hub.handle(ClientMsg::StartSession { machine_id: None, confirmed: None, id: "l".into(), agent_id: "codex".into(), cwd, prompt: Some("fail".into()) }, &actor()).await);
    assert!(v["warning"].is_string() && v["session"].is_object(), "{v}");
}

/// L9: while the agent is being told one answer, a second answer waits for the real outcome: if
/// the first is refused, the second is applied instead of getting a spurious already_decided.
#[tokio::test]
async fn a_concurrent_answer_waits_for_the_one_in_flight() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    let s = start(&f).await;
    let mut req = permission(&s.id, ToolKind::Execute);
    req.options.push(PermissionOption { option_id: "slow-fail".into(), name: "Flaky".into(), kind: PermissionOptionKind::RejectOnce });
    f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    let me = actor();
    let first = f.hub.resolve_permission(&req.request_id, Some("slow-fail".into()), ResolvedBy::User, &me);
    let second = async {
        tokio::time::sleep(Duration::from_millis(30)).await; // lands while the first is in flight
        intent(&f.hub, OTHER, "late", true, approve(&req.request_id, "allow")).await
    };
    let (first, second) = tokio::join!(first, second);
    assert!(first.is_err(), "the agent refused the first answer");
    assert_eq!(second.status, IntentStatus::Applied, "the waiting answer is applied: {second:?}");
    assert!(f.hub.pending_permissions().is_empty());
    let audit = f.hub.store().audit_log(5).unwrap();
    assert_eq!(audit.iter().map(|a| a.outcome).collect::<Vec<_>>(), vec![AuditOutcome::Allowed, AuditOutcome::Failed]);
}

/// M4: flood-type refusals are logged once per peer per minute (the rest counted); the log is pruned.
#[tokio::test]
async fn flood_refusals_are_logged_once_a_minute() {
    let f = fixture_config(|c| c.peer.accept_intents = false).await;
    roster_peer(&f.hub, OTHER).await;
    let deny = |i: usize| Intent::Deny { request_id: global_id(MID, format!("r{i}")), option_id: None };
    for i in 0..50 {
        assert_eq!(reason(&intent(&f.hub, OTHER, &format!("q{i}"), false, deny(i)).await), Some(Reject::Forbidden));
    }
    let log = f.hub.store().intent_log(100).unwrap();
    assert_eq!(log.len(), 1, "one entry for the burst");
    assert_eq!((log[0].reason.as_deref(), log[0].repeats), (Some("forbidden"), 0));
}

/// M5: the origin only accepts a start result that is a session of the target machine.
#[test]
fn the_origin_checks_owner_results() {
    use intents::checked_result;
    let sess = |mid: &str, id: &str| {
        let mut s = remote_session(mid, "x");
        s.id = id.into();
        json!({ "session": s })
    };
    assert_eq!(checked_result("approve", OTHER, Some(json!({"anything": "else"}))), Ok(json!({})), "only {{}} for non-starts");
    assert_eq!(checked_result("prompt", OTHER, None), Ok(json!({})));
    let ok = checked_result("start", OTHER, Some(sess(OTHER, &global_id(OTHER, "s")))).unwrap();
    assert_eq!(ok["session"]["machineId"], OTHER);
    assert_eq!(checked_result("start", OTHER, Some(sess(THIRD, &global_id(THIRD, "s")))), Err(AckCode::Invalid), "a third machine's session");
    assert_eq!(checked_result("start", OTHER, Some(sess(OTHER, &global_id(MID, "s")))), Err(AckCode::Invalid), "our id under their machine");
    assert_eq!(checked_result("start", OTHER, Some(json!({"session": "nope"}))), Err(AckCode::Invalid));
    assert_eq!(checked_result("start", OTHER, None), Err(AckCode::Invalid));
    let mut big = sess(OTHER, &global_id(OTHER, "s"));
    big["junk"] = json!("x".repeat(70 * 1024));
    assert_eq!(checked_result("start", OTHER, Some(big)), Err(AckCode::Invalid), "oversize");
    let mut warned = sess(OTHER, &global_id(OTHER, "s"));
    warned["warning"] = json!(format!("bad\u{1b}[2J{}", "w".repeat(500)));
    let w = checked_result("start", OTHER, Some(warned)).unwrap()["warning"].as_str().unwrap().to_string();
    assert!(!w.contains('\u{1b}') && w.chars().count() == 300);
}

#[tokio::test]
async fn accept_intents_false_forbids_every_intent() {
    let f = fixture_config(|c| c.peer.accept_intents = false).await;
    roster_peer(&f.hub, OTHER).await;
    let req = pending_request(&f).await;
    for (i, body) in [approve(&req.request_id, "allow"), Intent::Cancel { session_id: req.session_id.clone() }].into_iter().enumerate() {
        assert_eq!(reason(&intent(&f.hub, OTHER, &format!("f{i}"), true, body).await), Some(Reject::Forbidden));
    }
    assert_eq!(f.hub.pending_permissions().len(), 1);
    assert!(!f.fake.calls().iter().any(|c| c.starts_with("resolve") || c.starts_with("cancel")));
    assert!(HubConfig::parse("").unwrap().peer.accept_intents, "default: accept");
}

#[tokio::test]
async fn intents_are_rate_limited_per_peer() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    roster_peer(&f.hub, THIRD).await;
    let deny = |i: usize| Intent::Deny { request_id: global_id(MID, format!("r{i}")), option_id: None };
    for i in 0..pawl_protocol::INTENT_RATE_PER_SEC as usize {
        assert_eq!(reason(&intent(&f.hub, OTHER, &format!("q{i}"), false, deny(i)).await), Some(Reject::NotFound));
    }
    assert_eq!(reason(&intent(&f.hub, OTHER, "over", false, deny(99)).await), Some(Reject::RateLimited));
    assert_eq!(reason(&intent(&f.hub, THIRD, "other-peer", false, deny(98)).await), Some(Reject::NotFound), "per peer");
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(reason(&intent(&f.hub, OTHER, "over", false, deny(99)).await), Some(Reject::NotFound), "a rate-limited id may be retried");
}

/// A repeated intent id gets the same ack and changes nothing.
#[tokio::test]
async fn a_repeated_intent_id_returns_the_same_ack() {
    let mut f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    let req = pending_request(&f).await;
    let first = intent(&f.hub, OTHER, "dup", true, approve(&req.request_id, "allow")).await;
    drain(&mut f.rx);
    let again = intent(&f.hub, OTHER, "dup", true, approve(&req.request_id, "allow")).await;
    assert_eq!(again, first);
    let other_body = intent(&f.hub, OTHER, "dup", false, Intent::Deny { request_id: req.request_id.clone(), option_id: None }).await;
    assert_eq!(other_body, first, "keyed by intent id");
    assert_eq!(resolves(&f, &req.request_id), 1);
    assert!(drain(&mut f.rx).is_empty(), "nothing was re-applied or re-announced");
    // Another peer's intent with the same id is a different intent.
    roster_peer(&f.hub, THIRD).await;
    assert_eq!(reason(&intent(&f.hub, THIRD, "dup", true, approve(&req.request_id, "allow")).await), Some(Reject::AlreadyDecided));
}

/// The origin checks confirmation before sending and answers offline / not_found itself.
#[tokio::test]
async fn the_origin_checks_before_sending() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await; // paired, not linked
    let sid = global_id(OTHER, "s1");
    feed(&f.hub, OTHER, MirrorUpdate::Session { session: remote_session(OTHER, "s1") }).await;
    let mut req = remote_request(OTHER, "r1", &sid);
    req.options = permission(&sid, ToolKind::Execute).options;
    feed(&f.hub, OTHER, MirrorUpdate::PermissionRequest { request: req.clone() }).await;
    let resolve = |id: &str, option: Option<&str>, confirmed: Option<bool>| ClientMsg::ResolvePermission {
        id: id.into(),
        request_id: req.request_id.clone(),
        option_id: option.map(Into::into),
        confirmed,
    };
    let start_on = |m: &str, confirmed: Option<bool>| ClientMsg::StartSession {
        id: "st".into(),
        agent_id: "codex".into(),
        cwd: "/srv/api".into(),
        prompt: None,
        machine_id: Some(m.into()),
        confirmed,
    };
    for (msg, want) in [
        (resolve("a", Some("allow"), None), "unconfirmed"),
        (resolve("b", Some("allow"), Some(false)), "unconfirmed"),
        (start_on(OTHER, None), "unconfirmed"),
        (resolve("c", Some("bogus"), Some(true)), "invalid"),
    ] {
        assert_eq!(ack_error(f.hub.handle(msg.clone(), &actor()).await), want, "{msg:?}");
    }
    assert!(f.hub.store().intent_log(10).unwrap().is_empty(), "nothing was sent");
    for (msg, want) in [
        (resolve("d", Some("allow"), Some(true)), "offline"),
        (resolve("e", Some("reject"), None), "offline"), // deny: no confirmation needed
        (start_on(OTHER, Some(true)), "offline"),
        (start_on(THIRD, Some(true)), "not_found"),
        (ClientMsg::Prompt { id: "p".into(), session_id: global_id(THIRD, "s"), text: "hi".into() }, "not_found"),
    ] {
        assert_eq!(ack_error(f.hub.handle(msg.clone(), &actor()).await), want, "{msg:?}");
    }
    let log = f.hub.store().intent_log(10).unwrap();
    assert_eq!(log.len(), 3, "the offline ones were logged");
    assert!(log.iter().all(|e| e.role == IntentRole::Origin && e.status == "offline" && e.peer_machine_id == OTHER));
    assert!(log.iter().any(|e| e.kind == "approve") && log.iter().any(|e| e.kind == "deny") && log.iter().any(|e| e.kind == "start"));
    // list_agents for a paired machine (no presence yet: none known), for an unknown one: not_found.
    let v = ack_result(f.hub.handle(ClientMsg::ListAgents { id: "la".into(), refresh: None, machine_id: Some(OTHER.into()) }, &actor()).await);
    assert_eq!(v["agents"], json!([]));
    let err = ack_error(f.hub.handle(ClientMsg::ListAgents { id: "lb".into(), refresh: None, machine_id: Some(THIRD.into()) }, &actor()).await);
    assert_eq!(err, "not_found");
}

/// End to end over real peer links: B approves A's request (audited on both sides), a second
/// approval is already_decided, B starts a session on A, B lists A's agents, and A not answering
/// gives B `timeout`.
#[tokio::test]
async fn two_hubs_approve_start_and_time_out() {
    let da = tempfile::tempdir().unwrap();
    let (a, a_fake) = peer_hub_with_fake(da.path(), MID, "Desk", Some("127.0.0.1:0")).await;
    let (b, _db) = peer_hub(OTHER, "Laptop", None).await;
    pair(&a, &b).await;
    let cwd = da.path().to_string_lossy().into_owned();
    let v = ack_result(a.handle(ClientMsg::StartSession { machine_id: None, confirmed: None, id: "s".into(), agent_id: "codex".into(), cwd: cwd.clone(), prompt: None }, &actor()).await);
    let s: Session = serde_json::from_value(v["session"].clone()).unwrap();
    let req = permission(&s.id, ToolKind::Execute);
    a.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    wait_until("b mirrors the request", || {
        let (b, rid) = (b.clone(), req.request_id.clone());
        async move { b.welcome().pending_permissions.iter().any(|r| r.request_id == rid) }
    })
    .await;
    let mut brx = b.subscribe();

    // Approve from B (confirmed on B's client).
    let approve_msg = |id: &str| ClientMsg::ResolvePermission {
        id: id.into(),
        request_id: req.request_id.clone(),
        option_id: Some("allow".into()),
        confirmed: Some(true),
    };
    assert_eq!(ack_result(b.handle(approve_msg("ok"), &actor()).await), json!({}));
    assert!(a_fake.calls().contains(&format!("resolve {} allow", req.request_id)));
    let owner_audit = &a.store().audit_log(5).unwrap()[0];
    assert_eq!(owner_audit.origin.as_ref().map(|o| (o.machine_id.as_str(), o.name.as_deref(), o.client.as_str())), Some((OTHER, Some("Laptop"), "desktop")));
    let origin_log = &b.store().intent_log(5).unwrap()[0];
    assert_eq!((origin_log.role, origin_log.kind.as_str(), origin_log.peer_machine_id.as_str(), origin_log.status.as_str()), (IntentRole::Origin, "approve", MID, "applied"));
    assert_eq!(origin_log.intent_id, owner_audit.origin.as_ref().unwrap().intent_id, "both sides log the same intent");
    wait_for(&mut brx, "b sees who decided", |m| matches!(m, ServerMsg::PermissionResolved { request_id, by_machine_id: Some(by), .. } if *request_id == req.request_id && by == OTHER)).await;

    // A second approval, from B or on A itself, is already_decided.
    assert_eq!(ack_error(b.handle(approve_msg("again"), &actor()).await), "already_decided");
    assert_eq!(
        ack_error(a.handle(ClientMsg::ResolvePermission { confirmed: None, id: "a".into(), request_id: req.request_id.clone(), option_id: Some("allow".into()) }, &actor()).await),
        "already_decided"
    );
    assert_eq!(a_fake.calls().iter().filter(|c| c.starts_with("resolve")).count(), 1);

    // B starts a session on A: it runs on A and B's mirror gets it.
    let v = ack_result(b.handle(ClientMsg::StartSession { id: "rs".into(), agent_id: "codex".into(), cwd: cwd.clone(), prompt: None, machine_id: Some(MID.into()), confirmed: Some(true) }, &actor()).await);
    let remote: Session = serde_json::from_value(v["session"].clone()).unwrap();
    assert_eq!(remote.machine_id, MID);
    assert_eq!(a_fake.calls().iter().filter(|c| c.starts_with("start codex")).count(), 2);
    wait_until("b mirrors the remote start", || {
        let (b, id) = (b.clone(), remote.id.clone());
        async move { b.welcome().sessions.iter().any(|s| s.id == id) }
    })
    .await;
    // B lists A's agents (from A's presence).
    let v = ack_result(b.handle(ClientMsg::ListAgents { id: "la".into(), refresh: None, machine_id: Some(MID.into()) }, &actor()).await);
    assert_eq!(v["agents"][0]["id"], "codex");

    // A stops answering: B's client gets timeout.
    a.drop_intents(true);
    let started = std::time::Instant::now();
    let err = ack_error(b.handle(ClientMsg::Prompt { id: "p".into(), session_id: remote.id.clone(), text: "hi".into() }, &actor()).await);
    assert_eq!(err, "timeout");
    assert!(started.elapsed() >= INTENT_TIMEOUT);
    assert_eq!(b.store().intent_log(1).unwrap()[0].status, "timeout");
}

// ---------------------------------------------------------------- repos + remote commands (step 5)

use pawl_peer::PeerEvent;
use pawl_protocol::{ChatProposalKind, Presence, RepoRef};

fn repo(name: &str, path: &str) -> RepoRef {
    RepoRef { name: name.into(), path: path.into(), last_used_at: Some(1) }
}

/// A presence from `peer` with these agents and repos.
fn presence_of(peer: &str, agents: Vec<Agent>, repos: Vec<RepoRef>) -> Presence {
    let now = pawl_protocol::now_ms();
    Presence {
        machine: Machine {
            id: peer.into(),
            name: "Mac mini".into(),
            os: pawl_protocol::MachineOs::Macos,
            last_seen: now,
            online: true,
            hub_version: None,
            user_active_at: None,
        },
        agents,
        repos,
        pets: vec![],
        user_active_at: None,
        ts: now,
    }
}

async fn list_repos(hub: &Hub, machine: Option<&str>) -> std::result::Result<Vec<RepoRef>, String> {
    match hub.handle(ClientMsg::ListRepos { id: "lr".into(), machine_id: machine.map(Into::into) }, &actor()).await {
        ServerMsg::Ack { ok: true, result, .. } => Ok(serde_json::from_value(result.unwrap()["repos"].clone()).unwrap()),
        other => Err(ack_error(other)),
    }
}

/// The index comes from this machine's session folders (canonical, newest first) plus config
/// `[repos] paths`, is rebuilt when a session uses a new folder, and drops folders that are gone.
#[tokio::test]
async fn the_repo_index_follows_sessions_and_config() {
    let extra = tempfile::tempdir().unwrap();
    let docs = extra.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let docs_cfg = docs.to_string_lossy().into_owned();
    let f = fixture_config(move |c| c.repos.paths = vec![docs_cfg, "/definitely/not/here".into()]).await;
    let names = |h: &Hub| h.repos().into_iter().map(|r| r.name).collect::<Vec<_>>();
    assert_eq!(names(&f.hub), vec!["docs"], "config paths that exist; missing ones dropped");

    let api = f.dir.path().join("work").join("api");
    std::fs::create_dir_all(&api).unwrap();
    let api_cwd = api.join("..").join("api").to_string_lossy().into_owned(); // not canonical on purpose
    ack_result(f.hub.handle(ClientMsg::StartSession { machine_id: None, confirmed: None, id: "s".into(), agent_id: "codex".into(), cwd: api_cwd, prompt: None }, &actor()).await);
    wait_until("the new folder is indexed", || {
        let h = f.hub.clone();
        async move { h.repos().iter().any(|r| r.name == "api") }
    })
    .await;
    let idx = f.hub.repos();
    assert_eq!(idx[0].name, "api", "newest first");
    assert_eq!(idx[0].path, crate::repos::canonical_dir(&api).unwrap().to_string_lossy(), "canonical path");
    assert!(idx[0].last_used_at.is_some());
    assert_eq!(list_repos(&f.hub, None).await.unwrap(), idx);
    assert_eq!(list_repos(&f.hub, Some(MID)).await.unwrap(), idx, "this machine by id");

    // A folder that disappears is dropped at the next rebuild.
    std::fs::remove_dir_all(&docs).unwrap();
    f.hub.rebuild_repo_index().await;
    assert_eq!(names(&f.hub), vec!["api"]);
}

/// A paired hub's repos arrive in its presence, are validated, kept for when it is offline, and
/// served by list_repos; unknown machines are not_found.
#[tokio::test]
async fn peer_repos_are_validated_stored_and_listed() {
    let f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    assert_eq!(list_repos(&f.hub, Some(OTHER)).await.unwrap(), vec![], "nothing reported yet");
    let mut repos = vec![repo("api", "/srv/api"), repo("web", "/srv/web"), repo("bad\u{7}", "/srv/x"), repo("", "/srv/y"), repo("z", &"/".repeat(2000))];
    repos.extend((0..300).map(|i| repo(&format!("r{i}"), &format!("/r/{i}"))));
    let codex = Agent { machine_id: OTHER.into(), ..agent("codex") };
    let bad = Agent { id: "../evil".into(), ..codex.clone() };
    let noisy = Agent { id: "noisy".into(), name: "Noisy\u{1b}[2J".into(), ..codex.clone() };
    f.hub.on_peer_event(PeerEvent::Presence { device_id: OTHER.into(), presence: presence_of(OTHER, vec![codex.clone(), bad, noisy], repos) }).await;
    let v = ack_result(f.hub.handle(ClientMsg::ListAgents { id: "la".into(), refresh: None, machine_id: Some(OTHER.into()) }, &actor()).await);
    assert_eq!(v["agents"].as_array().unwrap().len(), 1, "invalid agents dropped: {v}");
    assert_eq!(f.hub.store().device_agents(OTHER).unwrap().unwrap().len(), 1, "and never persisted");
    let got = list_repos(&f.hub, Some(OTHER)).await.unwrap();
    assert_eq!(got.len(), pawl_protocol::REPO_INDEX_MAX);
    assert_eq!((got[0].name.as_str(), got[1].name.as_str(), got[2].name.as_str()), ("api", "web", "r0"), "invalid entries dropped");
    assert_eq!(f.hub.store().device_repos(OTHER).unwrap().unwrap(), got, "persisted for later");
    assert_eq!(list_repos(&f.hub, Some(THIRD)).await.unwrap_err(), "not_found");

    // The brain sees every machine's agents, repos and pets.
    feed(&f.hub, OTHER, MirrorUpdate::Session { session: remote_session(OTHER, "s1") }).await;
    feed(&f.hub, OTHER, MirrorUpdate::Pet { pet: remote_pet(OTHER, "codex", "Biscuit") }).await;
    let ctx = f.hub.brain_context(None).await;
    assert_eq!(ctx.remote_agents.get(OTHER).map(|a| a[0].id.as_str()), Some("codex"));
    assert_eq!(ctx.repos.get(OTHER).map(|r| r.len()), Some(pawl_protocol::REPO_INDEX_MAX));
    assert!(ctx.repos.contains_key(MID), "this machine's index too");
    assert!(ctx.pets.iter().any(|p| p.name == "Biscuit" && p.machine_id == OTHER));
}

/// The step-4 known-folder rule is the repo index (+ start_roots): a `[repos] paths` folder allows
/// a remote start without any session there; a folder no longer in the index does not.
#[tokio::test]
async fn remote_start_uses_the_repo_index() {
    let extra = tempfile::tempdir().unwrap();
    let listed = extra.path().join("listed");
    let unlisted = extra.path().join("unlisted");
    std::fs::create_dir_all(&listed).unwrap();
    std::fs::create_dir_all(&unlisted).unwrap();
    let cfg = listed.to_string_lossy().into_owned();
    let f = fixture_config(move |c| c.repos.paths = vec![cfg]).await;
    roster_peer(&f.hub, OTHER).await;
    let start = |p: &std::path::Path| Intent::Start { agent_id: "codex".into(), cwd: p.to_string_lossy().into(), prompt: None };
    assert_eq!(intent(&f.hub, OTHER, "a", true, start(&listed)).await.status, IntentStatus::Applied, "in the index");
    assert_eq!(reason(&intent(&f.hub, OTHER, "b", true, start(&unlisted)).await), Some(Reject::Forbidden), "not in it");
}

/// A reply's proposals are stored with the message and broadcast; nothing is started or sent by
/// the hub for them (the user confirms one in a client). At most five are kept.
#[tokio::test]
async fn chat_proposals_are_stored_and_broadcast() {
    let mut f = fixture(None).await;
    roster_peer(&f.hub, OTHER).await;
    drain(&mut f.rx);
    let proposal = |i: usize| pawl_protocol::ChatProposal {
        id: format!("p{i}"),
        kind: ChatProposalKind::Start,
        machine_id: OTHER.into(),
        agent_id: "codex".into(),
        cwd: Some("/srv/api".into()),
        repo_name: Some("api".into()),
        session_id: None,
        text: "run the tests".into(),
        summary: format!("Start Codex on Mac mini in api: run the tests ({i})"),
    };
    let reply = BrainReply { text: "Which one?".into(), action: None, proposals: (0..7).map(proposal).collect() };
    let msg = f.hub.deliver_reply(None, reply, &actor()).await;
    assert_eq!(msg.proposals.len(), 5, "capped");
    let got = match wait_for(&mut f.rx, "the pet's reply", |m| matches!(m, ServerMsg::Chat { message } if message.role == ChatRole::Pet)).await {
        ServerMsg::Chat { message } => message,
        _ => unreachable!(),
    };
    assert_eq!(got, msg, "broadcast with its proposals");
    assert_eq!(f.hub.store().recent_chat(5).unwrap().last().unwrap(), &msg, "stored with its proposals");
    assert!(!f.fake.calls().iter().any(|c| c.starts_with("start")), "nothing was started");
    assert!(f.hub.store().intent_log(5).unwrap().is_empty(), "nothing was sent to the other machine");
}

/// End to end with the real brain over real peer links: B's pet answers "tell Desk's codex to run
/// the tests in repo api" with one proposal for A, and runs nothing. A's codex has an open idle
/// session there, so it is a prompt to that session; once it ended, it is a start in A's folder.
#[tokio::test]
async fn a_chat_on_one_hub_proposes_a_command_on_the_other() {
    let da = tempfile::tempdir().unwrap();
    let (a, a_fake) = peer_hub_with_fake(da.path(), MID, "Desk", Some("127.0.0.1:0")).await;
    let (b, _db) = peer_hub(OTHER, "Laptop", None).await;
    let api = da.path().join("api");
    std::fs::create_dir_all(&api).unwrap();
    let v = ack_result(a.handle(ClientMsg::StartSession { machine_id: None, confirmed: None, id: "s".into(), agent_id: "codex".into(), cwd: api.to_string_lossy().into(), prompt: None }, &actor()).await);
    let open: Session = serde_json::from_value(v["session"].clone()).unwrap();
    pair(&a, &b).await;
    wait_until("b knows a's repos and session", || {
        let (b, sid) = (b.clone(), open.id.clone());
        async move {
            list_repos(&b, Some(MID)).await.is_ok_and(|r| r.iter().any(|x| x.name == "api"))
                && b.welcome().sessions.iter().any(|s| s.id == sid)
        }
    })
    .await;
    let ask = |b: Hub| async move {
        let mut brx = b.subscribe();
        ack_result(b.handle(ClientMsg::Chat { id: "c".into(), pet_id: None, text: "tell Desk's codex to run the tests in repo api".into() }, &actor()).await);
        match wait_for(&mut brx, "the pet's reply", |m| matches!(m, ServerMsg::Chat { message } if message.role == ChatRole::Pet)).await {
            ServerMsg::Chat { message } => message,
            _ => unreachable!(),
        }
    };
    let reply = ask(b.clone()).await;
    assert_eq!(reply.proposals.len(), 1, "{reply:?}");
    let p = &reply.proposals[0];
    assert_eq!((p.kind, p.machine_id.as_str(), p.agent_id.as_str()), (ChatProposalKind::Prompt, MID, "codex"));
    assert_eq!((p.session_id.as_deref(), p.text.as_str()), (Some(open.id.as_str()), "run the tests"));

    // The session ends: the same sentence proposes a start in A's indexed folder.
    ack_result(a.handle(ClientMsg::Cancel { id: "x".into(), session_id: open.id.clone() }, &actor()).await);
    a.close_session(&open.id).await.unwrap();
    wait_until("b sees it ended", || {
        let (b, sid) = (b.clone(), open.id.clone());
        async move { b.welcome().sessions.iter().any(|s| s.id == sid && s.ended_at.is_some()) }
    })
    .await;
    let reply = ask(b.clone()).await;
    assert_eq!(reply.proposals.len(), 1, "{reply:?}");
    let p = &reply.proposals[0];
    let api_path = crate::repos::canonical_dir(&api).unwrap().to_string_lossy().into_owned();
    assert_eq!((p.kind, p.machine_id.as_str(), p.cwd.as_deref()), (ChatProposalKind::Start, MID, Some(api_path.as_str())));
    assert_eq!(a_fake.calls().iter().filter(|c| c.starts_with("start codex")).count(), 1, "only A's own start: the chats ran nothing");
    assert!(!a_fake.calls().iter().any(|c| c.starts_with("prompt")), "nor prompted anything");
}

/// The chat with no pet attached is the default pet: its messages carry no pet id, and a command that
/// names no agent goes to the `bot.defaultAgent` agent (Claude Code here, though Codex had the last
/// session). It never runs by itself: the reply is a proposal for this machine, and the client's
/// confirmed start_session starts it.
#[tokio::test]
async fn the_default_pet_hands_agentless_work_to_the_default_agent() {
    let repo_dir = tempfile::tempdir().unwrap();
    let api = repo_dir.path().join("api");
    std::fs::create_dir_all(&api).unwrap();
    let api_path = api.to_string_lossy().into_owned();
    let mut f = fixture_config(|c| {
        c.brain.default_agent = Some("claude-code".into());
        c.repos.paths = vec![api_path];
    })
    .await;
    f.hub.rebuild_repo_index().await;
    start(&f).await;
    drain(&mut f.rx);
    let v = ack_result(
        f.hub.handle(ClientMsg::Chat { id: "c".into(), pet_id: None, text: "fix the failing test in repo api".into() }, &actor()).await,
    );
    let user: ChatMessage = serde_json::from_value(v["message"].clone()).unwrap();
    assert_eq!(user.pet_id, None);
    let reply = match wait_for(&mut f.rx, "the default pet's reply", |m| matches!(m, ServerMsg::Chat { message } if message.role == ChatRole::Pet)).await {
        ServerMsg::Chat { message } => message,
        _ => unreachable!(),
    };
    assert_eq!(reply.pet_id, None, "{reply:?}");
    assert!(reply.text.contains("I picked Claude Code (your default agent)"), "{}", reply.text);
    assert_eq!((reply.session_id.as_ref(), reply.proposals.len()), (None, 1), "{reply:?}");
    let p = &reply.proposals[0];
    assert_eq!((p.kind, p.machine_id.as_str(), p.agent_id.as_str()), (ChatProposalKind::Start, MID, "claude-code"));
    assert_eq!(f.hub.store().recent_chat(1).unwrap().last().unwrap().proposals, reply.proposals, "stored with the reply");
    assert!(!f.fake.calls().iter().any(|c| c.starts_with("start claude-code")), "nothing ran: {:?}", f.fake.calls());
    // The user confirms: the client sends the ordinary start_session for this machine.
    let (cwd, text) = (p.cwd.clone().unwrap(), p.text.clone());
    ack_result(
        f.hub
            .handle(
                ClientMsg::StartSession { id: "ok".into(), agent_id: p.agent_id.clone(), cwd, prompt: Some(text), machine_id: Some(MID.into()), confirmed: Some(true) },
                &actor(),
            )
            .await,
    );
    let calls = f.fake.calls();
    assert!(calls.iter().any(|c| c == "start claude-code "), "{calls:?}");
    assert!(calls.last().is_some_and(|c| c.starts_with("prompt ") && c.ends_with(" fix the failing test")), "{calls:?}");
}

/// Hook sessions: a prompt that is only injected text (a task notification, an image placeholder, a
/// message from another terminal) never becomes the title; until a real prompt arrives the title is
/// "<agent> in <folder>"; a later real prompt replaces it and the update is broadcast.
#[tokio::test]
async fn hook_session_titles_skip_injected_text() {
    let mut f = fixture(None).await;
    let title = |f: &Fixture| f.hub.st().sessions.values().find(|s| s.external_id.as_deref() == Some("ext-t")).and_then(|s| s.title.clone());
    let prompt = |text: &str| json!({ "hook_event_name": "UserPromptSubmit", "session_id": "ext-t", "cwd": "/w/tiny-whisper", "prompt": text });
    for injected in [
        "<task-notification> <task-id>bt7xilh3z</task-id> <status>completed</status> </task-notification>",
        "[Image #40]",
        "<hc from=\"t13\">t13: accepted, thanks.</hc>",
    ] {
        f.hub.ingest_hook(HookSource::Claude, prompt(injected)).await.unwrap();
        assert_eq!(title(&f).as_deref(), Some("Claude Code in tiny-whisper"), "{injected}");
    }
    let paste = "<pasted_content id=\"66d5\">\n<hc from=\"t13\">do the task</hc>\n</pasted_content id=\"66d5\">";
    f.hub.ingest_hook(HookSource::Claude, prompt(paste)).await.unwrap();
    assert_eq!(title(&f).as_deref(), Some("Pasted text"));
    drain(&mut f.rx);
    f.hub.ingest_hook(HookSource::Claude, prompt("[Image #2] fix the waveform view")).await.unwrap();
    assert_eq!(title(&f).as_deref(), Some("fix the waveform view"));
    let broadcast = drain(&mut f.rx).into_iter().any(|m| matches!(m, ServerMsg::Session { session } if session.title.as_deref() == Some("fix the waveform view")));
    assert!(broadcast, "the new title is broadcast");
    // Injected-only prompts and tool hooks keep it.
    f.hub.ingest_hook(HookSource::Claude, prompt("<system-reminder>be terse</system-reminder>")).await.unwrap();
    f.hub
        .ingest_hook(HookSource::Claude, json!({ "hook_event_name": "PreToolUse", "session_id": "ext-t", "tool_name": "Bash", "tool_input": { "command": "ls" } }))
        .await
        .unwrap();
    assert_eq!(title(&f).as_deref(), Some("fix the waveform view"));
    // The full prompt is used when the hook's clipped title is all injected text.
    let long = format!("<task-notification>{}</task-notification> then run the tests", "x".repeat(300));
    f.hub.ingest_hook(HookSource::Claude, prompt(&long)).await.unwrap();
    assert_eq!(title(&f).as_deref(), Some("then run the tests"));
}

/// Owned sessions: the agent's raw title is cleaned the same way; a placeholder title takes the next
/// real prompt (broadcast), and a real title is not replaced by injected text.
#[tokio::test]
async fn owned_session_titles_skip_injected_text() {
    let mut f = fixture(None).await;
    let s = start(&f).await;
    let folder = f.dir.path().file_name().unwrap().to_string_lossy().into_owned();
    let fallback = format!("Codex in {folder}");
    let title = |f: &Fixture| f.hub.session(&s.id).and_then(|s| s.title);
    assert_eq!(title(&f), Some(fallback.clone()), "no real prompt seen yet");
    let mut raw = f.hub.session(&s.id).unwrap();
    raw.title = Some("[Image #1]".into());
    f.hub.on_host_event(HostEvent::Session(raw.clone())).await;
    assert_eq!(title(&f), Some(fallback.clone()));
    let said = |text: &str| HostEvent::Event { session_id: s.id.clone(), kind: EventType::UserMessage, payload: json!({ "text": text }) };
    f.hub.on_host_event(said("<task-notification><task-id>b1</task-id></task-notification>")).await;
    assert_eq!(title(&f), Some(fallback.clone()));
    drain(&mut f.rx);
    f.hub.on_host_event(said("fix the build")).await;
    assert_eq!(title(&f).as_deref(), Some("fix the build"));
    assert!(drain(&mut f.rx).into_iter().any(|m| matches!(m, ServerMsg::Session { session } if session.title.as_deref() == Some("fix the build"))));
    // The host still reports its raw first-prompt title: the real one stays.
    f.hub.on_host_event(HostEvent::Session(raw.clone())).await;
    f.hub.on_host_event(said("and lint")).await;
    assert_eq!(title(&f).as_deref(), Some("fix the build"));
    // A real title from the agent is followed.
    raw.title = Some("Refactor the parser".into());
    f.hub.on_host_event(HostEvent::Session(raw)).await;
    assert_eq!(title(&f).as_deref(), Some("Refactor the parser"));
}

/// At start, stored titles that hold injected text are cleaned once (recent or not).
#[tokio::test]
async fn stored_injected_titles_are_cleaned_at_start() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    let store = Store::open(&paths.db, MID).unwrap();
    store.replace_agents(&[agent("codex"), agent("claude-code")]).unwrap();
    let old = |id: &str, title: &str| Session {
        id: global_id(MID, id),
        machine_id: MID.into(),
        agent_id: "claude-code".into(),
        acp_session_id: None,
        external_id: Some(id.into()),
        cwd: "/w/tiny-whisper".into(),
        title: Some(title.into()),
        source: SessionSource::CliHook,
        status: SessionStatus::Done,
        started_at: 1_000,
        last_event_at: 2_000,
        ended_at: Some(2_000),
    };
    store.upsert_session(&old("a", "<task-notification> <task-id>bt7…")).unwrap();
    store.upsert_session(&old("b", "[Image #40] fix the waveform view")).unwrap();
    store.upsert_session(&old("c", "<\\pasted_content id=x>secret<\\/pasted_content>")).unwrap();
    store.upsert_session(&old("d", "Fix login")).unwrap();
    let hub = Hub::build(
        HubOptions { paths, config: HubConfig { offline: true, ..Default::default() }, token: "t".repeat(64), machine: ident(), background: false },
        store,
        Host::Fake(FakeHost::default()),
    )
    .await
    .unwrap();
    let stored = |id: &str| hub.store().session(&global_id(MID, id)).unwrap().unwrap().title;
    assert_eq!(stored("a").as_deref(), Some("Claude Code in tiny-whisper"));
    assert_eq!(stored("b").as_deref(), Some("fix the waveform view"));
    assert_eq!(stored("c").as_deref(), Some("Pasted text"));
    assert_eq!(stored("d").as_deref(), Some("Fix login"), "clean titles are left alone");
}

mod phones;

// ---------- the "acp" bot provider: a hidden, tool-less agent session ----------

/// The brain session's id once the fake host saw its prompt.
async fn brain_prompt(f: &Fixture) -> String {
    wait_until("the brain session's prompt", || {
        let fake = f.fake.clone();
        async move { fake.calls().iter().any(|c| c.starts_with("prompt ") && c.contains(":brain-")) }
    })
    .await;
    let calls = f.fake.calls();
    let line = calls.iter().rev().find(|c| c.starts_with("prompt ") && c.contains(":brain-")).unwrap();
    line.split_whitespace().nth(1).unwrap().to_string()
}

fn chunk(id: &str, text: &str) -> HostEvent {
    HostEvent::Event { session_id: id.into(), kind: EventType::AgentMessageChunk, payload: json!({ "text": text }) }
}

fn stop(id: &str) -> HostEvent {
    HostEvent::Event { session_id: id.into(), kind: EventType::Stop, payload: json!({ "stopReason": "end_turn" }) }
}

#[tokio::test]
async fn the_acp_brain_session_is_locked_down_and_never_a_session() {
    let mut f = fixture(None).await;
    let model = f.hub.acp_model().expect("claude-code is installed: the acp provider exists");
    assert_eq!(model.label(), "claude-code:sonnet");
    drain(&mut f.rx);
    let m = model.clone();
    let turn = tokio::spawn(async move { m.complete("You are a pet.", "hi", Duration::from_secs(10)).await });
    let id = brain_prompt(&f).await;
    // Started on the user's own agent, in the locked folder, with the model and no thinking set by env.
    let started = f.fake.started.lock().unwrap().last().cloned().unwrap();
    let env = started.env.clone().unwrap_or_default();
    assert_eq!((started.id.as_str(), env.get("ANTHROPIC_MODEL").map(String::as_str), env.get("MAX_THINKING_TOKENS").map(String::as_str)), ("claude-code", Some("sonnet"), Some("0")));
    let settings: Value = serde_json::from_str(&std::fs::read_to_string(f.hub.paths().data_dir.join("brain-session/.claude/settings.json")).unwrap()).unwrap();
    assert!(settings["permissions"]["deny"].as_array().unwrap().iter().any(|d| d == "*"));
    assert_eq!(settings["disableAllHooks"], true);
    // Started with no tools at all (SDK options in session/new _meta) and a clean environment.
    let opts = f.fake.options.lock().unwrap().last().cloned().unwrap();
    assert!(opts.clean_env);
    let o = opts.meta.as_ref().unwrap()["claudeCode"]["options"].clone();
    assert_eq!((o["tools"].clone(), o["strictMcpConfig"].clone(), o["mcpServers"].clone()), (json!([]), json!(true), json!({})));
    assert_eq!(env.get("OPENAI_API_KEY").map(String::as_str), Some(""), "other credentials blanked too");
    // A permission request is rejected, never shown, and ends the turn: its answer is not used.
    let opt = |id: &str, kind| pawl_protocol::PermissionOption { option_id: id.into(), name: id.into(), kind };
    let req = PermissionRequest {
        request_id: global_id(MID, "brain-req"),
        machine_id: MID.into(),
        session_id: id.clone(),
        agent_id: "claude-code".into(),
        tool_call: ToolCallPayload { tool_call_id: "t".into(), title: Some("Bash: rm -rf ~".into()), kind: Some(ToolKind::Execute), ..Default::default() },
        options: vec![opt("allow-once", PermissionOptionKind::AllowOnce), opt("reject-once", PermissionOptionKind::RejectOnce)],
        created_at: 1,
    };
    f.hub.on_host_event(chunk(&id, "Sure, let me just")).await;
    f.hub.on_host_event(HostEvent::PermissionRequest(req)).await;
    assert!(f.fake.calls().iter().any(|c| c == &format!("resolve {} reject-once", global_id(MID, "brain-req"))), "{:?}", f.fake.calls());
    assert!(turn.await.unwrap().is_err(), "the answer of a turn that asked for a tool is discarded");
    assert!(f.fake.calls().iter().any(|c| c == &format!("close {id}")), "and the session is restarted");
    // The next turn runs in a fresh session and answers.
    let first = id;
    let m = model.clone();
    let turn = tokio::spawn(async move { m.complete("You are a pet.", "hi again", Duration::from_secs(10)).await });
    wait_until("the second prompt", || {
        let fake = f.fake.clone();
        async move { fake.calls().iter().filter(|c| c.starts_with("prompt ") && c.contains(":brain-")).count() == 2 }
    })
    .await;
    let id = brain_prompt(&f).await;
    assert_ne!(id, first);
    f.hub.on_host_event(HostEvent::Session(Session { id: id.clone(), machine_id: MID.into(), agent_id: "claude-code".into(), acp_session_id: None, external_id: None, cwd: "x".into(), title: None, source: SessionSource::Owned, status: SessionStatus::Thinking, started_at: 1, last_event_at: 1, ended_at: None })).await;
    f.hub.on_host_event(chunk(&id, "Woof! ")).await;
    f.hub.on_host_event(chunk(&id, "All quiet.")).await;
    f.hub.on_host_event(stop(&id)).await;
    assert_eq!(turn.await.unwrap().unwrap(), "Woof! All quiet.");
    // Nothing of it reached the hub: no session, no pending request, no event, no broadcast.
    assert!(f.hub.session(&id).is_none() && f.hub.store().session(&id).unwrap().is_none());
    assert!(f.hub.st().pending.is_empty());
    let msgs = drain(&mut f.rx);
    assert!(!msgs.iter().any(|m| serde_json::to_string(m).unwrap().contains(":brain-")), "{msgs:?}");
    // The warm session is reused for the next turn.
    let m = model.clone();
    let turn = tokio::spawn(async move { m.complete("sys", "again", Duration::from_secs(10)).await });
    wait_until("the third prompt", || {
        let fake = f.fake.clone();
        async move { fake.calls().iter().filter(|c| c.starts_with("prompt ") && c.contains(":brain-")).count() == 3 }
    })
    .await;
    assert_eq!(f.fake.calls().iter().filter(|c| c.starts_with("start claude-code")).count(), 2, "warm: no new session for this turn");
    // A tool the agent starts without asking ends the turn: the text so far is discarded, the turn cancelled.
    f.hub.on_host_event(chunk(&id, "Here is what I found: ")).await;
    f.hub
        .on_host_event(HostEvent::Event { session_id: id.clone(), kind: EventType::ToolCall, payload: json!({ "toolCallId": "t2", "title": "Read secrets", "kind": "read", "status": "pending" }) })
        .await;
    f.hub.on_host_event(stop(&id)).await;
    assert!(turn.await.unwrap().is_err(), "no answer from a turn that ran a tool");
    assert!(f.fake.calls().iter().any(|c| c == &format!("cancel {id}")));
    // A reply past the cap ends the turn too.
    let m = model.clone();
    let turn = tokio::spawn(async move { m.complete("sys", "long", Duration::from_secs(10)).await });
    wait_until("the fourth prompt", || {
        let fake = f.fake.clone();
        async move { fake.calls().iter().filter(|c| c.starts_with("prompt ") && c.contains(":brain-")).count() == 4 }
    })
    .await;
    let id = brain_prompt(&f).await;
    let big = "x".repeat(40 * 1024);
    f.hub.on_host_event(chunk(&id, &big)).await;
    f.hub.on_host_event(chunk(&id, &big)).await;
    assert!(turn.await.unwrap().unwrap_err().to_string().contains("longer than"));
    // Hook reports from its folder are dropped.
    let cwd = f.hub.paths().data_dir.join("brain-session").to_string_lossy().into_owned();
    f.hub.ingest_hook(HookSource::Claude, json!({ "hook_event_name": "UserPromptSubmit", "session_id": "brain-ext", "cwd": cwd, "prompt": "hi" })).await.unwrap();
    assert!(!f.hub.st().sessions.values().any(|s| s.external_id.as_deref() == Some("brain-ext")));
}

#[tokio::test]
async fn only_claude_code_can_be_the_acp_provider() {
    let f = fixture_config(|c| c.brain.acp_agent = Some("codex".into())).await;
    assert!(f.hub.acp_model().is_none(), "codex has no lockdown: no acp provider");
    let f = fixture_config(|c| c.brain.acp_agent = None).await;
    assert!(f.hub.acp_model().is_some());
}

#[tokio::test]
async fn a_silent_brain_session_times_out_and_restarts() {
    let f = fixture(None).await;
    let model = f.hub.acp_model().unwrap();
    let err = model.complete("sys", "hi", Duration::from_millis(300)).await.unwrap_err();
    assert!(err.to_string().contains("did not answer"), "{err:#}");
    let first = f.fake.calls().iter().find(|c| c.starts_with("prompt ")).unwrap().split_whitespace().nth(1).unwrap().to_string();
    assert!(f.fake.calls().iter().any(|c| c == &format!("close {first}")), "closed after the timeout: {:?}", f.fake.calls());
    // The next turn starts a new session.
    let m = model.clone();
    let turn = tokio::spawn(async move { m.complete("sys", "hi", Duration::from_secs(10)).await });
    wait_until("a second session", || {
        let fake = f.fake.clone();
        async move { fake.calls().iter().filter(|c| c.starts_with("start claude-code")).count() == 2 }
    })
    .await;
    let id = brain_prompt(&f).await;
    assert_ne!(id, first);
    f.hub.on_host_event(chunk(&id, "ok")).await;
    f.hub.on_host_event(stop(&id)).await;
    assert_eq!(turn.await.unwrap().unwrap(), "ok");
}

/// End to end: with no API key and Claude Code installed, the default pet reads a free-form message
/// through the agent session; its "approve" is never an approval (the reply points at the card).
#[tokio::test]
async fn the_default_pet_reads_free_form_messages_through_the_agent_session() {
    let mut f = fixture(None).await;
    let s = start(&f).await;
    let opt = |id: &str, kind| pawl_protocol::PermissionOption { option_id: id.into(), name: id.into(), kind };
    f.hub
        .on_host_event(HostEvent::PermissionRequest(PermissionRequest {
            request_id: global_id(MID, "r1"),
            machine_id: MID.into(),
            session_id: s.id.clone(),
            agent_id: "codex".into(),
            tool_call: ToolCallPayload { tool_call_id: "t".into(), title: Some("bun test".into()), kind: Some(ToolKind::Execute), ..Default::default() },
            options: vec![opt("allow-once", PermissionOptionKind::AllowOnce), opt("reject-once", PermissionOptionKind::RejectOnce)],
            created_at: 1,
        }))
        .await;
    drain(&mut f.rx);
    ack_result(f.hub.handle(ClientMsg::Chat { id: "c".into(), pet_id: None, text: "ya go for it buddy, ship that".into() }, &actor()).await);
    let id = brain_prompt(&f).await;
    let prompt = f.fake.calls().into_iter().find(|c| c.starts_with(&format!("prompt {id}"))).unwrap();
    assert!(prompt.contains("JSON schema") && prompt.contains("<context>") && prompt.contains("R1: Codex"), "{prompt}");
    let answer = json!({ "kind": "resolve", "agent": null, "machine": null, "repo": null, "pet": null, "session": null, "task": null, "decision": "approve", "which": "R1", "answer": null });
    f.hub.on_host_event(chunk(&id, &answer.to_string())).await;
    f.hub.on_host_event(stop(&id)).await;
    let reply = match wait_for(&mut f.rx, "the pet's reply", |m| matches!(m, ServerMsg::Chat { message } if message.role == ChatRole::Pet)).await {
        ServerMsg::Chat { message } => message,
        _ => unreachable!(),
    };
    assert!(reply.text.contains("Tap Approve on the card above if you want Codex to run bun test."), "{}", reply.text);
    assert!(reply.proposals.is_empty() && reply.session_id.is_none());
    assert!(!f.fake.calls().iter().any(|c| c.starts_with(&format!("resolve {}", global_id(MID, "r1")))), "never approved: {:?}", f.fake.calls());
    assert!(f.hub.st().pending.contains_key(&global_id(MID, "r1")), "still waiting for the user");
}

/// At start the "acp" provider exists only once agent discovery has run: a chat in that window waits for it
/// (bounded) instead of answering as if no model were configured.
#[tokio::test]
async fn a_chat_during_startup_discovery_waits_for_the_agent_model() {
    let mut f = fixture(None).await;
    // As right after a start: discovery running, Claude Code not listed yet, no model.
    f.hub.inner.agents_discovered.send_replace(false);
    f.hub.st().agents.retain(|a| a.id != "claude-code");
    f.hub.rebuild_brain();
    assert!(!f.hub.brain().has_llm() && f.hub.pet_model_pending());
    drain(&mut f.rx);
    ack_result(f.hub.handle(ClientMsg::Chat { id: "c".into(), pet_id: None, text: "zxq frobnicate the thing".into() }, &actor()).await);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !drain(&mut f.rx).iter().any(|m| matches!(m, ServerMsg::Chat { message } if message.role == ChatRole::Pet)),
        "no reply while the model is resolving"
    );
    // Discovery finishes: Claude Code is there, the brain gets the acp provider, the chat goes to it.
    f.hub.st().agents.push(agent("claude-code"));
    f.hub.rebuild_brain();
    f.hub.inner.agents_discovered.send_replace(true);
    let id = brain_prompt(&f).await;
    let answer = json!({ "kind": "smalltalk", "agent": null, "machine": null, "repo": null, "pet": null, "session": null, "task": null, "decision": null, "which": null, "answer": "Woof! Right here." });
    f.hub.on_host_event(chunk(&id, &answer.to_string())).await;
    f.hub.on_host_event(stop(&id)).await;
    let reply = match wait_for(&mut f.rx, "the pet's reply", |m| matches!(m, ServerMsg::Chat { message } if message.role == ChatRole::Pet)).await {
        ServerMsg::Chat { message } => message,
        _ => unreachable!(),
    };
    assert_eq!(reply.text, "\u{1F43E} Right here.", "plain style: the pet sound goes, one emoji first");
    // Nothing to wait for once discovery is done (or when another model answers).
    assert!(!f.hub.pet_model_pending());
}

// ---------- ask-by-default rules and the default pet's name ----------

async fn hub_on(dir: &std::path::Path, edit: impl FnOnce(&Store)) -> Hub {
    let paths = Paths::in_dir(dir);
    let store = Store::open(&paths.db, MID).unwrap();
    store.replace_agents(&[agent("codex"), agent("claude-code")]).unwrap();
    edit(&store);
    Hub::build(
        HubOptions { paths, config: HubConfig { offline: true, ..Default::default() }, token: "t".repeat(64), machine: ident(), background: false },
        store,
        Host::Fake(FakeHost::default()),
    )
    .await
    .unwrap()
}

/// Stored rules that are exactly the old defaults (reads automatic) move to the new defaults once, audited,
/// with one notice; a customised rule set is left alone.
#[tokio::test]
async fn never_customised_default_rules_move_to_ask_once() {
    let dir = tempfile::tempdir().unwrap();
    let hub = hub_on(dir.path(), |s| s.set_rules(&pawl_brain::rules::legacy_default_rules()).unwrap()).await;
    assert_eq!(hub.st().rules, pawl_brain::rules::default_rules());
    assert_eq!(hub.store().rules().unwrap(), Some(pawl_brain::rules::default_rules()), "persisted");
    let notices = hub.inner.startup_notices.lock().unwrap().clone();
    assert!(notices.iter().any(|n| n.title == "Default rules now ask before reads"), "{notices:?}");
    let audit = hub.store().settings_audit(10).unwrap();
    assert!(audit.iter().any(|a| a.keys == ["rules"] && a.actor == "hub:default-rules"), "{audit:?}");
    drop(hub);
    // The next start finds the new defaults: nothing to do, no notice.
    let hub = hub_on(dir.path(), |_| {}).await;
    assert!(!hub.inner.startup_notices.lock().unwrap().iter().any(|n| n.title.starts_with("Default rules")));
    drop(hub);
    // A customised set (the old defaults plus one rule) stays as it is.
    let dir = tempfile::tempdir().unwrap();
    let mut custom = pawl_brain::rules::legacy_default_rules();
    custom.push(Rule { id: "mine".into(), matcher: pawl_protocol::RuleMatch { tool_kind: Some(ToolKind::Fetch), path: None, agent: None }, action: pawl_protocol::RuleAction::Deny });
    let c2 = custom.clone();
    let hub = hub_on(dir.path(), move |s| s.set_rules(&c2).unwrap()).await;
    assert_eq!(hub.st().rules, custom);
    // No stored rules: the new defaults, read asks.
    let f = fixture(None).await;
    assert_eq!(f.hub.st().rules, pawl_brain::rules::default_rules());
}

/// The default pet gets a name of the pool once (not "Pawl", not another pet's), kept across starts.
#[tokio::test]
async fn the_default_pet_is_named_once_and_renamed_by_the_pet_name_rules() {
    let dir = tempfile::tempdir().unwrap();
    let hub = hub_on(dir.path(), |_| {}).await;
    let name = hub.settings().bot.pet_name;
    assert!(!name.is_empty() && name != "Pawl", "{name:?}");
    assert!(!hub.pets().iter().any(|p| p.name.eq_ignore_ascii_case(&name)), "unique");
    assert!(std::fs::read_to_string(&hub.paths().config).unwrap().lines().any(|l| l.trim() == format!("pet_name = \"{name}\"")));
    assert_eq!(hub.brain_context(None).await.pet_name.as_deref(), Some(name.as_str()), "the bot knows it");
    drop(hub);
    let saved = HubConfig::load(&Paths::in_dir(dir.path())).unwrap();
    assert_eq!(saved.brain.pet_name.as_deref(), Some(name.as_str()));
    let again = hub_on(dir.path(), |_| {}).await;
    assert_eq!(again.settings().bot.pet_name, name, "the same name after a restart");
    again.ensure_pet("codex").await;
    assert!(!again.pets()[0].name.eq_ignore_ascii_case(&name), "a new pet never gets the default pet's name");
    // Renames follow the pet-name rules and broadcast the settings.
    let mut rx = again.subscribe();
    let actor = actor();
    let set = |n: &str| ClientMsg::UpdateSettings { id: "u".into(), patch: Box::new(serde_json::from_value(json!({ "bot": { "petName": n } })).unwrap()) };
    let taken = again.pets().into_iter().next().map(|p| p.name);
    if let Some(taken) = taken {
        let e = ack_error(again.handle(set(&taken.to_uppercase()), &actor).await);
        assert!(e.contains("invalid:bot.petName") && e.contains("another pet's name"), "{e}");
    }
    for bad in ["", "   ", "Mo\u{202e}chi", &"x".repeat(41)] {
        assert!(ack_error(again.handle(set(bad), &actor).await).contains("invalid:bot.petName"), "{bad:?}");
    }
    ack_result(again.handle(set(" Juno "), &actor).await);
    assert_eq!(again.settings().bot.pet_name, "Juno");
    assert!(std::iter::from_fn(|| rx.try_recv().ok()).any(|m| matches!(m, ServerMsg::Settings { .. })), "announced");
    // A regular pet cannot take the default pet's name.
    let pet = again.pets().into_iter().next().unwrap();
    let r = again
        .handle(ClientMsg::UpdatePet { id: "p".into(), pet_id: pet.id, patch: PetPatch { name: Some("juno".into()), ..Default::default() } }, &actor)
        .await;
    assert!(ack_error(r).contains("name_taken"));
}
