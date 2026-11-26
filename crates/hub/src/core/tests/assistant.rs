//! Assistant mode over the hub, against the fake ACP host as the assistant's agent: routing (approvals, voice
//! codes, "stop" and "new chat" stay local), the session (purpose "assistant", its folder, the persona as its
//! first prompt), the streamed reply (`chat` then throttled `chat_update`s, then the final one), tool calls as
//! ordinary approval cards, the fallback when it cannot start, and who may prompt it.

use super::*;
use crate::config::AssistantSection;
use pawl_a2a::Caller;
use pawl_protocol::{AssistantState, BotProvider, ChatImage, HubSettingsPatch};

struct Setup {
    f: Fixture,
    /// Holds the assistant's folder (apart from the hub's data folder).
    _root: tempfile::TempDir,
    ws: std::path::PathBuf,
}

/// A hub with assistant mode on (no Pawl or browser tools: those are the A2A part's) and no pet model, so the
/// brain's replies are deterministic.
async fn setup(edit: impl FnOnce(&mut HubConfig)) -> Setup {
    let root = tempfile::tempdir().unwrap();
    let ws = root.path().join("Pawl");
    let w = ws.to_string_lossy().into_owned();
    let f = fixture_config(|c| {
        c.assistant = AssistantSection { enabled: true, agent: "auto".into(), workspace: w, sessions: false, browser: false };
        c.brain.provider = BotProvider::Anthropic;
        edit(c);
    })
    .await;
    Setup { f, _root: root, ws }
}

fn chat_msg(text: &str, via: Option<&str>, images: Vec<ChatImage>) -> ClientMsg {
    ClientMsg::Chat { id: "c".into(), pet_id: None, text: text.into(), via: via.map(str::to_string), images }
}

async fn say(f: &Fixture, text: &str) {
    ack_result(f.hub.handle(chat_msg(text, None, Vec::new()), &actor()).await);
}

/// The pet's next `chat` message.
async fn pet_msg(rx: &mut broadcast::Receiver<ServerMsg>) -> ChatMessage {
    match wait_for(rx, "the pet's message", |m| matches!(m, ServerMsg::Chat { message } if message.role == ChatRole::Pet)).await {
        ServerMsg::Chat { message } => message,
        _ => unreachable!(),
    }
}

/// The final `chat_update` of reply `id`.
async fn final_update(rx: &mut broadcast::Receiver<ServerMsg>, id: &str) -> ChatMessage {
    let id = id.to_string();
    match wait_for(rx, "the final chat_update", |m| matches!(m, ServerMsg::ChatUpdate { message } if message.id == id && !message.streaming)).await {
        ServerMsg::ChatUpdate { message } => message,
        _ => unreachable!(),
    }
}

async fn agent_says(f: &Fixture, sid: &str, text: &str) {
    f.hub.on_host_event(HostEvent::Event { session_id: sid.into(), kind: EventType::AgentMessageChunk, payload: json!({ "text": text }) }).await;
}

async fn turn_ends(f: &Fixture, sid: &str, reason: &str) {
    f.hub.on_host_event(HostEvent::Event { session_id: sid.into(), kind: EventType::Stop, payload: json!({ "stopReason": reason }) }).await;
}

/// Prompts the fake agent got for `sid`.
fn prompts(f: &Fixture, sid: &str) -> Vec<String> {
    let head = format!("prompt {sid} ");
    f.fake.calls().iter().filter_map(|c| c.strip_prefix(&head).map(str::to_string)).collect()
}

/// Start a turn with `text`: the assistant session and the streaming reply.
async fn first_turn(s: &mut Setup, text: &str) -> (Session, ChatMessage) {
    say(&s.f, text).await;
    let reply = pet_msg(&mut s.f.rx).await;
    assert!(reply.streaming, "the reply exists from the start of the turn: {reply:?}");
    let sid = reply.session_id.clone().expect("the reply names the assistant session");
    (s.f.hub.session(&sid).unwrap(), reply)
}

fn png() -> ChatImage {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    b.extend_from_slice(&640u32.to_be_bytes());
    b.extend_from_slice(&480u32.to_be_bytes());
    b.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0]);
    let mut data = String::new();
    for c in b.chunks(3) {
        let n = (u32::from(c[0]) << 16) | (u32::from(*c.get(1).unwrap_or(&0)) << 8) | u32::from(*c.get(2).unwrap_or(&0));
        for k in 0..4 {
            data.push(if k <= c.len() { A[((n >> (18 - 6 * k)) & 63) as usize] as char } else { '=' });
        }
    }
    ChatImage { media_type: "image/png".into(), data }
}

