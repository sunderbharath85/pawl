//! Settings over the hub: runtime apply, audit, hooks, local-only (fake ACP host, temp dirs).

use super::*;
use crate::core::host::fake::FakeHost;
use crate::store::SettingsAuditKind;
use pawl_protocol::{Intent, IntentRejectReason, PeerBody};

const MID: &str = "abcdef012345";
const OTHER: &str = "zyxwvu987654";
const SECRET: &str = "sk-ant-settings-test-9f8e7d6c5b4a";
const RELAY_SECRET: &str = "rk_settings_test_ABCdef012";

struct Fx {
    hub: Hub,
    rx: broadcast::Receiver<ServerMsg>,
    dir: tempfile::TempDir,
}

async fn hub_in(dir: &Path, mid: &str, name: &str, edit: impl FnOnce(&mut HubConfig)) -> Hub {
    let paths = Paths::in_dir(dir);
    let store = Store::open(&paths.db, mid).unwrap();
    let mut config = HubConfig { offline: true, assistant: crate::config::AssistantSection::off(), ..Default::default() };
    edit(&mut config);
    let hub = Hub::build(
        HubOptions { paths, config, token: "t".repeat(64), machine: MachineIdentity { id: mid.into(), name: name.into() }, background: false },
        store,
        Host::Fake(FakeHost::default()),
    )
    .await
    .unwrap();
    hub.start_peer_links().await.unwrap();
    hub
}

async fn fx() -> Fx {
    fx_with(|_| {}).await
}

async fn fx_with(edit: impl FnOnce(&mut HubConfig)) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let hub = hub_in(dir.path(), MID, "Test PC", edit).await;
    hub.set_launch(47800, BTreeMap::new());
    hub.set_hooks_home(dir.path().join("home"));
    let rx = hub.subscribe();
    Fx { hub, rx, dir }
}

fn desktop() -> Actor {
    Actor::client(ClientKind::Desktop, 7)
}

async fn update(hub: &Hub, patch: Value) -> ServerMsg {
    let patch: HubSettingsPatch = serde_json::from_value(patch).unwrap();
    hub.handle(ClientMsg::UpdateSettings { id: "u".into(), patch: Box::new(patch) }, &desktop()).await
}

fn ok(m: ServerMsg) -> Value {
    match m {
        ServerMsg::Ack { ok: true, result, .. } => result.unwrap_or(Value::Null),
        other => panic!("expected ok ack, got {other:?}"),
    }
}

fn err(m: ServerMsg) -> String {
    match m {
        ServerMsg::Ack { ok: false, error, .. } => error.unwrap_or_default(),
        other => panic!("expected error ack, got {other:?}"),
    }
}

fn drain(rx: &mut broadcast::Receiver<ServerMsg>) -> Vec<ServerMsg> {
    std::iter::from_fn(|| rx.try_recv().ok()).collect()
}

fn settings_msgs(msgs: &[ServerMsg]) -> usize {
    msgs.iter().filter(|m| matches!(m, ServerMsg::Settings { .. })).count()
}

fn config_text(hub: &Hub) -> String {
    std::fs::read_to_string(&hub.paths().config).unwrap_or_default()
}

/// Every file in the data dir except config.toml (hub.db, its WAL, logs) as bytes.
fn data_dir_blobs(dir: &Path) -> Vec<(String, Vec<u8>)> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().is_file() && e.file_name() != "config.toml")
        .map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap()))
        .collect()
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[tokio::test]
async fn get_settings_reports_effective_values_and_overrides() {
    let f = fx_with(|c| c.brain.anthropic_api_key = Some(SECRET.into())).await;
    let mut overrides = BTreeMap::new();
    overrides.insert("hub.port".to_string(), "--port".to_string());
    f.hub.set_launch(47800, overrides);
    let v = ok(f.hub.handle(ClientMsg::GetSettings { id: "g".into() }, &desktop()).await);
    let s: HubSettings = serde_json::from_value(v["settings"].clone()).unwrap();
    assert_eq!(s.hub.port, 47800);
    assert!(s.bot.anthropic_api_key_set);
    assert_eq!(s.overridden_by.get("hub.port").map(String::as_str), Some("--port"));
    assert_eq!(v["restartRequired"], json!([]));
    assert_eq!(v["configPath"].as_str().unwrap(), f.hub.paths().config.to_string_lossy());
    assert!(!v.to_string().contains(SECRET), "{v}");
}

