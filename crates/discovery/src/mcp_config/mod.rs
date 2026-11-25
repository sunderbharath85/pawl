//! "Connect coding agents" (Settings > A2A): put a `pawl` MCP server entry, which runs `pawl mcp`
//! (a stdio MCP client of Pawl's localhost A2A server), into a coding agent's own user-level MCP
//! config, or take it out again.
//!
//! Formats (checked 2026-11-25 against each agent's docs, see docs/reports/sde-a2a.md):
//! - Claude Code: `~/.claude.json` (`$CLAUDE_CONFIG_DIR/.claude.json`), user scope is the top-level
//!   `mcpServers`: `{"type": "stdio", "command", "args", "env"}`.
//! - Codex: `~/.codex/config.toml` (`$CODEX_HOME`): `[mcp_servers.<name>]` `command`, `args`, and an
//!   `env` table.
//! - Copilot CLI: `~/.copilot/mcp-config.json` (`$COPILOT_HOME`): `mcpServers`, `{"type": "local",
//!   "command", "args", "env", "tools": ["*"]}`.
//! - VS Code: the user profile's `mcp.json` (JSON with comments): `servers`, `{"type": "stdio",
//!   "command", "args", "env"}`.
//!
//! Only Pawl's own bytes change: JSON is edited in place ([`jsonc`]); in TOML only the text of
//! Pawl's `[mcp_servers.pawl]` tables is cut out of the original or spliced into it. Comments, key
//! order, indentation and line endings elsewhere stay. Every edit is read back and refused if it
//! would change anything but Pawl's entry. A file is backed up before it is changed, and is not
//! overwritten if it changed after it was read ([`ChangedWhileEditing`]).

use crate::hooks::install::{backup_copy, backup_path, copy_permissions, create_new, item_to_json, prune_backups, random_suffix, read_opt, utc_stamp, KEEP_BACKUPS};
use anyhow::{bail, Context};
use serde_json::{json, Value};
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;
use toml_edit::{DocumentMut, Item, Table};

pub(crate) mod jsonc;

/// The name of Pawl's entry in every agent's MCP config.
pub const ENTRY_NAME: &str = "pawl";
/// `pawl mcp`: the subcommand the entry runs.
pub const MCP_ARG: &str = "mcp";
/// Env vars of the entry: Pawl's A2A URL and the caller's bearer token.
pub const URL_ENV: &str = "PAWL_A2A_URL";
pub const TOKEN_ENV: &str = "PAWL_A2A_TOKEN";
/// What plans show instead of a token.
pub const TOKEN_PLACEHOLDER: &str = "<new token>";
/// Bigger config files are not edited.
const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
/// A temp file of Pawl's older than this is left over from a crash (it may hold a token).
const STALE_TEMP: Duration = Duration::from_secs(60);
/// At most this many duplicate `pawl` members are taken out of one object.
const MAX_DUPLICATES: usize = 64;

/// The file changed between Pawl reading it and writing it (the agent itself rewrites it often):
/// nothing was written; reading it again and retrying is safe.
#[derive(Debug)]
pub struct ChangedWhileEditing(pub PathBuf);

impl fmt::Display for ChangedWhileEditing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} changed while Pawl was editing it; try again", self.0.display())
    }
}

impl std::error::Error for ChangedWhileEditing {}

impl ChangedWhileEditing {
    /// Is `e` (or what it wraps) this error?
    pub fn is(e: &anyhow::Error) -> bool {
        e.chain().any(|c| c.is::<ChangedWhileEditing>())
    }
}

/// A coding agent Connect can wire up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpTarget {
    Claude,
    Codex,
    CopilotCli,
    Vscode,
}

impl McpTarget {
    pub const ALL: [McpTarget; 4] = [McpTarget::Claude, McpTarget::Codex, McpTarget::CopilotCli, McpTarget::Vscode];

    pub fn name(self) -> &'static str {
        match self {
            McpTarget::Claude => "Claude Code",
            McpTarget::Codex => "Codex",
            McpTarget::CopilotCli => "Copilot CLI",
            McpTarget::Vscode => "Copilot in VS Code",
        }
    }

    /// The A2A caller (`[[a2a.callers]]` id) this agent uses.
    pub fn caller_id(self) -> &'static str {
        match self {
            McpTarget::Claude => "claude-code",
            McpTarget::Codex => "codex",
            McpTarget::CopilotCli => "copilot-cli",
            McpTarget::Vscode => "copilot-vscode",
        }
    }

    /// Where the servers live in the file ("mcpServers", "servers", "mcp_servers").
    pub fn servers_key(self) -> &'static str {
        match self {
            McpTarget::Claude | McpTarget::CopilotCli => "mcpServers",
            McpTarget::Vscode => "servers",
            McpTarget::Codex => "mcp_servers",
        }
    }

    /// VS Code reads JSON with comments and trailing commas; the others plain JSON.
    fn jsonc(self) -> bool {
        self == McpTarget::Vscode
    }
}

