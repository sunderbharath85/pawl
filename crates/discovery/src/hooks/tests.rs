use super::*;
use krypto_protocol::{EventType, HookSource, HookState, NotificationLevel, SessionSource, SessionStatus};
use serde_json::json;

fn n(source: HookSource, raw: Value) -> NormalizedHook {
    normalize(source, raw).expect("normalized")
}

// ---------- normalize: Claude Code (payloads from code.claude.com/docs/en/hooks) ----------

#[test]
fn claude_session_lifecycle() {
    let start = n(
        HookSource::Claude,
        json!({ "session_id": "abc123", "transcript_path": "/t.jsonl", "cwd": "/repo", "hook_event_name": "SessionStart",
                "source": "startup", "model": "claude-opus-5", "session_title": "Fix login" }),
    );
    assert_eq!(start.external_session_id, "abc123");
    assert_eq!(start.agent_id, "claude-code");
    assert_eq!(start.source, SessionSource::CliHook);
    assert_eq!(start.cwd.as_deref(), Some("/repo"));
    assert_eq!(start.title.as_deref(), Some("Fix login"));
    assert_eq!(start.status, Some(SessionStatus::Idle));
    assert_eq!(start.event, EventType::SessionStart);
    assert_eq!(start.payload.source, HookSource::Claude);
    assert_eq!(start.payload.hook_event, "SessionStart");
    assert!(!start.ended && start.notification.is_none());

    let prompt = n(
        HookSource::Claude,
        json!({ "session_id": "abc123", "cwd": "/repo", "permission_mode": "default", "hook_event_name": "UserPromptSubmit",
                "prompt": "Write a function to calculate the factorial of a number" }),
    );
    assert_eq!(prompt.status, Some(SessionStatus::Thinking));
    assert_eq!(prompt.event, EventType::Hook);
    assert_eq!(prompt.title.as_deref(), Some("Write a function to calculate the factorial of a number"));
    assert_eq!(prompt.payload.message.as_deref(), Some("Write a function to calculate the factorial of a number"));

    let pre = n(
        HookSource::Claude,
        json!({ "session_id": "abc123", "cwd": "/repo", "hook_event_name": "PreToolUse", "tool_name": "Bash",
                "tool_input": { "command": "npm test", "description": "Run test suite", "timeout": 120000 }, "tool_use_id": "toolu_01" }),
    );
    assert_eq!(pre.status, Some(SessionStatus::Working));
    assert_eq!(pre.payload.message.as_deref(), Some("Bash: npm test"));

    let post = n(
        HookSource::Claude,
        json!({ "session_id": "abc123", "cwd": "/repo", "hook_event_name": "PostToolUse", "tool_name": "Write",
                "tool_input": { "file_path": "/repo/a.txt", "content": "x".repeat(10_000) },
                "tool_response": { "filePath": "/repo/a.txt", "type": "create" }, "duration_ms": 12 }),
    );
    assert_eq!(post.status, Some(SessionStatus::Working));
    assert_eq!(post.payload.message.as_deref(), Some("Write: /repo/a.txt"));
    let content = post.payload.raw["tool_input"]["content"].as_str().unwrap();
    assert!(content.len() < 4100 && content.contains("truncated"), "large raw strings are bounded");

    let fail = n(
        HookSource::Claude,
        json!({ "session_id": "abc123", "hook_event_name": "PostToolUseFailure", "tool_name": "Bash",
                "tool_input": { "command": "npm test" }, "error": "Exit code 1\nError: Cannot find module 'express'" }),
    );
    assert_eq!(fail.status, Some(SessionStatus::Working));
    assert_eq!(fail.payload.message.as_deref(), Some("Bash: npm test failed: Exit code 1"));

    let stop = n(
        HookSource::Claude,
        json!({ "session_id": "abc123", "cwd": "/repo", "hook_event_name": "Stop", "stop_hook_active": false,
                "last_assistant_message": "I've completed the refactoring.", "background_tasks": [], "session_crons": [] }),
    );
    assert_eq!(stop.status, Some(SessionStatus::Done));
    assert!(!stop.ended);
    let (lvl, title, body) = stop.notification.unwrap();
    assert_eq!(lvl, NotificationLevel::Success);
    assert_eq!(title, "Claude Code finished");
    assert_eq!(body, "I've completed the refactoring.");

    let sub = n(
        HookSource::Claude,
        json!({ "session_id": "abc123", "hook_event_name": "SubagentStop", "stop_hook_active": false, "agent_id": "def456",
                "agent_type": "Explore", "last_assistant_message": "Analysis complete." }),
    );
    assert_eq!(sub.status, Some(SessionStatus::Working));
    assert!(sub.notification.is_none(), "subagent stop is not a turn end");

    let end = n(
        HookSource::Claude,
        json!({ "session_id": "abc123", "cwd": "/repo", "hook_event_name": "SessionEnd", "reason": "prompt_input_exit" }),
    );
    assert_eq!(end.status, Some(SessionStatus::Done));
    assert!(end.ended);
    assert_eq!(end.event, EventType::SessionEnd);
    assert_eq!(end.payload.message.as_deref(), Some("prompt_input_exit"));
}

