//! Settings view, validation table and config.toml round-trip tests (pure; temp dirs only).

use super::*;
use pawl_protocol::{
    HubSettingsBotPatch, HubSettingsHubPatch, HubSettingsPeerPatch, HubSettingsRelayPatch, HubSettingsReposPatch,
    HubSettingsWatchdogPatch,
};
use serde_json::json;

const SECRET: &str = "sk-ant-test-0123456789abcdef";
const RELAY_SECRET: &str = "rk_0123456789abcdefABCDEF";

fn patch(v: serde_json::Value) -> HubSettingsPatch {
    serde_json::from_value(v).expect("patch parses")
}

fn reserved() -> Vec<String> {
    vec!["claude-code".into(), "codex".into(), "gemini".into()]
}

fn err_of(cur: &HubConfig, p: serde_json::Value) -> InvalidSetting {
    apply_patch(cur, &patch(p), &reserved()).expect_err("patch must be refused")
}

/// Contract-level: every refusal names the camelCase wire path of the field (with list indices)
/// and reads `invalid:<field>: <reason>`.
#[test]
fn validation_table_names_camel_case_fields() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_string_lossy().into_owned();
    let missing = dir.path().join("nope").to_string_lossy().into_owned();
    let file = dir.path().join("f.txt");
    std::fs::write(&file, "x").unwrap();
    let file = file.to_string_lossy().into_owned();
    let cur = HubConfig::default();
    let agent = |id: &str, cmd: serde_json::Value| json!({ "id": id, "name": "A", "command": cmd });
    let table: Vec<(serde_json::Value, &str)> = vec![
        (json!({ "hub": { "port": 80 } }), "hub.port"),
        (json!({ "hub": { "port": 0 } }), "hub.port"),
        (json!({ "hub": { "hookSessionStaleHours": 9000 } }), "hub.hookSessionStaleHours"),
        (json!({ "hub": { "ownedSessionIdleHours": 100000 } }), "hub.ownedSessionIdleHours"),
        (json!({ "hub": { "eventRetentionDays": 5000 } }), "hub.eventRetentionDays"),
        (json!({ "hub": { "dailyReportTime": "25:00" } }), "hub.dailyReportTime"),
        (json!({ "hub": { "dailyReportTime": "9:00" } }), "hub.dailyReportTime"),
        (json!({ "hub": { "dailyReportTime": "noon" } }), "hub.dailyReportTime"),
        (json!({ "bot": { "model": "" } }), "bot.model"),
        (json!({ "bot": { "model": "claude haiku" } }), "bot.model"),
        (json!({ "bot": { "anthropicApiKey": "" } }), "bot.anthropicApiKey"),
        (json!({ "bot": { "anthropicApiKey": "has space" } }), "bot.anthropicApiKey"),
        (json!({ "bot": { "ollamaUrl": "ftp://x" } }), "bot.ollamaUrl"),
        (json!({ "bot": { "ollamaUrl": "http://user:pw@host:11434" } }), "bot.ollamaUrl"),
        (json!({ "bot": { "ollamaUrl": "http://host:99999" } }), "bot.ollamaUrl"),
        (json!({ "bot": { "ollamaModel": "a b" } }), "bot.ollamaModel"),
        (json!({ "bot": { "defaultAgent": "nope" } }), "bot.defaultAgent"),
        (json!({ "watchdog": { "waitingNudgeAfterMs": 0 } }), "watchdog.waitingNudgeAfterMs"),
        (json!({ "watchdog": { "repeatEveryMs": -5 } }), "watchdog.repeatEveryMs"),
        (json!({ "watchdog": { "tickSecs": 0 } }), "watchdog.tickSecs"),
        (json!({ "watchdog": { "tickSecs": 4000 } }), "watchdog.tickSecs"),
        (json!({ "peer": { "listen": "my-pc:47801" } }), "peer.listen"),
        (json!({ "peer": { "listen": "192.168.1.5:80" } }), "peer.listen"),
        (json!({ "peer": { "listen": "127.0.0.1:47800" } }), "peer.listen"),
        (json!({ "peer": { "startRoots": [d.clone(), d.clone()] } }), "peer.startRoots[1]"),
        (json!({ "peer": { "startRoots": [d.clone(), file.clone(), missing.clone()] } }), "peer.startRoots[1]"),
        (json!({ "peer": { "startRoots": [d.clone(), "relative/dir", d.clone()] } }), "peer.startRoots[1]"),
        (json!({ "peer": { "startRoots": [d.clone(), format!("{d}/a"), missing.clone()] } }), "peer.startRoots[1]"),
        (json!({ "repos": { "paths": [missing.clone()] } }), "repos.paths[0]"),
        (json!({ "repos": { "paths": [""] } }), "repos.paths[0]"),
        (json!({ "relay": { "url": "ftp://relay" } }), "relay.url"),
        (json!({ "relay": { "url": "ws://relay.example.com" } }), "relay.url"),
        (json!({ "relay": { "url": "wss://relay.example.com/?x=1" } }), "relay.url"),
        (json!({ "relay": { "accessKey": "not base64url!" } }), "relay.accessKey"),
        (json!({ "relay": { "accessKey": "" } }), "relay.accessKey"),
        (json!({ "customAgents": [agent("ok", json!(["a"])), agent("Bad_Id", json!(["b"]))] }), "customAgents[1].id"),
        (json!({ "customAgents": [agent("ok", json!(["a"])), agent("ok", json!(["b"]))] }), "customAgents[1].id"),
        (json!({ "customAgents": [agent("ok", json!(["a"])), agent("codex", json!(["b"]))] }), "customAgents[1].id"),
        (json!({ "customAgents": [agent("ok", json!(["a"])), agent("two", json!([]))] }), "customAgents[1].command"),
        (json!({ "customAgents": [agent("ok", json!(["a"])), agent("two", json!(["  "]))] }), "customAgents[1].command"),
        (json!({ "customAgents": [agent("ok", json!(["a"])), json!({ "id": "two", "name": " ", "command": ["b"] })] }), "customAgents[1].name"),
        (json!({ "customAgents": [json!({ "id": "two", "name": "Co\u{200B}dex", "command": ["b"] })] }), "customAgents[0].name"),
    ];
    // peer.startRoots[1] in the 4th root case is the "a" subfolder that does not exist.
    for (p, field) in table {
        let e = err_of(&cur, p.clone());
        assert_eq!(e.field, field, "{p}");
        let text = anyhow::Error::new(e.clone()).to_string();
        assert!(text.starts_with(&format!("invalid:{field}: ")), "{text}");
        assert!(!e.reason.is_empty());
    }
    let many: Vec<serde_json::Value> = (0..17).map(|i| agent(&format!("a{i}"), json!(["x"]))).collect();
    assert_eq!(err_of(&cur, json!({ "customAgents": many })).field, "customAgents");
    let roots: Vec<String> = (0..65).map(|_| d.clone()).collect();
    assert_eq!(err_of(&cur, json!({ "peer": { "startRoots": roots } })).field, "peer.startRoots");
}

