//! Pictures in chat and prompts (v0.2 item 4) and browser tasks (v0.2 item 5) over the hub: pictures are
//! checked, used once and never stored, mirrored or sent to another machine; browser tasks start only
//! confirmed and enabled, run locked to the Chrome DevTools MCP server, and their steps follow the browser
//! approval kinds.

use super::*;
use pawl_protocol::ChatImage;

/// Standard base64 (tests only).
fn b64(bytes: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for k in 0..4 {
            s.push(if k <= c.len() { A[((n >> (18 - 6 * k)) & 63) as usize] as char } else { '=' });
        }
    }
    s
}

/// The marker inside every test picture: looked for in the store afterwards.
const MARKER: &[u8] = b"PAWL-TEST-PICTURE-MARKER-7f3a9c";

/// A PNG of `w` x `h` (header, then the marker bytes).
fn png_sized(w: u32, h: u32) -> ChatImage {
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0]);
    for _ in 0..40 {
        b.extend_from_slice(MARKER);
    }
    ChatImage { media_type: "image/png".into(), data: b64(&b) }
}

fn png() -> ChatImage {
    png_sized(1280, 800)
}

fn chat_img(text: &str, images: Vec<ChatImage>) -> ClientMsg {
    ClientMsg::Chat { id: "i".into(), pet_id: None, text: text.into(), via: None, images }
}

async fn pet_reply(rx: &mut broadcast::Receiver<ServerMsg>) -> ChatMessage {
    match wait_for(rx, "the pet's reply", |m| matches!(m, ServerMsg::Chat { message } if message.role == ChatRole::Pet)).await {
        ServerMsg::Chat { message } => message,
        _ => unreachable!(),
    }
}

/// Every byte the store wrote (hub.db and its journal files), to look for a picture in.
fn store_bytes(f: &Fixture) -> Vec<u8> {
    let mut all = Vec::new();
    for e in std::fs::read_dir(&f.hub.paths().data_dir).unwrap().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("hub.db") {
            all.extend(std::fs::read(e.path()).unwrap_or_default());
        }
    }
    all
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// The picture's base64 (or a 40-character piece of it) is nowhere in `bytes`.
fn no_picture_in(bytes: &[u8], img: &ChatImage) {
    assert!(!contains(bytes, img.data.as_bytes()), "the picture is there");
    let mid = img.data.len() / 2;
    assert!(!contains(bytes, &img.data.as_bytes()[mid..mid + 40]), "a piece of the picture is there");
    assert!(!contains(bytes, MARKER), "the picture's bytes are there");
}

/// A hub whose pet has no model at all (a chosen provider that is not configured).
async fn no_model() -> Fixture {
    fixture_config(|c| c.brain.provider = pawl_protocol::BotProvider::Anthropic).await
}

#[tokio::test]
async fn a_picture_in_chat_is_answered_once_and_never_stored() {
    let mut f = no_model().await;
    drain(&mut f.rx);
    let img = png();
    let v = ack_result(f.hub.handle(chat_img("what's on my screen?", vec![img.clone()]), &actor()).await);
    let sent: ChatMessage = serde_json::from_value(v["message"].clone()).unwrap();
    assert!(sent.image, "the message says a picture came with it");
    assert_eq!(sent.text, "what's on my screen?");
    // No model that sees: the plain reply (the picture is not stored to try later).
    let reply = pet_reply(&mut f.rx).await;
    assert_eq!(reply.text, pawl_brain::chat::vision::NO_VISION);
    assert!(!reply.image && reply.proposals.is_empty() && reply.session_id.is_none());
    // The chat history keeps the flag only.
    let history = f.hub.store().recent_chat(10).unwrap();
    let user = history.iter().find(|m| m.role == ChatRole::User).unwrap();
    assert!(user.image && user.text == "what's on my screen?");
    no_picture_in(&store_bytes(&f), &img);
    // Nor in anything broadcast after the request.
    let all: String = drain(&mut f.rx).iter().map(|m| serde_json::to_string(m).unwrap()).collect();
    assert!(!all.contains(&img.data[..40]));
    // A picture may come without words.
    let v = ack_result(f.hub.handle(chat_img("  ", vec![png()]), &actor()).await);
    assert_eq!(v["message"]["image"], true);
    // Typed chat without one has no flag on the wire.
    let v = ack_result(f.hub.handle(chat_img("hi", Vec::new()), &actor()).await);
    assert!(v["message"].get("image").is_none());
}