#[test]
fn claude_notifications_and_permissions() {
    let perm = n(
        HookSource::Claude,
        json!({ "session_id": "s", "cwd": "/r", "hook_event_name": "Notification", "message": "Claude needs your permission to use Bash",
                "notification_type": "permission_prompt" }),
    );
    assert_eq!(perm.status, Some(SessionStatus::WaitingPermission));
    let (lvl, title, body) = perm.notification.unwrap();
    assert_eq!(lvl, NotificationLevel::Alert);
    assert_eq!(title, "Claude Code needs permission");
    assert_eq!(body, "Claude needs your permission to use Bash");

    let idle = n(
        HookSource::Claude,
        json!({ "session_id": "s", "hook_event_name": "Notification", "message": "Claude is waiting for your input", "notification_type": "idle_prompt" }),
    );
    assert_eq!(idle.status, None);
    assert_eq!(idle.notification.unwrap().0, NotificationLevel::Info);

    let auth = n(HookSource::Claude, json!({ "session_id": "s", "hook_event_name": "Notification", "message": "ok", "notification_type": "auth_success" }));
    assert!(auth.notification.is_none());

    let req = n(
        HookSource::Claude,
        json!({ "session_id": "s", "hook_event_name": "PermissionRequest", "tool_name": "Bash",
                "tool_input": { "command": "rm -rf node_modules", "description": "Remove node_modules" },
                "permission_suggestions": [] }),
    );
    assert_eq!(req.status, Some(SessionStatus::WaitingPermission));
    let (lvl, _, body) = req.notification.unwrap();
    assert_eq!(lvl, NotificationLevel::Alert);
    assert_eq!(body, "Bash: rm -rf node_modules");

    let fail = n(
        HookSource::Claude,
        json!({ "session_id": "s", "hook_event_name": "StopFailure", "error": "rate_limit", "error_details": "429 Too Many Requests" }),
    );
    assert_eq!(fail.status, Some(SessionStatus::Error));
    assert_eq!(fail.notification.unwrap().2, "rate_limit: 429 Too Many Requests");

    let other = n(HookSource::Claude, json!({ "session_id": "s", "hook_event_name": "PreCompact", "trigger": "auto" }));
    assert_eq!(other.status, None);
    assert_eq!(other.event, EventType::Hook);
}

#[test]
fn claude_rejects_and_synthesizes() {
    assert!(normalize(HookSource::Claude, json!("text")).is_none());
    assert!(normalize(HookSource::Claude, json!({ "session_id": "s" })).is_none(), "no event name");
    let h = n(HookSource::Claude, json!({ "hook_event_name": "Stop", "cwd": "/work/repo" }));
    assert_eq!(h.external_session_id, "claude-code:/work/repo");
    let h = n(HookSource::Claude, json!({ "hook_event_name": "Stop" }));
    assert_eq!(h.external_session_id, "claude-code:unknown");
}

#[test]
fn vscode_origin_detection() {
    let base = || json!({ "session_id": "s", "cwd": "/r", "hook_event_name": "Stop" });
    let src = |source: HookSource, mut raw: Value, meta: Value| {
        raw[META_KEY] = meta;
        n(source, raw).source
    };
    // Vendor VS Code extensions -> vscode_hook.
    assert_eq!(src(HookSource::Claude, base(), json!({ "claude_entrypoint": "claude-vscode", "vscode_ext_host": true })), SessionSource::VscodeHook);
    // Entrypoint wins even if VS Code itself was started from an integrated terminal.
    assert_eq!(src(HookSource::Claude, base(), json!({ "claude_entrypoint": "claude-vscode", "term_program": "vscode" })), SessionSource::VscodeHook);
    assert_eq!(
        src(HookSource::Codex, json!({ "type": "agent-turn-complete", "thread-id": "t" }), json!({ "codex_originator": "codex_vscode" })),
        SessionSource::VscodeHook
    );
    // Copilot agent hooks run by the extension host.
    let copilot = || json!({ "hook_event_name": "Stop", "session_id": "c", "cwd": "/r" });
    assert_eq!(src(HookSource::Copilot, copilot(), json!({ "vscode_ext_host": true, "vscode_process_type": "extensionHost" })), SessionSource::VscodeHook);
    assert_eq!(src(HookSource::Copilot, copilot(), json!({ "vscode_ext_host": true })), SessionSource::VscodeHook);
    assert_eq!(src(HookSource::Copilot, copilot(), json!({ "vscode_process_type": "extensionHost", "term_program": "vscode" })), SessionSource::VscodeHook);

    // PLAN.md: CLIs in VS Code's integrated terminal are cli_hook.
    assert_eq!(src(HookSource::Claude, base(), json!({ "claude_entrypoint": "cli", "term_program": "vscode", "vscode_ext_host": false })), SessionSource::CliHook);
    assert_eq!(src(HookSource::Copilot, copilot(), json!({ "term_program": "vscode", "vscode_ext_host": true })), SessionSource::CliHook);
    assert_eq!(
        src(HookSource::Codex, json!({ "type": "agent-turn-complete", "thread-id": "t" }), json!({ "codex_originator": "codex_cli_rs", "term_program": "vscode" })),
        SessionSource::CliHook
    );
    // Plain terminals and payloads without our metadata.
    assert_eq!(src(HookSource::Claude, base(), json!({ "claude_entrypoint": "cli", "term_program": "WezTerm", "vscode_ext_host": false })), SessionSource::CliHook);
    assert_eq!(n(HookSource::Claude, base()).source, SessionSource::CliHook);
}

#[test]
fn env_meta_reads_only_known_vars() {
    // Integrated terminal: terminal-only VS Code markers do not count as the extension host.
    let term = std::collections::HashMap::from([
        ("TERM_PROGRAM", "vscode"),
        ("CLAUDE_CODE_ENTRYPOINT", "cli"),
        ("VSCODE_IPC_HOOK_CLI", "/tmp/x.sock"),
        ("VSCODE_GIT_IPC_HANDLE", "/tmp/git.sock"),
        ("VSCODE_INJECTION", "1"),
        ("ANTHROPIC_API_KEY", "sk-secret"),
    ]);
    let m = env_meta_from(|k| term.get(k).map(|v| v.to_string()));
    assert_eq!(m["term_program"], "vscode");
    assert_eq!(m["claude_entrypoint"], "cli");
    assert_eq!(m["vscode_ext_host"], false);
    assert!(!m.to_string().contains("sk-secret"));
    assert_eq!(normalize(HookSource::Claude, json!({ "hook_event_name": "Stop", "_krypto": m })).unwrap().source, SessionSource::CliHook);

    // Extension host (e.g. Copilot agent hooks in VS Code).
    let ext = std::collections::HashMap::from([
        ("VSCODE_PID", "4242"),
        ("VSCODE_IPC_HOOK", "/tmp/main.sock"),
        ("VSCODE_CRASH_REPORTER_PROCESS_TYPE", "extensionHost"),
    ]);
    let m = env_meta_from(|k| ext.get(k).map(|v| v.to_string()));
    assert_eq!(m["vscode_ext_host"], true);
    assert_eq!(m["vscode_process_type"], "extensionHost");
    assert_eq!(normalize(HookSource::Copilot, json!({ "hook_event_name": "Stop", "_krypto": m })).unwrap().source, SessionSource::VscodeHook);
    let mut raw = json!({ "a": 1 });
    enrich(&mut raw);
    assert!(raw.get(META_KEY).is_some());
}