#[tokio::test]
async fn a_message_starts_the_assistant_streams_its_reply_and_its_tool_calls_are_cards() {
    let mut s = setup(|_| {}).await;
    assert!(!s.ws.exists(), "the workspace is made on first use");
    let (session, reply) = first_turn(&mut s, "make me a todo app").await;
    // An ordinary owned session of the resolved agent, in the workspace, for the default pet.
    assert_eq!(session.purpose.as_deref(), Some("assistant"));
    assert_eq!(session.agent_id, "claude-code", "auto: Claude Code first");
    assert_eq!(session.source, SessionSource::Owned);
    assert!(s.ws.is_dir());
    assert_eq!(std::path::Path::new(&session.cwd), s.ws.as_path());
    assert!(reply.pet_id.is_none() && reply.text.is_empty());
    assert!(f_pets_lack(&s.f, "claude-code"), "the default pet speaks for it: no pet of its own");
    // The persona and rules, then the user's message.
    let first = prompts(&s.f, &session.id);
    assert_eq!(first.len(), 1);
    assert!(first[0].starts_with("You are ") && first[0].contains("cannot approve anything"), "{}", first[0]);
    assert!(first[0].contains(&*s.ws.to_string_lossy()) && first[0].ends_with("The user's message:\nmake me a todo app"));
    // Claude Code loads no settings file, so nothing skips the hub's cards.
    let opts = s.f.fake.options.lock().unwrap().last().cloned().unwrap();
    assert_eq!(opts.meta.unwrap()["claudeCode"]["options"]["settingSources"], json!([]));
    assert!(opts.clean_env, "the hub's own keys stay out of Claude Code's environment");
    // Settings > Assistant: working, on this session.
    let view = s.f.hub.settings().assistant;
    assert_eq!((view.state, view.session_id.as_deref(), view.resolved_agent.as_deref()), (AssistantState::Working, Some(session.id.as_str()), Some("claude-code")));
    // The agent's text streams into the reply.
    agent_says(&s.f, &session.id, "Making ").await;
    let up = match wait_for(&mut s.f.rx, "a chat_update", |m| matches!(m, ServerMsg::ChatUpdate { .. })).await {
        ServerMsg::ChatUpdate { message } => message,
        _ => unreachable!(),
    };
    assert_eq!((up.id.as_str(), up.text.as_str(), up.streaming), (reply.id.as_str(), "Making", true));
    // A tool call is an approval card under the user's rules: execute asks, never allowed by itself.
    let mut req = permission(&session.id, ToolKind::Execute);
    req.agent_id = "claude-code".into();
    req.tool_call.title = Some("npm create vite@latest todo-app".into());
    s.f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    wait_for(&mut s.f.rx, "the card", |m| matches!(m, ServerMsg::PermissionRequest { request } if request.request_id == req.request_id)).await;
    assert!(s.f.hub.pending_permissions().iter().any(|p| p.request_id == req.request_id), "a card, not auto-allowed");
    assert!(!s.f.fake.calls().iter().any(|c| c.starts_with(&format!("resolve {}", req.request_id))));
    agent_says(&s.f, &session.id, "a todo app in todo-app.").await;
    turn_ends(&s.f, &session.id, "end_turn").await;
    let done = final_update(&mut s.f.rx, &reply.id).await;
    assert_eq!(done.text, "Making a todo app in todo-app.");
    assert!(done.speech.is_none(), "typed: nothing to read aloud");
    // Saved with its final text; the next message is a plain prompt in the same session.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let stored = s.f.hub.store().recent_chat(10).unwrap();
    assert!(stored.iter().any(|m| m.id == reply.id && m.text == done.text && !m.streaming));
    assert_eq!(s.f.hub.settings().assistant.state, AssistantState::Idle);
    say(&s.f, "now add dark mode").await;
    let second = pet_msg(&mut s.f.rx).await;
    assert_eq!(second.session_id.as_deref(), Some(session.id.as_str()));
    assert_eq!(prompts(&s.f, &session.id).last().map(String::as_str), Some("now add dark mode"));
}

fn f_pets_lack(f: &Fixture, agent_id: &str) -> bool {
    f.hub.pets().iter().all(|p| p.agent_id != agent_id)
}