/// Is the agent on this machine? `claude`, `codex`, `copilot`, or stable VS Code's `code` launcher
/// (Insiders keeps its config elsewhere, which Connect does not write).
pub fn agent_found(target: McpTarget, env: &crate::detect::HostEnv) -> bool {
    let name = match target {
        McpTarget::Claude => "claude",
        McpTarget::Codex => "codex",
        McpTarget::CopilotCli => "copilot",
        McpTarget::Vscode => "code",
    };
    env.which(name).is_some()
}

/// Environment overrides the agents honour (as the hub process sees them).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct McpEnv {
    /// `CLAUDE_CONFIG_DIR`: `.claude.json` lives there instead of the home folder.
    pub claude_config_dir: Option<PathBuf>,
    pub codex_home: Option<PathBuf>,
    pub copilot_home: Option<PathBuf>,
    /// Windows `APPDATA` (default `<home>\AppData\Roaming`).
    pub appdata: Option<PathBuf>,
    /// Linux `XDG_CONFIG_HOME` (default `<home>/.config`).
    pub xdg_config_home: Option<PathBuf>,
}

impl McpEnv {
    pub fn from_system() -> Self {
        let get = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        McpEnv {
            claude_config_dir: get("CLAUDE_CONFIG_DIR"),
            codex_home: get("CODEX_HOME"),
            copilot_home: get("COPILOT_HOME"),
            appdata: get("APPDATA"),
            xdg_config_home: get("XDG_CONFIG_HOME"),
        }
    }
}

/// The file `target`'s entry lives in.
pub fn config_path(target: McpTarget, home: &Path, env: &McpEnv) -> PathBuf {
    match target {
        McpTarget::Claude => env.claude_config_dir.clone().unwrap_or_else(|| home.to_path_buf()).join(".claude.json"),
        McpTarget::Codex => env.codex_home.clone().unwrap_or_else(|| home.join(".codex")).join("config.toml"),
        McpTarget::CopilotCli => env.copilot_home.clone().unwrap_or_else(|| home.join(".copilot")).join("mcp-config.json"),
        McpTarget::Vscode => vscode_user_dir(home, env).join("mcp.json"),
    }
}

fn vscode_user_dir(home: &Path, env: &McpEnv) -> PathBuf {
    let base = if cfg!(windows) {
        env.appdata.clone().unwrap_or_else(|| home.join("AppData").join("Roaming"))
    } else if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support")
    } else {
        env.xdg_config_home.clone().unwrap_or_else(|| home.join(".config"))
    };
    base.join("Code").join("User")
}

/// What Pawl's entry should hold.
#[derive(Clone, PartialEq)]
pub struct McpEntry {
    /// The Pawl executable (absolute).
    pub exe: PathBuf,
    /// `http://127.0.0.1:<port>`.
    pub url: String,
    pub token: String,
}

impl fmt::Debug for McpEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpEntry").field("exe", &self.exe).field("url", &self.url).field("token", &"<redacted>").finish()
    }
}

impl McpEntry {
    /// The same entry with the token replaced by `placeholder` (for plans).
    pub fn redacted(&self, placeholder: &str) -> McpEntry {
        McpEntry { token: placeholder.to_string(), ..self.clone() }
    }

    fn env(&self) -> Value {
        json!({ URL_ENV: self.url, TOKEN_ENV: self.token })
    }

    /// The entry as the agent's JSON (Codex: the same keys its TOML table gets).
    pub fn to_json(&self, target: McpTarget) -> Value {
        let exe = self.exe.to_string_lossy();
        match target {
            McpTarget::Claude | McpTarget::Vscode => json!({ "type": "stdio", "command": exe, "args": [MCP_ARG], "env": self.env() }),
            McpTarget::CopilotCli => json!({ "type": "local", "command": exe, "args": [MCP_ARG], "env": self.env(), "tools": ["*"] }),
            McpTarget::Codex => json!({ "command": exe, "args": [MCP_ARG], "env": self.env() }),
        }
    }
}

