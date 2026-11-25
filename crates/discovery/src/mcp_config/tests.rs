use super::*;
use tempfile::TempDir;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

fn entry() -> McpEntry {
    McpEntry { exe: PathBuf::from(if cfg!(windows) { r"C:\Program Files\Pawl\pawl.exe" } else { "/opt/pawl/bin/pawl" }), url: "http://127.0.0.1:8941".into(), token: TOKEN.into() }
}

fn env() -> McpEnv {
    McpEnv::default()
}

fn path_of(t: McpTarget, home: &TempDir) -> PathBuf {
    config_path(t, home.path(), &env())
}

fn write(t: McpTarget, home: &TempDir, text: &str) -> PathBuf {
    let p = path_of(t, home);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, text).unwrap();
    p
}

fn read(p: &Path) -> String {
    String::from_utf8(std::fs::read(p).unwrap()).unwrap()
}

fn connect(t: McpTarget, home: &TempDir, e: &McpEntry) -> McpPlan {
    let plan = plan(t, home.path(), &env(), McpChange::Connect(e)).unwrap();
    apply(&plan).unwrap();
    plan
}

/// Remove as the hub does for a servers object the user had (it stays).
fn remove(t: McpTarget, home: &TempDir) -> McpPlan {
    remove_with(t, home, false)
}

/// `drop_empty_servers`: what the hub recorded from Connect's `added_servers`.
fn remove_with(t: McpTarget, home: &TempDir, drop_empty_servers: bool) -> McpPlan {
    let plan = plan(t, home.path(), &env(), McpChange::Remove { drop_empty_servers }).unwrap();
    apply(&plan).unwrap();
    plan
}

fn state(t: McpTarget, home: &TempDir, want: Option<&McpEntry>) -> Result<EntryState, String> {
    check(t, home.path(), &env(), want).state
}

/// `new` is `old` with bytes only inserted: every byte of `old` is still there, in order.
fn only_inserted(old: &str, new: &str) -> bool {
    let mut it = new.bytes();
    old.bytes().all(|b| it.any(|n| n == b))
}

fn entry_json(t: McpTarget, p: &Path) -> Value {
    let text = read(p);
    find_entry(t, p, &text).unwrap().entry.expect("an entry")
}

// ---------------------------------------------------------------- JSONC reader

#[test]
fn the_reader_keeps_spans_and_knows_jsonc_from_json() {
    let text = "{\n  // servers\n  \"a\": [1, 2,],\n  \"b\": {\"x\": \"y\\\"z\"}, /* end */\n}\n";
    let root = jsonc::parse(text, true).unwrap();
    let b = root.get("b").unwrap();
    assert_eq!(&text[b.value.start..b.value.end], "{\"x\": \"y\\\"z\"}");
    assert_eq!(root.to_value(text), json!({ "a": [1, 2], "b": { "x": "y\"z" } }));
    for bad in [text, "{\"a\": 1,}", "{\"a\": [1,]}", "{\"a\": 1 \"b\": 2}", "{\"a\": tru}", "[1]", "{\"a\": 1} x", "{\"a\": \"open"] {
        assert!(jsonc::parse(bad, false).is_err(), "{bad}");
    }
    assert!(jsonc::parse("{\"a\": 1} /* open", true).is_err());
    assert!(jsonc::parse("\u{feff}{\"a\": 1}", false).is_ok());
}

// ---------------------------------------------------------------- JSON targets