#[test]
fn folders_are_canonical_and_never_a_whole_drive() {
    let cur = HubConfig::default();
    let root = if cfg!(windows) { "C:\\" } else { "/" };
    let e = err_of(&cur, json!({ "peer": { "startRoots": [root] } }));
    assert_eq!(e.field, "peer.startRoots[0]");
    assert!(e.reason.contains("whole drive"), "{e}");
    assert_eq!(err_of(&cur, json!({ "repos": { "paths": [root] } })).field, "repos.paths[0]");
    if cfg!(windows) {
        assert_eq!(err_of(&cur, json!({ "repos": { "paths": ["\\\\?\\C:\\"] } })).field, "repos.paths[0]");
    }
    // `..` and links are resolved before the path is stored; the same folder twice is refused.
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a");
    std::fs::create_dir_all(a.join("b")).unwrap();
    let dotted = a.join("b").join("..").to_string_lossy().into_owned();
    let (next, _) = apply_patch(&cur, &patch(json!({ "peer": { "startRoots": [dotted] } })), &reserved()).unwrap();
    let canon = crate::repos::canonical_dir(&a).unwrap().to_string_lossy().into_owned();
    assert_eq!(next.peer.start_roots, vec![canon.clone()]);
    assert!(!next.peer.start_roots[0].contains(".."));
    let e = err_of(&cur, json!({ "repos": { "paths": [canon, a.join("b").join("..").to_string_lossy()] } }));
    assert_eq!(e.field, "repos.paths[1]");
}