/// Does `have` hold every field Pawl writes, with `want`'s values (other fields are the user's)?
fn entry_matches(have: &Value, want: &Value) -> bool {
    let (Some(h), Some(w)) = (have.as_object(), want.as_object()) else { return false };
    w.iter().all(|(k, v)| match (k.as_str(), v) {
        ("env", Value::Object(env)) => env.iter().all(|(ek, ev)| h.get("env").and_then(|e| e.get(ek)) == Some(ev)),
        _ => h.get(k) == Some(v),
    })
}

/// The entry as it stands in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryState {
    /// It runs this Pawl with the wanted URL and token.
    Current,
    /// A `pawl` entry is there but differs (or there is no token to compare with, or it is there twice).
    Stale,
    Absent,
}

#[derive(Debug, Clone)]
pub struct McpCheck {
    pub config_path: PathBuf,
    /// Err: the file cannot be read or edited (why).
    pub state: Result<EntryState, String>,
}

/// What Pawl's entry looks like in a file's text.
struct Found {
    /// The (last) entry, as JSON.
    entry: Option<Value>,
    /// How many `pawl` members there are (JSON allows repeats).
    count: usize,
}

/// The entry's state, reading the file only. `want` None: Pawl has no token for this agent's caller,
/// so an entry can only be stale.
pub fn check(target: McpTarget, home: &Path, env: &McpEnv, want: Option<&McpEntry>) -> McpCheck {
    let path = config_path(target, home, env);
    let state = (|| -> anyhow::Result<EntryState> {
        let (_, text) = read_config(&path)?;
        let found = match text {
            None => Found { entry: None, count: 0 },
            Some(t) => find_entry(target, &path, &t)?,
        };
        Ok(match (found.entry, want) {
            (None, _) => EntryState::Absent,
            (Some(h), Some(w)) if found.count == 1 && entry_matches(&h, &w.to_json(target)) => EntryState::Current,
            (Some(_), _) => EntryState::Stale,
        })
    })()
    .map_err(|e| format!("{e:#}"));
    McpCheck { config_path: path, state }
}

/// Connect (with the entry to write) or remove.
#[derive(Debug, Clone, Copy)]
pub enum McpChange<'a> {
    Connect(&'a McpEntry),
    /// `drop_empty_servers`: Pawl added the servers object when it connected (the hub records
    /// [`McpPlan::added_servers`]), so it goes too if Remove leaves it empty. Otherwise it stays.
    Remove { drop_empty_servers: bool },
}

/// A change to one file, computed from what was read; [`apply`] writes it.
#[derive(Clone)]
pub struct McpPlan {
    pub target: McpTarget,
    /// The file as named (a link is followed when writing).
    pub path: PathBuf,
    /// The file written (`path`, or the file a link points at).
    real: PathBuf,
    old: Option<String>,
    new: Option<String>,
    /// Plain text for the user; never holds the token or anything else from the file.
    pub summary: String,
    /// Connect adds the servers object itself (or creates the file): worth recording, so Remove
    /// can take an object Pawl made away again and leave one the user had.
    pub added_servers: bool,
}

impl fmt::Debug for McpPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpPlan").field("target", &self.target).field("path", &self.path).field("changed", &self.changed()).finish()
    }
}

impl McpPlan {
    /// Applying it would write the file.
    pub fn changed(&self) -> bool {
        self.new.is_some()
    }
}

/// The change `change` makes to `target`'s config, nothing written. Malformed files are refused.
pub fn plan(target: McpTarget, home: &Path, env: &McpEnv, change: McpChange) -> anyhow::Result<McpPlan> {
    let path = config_path(target, home, env);
    let (real, old) = read_config(&path)?;
    let label = format!("== {}: {} ==\n", target.name(), path.display());
    let key = target.servers_key();
    let found = match &old {
        Some(t) => find_entry(target, &path, t)?,
        None => Found { entry: None, count: 0 },
    };
    let mut added = false;
    let (new, summary) = match change {
        McpChange::Connect(entry) => {
            let want = entry.to_json(target);
            let shown = entry_text(target, &entry.redacted(TOKEN_PLACEHOLDER));
            if found.count == 1 && found.entry.as_ref().is_some_and(|h| entry_matches(h, &want)) {
                (None, format!("{label}already connected; nothing to change\n"))
            } else {
                let (new, add) = edit(target, &path, old.as_deref(), Some(entry), false)?;
                added = add;
                let verb = match (&old, found.count) {
                    (None, _) => format!("creates the file with the MCP server \"{ENTRY_NAME}\" under {key}"),
                    (Some(_), 0) => format!("adds the MCP server \"{ENTRY_NAME}\" under {key}"),
                    (Some(_), 1) => format!("replaces Pawl's MCP server \"{ENTRY_NAME}\" under {key}"),
                    (Some(_), n) => format!("replaces Pawl's MCP server \"{ENTRY_NAME}\" under {key} (it was there {n} times; one is kept)"),
                };
                let backup = if old.is_some() { "\nThe file is backed up first (a .bak file next to it).\n" } else { "\n" };
                (Some(new), format!("{label}{verb}:\n{shown}{backup}"))
            }
        }
        McpChange::Remove { drop_empty_servers } => {
            if found.count > 0 {
                let (new, _) = edit(target, &path, old.as_deref(), None, drop_empty_servers)?;
                (Some(new), format!("{label}removes the MCP server \"{ENTRY_NAME}\" from {key} (the file is backed up first)\n"))
            } else {
                (None, format!("{label}no Pawl entry here; nothing to change\n"))
            }
        }
    };
    Ok(McpPlan { target, path, real, old, new, summary, added_servers: added })
}