#[tokio::test]
async fn the_peer_listener_turns_on_and_off_at_runtime() {
    let mut f = fx().await;
    assert_eq!(f.hub.peer_status().listening, None);
    // Pick a free port, then let the listener take it.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let addr = format!("127.0.0.1:{port}");
    let v = ok(update(&f.hub, json!({ "peer": { "listen": addr } })).await);
    assert_eq!(v["applied"], json!(["peer.listen"]));
    assert_eq!(v["settings"]["peer"]["listen"], addr.as_str());
    assert_eq!(f.hub.peer_status().listening.as_deref(), Some(addr.as_str()));
    tokio::net::TcpStream::connect(&addr).await.expect("listener reachable");
    let msgs = drain(&mut f.rx);
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::PeerStatus { peer } if peer.listening.as_deref() == Some(addr.as_str()))));
    assert_eq!(settings_msgs(&msgs), 1);
    assert_eq!(HubConfig::load(f.hub.paths()).unwrap().peer.listen.as_deref(), Some(addr.as_str()), "written to config.toml");
    // Invites work now.
    ok(f.hub.handle(ClientMsg::CreateInvite { id: "i".into(), ttl_secs: None, addr: None, for_phone: None }, &desktop()).await);

    let v = ok(update(&f.hub, json!({ "peer": { "listen": null } })).await);
    assert_eq!(v["settings"]["peer"]["listen"], Value::Null);
    assert_eq!(f.hub.peer_status().listening, None);
    assert!(tokio::net::TcpStream::connect(&addr).await.is_err(), "refused once off");
    assert_eq!(HubConfig::load(f.hub.paths()).unwrap().peer.listen, None);
    assert_eq!(err(f.hub.handle(ClientMsg::CreateInvite { id: "i".into(), ttl_secs: None, addr: None, for_phone: None }, &desktop()).await), "peer_listener_off");
}

#[tokio::test]
async fn a_listener_that_cannot_bind_changes_nothing() {
    let f = fx().await;
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = taken.local_addr().unwrap().to_string();
    let before = config_text(&f.hub);
    let e = err(update(&f.hub, json!({ "peer": { "listen": addr } })).await);
    assert!(e.starts_with("invalid:peer.listen: could not listen"), "{e}");
    assert_eq!(config_text(&f.hub), before);
    assert_eq!(f.hub.peer_status().listening, None);
    assert_eq!(f.hub.config().peer.listen, None);
}

#[tokio::test]
async fn invalid_patches_write_and_apply_nothing() {
    let mut f = fx().await;
    let before = config_text(&f.hub);
    let cfg_before = f.hub.config();
    // A good value next to a bad one: the good one is not applied either.
    let e = err(update(&f.hub, json!({ "peer": { "acceptIntents": false }, "hub": { "dailyReportTime": "7pm" } })).await);
    assert!(e.starts_with("invalid:hub.dailyReportTime: "), "{e}");
    let e = err(update(&f.hub, json!({ "watchdog": { "tickSecs": 1 } })).await);
    assert!(e.starts_with("invalid:watchdog.tickSecs: "), "{e}");
    let e = err(update(&f.hub, json!({ "customAgents": [{ "id": "claude-code", "name": "X", "command": ["x"] }] })).await);
    assert!(e.starts_with("invalid:customAgents[0].id: "), "{e}");
    assert_eq!(config_text(&f.hub), before);
    assert!(f.hub.config().peer.accept_intents);
    assert_eq!(f.hub.settings(), cfgset::to_settings(&cfg_before, &f.hub.overrides()));
    assert!(f.hub.store().settings_audit(10).unwrap().is_empty());
    assert_eq!(settings_msgs(&drain(&mut f.rx)), 0);
    // A patch that changes nothing is fine and writes nothing.
    let v = ok(update(&f.hub, json!({ "peer": { "acceptIntents": true } })).await);
    assert_eq!(v["applied"], json!([]));
    assert!(f.hub.store().settings_audit(10).unwrap().is_empty());
}