#[test]
fn the_api_port_never_shares_the_listener_port() {
    let mut cur = HubConfig::default();
    cur.peer.listen = Some("127.0.0.1:47901".into());
    assert_eq!(err_of(&cur, json!({ "hub": { "port": 47901 } })).field, "hub.port");
    cur.peer.listen = Some("0.0.0.0:47901".into());
    assert_eq!(err_of(&cur, json!({ "hub": { "port": 47901 } })).field, "hub.port");
    // A LAN-only listener on that port does not collide with 127.0.0.1.
    cur.peer.listen = Some("192.168.1.20:47901".into());
    assert!(apply_patch(&cur, &patch(json!({ "hub": { "port": 47901 } })), &reserved()).is_ok());
}

#[test]
fn roster_intros_are_a_peer_setting_applied_at_once() {
    let cur = HubConfig::default();
    assert!(cur.peer.accept_roster_intros, "default: on");
    assert!(to_settings(&cur, &Overrides::default()).peer.accept_roster_intros);
    let (next, changed) = apply_patch(&cur, &patch(json!({ "peer": { "acceptRosterIntros": false } })), &reserved()).unwrap();
    assert_eq!(changed, [PEER_INTROS]);
    assert!(!RESTART_KEYS.contains(&PEER_INTROS) && !SECRET_KEYS.contains(&PEER_INTROS));
    assert!(!to_settings(&next, &Overrides::default()).peer.accept_roster_intros);
    let text = edit_config(Some("[peer]
accept_intents = true # keep me
"), &next, &changed).unwrap();
    assert!(text.contains("accept_intents = true # keep me") && text.contains("accept_roster_intros = false"), "{text}");
    assert!(!HubConfig::parse(&text).unwrap().peer.accept_roster_intros);
    // Not a bool: refused before anything is applied.
    assert!(serde_json::from_value::<HubSettingsPatch>(json!({ "peer": { "acceptRosterIntros": "no" } })).is_err());
}

#[test]
fn refusals_never_quote_secret_values() {
    let cur = HubConfig::default();
    for p in [
        json!({ "bot": { "anthropicApiKey": format!("{SECRET} x") } }),
        json!({ "bot": { "anthropicApiKey": "x".repeat(600) + SECRET } }),
        json!({ "relay": { "accessKey": format!("{RELAY_SECRET}!") } }),
    ] {
        let e = err_of(&cur, p);
        assert!(!e.reason.contains(SECRET) && !e.reason.contains(RELAY_SECRET), "{e}");
    }
}

#[test]
fn valid_patches_normalize_and_report_changed_keys() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().to_string_lossy().into_owned();
    let cur = HubConfig::default();
    let p = patch(json!({
        "hub": { "port": 48000, "dailyReportTime": "", "eventRetentionDays": 0 },
        "bot": { "model": " claude-haiku-5-5 ", "anthropicApiKey": SECRET, "ollamaUrl": "HTTP://127.0.0.1:11434/", "ollamaModel": "" },
        "watchdog": { "tickSecs": 60 },
        "peer": { "listen": "10.0.0.5", "acceptIntents": false, "startRoots": [format!(" {d} ")] },
        "repos": { "paths": [d.clone()] },
        "relay": { "url": "wss://relay.example.com/", "accessKey": RELAY_SECRET },
        "customAgents": [{ "id": "my-agent", "name": " Mine ", "command": [" my-agent ", "--acp"] }],
    }));
    let (next, changed) = apply_patch(&cur, &p, &reserved()).unwrap();
    assert_eq!(next.port, 48000);
    assert_eq!(next.daily_report_minutes(), None);
    assert_eq!(next.daily_report_time, "off");
    assert_eq!(next.brain.model, "claude-haiku-5-5");
    assert_eq!(next.brain.anthropic_api_key.as_deref(), Some(SECRET));
    assert_eq!(next.brain.ollama_url.as_deref(), Some("http://127.0.0.1:11434"));
    assert_eq!(next.peer.listen.as_deref(), Some("10.0.0.5:47801"));
    assert_eq!(next.peer.start_roots, vec![d.clone()]);
    assert_eq!(next.relay.url.as_deref(), Some("wss://relay.example.com"));
    assert_eq!(next.custom_agents[0].name, "Mine");
    assert_eq!(next.custom_agents[0].command, vec!["my-agent".to_string(), "--acp".into()]);
    assert_eq!(
        changed,
        vec![
            HUB_PORT,
            HUB_RETENTION,
            HUB_DAILY_REPORT,
            BOT_KEY,
            BOT_OLLAMA_URL,
            WD_TICK,
            PEER_LISTEN,
            PEER_ACCEPT,
            PEER_ROOTS,
            REPOS_PATHS,
            RELAY_URL,
            RELAY_KEY,
            CUSTOM_AGENTS
        ],
        "bot.model was already the default"
    );
    // The same patch again changes nothing.
    let (_, again) = apply_patch(&next, &p, &reserved()).unwrap();
    assert!(again.is_empty(), "{again:?}");
    // Clearing: null and "" both clear optional values; null clears secrets.
    let clear = patch(json!({ "bot": { "anthropicApiKey": null, "ollamaUrl": "" }, "peer": { "listen": null }, "relay": { "url": null, "accessKey": null } }));
    let (cleared, changed) = apply_patch(&next, &clear, &reserved()).unwrap();
    assert_eq!((cleared.brain.anthropic_api_key, cleared.brain.ollama_url, cleared.peer.listen), (None, None, None));
    assert_eq!((cleared.relay.url, cleared.relay.access_key), (None, None));
    assert_eq!(changed, vec![BOT_KEY, BOT_OLLAMA_URL, PEER_LISTEN, RELAY_URL, RELAY_KEY]);
    // Loopback relays may use ws:// (local tests, wrangler dev).
    let (n, _) = apply_patch(&cur, &patch(json!({ "relay": { "url": "ws://127.0.0.1:8787" } })), &reserved()).unwrap();
    assert_eq!(n.relay.url.as_deref(), Some("ws://127.0.0.1:8787"));
}

#[test]
fn settings_view_never_carries_secrets() {
    let mut cfg = HubConfig::default();
    cfg.brain.anthropic_api_key = Some(SECRET.into());
    cfg.relay.access_key = Some(RELAY_SECRET.into());
    cfg.relay.url = Some("wss://relay.example.com".into());
    let s = to_settings(&cfg, &Overrides::default());
    assert!(s.bot.anthropic_api_key_set && s.relay.access_key_set);
    let text = serde_json::to_string(&s).unwrap();
    assert!(!text.contains(SECRET) && !text.contains(RELAY_SECRET), "{text}");
    let printed = masked_lines(&s, &[]).join("\n");
    assert!(!printed.contains(SECRET) && !printed.contains(RELAY_SECRET), "{printed}");
    assert!(printed.contains("set (hidden)"));
    // Debug output of the config redacts them too.
    let dbg = format!("{cfg:?}");
    assert!(!dbg.contains(SECRET) && !dbg.contains(RELAY_SECRET), "{dbg}");

    // The env key counts as set and is reported as the source while config.toml has none.
    let env = Overrides { keys: BTreeMap::new(), env_anthropic_key: true, ..Default::default() };
    let s = to_settings(&HubConfig::default(), &env);
    assert!(s.bot.anthropic_api_key_set);
    assert_eq!(s.overridden_by.get(BOT_KEY).map(String::as_str), Some("ANTHROPIC_API_KEY"));
    let s = to_settings(&HubConfig::default(), &Overrides::default());
    assert!(!s.bot.anthropic_api_key_set && s.overridden_by.is_empty());
    assert_eq!(s.hub.daily_report_time, "18:00");
    assert_eq!(s.peer.listen, None);
}

const HAND_WRITTEN: &str = r#"# My hub settings
port = 47800 # the default port

daily_report_time = "18:00"

[brain]
# keep the cheap model
model = "claude-haiku-5-5"

[peer]
# my LAN address
listen = "192.168.1.20"
# comment above accept_intents
accept_intents = true

[future_section]
answer = 42 # unknown to this hub version

[[custom_agents]]
# my agent, keep this comment
id = "mine"
name = "Mine"
command = ["mine", "--acp"]
env = { TOKEN_FILE = "/x" }

[[custom_agents]]
id = "gone"
name = "Gone"
command = ["gone"]
"#;

#[test]
fn edits_keep_comments_unknown_keys_and_env() {
    let cur = HubConfig::parse(HAND_WRITTEN).unwrap();
    let p = patch(json!({
        "hub": { "port": 48100 },
        "bot": { "anthropicApiKey": SECRET },
        "peer": { "listen": null },
        "relay": { "url": "wss://relay.example.com", "accessKey": RELAY_SECRET },
        "customAgents": [
            { "id": "new-one", "name": "New", "command": ["new", "--acp"] },
            { "id": "mine", "name": "Mine renamed", "command": ["mine", "--acp", "--v2"] },
        ],
    }));
    let (next, changed) = apply_patch(&cur, &p, &reserved()).unwrap();
    let out = edit_config(Some(HAND_WRITTEN), &next, &changed).unwrap();
    for kept in [
        "# My hub settings",
        "# the default port",
        "# keep the cheap model",
        "# my LAN address",
        "# comment above accept_intents",
        "[future_section]",
        "answer = 42 # unknown to this hub version",
        "# my agent, keep this comment",
        "TOKEN_FILE",
    ] {
        assert!(out.contains(kept), "{kept:?} missing in:\n{out}");
    }
    assert!(out.contains("port = 48100 # the default port"), "{out}");
    assert!(!out.contains("listen ="), "{out}");
    assert!(!out.contains("\"gone\""), "{out}");
    let back = HubConfig::parse(&out).unwrap();
    assert_eq!(back.port, 48100);
    assert_eq!(back.peer.listen, None);
    assert_eq!(back.brain.anthropic_api_key.as_deref(), Some(SECRET));
    assert_eq!(back.relay.access_key.as_deref(), Some(RELAY_SECRET));
    assert_eq!(back.custom_agents.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["new-one", "mine"]);
    let mine = &back.custom_agents[1];
    assert_eq!((mine.name.as_str(), mine.command.len()), ("Mine renamed", 3));
    assert_eq!(mine.env.as_ref().and_then(|e| e.get("TOKEN_FILE")).map(String::as_str), Some("/x"), "env kept");
    // Untouched keys are byte-identical.
    assert!(out.contains("daily_report_time = \"18:00\""));
    assert!(out.contains("model = \"claude-haiku-5-5\""));

    // Turning the listener back on writes it into [peer] again.
    let (next2, changed2) = apply_patch(&back, &patch(json!({ "peer": { "listen": "10.1.2.3:47901" } })), &reserved()).unwrap();
    let out2 = edit_config(Some(&out), &next2, &changed2).unwrap();
    assert_eq!(HubConfig::parse(&out2).unwrap().peer.listen.as_deref(), Some("10.1.2.3:47901"));
    assert!(out2.contains("# comment above accept_intents"), "{out2}");
}

#[test]
fn a_missing_file_starts_from_the_commented_template() {
    let cur = HubConfig::default();
    let (next, changed) = apply_patch(&cur, &patch(json!({ "peer": { "listen": "127.0.0.1:47901" }, "watchdog": { "tickSecs": 15 } })), &reserved()).unwrap();
    let out = edit_config(None, &next, &changed).unwrap();
    assert!(out.contains("# Pawl hub configuration"), "{out}");
    assert!(out.contains("# [[custom_agents]]"), "template comments kept");
    let back = HubConfig::parse(&out).unwrap();
    assert_eq!(back.peer.listen.as_deref(), Some("127.0.0.1:47901"));
    assert_eq!(back.watchdog.tick_secs, 15);
    assert!(edit_config(Some("port = = 1"), &next, &changed).is_err(), "broken TOML is never overwritten");
}

#[test]
fn atomic_write_replaces_the_file_and_leaves_no_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "port = 1\n").unwrap();
    write_atomic(&path, "port = 48000\n").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "port = 48000\n");
    write_atomic(&path, "port = 48001\n").unwrap();
    let names: Vec<String> = std::fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["config.toml"]);
}

