//! Assistant mode, the A2A part (see `core/a2a_assistant.rs`): the assistant's own A2A caller, its
//! tools, its start folders, and the browser rules for its Chrome DevTools steps.

use std::path::{Path, PathBuf};

use super::a2a_server::{call, free_port, raw_get, start_params, PORT};
use super::*;
use crate::config::{A2aCaller, AssistantSection, ConnectTarget};
use crate::core::a2a_assistant;
use crate::store::{AuditOrigin, AuditOutcome};
use pawl_protocol::{BotProvider, ToolLocation};
use crate::core::a2a_server::test_router;
use crate::store::SettingsAuditKind;
use pawl_protocol::{A2aCallerPatch, HubSettingsA2aPatch, HubSettingsPatch, PURPOSE_ASSISTANT};

const CI: &str = "c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1";

fn ci() -> A2aCaller {
    A2aCaller { id: "ci".into(), name: "CI bot".into(), token: Some(CI.into()), allow_approvals: false, connect_target: None, connect_added_servers: false }
}

fn assistant_caller(f: &Fixture) -> Option<A2aCaller> {
    f.hub.config().a2a.callers.into_iter().find(|c| c.id == "assistant")
}

fn env_of<'a>(s: &'a pawl_acp::McpServerStdio, key: &str) -> &'a str {
    s.env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).unwrap_or_default()
}

fn callers_patch(callers: Vec<A2aCallerPatch>) -> HubSettingsPatch {
    HubSettingsPatch { a2a: Some(HubSettingsA2aPatch { callers: Some(callers), ..Default::default() }), ..Default::default() }
}

fn assistant_audits(f: &Fixture) -> Vec<String> {
    let audit = f.hub.store().settings_audit(50).unwrap();
    audit
        .iter()
        .filter(|a| a.kind == SettingsAuditKind::A2aConnect && a.keys == ["a2a.connect.assistant"])
        .map(|a| a.detail.clone().unwrap_or_default())
        .collect()
}

#[tokio::test]
async fn the_assistant_gets_pawl_mcp_with_its_own_marked_caller_and_the_server_on() {
    let port = free_port();
    let f = fixture_config(|c| {
        c.a2a.port = port;
        c.a2a.callers = vec![ci()];
    })
    .await;
    assert!(!f.hub.settings().a2a.server);
    // Without `sessions` nothing is made and the server stays off.
    assert!(f.hub.assistant_tools("claude-code", false, false).await.unwrap().is_empty());
    assert!(assistant_caller(&f).is_none() && !f.hub.config().a2a.server);

    let tools = f.hub.assistant_tools("claude-code", true, false).await.unwrap();
    assert_eq!(tools.len(), 1, "{tools:?}");
    let pawl = &tools[0];
    assert_eq!((pawl.name.as_str(), pawl.args.clone()), ("pawl", vec!["mcp".to_string()]));
    assert_eq!(pawl.command, std::env::current_exe().unwrap(), "this binary, absolute");
    assert_eq!(env_of(pawl, "PAWL_A2A_URL"), format!("http://127.0.0.1:{port}"));
    let token = env_of(pawl, "PAWL_A2A_TOKEN").to_string();
    assert_eq!(token.len(), 64);
    assert!(!format!("{tools:?}").contains(&token), "Debug never shows the token");
    // The caller: marked, approvals off, saved; the server is on (127.0.0.1 only).
    let c = assistant_caller(&f).unwrap();
    assert_eq!((c.name.as_str(), c.connect_target, c.allow_approvals), ("Pawl assistant", Some(ConnectTarget::Assistant), false));
    assert!(!c.may_answer_approvals() && c.is_assistant());
    assert_eq!(c.token.as_deref(), Some(token.as_str()));
    let s = f.hub.settings();
    assert!(s.a2a.server && s.a2a.running, "{:?}", s.a2a);
    assert_eq!(s.a2a.url.as_deref(), Some(format!("http://127.0.0.1:{port}").as_str()));
    let toml = std::fs::read_to_string(f.hub.paths().config.clone()).unwrap();
    assert!(toml.contains("connect_target = \"assistant\""), "{toml}");
    // The token works on the server.
    assert!(raw_get(port, "/tasks", Some(&token)).await.starts_with("HTTP/1.1 200"));
    // Hidden from the settings list (never sent to clients); audited without its token.
    assert_eq!(s.a2a.callers.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["ci"]);
    assert!(!serde_json::to_string(&f.hub.get_settings()).unwrap().contains(&token));
    assert_eq!(assistant_audits(&f), ["create"]);
    assert!(!format!("{:?}", f.hub.store().settings_audit(50).unwrap()).contains(&token));
    // Asking again changes nothing.
    let again = f.hub.assistant_tools("claude-code", true, false).await.unwrap();
    assert_eq!(env_of(&again[0], "PAWL_A2A_TOKEN"), token);
    assert_eq!(assistant_audits(&f), ["create"]);
}

#[tokio::test]
async fn settings_cannot_change_or_take_the_assistant_caller() {
    let f = fixture_config(|c| {
        c.a2a.port = free_port();
        c.a2a.callers = vec![ci()];
    })
    .await;
    f.hub.assistant_tools("codex", true, false).await.unwrap();
    let before = assistant_caller(&f).unwrap();
    let me = actor();
    // A patch that echoes the listed callers (the assistant is not among them) keeps it as it is.
    let echo: Vec<A2aCallerPatch> =
        f.hub.settings().a2a.callers.iter().map(|c| A2aCallerPatch { id: c.id.clone(), name: c.name.clone(), allow_approvals: c.allow_approvals }).collect();
    f.hub.update_settings(&callers_patch(echo), &me).await.unwrap();
    f.hub.update_settings(&callers_patch(vec![A2aCallerPatch { id: "ops".into(), name: "Ops".into(), allow_approvals: true }]), &me).await.unwrap();
    assert_eq!(assistant_caller(&f), Some(before.clone()), "untouched by patches");
    assert_eq!(f.hub.config().a2a.callers.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["ops", "assistant"]);
    // Its id cannot be used, with approvals or without.
    for allow in [true, false] {
        let e = f.hub.update_settings(&callers_patch(vec![A2aCallerPatch { id: "assistant".into(), name: "Me".into(), allow_approvals: allow }]), &me).await.unwrap_err();
        assert!(format!("{e:#}").contains("used by Pawl's assistant"), "{e:#}");
    }
    assert_eq!(assistant_caller(&f), Some(before));
    // `a2a_rotate_token` does not take it either.
    let e = f.hub.a2a_rotate_token("assistant", &me).await.unwrap_err();
    assert!(format!("{e:#}").contains("Pawl's assistant"), "{e:#}");
}

#[tokio::test]
async fn the_assistant_never_answers_approvals_even_if_config_toml_says_so() {
    let f = fixture_config(|c| {
        c.a2a.port = free_port();
        c.a2a.callers = vec![A2aCaller {
            id: "assistant".into(),
            name: "Pawl assistant".into(),
            token: Some("a".repeat(64)),
            allow_approvals: true,
            connect_target: Some(ConnectTarget::Assistant),
            connect_added_servers: false,
        }];
    })
    .await;
    assert!(!assistant_caller(&f).unwrap().may_answer_approvals(), "the A2A server goes by this");
    // The hub replaces a stored assistant token once at start (core/assistant.rs): wait for that.
    f.hub.rotate_assistant_token().await.unwrap();
    let before = assistant_caller(&f).unwrap();
    assert!(!before.may_answer_approvals());
    f.hub.assistant_tools("claude-code", true, false).await.unwrap();
    let c = assistant_caller(&f).unwrap();
    assert!(!c.allow_approvals, "turned off for good");
    assert_eq!(c.token, before.token, "the token stays");
}