// ---------- normalize: Codex ----------

#[test]
fn codex_notify_turn_complete() {
    let h = n(
        HookSource::Codex,
        json!({ "type": "agent-turn-complete", "thread-id": "b5f6c1c2-1111", "turn-id": "12345", "cwd": "/Users/alice/projects/example",
                "input-messages": ["Rename `foo` to `bar` and update the callsites."],
                "last-assistant-message": "Rename complete and verified `cargo build` succeeds." }),
    );
    assert_eq!(h.external_session_id, "b5f6c1c2-1111");
    assert_eq!(h.agent_id, "codex");
    assert_eq!(h.status, Some(SessionStatus::Done));
    assert_eq!(h.event, EventType::Hook);
    assert_eq!(h.payload.hook_event, "agent-turn-complete");
    assert_eq!(h.title.as_deref(), Some("Rename `foo` to `bar` and update the callsites."));
    let (lvl, title, body) = h.notification.unwrap();
    assert_eq!((lvl, title.as_str()), (NotificationLevel::Success, "Codex finished"));
    assert_eq!(body, "Rename complete and verified `cargo build` succeeds.");
    assert!(normalize(HookSource::Codex, json!({ "foo": 1 })).is_none());
}

#[test]
fn codex_lifecycle_hooks() {
    let start = n(
        HookSource::Codex,
        json!({ "session_id": "thr_123", "transcript_path": null, "cwd": "/w", "hook_event_name": "SessionStart", "model": "gpt-5.5", "source": "startup" }),
    );
    assert_eq!(start.status, Some(SessionStatus::Idle));
    assert_eq!(start.event, EventType::SessionStart);
    let perm = n(
        HookSource::Codex,
        json!({ "session_id": "thr_123", "cwd": "/w", "hook_event_name": "PermissionRequest", "turn_id": "t1", "tool_name": "Bash",
                "tool_input": { "command": "git push", "description": "needs network" } }),
    );
    assert_eq!(perm.status, Some(SessionStatus::WaitingPermission));
    assert_eq!(perm.notification.as_ref().unwrap().1, "Codex needs permission");
    let patch = n(
        HookSource::Codex,
        json!({ "session_id": "thr_123", "hook_event_name": "PreToolUse", "tool_name": "apply_patch", "tool_input": { "command": "*** Begin Patch" } }),
    );
    assert_eq!(patch.payload.message.as_deref(), Some("apply_patch: *** Begin Patch"));
    let int = n(HookSource::Codex, json!({ "session_id": "thr_123", "hook_event_name": "Interrupt", "turn_id": "t1" }));
    assert_eq!(int.status, Some(SessionStatus::Idle));
    let end = n(HookSource::Codex, json!({ "session_id": "thr_123", "cwd": "/w", "hook_event_name": "SessionEnd", "reason": "other" }));
    assert!(end.ended);
    assert_eq!(end.event, EventType::SessionEnd);
    assert_eq!(stdout_reply(HookSource::Codex, &json!({ "hook_event_name": "Stop" })), Some("{}"));
    assert_eq!(stdout_reply(HookSource::Codex, &json!({ "type": "agent-turn-complete" })), None);
    assert_eq!(stdout_reply(HookSource::Claude, &json!({ "hook_event_name": "Stop" })), None);
}

// ---------- normalize: Copilot (docs.github.com hooks reference) ----------

#[test]
fn copilot_vscode_compatible_payloads() {
    let start = n(
        HookSource::Copilot,
        json!({ "hook_event_name": "SessionStart", "session_id": "cp-1", "timestamp": "2026-11-23T10:00:00Z", "cwd": "/r", "source": "new",
                "initial_prompt": "Add tests" }),
    );
    assert_eq!(start.agent_id, "copilot");
    assert_eq!(start.event, EventType::SessionStart);
    assert_eq!(start.status, Some(SessionStatus::Thinking), "initial prompt starts work right away");
    assert_eq!(start.title.as_deref(), Some("Add tests"));

    let pre = n(
        HookSource::Copilot,
        json!({ "hook_event_name": "PreToolUse", "session_id": "cp-1", "cwd": "/r", "tool_name": "bash", "tool_input": { "command": "ls" } }),
    );
    assert_eq!(pre.status, Some(SessionStatus::Working));
    assert_eq!(pre.payload.message.as_deref(), Some("bash: ls"));

    let stop = n(
        HookSource::Copilot,
        json!({ "hook_event_name": "Stop", "session_id": "cp-1", "cwd": "/r", "transcript_path": "/t", "stop_reason": "end_turn", "stop_hook_active": false }),
    );
    assert_eq!(stop.status, Some(SessionStatus::Done));
    assert_eq!(stop.notification.unwrap().1, "Copilot finished");

    let err = n(
        HookSource::Copilot,
        json!({ "hook_event_name": "ErrorOccurred", "session_id": "cp-1", "cwd": "/r",
                "error": { "message": "model unavailable", "name": "Error" }, "error_context": "model_call", "recoverable": false }),
    );
    assert_eq!(err.status, Some(SessionStatus::Error));
    assert_eq!(err.payload.message.as_deref(), Some("model unavailable"));

    let note = n(
        HookSource::Copilot,
        json!({ "sessionId": "cp-1", "timestamp": 1, "cwd": "/r", "hook_event_name": "Notification", "message": "Permission needed for bash",
                "title": "Permission needed", "notification_type": "permission_prompt" }),
    );
    assert_eq!(note.status, Some(SessionStatus::WaitingPermission));
    assert_eq!(note.external_session_id, "cp-1");
}