#[tokio::test]
async fn pictures_are_checked_before_anything_uses_them() {
    let f = fixture(None).await;
    let bad = |img: ChatImage| chat_img("look", vec![img]);
    let mut gif = png();
    gif.media_type = "image/gif".into();
    let mut lie = png();
    lie.media_type = "image/jpeg".into();
    let cases = vec![
        (chat_img("look", vec![png(), png()]), "two pictures"),
        (bad(gif), "a GIF"),
        (bad(lie), "a PNG that says it is a JPEG"),
        (bad(ChatImage { media_type: "image/png".into(), data: "not base64 at all!".into() }), "bad base64"),
        (bad(png_sized(4096, 2160)), "larger than 2048 px"),
        (bad(ChatImage { media_type: "image/png".into(), data: "A".repeat(6 * 1024 * 1024) }), "more than 4 MiB"),
    ];
    for (msg, what) in cases {
        assert_eq!(ack_error(f.hub.handle(msg, &actor()).await), "invalid", "{what}");
    }
    assert!(f.hub.store().recent_chat(10).unwrap().is_empty(), "nothing stored for a refused picture");
    assert!(ack_error(f.hub.handle(chat_img("  ", Vec::new()), &actor()).await).contains("empty"));
}

#[tokio::test]
async fn a_prompt_with_a_picture_goes_only_to_a_local_agent_that_takes_pictures() {
    let f = fixture(None).await;
    let s = start(&f).await;
    let prompt = |sid: &str, images: Vec<ChatImage>| ClientMsg::Prompt { id: "p".into(), session_id: sid.into(), text: "why is this failing?".into(), images };
    // The agent does not advertise promptCapabilities.image: refused, nothing sent.
    assert_eq!(ack_error(f.hub.handle(prompt(&s.id, vec![png()]), &actor()).await), "invalid");
    assert!(!f.fake.calls().iter().any(|c| c.contains("why is this failing?")));
    // It does: one turn with the picture (the fake records only its type).
    f.fake.images_ok.store(true, std::sync::atomic::Ordering::SeqCst);
    ack_result(f.hub.handle(prompt(&s.id, vec![png()]), &actor()).await);
    assert!(f.fake.calls().contains(&format!("prompt {} why is this failing? [images: image/png]", s.id)), "{:?}", f.fake.calls());
    assert_eq!(f.fake.image_bytes.lock().unwrap().clone(), vec![png().data.len()]);
    // Checked like a chat picture; at most one.
    let mut gif = png();
    gif.media_type = "image/gif".into();
    assert_eq!(ack_error(f.hub.handle(prompt(&s.id, vec![gif]), &actor()).await), "invalid");
    assert_eq!(ack_error(f.hub.handle(prompt(&s.id, vec![png(), png()]), &actor()).await), "invalid");
    // Another machine's session: never forwarded (a picture never leaves this machine).
    let theirs = global_id(OTHER, "s-1");
    assert_eq!(ack_error(f.hub.handle(prompt(&theirs, vec![png()]), &actor()).await), "invalid");
    assert!(!f.fake.calls().iter().any(|c| c.contains(&theirs)));
    no_picture_in(&store_bytes(&f), &png());
    // A prompt without a picture is unchanged.
    ack_result(f.hub.handle(prompt(&s.id, Vec::new()), &actor()).await);
    assert!(f.fake.calls().contains(&format!("prompt {} why is this failing?", s.id)));
}