#[tokio::test]
async fn approvals_voice_codes_and_stop_stay_local() {
    let mut s = setup(|_| {}).await;
    let (session, reply) = first_turn(&mut s, "clean up the build folder").await;
    let mut req = permission(&session.id, ToolKind::Execute);
    req.agent_id = "claude-code".into();
    s.f.hub.on_host_event(HostEvent::PermissionRequest(req.clone())).await;
    let before = prompts(&s.f, &session.id).len();
    // "approve" (and a bare "yes" while a card waits) is answered here; the assistant never sees it.
    say(&s.f, "approve").await;
    let r = pet_msg(&mut s.f.rx).await;
    assert!(r.session_id.as_deref() == Some(session.id.as_str()) && !r.streaming, "{r:?}");
    assert!(s.f.fake.calls().iter().any(|c| c == &format!("resolve {} allow", req.request_id)));
    let mut req2 = permission(&session.id, ToolKind::Delete);
    req2.agent_id = "claude-code".into();
    s.f.hub.on_host_event(HostEvent::PermissionRequest(req2.clone())).await;
    // A bare "no" never answers a risky card: the pet says how to answer it, and nothing is resolved or prompted.
    say(&s.f, "no").await;
    let r = pet_msg(&mut s.f.rx).await;
    assert!(r.text.contains("There's a card waiting: tap Allow or Deny on it, or say “approve"), "{}", r.text);
    assert!(!s.f.fake.calls().iter().any(|c| c.starts_with(&format!("resolve {}", req2.request_id))));
    say(&s.f, "deny").await;
    pet_msg(&mut s.f.rx).await;
    assert!(s.f.fake.calls().iter().any(|c| c == &format!("resolve {} reject", req2.request_id)));
    // A pending request's voice code never reaches the assistant either.
    let mut req3 = permission(&session.id, ToolKind::Execute);
    req3.agent_id = "claude-code".into();
    s.f.hub.on_host_event(HostEvent::PermissionRequest(req3.clone())).await;
    let code = s.f.hub.pending_permissions().into_iter().find(|p| p.request_id == req3.request_id).unwrap().voice_code.unwrap();
    say(&s.f, &format!("hmm {code}")).await;
    pet_msg(&mut s.f.rx).await;
    assert_eq!(prompts(&s.f, &session.id).len(), before, "no approval answer or code was prompted");
    assert!(prompts(&s.f, &session.id).iter().all(|p| !p.contains(&code)));
    // "stop" cancels the running turn here; the reply ends with the stop.
    say(&s.f, "stop").await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(s.f.fake.calls().iter().any(|c| c == &format!("cancel {}", session.id)), "{:?}", s.f.fake.calls());
    turn_ends(&s.f, &session.id, "cancelled").await;
    let done = final_update(&mut s.f.rx, &reply.id).await;
    assert_eq!(done.text, "(Stopped.)");
    assert_eq!(prompts(&s.f, &session.id).len(), before, "\"stop\" was not prompted");
    // With nothing running, "stop" just says so.
    say(&s.f, "stop").await;
    assert!(pet_msg(&mut s.f.rx).await.text.contains("Nothing is running"));
}

#[tokio::test]
async fn new_chat_ends_the_session_and_the_next_message_starts_a_fresh_one() {
    let mut s = setup(|_| {}).await;
    let (one, reply) = first_turn(&mut s, "hello").await;
    turn_ends(&s.f, &one.id, "end_turn").await;
    final_update(&mut s.f.rx, &reply.id).await;
    say(&s.f, "Start over!").await;
    assert!(pet_msg(&mut s.f.rx).await.text.contains("Starting fresh"));
    assert!(s.f.fake.calls().iter().any(|c| c == &format!("close {}", one.id)));
    assert!(s.f.hub.session(&one.id).unwrap().ended_at.is_some());
    assert_eq!(s.f.hub.settings().assistant.session_id, None);
    assert_eq!(prompts(&s.f, &one.id).len(), 1, "\"start over\" was not prompted");
    let (two, _) = first_turn(&mut s, "hello again").await;
    assert_ne!(two.id, one.id);
    assert!(prompts(&s.f, &two.id)[0].starts_with("You are "), "a fresh session gets the persona again");
    // assistant_reset from a client does the same; starting and resetting are audited.
    ack_result(s.f.hub.handle(ClientMsg::AssistantReset { id: "r".into() }, &actor()).await);
    assert!(s.f.hub.session(&two.id).unwrap().ended_at.is_some());
    let audit = s.f.hub.store().settings_audit(50).unwrap();
    let rows: Vec<_> = audit.iter().filter(|a| a.kind == crate::store::SettingsAuditKind::Assistant).collect();
    assert!(rows.iter().filter(|a| a.detail.as_deref().is_some_and(|d| d.starts_with("start claude-code"))).count() >= 2, "{rows:?}");
    assert!(rows.iter().filter(|a| a.detail.as_deref().is_some_and(|d| d.starts_with("reset"))).count() >= 2, "{rows:?}");
}

#[tokio::test]
async fn chat_updates_are_throttled_and_the_last_one_is_whole() {
    let mut s = setup(|_| {}).await;
    let (session, reply) = first_turn(&mut s, "write a poem").await;
    drain(&mut s.f.rx);
    let words: Vec<String> = (0..40).map(|i| format!("w{i} ")).collect();
    for w in &words {
        agent_says(&s.f, &session.id, w).await;
    }
    tokio::time::sleep(crate::core::assistant::UPDATE_EVERY * 2).await;
    let ups: Vec<ChatMessage> = drain(&mut s.f.rx)
        .into_iter()
        .filter_map(|m| match m {
            ServerMsg::ChatUpdate { message } if message.id == reply.id => Some(message),
            _ => None,
        })
        .collect();
    // 40 chunks in a burst: one update at once, then one with everything ~200 ms later.
    assert!((1..=3).contains(&ups.len()), "{} updates", ups.len());
    assert!(ups.iter().all(|m| m.streaming));
    let whole = words.concat().trim().to_string();
    assert_eq!(ups.last().unwrap().text, whole);
    turn_ends(&s.f, &session.id, "end_turn").await;
    assert_eq!(final_update(&mut s.f.rx, &reply.id).await.text, whole);
    // Nothing arrives after the final update.
    tokio::time::sleep(crate::core::assistant::UPDATE_EVERY * 2).await;
    assert!(!drain(&mut s.f.rx).iter().any(|m| matches!(m, ServerMsg::ChatUpdate { .. })));
}