#[test]
fn copilot_camel_case_payloads_without_event_name() {
    // camelCase payloads carry no event name: our env marker wins, else the shape decides.
    let mut tagged = json!({ "sessionId": "cp-2", "timestamp": 1704614600000i64, "cwd": "/r", "toolName": "edit",
                             "toolArgs": "{\"path\":\"src/app.ts\"}" });
    tagged[META_KEY] = json!({ "hook_event": "PreToolUse", "vscode_ext_host": true });
    let pre = n(HookSource::Copilot, tagged);
    assert_eq!(pre.payload.hook_event, "PreToolUse");
    assert_eq!(pre.payload.message.as_deref(), Some("edit: src/app.ts"));
    assert_eq!(pre.source, SessionSource::VscodeHook);

    let post = n(
        HookSource::Copilot,
        json!({ "sessionId": "cp-2", "cwd": "/r", "toolName": "bash", "toolArgs": { "command": "npm test" },
                "toolResult": { "resultType": "success", "textResultForLlm": "ok" } }),
    );
    assert_eq!(post.payload.hook_event, "postToolUse");
    assert_eq!(post.status, Some(SessionStatus::Working));

    let prompt = n(HookSource::Copilot, json!({ "timestamp": 1, "cwd": "/r", "prompt": "Fix the bug" }));
    assert_eq!(prompt.payload.hook_event, "userPromptSubmitted");
    assert_eq!(prompt.status, Some(SessionStatus::Thinking));
    assert_eq!(prompt.external_session_id, "copilot:/r", "older payloads without sessionId");

    let end = n(HookSource::Copilot, json!({ "sessionId": "cp-2", "cwd": "/r", "reason": "user_exit" }));
    assert!(end.ended);
    let stop = n(HookSource::Copilot, json!({ "sessionId": "cp-2", "cwd": "/r", "stopReason": "end_turn", "stop_hook_active": false }));
    assert_eq!(stop.status, Some(SessionStatus::Done));
    let recoverable = n(
        HookSource::Copilot,
        json!({ "sessionId": "cp-2", "error": { "message": "retrying", "name": "E" }, "errorContext": "tool_execution", "recoverable": true }),
    );
    assert_eq!(recoverable.status, None);
    assert!(recoverable.notification.is_none());
    assert!(normalize(HookSource::Copilot, json!({ "timestamp": 1 })).is_none());
}

// ---------- read_input / misc ----------

#[test]
fn read_input_prefers_trailing_json_arg() {
    let args = vec!["--verbose".to_string(), r#"{"type":"agent-turn-complete","thread-id":"t"}"#.to_string()];
    let v = read_input(&args).unwrap();
    assert_eq!(v["thread-id"], "t");
}

#[test]
fn target_and_source_parsing() {
    assert_eq!(HookTarget::from_cli("copilot-vscode", "/r"), Some(HookTarget::CopilotVscode { repo: "/r".into() }));
    assert_eq!(HookTarget::from_cli("claude", "/r"), Some(HookTarget::Claude));
    assert_eq!(HookTarget::from_cli("nope", "/r"), None);
    assert_eq!(parse_hook_source("Codex"), Some(HookSource::Codex));
    assert_eq!(hook_source_str(HookSource::Copilot), "copilot");
    assert_eq!(install::test_support::utc_stamp(0), "19700101-000000");
    assert_eq!(install::test_support::utc_stamp(1_795_483_343), "20261124-012223");
}

// ---------- install (temp dirs only; never the real user config) ----------

fn hub_exe(dir: &Path) -> PathBuf {
    dir.join("bin").join(if cfg!(windows) { "krypto.exe" } else { "krypto" })
}

fn backups(dir: &Path, prefix: &str) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy();
            n.starts_with(prefix) && n.ends_with(".bak")
        })
        .collect()
}