#[tokio::test]
async fn the_acp_brain_session_looks_at_a_picture_through_an_image_block() {
    let mut f = fixture(None).await;
    f.fake.images_ok.store(true, std::sync::atomic::Ordering::SeqCst);
    drain(&mut f.rx);
    let img = png();
    ack_result(f.hub.handle(chat_img("what's this error?", vec![img.clone()]), &actor()).await);
    // The pet's model is the hidden Claude Code session: the picture goes with that one turn.
    wait_until("the brain session's picture prompt", || {
        let fake = f.fake.clone();
        async move { fake.calls().iter().any(|c| c.starts_with("prompt ") && c.contains(":brain-") && c.ends_with("[images: image/png]")) }
    })
    .await;
    let id = brain_prompt(&f).await;
    f.hub.on_host_event(chunk(&id, "A test assertion failed in lib.rs.")).await;
    f.hub.on_host_event(stop(&id)).await;
    let reply = pet_reply(&mut f.rx).await;
    assert!(reply.text.contains("A test assertion failed in lib.rs."), "{}", reply.text);
    // The picture does not stay in the session's context: that session is closed after its turn.
    assert!(f.fake.calls().contains(&format!("close {id}")), "{:?}", f.fake.calls());
    // Only the folder's settings load, and no transcript is kept on disk.
    let o = f.fake.options.lock().unwrap().last().cloned().unwrap().meta.unwrap()["claudeCode"]["options"].clone();
    assert_eq!((o["settingSources"].clone(), o["persistSession"].clone()), (json!(["project"]), json!(false)));
    no_picture_in(&store_bytes(&f), &img);
    // Its agent says it takes no pictures: the plain reply, no turn.
    f.hub
        .on_host_event(HostEvent::AgentCapabilities {
            agent_id: "claude-code".into(),
            capabilities: AgentCapabilities { prompt_image: false, ..Default::default() },
        })
        .await;
    let before = f.fake.calls().len();
    ack_result(f.hub.handle(chat_img("and now?", vec![png()]), &actor()).await);
    let reply = pet_reply(&mut f.rx).await;
    assert_eq!(reply.text, pawl_brain::chat::vision::NO_VISION);
    assert!(!f.fake.calls()[before..].iter().any(|c| c.contains("[images:")));
}

// ---------- browser tasks ----------

async fn browser_fixture() -> Fixture {
    fixture_config(|c| {
        c.browser.enabled = true;
        c.browser.allow_sites = vec!["github.com".into()];
    })
    .await
}

fn start_task(task: &str, confirmed: bool) -> ClientMsg {
    ClientMsg::StartBrowserTask { id: "b".into(), task: task.into(), confirmed }
}

/// Start a confirmed task; its session id.
async fn start_browser(f: &Fixture) -> String {
    let v = ack_result(f.hub.handle(start_task("check if CI passed on my PR", true), &actor()).await);
    v["sessionId"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn browser_tasks_start_only_confirmed_and_enabled() {
    // Off by default.
    let f = fixture(None).await;
    assert_eq!(ack_error(f.hub.handle(start_task("open the staging page", true), &actor()).await), "forbidden");
    let f = browser_fixture().await;
    assert_eq!(ack_error(f.hub.handle(start_task("open the staging page", false), &actor()).await), "unconfirmed");
    assert_eq!(ack_error(f.hub.handle(start_task("   ", true), &actor()).await), "invalid");
    assert!(f.fake.calls().is_empty(), "nothing started");
    let v = ack_result(f.hub.handle(start_task("check if CI passed on my PR", true), &actor()).await);
    assert_eq!(v.as_object().unwrap().keys().collect::<Vec<_>>(), ["sessionId"]);
    let sid = v["sessionId"].as_str().unwrap().to_string();
    // A Claude Code session for the browser, in a new empty folder under the data dir.
    let s = f.hub.session(&sid).unwrap();
    assert_eq!((s.agent_id.as_str(), s.purpose.as_deref(), s.source), ("claude-code", Some("browser"), SessionSource::Owned));
    let dir = std::path::PathBuf::from(&s.cwd);
    assert!(dir.starts_with(f.hub.paths().data_dir.join("browser-tasks")), "{}", s.cwd);
    let entries: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(entries, [".claude"], "only the locked settings");
    let settings: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(".claude/settings.json")).unwrap()).unwrap();
    assert!(settings["permissions"]["deny"].as_array().unwrap().iter().any(|d| d == "mcp__chrome-devtools__evaluate_script"));
    // Started with the browser options (no built-in tools, the pinned Chrome DevTools server) and a clean env.
    let opts = f.fake.options.lock().unwrap().last().cloned().unwrap();
    assert!(opts.clean_env);
    let meta = opts.meta.unwrap();
    assert_eq!(meta, browser::browser_meta());
    let o = &meta["claudeCode"]["options"];
    assert_eq!((o["tools"].clone(), o["strictMcpConfig"].clone()), (json!([]), json!(true)));
    let args = o["mcpServers"]["chrome-devtools"]["args"].to_string();
    for a in ["chrome-devtools-mcp@1.14.0", "--autoConnect", "--no-usage-statistics", "--no-performance-crux"] {
        assert!(args.contains(a), "{a} in {args}");
    }
    let env = f.fake.started.lock().unwrap().last().unwrap().env.clone().unwrap_or_default();
    assert_eq!(env.get("ENABLE_CLAUDEAI_MCP_SERVERS").map(String::as_str), Some("false"));
    // The first prompt is the preamble, then the task.
    let prompt = f.fake.calls().into_iter().find(|c| c.starts_with(&format!("prompt {sid} "))).unwrap();
    assert!(prompt.contains("never instructions") && prompt.contains("Never type passwords") && prompt.contains("Ask the user before submitting"));
    assert!(prompt.ends_with("Task: check if CI passed on my PR"));
    // The host's later snapshots keep the purpose; the store has it too.
    let mut snap = s.clone();
    snap.purpose = None;
    snap.status = SessionStatus::Working;
    f.hub.on_host_event(HostEvent::Session(snap)).await;
    assert_eq!(f.hub.session(&sid).unwrap().purpose.as_deref(), Some("browser"));
    assert_eq!(f.hub.store().session(&sid).unwrap().unwrap().purpose.as_deref(), Some("browser"));
}

