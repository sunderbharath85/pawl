use super::*;
use pet_protocol::{
    AgentSource, EventType, PermissionOption, PermissionOptionKind, Species, ToolCallPayload, ToolKind,
};
use serde_json::json;

fn agent(id: &str, name: &str, installed: bool) -> Agent {
    Agent {
        id: id.into(),
        name: name.into(),
        command: vec!["x".into()],
        env: None,
        version: None,
        description: None,
        icon: None,
        installed,
        source: AgentSource::Registry,
        capabilities: None,
    }
}

fn session(id: &str, agent: &str, cwd: &str, source: SessionSource, status: SessionStatus) -> Session {
    let now = now_ms();
    Session {
        id: id.into(),
        agent_id: agent.into(),
        acp_session_id: None,
        external_id: None,
        cwd: cwd.into(),
        title: None,
        source,
        status,
        started_at: now - 3_600_000,
        last_event_at: now - 60_000,
        ended_at: None,
    }
}

fn event(id: i64, session: &str, kind: EventType, payload: serde_json::Value) -> HubEvent {
    HubEvent { id, session_id: session.into(), kind, payload, ts: now_ms() - 100_000 + id }
}

fn request(id: &str, session: &str, agent: &str, title: &str, created: i64) -> PermissionRequest {
    let opt = |id: &str, kind| PermissionOption { option_id: id.into(), name: id.into(), kind };
    PermissionRequest {
        request_id: id.into(),
        session_id: session.into(),
        agent_id: agent.into(),
        tool_call: ToolCallPayload {
            tool_call_id: format!("tc-{id}"),
            title: Some(title.into()),
            kind: Some(ToolKind::Execute),
            ..Default::default()
        },
        options: vec![
            opt("allow-once", PermissionOptionKind::AllowOnce),
            opt("allow-always", PermissionOptionKind::AllowAlways),
            opt("reject-once", PermissionOptionKind::RejectOnce),
            opt("reject-always", PermissionOptionKind::RejectAlways),
        ],
        created_at: created,
    }
}

fn ctx() -> BrainContext {
    BrainContext {
        agents: vec![
            agent("claude-code", "Claude Code", true),
            agent("codex", "Codex", true),
            agent("gemini", "Gemini CLI", false),
        ],
        sessions: vec![
            session("s-claude", "claude-code", "E:\\workspace\\ai-pet", SessionSource::Owned, SessionStatus::Working),
            session("s-hook", "copilot", "/home/me/api", SessionSource::VscodeHook, SessionStatus::Done),
        ],
        recent_events: vec![
            event(1, "s-claude", EventType::UserMessage, json!({"text": "fix the build"})),
            event(
                2,
                "s-claude",
                EventType::ToolCall,
                json!({"toolCallId": "t1", "title": "Run cargo build", "kind": "execute", "status": "in_progress"}),
            ),
        ],
        pending: vec![],
        pet: None,
    }
}

fn brain() -> Brain {
    Brain::new(BrainConfig::default())
}

async fn ask(text: &str, c: &BrainContext) -> BrainReply {
    brain().handle(text, c).await.expect("handle never errors")
}

#[tokio::test]
async fn status_summarizes_live_session() {
    let c = ctx();
    let r = ask("What's Claude doing?", &c).await;
    assert_eq!(r.action, None);
    assert!(r.text.contains("Claude Code in ai-pet is working on: Run cargo build."), "{}", r.text);
    assert!(r.text.starts_with('*'), "dog action first: {}", r.text);

    let r = ask("status", &c).await;
    assert!(r.text.contains("2 sessions open, 1 busy"), "{}", r.text);
    assert!(r.text.contains("• Claude Code · ai-pet: working: Run cargo build"), "{}", r.text);
    assert!(r.text.contains("• Copilot · api: done"), "{}", r.text);

    let r = ask("what's codex doing?", &c).await;
    assert!(r.text.contains("haven't seen Codex"), "{}", r.text);
    let r = ask("is goose busy?", &c).await;
    assert!(r.text.contains("haven't seen Goose"), "{}", r.text);
}

#[tokio::test]
async fn command_starts_session_in_repo_resolved_by_basename() {
    let c = ctx();
    let r = ask("Tell Codex to run the tests in repo ai-pet", &c).await;
    assert_eq!(
        r.action,
        Some(BrainAction::StartSession {
            agent_id: "codex".into(),
            cwd: "E:\\workspace\\ai-pet".into(),
            prompt: "run the tests".into(),
        })
    );
    assert!(r.text.contains("ai-pet"), "{}", r.text);
}