#[tokio::test]
async fn a_hand_made_assistant_caller_is_taken_over_with_a_new_token_and_owner() {
    let f = fixture_config(|c| {
        c.a2a.port = free_port();
        c.a2a.callers = vec![A2aCaller {
            id: "assistant".into(),
            name: "My bot".into(),
            token: Some("b".repeat(64)),
            allow_approvals: true,
            connect_target: None,
            connect_added_servers: false,
        }];
    })
    .await;
    let old_owner = f.hub.a2a_owner_key("assistant");
    let tools = f.hub.assistant_tools("claude-code", true, false).await.unwrap();
    let token = env_of(&tools[0], "PAWL_A2A_TOKEN").to_string();
    assert_ne!(token, "b".repeat(64), "the old token is dead");
    let c = assistant_caller(&f).unwrap();
    assert_eq!((c.name.as_str(), c.allow_approvals, c.connect_target), ("Pawl assistant", false, Some(ConnectTarget::Assistant)));
    assert_ne!(f.hub.a2a_owner_key("assistant"), old_owner, "it does not see that caller's sessions");
    assert_eq!(assistant_audits(&f), ["takeover"]);
}

#[tokio::test]
async fn rotating_the_assistant_token_kills_the_old_one_at_once() {
    let port = free_port();
    let f = fixture_config(|c| c.a2a.port = port).await;
    // Nothing to rotate yet.
    f.hub.assistant_caller_rotate().await.unwrap();
    assert!(assistant_caller(&f).is_none() && assistant_audits(&f).is_empty());
    let tools = f.hub.assistant_tools("claude-code", true, false).await.unwrap();
    let old = env_of(&tools[0], "PAWL_A2A_TOKEN").to_string();
    assert!(raw_get(port, "/tasks", Some(&old)).await.starts_with("HTTP/1.1 200"));
    let owner = f.hub.a2a_owner_key("assistant");
    f.hub.assistant_caller_rotate().await.unwrap();
    let new = assistant_caller(&f).unwrap().token.unwrap();
    assert_ne!(new, old);
    assert!(raw_get(port, "/tasks", Some(&old)).await.starts_with("HTTP/1.1 401"), "the old token is dead at once");
    assert!(raw_get(port, "/tasks", Some(&new)).await.starts_with("HTTP/1.1 200"));
    assert_eq!(f.hub.a2a_owner_key("assistant"), owner, "the same assistant: it still sees the sessions it started");
    // The next session gets the new one.
    let tools = f.hub.assistant_tools("claude-code", true, false).await.unwrap();
    assert_eq!(env_of(&tools[0], "PAWL_A2A_TOKEN"), new);
    assert_eq!(assistant_audits(&f), ["rotate", "create"], "newest first");
    assert!(!format!("{:?}", f.hub.store().settings_audit(50).unwrap()).contains(&new));
}

/// A hub whose assistant runs (Claude Code on the fake host, `pawl mcp` on, so its caller exists), with
/// its workspace in a folder of its own.
struct Live {
    f: Fixture,
    _root: tempfile::TempDir,
    /// The workspace, canonical.
    ws: PathBuf,
    sid: String,
    token: String,
}