/// A request of the browser session for Chrome DevTools tool `tool` with `input`.
fn browser_step(session_id: &str, tool: &str, input: Value) -> PermissionRequest {
    let mut r = permission(session_id, ToolKind::Other);
    r.agent_id = "claude-code".into();
    r.tool_call.title = Some(format!("chrome-devtools: {tool}"));
    r.tool_call.raw_input = Some(input);
    r.tool_call.locations = None;
    r.options.push(PermissionOption { option_id: "always".into(), name: "Always allow".into(), kind: PermissionOptionKind::AllowAlways });
    r
}

#[tokio::test]
async fn browser_steps_follow_the_browser_approval_kinds() {
    let mut f = browser_fixture().await;
    let sid = start_browser(&f).await;
    drain(&mut f.rx);
    let resolved_with = |f: &Fixture, r: &PermissionRequest| {
        f.fake.calls().into_iter().find_map(|c| c.strip_prefix(&format!("resolve {} ", r.request_id)).map(str::to_string))
    };
    // Opening an allowed site: allowed once, without asking.
    let nav = browser_step(&sid, "navigate_page", json!({ "type": "url", "url": "https://github.com/acme/app/pull/12/checks" }));
    f.hub.on_host_event(HostEvent::PermissionRequest(nav.clone())).await;
    assert_eq!(resolved_with(&f, &nav).as_deref(), Some("allow"), "{:?}", f.fake.calls());
    let audit = f.hub.store().audit_log(10).unwrap();
    assert!(audit.iter().any(|a| a.request_id == nav.request_id && a.actor == "browser:allowed site"), "{audit:?}");
    // Any other site asks; the card shows where, and offers no "always allow".
    let other = browser_step(&sid, "navigate_page", json!({ "url": "https://ci.example.org/run/1" }));
    f.hub.on_host_event(HostEvent::PermissionRequest(other.clone())).await;
    assert_eq!(resolved_with(&f, &other), None);
    let card = f.hub.st().pending.get(&other.request_id).cloned().unwrap();
    assert_eq!(card.tool_call.title.as_deref(), Some("Browser: open “https://ci.example.org/run/1” · input {url: “https://ci.example.org/run/1”}"));
    assert!(card.options.iter().all(|o| o.kind != PermissionOptionKind::AllowAlways));
    let sent = drain(&mut f.rx);
    assert!(sent.iter().any(|m| matches!(m, ServerMsg::PermissionRequest { request } if request.request_id == other.request_id && request.options.len() == 2)));
    // http, or another port, of an allowed host is another origin: asks.
    let plain_http = browser_step(&sid, "navigate_page", json!({ "url": "http://github.com/acme" }));
    f.hub.on_host_event(HostEvent::PermissionRequest(plain_http.clone())).await;
    assert_eq!(resolved_with(&f, &plain_http), None);
    // Typing always asks and shows exactly what will be typed, and into which element (latest snapshot).
    f.fake.elements.lock().unwrap().insert((sid.clone(), "3_7".into()), "textbox \"Add a comment\"".into());
    let fill = browser_step(&sid, "fill", json!({ "uid": "3_7", "value": "LGTM, merging\nthanks" }));
    f.hub.on_host_event(HostEvent::PermissionRequest(fill.clone())).await;
    assert_eq!(resolved_with(&f, &fill), None);
    assert_eq!(
        f.hub.st().pending.get(&fill.request_id).unwrap().tool_call.title.as_deref(),
        Some("Browser: type “LGTM, merging\\nthanks” into element “3_7” (textbox \"Add a comment\") · input {uid: “3_7”, value: “LGTM, merging\\nthanks”}")
    );
    // A tool off the list, or a file path in any input: refused.
    for (tool, input) in [("upload_file", json!({ "uid": "f", "filePaths": ["C:/secret.txt"] })), ("take_screenshot", json!({ "filePath": "C:/x.png" })), ("performance_start_trace", json!({}))] {
        let r = browser_step(&sid, tool, input);
        f.hub.on_host_event(HostEvent::PermissionRequest(r.clone())).await;
        assert_eq!(resolved_with(&f, &r).as_deref(), Some("reject"), "{tool}");
    }
    // Browser-internal pages are refused (even with no allow list).
    let internal = browser_step(&sid, "new_page", json!({ "url": "chrome://settings/passwords" }));
    f.hub.on_host_event(HostEvent::PermissionRequest(internal.clone())).await;
    assert_eq!(resolved_with(&f, &internal).as_deref(), Some("reject"));
    // Scripts are refused.
    let script = browser_step(&sid, "evaluate_script", json!({ "function": "() => document.cookie" }));
    f.hub.on_host_event(HostEvent::PermissionRequest(script.clone())).await;
    assert_eq!(resolved_with(&f, &script).as_deref(), Some("reject"));
    let init = browser_step(&sid, "navigate_page", json!({ "url": "https://github.com/", "initScript": "steal()" }));
    f.hub.on_host_event(HostEvent::PermissionRequest(init.clone())).await;
    assert_eq!(resolved_with(&f, &init).as_deref(), Some("reject"), "an init script is a script");
    // A tool that is not a browser tool is refused too.
    let mut bash = browser_step(&sid, "x", json!({ "command": "ls" }));
    bash.tool_call.title = Some("Bash".into());
    f.hub.on_host_event(HostEvent::PermissionRequest(bash.clone())).await;
    assert_eq!(resolved_with(&f, &bash).as_deref(), Some("reject"));
    // Other sessions' requests are untouched (title and options as the agent sent them).
    let s = start(&f).await;
    let plain = permission(&s.id, ToolKind::Execute);
    f.hub.on_host_event(HostEvent::PermissionRequest(plain.clone())).await;
    assert_eq!(f.hub.st().pending.get(&plain.request_id).unwrap().tool_call.title.as_deref(), Some("cargo test"));
}