/// Write `plan`: a backup of the old file first, then the new text, atomically. Refused with
/// [`ChangedWhileEditing`] if the file changed since the plan read it. Returns what happened
/// ("created", "updated (backup: ...)").
pub fn apply(plan: &McpPlan) -> anyhow::Result<String> {
    let Some(new) = &plan.new else { return Ok("nothing to change".into()) };
    let backup = match &plan.old {
        Some(old) => {
            ensure_unchanged(&plan.real, Some(old))?;
            let bak = backup_path(&plan.real, &utc_stamp(pawl_protocol::now_ms() / 1000));
            backup_copy(&plan.real, &bak).with_context(|| format!("backing up {} to {}", plan.real.display(), bak.display()))?;
            // Only the newest few backups stay (each may hold a token).
            prune_backups(&plan.real, KEEP_BACKUPS);
            Some(bak)
        }
        None => None,
    };
    write_checked(&plan.real, new, plan.old.as_deref())?;
    Ok(match backup {
        Some(b) => format!("updated (backup: {})", b.display()),
        None => "created".into(),
    })
}

/// Delete Pawl's temp files next to `target`'s config that a crash left behind (they may hold a
/// token). Returns how many went.
pub fn clean_stale_temps(target: McpTarget, home: &Path, env: &McpEnv) -> usize {
    let path = config_path(target, home, env);
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().map(|n| n.to_string_lossy().into_owned())) else { return 0 };
    let Ok(entries) = std::fs::read_dir(dir) else { return 0 };
    let prefix = format!(".{name}.");
    let mut n = 0;
    for e in entries.flatten() {
        let file = e.file_name().to_string_lossy().into_owned();
        let Some(mid) = file.strip_prefix(&prefix).and_then(|r| r.strip_suffix(".pawl-tmp")) else { continue };
        if mid.len() != 16 || !mid.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let old = e.metadata().ok().filter(|m| m.is_file()).and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).is_some_and(|age| age > STALE_TEMP);
        if old && std::fs::remove_file(e.path()).is_ok() {
            n += 1;
        }
    }
    n
}

// ---------------------------------------------------------------- reading

/// The file a link points at (written in its place), and the text (None: no file).
fn read_config(path: &Path) -> anyhow::Result<(PathBuf, Option<String>)> {
    let real = match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(path).with_context(|| format!("{} is a link Pawl cannot follow", path.display()))?,
        _ => path.to_path_buf(),
    };
    if std::fs::metadata(&real).is_ok_and(|m| m.len() > MAX_FILE_BYTES as u64) {
        bail!("{} is too large for Pawl to edit; add the entry by hand", path.display());
    }
    let text = read_opt(&real)?;
    Ok((real, text))
}

fn not_valid(path: &Path, what: &str, why: &str) -> anyhow::Error {
    anyhow::anyhow!("{} is not valid {what} ({why}); fix it or add the entry by hand", path.display())
}

/// Line and column of byte `at` (for errors that must not quote the file).
fn line_col(text: &str, at: usize) -> (usize, usize) {
    let b = &text.as_bytes()[..at.min(text.len())];
    (b.iter().filter(|c| **c == b'\n').count() + 1, b.iter().rev().take_while(|c| **c != b'\n').count() + 1)
}