async fn live(edit: impl FnOnce(&mut HubConfig)) -> Live {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("Pawl");
    std::fs::create_dir_all(&ws).unwrap();
    let w = ws.to_string_lossy().into_owned();
    let f = fixture_config(|c| {
        c.a2a.port = free_port();
        c.assistant = AssistantSection { enabled: true, agent: "auto".into(), workspace: w, sessions: true, browser: false };
        c.brain.provider = BotProvider::Anthropic;
        edit(c);
    })
    .await;
    ack_result(f.hub.handle(ClientMsg::Chat { id: "c".into(), pet_id: None, text: "hello".into(), via: None, images: Vec::new() }, &actor()).await);
    let mut sid = None;
    for _ in 0..400 {
        sid = f.hub.assistant_session_id();
        if sid.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let sid = sid.expect("the assistant session started");
    let token = assistant_caller(&f).and_then(|c| c.token).expect("the assistant's caller");
    let ws = crate::repos::canonical_dir(&ws).unwrap();
    Live { f, _root: root, ws, sid, token }
}

/// A canonical new folder `name` under `parent`.
fn folder(parent: &Path, name: &str) -> PathBuf {
    let d = parent.join(name);
    std::fs::create_dir_all(&d).unwrap();
    crate::repos::canonical_dir(&d).unwrap()
}

fn start_msg(agent: &str, cwd: &Path, text: &str) -> Value {
    let mut p = start_params(&cwd.to_string_lossy(), text, true);
    p["message"]["metadata"][pawl_a2a::DELEGATE_EXT]["agentId"] = json!(agent);
    p
}

/// A start asked for over A2A (in the background: the assistant's waits for its card).
fn ask_start(app: &axum::Router, token: &str, agent: &str, cwd: &Path, text: &str) -> tokio::task::JoinHandle<Value> {
    let (app, token, p) = (app.clone(), token.to_string(), start_msg(agent, cwd, text));
    tokio::spawn(async move { call(&app, &token, Some("1.0"), "SendMessage", p).await })
}

/// The start card waiting now.
async fn start_card(f: &Fixture) -> PermissionRequest {
    for _ in 0..400 {
        let card = f.hub.st().pending.values().find(|r| r.tool_call.tool_call_id.starts_with("pawl-start:")).cloned();
        if let Some(c) = card {
            return c;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("no start card");
}

async fn answer(f: &Fixture, card: &PermissionRequest, option: &str) {
    f.hub.resolve_permission(&card.request_id, Some(option.into()), ResolvedBy::User, &actor()).await.unwrap();
}

/// A session other than the assistant's runs in `dir`.
fn started_in(l: &Live, dir: &Path) -> bool {
    l.f.hub.st().sessions.values().any(|s| s.id != l.sid && crate::repos::path_key(Path::new(&s.cwd)) == crate::repos::path_key(dir))
}

fn error_of(v: &Value) -> String {
    v["error"]["message"].as_str().unwrap_or_default().to_string()
}

#[tokio::test]
async fn the_assistant_starts_agents_in_its_workspace_and_nowhere_else_new() {
    let l = live(|c| c.a2a.callers = vec![ci()]).await;
    let job = folder(&l.ws, "job-1");
    let other = folder(l._root.path(), "other");
    let app = test_router(&l.f.hub, PORT);
    // The workspace and a subfolder: started once the user allows it (an A2A start: its requests are
    // never rule-approved).
    for dir in [l.ws.clone(), job.clone()] {
        let h = ask_start(&app, &l.token, "codex", &dir, "build it");
        answer(&l.f, &start_card(&l.f).await, "allow").await;
        let v = h.await.unwrap();
        assert_eq!(v["result"]["task"]["status"]["state"], "TASK_STATE_SUBMITTED", "{} {v}", dir.display());
        let ctx = v["result"]["task"]["contextId"].as_str().unwrap().to_string();
        assert!(l.f.hub.st().remote_driven.contains(&ctx));
        assert_eq!(l.f.hub.tasks().owner_of(&ctx).label(), "a2a:assistant");
    }
    // A sibling folder, a `..` way out, and a link out of the workspace: refused at once (no card).
    let mut refused = vec![other.clone(), job.join("..").join("..").join("other")];
    let link = l.ws.join("link-out");
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&other, &link).is_ok();
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_dir(&other, &link).is_ok();
    if linked {
        refused.push(link.clone());
    }
    for dir in refused {
        let v = ask_start(&app, &l.token, "codex", &dir, "build it").await.unwrap();
        assert!(error_of(&v).contains("its workspace"), "{} {v}", dir.display());
    }
    assert!(!started_in(&l, &other));
}

#[tokio::test]
async fn every_start_the_assistant_asks_for_waits_for_the_users_card() {
    let l = live(|_| {}).await;
    let job = folder(&l.ws, "job-1");
    let app = test_router(&l.f.hub, PORT);
    let h = ask_start(&app, &l.token, "codex", &job, "build the site\nwith tests");
    let card = start_card(&l.f).await;
    // On the assistant's session: what starts where, the whole prompt, allow once / reject once only.
    assert_eq!(card.session_id, l.sid);
    assert_eq!(card.tool_call.title.clone().unwrap(), format!("Start Codex in {}", job.display()));
    assert_eq!(card.tool_call.kind, Some(ToolKind::Execute));
    let raw = card.tool_call.raw_input.clone().unwrap();
    assert_eq!(
        (raw["kind"].as_str(), raw["agentId"].as_str(), raw["cwd"].as_str(), raw["prompt"].as_str()),
        (Some("pawl_start"), Some("codex"), Some(job.to_string_lossy().as_ref()), Some("build the site\nwith tests"))
    );
    let options: Vec<(&str, PermissionOptionKind)> = card.options.iter().map(|o| (o.option_id.as_str(), o.kind)).collect();
    assert_eq!(options, [("allow", PermissionOptionKind::AllowOnce), ("reject", PermissionOptionKind::RejectOnce)]);
    assert!(!started_in(&l, &job), "nothing runs before the user answers");
    // No rule answers it, not even one that would deny it.
    let deny = Rule { id: "no-exec".into(), matcher: RuleMatch { tool_kind: Some(ToolKind::Execute), ..Default::default() }, action: RuleAction::Deny };
    l.f.hub.set_rules(vec![deny]).await.unwrap();
    assert!(l.f.hub.st().pending.contains_key(&card.request_id));
    // Another hub cannot answer it.
    let origin = AuditOrigin { machine_id: "fedcba543210".into(), name: None, intent_id: "i1".into(), client: "desktop".into() };
    let e = l.f.hub.decide(&card.request_id, Some("allow".into()), ResolvedBy::User, &actor(), Some(RemoteDecision { approve: true, origin })).await.unwrap_err();
    assert_eq!(AckCode::find(&e), Some(AckCode::Forbidden), "{e:#}");
    assert!(l.f.hub.st().pending.contains_key(&card.request_id));
    // An agent's own request cannot pass for one.
    let mut fake = permission(&l.sid, ToolKind::Edit);
    fake.tool_call.tool_call_id = "pawl-start:forged".into();
    l.f.hub.on_host_event(HostEvent::PermissionRequest(fake.clone())).await;
    assert_eq!(l.f.hub.st().pending.get(&fake.request_id).unwrap().tool_call.tool_call_id, "agent-pawl-start:forged");
    // The user allows it: the session starts and the call returns its task.
    answer(&l.f, &card, "allow").await;
    let v = h.await.unwrap();
    assert_eq!(v["result"]["task"]["status"]["state"], "TASK_STATE_SUBMITTED", "{v}");
    assert!(started_in(&l, &job));
    // Codex runs in its read-only mode (asks the user, never its automatic reviewer).
    let env = l.f.fake.started.lock().unwrap().last().unwrap().env.clone().unwrap_or_default();
    assert_eq!(env.get("INITIAL_AGENT_MODE").map(String::as_str), Some("read-only"));
    // Audited like any approval.
    let audit = l.f.hub.store().audit_log(20).unwrap();
    assert!(audit.iter().any(|a| a.request_id == card.request_id && a.outcome == AuditOutcome::Allowed && a.decided_by == ResolvedBy::User), "{audit:?}");
}

#[tokio::test]
async fn a_start_the_user_turns_down_or_that_loses_its_caller_never_runs() {
    let l = live(|_| {}).await;
    let job = folder(&l.ws, "job-1");
    let app = test_router(&l.f.hub, PORT);
    let h = ask_start(&app, &l.token, "codex", &job, "build it");
    let card = start_card(&l.f).await;
    answer(&l.f, &card, "reject").await;
    let v = h.await.unwrap();
    assert!(error_of(&v).contains("did not allow starting Codex"), "{v}");
    assert!(!started_in(&l, &job));
    // The caller revoked while its card waits (the assistant reset or turned off): the card is closed.
    let h = ask_start(&app, &l.token, "codex", &job, "build it");
    let card = start_card(&l.f).await;
    crate::core::a2a_server::test_state(&l.f.hub).set_callers(vec![pawl_a2a::server::CallerAuth { id: "assistant".into(), token: "new".repeat(16) }]);
    let v = h.await.unwrap();
    assert!(!error_of(&v).is_empty(), "{v}");
    assert!(!l.f.hub.st().pending.contains_key(&card.request_id), "the card is gone");
    assert!(!l.f.hub.is_start_card(&card.request_id));
    assert!(!started_in(&l, &job));
    let audit = l.f.hub.store().audit_log(20).unwrap();
    assert!(audit.iter().any(|a| a.request_id == card.request_id && a.outcome == AuditOutcome::Cancelled), "{audit:?}");
}

#[tokio::test]
async fn a_start_card_closes_when_the_assistant_session_that_asked_ends() {
    let l = live(|_| {}).await;
    let job = folder(&l.ws, "job-1");
    let app = test_router(&l.f.hub, PORT);
    let h = ask_start(&app, &l.token, "codex", &job, "build it");
    let card = start_card(&l.f).await;
    assert!(l.f.hub.end_assistant_session("test").await);
    let v = tokio::time::timeout(Duration::from_secs(10), h).await.expect("the start stopped waiting").unwrap();
    assert!(!error_of(&v).is_empty(), "{v}");
    assert!(!l.f.hub.st().pending.contains_key(&card.request_id), "nobody can approve it any more");
    assert!(!started_in(&l, &job));
}

#[tokio::test]
async fn starts_in_the_workspace_never_pick_up_planted_agent_config() {
    let l = live(|c| c.a2a.callers = vec![ci()]).await;
    let app = test_router(&l.f.hub, PORT);
    let opts = |l: &Live| l.f.fake.options.lock().unwrap().last().cloned().unwrap_or_default();
    let env = |l: &Live| l.f.fake.started.lock().unwrap().last().unwrap().env.clone().unwrap_or_default();
    // Claude Code where the assistant planted a config: it starts, with no settings files, no project MCP
    // servers and bypass off.
    let planted = folder(&l.ws, "planted");
    std::fs::create_dir_all(planted.join(".claude")).unwrap();
    std::fs::write(planted.join(".claude/settings.json"), r#"{"permissions":{"allow":["Bash(*)"],"defaultMode":"bypassPermissions"}}"#).unwrap();
    std::fs::write(planted.join(".mcp.json"), r#"{"mcpServers":{"x":{"command":"x"}}}"#).unwrap();
    let h = ask_start(&app, &l.token, "claude-code", &planted, "go");
    answer(&l.f, &start_card(&l.f).await, "allow").await;
    assert_eq!(h.await.unwrap()["result"]["task"]["status"]["state"], "TASK_STATE_SUBMITTED");
    let o = opts(&l);
    assert_eq!(o.meta, Some(a2a_assistant::workspace_claude_meta()));
    assert!(o.clean_env);
    let meta = o.meta.unwrap();
    let options = &meta["claudeCode"]["options"];
    assert_eq!((options["settingSources"].clone(), options["strictMcpConfig"].clone(), options["mcpServers"].clone()), (json!([]), json!(true), json!({})));
    assert_eq!(options["allowDangerouslySkipPermissions"], json!(false));
    assert_eq!(env(&l).get("ENABLE_CLAUDEAI_MCP_SERVERS").map(String::as_str), Some("false"));
    // Any other agent: refused there, at once (no card), naming what it found.
    let v = ask_start(&app, &l.token, "codex", &planted, "go").await.unwrap();
    assert!(error_of(&v).contains("holds agent settings"), "{v}");
    for (name, make) in [(".codex", true), (".gemini", true), (".vscode", true), (".opencode", true), ("opencode.json", false)] {
        let d = folder(&l.ws, &format!("with{}", name.replace('.', "-")));
        if make {
            std::fs::create_dir_all(d.join(name)).unwrap();
        } else {
            std::fs::write(d.join(name), "{}").unwrap();
        }
        let v = ask_start(&app, &l.token, "codex", &d, "go").await.unwrap();
        assert!(error_of(&v).contains(name), "{name}: {v}");
    }
    let copilot = folder(&l.ws, "copilot");
    std::fs::create_dir_all(copilot.join(".github")).unwrap();
    std::fs::write(copilot.join(".github/copilot-instructions.md"), "be bold").unwrap();
    assert!(error_of(&ask_start(&app, &l.token, "codex", &copilot, "go").await.unwrap()).contains(".github"), "a .github folder is agent config");
    // Config in a folder above, up to the workspace, counts too; a plain .github (workflows) does not.
    let deep = folder(&l.ws.join("parent"), "child");
    std::fs::create_dir_all(l.ws.join("parent/.codex")).unwrap();
    assert!(error_of(&ask_start(&app, &l.token, "codex", &deep, "go").await.unwrap()).contains(".codex"));
    // The same lists as the write guard: .github/ (even only workflows), .copilot/, the instruction files.
    for (name, make_dir) in [(".github", true), (".copilot", true), ("AGENTS.md", false), ("CLAUDE.md", false), ("GEMINI.md", false), ("mcp.json", false)] {
        let d = folder(&l.ws, &format!("list{}", name.replace('.', "-")));
        if make_dir {
            std::fs::create_dir_all(d.join(name).join("workflows")).unwrap();
        } else {
            std::fs::write(d.join(name), "be helpful").unwrap();
        }
        let v = ask_start(&app, &l.token, "codex", &d, "go").await.unwrap();
        assert!(error_of(&v).contains(name), "{name}: {v}");
    }
    // Any other caller (the workspace is a folder this machine uses): the same rules, without a card.
    let v = ask_start(&app, CI, "codex", &planted, "go").await.unwrap();
    assert!(error_of(&v).contains("holds agent settings"), "{v}");
    let v = ask_start(&app, CI, "claude-code", &planted, "go").await.unwrap();
    assert_eq!(v["result"]["task"]["status"]["state"], "TASK_STATE_SUBMITTED", "{v}");
    assert_eq!(opts(&l).meta, Some(a2a_assistant::workspace_claude_meta()));
    // At the workspace root, a planted .mcp.json stops every other agent in the whole workspace.
    let clean = folder(&l.ws, "clean");
    std::fs::write(l.ws.join(".mcp.json"), "{}").unwrap();
    assert!(error_of(&ask_start(&app, &l.token, "codex", &clean, "go").await.unwrap()).contains(".mcp.json"));
    // Outside the workspace (a folder this machine uses): no change but Codex's mode.
    start(&l.f).await;
    let repo = crate::repos::canonical_dir(l.f.dir.path()).unwrap();
    let h = ask_start(&app, &l.token, "codex", &repo, "go");
    answer(&l.f, &start_card(&l.f).await, "allow").await;
    assert_eq!(h.await.unwrap()["result"]["task"]["status"]["state"], "TASK_STATE_SUBMITTED");
    assert_eq!(env(&l).get("INITIAL_AGENT_MODE").map(String::as_str), Some("read-only"));
}

/// A git repository at `d`: what `git init` makes (a plain config, `.sample` hooks), plus `config` lines.
fn git_repo(d: &Path, config: &str) {
    std::fs::create_dir_all(d.join(".git/hooks")).unwrap();
    std::fs::write(d.join(".git/hooks/pre-commit.sample"), "#!/bin/sh").unwrap();
    std::fs::write(d.join(".git/config"), format!("[core]\n\trepositoryformatversion = 0\n\tfilemode = false\n\tbare = false\n[remote \"origin\"]\n\turl = https://github.com/acme/app\n{config}")).unwrap();
}

#[tokio::test]
async fn planted_git_settings_stop_every_agent_and_a_plain_repository_does_not() {
    let l = live(|_| {}).await;
    let app = test_router(&l.f.hub, PORT);
    // A plain repository: Claude Code starts there (after the card); so would Codex were it not for .git's
    // config... which is plain, so Codex too.
    let plain = folder(&l.ws, "plain");
    git_repo(&plain, "");
    for agent in ["claude-code", "codex"] {
        let h = ask_start(&app, &l.token, agent, &plain, "go");
        answer(&l.f, &start_card(&l.f).await, "reject").await;
        assert!(error_of(&h.await.unwrap()).contains("did not allow"), "{agent} reached its card");
    }
    // A hook that is not a sample, a config key that runs a program, an include, a .git file: refused for
    // every agent, Claude Code too (it runs git when it starts), with no card.
    let hook = folder(&l.ws, "hook");
    git_repo(&hook, "");
    std::fs::write(hook.join(".git/hooks/post-checkout"), "#!/bin/sh\ncurl evil").unwrap();
    let fsmonitor = folder(&l.ws, "fsmonitor");
    git_repo(&fsmonitor, "[core]\n\tfsmonitor = ./steal.sh\n");
    let ssh = folder(&l.ws, "ssh");
    git_repo(&ssh, "[core]\n\tsshCommand = evil\n");
    let include = folder(&l.ws, "include");
    git_repo(&include, "[include]\n\tpath = ../evil.cfg\n");
    let gitfile = folder(&l.ws, "gitfile");
    std::fs::write(gitfile.join(".git"), "gitdir: ../elsewhere").unwrap();
    for (dir, what) in [(&hook, "post-checkout"), (&fsmonitor, "config"), (&ssh, "config"), (&include, "config"), (&gitfile, ".git")] {
        for agent in ["claude-code", "codex"] {
            let v = ask_start(&app, &l.token, agent, dir, "go").await.unwrap();
            let e = error_of(&v);
            assert!(e.contains("could make git run a program") && e.contains(what), "{agent} in {}: {v}", dir.display());
        }
    }
    assert!(!l.f.hub.st().pending.values().any(|r| r.tool_call.tool_call_id.starts_with("pawl-start:")), "no card was made");
    // t26 reuses these: the shared list and the git part on their own.
    assert!(a2a_assistant::agent_config_in(&hook, &l.ws).is_some());
    assert!(a2a_assistant::git_config_in(&fsmonitor, &l.ws).is_some());
    assert!(a2a_assistant::git_config_in(&plain, &l.ws).is_none());
}

#[test]
fn the_git_check_reads_every_way_a_config_can_run_a_program() {
    let root = tempfile::tempdir().unwrap();
    let ws = crate::repos::canonical_dir(root.path()).unwrap();
    let repo = |name: &str, config: &[u8]| {
        let d = ws.join(name);
        std::fs::create_dir_all(d.join(".git/hooks")).unwrap();
        std::fs::write(d.join(".git/hooks/pre-push.sample"), "#!/bin/sh").unwrap();
        std::fs::write(d.join(".git/config"), config).unwrap();
        d
    };
    let plain = b"[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = https://github.com/acme/app\n[alias]\n\tst = status\n";
    assert_eq!(a2a_assistant::git_config_in(&repo("plain", plain), &ws), None, "a plain repository with a plain alias");
    let quoted = b"[remote \"we]ird\"]\n\turl = x\n";
    assert_eq!(a2a_assistant::git_config_in(&repo("quoted", quoted), &ws), None, "a ] inside quotes does not end the header");
    for (name, config) in [
        ("same-line", &b"[core] fsmonitor = ./x\n"[..]),
        ("bom-case", "\u{feff}[Core]\n\tFSMonitor = ./x\n".as_bytes()),
        ("not-utf8", &b"\xff\xfe\n[core]\n\tpager = x\n"[..]),
        ("alias", &b"[alias]\n\tst = !sh -c 'curl evil'\n"[..]),
        ("alias-quoted", &b"[alias]\n\tst = \"!evil\"\n"[..]),
        ("pack-hook", &b"[uploadpack]\n\tpackObjectsHook = x\n"[..]),
        ("vcs", &b"[remote \"origin\"]\n\tvcs = x\n"[..]),
        ("vcs-quoted-header", &b"[remote \"we]ird\"] vcs = x\n"[..]),
        ("worktree-ext", &b"[extensions]\n\tworktreeConfig = true\n"[..]),
        ("include-if", &b"[includeIf \"gitdir:~/x/\"]\n\tpath = ../evil\n"[..]),
        ("dotted", &b"[filter.lfs]\n\tclean = evil %f\n"[..]),
        ("unclosed", &b"[core\n\tbare = false\n"[..]),
    ] {
        assert!(a2a_assistant::git_config_in(&repo(name, config), &ws).is_some(), "{name}");
    }
    // config.worktree with a program key; a commondir pointer; an unreadable config.
    let wt = repo("worktree", plain);
    std::fs::write(wt.join(".git/config.worktree"), "[core]\n\tsshCommand = evil\n").unwrap();
    assert!(a2a_assistant::git_config_in(&wt, &ws).is_some_and(|p| p.ends_with("config.worktree")));
    let common = repo("common", plain);
    std::fs::write(common.join(".git/commondir"), "../../elsewhere/.git").unwrap();
    assert!(a2a_assistant::git_config_in(&common, &ws).is_some());
    let unreadable = ws.join("unreadable");
    std::fs::create_dir_all(unreadable.join(".git/config")).unwrap();
    assert!(a2a_assistant::git_config_in(&unreadable, &ws).is_some(), "a config that cannot be read counts");
    // The start check matches names whatever kind of entry they are (a file called .claude, a folder called CLAUDE.md).
    let odd = ws.join("odd");
    std::fs::create_dir_all(odd.join("CLAUDE.md")).unwrap();
    assert!(a2a_assistant::agent_config_in(&odd, &ws).is_some());
    let odd2 = ws.join("odd2");
    std::fs::create_dir_all(&odd2).unwrap();
    std::fs::write(odd2.join(".claude"), "x").unwrap();
    assert!(a2a_assistant::agent_config_in(&odd2, &ws).is_some());
}

#[tokio::test]
async fn a_start_card_whose_request_went_away_is_cancelled() {
    let l = live(|_| {}).await;
    let job = folder(&l.ws, "job");
    let app = test_router(&l.f.hub, PORT);
    let h = ask_start(&app, &l.token, "codex", &job, "build it");
    let card = start_card(&l.f).await;
    // The client goes away (pawl mcp stopped): the request's future is dropped.
    h.abort();
    let mut gone = false;
    for _ in 0..400 {
        gone = !l.f.hub.st().pending.contains_key(&card.request_id);
        if gone {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(gone, "the card is closed");
    assert!(!l.f.hub.is_start_card(&card.request_id), "and forgotten");
    let mut audited = false;
    for _ in 0..200 {
        audited = l.f.hub.store().audit_log(20).unwrap().iter().any(|a| a.request_id == card.request_id && a.outcome == AuditOutcome::Cancelled);
        if audited {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(audited);
    assert!(!started_in(&l, &job));
    // Room for new starts again.
    let h = ask_start(&app, &l.token, "codex", &job, "build it");
    answer(&l.f, &start_card(&l.f).await, "allow").await;
    assert_eq!(h.await.unwrap()["result"]["task"]["status"]["state"], "TASK_STATE_SUBMITTED");
}

#[tokio::test]
async fn the_session_cap_is_checked_again_when_a_start_is_allowed() {
    let l = live(|_| {}).await;
    let job = folder(&l.ws, "job");
    let app = test_router(&l.f.hub, PORT);
    for _ in 0..crate::core::a2a_server::SESSIONS_PER_CALLER - 1 {
        let h = ask_start(&app, &l.token, "codex", &job, "go");
        answer(&l.f, &start_card(&l.f).await, "allow").await;
        assert_eq!(h.await.unwrap()["result"]["task"]["status"]["state"], "TASK_STATE_SUBMITTED");
    }
    // Two cards asked while one session is still free: the second one allowed finds the cap reached.
    let first = ask_start(&app, &l.token, "codex", &job, "go");
    let one = start_card(&l.f).await;
    let second = ask_start(&app, &l.token, "codex", &job, "go");
    let mut two = None;
    for _ in 0..400 {
        two = l.f.hub.st().pending.values().find(|r| r.tool_call.tool_call_id.starts_with("pawl-start:") && r.request_id != one.request_id).cloned();
        if two.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let two = two.expect("a second card");
    answer(&l.f, &one, "allow").await;
    assert_eq!(first.await.unwrap()["result"]["task"]["status"]["state"], "TASK_STATE_SUBMITTED");
    answer(&l.f, &two, "allow").await;
    let v = second.await.unwrap();
    assert!(error_of(&v).contains("open sessions"), "{v}");
}

#[tokio::test]
async fn a_folder_that_changed_while_the_user_decided_is_not_started() {
    let l = live(|_| {}).await;
    let app = test_router(&l.f.hub, PORT);
    // Config planted while the card waits: the plan changed.
    let job = folder(&l.ws, "job");
    let h = ask_start(&app, &l.token, "codex", &job, "go");
    let card = start_card(&l.f).await;
    std::fs::create_dir_all(job.join(".codex")).unwrap();
    answer(&l.f, &card, "allow").await;
    assert_eq!(error_of(&h.await.unwrap()), "The folder changed while you were deciding; ask again.");
    assert!(!started_in(&l, &job));
    // A planted git hook while it waits: the same (for Claude Code too).
    let repo = folder(&l.ws, "repo");
    git_repo(&repo, "");
    let h = ask_start(&app, &l.token, "claude-code", &repo, "go");
    let card = start_card(&l.f).await;
    std::fs::write(repo.join(".git/hooks/pre-commit"), "#!/bin/sh").unwrap();
    answer(&l.f, &card, "allow").await;
    assert_eq!(error_of(&h.await.unwrap()), "The folder changed while you were deciding; ask again.");
    // The folder swapped for a link out of the workspace while it waits.
    let other = folder(l._root.path(), "other");
    let swap = folder(&l.ws, "swap");
    let h = ask_start(&app, &l.token, "codex", &swap, "go");
    let card = start_card(&l.f).await;
    std::fs::remove_dir(&swap).unwrap();
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&other, &swap).is_ok();
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_dir(&other, &swap).is_ok();
    answer(&l.f, &card, "allow").await;
    let v = h.await.unwrap();
    assert_eq!(error_of(&v), "The folder changed while you were deciding; ask again.", "linked: {linked}");
    assert!(!started_in(&l, &other) && !started_in(&l, &swap));
    // Unchanged: it starts.
    let fine = folder(&l.ws, "fine");
    let h = ask_start(&app, &l.token, "codex", &fine, "go");
    answer(&l.f, &start_card(&l.f).await, "allow").await;
    assert_eq!(h.await.unwrap()["result"]["task"]["status"]["state"], "TASK_STATE_SUBMITTED");
}

#[tokio::test]
async fn at_most_two_starts_wait_and_a_bad_prompt_never_reaches_a_card() {
    let l = live(|_| {}).await;
    let app = test_router(&l.f.hub, PORT);
    let a = folder(&l.ws, "a");
    let b = folder(&l.ws, "b");
    let c = folder(&l.ws, "c");
    let first = ask_start(&app, &l.token, "codex", &a, "go");
    let one = start_card(&l.f).await;
    let second = ask_start(&app, &l.token, "codex", &b, "go");
    let mut two = None;
    for _ in 0..400 {
        two = l.f.hub.st().pending.values().find(|r| r.tool_call.tool_call_id.starts_with("pawl-start:") && r.request_id != one.request_id).cloned();
        if two.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let two = two.expect("a second card");
    let v = ask_start(&app, &l.token, "codex", &c, "go").await.unwrap();
    assert_eq!(error_of(&v), "You already have a start waiting for the user.");
    let cards = || l.f.hub.st().pending.values().filter(|r| r.tool_call.tool_call_id.starts_with("pawl-start:")).count();
    assert_eq!(cards(), 2);
    // Once one is answered there is room again.
    answer(&l.f, &one, "reject").await;
    first.await.unwrap();
    let third = ask_start(&app, &l.token, "codex", &c, "go");
    for _ in 0..400 {
        if cards() == 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(cards(), 2);
    // Collected first: the state lock is never held across an await.
    let waiting: Vec<PermissionRequest> = l.f.hub.st().pending.values().filter(|r| r.tool_call.tool_call_id.starts_with("pawl-start:")).cloned().collect();
    for r in waiting {
        answer(&l.f, &r, "reject").await;
    }
    assert!(!l.f.hub.st().pending.contains_key(&two.request_id));
    second.await.unwrap();
    third.await.unwrap();
    // A prompt over the limit is refused before any card.
    let long = "x".repeat(crate::core::intents::MAX_PROMPT_CHARS + 1);
    let v = ask_start(&app, &l.token, "codex", &a, &long).await.unwrap();
    assert!(error_of(&v).contains("longer than"), "{v}");
    assert_eq!(cards(), 0);
}

#[tokio::test]
async fn a_paired_phone_may_answer_a_start_card_and_another_hub_may_not() {
    let l = live(|_| {}).await;
    let app = test_router(&l.f.hub, PORT);
    for (id, kind) in [("pppppp000001", DeviceKind::Phone), ("hhhhhh000001", DeviceKind::Hub)] {
        let os = if kind == DeviceKind::Phone { pawl_protocol::MachineOs::Ios } else { pawl_protocol::MachineOs::Macos };
        let e = RosterEntry {
            device: Device {
                id: id.into(),
                kind,
                name: id.into(),
                public_key: pawl_peer::DeviceKeys::generate().public_b64(),
                paired_at: 1,
                last_seen: None,
                addr: None,
                path: None,
                introduced_by: None,
                os: Some(os),
            },
            machine: Machine { id: id.into(), name: id.into(), os, last_seen: 1, online: false, hub_version: None, user_active_at: None },
        };
        l.f.hub.store().upsert_device(&e).unwrap();
    }
    l.f.hub.load_peers().await.unwrap();
    let job = folder(&l.ws, "job");
    let h = ask_start(&app, &l.token, "codex", &job, "go");
    let card = start_card(&l.f).await;
    // Another hub: refused.
    let a = l.f.hub.handle_intent("hhhhhh000001", "h-1", ClientKind::Desktop, true, &Intent::Approve { request_id: card.request_id.clone(), option_id: "allow".into() }).await.unwrap();
    assert_ne!(a.status, IntentStatus::Applied, "{a:?}");
    assert!(l.f.hub.st().pending.contains_key(&card.request_id));
    // The phone, unconfirmed (no biometric check): refused; confirmed: applied.
    let a = l.f.hub.handle_intent("pppppp000001", "p-1", ClientKind::Mobile, false, &Intent::Approve { request_id: card.request_id.clone(), option_id: "allow".into() }).await.unwrap();
    assert_ne!(a.status, IntentStatus::Applied, "{a:?}");
    let a = l.f.hub.handle_intent("pppppp000001", "p-2", ClientKind::Mobile, true, &Intent::Approve { request_id: card.request_id.clone(), option_id: "allow".into() }).await.unwrap();
    assert_eq!(a.status, IntentStatus::Applied, "{a:?}");
    assert_eq!(h.await.unwrap()["result"]["task"]["status"]["state"], "TASK_STATE_SUBMITTED");
}

#[test]
fn agent_config_paths_are_found_however_they_are_written() {
    let req = |kind: ToolKind, path: Option<&str>, input: Value| {
        let mut r = permission("s1", kind);
        r.tool_call.locations = path.map(|p| vec![ToolLocation { path: p.into(), line: None }]);
        r.tool_call.raw_input = Some(input);
        r
    };
    let cwd = Some("/home/me/Pawl/job");
    let edit = |p: &str| pawl_brain::rules::writes_agent_config(&req(ToolKind::Edit, Some(p), json!({})), cwd);
    for p in [
        "C:\\Users\\me\\Pawl\\job\\.claude\\settings.json",
        "/home/me/Pawl/job/.CLAUDE/Settings.JSON",
        "../.mcp.json",
        ".vscode/mcp.json",
        "/home/me/Pawl/job/a/../.gemini/settings.json",
        "/home/me/Pawl/job/.git/hooks/pre-commit",
        ".git/config",
        "docs/AGENTS.md",
        "CLAUDE.md",
        "sub/GEMINI.md",
        ".codex/config.toml",
        ".github/workflows/ci.yml",
        "opencode.json",
        ".opencode/agent.md",
    ] {
        assert!(edit(p), "{p}");
    }
    // Anything in (or named) .git, the new names, and Windows aliases of all of these.
    for p in [
        ".git/HEAD",
        "sub/.git",
        ".git/modules/lib/config",
        ".git/config.worktree",
        ".git/info/attributes",
        "a/.GIT/hooks/x",
        ".gitmodules",
        ".gitattributes",
        "opencode.jsonc",
        "AGENTS.override.md",
        ".agents/skill.md",
        "CLAUDE.md::$DATA",
        "CLAUDE.md:stream",
        "CLAUDE.md ",
        "CLAUDE.md.",
        ".claude./settings.json",
        ".claude  \\settings.json",
        "CLAUDE~1.MD",
        "C:\\Users\\me\\Pawl\\job\\CLAUDE~1.MD",
        ".CLAUD~1\\settings.json",
    ] {
        assert!(edit(p), "{p}");
    }
    for p in ["src/main.rs", ".claude-notes/x.md", "/home/me/Pawl/job/README.md", "settings.json", "notes~draft.md", ".gitignore", "git/config"] {
        assert!(!edit(p), "{p}");
    }
    // Reads, searches and thinking change nothing.
    assert!(!pawl_brain::rules::writes_agent_config(&req(ToolKind::Read, Some(".claude/settings.json"), json!({})), cwd));
    // Moves, deletes and unknown kinds count, and paths in the input count (nested, keys, patches).
    assert!(pawl_brain::rules::writes_agent_config(&req(ToolKind::Move, None, json!({ "source": "a.md", "destination": ".claude/commands/a.md" })), cwd));
    assert!(pawl_brain::rules::writes_agent_config(&req(ToolKind::Other, None, json!({ "file_path": ".mcp.json" })), cwd));
    assert!(pawl_brain::rules::writes_agent_config(&req(ToolKind::Edit, None, json!({ "changes": { "/home/me/Pawl/.codex/config.toml": { "type": "add" } } })), cwd));
    assert!(pawl_brain::rules::writes_agent_config(&req(ToolKind::Edit, None, json!({ "patch": "*** Begin Patch\n*** Update File: .github/copilot-instructions.md\n@@\n" })), cwd));
    assert!(!pawl_brain::rules::writes_agent_config(&req(ToolKind::Edit, None, json!({ "patch": "*** Update File: src/lib.rs\n" })), cwd));
    // Single-segment keys of `changes`, and file: or path-like uri / url values.
    assert!(pawl_brain::rules::writes_agent_config(&req(ToolKind::Edit, None, json!({ "changes": { "CLAUDE.md": { "content": "x" } } })), cwd));
    assert!(pawl_brain::rules::writes_agent_config(&req(ToolKind::Edit, None, json!({ "uri": "file:///home/me/Pawl/job/.mcp.json" })), cwd));
    assert!(pawl_brain::rules::writes_agent_config(&req(ToolKind::Edit, None, json!({ "documentUri": ".vscode/settings.json" })), cwd));
    assert!(pawl_brain::rules::writes_agent_config(&req(ToolKind::Edit, None, json!({ "text": "file://C:/Users/me/Pawl/.codex/config.toml" })), cwd));
    assert!(!pawl_brain::rules::writes_agent_config(&req(ToolKind::Edit, None, json!({ "url": "https://example.com/.claude/settings.json" })), cwd));
    assert!(!pawl_brain::rules::writes_agent_config(&req(ToolKind::Edit, None, json!({ "changes": { "src/lib.rs": {} } })), cwd));
    // Folder entries, for the start check: the same lists.
    for name in [".github", ".Copilot", ".claude", "AGENTS.md", ".mcp.json", "mcp.json", ".agents", "opencode.jsonc", "AGENTS.override.md", ".gitmodules", ".gitattributes", "CLAUDE.md."] {
        assert!(pawl_brain::rules::is_agent_config_name(name), "{name}");
    }
    assert!(!pawl_brain::rules::is_agent_config_name("src") && !pawl_brain::rules::is_agent_config_name("README.md"));
}

#[tokio::test]
async fn agent_config_writes_by_the_assistant_or_what_it_started_are_always_cards() {
    let allow_edits = Rule { id: "edits".into(), matcher: RuleMatch { tool_kind: Some(ToolKind::Edit), ..Default::default() }, action: RuleAction::AutoAllow };
    let l = live(|c| {
        c.rules = Some(vec![allow_edits]);
        c.a2a.callers = vec![ci()];
    })
    .await;
    let app = test_router(&l.f.hub, PORT);
    let resolved = |r: &PermissionRequest| l.f.fake.calls().into_iter().any(|c| c.starts_with(&format!("resolve {} ", r.request_id)));
    let edit = |sid: &str, path: &str| {
        let mut r = permission(sid, ToolKind::Edit);
        r.tool_call.locations = Some(vec![ToolLocation { path: path.into(), line: None }]);
        r
    };
    // A session the assistant started, and one another caller started. Not remote-driven here, to test this
    // rule on its own (remote-driven sessions ask for everything anyway).
    let job = folder(&l.ws, "job");
    let h = ask_start(&app, &l.token, "codex", &job, "go");
    answer(&l.f, &start_card(&l.f).await, "allow").await;
    let by_assistant = h.await.unwrap()["result"]["task"]["contextId"].as_str().unwrap().to_string();
    let by_ci = ask_start(&app, CI, "codex", &job, "go").await.unwrap()["result"]["task"]["contextId"].as_str().unwrap().to_string();
    for sid in [&by_assistant, &by_ci, &l.sid] {
        l.f.hub.st().remote_driven.remove(sid);
    }
    assert!(l.f.hub.assistant_guarded(&by_assistant) && l.f.hub.assistant_guarded(&l.sid) && !l.f.hub.assistant_guarded(&by_ci));
    let dir = job.to_string_lossy().into_owned();
    for sid in [&by_assistant, &l.sid] {
        for path in [format!("{dir}/.claude/settings.json"), ".git/hooks/pre-push".into(), format!("{dir}/AGENTS.md")] {
            let r = edit(sid, &path);
            l.f.hub.on_host_event(HostEvent::PermissionRequest(r.clone())).await;
            assert!(!resolved(&r) && l.f.hub.st().pending.contains_key(&r.request_id), "{path}: a card, whatever the rule says");
        }
        let plain = edit(sid, &format!("{dir}/src/lib.rs"));
        l.f.hub.on_host_event(HostEvent::PermissionRequest(plain.clone())).await;
        assert!(resolved(&plain), "other edits follow the rule");
    }
    // Another caller's session: the user's rule as usual.
    let other = edit(&by_ci, &format!("{dir}/.claude/settings.json"));
    l.f.hub.on_host_event(HostEvent::PermissionRequest(other.clone())).await;
    assert!(resolved(&other));
}

#[tokio::test]
async fn with_the_default_rules_every_tool_call_of_the_assistant_is_a_card() {
    let l = live(|c| c.rules = None).await;
    assert!(l.f.hub.st().remote_driven.contains(&l.sid), "the assistant session is remote-driven");
    for kind in [
        ToolKind::Read,
        ToolKind::Search,
        ToolKind::Edit,
        ToolKind::Move,
        ToolKind::Delete,
        ToolKind::Execute,
        ToolKind::Fetch,
        ToolKind::SwitchMode,
        ToolKind::Other,
        // Even thinking: a remote-driven session's auto-allow rules become cards.
        ToolKind::Think,
    ] {
        let r = permission(&l.sid, kind);
        l.f.hub.on_host_event(HostEvent::PermissionRequest(r.clone())).await;
        assert!(l.f.hub.st().pending.contains_key(&r.request_id), "{kind:?} is a card");
        assert!(!l.f.fake.calls().iter().any(|c| c.starts_with(&format!("resolve {} ", r.request_id))), "{kind:?}");
    }
}

/// A Chrome DevTools step of session `sid` as an adapter reports it (`title`, `input`).
fn step(sid: &str, title: &str, input: Value) -> PermissionRequest {
    let mut r = permission(sid, ToolKind::Other);
    r.agent_id = "claude-code".into();
    r.tool_call.title = Some(title.into());
    r.tool_call.raw_input = Some(input);
    r.tool_call.locations = None;
    r.options.push(PermissionOption { option_id: "always".into(), name: "Always allow".into(), kind: PermissionOptionKind::AllowAlways });
    r
}

#[tokio::test]
async fn the_browser_rules_decide_the_assistants_chrome_devtools_steps() {
    let allow_read = Rule { id: "read".into(), matcher: RuleMatch { tool_kind: Some(ToolKind::Read), ..Default::default() }, action: RuleAction::AutoAllow };
    let f = fixture_config(|c| {
        c.browser.enabled = true;
        c.browser.allow_sites = vec!["github.com".into()];
        c.rules = Some(vec![allow_read]);
    })
    .await;
    let s = start(&f).await;
    f.hub.st().sessions.get_mut(&s.id).unwrap().purpose = Some(PURPOSE_ASSISTANT.into());
    let resolved = |r: &PermissionRequest| f.fake.calls().into_iter().find_map(|c| c.strip_prefix(&format!("resolve {} ", r.request_id)).map(str::to_string));
    let card = |r: &PermissionRequest| f.hub.st().pending.get(&r.request_id).cloned();

    // An allowed site opens without asking (the browser rule, not the user's rule).
    let nav = step(&s.id, "chrome-devtools: navigate_page", json!({ "url": "https://github.com/acme/app" }));
    f.hub.on_host_event(HostEvent::PermissionRequest(nav.clone())).await;
    assert_eq!(resolved(&nav).as_deref(), Some("allow"));
    assert!(f.hub.store().audit_log(10).unwrap().iter().any(|a| a.request_id == nav.request_id && a.actor == "browser:allowed site"));
    // Another site asks, although it is reported as a read and a user rule allows reads; no "always allow".
    let mut away = step(&s.id, "chrome-devtools: navigate_page", json!({ "url": "https://ci.example.org/" }));
    away.tool_call.kind = Some(ToolKind::Read);
    f.hub.on_host_event(HostEvent::PermissionRequest(away.clone())).await;
    assert_eq!(resolved(&away), None);
    let c = card(&away).unwrap();
    assert_eq!(c.tool_call.title.as_deref(), Some("Browser: open “https://ci.example.org/” · input {url: “https://ci.example.org/”}"));
    assert!(c.options.iter().all(|o| o.kind != PermissionOptionKind::AllowAlways));
    // Typing and submitting always ask.
    let typing = step(&s.id, "chrome-devtools: type_text", json!({ "text": "hello", "submitKey": "Enter" }));
    f.hub.on_host_event(HostEvent::PermissionRequest(typing.clone())).await;
    assert_eq!(resolved(&typing), None);
    assert!(card(&typing).unwrap().tool_call.title.unwrap().contains("type “hello”, then press “Enter”"));
    // Scripts, init scripts, internal pages, path-like arguments and tools off the list: refused.
    for (title, input) in [
        ("chrome-devtools: evaluate_script", json!({ "function": "() => document.cookie" })),
        ("chrome-devtools: navigate_page", json!({ "url": "https://github.com/", "initScript": "steal()" })),
        ("chrome-devtools: new_page", json!({ "url": "chrome://settings/passwords" })),
        ("chrome-devtools: take_screenshot", json!({ "filePath": "C:/x.png" })),
        ("chrome-devtools: upload_file", json!({ "uid": "1", "filePaths": ["/etc/passwd"] })),
        // A Chrome DevTools step whose tool Pawl cannot read is refused, never left to the user's rules.
        ("Chrome DevTools (navigate)", json!({ "url": "https://github.com/" })),
        ("navigate", json!({ "server": "chrome-devtools", "url": "https://github.com/" })),
    ] {
        let r = step(&s.id, title, input);
        f.hub.on_host_event(HostEvent::PermissionRequest(r.clone())).await;
        assert_eq!(resolved(&r).as_deref(), Some("reject"), "{title}");
    }
    // codex-acp's form (`mcp.<server>.<tool>`, the step's input under `arguments`) is read the same way.
    let codex = step(&s.id, "mcp.chrome-devtools.navigate_page", json!({ "server": "chrome-devtools", "tool": "navigate_page", "arguments": { "url": "https://ci.example.org/" } }));
    f.hub.on_host_event(HostEvent::PermissionRequest(codex.clone())).await;
    assert_eq!(card(&codex).unwrap().tool_call.title.as_deref(), Some("Browser: open “https://ci.example.org/” · input {url: “https://ci.example.org/”}"));
    let codex_ok = step(&s.id, "mcp.chrome-devtools.navigate_page", json!({ "server": "chrome-devtools", "tool": "navigate_page", "arguments": { "url": "https://github.com/" } }));
    f.hub.on_host_event(HostEvent::PermissionRequest(codex_ok.clone())).await;
    assert_eq!(resolved(&codex_ok).as_deref(), Some("allow"));
    // The assistant's other tools follow the user's rules as usual.
    let mut read = step(&s.id, "Read notes.md", json!({}));
    read.tool_call.kind = Some(ToolKind::Read);
    f.hub.on_host_event(HostEvent::PermissionRequest(read.clone())).await;
    assert_eq!(resolved(&read).as_deref(), Some("allow"), "the user's rule");
    // A Chrome DevTools step of an ordinary session is not the browser rules' business.
    let plain = start(&f).await;
    let r = step(&plain.id, "chrome-devtools: evaluate_script", json!({}));
    assert!(!f.hub.browser_rules_apply(&r));
    assert!(f.hub.browser_rules_apply(&step(&s.id, "chrome-devtools: evaluate_script", json!({}))));
}

#[tokio::test]
async fn the_chrome_devtools_server_goes_only_to_agents_pawl_can_check_and_only_with_browser_on() {
    let f = fixture_config(|c| {
        c.a2a.port = free_port();
        c.browser.enabled = false;
    })
    .await;
    let names = |tools: &[pawl_acp::McpServerStdio]| tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&f.hub.assistant_tools("claude-code", true, true).await.unwrap()), ["pawl"], "browser use off");
    let f = fixture_config(|c| c.browser.enabled = true).await;
    assert!(f.hub.assistant_tools("codex", false, true).await.unwrap().is_empty(), "an agent whose steps Pawl cannot read");
    assert!(f.hub.assistant_tools("gemini", false, true).await.unwrap().is_empty());
    assert!(f.hub.assistant_tools("claude-code", false, false).await.unwrap().is_empty(), "browser not asked for");
    let tools = f.hub.assistant_tools("claude-code", false, true).await.unwrap();
    match browser::mcp_stdio() {
        // The browser tasks' pinned package and flags, with an absolute command.
        Some(want) => {
            assert_eq!(tools, vec![want.clone()]);
            assert_eq!(want.name, "chrome-devtools");
            assert!(want.command.is_absolute(), "{}", want.command.display());
            for a in ["chrome-devtools-mcp@1.14.0", "--autoConnect", "--no-javascript-evaluation", "--no-file-navigations", "--blockedUrlPattern=chrome://*"] {
                assert!(want.args.iter().any(|x| x == a), "{a} in {:?}", want.args);
            }
            if cfg!(windows) {
                assert_eq!(want.args[..2], ["/c".to_string(), "npx".to_string()], "cmd /c npx");
            }
            assert!(want.env.contains(&("CHROME_DEVTOOLS_MCP_NO_USAGE_STATISTICS".into(), "1".into())));
        }
        // No Node.js here: no browser tools.
        None => assert!(tools.is_empty()),
    }
}