#[test]
fn install_claude_merges_without_clobbering() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let settings_dir = home.join(".claude");
    std::fs::create_dir_all(&settings_dir).unwrap();
    let settings = settings_dir.join("settings.json");
    let original = json!({
        "model": "opus",
        "permissions": { "allow": ["Bash(npm test)"] },
        "hooks": {
            "Stop": [ { "hooks": [ { "type": "command", "command": "say done" } ] } ],
            "PreToolUse": [ { "matcher": "Bash", "hooks": [ { "type": "command", "command": "/usr/local/bin/guard.sh" } ] } ]
        }
    });
    std::fs::write(&settings, serde_json::to_string_pretty(&original).unwrap()).unwrap();
    let exe = hub_exe(tmp.path());
    let env = InstallEnv::default();

    // Dry run: diff only, file untouched, no backup.
    let out = install_with_home(HookTarget::Claude, &exe, false, &home, &env).unwrap();
    assert!(out.contains("dry run"), "{out}");
    assert!(out.contains("+") && out.contains("hook claude"), "{out}");
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(on_disk, original);
    assert!(backups(&settings_dir, "settings.json.").is_empty());

    // Apply: backup + merge.
    let out = install_with_home(HookTarget::Claude, &exe, true, &home, &env).unwrap();
    assert!(out.contains("backup"), "{out}");
    let baks = backups(&settings_dir, "settings.json.");
    assert_eq!(baks.len(), 1);
    let bak: Value = serde_json::from_str(&std::fs::read_to_string(&baks[0]).unwrap()).unwrap();
    assert_eq!(bak, original);

    let v: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(v["model"], "opus");
    assert_eq!(v["permissions"], original["permissions"]);
    let stop = v["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop[0]["hooks"][0]["command"], "say done", "user hook kept first");
    let ours = &stop[1]["hooks"][0];
    let expected_cmd = format!("{} hook claude", exe.to_string_lossy().replace('\\', "/"));
    assert_eq!(ours["command"], expected_cmd.as_str());
    assert_eq!(ours["type"], "command");
    let pre = v["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre[0]["matcher"], "Bash");
    assert_eq!(pre[0]["hooks"][0]["command"], "/usr/local/bin/guard.sh");
    assert_eq!(pre.len(), 2);
    for event in ["SessionStart", "UserPromptSubmit", "PreToolUse", "PermissionRequest", "PostToolUse", "Notification", "Stop", "SubagentStop", "SessionEnd"] {
        assert!(v["hooks"][event].is_array(), "{event} installed");
    }
    assert_eq!(v["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"], 3);

    // Re-run: idempotent, no new backup.
    let out = install_with_home(HookTarget::Claude, &exe, true, &home, &env).unwrap();
    assert!(out.contains("already up to date"), "{out}");
    assert_eq!(backups(&settings_dir, "settings.json.").len(), 1);

    // Hub moved: our entry is replaced in place, user hooks untouched, still one of ours.
    let moved = tmp.path().join("other").join(if cfg!(windows) { "krypto.exe" } else { "krypto" });
    install_with_home(HookTarget::Claude, &moved, true, &home, &env).unwrap();
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    let stop = v["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 2);
    assert!(stop[1]["hooks"][0]["command"].as_str().unwrap().contains("/other/"));
}

#[test]
fn install_claude_replaces_entry_written_by_other_hub_copy() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let settings_dir = home.join(".claude");
    std::fs::create_dir_all(&settings_dir).unwrap();
    let settings = settings_dir.join("settings.json");
    let old = json!({
        "hooks": {
            "Stop": [
                { "hooks": [ { "type": "command", "command": "say done" } ] },
                { "hooks": [ { "type": "command", "command": "C:/tools/krypto.exe hook claude" } ] }
            ]
        }
    });
    std::fs::write(&settings, serde_json::to_string_pretty(&old).unwrap()).unwrap();
    // Running from a copy with another file name: the entry is still recognised by the
    // `krypto` binary name and replaced, not duplicated.
    let exe = tmp.path().join("dev").join(if cfg!(windows) { "hub-dev.exe" } else { "hub-dev" });
    install_with_home(HookTarget::Claude, &exe, true, &home, &InstallEnv::default()).unwrap();
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    let stop = v["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 2, "old entry replaced, not duplicated: {stop:?}");
    assert_eq!(stop[0]["hooks"][0]["command"], "say done");
    let cmd = stop[1]["hooks"][0]["command"].as_str().unwrap();
    assert!(cmd.contains("/dev/hub-dev") && cmd.ends_with("hook claude") && !cmd.contains("C:/tools/"), "{cmd}");
}

#[test]
fn install_claude_creates_file_and_respects_config_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_dir = tmp.path().join("custom-claude");
    let env = InstallEnv { claude_config_dir: Some(cfg_dir.clone()), ..Default::default() };
    let out = install_with_home(HookTarget::Claude, &hub_exe(tmp.path()), true, &tmp.path().join("home"), &env).unwrap();
    assert!(out.contains("created"), "{out}");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(cfg_dir.join("settings.json")).unwrap()).unwrap();
    assert!(v["hooks"]["SessionStart"][0]["hooks"][0]["command"].as_str().unwrap().ends_with("hook claude"));
    assert!(!tmp.path().join("home").exists(), "default location untouched");
}

#[test]
fn install_claude_exec_form_for_paths_with_spaces() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = tmp.path().join("Program Files").join("Krypto").join("krypto.exe");
    install_with_home(HookTarget::Claude, &exe, true, tmp.path(), &InstallEnv::default()).unwrap();
    let v: Value = serde_json::from_str(&std::fs::read_to_string(tmp.path().join(".claude/settings.json")).unwrap()).unwrap();
    let h = &v["hooks"]["Stop"][0]["hooks"][0];
    assert_eq!(h["command"], exe.to_string_lossy().as_ref());
    assert_eq!(h["args"], json!(["hook", "claude"]));
}

#[test]
fn install_refuses_invalid_json() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join(".claude");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(p.join("settings.json"), "{ \"model\": ").unwrap();
    let err = install_with_home(HookTarget::Claude, &hub_exe(tmp.path()), true, tmp.path(), &InstallEnv::default()).unwrap_err();
    assert!(err.to_string().contains("not valid JSON"), "{err}");
    assert_eq!(std::fs::read_to_string(p.join("settings.json")).unwrap(), "{ \"model\": ");
}