#[tokio::test]
async fn command_prompts_existing_idle_owned_session() {
    let mut c = ctx();
    c.sessions.push(session("s-codex", "codex", "E:/workspace/ai-pet/", SessionSource::Owned, SessionStatus::Done));
    let r = ask("tell codex to run the tests in repo ai-pet", &c).await;
    assert_eq!(r.action, Some(BrainAction::Prompt { session_id: "s-codex".into(), text: "run the tests".into() }));
    // No repo given: the agent's only live session is used.
    let r = ask("codex, now fix the failures", &c).await;
    assert_eq!(r.action, Some(BrainAction::Prompt { session_id: "s-codex".into(), text: "now fix the failures".into() }));
    // "start a new … session" always starts.
    let r = ask("start a new codex session in ai-pet to lint", &c).await;
    assert!(matches!(r.action, Some(BrainAction::StartSession { ref prompt, .. }) if prompt == "lint"), "{r:?}");
}

#[tokio::test]
async fn command_refuses_busy_unknown_or_uninstalled() {
    let c = ctx();
    // Claude is working in ai-pet: no action, explain.
    let r = ask("tell claude to also update the docs in ai-pet", &c).await;
    assert_eq!(r.action, None);
    assert!(r.text.contains("busy"), "{}", r.text);
    // Unknown repo.
    let r = ask("tell codex to run the tests in repo nowhere", &c).await;
    assert_eq!(r.action, None);
    assert!(r.text.contains("“nowhere”"), "{}", r.text);
    // Not installed.
    let r = ask("tell gemini to explain main.rs in ai-pet", &c).await;
    assert_eq!(r.action, None);
    assert!(r.text.contains("isn't installed"), "{}", r.text);
    // Agent only known from hooks.
    let r = ask("tell copilot to fix it in api", &c).await;
    assert_eq!(r.action, None);
    assert!(r.text.contains("hooks"), "{}", r.text);
    // Ambiguous repo: two different live cwds and none named.
    let r = ask("tell codex to run the tests", &c).await;
    assert_eq!(r.action, None);
    assert!(r.text.contains("Which repo"), "{}", r.text);
    // Hook sessions are never prompted; a new owned session starts instead.
    let r = ask("tell claude to check the api in repo api", &c).await;
    assert!(matches!(r.action, Some(BrainAction::StartSession { ref cwd, .. }) if cwd == "/home/me/api"), "{r:?}");
}

#[tokio::test]
async fn approve_and_deny_single_pending_request() {
    let mut c = ctx();
    c.pending.push(request("r1", "s-claude", "claude-code", "Run cargo test", 1));
    let r = ask("approve", &c).await;
    assert_eq!(
        r.action,
        Some(BrainAction::ResolvePermission { request_id: "r1".into(), option_id: Some("allow-once".into()) })
    );
    assert!(r.text.contains("Run cargo test"), "{}", r.text);
    let r = ask("yes please", &c).await;
    assert_eq!(
        r.action,
        Some(BrainAction::ResolvePermission { request_id: "r1".into(), option_id: Some("allow-once".into()) })
    );
    let r = ask("always allow it", &c).await;
    assert_eq!(
        r.action,
        Some(BrainAction::ResolvePermission { request_id: "r1".into(), option_id: Some("allow-always".into()) })
    );
    let r = ask("deny", &c).await;
    assert_eq!(
        r.action,
        Some(BrainAction::ResolvePermission { request_id: "r1".into(), option_id: Some("reject-once".into()) })
    );
    // No reject option -> cancelled outcome.
    c.pending[0].options.retain(|o| o.kind == PermissionOptionKind::AllowOnce);
    let r = ask("no", &c).await;
    assert_eq!(r.action, Some(BrainAction::ResolvePermission { request_id: "r1".into(), option_id: None }));
}

#[tokio::test]
async fn multiple_pending_requests_need_a_choice() {
    let mut c = ctx();
    c.sessions.push(session("s-codex", "codex", "/w/web", SessionSource::Owned, SessionStatus::WaitingPermission));
    c.pending.push(request("r1", "s-claude", "claude-code", "Run cargo test", 1));
    c.pending.push(request("r2", "s-codex", "codex", "rm -rf build", 2));
    let r = ask("approve", &c).await;
    assert_eq!(r.action, None);
    assert!(r.text.contains("1. Claude Code: Run cargo test (ai-pet)"), "{}", r.text);
    assert!(r.text.contains("2. Codex: rm -rf build (web)"), "{}", r.text);
    let r = ask("approve codex", &c).await;
    assert_eq!(
        r.action,
        Some(BrainAction::ResolvePermission { request_id: "r2".into(), option_id: Some("allow-once".into()) })
    );
    let r = ask("deny 1", &c).await;
    assert_eq!(
        r.action,
        Some(BrainAction::ResolvePermission { request_id: "r1".into(), option_id: Some("reject-once".into()) })
    );
    let r = ask("approve 7", &c).await;
    assert_eq!(r.action, None);
    let r = ask("approve gemini", &c).await;
    assert_eq!(r.action, None);
    assert!(r.text.contains("isn't waiting"), "{}", r.text);
    // Agent status mentions its pending request.
    let r = ask("how's codex?", &c).await;
    assert!(r.text.contains("Needs your OK: rm -rf build"), "{}", r.text);
}