#[tokio::test]
async fn accept_intents_and_start_roots_apply_at_runtime() {
    let f = fx().await;
    let cancel = Intent::Cancel { session_id: global_id(MID, "nope") };
    let ack = f.hub.handle_intent(OTHER, "i-1", ClientKind::Desktop, true, &cancel).await.unwrap();
    assert_ne!(ack.reason, Some(IntentRejectReason::Forbidden));
    ok(update(&f.hub, json!({ "peer": { "acceptIntents": false } })).await);
    let ack = f.hub.handle_intent(OTHER, "i-2", ClientKind::Desktop, true, &cancel).await.unwrap();
    assert_eq!(ack.reason, Some(IntentRejectReason::Forbidden), "remote control off at once");
    ok(update(&f.hub, json!({ "peer": { "acceptIntents": true } })).await);
    let ack = f.hub.handle_intent(OTHER, "i-3", ClientKind::Desktop, true, &cancel).await.unwrap();
    assert_ne!(ack.reason, Some(IntentRejectReason::Forbidden));

    let root = f.dir.path().join("work");
    let inside = root.join("api");
    std::fs::create_dir_all(&inside).unwrap();
    let canon = crate::repos::canonical_dir(&inside).unwrap();
    assert!(!f.hub.is_known_folder(&canon).await);
    ok(update(&f.hub, json!({ "peer": { "startRoots": [root.to_string_lossy()] } })).await);
    assert!(f.hub.is_known_folder(&canon).await, "new start root used at once");

    // [repos] paths rebuild the index right away.
    ok(update(&f.hub, json!({ "repos": { "paths": [inside.to_string_lossy()] } })).await);
    assert!(f.hub.repos().iter().any(|r| r.name == "api"), "{:?}", f.hub.repos());
}

#[tokio::test]
async fn the_port_needs_a_restart_and_everything_else_applies_now() {
    let mut f = fx().await;
    let v = ok(update(&f.hub, json!({ "hub": { "port": 48123, "eventRetentionDays": 7 }, "watchdog": { "tickSecs": 10 }, "bot": { "model": "claude-sonnet-5-5" } })).await);
    assert_eq!(v["restartRequired"], json!(["hub.port"]));
    assert_eq!(v["applied"], json!(["hub.eventRetentionDays", "bot.model", "watchdog.tickSecs"]));
    assert_eq!(v["settings"]["hub"]["port"], 48123);
    let msgs = drain(&mut f.rx);
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Settings { restart_required, .. } if restart_required == &vec!["hub.port".to_string()])));
    assert_eq!(f.hub.config().watchdog.tick_secs, 10);
    assert_eq!(HubConfig::load(f.hub.paths()).unwrap().port, 48123);
    // Back to the running port: nothing to restart for.
    let v = ok(update(&f.hub, json!({ "hub": { "port": 47800 } })).await);
    assert_eq!(v["restartRequired"], json!([]));
    assert_eq!(ok(f.hub.handle(ClientMsg::GetSettings { id: "g".into() }, &desktop()).await)["restartRequired"], json!([]));
}

