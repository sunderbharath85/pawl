//! Settings > A2A > "Connect coding agents": a2a_connect_status / a2a_connect over the hub, and a
//! connected agent's `pawl mcp` talking to Pawl's own A2A server.

use super::*;
use crate::core::a2a_connect::caller_token;
use crate::store::SettingsAuditKind;
use pawl_protocol::{A2aConnectState, A2aConnectStatus};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn fx_a2a() -> (Fx, u16) {
    let port = free_port();
    (fx_with(|c| c.a2a.port = port).await, port)
}

fn connect(target: HookTargetName, action: HookAction) -> ClientMsg {
    ClientMsg::A2aConnect { id: "c".into(), target, action }
}

fn status(v: &Value) -> A2aConnectStatus {
    serde_json::from_value(v["status"].clone()).unwrap()
}

fn claude_json(f: &Fx) -> PathBuf {
    f.dir.path().join("home").join(".claude.json")
}

fn pawl_entry(path: &Path) -> Option<Value> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v["mcpServers"].get("pawl").cloned()
}

fn connect_rows(hub: &Hub) -> Vec<(Vec<String>, Option<String>)> {
    hub.store().settings_audit(50).unwrap().into_iter().filter(|r| r.kind == SettingsAuditKind::A2aConnect).map(|r| (r.keys, r.detail)).collect()
}

#[tokio::test]
async fn connect_status_lists_the_four_agents() {
    let (f, _) = fx_a2a().await;
    let v = ok(f.hub.handle(ClientMsg::A2aConnectStatus { id: "s".into() }, &desktop()).await);
    let targets: Vec<A2aConnectStatus> = serde_json::from_value(v["targets"].clone()).unwrap();
    let names: Vec<_> = targets.iter().map(|t| (t.target, t.name.as_str(), t.caller_id.as_str())).collect();
    assert_eq!(names, vec![
        (HookTargetName::Claude, "Claude Code", "claude-code"),
        (HookTargetName::Codex, "Codex", "codex"),
        (HookTargetName::CopilotCli, "Copilot CLI", "copilot-cli"),
        (HookTargetName::CopilotVscode, "Copilot in VS Code", "copilot-vscode"),
    ]);
    assert!(targets.iter().all(|t| t.state == A2aConnectState::Missing && !t.caller_exists));
    assert_eq!(Path::new(&targets[0].config_path), claude_json(&f));
    assert!(targets[1].config_path.ends_with("config.toml") && targets[3].config_path.ends_with("mcp.json"));
}

