//! Message-handling tests against a fake ACP host (no agent processes).

use super::host::fake::FakeHost;
use super::*;
use pet_protocol::{
    PermissionOption, PermissionOptionKind, RuleMatch, Species, ToolCallPayload, ToolKind, ToolLocation,
};

struct Fixture {
    hub: Hub,
    fake: FakeHost,
    rx: broadcast::Receiver<ServerMsg>,
    dir: tempfile::TempDir,
}

fn agent(id: &str) -> Agent {
    Agent {
        id: id.into(),
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
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::in_dir(dir.path());
    let store = Store::open(&paths.db).unwrap();
    store.replace_agents(&[agent("codex"), agent("claude-code")]).unwrap();
    if let Some(r) = &rules {
        store.set_rules(r).unwrap();
    }
    let fake = FakeHost::default();
    let config = HubConfig { offline: true, ..Default::default() };
    let hub = Hub::build(
        HubOptions { paths, config, token: "t".repeat(64), background: false },
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
        request_id: format!("req-{}", uuid::Uuid::new_v4()),
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
        created_at: pet_protocol::now_ms(),
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

    let v = ack_result(
        f.hub.handle(ClientMsg::GetEvents { id: "3".into(), session_id: s.id.clone(), since_id: None, limit: None }, &actor()).await,
    );
    let events: Vec<HubEvent> = serde_json::from_value(v["events"].clone()).unwrap();
    let kinds: Vec<EventType> = events.iter().map(|e| e.kind).collect();
    assert_eq!(kinds, vec![EventType::SessionStart, EventType::UserMessage, EventType::UserMessage]);
    let since = ack_result(
        f.hub
            .handle(ClientMsg::GetEvents { id: "4".into(), session_id: s.id.clone(), since_id: Some(events[1].id), limit: Some(10) }, &actor())
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
    assert!(err.contains("unknown session"));
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

    let err = ack_error(
        f.hub.handle(ClientMsg::ResolvePermission { id: "r2".into(), request_id: req.request_id.clone(), option_id: None }, &actor()).await,
    );
    assert!(err.contains("no pending permission request"));
}

#[tokio::test]
async fn auto_rules_resolve_without_asking() {
    let rules = vec![
        Rule { id: "allow-exec".into(), matcher: RuleMatch { tool_kind: Some(ToolKind::Execute), ..Default::default() }, action: RuleAction::AutoAllow },
        Rule { id: "no-delete".into(), matcher: RuleMatch { tool_kind: Some(ToolKind::Delete), ..Default::default() }, action: RuleAction::Deny },
    ];
    let mut f = fixture(Some(rules)).await;
    let s = start(&f).await;
    drain(&mut f.rx);

    let allow = permission(&s.id, ToolKind::Execute);
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
    let req = permission(&s.id, ToolKind::Execute);
    f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    assert_eq!(f.hub.pending_permissions().len(), 1);
    drain(&mut f.rx);

    let dup = vec![
        Rule { id: "a".into(), matcher: RuleMatch::default(), action: RuleAction::Ask },
        Rule { id: "a".into(), matcher: RuleMatch::default(), action: RuleAction::Ask },
    ];
    assert!(ack_error(f.hub.handle(ClientMsg::SetRules { id: "1".into(), rules: dup }, &actor()).await).contains("duplicate"));

    let rules = vec![Rule {
        id: "exec-ok".into(),
        matcher: RuleMatch { tool_kind: Some(ToolKind::Execute), ..Default::default() },
        action: RuleAction::AutoAllow,
    }];
    let v = ack_result(f.hub.handle(ClientMsg::SetRules { id: "2".into(), rules: rules.clone() }, &actor()).await);
    assert_eq!(serde_json::from_value::<Vec<Rule>>(v["rules"].clone()).unwrap(), rules);
    assert!(f.hub.pending_permissions().is_empty());
    assert_eq!(f.hub.store().rules().unwrap(), Some(rules));
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
    ended.ended_at = Some(pet_protocol::now_ms());
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
    assert!(err.contains("name"));
    assert!(ack_error(f.hub.handle(ClientMsg::UpdatePet { id: "u3".into(), pet_id: "nope".into(), patch: PetPatch::default() }, &actor()).await)
        .contains("unknown pet"));
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
    let store = Store::open(&paths.db).unwrap();
    let hub = Hub::build(
        HubOptions { paths, config: HubConfig { offline: true, ..Default::default() }, token: "t".into(), background: false },
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