/// The JSON root of a config file (empty text: none yet).
fn json_root(target: McpTarget, path: &Path, text: &str) -> anyhow::Result<Option<jsonc::Node>> {
    if text.trim_start_matches('\u{feff}').trim().is_empty() {
        return Ok(None);
    }
    let what = if target.jsonc() { "JSON with comments" } else { "JSON" };
    let root = jsonc::parse(text, target.jsonc()).map_err(|e| not_valid(path, what, &e))?;
    if let Some(m) = root.get(target.servers_key()) {
        if m.value.members().is_none() {
            bail!("\"{}\" in {} is not an object; fix it or add the entry by hand", target.servers_key(), path.display());
        }
    }
    Ok(Some(root))
}

/// The TOML document with spans; `mcp_servers` must be a table.
fn toml_doc(path: &Path, text: &str) -> anyhow::Result<toml_edit::Document<String>> {
    let doc = toml_edit::Document::parse(text.to_string()).map_err(|e| {
        let (line, col) = line_col(text, e.span().map(|s| s.start).unwrap_or(0));
        not_valid(path, "TOML", &format!("it does not parse at line {line}, column {col}"))
    })?;
    if let Some(item) = doc.as_table().get("mcp_servers") {
        if !item.is_table() {
            bail!("mcp_servers in {} is not a [mcp_servers] table; add the entry by hand", path.display());
        }
    }
    Ok(doc)
}

/// Pawl's entry in the file text.
fn find_entry(target: McpTarget, path: &Path, text: &str) -> anyhow::Result<Found> {
    if target == McpTarget::Codex {
        let doc = toml_doc(path, text)?;
        toml_regions(path, text, &doc)?;
        let entry = doc.as_table().get("mcp_servers").and_then(|s| s.as_table()).and_then(|t| t.get(ENTRY_NAME)).map(item_to_json);
        return Ok(Found { count: usize::from(entry.is_some()), entry });
    }
    let Some(root) = json_root(target, path, text)? else { return Ok(Found { entry: None, count: 0 }) };
    let servers = root.get(target.servers_key()).map(|m| &m.value);
    Ok(Found {
        entry: servers.and_then(|s| s.get(ENTRY_NAME)).map(|m| m.value.to_value(text)),
        count: servers.map(|s| s.count(ENTRY_NAME)).unwrap_or(0),
    })
}

// ---------------------------------------------------------------- editing JSON

/// The new file text: Pawl's entry set to `entry`, or removed (None; `drop_empty_servers`: an
/// emptied servers object goes too). Also: whether Connect added the servers object itself.
fn edit(target: McpTarget, path: &Path, old: Option<&str>, entry: Option<&McpEntry>, drop_empty_servers: bool) -> anyhow::Result<(String, bool)> {
    if target == McpTarget::Codex {
        return Ok((edit_toml(path, old.unwrap_or(""), entry)?, false));
    }
    let key = target.servers_key();
    let mut text = old.unwrap_or("").to_string();
    let Some(_) = json_root(target, path, &text)? else {
        let Some(e) = entry else { return Ok((text, false)) };
        let mut s = serde_json::to_string_pretty(&json!({ key: { ENTRY_NAME: e.to_json(target) } })).expect("serializable");
        s.push('\n');
        return Ok((s, true));
    };
    let reparse = |t: &str| jsonc::parse(t, target.jsonc()).map_err(|e| not_valid(path, "JSON", &e));
    // Repeated `pawl` members: Connect keeps the last one (which every reader uses), Remove takes all.
    let keep = usize::from(entry.is_some());
    for _ in 0..MAX_DUPLICATES {
        let root = reparse(&text)?;
        let Some(servers) = root.get(key).map(|m| m.value.clone()) else { break };
        if servers.count(ENTRY_NAME) <= keep {
            break;
        }
        let first = servers.members().unwrap_or_default().iter().position(|m| m.key == ENTRY_NAME).expect("counted");
        text = jsonc::remove_member_at(&text, &servers, first);
    }
    let root = reparse(&text)?;
    let style = jsonc::Style::of(&text, &root);
    let added = entry.is_some() && root.get(key).is_none();
    text = match (entry, root.get(key)) {
        (Some(e), Some(servers)) => jsonc::set_member(&text, &servers.value, ENTRY_NAME, &e.to_json(target), &style),
        (Some(e), None) => jsonc::set_member(&text, &root, key, &json!({ ENTRY_NAME: e.to_json(target) }), &style),
        (None, Some(servers)) => {
            // A servers object Pawl added, left empty (no members, no comments), goes too; one the
            // user had stays, byte for byte.
            let inner = &text[servers.value.start + 1..servers.value.end - 1];
            let empty = servers.value.members().is_some_and(|m| m.is_empty()) && inner.trim().is_empty();
            if empty && drop_empty_servers {
                let i = root.members().unwrap_or_default().iter().rposition(|m| m.key == key).expect("found");
                jsonc::remove_member_at(&text, &root, i)
            } else {
                text
            }
        }
        (None, None) => text,
    };
    verify_json(target, path, old.unwrap_or(""), &text, entry)?;
    Ok((text, added))
}