#[test]
fn patches_build_from_typed_sections_too() {
    // The Rust patch types (what a Rust client would send) take the same path.
    let p = HubSettingsPatch {
        hub: Some(HubSettingsHubPatch { offline: Some(true), ..Default::default() }),
        bot: Some(HubSettingsBotPatch { ollama_model: Some(Some("llama3.2".into())), ..Default::default() }),
        watchdog: Some(HubSettingsWatchdogPatch { repeat_every_ms: Some(60_000), ..Default::default() }),
        peer: Some(HubSettingsPeerPatch { accept_intents: Some(false), ..Default::default() }),
        repos: Some(HubSettingsReposPatch { paths: Some(vec![]) }),
        relay: Some(HubSettingsRelayPatch { url: Some(None), access_key: None }),
        custom_agents: None,
        a2a: None,
        browser: None,
    };
    let (next, changed) = apply_patch(&HubConfig::default(), &p, &reserved()).unwrap();
    assert!(next.offline && !next.peer.accept_intents);
    assert_eq!(changed, vec![HUB_OFFLINE, BOT_OLLAMA_MODEL, WD_REPEAT, PEER_ACCEPT]);
}

#[test]
fn default_agent_is_validated_persisted_and_cleared() {
    let cur = HubConfig::default();
    // A known id: set (trimmed), shown, written under [brain] next to its comments, read back.
    let (next, changed) = apply_patch(&cur, &patch(json!({ "bot": { "defaultAgent": " codex " } })), &reserved()).unwrap();
    assert_eq!(changed, vec![BOT_DEFAULT_AGENT]);
    assert_eq!(next.brain.default_agent.as_deref(), Some("codex"));
    assert_eq!(to_settings(&next, &Overrides::default()).bot.default_agent.as_deref(), Some("codex"));
    let text = edit_config(Some("[brain]\nmodel = \"claude-haiku-5-5\" # keep me\n"), &next, &changed).unwrap();
    assert!(text.contains("default_agent = \"codex\"") && text.contains("# keep me"), "{text}");
    assert_eq!(HubConfig::parse(&text).unwrap().brain.default_agent.as_deref(), Some("codex"));
    assert!(masked_lines(&to_settings(&next, &Overrides::default()), &[]).iter().any(|l| l.starts_with("default agent") && l.ends_with("codex")));
    // null or "" = automatic: the key goes away.
    for v in [json!(null), json!("")] {
        let (back, changed) = apply_patch(&next, &patch(json!({ "bot": { "defaultAgent": v } })), &reserved()).unwrap();
        assert_eq!((back.brain.default_agent.as_deref(), changed.clone()), (None, vec![BOT_DEFAULT_AGENT]));
        let t = edit_config(Some(&text), &back, &changed).unwrap();
        assert!(!t.contains("default_agent") && t.contains("# keep me"), "{t}");
    }
    // Unknown ids are refused; one with control characters is not echoed.
    let e = err_of(&cur, json!({ "bot": { "defaultAgent": "nope" } }));
    assert_eq!(e.to_string(), "invalid:bot.defaultAgent: \"nope\" is not a known agent (installed, registry or custom)");
    let e = err_of(&cur, json!({ "bot": { "defaultAgent": "x\u{1b}[2J" } }));
    assert!(e.field == BOT_DEFAULT_AGENT && !e.to_string().contains('\u{1b}'), "{e}");
    // A custom agent may be the default, also one the same patch adds; removing it = automatic again.
    let custom = json!([{ "id": "my-agent", "name": "My Agent", "command": ["my-agent", "--acp"] }]);
    let (with, changed) = apply_patch(&cur, &patch(json!({ "customAgents": custom, "bot": { "defaultAgent": "my-agent" } })), &reserved()).unwrap();
    assert_eq!((with.brain.default_agent.as_deref(), changed), (Some("my-agent"), vec![BOT_DEFAULT_AGENT, CUSTOM_AGENTS]));
    let (without, changed) = apply_patch(&with, &patch(json!({ "customAgents": [] })), &reserved()).unwrap();
    assert_eq!((without.brain.default_agent.as_deref(), changed), (None, vec![BOT_DEFAULT_AGENT, CUSTOM_AGENTS]));
    // Other patches leave it alone.
    let (same, changed) = apply_patch(&with, &patch(json!({ "hub": { "offline": true } })), &reserved()).unwrap();
    assert_eq!((same.brain.default_agent.as_deref(), changed), (Some("my-agent"), vec![HUB_OFFLINE]));
}