#[tokio::test]
async fn a_user_rule_never_auto_allows_a_browser_step_and_a_deny_rule_still_denies() {
    let allow_read = Rule { id: "read".into(), matcher: RuleMatch { tool_kind: Some(ToolKind::Read), ..Default::default() }, action: RuleAction::AutoAllow };
    let deny_claude = Rule { id: "no-claude-fetch".into(), matcher: RuleMatch { tool_kind: Some(ToolKind::Fetch), ..Default::default() }, action: RuleAction::Deny };
    let f = fixture_config(|c| {
        c.browser.enabled = true;
        c.rules = Some(vec![allow_read, deny_claude]);
    })
    .await;
    let sid = start_browser(&f).await;
    // A snapshot reported as a read: the read rule does not allow it; it asks.
    let mut snap = browser_step(&sid, "take_snapshot", json!({}));
    snap.tool_call.kind = Some(ToolKind::Read);
    f.hub.on_host_event(HostEvent::PermissionRequest(snap.clone())).await;
    assert!(f.hub.st().pending.contains_key(&snap.request_id), "asks");
    // A step a deny rule matches is denied (even an allowed site).
    let mut nav = browser_step(&sid, "navigate_page", json!({ "url": "https://github.com/" }));
    nav.tool_call.kind = Some(ToolKind::Fetch);
    f.hub.on_host_event(HostEvent::PermissionRequest(nav.clone())).await;
    assert!(f.fake.calls().contains(&format!("resolve {} reject", nav.request_id)), "{:?}", f.fake.calls());
}