/// Without Pawl's entry (and a servers object that only held it), as JSON.
fn without_ours(mut v: Value, key: &str) -> (Value, Option<Value>) {
    let mut ours = None;
    if let Some(obj) = v.as_object_mut() {
        if let Some(servers) = obj.get_mut(key).and_then(Value::as_object_mut) {
            ours = servers.shift_remove(ENTRY_NAME);
            if servers.is_empty() {
                obj.shift_remove(key);
            }
        }
    }
    (v, ours)
}

/// The edited text reads back as the old document with only Pawl's entry changed.
fn verify_json(target: McpTarget, path: &Path, old: &str, new: &str, entry: Option<&McpEntry>) -> anyhow::Result<()> {
    let key = target.servers_key();
    let read = |text: &str| -> anyhow::Result<(Value, Option<Value>, usize)> {
        if text.trim_start_matches('\u{feff}').trim().is_empty() {
            return Ok((json!({}), None, 0));
        }
        let root = jsonc::parse(text, target.jsonc()).map_err(|_| anyhow::anyhow!("Pawl's edit of {} did not read back; nothing was written", path.display()))?;
        let count = root.get(key).map(|m| m.value.count(ENTRY_NAME)).unwrap_or(0);
        let (v, ours) = without_ours(root.to_value(text), key);
        Ok((v, ours, count))
    };
    let (before, _, _) = read(old)?;
    let (after, ours, count) = read(new)?;
    let entry_ok = match entry {
        Some(e) => count == 1 && ours.as_ref() == Some(&e.to_json(target)),
        None => count == 0,
    };
    if before != after || !entry_ok {
        bail!("Pawl's edit of {} would change more than its own entry; nothing was written", path.display());
    }
    if !target.jsonc() && !new.trim().is_empty() {
        serde_json::from_str::<serde::de::IgnoredAny>(new.trim_start_matches('\u{feff}')).map_err(|_| anyhow::anyhow!("Pawl's edit of {} is not valid JSON; nothing was written", path.display()))?;
    }
    Ok(())
}

// ---------------------------------------------------------------- editing TOML

fn line_start(text: &str, at: usize) -> usize {
    text[..at].rfind('\n').map(|i| i + 1).unwrap_or(0)
}

/// The position after the newline ending the line that holds byte `at` (or the end of the text).
fn line_end(text: &str, at: usize) -> usize {
    text[at..].find('\n').map(|i| at + i + 1).unwrap_or(text.len())
}

/// What Remove cuts for Pawl's tables at `regions`: each table, plus the one blank line Connect puts
/// above it; at the end of a file without a final newline, also the newline Connect put before;
/// at the top of the file, the blank line after (the file never starts with one). Merged, in order.
fn toml_cuts(text: &str, regions: &[Range<usize>]) -> Vec<(usize, usize)> {
    let mut cuts: Vec<(usize, usize)> = Vec::new();
    for r in regions {
        let mut start = r.start;
        if start > 0 {
            let prev = line_start(text, start - 1);
            if text[prev..start].trim().is_empty() {
                start = prev;
            }
        }
        match cuts.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(r.end),
            _ => cuts.push((start, r.end)),
        }
    }
    if let Some(last) = cuts.last_mut() {
        if last.1 == text.len() && !text.ends_with('\n') {
            if text[..last.0].ends_with("\r\n") {
                last.0 -= 2;
            } else if text[..last.0].ends_with('\n') {
                last.0 -= 1;
            }
        }
    }
    let second = cuts.get(1).map(|c| c.0);
    if let Some(first) = cuts.first_mut() {
        if first.0 == 0 && first.1 < text.len() {
            let next = line_end(text, first.1);
            if text[first.1..next].trim().is_empty() && second.is_none_or(|s| s >= next) {
                first.1 = next;
            }
        }
    }
    cuts
}

/// The end of the last value of `t` (its own keys and dotted keys, not sub-tables with headers).
fn values_end(t: &Table, mut end: usize) -> usize {
    for (_, item) in t.iter() {
        match item {
            Item::Value(v) => end = end.max(v.span().map(|s| s.end).unwrap_or(0)),
            Item::Table(sub) if sub.is_dotted() => end = values_end(sub, end),
            _ => {}
        }
    }
    end
}