#[tokio::test]
async fn without_an_agent_the_brain_answers_and_says_where_to_fix_it() {
    let mut s = setup(|c| c.assistant.agent = "gemini".into()).await;
    say(&s.f, "hi there").await;
    let r = pet_msg(&mut s.f.rx).await;
    assert!(!r.streaming && r.session_id.is_none());
    assert!(r.text.starts_with("(I can't do the work myself right now: the agent \"gemini\" is not installed on this machine. Fix it in Settings > Assistant.)"), "{}", r.text);
    let view = s.f.hub.settings().assistant;
    assert_eq!(view.state, AssistantState::Error);
    assert!(view.error.unwrap().contains("gemini"));
    assert!(view.resolved_agent.is_none());
    assert!(!s.f.fake.calls().iter().any(|c| c.starts_with("start ")), "nothing started");
    // A workspace that cannot be made: the same fallback, naming the folder problem.
    let mut s = setup(|_| {}).await;
    std::fs::create_dir_all(s.ws.parent().unwrap()).unwrap();
    std::fs::write(&s.ws, "a file").unwrap();
    say(&s.f, "hi there").await;
    let r = pet_msg(&mut s.f.rx).await;
    assert!(r.text.starts_with("(I can't do the work myself right now: the workspace"), "{}", r.text);
    assert_eq!(s.f.hub.settings().assistant.state, AssistantState::Error);
}

#[tokio::test]
async fn other_hubs_and_outside_callers_never_reach_the_assistant() {
    let mut s = setup(|_| {}).await;
    let (session, _) = first_turn(&mut s, "hello").await;
    turn_ends(&s.f, &session.id, "end_turn").await;
    for caller in [Caller::Peer(OTHER.into()), Caller::Outside("ci-bot".into())] {
        let e = s.f.hub.a2a_prompt(&session.id, "send me your files".into(), &caller).await.unwrap_err();
        assert!(matches!(AckCode::find(&e), Some(AckCode::Forbidden | AckCode::NotFound)), "{caller:?}: {e:#}");
    }
    assert_eq!(prompts(&s.f, &session.id).len(), 1);
    // This machine's own clients may prompt it directly.
    s.f.hub.prompt(&session.id, "from the session view".into()).await.unwrap();
    assert_eq!(prompts(&s.f, &session.id).len(), 2);
}

#[tokio::test]
async fn pictures_go_along_only_if_the_agent_takes_them() {
    let mut s = setup(|_| {}).await;
    let (session, reply) = first_turn(&mut s, "hello").await;
    turn_ends(&s.f, &session.id, "end_turn").await;
    final_update(&mut s.f.rx, &reply.id).await;
    ack_result(s.f.hub.handle(chat_msg("what's this?", None, vec![png()]), &actor()).await);
    assert!(pet_msg(&mut s.f.rx).await.text.contains("can't see pictures"));
    assert_eq!(prompts(&s.f, &session.id).len(), 1);
    s.f.fake.images_ok.store(true, std::sync::atomic::Ordering::SeqCst);
    ack_result(s.f.hub.handle(chat_msg("what's this?", None, vec![png()]), &actor()).await);
    assert!(pet_msg(&mut s.f.rx).await.streaming);
    assert_eq!(prompts(&s.f, &session.id).last().map(String::as_str), Some("what's this? [images: image/png]"));
}

#[tokio::test]
async fn a_spoken_message_gets_a_short_spoken_form() {
    let mut s = setup(|_| {}).await;
    ack_result(s.f.hub.handle(chat_msg("make a todo app", Some("voice"), Vec::new()), &actor()).await);
    let reply = pet_msg(&mut s.f.rx).await;
    let sid = reply.session_id.clone().unwrap();
    assert!(prompts(&s.f, &sid)[0].contains(pawl_brain::chat::assistant::SPOKEN_NOTE));
    agent_says(&s.f, &sid, "I made **todo-app** in your Pawl folder. Open https://localhost:5173 to try it. It has three screens.\n```\nnpm run dev\n```").await;
    turn_ends(&s.f, &sid, "end_turn").await;
    let done = final_update(&mut s.f.rx, &reply.id).await;
    assert!(done.text.contains("```"), "the chat keeps the whole reply");
    assert_eq!(done.speech.as_deref(), Some("I made todo-app in your Pawl folder. Open a link to try it."));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(s.f.hub.store().recent_chat(10).unwrap().iter().all(|m| m.speech.is_none()), "never stored");
}