#[test]
fn a_fresh_file_gets_just_the_entry() {
    for t in [McpTarget::Claude, McpTarget::CopilotCli, McpTarget::Vscode] {
        let home = TempDir::new().unwrap();
        assert_eq!(state(t, &home, Some(&entry())), Ok(EntryState::Absent));
        let plan = connect(t, &home, &entry());
        assert!(plan.summary.contains("creates the file") && plan.summary.contains(TOKEN_PLACEHOLDER) && !plan.summary.contains(TOKEN), "{}", plan.summary);
        let p = path_of(t, &home);
        let doc: Value = serde_json::from_str(&read(&p)).unwrap();
        assert_eq!(doc, json!({ t.servers_key(): { "pawl": entry().to_json(t) } }));
        assert_eq!(state(t, &home, Some(&entry())), Ok(EntryState::Current), "{t:?}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
    assert_eq!(entry().to_json(McpTarget::CopilotCli)["type"], "local");
    assert_eq!(entry().to_json(McpTarget::CopilotCli)["tools"], json!(["*"]));
    assert_eq!(entry().to_json(McpTarget::Vscode)["type"], "stdio");
}

#[test]
fn claude_json_keeps_every_other_byte_and_round_trips() {
    let home = TempDir::new().unwrap();
    // Claude Code's own file: lots of state, other user servers, CRLF here to check line endings.
    let old = "{\r\n  \"numStartups\": 42,\r\n  \"mcpServers\": {\r\n    \"github\": {\r\n      \"type\": \"stdio\",\r\n      \"command\": \"npx\",\r\n      \"env\": {\r\n        \"GITHUB_TOKEN\": \"ghp_secret\"\r\n      }\r\n    }\r\n  },\r\n  \"projects\": {\"/r\": {\"mcpServers\": {}}}\r\n}\r\n";
    let p = write(McpTarget::Claude, &home, old);
    let first = connect(McpTarget::Claude, &home, &entry());
    assert!(!first.summary.contains("ghp_secret"), "neighbours never show in the plan");
    let new = read(&p);
    assert!(only_inserted(old, &new), "{new}");
    assert!(!new.replace("\r\n", "").contains('\n'), "CRLF kept: {new:?}");
    assert!(new.contains("    \"pawl\": {\r\n      \"type\": \"stdio\""), "{new}");
    assert_eq!(entry_json(McpTarget::Claude, &p), entry().to_json(McpTarget::Claude));
    let projects: Value = serde_json::from_str(&new).unwrap();
    assert_eq!(projects["projects"]["/r"]["mcpServers"], json!({}), "project scope untouched");
    // A backup of the old file sits next to it.
    let baks: Vec<_> = std::fs::read_dir(home.path()).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().ends_with(".bak")).collect();
    assert_eq!(baks.len(), 1);
    assert_eq!(read(&baks[0].path()), old);
    // Connected: nothing to do. Remove: the old bytes exactly.
    let again = plan(McpTarget::Claude, home.path(), &env(), McpChange::Connect(&entry())).unwrap();
    assert!(!again.changed() && again.summary.contains("already connected"));
    remove(McpTarget::Claude, &home);
    assert_eq!(read(&p), old);
    assert_eq!(state(McpTarget::Claude, &home, Some(&entry())), Ok(EntryState::Absent));
    let nothing = plan(McpTarget::Claude, home.path(), &env(), McpChange::Remove { drop_empty_servers: true }).unwrap();
    assert!(!nothing.changed() && nothing.summary.contains("nothing to change"));
}

#[test]
fn claude_json_without_servers_gets_the_key_added_at_the_end() {
    let home = TempDir::new().unwrap();
    let old = "{\n\t\"numStartups\": 1,\n\t\"theme\": \"dark\"\n}\n";
    let p = write(McpTarget::Claude, &home, old);
    let pl = connect(McpTarget::Claude, &home, &entry());
    assert!(pl.added_servers, "recorded for Remove");
    let new = read(&p);
    assert!(only_inserted(old, &new), "{new}");
    assert!(new.contains("\t\"mcpServers\": {\n\t\t\"pawl\": {\n\t\t\t\"type\""), "tabs kept: {new}");
    remove_with(McpTarget::Claude, &home, pl.added_servers);
    assert_eq!(read(&p), old, "the servers object Pawl added goes too");
}

#[test]
fn a_one_line_file_stays_on_one_line() {
    let home = TempDir::new().unwrap();
    let old = r#"{"mcpServers":{"a":{"command":"x"}},"z":1}"#;
    let p = write(McpTarget::CopilotCli, &home, old);
    connect(McpTarget::CopilotCli, &home, &entry());
    let new = read(&p);
    assert!(!new.contains('\n') && only_inserted(old, &new), "{new}");
    remove(McpTarget::CopilotCli, &home);
    assert_eq!(read(&p), old);
}

#[test]
fn vscode_mcp_json_keeps_comments_and_trailing_commas() {
    let home = TempDir::new().unwrap();
    let old = "{\n  // My servers\n  \"servers\": {\n    \"fetch\": {\n      \"type\": \"stdio\",\n      \"command\": \"uvx\", // fetch server\n      \"args\": [\"mcp-server-fetch\",],\n    },\n  },\n  \"inputs\": [],\n}\n";
    let p = write(McpTarget::Vscode, &home, old);
    connect(McpTarget::Vscode, &home, &entry());
    let new = read(&p);
    assert!(only_inserted(old, &new), "{new}");
    assert!(new.contains("// My servers") && new.contains("// fetch server"));
    assert_eq!(state(McpTarget::Vscode, &home, Some(&entry())), Ok(EntryState::Current));
    remove(McpTarget::Vscode, &home);
    assert_eq!(read(&p), old);
    // Comments are fine for VS Code only.
    let home2 = TempDir::new().unwrap();
    write(McpTarget::CopilotCli, &home2, old.replace("\"servers\"", "\"mcpServers\"").as_str());
    assert!(state(McpTarget::CopilotCli, &home2, None).unwrap_err().contains("is not valid JSON"));
}

#[test]
fn an_empty_servers_object_and_an_empty_file() {
    let home = TempDir::new().unwrap();
    let old = "{\n  \"servers\": {},\n  \"inputs\": []\n}\n";
    let p = write(McpTarget::Vscode, &home, old);
    connect(McpTarget::Vscode, &home, &entry());
    let new = read(&p);
    assert!(new.starts_with("{\n  \"servers\": {\n    \"pawl\": {\n      \"type\": \"stdio\""), "{new}");
    remove(McpTarget::Vscode, &home);
    assert_eq!(read(&p), old, "VS Code's own servers object stays");
    let home2 = TempDir::new().unwrap();
    let p2 = write(McpTarget::CopilotCli, &home2, "  \n");
    connect(McpTarget::CopilotCli, &home2, &entry());
    assert_eq!(entry_json(McpTarget::CopilotCli, &p2), entry().to_json(McpTarget::CopilotCli));
}

#[test]
fn a_stale_entry_is_replaced_and_user_keys_elsewhere_stay() {
    let home = TempDir::new().unwrap();
    let mut old_entry = entry();
    old_entry.token = "f".repeat(32);
    old_entry.url = "http://127.0.0.1:9000".into();
    connect(McpTarget::Claude, &home, &old_entry);
    assert_eq!(state(McpTarget::Claude, &home, Some(&entry())), Ok(EntryState::Stale));
    assert_eq!(state(McpTarget::Claude, &home, None), Ok(EntryState::Stale), "no token to compare: stale");
    let plan = connect(McpTarget::Claude, &home, &entry());
    assert!(plan.summary.contains("replaces") && !plan.summary.contains(&"f".repeat(32)), "{}", plan.summary);
    assert_eq!(state(McpTarget::Claude, &home, Some(&entry())), Ok(EntryState::Current));
    // A field the user added to the entry does not make it stale.
    let p = path_of(McpTarget::Claude, &home);
    let text = read(&p).replace("\"type\": \"stdio\",", "\"type\": \"stdio\",\n      \"timeout\": 30,");
    std::fs::write(&p, text).unwrap();
    assert_eq!(state(McpTarget::Claude, &home, Some(&entry())), Ok(EntryState::Current));
}

#[test]
fn malformed_files_are_refused_and_left_alone() {
    let cases = [
        (McpTarget::Claude, "{\"mcpServers\": {\"a\": 1,}"),
        (McpTarget::Claude, "{\"mcpServers\": [1]}"),
        (McpTarget::CopilotCli, "[]"),
        (McpTarget::Vscode, "{\"servers\": {\"a\": /* x */ }}"),
        (McpTarget::Codex, "[mcp_servers\ncommand = 1"),
        (McpTarget::Codex, "mcp_servers = 5\n"),
    ];
    for (t, text) in cases {
        let home = TempDir::new().unwrap();
        let p = write(t, &home, text);
        let e = format!("{:#}", plan(t, home.path(), &env(), McpChange::Connect(&entry())).unwrap_err());
        assert!(e.contains("add the entry by hand"), "{t:?} {text}: {e}");
        assert!(state(t, &home, Some(&entry())).is_err());
        assert!(plan(t, home.path(), &env(), McpChange::Remove { drop_empty_servers: true }).is_err());
        assert_eq!(read(&p), text, "nothing written");
    }
}

#[test]
fn a_file_changed_after_the_plan_is_not_overwritten() {
    let home = TempDir::new().unwrap();
    let p = write(McpTarget::Claude, &home, "{\"a\": 1}\n");
    let plan = plan(McpTarget::Claude, home.path(), &env(), McpChange::Connect(&entry())).unwrap();
    std::fs::write(&p, "{\"a\": 2}\n").unwrap();
    let e = format!("{:#}", apply(&plan).unwrap_err());
    assert!(e.contains("changed while Pawl was editing it"), "{e}");
    assert_eq!(read(&p), "{\"a\": 2}\n");
}

#[test]
fn claude_config_dir_moves_the_file() {
    let home = TempDir::new().unwrap();
    let dir = home.path().join("cfg");
    let env = McpEnv { claude_config_dir: Some(dir.clone()), ..Default::default() };
    assert_eq!(config_path(McpTarget::Claude, home.path(), &env), dir.join(".claude.json"));
    assert_eq!(config_path(McpTarget::Claude, home.path(), &McpEnv::default()), home.path().join(".claude.json"));
    let env = McpEnv { copilot_home: Some(dir.clone()), codex_home: Some(dir.clone()), ..Default::default() };
    assert_eq!(config_path(McpTarget::CopilotCli, home.path(), &env), dir.join("mcp-config.json"));
    assert_eq!(config_path(McpTarget::Codex, home.path(), &env), dir.join("config.toml"));
}

#[test]
fn entries_never_print_their_token() {
    assert!(!format!("{:?}", entry()).contains(TOKEN));
    let home = TempDir::new().unwrap();
    let plan = plan(McpTarget::Codex, home.path(), &env(), McpChange::Connect(&entry())).unwrap();
    assert!(!format!("{plan:?}").contains(TOKEN) && !plan.summary.contains(TOKEN));
    assert!(plan.summary.contains("[mcp_servers.pawl]") && plan.summary.contains(TOKEN_PLACEHOLDER), "{}", plan.summary);
}

// ---------------------------------------------------------------- Codex TOML

#[test]
fn codex_toml_keeps_the_rest_and_updates_in_place() {
    let home = TempDir::new().unwrap();
    let old = "# my codex\nmodel = \"gpt-5\"   # pinned\n\n[mcp_servers.docs]\ncommand = \"npx\"\nargs = [\"-y\", \"docs-mcp\"]\n\n[profiles.fast]\nmodel = \"mini\"\n";
    let p = write(McpTarget::Codex, &home, old);
    connect(McpTarget::Codex, &home, &entry());
    let new = read(&p);
    assert!(new.starts_with(old.split("[profiles.fast]").next().unwrap()) && new.contains("[profiles.fast]\nmodel = \"mini\"\n"), "{new}");
    assert!(new.contains("[mcp_servers.pawl]") && new.contains("[mcp_servers.pawl.env]"), "{new}");
    assert_eq!(entry_json(McpTarget::Codex, &p), entry().to_json(McpTarget::Codex));
    assert_eq!(state(McpTarget::Codex, &home, Some(&entry())), Ok(EntryState::Current));
    // The user tunes our entry; Connect with a new token keeps the tuning.
    std::fs::write(&p, new.replace("[mcp_servers.pawl]\n", "[mcp_servers.pawl]\nstartup_timeout_sec = 20\n")).unwrap();
    let mut e2 = entry();
    e2.token = "a".repeat(32);
    connect(McpTarget::Codex, &home, &e2);
    let tuned = read(&p);
    assert!(tuned.contains("startup_timeout_sec = 20") && tuned.contains(&"a".repeat(32)) && !tuned.contains(TOKEN), "{tuned}");
    remove(McpTarget::Codex, &home);
    assert_eq!(read(&p), old, "only Pawl's tables went");
}

#[test]
fn codex_fresh_and_already_connected() {
    let home = TempDir::new().unwrap();
    connect(McpTarget::Codex, &home, &entry());
    let p = path_of(McpTarget::Codex, &home);
    let text = read(&p);
    assert!(text.starts_with("[mcp_servers.pawl]\ncommand = "), "{text}");
    let again = plan(McpTarget::Codex, home.path(), &env(), McpChange::Connect(&entry())).unwrap();
    assert!(!again.changed());
    remove(McpTarget::Codex, &home);
    assert_eq!(read(&p).trim(), "", "an emptied implicit [mcp_servers] goes too");
}


// ---------------------------------------------------------------- review round 2

#[test]
fn remove_keeps_the_users_comments_around_pawl() {
    let home = TempDir::new().unwrap();
    // A note above pawl, a comment at the end of the previous member's line, a section header.
    let old = "{\n  \"servers\": {\n    // ---- servers ----\n    \"github\": { \"command\": \"gh\" }, // github note\n    // pawl note\n    \"pawl\": { \"command\": \"old\" }\n  }\n}\n";
    let p = write(McpTarget::Vscode, &home, old);
    remove(McpTarget::Vscode, &home);
    assert_eq!(read(&p), "{\n  \"servers\": {\n    // ---- servers ----\n    \"github\": { \"command\": \"gh\" } // github note\n    // pawl note\n  }\n}\n");
    // pawl in the middle: its line goes, with its comma; the comments around it stay.
    let old = "{\n  \"servers\": {\n    \"a\": 1, // a note\n    // pawl note\n    \"pawl\": {}, // pawl's own\n    \"b\": 2\n  }\n}\n";
    let p = write(McpTarget::Vscode, &home, old);
    remove(McpTarget::Vscode, &home);
    assert_eq!(read(&p), "{\n  \"servers\": {\n    \"a\": 1, // a note\n    // pawl note\n    \"b\": 2\n  }\n}\n");
}

#[test]
fn connect_keeps_a_trailing_comment_on_its_line_and_fills_comment_only_objects() {
    let home = TempDir::new().unwrap();
    let old = "{\n  \"servers\": {\n    \"a\": { \"command\": \"x\" } // a's note\n  }\n}\n";
    let p = write(McpTarget::Vscode, &home, old);
    connect(McpTarget::Vscode, &home, &entry());
    let new = read(&p);
    assert!(new.contains("    \"a\": { \"command\": \"x\" }, // a's note\n    \"pawl\": {"), "{new}");
    assert!(only_inserted(old, &new));
    remove(McpTarget::Vscode, &home);
    assert_eq!(read(&p), old);
    // Only comments inside the servers object: the entry goes after them, no blank line.
    let old = "{\n  \"servers\": {\n    // nothing yet\n  }\n}\n";
    let p = write(McpTarget::Vscode, &home, old);
    connect(McpTarget::Vscode, &home, &entry());
    let new = read(&p);
    assert!(new.starts_with("{\n  \"servers\": {\n    // nothing yet\n    \"pawl\": {\n"), "{new}");
    assert!(!new.contains("\n\n"), "{new}");
    remove(McpTarget::Vscode, &home);
    assert_eq!(read(&p), old);
}

#[test]
fn repeated_pawl_members_are_all_handled() {
    let home = TempDir::new().unwrap();
    let old = "{\n  \"mcpServers\": {\n    \"pawl\": { \"command\": \"old-1\" },\n    \"github\": { \"command\": \"gh\" },\n    \"pawl\": { \"command\": \"old-2\" }\n  }\n}\n";
    let p = write(McpTarget::Claude, &home, old);
    assert_eq!(state(McpTarget::Claude, &home, Some(&entry())), Ok(EntryState::Stale));
    let pl = connect(McpTarget::Claude, &home, &entry());
    assert!(pl.summary.contains("there 2 times"), "{}", pl.summary);
    let new = read(&p);
    assert_eq!(new.matches("\"pawl\"").count(), 1, "{new}");
    assert!(!new.contains("old-1") && !new.contains("old-2") && new.contains("\"github\""));
    assert_eq!(state(McpTarget::Claude, &home, Some(&entry())), Ok(EntryState::Current));
    // Remove takes every copy.
    let p = write(McpTarget::Claude, &home, old);
    remove(McpTarget::Claude, &home);
    assert_eq!(read(&p), "{\n  \"mcpServers\": {\n    \"github\": { \"command\": \"gh\" }\n  }\n}\n");
}

#[test]
fn lone_surrogates_and_odd_escapes_are_kept_byte_for_byte() {
    let home = TempDir::new().unwrap();
    let old = "{\n  \"note\": \"\\ud83d alone \\u00e9\",\n  \"mcpServers\": {}\n}\n";
    let p = write(McpTarget::Claude, &home, old);
    connect(McpTarget::Claude, &home, &entry());
    let new = read(&p);
    assert!(new.contains("\"\\ud83d alone \\u00e9\"") && only_inserted(old, &new), "{new}");
    remove(McpTarget::Claude, &home);
    assert_eq!(read(&p), old, "the user's own empty servers object stays");
    assert!(jsonc::parse("{\"a\": \"\\x\"}", false).unwrap_err().contains("bad escape"));
    assert!(jsonc::parse("{\"a\": \"\\u12\"}", false).is_err());
}

#[test]
fn errors_name_a_place_never_the_files_text() {
    let home = TempDir::new().unwrap();
    for (t, text) in [
        (McpTarget::Claude, "{\n  \"key\": sk-ant-secret-1234\n}"),
        (McpTarget::Vscode, "{\"servers\": {\"x\": {\"token\": \"sk-ant-secret-1234\" \"y\": 1}}}"),
        (McpTarget::Codex, "token = sk-ant-secret-1234\n"),
    ] {
        write(t, &home, text);
        let e = format!("{:#}", plan(t, home.path(), &env(), McpChange::Connect(&entry())).unwrap_err());
        assert!(!e.contains("sk-ant") && e.contains("line ") && e.contains("column "), "{t:?}: {e}");
    }
}

#[test]
fn codex_keeps_crlf_comments_and_every_byte_outside_pawls_tables() {
    let home = TempDir::new().unwrap();
    let old = "# my codex\r\nmodel = \"gpt-5\"\r\n\r\n[mcp_servers.docs] # docs\r\ncommand = \"npx\"\r\n\r\n# ---- profiles ----\r\n[profiles.fast]\r\nmodel = \"mini\"\r\n";
    let p = write(McpTarget::Codex, &home, old);
    connect(McpTarget::Codex, &home, &entry());
    let new = read(&p);
    assert!(!new.replace("\r\n", "").contains('\n'), "CRLF kept: {new:?}");
    assert!(only_inserted(old, &new), "{new:?}");
    assert!(new.contains("command = \"npx\"\r\n\r\n[mcp_servers.pawl]\r\n"), "after the other servers: {new:?}");
    remove(McpTarget::Codex, &home);
    assert_eq!(read(&p), old);
    // A comment block (and a commented-out table) above Pawl's table stays when it goes.
    let old = "[mcp_servers.docs]\ncommand = \"npx\"\n\n# Pawl, added by Connect\n# [mcp_servers.old]\n# command = \"x\"\n[mcp_servers.pawl]\ncommand = \"old\"\nargs = [\"mcp\"] # mine\n\n[mcp_servers.pawl.env]\nPAWL_A2A_TOKEN = \"t\"\n\n[profiles.fast]\nmodel = \"mini\"\n";
    let p = write(McpTarget::Codex, &home, old);
    remove(McpTarget::Codex, &home);
    assert_eq!(read(&p), "[mcp_servers.docs]\ncommand = \"npx\"\n\n# Pawl, added by Connect\n# [mcp_servers.old]\n# command = \"x\"\n\n[profiles.fast]\nmodel = \"mini\"\n");
}

#[test]
fn codex_hooks_keep_crlf_too() {
    let home = TempDir::new().unwrap();
    let cfg = home.path().join(".codex").join("config.toml");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    std::fs::write(&cfg, "model = \"gpt-5\"\r\n\r\n[profiles.fast]\r\nmodel = \"mini\"\r\n").unwrap();
    let exe = home.path().join("pawl.exe");
    crate::hooks::install_with_home(crate::hooks::HookTarget::Codex, &exe, true, home.path(), &crate::hooks::InstallEnv::default()).unwrap();
    let text = read(&cfg);
    assert!(text.contains("notify") && !text.replace("\r\n", "").contains('\n'), "{text:?}");
}

#[test]
fn codex_entries_written_another_way_are_left_to_the_user() {
    let home = TempDir::new().unwrap();
    for text in ["[mcp_servers]\npawl = { command = \"x\" }\n", "[mcp_servers]\npawl.command = \"x\"\n", "[[mcp_servers.pawl]]\ncommand = \"x\"\n"] {
        let p = write(McpTarget::Codex, &home, text);
        let e = format!("{:#}", plan(McpTarget::Codex, home.path(), &env(), McpChange::Remove { drop_empty_servers: true }).unwrap_err());
        assert!(e.contains("edit it by hand"), "{text}: {e}");
        assert_eq!(read(&p), text);
    }
}

#[test]
fn stale_temp_files_are_cleaned_up() {
    let home = TempDir::new().unwrap();
    let p = write(McpTarget::Claude, &home, "{}\n");
    let dir = p.parent().unwrap();
    // `.<file name>.<16 hex>.pawl-tmp`, and the file name is `.claude.json`.
    let stale = dir.join("..claude.json.0123456789abcdef.pawl-tmp");
    let fresh = dir.join("..claude.json.fedcba9876543210.pawl-tmp");
    let other = dir.join(".other.json.0123456789abcdef.pawl-tmp");
    for f in [&stale, &fresh, &other] {
        std::fs::write(f, "{\"token\": \"t\"}").unwrap();
    }
    let old = std::time::SystemTime::now() - Duration::from_secs(600);
    for f in [&stale, &other] {
        std::fs::File::options().write(true).open(f).unwrap().set_modified(old).unwrap();
    }
    assert_eq!(clean_stale_temps(McpTarget::Claude, home.path(), &env()), 1);
    assert!(!stale.exists() && fresh.exists() && other.exists());
}

#[test]
fn only_stable_vscode_counts_as_installed() {
    let dir = TempDir::new().unwrap();
    let name = if cfg!(windows) { "code-insiders.cmd" } else { "code-insiders" };
    std::fs::write(dir.path().join(name), "").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path().join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let env = crate::detect::HostEnv::with_paths(cfg!(windows), vec![dir.path().to_path_buf()], ".CMD;.EXE", None);
    assert!(env.which("code-insiders").is_some());
    assert!(!agent_found(McpTarget::Vscode, &env));
}

#[test]
fn a_changed_file_error_can_be_told_apart() {
    let home = TempDir::new().unwrap();
    let p = write(McpTarget::Claude, &home, "{}\n");
    let pl = plan(McpTarget::Claude, home.path(), &env(), McpChange::Connect(&entry())).unwrap();
    std::fs::write(&p, "{ }\n").unwrap();
    let e = apply(&pl).unwrap_err();
    assert!(ChangedWhileEditing::is(&e));
    assert!(!ChangedWhileEditing::is(&anyhow::anyhow!("other")));
}

#[cfg(unix)]
#[test]
fn backups_and_temp_files_are_private_from_the_start() {
    use std::os::unix::fs::PermissionsExt;
    let home = TempDir::new().unwrap();
    let f = home.path().join("x");
    let file = crate::hooks::install::create_new(&f).unwrap();
    drop(file);
    assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o600);
}