#[test]
fn bot_providers_are_validated_persisted_and_never_shown() {
    const OPENAI: &str = "sk-proj-openai-test-0123456789";
    const GEMINI: &str = "AIza-gemini-test-0123456789";
    let cur = HubConfig::default();
    // Defaults: automatic provider, understanding on, the documented default models, Claude Code + Sonnet for acp.
    let s = to_settings(&cur, &Overrides::default());
    assert_eq!((s.bot.provider, s.bot.understand), (BotProvider::Auto, true));
    assert_eq!((s.bot.openai_model.as_str(), s.bot.gemini_model.as_str()), ("gpt-6-luna", "gemini-3.5-flash-lite"));
    assert_eq!((s.bot.acp_agent.as_str(), s.bot.acp_model.as_str()), ("claude-code", "sonnet"));
    assert!(!s.bot.openai_api_key_set && !s.bot.gemini_api_key_set);
    let p = json!({ "bot": {
        "provider": "openai", "openaiApiKey": OPENAI, "openaiModel": " gpt-6-luna ", "geminiApiKey": GEMINI,
        "geminiModel": "", "acpAgent": "claude-code", "acpModel": "opus", "understand": false,
    } });
    let (next, changed) = apply_patch(&cur, &patch(p), &reserved()).unwrap();
    assert_eq!(changed, vec![BOT_PROVIDER, BOT_OPENAI_KEY, BOT_OPENAI_MODEL, BOT_GEMINI_KEY, BOT_ACP_AGENT, BOT_ACP_MODEL, BOT_UNDERSTAND]);
    let s = to_settings(&next, &Overrides::default());
    assert_eq!((s.bot.provider, s.bot.understand, s.bot.openai_api_key_set, s.bot.gemini_api_key_set), (BotProvider::Openai, false, true, true));
    assert_eq!((s.bot.acp_agent.as_str(), s.bot.acp_model.as_str(), s.bot.gemini_model.as_str()), ("claude-code", "opus", "gemini-3.5-flash-lite"));
    // Written under [brain]; read back the same; keys never in the view, the CLI lines or Debug.
    let text = edit_config(None, &next, &changed).unwrap();
    for line in ["provider = \"openai\"", "openai_model = \"gpt-6-luna\"", "acp_agent = \"claude-code\"", "acp_model = \"opus\"", "understand = false"] {
        assert!(text.lines().any(|l| l.trim() == line), "{line}: {text}");
    }
    assert!(!text.lines().any(|l| l.trim_start().starts_with("gemini_model")), "blank = default, not written: {text}");
    let back = HubConfig::parse(&text).unwrap();
    assert_eq!(to_settings(&back, &Overrides::default()), s);
    let shown = serde_json::to_string(&s).unwrap() + &masked_lines(&s, &[]).join("\n") + &format!("{next:?}");
    assert!(!shown.contains(OPENAI) && !shown.contains(GEMINI), "{shown}");
    assert!(masked_lines(&s, &[]).iter().any(|l| l.contains("uses your own plan")));
    // Clearing a key: null. An empty key, a bad model, an unknown acp agent or provider: refused.
    let (cleared, changed) = apply_patch(&next, &patch(json!({ "bot": { "openaiApiKey": null } })), &reserved()).unwrap();
    assert_eq!((cleared.brain.openai_api_key.as_deref(), changed), (None, vec![BOT_OPENAI_KEY]));
    for (p, field) in [
        (json!({ "bot": { "openaiApiKey": "" } }), BOT_OPENAI_KEY),
        (json!({ "bot": { "geminiApiKey": "has space" } }), BOT_GEMINI_KEY),
        (json!({ "bot": { "openaiModel": "gpt 6" } }), BOT_OPENAI_MODEL),
        (json!({ "bot": { "geminiModel": "x".repeat(200) } }), BOT_GEMINI_MODEL),
        (json!({ "bot": { "acpModel": "a\u{7}b" } }), BOT_ACP_MODEL),
        (json!({ "bot": { "acpAgent": "no-such-agent" } }), BOT_ACP_AGENT),
        (json!({ "bot": { "acpAgent": "codex" } }), BOT_ACP_AGENT),
    ] {
        assert_eq!(err_of(&cur, p.clone()).field, field, "{p}");
    }
    // Only Claude Code's tools can be locked down, so only it can be the pet's model (for now).
    let e = err_of(&cur, json!({ "bot": { "acpAgent": "codex" } }));
    assert!(e.reason.contains("only claude-code"), "{e}");
    assert!(serde_json::from_value::<HubSettingsPatch>(json!({ "bot": { "provider": "skynet" } })).is_err(), "providers are a closed list");
    // The env keys count as set and are named as the source while config.toml has none.
    let env = Overrides { env_openai_key: true, env_gemini_key: true, ..Default::default() };
    let s = to_settings(&HubConfig::default(), &env);
    assert!(s.bot.openai_api_key_set && s.bot.gemini_api_key_set);
    assert_eq!(s.overridden_by.get(BOT_OPENAI_KEY).map(String::as_str), Some("OPENAI_API_KEY"));
    assert_eq!(s.overridden_by.get(BOT_GEMINI_KEY).map(String::as_str), Some("GEMINI_API_KEY"));
    // The brain gets the keys and the choices.
    let b = next.brain.to_brain_config();
    assert_eq!((b.provider, b.openai_api_key.as_deref(), b.understand), (BotProvider::Openai, Some(OPENAI), false));
}