#[tokio::test]
async fn turning_it_off_or_changing_its_agent_ends_the_session_and_other_pets_are_unchanged() {
    let mut s = setup(|_| {}).await;
    let (session, _) = first_turn(&mut s, "hello").await;
    let patch: HubSettingsPatch = serde_json::from_value(json!({ "assistant": { "agent": "codex" } })).unwrap();
    s.f.hub.update_settings(&patch, &actor()).await.unwrap();
    assert!(s.f.hub.session(&session.id).unwrap().ended_at.is_some(), "a new agent: the session ends");
    let (two, _) = first_turn(&mut s, "hello").await;
    assert_eq!(two.agent_id, "codex");
    // Codex starts in its read-only mode (every approval reviewed by the user), with no Claude-only options.
    let started = s.f.fake.started.lock().unwrap().last().cloned().unwrap();
    assert_eq!(started.env.unwrap_or_default().get("INITIAL_AGENT_MODE").map(String::as_str), Some("read-only"));
    let opts = s.f.fake.options.lock().unwrap().last().cloned().unwrap();
    assert!(opts.meta.is_none() && !opts.clean_env);
    assert!(s.f.hub.settings().assistant.agent_note.is_some(), "a caution for an agent Pawl cannot lock down");
    let patch: HubSettingsPatch = serde_json::from_value(json!({ "assistant": { "enabled": false } })).unwrap();
    s.f.hub.update_settings(&patch, &actor()).await.unwrap();
    assert!(s.f.hub.session(&two.id).unwrap().ended_at.is_some());
    assert_eq!(s.f.hub.settings().assistant.state, AssistantState::Off);
    // Off: today's brain answers, and its prompt says where to turn the assistant on.
    say(&s.f, "make me a todo app").await;
    let r = pet_msg(&mut s.f.rx).await;
    assert!(!r.streaming && r.session_id.is_none(), "{r:?}");
    let starts = s.f.fake.calls().iter().filter(|c| c.starts_with("start ")).count();
    assert_eq!(starts, 2, "no assistant session while it is off");
    // The workspace may never overlap Pawl's own data.
    let inside = s.f.hub.paths().data_dir.join("ws").to_string_lossy().into_owned();
    let patch: HubSettingsPatch = serde_json::from_value(json!({ "assistant": { "workspace": inside } })).unwrap();
    let e = s.f.hub.update_settings(&patch, &actor()).await.unwrap_err();
    assert!(format!("{e:#}").starts_with("invalid:assistant.workspace"), "{e:#}");
}

#[tokio::test]
async fn every_session_end_replaces_the_token_before_the_next_start() {
    let mut s = setup(|_| {}).await;
    let (one, _) = first_turn(&mut s, "hello").await;
    assert_eq!(s.f.hub.assistant_session_id().as_deref(), Some(one.id.as_str()));
    assert_eq!(s.f.hub.assistant_token_state(), (false, None));
    // The session ends on its own (idle close, crash): the token is due and is replaced before anything new starts.
    s.f.hub.close_session(&one.id).await.unwrap();
    assert_eq!(s.f.hub.assistant_session_id(), None);
    let (two, _) = first_turn(&mut s, "hello again").await;
    assert_ne!(two.id, one.id);
    assert_eq!(s.f.hub.assistant_token_state(), (false, None), "replaced before the new start");
}

#[tokio::test]
async fn a_token_that_cannot_be_replaced_keeps_the_assistant_from_starting_with_its_tools() {
    let port = super::a2a_server::free_port();
    let mut s = setup(|c| {
        c.assistant.sessions = true;
        c.a2a.server = true;
        c.a2a.port = port;
        c.a2a.callers = vec![crate::config::A2aCaller {
            id: crate::settings::ASSISTANT_CALLER_ID.into(),
            name: "Pawl assistant".into(),
            token: Some("t".repeat(43)),
            allow_approvals: false,
            connect_target: Some(crate::config::ConnectTarget::Assistant),
            connect_added_servers: false,
        }];
    })
    .await;
    // config.toml cannot be written: replacing the token fails (twice), and says so.
    let cfg = s.f.hub.paths().config.clone();
    let _ = std::fs::remove_file(&cfg);
    std::fs::create_dir_all(&cfg).unwrap();
    ack_result(s.f.hub.handle(ClientMsg::AssistantReset { id: "r".into() }, &actor()).await);
    let (due, err) = s.f.hub.assistant_token_state();
    assert!(due && err.as_deref().is_some_and(|e| e.contains("could not be replaced")), "{err:?}");
    assert_eq!(s.f.hub.settings().assistant.state, AssistantState::Error);
    // No start with Pawl's tools until it works: the brain answers with the reason.
    say(&s.f, "make me a todo app").await;
    let r = pet_msg(&mut s.f.rx).await;
    assert!(r.text.contains("old A2A token could not be replaced") && r.text.contains("Settings > Assistant"), "{}", r.text);
    assert!(!s.f.fake.calls().iter().any(|c| c.starts_with("start ")));
    // Fixed: the next message replaces it first, then starts.
    std::fs::remove_dir_all(&cfg).unwrap();
    let (session, _) = first_turn(&mut s, "make me a todo app").await;
    assert_eq!(session.purpose.as_deref(), Some("assistant"));
    assert_eq!(s.f.hub.assistant_token_state(), (false, None));
}