// ---------------------------------------------------------------- review round 3

#[test]
fn an_empty_servers_object_the_user_had_stays_and_one_pawl_added_goes() {
    let home = TempDir::new().unwrap();
    let old = "{\"a\":1,\"mcpServers\":{}}";
    let p = write(McpTarget::Claude, &home, old);
    let pl = connect(McpTarget::Claude, &home, &entry());
    assert!(!pl.added_servers);
    remove_with(McpTarget::Claude, &home, pl.added_servers);
    assert_eq!(read(&p), old);
    let old = "{\"a\":1}";
    let p = write(McpTarget::CopilotCli, &home, old);
    let pl = connect(McpTarget::CopilotCli, &home, &entry());
    assert!(pl.added_servers);
    remove_with(McpTarget::CopilotCli, &home, pl.added_servers);
    assert_eq!(read(&p), old);
    // A file Pawl created ends as an empty object.
    let home2 = TempDir::new().unwrap();
    let pl = connect(McpTarget::Vscode, &home2, &entry());
    assert!(pl.added_servers);
    remove_with(McpTarget::Vscode, &home2, true);
    assert_eq!(read(&path_of(McpTarget::Vscode, &home2)), "{}\n");
}

#[test]
fn codex_connect_then_remove_is_byte_identical() {
    let cases = [
        // No final newline.
        "[mcp_servers.docs]\ncommand = \"npx\"",
        "model = \"x\"",
        // A comment after the last server, with and without a blank line.
        "[mcp_servers.docs]\ncommand = \"npx\"\n# trailing comment\n",
        "[mcp_servers.docs]\ncommand = \"npx\"\n\n# trailing comment\n",
        "[mcp_servers.docs]\ncommand = \"npx\"\n# about profiles\n[profiles.fast]\nmodel = \"mini\"\n",
        // Blank lines already there, CRLF, nothing at all.
        "[mcp_servers.docs]\r\ncommand = \"npx\"\r\n\r\n\r\n[profiles.fast]\r\nmodel = \"mini\"\r\n",
        "model = \"x\"\n\n",
        "",
    ];
    for old in cases {
        let home = TempDir::new().unwrap();
        let p = write(McpTarget::Codex, &home, old);
        connect(McpTarget::Codex, &home, &entry());
        let mid = read(&p);
        assert!(only_inserted(old, &mid), "{old:?} -> {mid:?}");
        assert_eq!(old.ends_with('\n') || old.is_empty(), mid.ends_with('\n'), "final newline kept as it was: {mid:?}");
        remove(McpTarget::Codex, &home);
        assert_eq!(read(&p), old, "{old:?} via {mid:?}");
    }
}