#[tokio::test]
async fn preview_writes_nothing_then_connect_turns_the_server_on_and_remove_undoes_it() {
    let (f, port) = fx_a2a().await;
    let me = desktop();
    // Preview: says it will turn the server on; nothing written, no caller, no audit.
    let v = ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Preview), &me).await);
    let plan = v["plan"].as_str().unwrap();
    assert_eq!((v["applied"].as_bool(), v["startsServer"].as_bool()), (Some(false), Some(true)));
    assert!(plan.contains("Turns on Pawl's A2A server (on 127.0.0.1 only") && plan.contains("Creates the A2A caller \"claude-code\"") && plan.contains("<new token>"), "{plan}");
    assert!(!claude_json(&f).exists() && !f.hub.config().a2a.server && f.hub.config().a2a.callers.is_empty());
    assert!(f.hub.store().settings_audit(10).unwrap().is_empty());

    // Connect.
    let v = ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Apply), &me).await);
    assert_eq!((v["applied"].as_bool(), v["startsServer"].as_bool()), (Some(true), Some(true)), "{v}");
    let st = status(&v);
    assert_eq!((st.state, st.caller_exists), (A2aConnectState::Connected, true));
    let cfg = f.hub.config();
    assert!(cfg.a2a.server && f.hub.a2a_status().running);
    let c = cfg.a2a.callers.iter().find(|c| c.id == "claude-code").unwrap();
    assert_eq!((c.name.as_str(), c.allow_approvals), ("Claude Code", false));
    let token = c.token.clone().unwrap();
    assert!(!v["plan"].as_str().unwrap().contains(&token), "the token is never shown");
    let entry = pawl_entry(&claude_json(&f)).unwrap();
    assert_eq!(entry["type"], "stdio");
    assert_eq!(entry["args"], json!(["mcp"]));
    assert_eq!(entry["command"], json!(std::env::current_exe().unwrap().to_string_lossy()));
    assert_eq!(entry["env"], json!({ "PAWL_A2A_URL": format!("http://127.0.0.1:{port}"), "PAWL_A2A_TOKEN": token }));
    assert_eq!(connect_rows(&f.hub), vec![(vec!["a2a.connect.claude".to_string()], Some("apply".to_string()))]);

    // Again: nothing to do, same token.
    let v = ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Apply), &me).await);
    assert_eq!((v["applied"].as_bool(), v["startsServer"].as_bool()), (Some(false), Some(false)));
    assert_eq!(caller_token(&f.hub, "claude-code").as_deref(), Some(token.as_str()));
    let v = ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Preview), &me).await);
    assert!(v["plan"].as_str().unwrap().starts_with("Already connected"), "{v}");

    // A rotated token leaves the entry stale; Connect puts the new one in.
    f.hub.a2a_rotate_token("claude-code", &me).await.unwrap();
    let v = ok(f.hub.handle(ClientMsg::A2aConnectStatus { id: "s".into() }, &me).await);
    assert_eq!(v["targets"][0]["state"], "outdated");
    ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Apply), &me).await);
    let fresh = caller_token(&f.hub, "claude-code").unwrap();
    assert_eq!(pawl_entry(&claude_json(&f)).unwrap()["env"]["PAWL_A2A_TOKEN"], json!(fresh));
    assert_ne!(fresh, token);

    // Remove: the entry and the caller go; the server stays on.
    let v = ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Remove), &me).await);
    assert_eq!(v["applied"], true);
    assert_eq!(status(&v).state, A2aConnectState::Missing);
    assert!(pawl_entry(&claude_json(&f)).is_none());
    assert!(f.hub.config().a2a.callers.is_empty() && f.hub.config().a2a.server);
    let v = ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Remove), &me).await);
    assert_eq!(v["applied"], false);
    let rows = connect_rows(&f.hub);
    assert_eq!(rows.first(), Some(&(vec!["a2a.connect.claude".to_string()], Some("remove".to_string()))));
    assert_eq!(rows.len(), 3, "apply, apply, remove: {rows:?}");
    // Nothing logged or stored holds a token.
    let audit = format!("{:?}", f.hub.store().settings_audit(50).unwrap());
    assert!(!audit.contains(&token) && !audit.contains(&fresh));
    f.hub.stop_a2a_server().await;
}

#[tokio::test]
async fn connect_takes_over_a_hand_made_caller_as_a_new_owner() {
    let port = free_port();
    let f = fx_with(|c| {
        c.a2a.port = port;
        c.a2a.server = true;
        c.a2a.callers.push(crate::config::A2aCaller { id: "codex".into(), name: "My Codex".into(), token: Some("a".repeat(64)), allow_approvals: true, connect_target: None, connect_added_servers: false });
    })
    .await;
    let me = desktop();
    let before = f.hub.a2a_owner_key("codex");
    let v = ok(f.hub.handle(connect(HookTargetName::Codex, HookAction::Preview), &me).await);
    let plan = v["plan"].as_str().unwrap();
    assert_eq!((v["startsServer"].as_bool(), v["newToken"].as_bool()), (Some(false), Some(true)));
    assert!(plan.contains("Takes over the caller \"codex\" you added; its open tasks stay with the old owner and its token stops working") && plan.contains("[mcp_servers.pawl]"), "{plan}");
    assert!(!plan.contains(&"a".repeat(64)));
    assert_eq!(f.hub.a2a_owner_key("codex"), before, "a preview changes nothing");
    let v = ok(f.hub.handle(connect(HookTargetName::Codex, HookAction::Apply), &me).await);
    assert_eq!(v["newToken"], true);
    let after = f.hub.a2a_owner_key("codex");
    assert_ne!(after, before, "the coding agent cannot see the old caller's tasks");
    assert!(after.starts_with("codex~"));
    // Connect again (a new token after a rotation): the same owner, its own tasks stay its own.
    f.hub.a2a_rotate_token("codex", &me).await.unwrap();
    ok(f.hub.handle(connect(HookTargetName::Codex, HookAction::Apply), &me).await);
    assert_eq!(f.hub.a2a_owner_key("codex"), after);
    let c = f.hub.config().a2a.callers.into_iter().find(|c| c.id == "codex").unwrap();
    assert_eq!((c.name.as_str(), c.allow_approvals, c.connect_target), ("My Codex", false, Some(crate::config::ConnectTarget::Codex)), "taken over and marked");
    assert_ne!(c.token.as_deref(), Some("a".repeat(64).as_str()));
    let toml = std::fs::read_to_string(f.dir.path().join("home").join(".codex").join("config.toml")).unwrap();
    assert!(toml.contains("[mcp_servers.pawl]") && toml.contains(c.token.as_deref().unwrap()), "{toml}");
    f.hub.stop_a2a_server().await;
}