#[tokio::test]
async fn a_settings_change_during_a_start_ends_what_it_started() {
    let mut s = setup(|_| {}).await;
    s.f.fake.start_delay_ms.store(300, std::sync::atomic::Ordering::SeqCst);
    say(&s.f, "hello").await;
    // While the first start (Claude Code) is under way, the agent changes to Codex.
    let mut starting = None;
    for _ in 0..50 {
        starting = s.f.hub.assistant_starting_id();
        if starting.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let starting = starting.expect("a start is under way");
    // A snapshot of it without a purpose (as the host sends) still counts as the assistant's: no pet appears.
    let now = pawl_protocol::now_ms();
    let snap = Session {
        id: starting.clone(),
        machine_id: MID.into(),
        agent_id: "claude-code".into(),
        acp_session_id: None,
        external_id: None,
        cwd: s.ws.to_string_lossy().into_owned(),
        title: None,
        source: SessionSource::Owned,
        status: SessionStatus::Idle,
        started_at: now,
        last_event_at: now,
        ended_at: None,
        purpose: None,
    };
    s.f.hub.on_host_event(HostEvent::Session(snap)).await;
    assert_eq!(s.f.hub.session(&starting).unwrap().purpose.as_deref(), Some("assistant"));
    assert!(f_pets_lack(&s.f, "claude-code"));
    let patch: HubSettingsPatch = serde_json::from_value(json!({ "assistant": { "agent": "codex" } })).unwrap();
    s.f.hub.update_settings(&patch, &actor()).await.unwrap();
    let reply = pet_msg(&mut s.f.rx).await;
    let live = s.f.hub.session(reply.session_id.as_deref().unwrap()).unwrap();
    assert_eq!(live.agent_id, "codex", "the new settings were used");
    assert!(s.f.hub.session(&starting).unwrap().ended_at.is_some(), "the stale session was ended at once");
    assert!(s.f.fake.calls().iter().any(|c| c == &format!("close {starting}")));
    assert_eq!(s.f.hub.assistant_session_id().as_deref(), Some(live.id.as_str()));
    assert!(f_pets_lack(&s.f, "claude-code") && f_pets_lack(&s.f, "codex"));
}

#[tokio::test]
async fn two_messages_at_once_make_one_turn_and_one_busy_reply() {
    let mut s = setup(|_| {}).await;
    let (session, reply) = first_turn(&mut s, "hello").await;
    turn_ends(&s.f, &session.id, "end_turn").await;
    final_update(&mut s.f.rx, &reply.id).await;
    let who = actor();
    let (a, b) = tokio::join!(
        s.f.hub.handle(chat_msg("first", None, Vec::new()), &who),
        s.f.hub.handle(chat_msg("second", None, Vec::new()), &who)
    );
    ack_result(a);
    ack_result(b);
    let (x, y) = (pet_msg(&mut s.f.rx).await, pet_msg(&mut s.f.rx).await);
    let (turn, busy) = if x.streaming { (x, y) } else { (y, x) };
    assert!(turn.streaming && !busy.streaming, "{turn:?} {busy:?}");
    assert_eq!(busy.text, "I'm still on your last message; say stop to cancel it.");
    assert_eq!(prompts(&s.f, &session.id).len(), 2, "one of the two was prompted");
    turn_ends(&s.f, &session.id, "end_turn").await;
    final_update(&mut s.f.rx, &turn.id).await;
    // No reply row is left empty (an empty one would stream forever after a reload).
    assert!(s.f.hub.store().recent_chat(20).unwrap().iter().all(|m| m.role != ChatRole::Pet || !m.text.is_empty()));
}

#[tokio::test]
async fn a_long_reply_is_cut_short_and_saved_before_it_is_announced() {
    let mut s = setup(|_| {}).await;
    let (session, reply) = first_turn(&mut s, "write a novel").await;
    let chunk = "é".repeat(10_000);
    for _ in 0..8 {
        agent_says(&s.f, &session.id, &chunk).await;
    }
    turn_ends(&s.f, &session.id, "end_turn").await;
    let done = final_update(&mut s.f.rx, &reply.id).await;
    assert!(done.text.ends_with("(Reply cut short.)"));
    assert!(done.text.len() <= crate::core::assistant::MAX_REPLY_BYTES + 40, "{}", done.text.len());
    // The final write happened before the final update went out.
    let stored = s.f.hub.store().recent_chat(5).unwrap();
    assert!(stored.iter().any(|m| m.id == reply.id && m.text == done.text));
}

#[tokio::test]
async fn its_session_is_remote_driven_so_auto_allow_rules_ask_and_its_title_is_the_users() {
    let mut s = setup(|c| c.rules = Some(vec![rule("reads", Some(ToolKind::Read), RuleAction::AutoAllow)])).await;
    // An ordinary session still gets the user's auto-allow.
    let plain = start(&s.f).await;
    let r = permission(&plain.id, ToolKind::Read);
    s.f.hub.on_host_event(HostEvent::PermissionRequest(r.clone())).await;
    assert!(s.f.fake.calls().iter().any(|c| c == &format!("resolve {} allow", r.request_id)));
    let (session, _) = first_turn(&mut s, "make me a todo app").await;
    assert!(s.f.hub.st().remote_driven.contains(&session.id));
    let mut read = permission(&session.id, ToolKind::Read);
    read.agent_id = "claude-code".into();
    s.f.hub.on_host_event(HostEvent::PermissionRequest(read.clone())).await;
    assert!(s.f.hub.pending_permissions().iter().any(|p| p.request_id == read.request_id), "asks instead");
    // The title is what the user said, never the persona (from the prompt event and from the host's title).
    let first = prompts(&s.f, &session.id).remove(0);
    assert!(first.starts_with("You are "));
    s.f.hub
        .on_host_event(HostEvent::Event { session_id: session.id.clone(), kind: EventType::UserMessage, payload: json!({ "text": first.clone() }) })
        .await;
    let mut snap = s.f.hub.session(&session.id).unwrap();
    snap.title = Some(first);
    s.f.hub.on_host_event(HostEvent::Session(snap)).await;
    let title = s.f.hub.session(&session.id).unwrap().title.unwrap();
    assert!(title.contains("todo app") && !title.contains("You are"), "{title}");
}

#[tokio::test]
async fn a_prompt_from_elsewhere_gets_its_reply_in_the_chat_and_grows_no_pet() {
    let mut s = setup(|_| {}).await;
    // Claude Code has a pet from an ordinary session; the assistant's work never grows it.
    let cwd = s.f.dir.path().to_string_lossy().into_owned();
    let start = ClientMsg::StartSession { machine_id: None, confirmed: None, id: "1".into(), agent_id: "claude-code".into(), cwd, prompt: None };
    ack_result(s.f.hub.handle(start, &actor()).await);
    let xp = |f: &Fixture| f.hub.pets().into_iter().find(|p| p.agent_id == "claude-code").map(|p| (p.xp, p.mood));
    let before = xp(&s.f);
    assert!(before.is_some());
    let (session, reply) = first_turn(&mut s, "hello").await;
    turn_ends(&s.f, &session.id, "end_turn").await;
    final_update(&mut s.f.rx, &reply.id).await;
    // A prompt from the session view (or a paired phone): the agent's answer reaches the chat as a reply too.
    s.f.hub.prompt(&session.id, "status?".into()).await.unwrap();
    s.f.hub
        .on_host_event(HostEvent::Event { session_id: session.id.clone(), kind: EventType::UserMessage, payload: json!({ "text": "status?" }) })
        .await;
    let other = pet_msg(&mut s.f.rx).await;
    assert!(other.streaming && other.session_id.as_deref() == Some(session.id.as_str()));
    agent_says(&s.f, &session.id, "All quiet.").await;
    turn_ends(&s.f, &session.id, "end_turn").await;
    assert_eq!(final_update(&mut s.f.rx, &other.id).await.text, "All quiet.");
    assert_eq!(xp(&s.f), before, "the assistant's events grow no other pet");
}

#[tokio::test]
async fn workspace_links_are_judged_where_they_lead() {
    let root = tempfile::tempdir().unwrap();
    let link = root.path().join("ws-link");
    let mut s = setup(|_| {}).await;
    // A link from the chosen folder into Pawl's own data folder: refused at the start, nothing started.
    let data = s.f.hub.paths().data_dir.clone();
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&data, &link).is_ok();
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_dir(&data, &link).is_ok();
    if !linked {
        eprintln!("skipped: this machine cannot make folder links");
        return;
    }
    s.f.hub.inner.config.write().unwrap().assistant.workspace = link.to_string_lossy().into_owned();
    say(&s.f, "hi").await;
    let r = pet_msg(&mut s.f.rx).await;
    assert!(r.text.contains("overlaps Pawl's own data folder"), "{}", r.text);
    assert!(!s.f.fake.calls().iter().any(|c| c.starts_with("start ")));
}