#[tokio::test]
async fn audit_rows_name_keys_and_clients_never_secret_values() {
    let mut f = fx().await;
    let v = ok(update(&f.hub, json!({ "bot": { "anthropicApiKey": SECRET, "ollamaModel": "llama3.2" }, "relay": { "url": "wss://relay.invalid", "accessKey": RELAY_SECRET } })).await);
    assert_eq!(v["settings"]["bot"]["anthropicApiKeySet"], true);
    assert_eq!(v["settings"]["relay"]["accessKeySet"], true);
    assert!(!v.to_string().contains(SECRET) && !v.to_string().contains(RELAY_SECRET));
    for m in drain(&mut f.rx) {
        let t = serde_json::to_string(&m).unwrap();
        assert!(!t.contains(SECRET) && !t.contains(RELAY_SECRET), "{t}");
    }
    let rows = f.hub.store().settings_audit(10).unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.kind, SettingsAuditKind::Settings);
    assert_eq!(row.actor, "desktop#7");
    assert_eq!(row.client.as_deref(), Some("desktop"));
    assert_eq!(row.keys, ["bot.anthropicApiKey", "bot.ollamaModel", "relay.url", "relay.accessKey"]);
    // The secrets live in config.toml only: never in hub.db (or its WAL) or any other file here.
    assert!(config_text(&f.hub).contains(SECRET));
    for (name, bytes) in data_dir_blobs(f.dir.path()) {
        assert!(!contains(&bytes, SECRET) && !contains(&bytes, RELAY_SECRET), "a secret leaked into {name}");
    }
    // The relay setting reached the link layer.
    assert_eq!(f.hub.peer_status().relay.url.as_deref(), Some("wss://relay.invalid"));
    ok(update(&f.hub, json!({ "relay": { "url": null } })).await);
    assert_eq!(f.hub.peer_status().relay.url, None);
    assert_eq!(f.hub.store().settings_audit(10).unwrap()[0].keys, ["relay.url"]);
}

#[tokio::test]
async fn a_relay_from_a_pairing_invite_is_kept_once() {
    let mut f = fx().await;
    let relay = RelayConfig { url: "wss://relay.invalid".into(), access_key: Some(RELAY_SECRET.into()) };
    f.hub.adopt_relay(relay.clone(), &desktop()).await.unwrap();
    let cfg = HubConfig::load(f.hub.paths()).unwrap();
    assert_eq!(cfg.relay.url.as_deref(), Some("wss://relay.invalid"));
    assert_eq!(cfg.relay.access_key.as_deref(), Some(RELAY_SECRET));
    let s = f.hub.settings();
    assert!(s.relay.access_key_set && s.relay.url.is_some());
    assert_eq!(settings_msgs(&drain(&mut f.rx)), 1);
    let rows = f.hub.store().settings_audit(10).unwrap();
    assert_eq!((rows.len(), rows[0].kind), (1, SettingsAuditKind::Pairing));
    // A configured relay is never replaced by a later invite's.
    let bad = RelayConfig { url: "ws://not-loopback.example.com".into(), access_key: None };
    let fresh = fx().await;
    let e = fresh.hub.adopt_relay(bad, &desktop()).await.unwrap_err();
    assert!(format!("{e:#}").starts_with("invalid:relay.url: "), "{e:#}");
    let e = fresh.hub.adopt_relay(RelayConfig { url: "wss://relay.invalid".into(), access_key: Some("bad key!".into()) }, &desktop()).await.unwrap_err();
    assert!(format!("{e:#}").starts_with("invalid:relay.accessKey: "), "{e:#}");
    assert_eq!(HubConfig::load(fresh.hub.paths()).unwrap().relay.url, None, "an invalid invite relay is never written");
    assert!(fresh.hub.store().settings_audit(10).unwrap().is_empty());
    let other = RelayConfig { url: "wss://other.invalid".into(), access_key: None };
    f.hub.adopt_relay(other, &desktop()).await.unwrap();
    assert_eq!(HubConfig::load(f.hub.paths()).unwrap().relay.url.as_deref(), Some("wss://relay.invalid"));
    assert_eq!(f.hub.store().settings_audit(10).unwrap().len(), 1);
}

#[tokio::test]
async fn bad_relay_values_are_field_errors() {
    let f = fx().await;
    let e = err(update(&f.hub, json!({ "relay": { "url": "ws://relay.example.com" } })).await);
    assert!(e.starts_with("invalid:relay.url: "), "{e}");
    let e = err(update(&f.hub, json!({ "relay": { "url": "wss://relay.example.com", "accessKey": "has spaces" } })).await);
    assert!(e.starts_with("invalid:relay.accessKey: "), "{e}");
    assert_eq!(f.hub.peer_status().relay.url, None);
}