/// A table with a header: from the start of the header's line through the line of its last value.
fn table_region(text: &str, t: &Table) -> Option<Range<usize>> {
    let h = t.span()?;
    Some(line_start(text, h.start)..line_end(text, values_end(t, h.end).max(h.end) - 1))
}

/// Regions of `t` and every sub-table with a header below it.
fn regions_below(text: &str, t: &Table, out: &mut Vec<Range<usize>>) -> Result<(), ()> {
    if !t.is_implicit() && !t.is_dotted() {
        out.extend(table_region(text, t));
    }
    for (_, item) in t.iter() {
        match item {
            Item::Table(sub) if !sub.is_dotted() => regions_below(text, sub, out)?,
            Item::ArrayOfTables(_) => return Err(()),
            _ => {}
        }
    }
    Ok(())
}

/// Pawl's tables (`[mcp_servers.pawl]` and its sub-tables) in file order, and where a new entry
/// goes: after the last other `mcp_servers` table, or at the end. An entry written another way (an
/// inline table, dotted keys, an array of tables) is refused: Pawl does not edit it.
fn toml_regions(path: &Path, text: &str, doc: &toml_edit::Document<String>) -> anyhow::Result<(Vec<Range<usize>>, usize)> {
    let refuse = || anyhow::anyhow!("Pawl's entry in {} is not a [mcp_servers.pawl] table; edit it by hand", path.display());
    let Some(servers) = doc.as_table().get("mcp_servers").and_then(Item::as_table) else { return Ok((Vec::new(), text.len())) };
    let mut ours = Vec::new();
    match servers.get(ENTRY_NAME) {
        None => {}
        Some(Item::Table(t)) if !t.is_dotted() => regions_below(text, t, &mut ours).map_err(|_| refuse())?,
        Some(_) => return Err(refuse()),
    }
    let mut others = Vec::new();
    if !servers.is_implicit() && !servers.is_dotted() {
        others.extend(table_region(text, servers));
    }
    for (k, item) in servers.iter() {
        if let (true, Item::Table(t)) = (k != ENTRY_NAME, item) {
            if !t.is_dotted() {
                let _ = regions_below(text, t, &mut others);
            }
        }
    }
    ours.sort_by_key(|r| r.start);
    let anchor = others.iter().map(|r| r.end).max().unwrap_or(text.len());
    Ok((ours, anchor))
}

/// Cut Pawl's tables out of `old` and, for Connect, splice in the new ones (rendered by
/// `toml_edit`, so keys the user added to Pawl's table stay). Nothing else of `old` changes.
fn edit_toml(path: &Path, old: &str, entry: Option<&McpEntry>) -> anyhow::Result<String> {
    let nl = if old.contains("\r\n") { "\r\n" } else { "\n" };
    let parsed = toml_doc(path, old)?;
    let (regions, anchor) = toml_regions(path, old, &parsed)?;
    let cuts = toml_cuts(old, &regions);
    let pos = cuts.first().map(|c| c.0).unwrap_or(anchor);
    let mut base = old.to_string();
    for (a, b) in cuts.iter().rev() {
        base.replace_range(*a..*b, "");
    }
    let ours = match entry {
        None => String::new(),
        Some(e) => {
            let mut doc = parsed.clone().into_mut();
            set_toml_entry(&mut doc, e);
            let rendered = doc.to_string();
            let back = toml_doc(path, &rendered)?;
            let (r0, _) = toml_regions(path, &rendered, &back)?;
            let tables: Vec<String> = r0
                .iter()
                .map(|r| {
                    let mut t = rendered[r.clone()].replace("\r\n", "\n").replace('\n', nl);
                    if !t.ends_with('\n') {
                        t.push_str(nl);
                    }
                    t
                })
                .collect();
            tables.join(nl)
        }
    };
    let out = if ours.is_empty() {
        base
    } else {
        // Exactly what Remove takes away again (toml_cuts): one blank line above Pawl's tables; a
        // file without a final newline keeps having none.
        let before = &base[..pos];
        let after = &base[pos..];
        if before.is_empty() {
            format!("{ours}{after}")
        } else if before.ends_with('\n') {
            format!("{before}{nl}{ours}{after}")
        } else {
            format!("{before}{nl}{nl}{}{after}", ours.strip_suffix(nl).unwrap_or(&ours))
        }
    };
    verify_toml(path, old, &out, entry)?;
    Ok(out)
}