#[test]
fn install_codex_toml_preserves_config() {
    let tmp = tempfile::tempdir().unwrap();
    let codex = tmp.path().join(".codex");
    std::fs::create_dir_all(&codex).unwrap();
    let original = "# my codex config\nmodel = \"gpt-5.5\" # favourite\n\n[projects.\"/repo\"]\ntrust_level = \"trusted\"\n\n[[hooks.PreToolUse]]\nmatcher = \"^Bash$\"\n\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"python3 ~/.codex/policy.py\"\n";
    std::fs::write(codex.join("config.toml"), original).unwrap();
    let exe = hub_exe(tmp.path());

    let dry = install_with_home(HookTarget::Codex, &exe, false, tmp.path(), &InstallEnv::default()).unwrap();
    assert!(dry.contains("notify"), "{dry}");
    assert_eq!(std::fs::read_to_string(codex.join("config.toml")).unwrap(), original);

    install_with_home(HookTarget::Codex, &exe, true, tmp.path(), &InstallEnv::default()).unwrap();
    let text = std::fs::read_to_string(codex.join("config.toml")).unwrap();
    assert!(text.contains("# my codex config") && text.contains("# favourite"), "comments kept:\n{text}");
    assert!(text.contains("python3 ~/.codex/policy.py"), "user hook kept:\n{text}");
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    let notify: Vec<String> = doc["notify"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
    assert_eq!(notify, [exe.to_string_lossy().to_string(), "hook".into(), "codex".into()]);
    assert_eq!(doc["projects"]["/repo"]["trust_level"].as_str(), Some("trusted"));
    let pre = doc["hooks"]["PreToolUse"].as_array_of_tables().unwrap();
    assert_eq!(pre.len(), 2, "user group + ours");
    assert!(doc["hooks"]["SessionStart"].is_array());
    assert!(doc["hooks"].get("Stop").is_none(), "notify covers turn end");
    assert_eq!(backups(&codex, "config.toml.").len(), 1);

    // Idempotent.
    let again = install_with_home(HookTarget::Codex, &exe, true, tmp.path(), &InstallEnv::default()).unwrap();
    assert!(again.contains("already up to date"), "{again}");
    assert_eq!(std::fs::read_to_string(codex.join("config.toml")).unwrap(), text);
}

#[test]
fn install_codex_keeps_foreign_notify_and_uses_hooks_json() {
    let tmp = tempfile::tempdir().unwrap();
    let codex_home = tmp.path().join("codex-home");
    std::fs::create_dir_all(&codex_home).unwrap();
    std::fs::write(codex_home.join("config.toml"), "notify = [\"python3\", \"/opt/notify.py\"]\n").unwrap();
    std::fs::write(codex_home.join("hooks.json"), r#"{"description":"mine","hooks":{"Stop":[{"hooks":[{"type":"command","command":"echo bye"}]}]}}"#).unwrap();
    let env = InstallEnv { codex_home: Some(codex_home.clone()), ..Default::default() };
    let out = install_with_home(HookTarget::Codex, &hub_exe(tmp.path()), true, tmp.path(), &env).unwrap();
    assert!(out.contains("kept your existing `notify`"), "{out}");
    let cfg = std::fs::read_to_string(codex_home.join("config.toml")).unwrap();
    assert_eq!(cfg, "notify = [\"python3\", \"/opt/notify.py\"]\n", "config.toml untouched");
    let hooks: Value = serde_json::from_str(&std::fs::read_to_string(codex_home.join("hooks.json")).unwrap()).unwrap();
    assert_eq!(hooks["description"], "mine");
    let stop = hooks["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 2, "user Stop hook kept, ours added because notify is taken");
    assert_eq!(stop[0]["hooks"][0]["command"], "echo bye");
    assert!(hooks["hooks"]["SessionEnd"][0]["hooks"][0]["command"].as_str().unwrap().ends_with("hook codex"));
    assert_eq!(hooks["hooks"]["SessionEnd"][0]["hooks"][0]["timeout"], 3);
    assert!(!tmp.path().join(".codex").exists());
}

#[test]
fn install_copilot_cli_and_vscode() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = hub_exe(tmp.path());
    let out = install_with_home(HookTarget::CopilotCli, &exe, true, tmp.path(), &InstallEnv::default()).unwrap();
    assert!(out.contains("created"), "{out}");
    let path = tmp.path().join(".copilot").join("hooks").join("krypto.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v["version"], 1);
    let pre = &v["hooks"]["PreToolUse"][0];
    assert_eq!(pre["type"], "command");
    assert!(pre["bash"].as_str().unwrap().starts_with('\''));
    assert!(pre["powershell"].as_str().unwrap().starts_with("& '"));
    assert!(pre["powershell"].as_str().unwrap().ends_with("hook copilot"));
    assert_eq!(pre["env"][HOOK_EVENT_ENV], "PreToolUse");
    assert!(v["hooks"]["notification"].is_array());

    // A user-added entry in our file survives a re-install.
    let mut edited = v.clone();
    edited["hooks"]["PreToolUse"].as_array_mut().unwrap().push(json!({ "type": "command", "bash": "./audit.sh" }));
    std::fs::write(&path, serde_json::to_string_pretty(&edited).unwrap()).unwrap();
    install_with_home(HookTarget::CopilotCli, &exe, true, tmp.path(), &InstallEnv::default()).unwrap();
    let v2: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(v2["hooks"]["PreToolUse"].as_array().unwrap().len(), 2);

    let repo = tmp.path().join("repo");
    assert!(install_with_home(HookTarget::CopilotVscode { repo: repo.clone() }, &exe, true, tmp.path(), &InstallEnv::default()).is_err());
    std::fs::create_dir_all(&repo).unwrap();
    let out = install_with_home(HookTarget::CopilotVscode { repo: repo.clone() }, &exe, false, tmp.path(), &InstallEnv::default()).unwrap();
    assert!(out.contains(".gitignore"), "{out}");
    assert!(!repo.join(".github").exists(), "dry run writes nothing");
    install_with_home(HookTarget::CopilotVscode { repo: repo.clone() }, &exe, true, tmp.path(), &InstallEnv::default()).unwrap();
    assert!(repo.join(".github").join("hooks").join("krypto.json").is_file());
}

#[test]
fn powershell_and_posix_quoting() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = tmp.path().join("it's here").join("krypto.exe");
    install_with_home(HookTarget::CopilotCli, &exe, true, tmp.path(), &InstallEnv::default()).unwrap();
    let v: Value = serde_json::from_str(&std::fs::read_to_string(tmp.path().join(".copilot/hooks/krypto.json")).unwrap()).unwrap();
    let h = &v["hooks"]["Stop"][0];
    assert!(h["powershell"].as_str().unwrap().contains("it''s here"));
    assert!(h["bash"].as_str().unwrap().contains(r"it'\''s here"));
}

// ---------- status + remove (temp dirs only) ----------

fn state(target: &HookTarget, exe: &Path, home: &Path, env: &InstallEnv) -> HookState {
    status_with_home(target, exe, home, env).state
}

#[test]
fn claude_status_follows_install_and_remove() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let env = InstallEnv::default();
    let exe = hub_exe(tmp.path());
    let settings = home.join(".claude").join("settings.json");
    let check = status_with_home(&HookTarget::Claude, &exe, &home, &env);
    assert_eq!((check.state, check.config_path.clone()), (HookState::Missing, settings.clone()));
    assert!(!settings.exists(), "status never writes");

    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let user = json!({ "model": "opus", "hooks": { "Stop": [ { "hooks": [ { "type": "command", "command": "say done" } ] } ] } });
    std::fs::write(&settings, serde_json::to_string_pretty(&user).unwrap()).unwrap();
    assert_eq!(state(&HookTarget::Claude, &exe, &home, &env), HookState::Missing);

    // Preview: the exact change, nothing written.
    let preview = change_with_home(&HookTarget::Claude, &exe, HookChange::Install, false, &home, &env).unwrap();
    assert!(preview.changed && preview.text.contains("hook claude"), "{}", preview.text);
    assert_eq!(serde_json::from_str::<Value>(&std::fs::read_to_string(&settings).unwrap()).unwrap(), user);

    change_with_home(&HookTarget::Claude, &exe, HookChange::Install, true, &home, &env).unwrap();
    assert_eq!(state(&HookTarget::Claude, &exe, &home, &env), HookState::Installed);
    let again = change_with_home(&HookTarget::Claude, &exe, HookChange::Install, false, &home, &env).unwrap();
    assert!(!again.changed, "{}", again.text);

    // Another hub executable: ours are there but stale.
    let moved = tmp.path().join("other").join(if cfg!(windows) { "krypto.exe" } else { "krypto" });
    let check = status_with_home(&HookTarget::Claude, &moved, &home, &env);
    assert_eq!(check.state, HookState::Outdated);
    assert!(check.detail.is_some());

    // Remove preview writes nothing; remove keeps every other hook and setting.
    let before = std::fs::read_to_string(&settings).unwrap();
    let preview = change_with_home(&HookTarget::Claude, &exe, HookChange::Remove, false, &home, &env).unwrap();
    assert!(preview.changed && preview.text.contains("Removing"), "{}", preview.text);
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), before);
    let out = change_with_home(&HookTarget::Claude, &exe, HookChange::Remove, true, &home, &env).unwrap();
    assert!(out.text.contains("backup"), "{}", out.text);
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(v, user, "back to the user's own settings");
    assert_eq!(state(&HookTarget::Claude, &exe, &home, &env), HookState::Missing);
    let none = change_with_home(&HookTarget::Claude, &exe, HookChange::Remove, true, &home, &env).unwrap();
    assert!(!none.changed && none.text.contains("no Krypto hooks"), "{}", none.text);

    // Removing from a machine without the file creates nothing.
    let empty = tmp.path().join("empty-home");
    let out = change_with_home(&HookTarget::Claude, &exe, HookChange::Remove, true, &empty, &env).unwrap();
    assert!(!out.changed);
    assert!(!empty.exists());
}