#[test]
fn the_reply_style_defaults_to_plain_and_is_persisted() {
    let cur = HubConfig::default();
    assert_eq!(to_settings(&cur, &Overrides::default()).bot.style, BotStyle::Plain);
    assert_eq!(cur.brain.to_brain_config().style, BotStyle::Plain);
    let (next, changed) = apply_patch(&cur, &patch(json!({ "bot": { "style": "playful" } })), &reserved()).unwrap();
    assert_eq!((next.brain.style, changed.clone()), (BotStyle::Playful, vec![BOT_STYLE]));
    let text = edit_config(None, &next, &changed).unwrap();
    assert!(text.lines().any(|l| l.trim() == "style = \"playful\""), "{text}");
    assert_eq!(HubConfig::parse(&text).unwrap().brain.style, BotStyle::Playful);
    assert!(masked_lines(&to_settings(&next, &Overrides::default()), &[]).iter().any(|l| l.starts_with("style") && l.ends_with("playful")));
    // A closed list: anything else fails the patch.
    assert!(serde_json::from_value::<HubSettingsPatch>(json!({ "bot": { "style": "pirate" } })).is_err());
    assert!(HubConfig::parse("[brain]\nstyle = \"pirate\"\n").is_err());
    let (back, _) = apply_patch(&next, &patch(json!({ "bot": { "style": "plain" } })), &reserved()).unwrap();
    assert_eq!(back.brain.style, BotStyle::Plain);
}