/// Set Pawl's entry in a parsed document: its table updated in place (other keys kept) or added.
fn set_toml_entry(doc: &mut DocumentMut, e: &McpEntry) {
    if doc.get("mcp_servers").is_none() {
        let mut t = Table::new();
        t.set_implicit(true);
        doc.insert("mcp_servers", Item::Table(t));
    }
    let servers = doc["mcp_servers"].as_table_mut().expect("checked table");
    let exe = e.exe.to_string_lossy().into_owned();
    let mut args = toml_edit::Array::new();
    args.push(MCP_ARG);
    match servers.get_mut(ENTRY_NAME).and_then(Item::as_table_mut) {
        Some(t) => {
            t["command"] = toml_edit::value(exe);
            t["args"] = toml_edit::value(args);
            match t.get_mut("env").and_then(Item::as_table_like_mut) {
                Some(env) => {
                    env.insert(URL_ENV, toml_edit::value(e.url.clone()));
                    env.insert(TOKEN_ENV, toml_edit::value(e.token.clone()));
                }
                None => {
                    t.insert("env", Item::Table(env_table(e)));
                }
            }
        }
        None => {
            let mut t = Table::new();
            t["command"] = toml_edit::value(exe);
            t["args"] = toml_edit::value(args);
            t.insert("env", Item::Table(env_table(e)));
            servers.insert(ENTRY_NAME, Item::Table(t));
        }
    }
}

/// The whole document as JSON, without Pawl's entry (and an `mcp_servers` that only held it).
fn toml_without_ours(doc: &toml_edit::Document<String>) -> (Value, Option<Value>) {
    let mut v = serde_json::Map::new();
    for (k, item) in doc.as_table().iter() {
        v.insert(k.to_string(), item_to_json(item));
    }
    without_ours(Value::Object(v), "mcp_servers")
}

fn verify_toml(path: &Path, old: &str, new: &str, entry: Option<&McpEntry>) -> anyhow::Result<()> {
    let failed = || anyhow::anyhow!("Pawl's edit of {} would change more than its own entry; nothing was written", path.display());
    let before = toml_doc(path, old).map_err(|_| failed())?;
    let after = toml_doc(path, new).map_err(|_| failed())?;
    let (b, _) = toml_without_ours(&before);
    let (a, ours) = toml_without_ours(&after);
    let entry_ok = match entry {
        Some(e) => ours.as_ref().is_some_and(|o| entry_matches(o, &e.to_json(McpTarget::Codex))),
        None => ours.is_none(),
    };
    if a != b || !entry_ok {
        return Err(failed());
    }
    Ok(())
}

fn env_table(e: &McpEntry) -> Table {
    let mut env = Table::new();
    env.insert(URL_ENV, toml_edit::value(e.url.clone()));
    env.insert(TOKEN_ENV, toml_edit::value(e.token.clone()));
    env
}

/// The entry as it will appear in the file (for plans: call with a redacted entry).
fn entry_text(target: McpTarget, entry: &McpEntry) -> String {
    if target == McpTarget::Codex {
        if let Ok(text) = edit_toml(Path::new("config.toml"), "", Some(entry)) {
            return text;
        }
    }
    let mut s = serde_json::to_string_pretty(&json!({ ENTRY_NAME: entry.to_json(target) })).expect("serializable");
    s.push('\n');
    s
}

// ---------------------------------------------------------------- writing

fn ensure_unchanged(path: &Path, expect: Option<&str>) -> anyhow::Result<()> {
    if read_opt(path)?.as_deref() != expect {
        return Err(ChangedWhileEditing(path.to_path_buf()).into());
    }
    Ok(())
}

/// Replace `path` atomically with `content` (a randomly named temp file created exclusively next to
/// it, private to the user from the start, then given the old file's permissions), if the file
/// still holds `expect`.
fn write_checked(path: &Path, content: &str, expect: Option<&str>) -> anyhow::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut attempt = 0;
    let (tmp, mut file) = loop {
        let tmp = dir.join(format!(".{name}.{:016x}.pawl-tmp", random_suffix()));
        match create_new(&tmp) {
            Ok(f) => break (tmp, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 8 => attempt += 1,
            Err(e) => return Err(anyhow::Error::new(e).context(format!("creating {}", tmp.display()))),
        }
    };
    let written = (|| -> anyhow::Result<()> {
        std::io::Write::write_all(&mut file, content.as_bytes()).with_context(|| format!("writing {}", tmp.display()))?;
        file.sync_all()?;
        if expect.is_some() {
            copy_permissions(path, &tmp)?;
        }
        ensure_unchanged(path, expect)?;
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
    })();
    drop(file);
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests;