#[tokio::test]
async fn a_stale_session_that_will_not_close_is_kept_and_closed_before_the_next_start() {
    let mut s = setup(|_| {}).await;
    s.f.fake.start_delay_ms.store(300, std::sync::atomic::Ordering::SeqCst);
    s.f.fake.close_fails.store(true, std::sync::atomic::Ordering::SeqCst);
    say(&s.f, "hello").await;
    let mut starting = None;
    for _ in 0..50 {
        starting = s.f.hub.assistant_starting_id();
        if starting.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let starting = starting.expect("a start is under way");
    let patch: HubSettingsPatch = serde_json::from_value(json!({ "assistant": { "agent": "codex" } })).unwrap();
    s.f.hub.update_settings(&patch, &actor()).await.unwrap();
    // It could not be closed: not used, not forgotten, and the reply says why.
    let r = pet_msg(&mut s.f.rx).await;
    assert!(r.text.contains("previous session could not be closed"), "{}", r.text);
    assert_eq!(s.f.hub.assistant_session_id(), None, "never reused while it waits to be closed");
    let closes = s.f.fake.calls().iter().filter(|c| *c == &format!("close {starting}")).count();
    assert!(closes >= 2, "retried: {closes}");
    // The agent lets go: the next message closes it first, then starts with the new settings.
    s.f.fake.start_delay_ms.store(0, std::sync::atomic::Ordering::SeqCst);
    s.f.fake.close_fails.store(false, std::sync::atomic::Ordering::SeqCst);
    let (live, _) = first_turn(&mut s, "hello again").await;
    assert_eq!(live.agent_id, "codex");
    assert!(s.f.hub.session(&starting).unwrap().ended_at.is_some());
}

#[tokio::test]
async fn agent_config_in_the_workspace_keeps_another_agent_from_starting_there() {
    let mut s = setup(|c| c.assistant.agent = "codex".into()).await;
    std::fs::create_dir_all(s.ws.join(".codex")).unwrap();
    say(&s.f, "hello").await;
    let r = pet_msg(&mut s.f.rx).await;
    assert!(r.text.contains("There's agent config in your workspace (") && r.text.contains("Remove it or use Claude Code"), "{}", r.text);
    let view = s.f.hub.settings().assistant;
    assert_eq!(view.state, AssistantState::Error);
    assert!(view.error.unwrap().ends_with("Remove it or use Claude Code."));
    assert!(!s.f.fake.calls().iter().any(|c| c.starts_with("start ")));
    // Claude Code loads no project config: it starts there.
    let patch: HubSettingsPatch = serde_json::from_value(json!({ "assistant": { "agent": "claude-code" } })).unwrap();
    s.f.hub.update_settings(&patch, &actor()).await.unwrap();
    let (session, _) = first_turn(&mut s, "hello").await;
    assert_eq!(session.agent_id, "claude-code");
}

#[tokio::test]
async fn a_git_setting_that_runs_a_program_keeps_even_claude_from_starting() {
    let mut s = setup(|_| {}).await;
    std::fs::create_dir_all(s.ws.join(".git")).unwrap();
    std::fs::write(s.ws.join(".git").join("config"), "[core]\n\tfsmonitor = ./evil.sh\n").unwrap();
    say(&s.f, "hello").await;
    let r = pet_msg(&mut s.f.rx).await;
    assert!(r.text.contains("git setting that runs a program"), "{}", r.text);
    assert!(!s.f.fake.calls().iter().any(|c| c.starts_with("start ")));
}

#[tokio::test]
async fn the_token_is_replaced_once_when_the_hub_starts() {
    let port = super::a2a_server::free_port();
    let old = "t".repeat(43);
    let s = setup(|c| {
        c.a2a.port = port;
        c.a2a.callers = vec![crate::config::A2aCaller {
            id: crate::settings::ASSISTANT_CALLER_ID.into(),
            name: "Pawl assistant".into(),
            token: Some(old.clone()),
            allow_approvals: false,
            connect_target: Some(crate::config::ConnectTarget::Assistant),
            connect_added_servers: false,
        }];
    })
    .await;
    let mut token = None;
    for _ in 0..100 {
        token = s.f.hub.config().a2a.callers.iter().find(|c| c.id == crate::settings::ASSISTANT_CALLER_ID).and_then(|c| c.token.clone());
        if token.as_deref() != Some(old.as_str()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(token.is_some_and(|t| t != old), "a crashed hub's agent cannot keep using the stored token");
    assert_eq!(s.f.hub.assistant_token_state(), (false, None));
}

#[tokio::test]
async fn with_the_assistant_on_a_bare_yes_never_answers_a_card() {
    let mut s = setup(|_| {}).await;
    let (session, reply) = first_turn(&mut s, "tidy my notes").await;
    turn_ends(&s.f, &session.id, "end_turn").await;
    final_update(&mut s.f.rx, &reply.id).await;
    // Even a read, typed or spoken: the pet says how to answer, nothing is resolved, nothing is prompted.
    let mut read = permission(&session.id, ToolKind::Read);
    read.agent_id = "claude-code".into();
    s.f.hub.on_host_event(HostEvent::PermissionRequest(read.clone())).await;
    let before = prompts(&s.f, &session.id).len();
    for (text, via) in [("yes", None), ("sure", None), ("yes", Some("voice"))] {
        ack_result(s.f.hub.handle(chat_msg(text, via, Vec::new()), &actor()).await);
        let r = pet_msg(&mut s.f.rx).await;
        assert!(r.text.starts_with("There's a card waiting: tap Allow or Deny on it, or say “approve”"), "{text} {via:?}: {}", r.text);
    }
    assert!(!s.f.fake.calls().iter().any(|c| c.starts_with(&format!("resolve {}", read.request_id))));
    assert!(s.f.hub.pending_permissions().iter().any(|p| p.request_id == read.request_id));
    assert_eq!(prompts(&s.f, &session.id).len(), before);
    // "approve" answers it as today.
    say(&s.f, "approve").await;
    pet_msg(&mut s.f.rx).await;
    assert!(s.f.fake.calls().iter().any(|c| c == &format!("resolve {} allow", read.request_id)));
}

#[tokio::test]
async fn with_the_assistant_off_a_bare_yes_answers_as_before() {
    let mut s = setup(|c| c.assistant.enabled = false).await;
    let session = start(&s.f).await;
    let r = permission(&session.id, ToolKind::Read);
    s.f.hub.on_host_event(HostEvent::PermissionRequest(r.clone())).await;
    say(&s.f, "yes").await;
    pet_msg(&mut s.f.rx).await;
    assert!(s.f.fake.calls().iter().any(|c| c == &format!("resolve {} allow", r.request_id)), "{:?}", s.f.fake.calls());
}