#[test]
fn codex_remove_of_a_table_at_the_top_leaves_no_blank_line_there() {
    let home = TempDir::new().unwrap();
    let p = write(McpTarget::Codex, &home, "[mcp_servers.pawl]\ncommand = \"x\"\n\n[mcp_servers.pawl.env]\nPAWL_A2A_TOKEN = \"t\"\n\n[profiles.fast]\nmodel = \"mini\"\n");
    remove(McpTarget::Codex, &home);
    assert_eq!(read(&p), "[profiles.fast]\nmodel = \"mini\"\n");
}

#[test]
fn only_the_newest_backups_stay() {
    let home = TempDir::new().unwrap();
    let p = write(McpTarget::Claude, &home, "{}\n");
    let dir = p.parent().unwrap();
    let old_baks = [".claude.json.20260101-000000.bak", ".claude.json.20260102-000000.bak", ".claude.json.20260102-000000-1.bak", ".claude.json.20260103-000000.bak"];
    for b in old_baks {
        std::fs::write(dir.join(b), "old").unwrap();
    }
    // Not Pawl's: other names stay whatever their age.
    let keep = [".claude.json.bak", ".claude.json.mine.bak", "other.json.20260101-000000.bak"];
    for b in keep {
        std::fs::write(dir.join(b), "mine").unwrap();
    }
    connect(McpTarget::Claude, &home, &entry());
    let names: std::collections::BTreeSet<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    let pawl_baks: Vec<&String> = names.iter().filter(|n| n.starts_with(".claude.json.2") && n.ends_with(".bak")).collect();
    assert_eq!(pawl_baks.len(), 3, "{names:?}");
    assert!(!names.contains(".claude.json.20260101-000000.bak") && !names.contains(".claude.json.20260102-000000.bak"), "{names:?}");
    assert!(names.contains(".claude.json.20260102-000000-1.bak") && names.contains(".claude.json.20260103-000000.bak"));
    for k in keep {
        assert!(names.contains(k), "{k}");
    }
}