#[test]
fn status_reports_unreadable_configs_as_unsupported() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("settings.json"), "{ \"model\": ").unwrap();
    let check = status_with_home(&HookTarget::Claude, &hub_exe(tmp.path()), tmp.path(), &InstallEnv::default());
    assert_eq!(check.state, HookState::Unsupported);
    assert!(check.detail.unwrap().contains("not valid JSON"));
    let missing_repo = HookTarget::CopilotVscode { repo: tmp.path().join("no-such-repo") };
    let check = status_with_home(&missing_repo, &hub_exe(tmp.path()), tmp.path(), &InstallEnv::default());
    assert_eq!(check.state, HookState::Unsupported);
    assert!(check.detail.unwrap().contains("does not exist"));
}

#[test]
fn codex_remove_takes_out_notify_and_inline_hooks_only() {
    let tmp = tempfile::tempdir().unwrap();
    let codex = tmp.path().join(".codex");
    std::fs::create_dir_all(&codex).unwrap();
    let original = "# my codex config\nmodel = \"gpt-5.5\" # favourite\n\n[[hooks.PreToolUse]]\nmatcher = \"^Bash$\"\n\n[[hooks.PreToolUse.hooks]]\ntype = \"command\"\ncommand = \"python3 ~/.codex/policy.py\"\n";
    std::fs::write(codex.join("config.toml"), original).unwrap();
    let exe = hub_exe(tmp.path());
    let env = InstallEnv::default();
    change_with_home(&HookTarget::Codex, &exe, HookChange::Install, true, tmp.path(), &env).unwrap();
    assert_eq!(state(&HookTarget::Codex, &exe, tmp.path(), &env), HookState::Installed);
    change_with_home(&HookTarget::Codex, &exe, HookChange::Remove, true, tmp.path(), &env).unwrap();
    let text = std::fs::read_to_string(codex.join("config.toml")).unwrap();
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    assert!(doc.get("notify").is_none(), "{text}");
    assert!(text.contains("# my codex config") && text.contains("# favourite"), "{text}");
    assert!(text.contains("python3 ~/.codex/policy.py"), "{text}");
    assert!(!text.contains("hook codex"), "{text}");
    assert!(doc["hooks"].get("SessionStart").is_none(), "{text}");
    assert_eq!(state(&HookTarget::Codex, &exe, tmp.path(), &env), HookState::Missing);

    // A foreign notify is never removed.
    std::fs::write(codex.join("config.toml"), "notify = [\"python3\", \"/opt/notify.py\"]\n").unwrap();
    let out = change_with_home(&HookTarget::Codex, &exe, HookChange::Remove, true, tmp.path(), &env).unwrap();
    assert!(!out.changed, "{}", out.text);
    assert_eq!(std::fs::read_to_string(codex.join("config.toml")).unwrap(), "notify = [\"python3\", \"/opt/notify.py\"]\n");
}

#[test]
fn copilot_remove_deletes_our_file_unless_the_user_added_to_it() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = hub_exe(tmp.path());
    let env = InstallEnv::default();
    let path = tmp.path().join(".copilot").join("hooks").join("krypto.json");
    change_with_home(&HookTarget::CopilotCli, &exe, HookChange::Install, true, tmp.path(), &env).unwrap();
    assert_eq!(state(&HookTarget::CopilotCli, &exe, tmp.path(), &env), HookState::Installed);
    let out = change_with_home(&HookTarget::CopilotCli, &exe, HookChange::Remove, true, tmp.path(), &env).unwrap();
    assert!(out.text.contains("deleted"), "{}", out.text);
    assert!(!path.exists());
    assert_eq!(backups(path.parent().unwrap(), "krypto.json.").len(), 1);
    assert_eq!(state(&HookTarget::CopilotCli, &exe, tmp.path(), &env), HookState::Missing);

    change_with_home(&HookTarget::CopilotCli, &exe, HookChange::Install, true, tmp.path(), &env).unwrap();
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    v["hooks"]["PreToolUse"].as_array_mut().unwrap().push(json!({ "type": "command", "bash": "./audit.sh" }));
    std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    change_with_home(&HookTarget::CopilotCli, &exe, HookChange::Remove, true, tmp.path(), &env).unwrap();
    let left: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(left["hooks"], json!({ "PreToolUse": [ { "type": "command", "bash": "./audit.sh" } ] }));
}