#[tokio::test]
async fn hooks_preview_writes_nothing_then_apply_and_remove() {
    let f = fx().await;
    let home = f.dir.path().join("home");
    let settings_json = home.join(".claude").join("settings.json");
    let v = ok(f.hub.handle(ClientMsg::HookStatus { id: "h".into() }, &desktop()).await);
    let hooks: Vec<HookStatus> = serde_json::from_value(v["hooks"].clone()).unwrap();
    let claude = hooks.iter().find(|h| h.target == HookTargetName::Claude).unwrap();
    assert_eq!(claude.state, HookState::Missing);
    assert_eq!(Path::new(&claude.config_path), settings_json);
    assert!(hooks.iter().any(|h| h.target == HookTargetName::Codex) && hooks.iter().any(|h| h.target == HookTargetName::CopilotCli));

    let install = |action: HookAction| ClientMsg::InstallHooks { id: "i".into(), target: HookTargetName::Claude, repo: None, action };
    let v = ok(f.hub.handle(install(HookAction::Preview), &desktop()).await);
    assert_eq!(v["applied"], false);
    assert!(v["plan"].as_str().unwrap().contains("hook claude"), "{v}");
    assert_eq!(v["status"]["state"], "missing");
    assert!(!settings_json.exists(), "preview writes nothing");
    assert!(f.hub.store().settings_audit(10).unwrap().is_empty(), "previews are not audited");

    let v = ok(f.hub.handle(install(HookAction::Apply), &desktop()).await);
    assert_eq!((v["applied"].as_bool(), v["status"]["state"].as_str()), (Some(true), Some("installed")));
    assert!(settings_json.is_file());
    let v = ok(f.hub.handle(install(HookAction::Apply), &desktop()).await);
    assert_eq!(v["applied"], false, "already installed: nothing written");

    let v = ok(f.hub.handle(install(HookAction::Remove), &desktop()).await);
    assert_eq!((v["applied"].as_bool(), v["status"]["state"].as_str()), (Some(true), Some("missing")));
    let rows = f.hub.store().settings_audit(10).unwrap();
    assert_eq!(rows.iter().map(|r| (r.kind, r.keys.clone(), r.detail.clone())).collect::<Vec<_>>(), vec![
        (SettingsAuditKind::Hooks, vec!["hooks.claude".to_string()], Some("remove".to_string())),
        (SettingsAuditKind::Hooks, vec!["hooks.claude".to_string()], Some("apply".to_string())),
    ]);

    // copilot-vscode needs an existing repo folder.
    let e = err(f.hub.handle(ClientMsg::InstallHooks { id: "i".into(), target: HookTargetName::CopilotVscode, repo: None, action: HookAction::Preview }, &desktop()).await);
    assert!(e.starts_with("invalid:repo: "), "{e}");
    let repo = f.dir.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let vscode = |repo: String| ClientMsg::InstallHooks { id: "i".into(), target: HookTargetName::CopilotVscode, repo: Some(repo), action: HookAction::Preview };
    // A folder that is neither in the repo index nor a git checkout is refused.
    let e = err(f.hub.handle(vscode(repo.to_string_lossy().into()), &desktop()).await);
    assert!(e.starts_with("invalid:repo: is not a repository"), "{e}");
    // Network shares and device paths never.
    for p in [r"\\server\share\repo", r"\\.\C:\repo"] {
        let e = err(f.hub.handle(vscode(p.into()), &desktop()).await);
        assert!(e.starts_with("invalid:repo: "), "{p}: {e}");
    }
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let v = ok(f.hub.handle(vscode(repo.to_string_lossy().into()), &desktop()).await);
    let canon = crate::repos::canonical_dir(&repo).unwrap();
    assert_eq!(v["status"]["repo"].as_str().map(PathBuf::from), Some(canon));
    assert!(!repo.join(".github").exists());
    // A repo-index folder needs no .git.
    let indexed = f.dir.path().join("indexed");
    std::fs::create_dir_all(&indexed).unwrap();
    ok(update(&f.hub, json!({ "repos": { "paths": [indexed.to_string_lossy()] } })).await);
    ok(f.hub.handle(vscode(indexed.to_string_lossy().into()), &desktop()).await);
}