#[tokio::test]
async fn a_malformed_agent_config_refuses_and_changes_nothing() {
    let (f, _) = fx_a2a().await;
    let path = claude_json(&f);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{\"mcpServers\": {").unwrap();
    let e = err(f.hub.handle(connect(HookTargetName::Claude, HookAction::Apply), &desktop()).await);
    assert!(e.contains("is not valid JSON") && e.contains("add the entry by hand"), "{e}");
    assert!(!f.hub.config().a2a.server && f.hub.config().a2a.callers.is_empty(), "nothing changed");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"mcpServers\": {");
    let v = ok(f.hub.handle(ClientMsg::A2aConnectStatus { id: "s".into() }, &desktop()).await);
    assert_eq!(v["targets"][0]["state"], "unsupported");
    assert!(v["targets"][0]["detail"].as_str().unwrap().contains("is not valid JSON"));
    assert!(connect_rows(&f.hub).is_empty());
}

#[tokio::test]
async fn a_connected_agent_talks_to_pawl_through_pawl_mcp_until_removed() {
    use pawl_a2a::mcp::{Bridge, BridgeConfig};
    let (f, port) = fx_a2a().await;
    let me = desktop();
    ok(f.hub.handle(connect(HookTargetName::CopilotCli, HookAction::Apply), &me).await);
    // What the agent would run: the env of the entry Connect wrote.
    let cfg: Value = serde_json::from_str(&std::fs::read_to_string(f.dir.path().join("home").join(".copilot").join("mcp-config.json")).unwrap()).unwrap();
    let env = &cfg["mcpServers"]["pawl"]["env"];
    assert_eq!(cfg["mcpServers"]["pawl"]["type"], "local");
    let url = env["PAWL_A2A_URL"].as_str().unwrap();
    assert_eq!(url, format!("http://127.0.0.1:{port}"));
    let mut b = Bridge::new(BridgeConfig::new(url, env["PAWL_A2A_TOKEN"].as_str().unwrap()).unwrap()).unwrap();
    let call = |name: &str| json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": {} } });
    let r = b.handle(call("pawl_status")).await.unwrap();
    assert_eq!(r["result"]["isError"], false, "{r}");
    assert!(r["result"]["content"][0]["text"].as_str().unwrap().starts_with("Nothing started through Pawl"), "{r}");
    let r = b.handle(call("pawl_agents")).await.unwrap();
    assert_eq!(r["result"]["isError"], false, "{r}");
    // A start in a folder Pawl does not know is Pawl's refusal, shown as a tool error.
    let r = b
        .handle(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "pawl_start", "arguments": { "agent": "nope", "folder": "/no/such/folder", "prompt": "hi" } } }))
        .await
        .unwrap();
    assert_eq!(r["result"]["isError"], true, "{r}");
    // Remove: the token stops working at once.
    ok(f.hub.handle(connect(HookTargetName::CopilotCli, HookAction::Remove), &me).await);
    let r = b.handle(call("pawl_status")).await.unwrap();
    assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("refused this agent's token"), "{r}");
    f.hub.stop_a2a_server().await;
}