#[test]
fn the_pet_name_follows_the_pet_name_rules() {
    let cur = HubConfig::default();
    let (next, changed) = apply_patch(&cur, &patch(json!({ "bot": { "petName": "  Mochi " } })), &reserved()).unwrap();
    assert_eq!((next.brain.pet_name.as_deref(), changed.clone()), (Some("Mochi"), vec![BOT_PET_NAME]));
    assert_eq!(to_settings(&next, &Overrides::default()).bot.pet_name, "Mochi");
    let text = edit_config(None, &next, &changed).unwrap();
    assert!(text.lines().any(|l| l.trim() == "pet_name = \"Mochi\""), "{text}");
    for bad in [json!(""), json!("  "), json!("a\u{0007}b"), json!("Mo\u{200B}chi"), json!("x".repeat(41))] {
        assert_eq!(err_of(&cur, json!({ "bot": { "petName": bad } })).field, BOT_PET_NAME, "{bad}");
    }
    assert_eq!(to_settings(&cur, &Overrides::default()).bot.pet_name, "", "not picked yet");
}

#[test]
fn browser_use_is_off_by_default_and_stores_sites_as_origins() {
    let cur = HubConfig::default();
    let view = to_settings(&cur, &Overrides::default());
    assert!(!view.browser.enabled && view.browser.allow_sites.is_empty(), "off by default");
    let (next, changed) = apply_patch(
        &cur,
        &patch(json!({ "browser": { "enabled": true, "allowSites": [" GitHub.com ", "staging.example.com.", "https://github.com/", "github.com:8443", "http://localhost:3000", "bücher.de"] } })),
        &reserved(),
    )
    .unwrap();
    assert_eq!(changed, [BROWSER_ENABLED, BROWSER_SITES]);
    assert!(next.browser.enabled);
    assert_eq!(
        next.browser.allow_sites,
        ["https://github.com", "https://staging.example.com", "https://github.com:8443", "http://localhost:3000", "https://xn--bcher-kva.de"],
        "a bare host is https on 443; a port or http is written out; duplicates dropped; punycode"
    );
    let text = edit_config(Some("[browser]\nenabled = false # keep me\n"), &next, &changed).unwrap();
    assert!(text.contains("# keep me") && text.contains("\"https://github.com:8443\""), "{text}");
    let back = HubConfig::parse(&text).unwrap();
    assert_eq!((back.browser.enabled, back.browser.allow_sites.clone()), (true, next.browser.allow_sites.clone()));
    assert!(masked_lines(&to_settings(&next, &Overrides::default()), &[]).iter().any(|l| l.starts_with("allow sites") && l.contains("https://github.com")));
    for (i, bad) in ["ftp://github.com", "github.com/acme", "github.com:0", "*.github.com", "user@github.com", "git hub.com", "-x.com", "", "a..b", "https://x.com/?q"]
        .iter()
        .enumerate()
    {
        let e = err_of(&cur, json!({ "browser": { "allowSites": ["ok.com", bad] } }));
        assert_eq!(e.field, format!("{BROWSER_SITES}[1]"), "{i}: {bad}");
    }
    let many: Vec<String> = (0..=MAX_ALLOW_SITES).map(|i| format!("s{i}.example.com")).collect();
    assert_eq!(err_of(&cur, json!({ "browser": { "allowSites": many } })).field, BROWSER_SITES);
    assert!(HubConfig::parse("[browser]\nenabled = \"yes\"\n").is_err());
}