#[test]
fn config_paths_honour_agent_env_overrides() {
    let home = Path::new("/home/u");
    let env = InstallEnv { claude_config_dir: Some("/cfg/claude".into()), codex_home: None, copilot_home: Some("/cfg/copilot".into()) };
    assert_eq!(config_path_with_home(&HookTarget::Claude, home, &env), Path::new("/cfg/claude").join("settings.json"));
    assert_eq!(config_path_with_home(&HookTarget::Codex, home, &env), home.join(".codex").join("config.toml"));
    assert_eq!(config_path_with_home(&HookTarget::CopilotCli, home, &env), Path::new("/cfg/copilot").join("hooks").join("krypto.json"));
    let repo = PathBuf::from("/r");
    assert_eq!(config_path_with_home(&HookTarget::CopilotVscode { repo: repo.clone() }, home, &env), repo.join(".github").join("hooks").join("krypto.json"));
}

// ---------- forward ----------

#[tokio::test]
async fn forward_posts_with_bearer_and_meta() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = sock.read(&mut chunk).await.unwrap();
            buf.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buf).to_string();
            if let Some(i) = text.find("\r\n\r\n") {
                let len: usize = text[..i]
                    .lines()
                    .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap()))
                    .unwrap_or(0);
                if buf.len() >= i + 4 + len || n == 0 {
                    break;
                }
            }
            if n == 0 {
                break;
            }
        }
        sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\nconnection: close\r\n\r\n{\"ok\":true}").await.unwrap();
        String::from_utf8_lossy(&buf).to_string()
    });
    let hub = format!("http://{addr}/");
    forward(HookSource::Claude, json!({ "hook_event_name": "Stop", "session_id": "s" }), &hub, " tok123 ").await.unwrap();
    let req = server.await.unwrap();
    assert!(req.starts_with("POST /hooks/claude HTTP/1.1"), "{req}");
    assert!(req.to_ascii_lowercase().contains("authorization: bearer tok123"), "{req}");
    let body: Value = serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap();
    assert_eq!(body["session_id"], "s");
    assert!(body[META_KEY].is_object());
}

#[tokio::test]
async fn forward_fails_fast_when_hub_down() {
    // Bind then drop to get a port that is (almost certainly) closed.
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let started = std::time::Instant::now();
    let r = forward(HookSource::Codex, json!({ "type": "agent-turn-complete" }), &format!("http://127.0.0.1:{port}"), "t").await;
    assert!(r.is_err());
    assert!(started.elapsed() < Duration::from_millis(2600), "{:?}", started.elapsed());
    // run_hook_cli never panics or errors, even with the hub down.
    run_hook_cli(HookSource::Codex, &[r#"{"type":"agent-turn-complete","thread-id":"x"}"#.to_string()], &format!("http://127.0.0.1:{port}"), "t").await;
}

/// A symlink to `target` at `link`, or None where this OS / account cannot make one (Windows
/// without Developer Mode).
fn try_symlink_dir(target: &Path, link: &Path) -> Option<()> {
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(target, link).or_else(|_| {
        // A directory junction needs no privilege and is a link all the same.
        let ok = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .is_ok_and(|o| o.status.success());
        if ok { Ok(()) } else { Err(std::io::Error::other("mklink /J failed")) }
    });
    made.ok()
}

#[test]
fn repo_hooks_never_go_through_a_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = hub_exe(tmp.path());
    let env = InstallEnv::default();
    let repo = tmp.path().join("repo");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(outside.join("hooks")).unwrap();
    std::fs::write(outside.join("hooks").join("krypto.json"), r#"{"secret":"do not show"}"#).unwrap();
    if try_symlink_dir(&outside, &repo.join(".github")).is_none() {
        eprintln!("skipped: this account cannot create symlinks");
        return;
    }
    let target = HookTarget::CopilotVscode { repo: repo.clone() };
    for (change, apply) in [(HookChange::Install, false), (HookChange::Install, true), (HookChange::Remove, true)] {
        let e = change_with_home(&target, &exe, change, apply, tmp.path(), &env).unwrap_err();
        assert!(format!("{e:#}").contains("symbolic link"), "{e:#}");
    }
    let check = status_with_home(&target, &exe, tmp.path(), &env);
    assert_eq!(check.state, HookState::Unsupported);
    assert!(!check.detail.unwrap_or_default().contains("do not show"), "the linked file is never read");
    assert_eq!(std::fs::read_to_string(outside.join("hooks").join("krypto.json")).unwrap(), r#"{"secret":"do not show"}"#);
    assert_eq!(std::fs::read_dir(outside.join("hooks")).unwrap().count(), 1, "no backup or temp file outside the repo");
}

#[test]
fn rewrites_leave_no_temp_files_and_keep_permissions() {
    let tmp = tempfile::tempdir().unwrap();
    let exe = hub_exe(tmp.path());
    let env = InstallEnv::default();
    let dir = tmp.path().join(".claude");
    std::fs::create_dir_all(&dir).unwrap();
    let settings = dir.join("settings.json");
    std::fs::write(&settings, r#"{"model":"opus"}"#).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    change_with_home(&HookTarget::Claude, &exe, HookChange::Install, true, tmp.path(), &env).unwrap();
    let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().all(|n| !n.contains("krypto-tmp")), "{names:?}");
    assert_eq!(names.iter().filter(|n| n.ends_with(".bak")).count(), 1, "{names:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&settings), 0o600, "the rewritten file keeps the original's mode");
        let bak = dir.join(names.iter().find(|n| n.ends_with(".bak")).unwrap());
        assert_eq!(mode(&bak), 0o600, "the backup too");
    }
}