#[test]
fn the_bridge_and_the_writers_agree_on_the_env_names() {
    assert_eq!(pawl_a2a::mcp::URL_ENV, pawl_discovery::mcp_config::URL_ENV);
    assert_eq!(pawl_a2a::mcp::TOKEN_ENV, pawl_discovery::mcp_config::TOKEN_ENV);
    assert_eq!(pawl_discovery::mcp_config::MCP_ARG, "mcp");
}

#[tokio::test]
async fn a_connected_agents_caller_can_never_be_given_approvals() {
    let (f, _) = fx_a2a().await;
    let me = desktop();
    ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Apply), &me).await);
    let row = f.hub.settings().a2a.callers.into_iter().find(|c| c.id == "claude-code").unwrap();
    assert_eq!(row.connect_target, Some(HookTargetName::Claude));
    assert_eq!(serde_json::to_value(&row).unwrap()["connectTarget"], "claude");
    assert!(config_text(&f.hub).contains("connect_target = \"claude\""), "{}", config_text(&f.hub));
    let token = caller_token(&f.hub, "claude-code");
    // The UI sends the callers list back: approvals on is refused, nothing changes.
    let e = err(update(&f.hub, json!({ "a2a": { "callers": [{ "id": "claude-code", "name": "Claude Code", "allowApprovals": true }] } })).await);
    assert_eq!(e, "invalid:a2a.callers[0].allowApprovals: A coding agent connected by Pawl can never answer approvals.");
    assert!(!f.hub.config().a2a.callers[0].allow_approvals);
    // Other edits keep the mark and the token; a caller of the user's own may have approvals.
    ok(update(&f.hub, json!({ "a2a": { "callers": [
        { "id": "claude-code", "name": "Claude (laptop)", "allowApprovals": false },
        { "id": "ops", "name": "Ops", "allowApprovals": true }
    ] } })).await);
    let cfg = f.hub.config();
    assert_eq!((cfg.a2a.callers[0].name.as_str(), cfg.a2a.callers[0].connect_target), ("Claude (laptop)", Some(crate::config::ConnectTarget::Claude)));
    assert_eq!(caller_token(&f.hub, "claude-code"), token);
    assert!(cfg.a2a.callers[1].allow_approvals && cfg.a2a.callers[1].connect_target.is_none());
    // Removed and added again by hand under the same id: an ordinary caller again.
    ok(update(&f.hub, json!({ "a2a": { "callers": [] } })).await);
    ok(update(&f.hub, json!({ "a2a": { "callers": [{ "id": "claude-code", "name": "Mine", "allowApprovals": true }] } })).await);
    assert!(f.hub.config().a2a.callers[0].connect_target.is_none());
    f.hub.stop_a2a_server().await;
}

#[test]
fn may_answer_approvals_needs_no_connect_mark() {
    let mut c = crate::config::A2aCaller { id: "x".into(), name: "X".into(), token: None, allow_approvals: true, connect_target: None, connect_added_servers: false };
    assert!(c.may_answer_approvals());
    c.connect_target = Some(crate::config::ConnectTarget::CopilotCli);
    assert!(!c.may_answer_approvals());
    assert!(format!("{c:?}").contains("CopilotCli"));
}

// ---------------------------------------------------------------- review round 2

#[tokio::test]
async fn remove_revokes_the_token_even_when_the_agents_file_is_broken() {
    let (f, _) = fx_a2a().await;
    let me = desktop();
    ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Apply), &me).await);
    // The agent's own file breaks (or the agent breaks it on purpose).
    let path = claude_json(&f);
    let broken = std::fs::read_to_string(&path).unwrap().replacen("{", "{,", 1);
    std::fs::write(&path, &broken).unwrap();
    let v = ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Remove), &me).await);
    assert_eq!(v["applied"], true, "{v}");
    let w = v["warning"].as_str().unwrap();
    assert!(w.starts_with(&format!("Token revoked. Take the pawl entry out of {} by hand", path.display())), "{w}");
    assert!(f.hub.config().a2a.callers.is_empty(), "the token stopped working");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), broken, "the file is left alone");
    assert_eq!(connect_rows(&f.hub)[0], (vec!["a2a.connect.claude".to_string()], Some("remove".to_string())));
    // Nothing to revoke and a broken file: an error, nothing changed.
    let e = err(f.hub.handle(connect(HookTargetName::Claude, HookAction::Remove), &me).await);
    assert!(e.contains("is not valid JSON"), "{e}");
    f.hub.stop_a2a_server().await;
}