#[tokio::test]
async fn nothing_pending() {
    let r = ask("approve", &ctx()).await;
    assert_eq!(r.action, None);
    assert!(r.text.contains("Nothing is waiting"), "{}", r.text);
}

#[tokio::test]
async fn help_report_and_small_talk_are_deterministic_without_model() {
    let c = ctx();
    let r = ask("help", &c).await;
    assert!(r.text.contains("approve") && r.text.contains("daily report"), "{}", r.text);
    assert!(r.text.contains("repo ai-pet"), "{}", r.text);
    let r = ask("daily report", &c).await;
    assert!(r.text.contains("Daily report: 2 sessions across 2 agents in 2 repos"), "{}", r.text);
    assert_eq!(r.action, None);
    for text in ["hi!", "who's a good boy?", "thanks", "how are you?", "asdf qwerty", "", "   "] {
        let r = ask(text, &c).await;
        assert_eq!(r.action, None, "{text}");
        assert!(!r.text.is_empty(), "{text}");
    }
    let r = ask("who's a good boy?", &c).await;
    assert!(r.text.contains('*'), "{}", r.text);
}

#[tokio::test]
async fn pet_species_and_name_shape_the_voice() {
    let mut c = ctx();
    c.pet = Some(Pet {
        id: "pet-codex".into(),
        agent_id: "codex".into(),
        species: Species::Cat,
        skin: "default".into(),
        name: "Tofu".into(),
        personality: "aloof".into(),
        mood: 70,
        xp: 0,
    });
    let r = ask("hello", &c).await;
    assert!(r.text.contains("Tofu here"), "{}", r.text);
    assert!(!r.text.contains("Woof"), "{}", r.text);
    // Pet name prefix is ignored by the parser.
    c.pending.push(request("r1", "s-claude", "claude-code", "Run cargo test", 1));
    let r = ask("Tofu, approve", &c).await;
    assert!(matches!(r.action, Some(BrainAction::ResolvePermission { .. })), "{r:?}");
}

#[tokio::test]
async fn failing_model_falls_back_and_never_acts() {
    // Nothing listens on port 9: the LLM call fails fast and the reply is deterministic.
    let b = Brain::new(BrainConfig {
        ollama_url: Some("http://127.0.0.1:9".into()),
        ollama_model: Some("none".into()),
        ..BrainConfig::default()
    });
    assert!(b.has_llm());
    let c = ctx();
    let r = b.handle("tell me a joke about rust", &c).await.unwrap();
    assert_eq!(r.action, None);
    assert!(!r.text.is_empty());
    let r = b.handle("what's claude doing", &c).await.unwrap();
    assert!(r.text.contains("Run cargo build"), "{}", r.text);
    let s = b.summarize_session(&c.sessions[0], &c.recent_events).await.unwrap();
    assert!(s.starts_with("Claude Code in ai-pet is working"), "{s}");
    let d = b.daily_report(&c.sessions, &c.recent_events).await.unwrap();
    assert!(d.contains("Daily report"), "{d}");
}

#[tokio::test]
async fn public_summaries_without_model() {
    let b = brain();
    let c = ctx();
    let s = b.summarize_session(&c.sessions[0], &c.recent_events).await.unwrap();
    assert_eq!(s, "Claude Code in ai-pet is working on: Run cargo build. 1 tool call so far.");
    let d = b.daily_report(&[], &[]).await.unwrap();
    assert!(d.contains("Quiet day"), "{d}");
}

#[test]
fn config_debug_redacts_key_and_types_are_send() {
    let cfg = BrainConfig { anthropic_api_key: Some("sk-ant-secret".into()), ..BrainConfig::default() };
    assert!(cfg.has_model());
    assert!(!BrainConfig::default().has_model());
    let dbg = format!("{cfg:?} {:?}", Brain::new(cfg.clone()));
    assert!(!dbg.contains("sk-ant-secret"), "{dbg}");
    assert_eq!(BrainConfig::default().model, "claude-haiku-5-5");

    fn assert_send_sync<T: Send + Sync>(_: &T) {}
    fn assert_send<T: Send>(_: T) {}
    let b = brain();
    assert_send_sync(&b);
    let c = ctx();
    assert_send(b.handle("status", &c));
    assert_send(b.summarize_session(&c.sessions[0], &c.recent_events));
    assert_send(b.daily_report(&c.sessions, &c.recent_events));
}

#[test]
fn action_wire_shape() {
    let a = BrainAction::StartSession { agent_id: "codex".into(), cwd: "/r".into(), prompt: "go".into() };
    assert_eq!(
        serde_json::to_value(&a).unwrap(),
        json!({"type": "start_session", "agentId": "codex", "cwd": "/r", "prompt": "go"})
    );
    let cfg: BrainConfig = serde_json::from_value(json!({"model": "claude-haiku-5-5"})).unwrap();
    assert_eq!(cfg, BrainConfig::default());
}