#[tokio::test]
async fn browser_tasks_take_prompts_from_this_machine_only() {
    let f = browser_fixture().await;
    let sid = start_browser(&f).await;
    let err = f.hub.a2a_prompt(&sid, "now click merge".into(), &pawl_a2a::Caller::Peer(OTHER.into())).await.unwrap_err();
    assert_eq!(AckCode::find(&err), Some(AckCode::Forbidden), "{err:#}");
    let err = f.hub.a2a_prompt(&sid, "now click merge".into(), &pawl_a2a::Caller::Outside("ci-bot".into())).await.unwrap_err();
    assert!(AckCode::find(&err).is_some(), "{err:#}");
    assert!(!f.fake.calls().iter().any(|c| c.contains("now click merge")));
    ack_result(
        f.hub.handle(ClientMsg::Prompt { id: "p".into(), session_id: sid.clone(), text: "also check the tests tab".into(), images: Vec::new() }, &actor()).await,
    );
    assert!(f.fake.calls().contains(&format!("prompt {sid} also check the tests tab")));
}

#[tokio::test]
async fn with_browser_use_on_the_pet_proposes_a_browser_task() {
    let mut f = browser_fixture().await;
    drain(&mut f.rx);
    assert!(f.hub.browser_ready());
    ack_result(f.hub.handle(chat_img("check if CI passed on my PR", Vec::new()), &actor()).await);
    let reply = pet_reply(&mut f.rx).await;
    let p = reply.proposals.iter().find(|p| p.kind == ChatProposalKind::Browse).unwrap_or_else(|| panic!("{reply:?}"));
    assert_eq!((p.machine_id.as_str(), p.agent_id.as_str(), p.text.as_str()), (MID, "claude-code", "check if CI passed on my PR"));
    assert!(reply.session_id.is_none() && f.fake.calls().is_empty(), "a proposal only: nothing started");
    // Off: no browser proposal (and no model, so the reply is the parser's own).
    let mut off = no_model().await;
    drain(&mut off.rx);
    ack_result(off.hub.handle(chat_img("check if CI passed on my PR", Vec::new()), &actor()).await);
    let reply = pet_reply(&mut off.rx).await;
    assert!(reply.proposals.iter().all(|p| p.kind != ChatProposalKind::Browse), "{reply:?}");
}

#[tokio::test]
async fn hook_reports_from_a_browser_task_folder_are_dropped() {
    let f = browser_fixture().await;
    let cwd = f.hub.paths().data_dir.join("browser-tasks").join("abc").to_string_lossy().into_owned();
    f.hub.ingest_hook(HookSource::Claude, json!({ "hook_event_name": "SessionStart", "session_id": "ext-b", "cwd": cwd })).await.unwrap();
    assert!(f.hub.list_sessions(true).await.unwrap().iter().all(|s| s.external_id.as_deref() != Some("ext-b")));
}

#[tokio::test]
async fn pictures_never_go_to_sessions_driven_from_elsewhere() {
    let f = fixture(None).await;
    f.fake.images_ok.store(true, std::sync::atomic::Ordering::SeqCst);
    let prompt = |sid: &str| ClientMsg::Prompt { id: "p".into(), session_id: sid.into(), text: "see this".into(), images: vec![png()] };
    // Driven by another machine.
    let s = start(&f).await;
    f.hub.mark_remote_driven(&s.id).await;
    assert_eq!(ack_error(f.hub.handle(prompt(&s.id), &actor()).await), "invalid");
    // Started by an outside A2A caller (even with its remote-driven mark gone).
    let cwd = f.dir.path().to_string_lossy().into_owned();
    let s2 = f.hub.a2a_start("codex", &cwd, None, &pawl_a2a::Caller::Outside("ci-bot".into())).await.unwrap().session.unwrap();
    f.hub.st().remote_driven.remove(&s2.id);
    assert_eq!(ack_error(f.hub.handle(prompt(&s2.id), &actor()).await), "invalid");
    assert!(!f.fake.calls().iter().any(|c| c.contains("see this")));
    // This machine's own session: sent.
    let s3 = start(&f).await;
    ack_result(f.hub.handle(prompt(&s3.id), &actor()).await);
    assert!(f.fake.calls().contains(&format!("prompt {} see this [images: image/png]", s3.id)));
}