#[tokio::test]
async fn the_ack_says_whether_a_token_is_made_and_a_stopped_server_is_named() {
    let (f, _) = fx_a2a().await;
    let me = desktop();
    let v = ok(f.hub.handle(connect(HookTargetName::CopilotVscode, HookAction::Preview), &me).await);
    assert_eq!(v["newToken"], true);
    let plan = v["plan"].as_str().unwrap();
    assert!(plan.contains("only as contained as your OS user") && plan.contains("Settings Sync"), "{plan}");
    assert!(v.get("warning").is_none());
    ok(f.hub.handle(connect(HookTargetName::CopilotVscode, HookAction::Apply), &me).await);
    let v = ok(f.hub.handle(connect(HookTargetName::CopilotVscode, HookAction::Apply), &me).await);
    assert_eq!((v["applied"].as_bool(), v["newToken"].as_bool()), (Some(false), Some(false)));
    // The server turned off by hand: the entry is current, Connect only turns it back on.
    ok(update(&f.hub, json!({ "a2a": { "server": false } })).await);
    let st = status(&ok(f.hub.handle(connect(HookTargetName::CopilotVscode, HookAction::Preview), &me).await));
    assert_eq!(st.state, A2aConnectState::Outdated);
    assert!(st.detail.unwrap().contains("A2A server is off"));
    let v = ok(f.hub.handle(connect(HookTargetName::CopilotVscode, HookAction::Preview), &me).await);
    let plan = v["plan"].as_str().unwrap();
    assert_eq!((v["startsServer"].as_bool(), v["newToken"].as_bool()), (Some(true), Some(false)), "{plan}");
    assert!(plan.contains("Turns on Pawl's A2A server") && plan.contains("no new token") && plan.contains("up to date; nothing written"), "{plan}");
    let token = caller_token(&f.hub, "copilot-vscode");
    let v = ok(f.hub.handle(connect(HookTargetName::CopilotVscode, HookAction::Apply), &me).await);
    assert_eq!((v["applied"].as_bool(), v["newToken"].as_bool(), v["status"]["state"].as_str()), (Some(true), Some(false), Some("connected")));
    assert_eq!(caller_token(&f.hub, "copilot-vscode"), token);
    f.hub.stop_a2a_server().await;
}

#[tokio::test]
async fn a_server_that_could_not_start_is_named_in_the_status() {
    let port = free_port();
    let _taken = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    let f = fx_with(|c| {
        c.a2a.port = port;
        c.a2a.server = true;
    })
    .await;
    f.hub.sync_a2a_server().await;
    assert!(!f.hub.a2a_status().running);
    let v = ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Apply), &desktop()).await);
    let st = status(&v);
    assert_eq!(st.state, A2aConnectState::Connected);
    assert!(st.detail.as_deref().unwrap_or("").starts_with("Pawl's A2A server is not running"), "{st:?}");
}

#[tokio::test]
async fn a_hand_edited_marked_caller_is_shown_and_treated_as_never() {
    let port = free_port();
    let f = fx_with(|c| {
        c.a2a.port = port;
        c.a2a.callers.push(crate::config::A2aCaller {
            id: "codex".into(),
            name: "Codex".into(),
            token: Some("b".repeat(64)),
            allow_approvals: true,
            connect_target: Some(crate::config::ConnectTarget::Codex),
            connect_added_servers: false,
        });
    })
    .await;
    let row = f.hub.settings().a2a.callers.into_iter().next().unwrap();
    assert!(!row.allow_approvals, "reported as the server goes by it");
    // The UI echoes the row back: accepted (not refused because of the hand edit); approvals on is not.
    ok(update(&f.hub, json!({ "a2a": { "callers": [{ "id": "codex", "name": "Codex", "allowApprovals": false }] } })).await);
    assert!(!f.hub.settings().a2a.callers[0].allow_approvals);
    let e = err(update(&f.hub, json!({ "a2a": { "callers": [{ "id": "codex", "name": "Codex", "allowApprovals": true }] } })).await);
    assert!(e.ends_with("A coding agent connected by Pawl can never answer approvals."), "{e}");
}