#[tokio::test]
async fn a_port_in_use_or_shared_with_the_listener_is_refused() {
    let f = fx().await;
    let before = config_text(&f.hub);
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = taken.local_addr().unwrap().port();
    let e = err(update(&f.hub, json!({ "hub": { "port": busy } })).await);
    assert!(e.starts_with("invalid:hub.port: is in use"), "{e}");
    assert_eq!(config_text(&f.hub), before, "nothing written");
    assert_eq!(f.hub.config().port, 47800);
    drop(taken);
    // The running port itself is fine (it is ours).
    let v = ok(update(&f.hub, json!({ "hub": { "port": 47800 } })).await);
    assert_eq!(v["applied"], json!([]));
    // The peer listener's port is never the API port.
    let peer_port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    ok(update(&f.hub, json!({ "peer": { "listen": format!("127.0.0.1:{peer_port}") } })).await);
    let e = err(update(&f.hub, json!({ "hub": { "port": peer_port } })).await);
    assert!(e.starts_with("invalid:hub.port: is the peer listener's port"), "{e}");
    ok(update(&f.hub, json!({ "peer": { "listen": null } })).await);
}

#[tokio::test]
async fn open_panel_reaches_desktop_clients_only_locally() {
    let mut f = fx().await;
    let v = ok(f.hub.handle(ClientMsg::OpenPanel { id: "o".into(), view: PanelView::Settings }, &desktop()).await);
    assert_eq!(v["delivered"], false);
    let guard = f.hub.client_connected(ClientKind::Desktop);
    let _vscode = f.hub.client_connected(ClientKind::Vscode);
    let v = ok(f.hub.handle(ClientMsg::OpenPanel { id: "o".into(), view: PanelView::Settings }, &Actor::client(ClientKind::Vscode, 2)).await);
    assert_eq!(v["delivered"], true);
    assert!(drain(&mut f.rx).iter().any(|m| matches!(m, ServerMsg::OpenPanel { view: PanelView::Settings })));
    drop(guard);
    let v = ok(f.hub.handle(ClientMsg::OpenPanel { id: "o".into(), view: PanelView::Home }, &desktop()).await);
    assert_eq!(v["delivered"], false, "only the vscode client is left");
    // Neither message is ever offered to the mirror.
    assert!(!crate::bus::is_mirrorable(&ServerMsg::OpenPanel { view: PanelView::Home }));
    assert!(!crate::bus::is_mirrorable(&ServerMsg::Settings { settings: f.hub.settings(), restart_required: vec![] }));
}

/// Compile-time guard: what a paired machine can send is exactly these bodies and intents. A new
/// variant must be reviewed for the settings rule (settings are local-only) before it compiles.
#[allow(dead_code)]
fn peer_surface_has_no_settings(body: &PeerBody, intent: &Intent) {
    match body {
        PeerBody::PairRequest { .. }
        | PeerBody::PairAccept { .. }
        | PeerBody::PairReject { .. }
        | PeerBody::Hello { .. }
        | PeerBody::Presence { .. }
        | PeerBody::Bye { .. }
        | PeerBody::Mirror { .. }
        | PeerBody::SyncRequest { .. }
        | PeerBody::MirrorSnapshot { .. }
        // A2A over the link (protocol v9): start / prompt / cancel and task reads only (`core/a2a_peer.rs`);
        // it never reaches settings.
        | PeerBody::A2a { .. }
        | PeerBody::MirrorEvents { .. }
        | PeerBody::SyncDone { .. }
        | PeerBody::Intent { .. }
        | PeerBody::IntentAck { .. } => {}
        // Step 7: may add roster devices (gated by peer.acceptRosterIntros); never reads or writes settings.
        PeerBody::RosterIntro { .. } => {}
    }
    match intent {
        Intent::Approve { .. } | Intent::Deny { .. } | Intent::Prompt { .. } | Intent::Start { .. } | Intent::Cancel { .. } => {}
    }
}