#[tokio::test]
async fn a_browser_task_ends_with_its_answer_and_its_folder_goes() {
    let f = browser_fixture().await;
    let sid = start_browser(&f).await;
    let dir = std::path::PathBuf::from(f.hub.session(&sid).unwrap().cwd);
    assert!(dir.is_dir());
    f.hub.on_host_event(chunk(&sid, "CI passed: all 12 checks are green.")).await;
    f.hub.on_host_event(stop(&sid)).await;
    wait_until("the browser session's close", || {
        let fake = f.fake.clone();
        let id = sid.clone();
        async move { fake.calls().contains(&format!("close {id}")) }
    })
    .await;
    wait_until("the task folder's removal", || {
        let d = dir.clone();
        async move { !d.exists() }
    })
    .await;
    // The host reports the end: nothing left to clean, no error.
    let mut ended = f.hub.session(&sid).unwrap();
    ended.ended_at = Some(pawl_protocol::now_ms());
    f.hub.on_host_event(HostEvent::Session(ended)).await;
    assert!(f.hub.live_browser_sessions().is_empty());
    // A session that ends any other way (the process exits) loses its folder too.
    let sid = start_browser(&f).await;
    let dir = std::path::PathBuf::from(f.hub.session(&sid).unwrap().cwd);
    let mut gone = f.hub.session(&sid).unwrap();
    gone.ended_at = Some(pawl_protocol::now_ms());
    gone.status = SessionStatus::Error;
    f.hub.on_host_event(HostEvent::Session(gone)).await;
    assert!(!dir.exists());
}

#[tokio::test]
async fn browser_tasks_are_capped_timed_and_ended_when_turned_off() {
    let f = browser_fixture().await;
    let a = start_browser(&f).await;
    let b = start_browser(&f).await;
    // Two at once.
    assert_eq!(ack_error(f.hub.handle(start_task("one more", true), &actor()).await), "rate_limited");
    // Idle for 10 minutes: ended (both).
    let now = pawl_protocol::now_ms();
    f.hub.browser_tick(now + browser::IDLE_MS - 1_000).await;
    assert!(!f.fake.calls().iter().any(|c| c.starts_with("close ")), "not idle yet");
    f.hub.browser_tick(now + browser::IDLE_MS + 1_000).await;
    for id in [&a, &b] {
        assert!(f.fake.calls().contains(&format!("close {id}")), "{id}");
    }
    // Once they ended, a new one may start; a turn longer than 15 minutes ends it.
    for id in [&a, &b] {
        let mut s = f.hub.session(id).unwrap();
        s.ended_at = Some(now);
        f.hub.on_host_event(HostEvent::Session(s)).await;
    }
    let c = start_browser(&f).await;
    assert!(f.hub.inner.browser.lock().unwrap().turn_started.contains_key(&c), "its turn is timed");
    f.hub.inner.browser.lock().unwrap().turn_started.insert(c.clone(), now - browser::TURN_MS - 1_000);
    f.hub.browser_tick(now).await;
    assert!(f.fake.calls().contains(&format!("close {c}")));
    // Turning browser use off ends running tasks at once.
    let mut s = f.hub.session(&c).unwrap();
    s.ended_at = Some(now);
    f.hub.on_host_event(HostEvent::Session(s)).await;
    let d = start_browser(&f).await;
    let patch: pawl_protocol::HubSettingsPatch = serde_json::from_value(json!({ "browser": { "enabled": false } })).unwrap();
    ack_result(f.hub.handle(ClientMsg::UpdateSettings { id: "s".into(), patch: Box::new(patch) }, &actor()).await);
    assert!(f.fake.calls().contains(&format!("close {d}")), "{:?}", f.fake.calls());
    assert_eq!(ack_error(f.hub.handle(start_task("again", true), &actor()).await), "forbidden");
}

#[tokio::test]
async fn leftover_task_folders_are_swept_but_running_ones_kept() {
    let f = browser_fixture().await;
    let sid = start_browser(&f).await;
    let live = std::path::PathBuf::from(f.hub.session(&sid).unwrap().cwd);
    let stale = f.hub.paths().data_dir.join("browser-tasks").join("from-an-earlier-run");
    std::fs::create_dir_all(stale.join(".claude")).unwrap();
    f.hub.sweep_browser_dirs();
    assert!(!stale.exists() && live.is_dir());
}