// ---------------------------------------------------------------- review round 3

#[tokio::test]
async fn remove_leaves_a_hand_made_caller_with_that_id_alone() {
    let port = free_port();
    let f = fx_with(|c| {
        c.a2a.port = port;
        c.a2a.server = true;
        c.a2a.callers.push(crate::config::A2aCaller { id: "codex".into(), name: "CI bot".into(), token: Some("c".repeat(64)), allow_approvals: false, connect_target: None, connect_added_servers: false });
    })
    .await;
    let me = desktop();
    let row = |v: &Value| status(v);
    let v = ok(f.hub.handle(ClientMsg::A2aConnectStatus { id: "s".into() }, &me).await);
    let codex: A2aConnectStatus = serde_json::from_value(v["targets"][1].clone()).unwrap();
    assert_eq!((codex.state, codex.caller_exists, codex.caller_marked), (A2aConnectState::Missing, true, false));
    // A missing row: Remove changes nothing and says why the caller stays.
    let v = ok(f.hub.handle(connect(HookTargetName::Codex, HookAction::Remove), &me).await);
    assert_eq!(v["applied"], false);
    assert!(v["plan"].as_str().unwrap().contains("Leaves the A2A caller \"codex\" alone: you added it"), "{v}");
    assert_eq!(caller_token(&f.hub, "codex"), Some("c".repeat(64)), "the CI bot keeps working");
    assert!(connect_rows(&f.hub).is_empty());
    // A pawl entry the user wrote by hand: only the entry goes.
    let cfg = f.dir.path().join("home").join(".codex").join("config.toml");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, "[mcp_servers.pawl]\ncommand = \"pawl\"\n").unwrap();
    let v = ok(f.hub.handle(connect(HookTargetName::Codex, HookAction::Remove), &me).await);
    assert_eq!(v["applied"], true);
    assert!(v["plan"].as_str().unwrap().contains("Leaves the A2A caller \"codex\" alone"), "{v}");
    assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "");
    assert_eq!(caller_token(&f.hub, "codex"), Some("c".repeat(64)));
    let st = row(&v);
    assert_eq!((st.caller_exists, st.caller_marked), (true, false));
    // Connect takes it over (marked); now Remove deletes it.
    let v = ok(f.hub.handle(connect(HookTargetName::Codex, HookAction::Apply), &me).await);
    assert!(row(&v).caller_marked);
    ok(f.hub.handle(connect(HookTargetName::Codex, HookAction::Remove), &me).await);
    assert!(f.hub.config().a2a.callers.is_empty());
    f.hub.stop_a2a_server().await;
}

#[tokio::test]
async fn remove_takes_away_only_a_servers_object_connect_added() {
    let (f, _) = fx_a2a().await;
    let me = desktop();
    // The user's own empty mcpServers stays, byte for byte.
    let path = claude_json(&f);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let old = "{\"a\":1,\"mcpServers\":{}}";
    std::fs::write(&path, old).unwrap();
    ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Apply), &me).await);
    assert!(!f.hub.config().a2a.callers[0].connect_added_servers);
    ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Remove), &me).await);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
    // No mcpServers before: Connect adds it, records that, and Remove takes it away again.
    let old = "{\"a\":1}";
    std::fs::write(&path, old).unwrap();
    ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Apply), &me).await);
    assert!(f.hub.config().a2a.callers[0].connect_added_servers);
    assert!(config_text(&f.hub).contains("connect_added_servers = true"), "{}", config_text(&f.hub));
    // A reconnect (new token) keeps the record.
    f.hub.a2a_rotate_token("claude-code", &me).await.unwrap();
    ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Apply), &me).await);
    assert!(f.hub.config().a2a.callers[0].connect_added_servers);
    ok(f.hub.handle(connect(HookTargetName::Claude, HookAction::Remove), &me).await);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), old);
    f.hub.stop_a2a_server().await;
}