/// Runtime: a paired machine sends everything it can (presence, mirror sync, every intent kind);
/// this hub's settings, config.toml and settings audit never move.
#[tokio::test]
async fn nothing_a_paired_machine_sends_reaches_settings() {
    let da = tempfile::tempdir().unwrap();
    let a = hub_in(da.path(), MID, "Desk", |_| {}).await;
    a.set_launch(47800, BTreeMap::new());
    // The listener is turned on through settings (the one change in this test).
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let bound = format!("127.0.0.1:{port}");
    let v = ok(update(&a, json!({ "peer": { "listen": bound } })).await);
    assert_eq!(v["settings"]["peer"]["listen"], bound.as_str());
    assert_eq!(a.peer_status().listening.as_deref(), Some(bound.as_str()));
    let db = tempfile::tempdir().unwrap();
    let b = hub_in(db.path(), OTHER, "Laptop", |_| {}).await;
    let mut arx = a.subscribe();
    let uri = ok(a.handle(ClientMsg::CreateInvite { id: "i".into(), ttl_secs: None, addr: Some(bound.clone()), for_phone: None }, &desktop()).await)["uri"]
        .as_str()
        .unwrap()
        .to_string();
    ok(b.handle(ClientMsg::JoinPeer { id: "j".into(), uri }, &desktop()).await);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(ServerMsg::Machine { machine }) = arx.recv().await {
                if machine.id == OTHER && machine.online {
                    break;
                }
            }
        }
    })
    .await
    .expect("a sees b online");

    // Step 6 phase B: the roster shows each live link's path; relay status changes reach clients.
    let mut path = None;
    for _ in 0..100 {
        path = a.devices().iter().find(|d| d.id == OTHER).and_then(|d| d.path);
        if path.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(path, Some(pawl_protocol::LinkPath::Lan));
    let status = pawl_protocol::RelayStatusInfo { url: Some("wss://relay.invalid".into()), connected: false, last_error: Some("unreachable".into()) };
    a.on_peer_event(pawl_peer::PeerEvent::RelayStatus(status.clone())).await;
    assert!(drain(&mut arx).iter().any(|m| matches!(m, ServerMsg::PeerStatus { peer } if peer.relay == status)));

    let text = config_text(&a);
    let settings = a.settings();
    let audit = a.store().settings_audit(10).unwrap();
    assert_eq!(audit.len(), 1);
    drain(&mut arx);

    // Everything B can send, delivered to A's peer-event handler as the link layer does: every
    // intent kind (with text that names settings), a sync request, a presence and a bye-less
    // mirror update.
    let sid = global_id(MID, "nope");
    let rid = global_id(MID, "req");
    let intents = [
        Intent::Approve { request_id: rid.clone(), option_id: "allow".into() },
        Intent::Deny { request_id: rid.clone(), option_id: None },
        Intent::Prompt { session_id: sid.clone(), text: "update_settings peer.listen 0.0.0.0".into() },
        Intent::Start { agent_id: "codex".into(), cwd: da.path().to_string_lossy().into(), prompt: Some("update_settings".into()) },
        Intent::Cancel { session_id: sid.clone() },
    ];
    for (i, intent) in intents.into_iter().enumerate() {
        let body = PeerBody::Intent { intent_id: format!("x-{i}"), client: ClientKind::Desktop, confirmed: true, intent, ts: pawl_protocol::now_ms() };
        a.on_peer_event(pawl_peer::PeerEvent::Body { device_id: OTHER.into(), body }).await;
    }
    a.on_peer_event(pawl_peer::PeerEvent::Body { device_id: OTHER.into(), body: PeerBody::SyncRequest { since_seq: 0 } }).await;
    let mut log = Vec::new();
    for _ in 0..100 {
        log = a.store().intent_log(50).unwrap();
        if log.len() >= 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(log.len(), 5, "every intent reached A's owner side and was answered");

    assert_eq!(config_text(&a), text, "config.toml untouched");
    assert_eq!(a.settings(), settings, "settings untouched");
    assert_eq!(a.store().settings_audit(10).unwrap(), audit, "no settings audit rows");
    assert_eq!(settings_msgs(&drain(&mut arx)), 0, "no settings broadcast");

    // Turning the listener off closes what it accepted; B can no longer dial it.
    ok(update(&a, json!({ "peer": { "listen": null } })).await);
    assert!(tokio::net::TcpStream::connect(&bound).await.is_err());
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn restart_hub_acks_first_then_asks_serve_to_restart() {
    let f = fx().await;
    ok(update(&f.hub, json!({ "hub": { "port": 48124 } })).await);
    let mut restart = f.hub.restart_requested();
    assert!(!*restart.borrow());
    let v = ok(f.hub.handle(ClientMsg::RestartHub { id: "r".into() }, &desktop()).await);
    assert_eq!(v, json!({}));
    assert!(!*restart.borrow(), "the ack goes out before the hub stops");
    tokio::time::timeout(Duration::from_secs(5), restart.wait_for(|r| *r)).await.expect("restart signalled").unwrap();
    let row = &f.hub.store().settings_audit(10).unwrap()[0];
    assert_eq!((row.kind, row.keys.clone(), row.actor.as_str()), (SettingsAuditKind::Restart, vec!["hub.port".to_string()], "desktop#7"));

    // A hub not run by `pawl serve` (e.g. `pawl start` in-process) cannot replace itself.
    let dir = tempfile::tempdir().unwrap();
    let in_process = hub_in(dir.path(), MID, "Test PC", |_| {}).await;
    let e = err(in_process.handle(ClientMsg::RestartHub { id: "r".into() }, &desktop()).await);
    assert!(e.contains("pawl serve"), "{e}");
    assert!(!*in_process.restart_requested().borrow());
}

#[tokio::test]
async fn watchdog_timings_change_without_repeating_nudges() {
    use pawl_protocol::{PermissionOption, PermissionOptionKind, ToolCallPayload};
    let mut f = fx().await;
    let req = PermissionRequest {
        request_id: global_id(MID, "req-wd"),
        machine_id: MID.into(),
        session_id: global_id(MID, "s-wd"),
        agent_id: "codex".into(),
        tool_call: ToolCallPayload { tool_call_id: "tc".into(), title: Some("cargo test".into()), kind: None, status: None, locations: None, raw_input: None },
        options: vec![PermissionOption { option_id: "allow".into(), name: "Allow".into(), kind: PermissionOptionKind::AllowOnce }],
        created_at: pawl_protocol::now_ms() - 3_600_000,
        voice_code: None,
    };
    f.hub.st().pending.insert(req.request_id.clone(), req);
    let nudges = |msgs: &[ServerMsg]| msgs.iter().filter(|m| matches!(m, ServerMsg::Notification { .. })).count();
    f.hub.watchdog_tick().await;
    assert_eq!(nudges(&drain(&mut f.rx)), 1, "an hour-old request is nudged");

    ok(update(&f.hub, json!({ "watchdog": { "waitingNudgeAfterMs": 60_000, "repeatEveryMs": 1_800_000 } })).await);
    let cfg = f.hub.inner.watchdog.lock().unwrap().config();
    assert_eq!((cfg.waiting_nudge_after_ms, cfg.repeat_every_ms), (60_000, 1_800_000), "new timings in use");
    drain(&mut f.rx);
    f.hub.watchdog_tick().await;
    assert_eq!(nudges(&drain(&mut f.rx)), 0, "the watchdog still remembers that nudge");
}

#[tokio::test]
async fn the_default_agent_is_saved_shown_and_refused_when_unknown() {
    let f = fx().await;
    let v = ok(update(&f.hub, json!({ "bot": { "defaultAgent": "codex" } })).await);
    assert_eq!(v["settings"]["bot"]["defaultAgent"], json!("codex"));
    assert_eq!(v["applied"], json!(["bot.defaultAgent"]), "read at use time: applied now");
    let key_line = |hub: &Hub| config_text(hub).lines().any(|l| l.trim() == "default_agent = \"codex\"");
    assert!(key_line(&f.hub), "{}", config_text(&f.hub));
    assert!(err(update(&f.hub, json!({ "bot": { "defaultAgent": "no-such-agent" } })).await).starts_with("invalid:bot.defaultAgent:"));
    assert_eq!(f.hub.settings().bot.default_agent.as_deref(), Some("codex"), "a refused patch changes nothing");
    let v = ok(update(&f.hub, json!({ "bot": { "defaultAgent": null } })).await);
    assert_eq!(v["settings"]["bot"]["defaultAgent"], json!(null));
    assert!(!key_line(&f.hub), "{}", config_text(&f.hub));
}

mod connect;
