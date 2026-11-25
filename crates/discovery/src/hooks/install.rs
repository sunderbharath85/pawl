//! Hook installation: merge `<hub_exe> hook <source>` entries into agent configs.
//!
//! Formats (checked 2026-11-23):
//! - Claude Code `settings.json`: `{"hooks": {"<Event>": [{"matcher"?: "...", "hooks": [{"type":
//!   "command", "command": "...", "args"?: [...], "timeout": s}]}]}}`.
//! - Codex `config.toml`: `notify = ["prog", "args"...]` (JSON appended as last argv) plus
//!   Claude-compatible lifecycle hooks, inline under `[hooks]` or in `$CODEX_HOME/hooks.json`.
//! - Copilot (CLI `~/.copilot/hooks/*.json`, repo `.github/hooks/*.json`, also read by VS Code):
//!   `{"version": 1, "hooks": {"<event>": [{"type": "command", "bash": "...", "powershell":
//!   "...", "env": {...}, "timeoutSec": s}]}}`. PascalCase event names select the snake_case
//!   "VS Code compatible" payloads that carry `hook_event_name`.
//!
//! Our entries are recognised by containing the hub executable name plus `hook <source>`, so
//! re-running replaces them in place and every other hook is left untouched. Removing takes out
//! only our entries (and the `pawl.json` files we own once nothing else is left in them).

use super::{HookTarget, HOOK_EVENT_ENV};
use anyhow::{bail, Context};
use pawl_protocol::HookState;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, InlineTable, Item, TableLike, Value as TVal};

/// Install (or update) our hook entries, or remove them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookChange {
    Install,
    Remove,
}

/// What [`change_with_home`] did (or, without `apply`, would do).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookOutcome {
    /// Human-readable plan: one section per file with a unified diff, then notes.
    pub text: String,
    /// Some file differs (preview) or was written (apply).
    pub changed: bool,
}

/// A target's hook state, see [`status_with_home`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookCheck {
    pub state: HookState,
    /// The agent config file the hooks live in.
    pub config_path: PathBuf,
    pub detail: Option<String>,
}

/// Environment overrides honoured by the agents themselves.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstallEnv {
    /// `CLAUDE_CONFIG_DIR` (default `~/.claude`)
    pub claude_config_dir: Option<PathBuf>,
    /// `CODEX_HOME` (default `~/.codex`)
    pub codex_home: Option<PathBuf>,
    /// `COPILOT_HOME` (default `~/.copilot`)
    pub copilot_home: Option<PathBuf>,
}

impl InstallEnv {
    pub fn from_system() -> Self {
        let get = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        InstallEnv { claude_config_dir: get("CLAUDE_CONFIG_DIR"), codex_home: get("CODEX_HOME"), copilot_home: get("COPILOT_HOME") }
    }
}

const CLAUDE_EVENTS: &[(&str, u32)] = &[
    ("SessionStart", 5),
    ("UserPromptSubmit", 5),
    ("PreToolUse", 5),
    ("PermissionRequest", 5),
    ("PostToolUse", 5),
    ("PostToolUseFailure", 5),
    ("Notification", 5),
    ("Stop", 5),
    ("StopFailure", 5),
    ("SubagentStop", 5),
    // SessionEnd hooks share a 1.5s budget unless a hook sets a longer timeout.
    ("SessionEnd", 3),
];

/// Codex lifecycle hooks. `Stop` is only added when our `notify` is not installed (both would
/// report the same turn end). SessionEnd allows at most 3s.
const CODEX_EVENTS: &[(&str, u32)] = &[
    ("SessionStart", 5),
    ("UserPromptSubmit", 5),
    ("PreToolUse", 5),
    ("PermissionRequest", 5),
    ("PostToolUse", 5),
    ("SessionEnd", 3),
];

const COPILOT_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "SubagentStop",
    "ErrorOccurred",
    "SessionEnd",
    // camelCase: the notification payload always carries `hook_event_name`.
    "notification",
];

// ---------- hub command rendering ----------

/// Hub binary name. A hook entry whose command mentions it counts as ours even when the hub
/// running `install-hooks` now has another file name (e.g. a renamed dev copy).
const HUB_BIN_NAME: &str = "pawl";

struct HubCmd {
    exe: PathBuf,
    stem: String,
}

impl HubCmd {
    /// Does this (lowercased, `/`-separated) command text invoke a hub executable?
    fn names_hub(&self, text: &str) -> bool {
        text.contains(&self.stem) || text.contains(HUB_BIN_NAME)
    }

    fn new(hub_exe: &Path) -> anyhow::Result<Self> {
        let exe = std::path::absolute(hub_exe).with_context(|| format!("resolving {}", hub_exe.display()))?;
        let stem = exe
            .file_stem()
            .map(|s| s.to_string_lossy().to_lowercase())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("hub executable path has no file name: {}", exe.display()))?;
        Ok(HubCmd { exe, stem })
    }

    fn native(&self) -> String {
        self.exe.to_string_lossy().into_owned()
    }

    /// Forward slashes: valid for Windows programs and in bash, PowerShell and cmd.
    fn portable(&self) -> String {
        self.native().replace('\\', "/")
    }

    /// Can be used unquoted in sh, Git Bash, PowerShell and cmd.
    fn shell_safe(&self) -> bool {
        let p = self.portable();
        !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || "/:._-+".contains(c))
    }

    fn plain(&self, source: &str) -> String {
        format!("{} hook {source}", self.portable())
    }

    fn posix(&self, source: &str) -> String {
        format!("'{}' hook {source}", self.portable().replace('\'', "'\\''"))
    }

    fn powershell(&self, source: &str) -> String {
        format!("& '{}' hook {source}", self.native().replace('\'', "''"))
    }

    /// Is this hook handler (any agent's format) one of ours for `source`?
    fn is_ours(&self, handler: &Value, source: &str) -> bool {
        let mut text = String::new();
        for k in ["command", "exec", "bash", "powershell", "command_windows", "commandWindows"] {
            if let Some(v) = handler.get(k).and_then(Value::as_str) {
                text.push(' ');
                text.push_str(v);
            }
        }
        if let Some(Value::Array(args)) = handler.get("args") {
            for a in args.iter().filter_map(Value::as_str) {
                text.push(' ');
                text.push_str(a);
            }
        }
        let text = text.replace('\\', "/").to_lowercase();
        self.names_hub(&text) && text.contains(&format!(" hook {source}"))
    }
}

// ---------- JSON merging ----------

fn parse_json_doc(path: &Path, old: Option<&str>) -> anyhow::Result<Value> {
    match old.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(json!({})),
        Some(text) => {
            let v: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
                .with_context(|| format!("{} is not valid JSON; fix it or add the hooks by hand", path.display()))?;
            if !v.is_object() {
                bail!("{} does not contain a JSON object", path.display());
            }
            Ok(v)
        }
    }
}

fn hooks_object<'a>(doc: &'a mut Value, path: &Path) -> anyhow::Result<&'a mut Map<String, Value>> {
    let obj = doc.as_object_mut().expect("checked object");
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    hooks.as_object_mut().ok_or_else(|| anyhow::anyhow!("`hooks` in {} is not an object", path.display()))
}

/// Merge into a Claude-style `{Event: [{matcher?, hooks: [handler]}]}` map (`grouped`) or a
/// Copilot-style `{event: [handler]}` map. Ours are replaced in place, others kept verbatim.
fn merge_hook_map(
    hooks: &mut Map<String, Value>,
    desired: &[(String, Value)],
    grouped: bool,
    is_ours: &dyn Fn(&Value) -> bool,
    path: &Path,
) -> anyhow::Result<()> {
    // Strip stale entries of ours from events we no longer install.
    let keys: Vec<String> = hooks.keys().cloned().collect();
    for k in keys {
        if desired.iter().any(|(e, _)| *e == k) {
            continue;
        }
        if let Some(Value::Array(arr)) = hooks.get_mut(&k) {
            if remove_ours(arr, grouped, is_ours) && arr.is_empty() {
                hooks.remove(&k);
            }
        }
    }
    for (event, handler) in desired {
        let arr = hooks.entry(event.clone()).or_insert_with(|| json!([]));
        let Value::Array(arr) = arr else {
            bail!("`hooks.{event}` in {} is not an array", path.display());
        };
        let mut placed = false;
        if grouped {
            let mut emptied = Vec::new();
            for (gi, group) in arr.iter_mut().enumerate() {
                let Some(Value::Array(hs)) = group.get_mut("hooks") else { continue };
                let before = hs.len();
                let mut next = Vec::with_capacity(before);
                for h in hs.drain(..) {
                    if is_ours(&h) {
                        if !placed {
                            next.push(handler.clone());
                            placed = true;
                        }
                    } else {
                        next.push(h);
                    }
                }
                *hs = next;
                if hs.is_empty() && before > 0 {
                    emptied.push(gi);
                }
            }
            for gi in emptied.into_iter().rev() {
                arr.remove(gi);
            }
            if !placed {
                arr.push(json!({ "hooks": [handler.clone()] }));
            }
        } else {
            let mut next = Vec::with_capacity(arr.len() + 1);
            for h in arr.drain(..) {
                if is_ours(&h) {
                    if !placed {
                        next.push(handler.clone());
                        placed = true;
                    }
                } else {
                    next.push(h);
                }
            }
            if !placed {
                next.push(handler.clone());
            }
            *arr = next;
        }
    }
    Ok(())
}

/// Remove our handlers from one event array; returns true if anything changed.
fn remove_ours(arr: &mut Vec<Value>, grouped: bool, is_ours: &dyn Fn(&Value) -> bool) -> bool {
    let before = serde_json::to_string(arr).unwrap_or_default();
    if grouped {
        arr.retain_mut(|group| match group.get_mut("hooks") {
            Some(Value::Array(hs)) => {
                let n = hs.len();
                hs.retain(|h| !is_ours(h));
                !(hs.is_empty() && n > 0)
            }
            _ => true,
        });
    } else {
        arr.retain(|h| !is_ours(h));
    }
    serde_json::to_string(arr).unwrap_or_default() != before
}

fn to_pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).expect("serializable");
    s.push('\n');
    s
}

/// Remove every handler of ours from a `hooks` map (all events); drops events and a `hooks`
/// object we emptied. Returns true if anything changed.
fn strip_json_hooks(doc: &mut Value, grouped: bool, is_ours: &dyn Fn(&Value) -> bool) -> bool {
    let Some(hooks) = doc.get_mut("hooks").and_then(Value::as_object_mut) else { return false };
    let mut changed = false;
    let keys: Vec<String> = hooks.keys().cloned().collect();
    for k in keys {
        if let Some(Value::Array(arr)) = hooks.get_mut(&k) {
            if remove_ours(arr, grouped, is_ours) {
                changed = true;
                if arr.is_empty() {
                    hooks.remove(&k);
                }
            }
        }
    }
    if changed && hooks.is_empty() {
        if let Some(obj) = doc.as_object_mut() {
            obj.remove("hooks");
        }
    }
    changed
}

/// A JSON file change: the new text only when the content changed (formatting alone never
/// counts, so an up-to-date file is never rewritten).
fn json_change(label: &'static str, path: PathBuf, old: Option<String>, doc: &Value) -> anyhow::Result<FileChange> {
    let before = match old.as_deref() {
        Some(text) => Some(parse_json_doc(&path, Some(text))?),
        None => None,
    };
    let changed = before.as_ref() != Some(doc);
    let new = if changed { Some(keep_newlines(old.as_deref(), to_pretty(doc))) } else { old.clone() };
    Ok(FileChange { label, path, old, new, changed, confine: None })
}

// ---------- TOML (Codex config.toml) ----------

fn tval_to_json(v: &TVal) -> Value {
    match v {
        TVal::String(s) => json!(s.value()),
        TVal::Integer(i) => json!(*i.value()),
        TVal::Float(f) => json!(*f.value()),
        TVal::Boolean(b) => json!(*b.value()),
        TVal::Datetime(d) => json!(d.value().to_string()),
        TVal::Array(a) => Value::Array(a.iter().map(tval_to_json).collect()),
        TVal::InlineTable(t) => table_like_to_json(t),
    }
}

pub(crate) fn item_to_json(item: &Item) -> Value {
    match item {
        Item::None => Value::Null,
        Item::Value(v) => tval_to_json(v),
        Item::Table(t) => table_like_to_json(t),
        Item::ArrayOfTables(a) => Value::Array(a.iter().map(|t| table_like_to_json(t)).collect()),
    }
}

fn table_like_to_json(t: &dyn TableLike) -> Value {
    Value::Object(t.iter().map(|(k, i)| (k.to_string(), item_to_json(i))).collect())
}

fn json_to_tval(v: &Value) -> TVal {
    match v {
        Value::String(s) => TVal::from(s.as_str()),
        Value::Bool(b) => TVal::from(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => TVal::from(i),
            None => TVal::from(n.as_f64().unwrap_or(0.0)),
        },
        Value::Array(a) => {
            let mut arr = toml_edit::Array::new();
            for x in a {
                arr.push(json_to_tval(x));
            }
            TVal::Array(arr)
        }
        Value::Object(m) => {
            let mut t = InlineTable::new();
            for (k, x) in m {
                t.insert(k, json_to_tval(x));
            }
            TVal::InlineTable(t)
        }
        Value::Null => TVal::from(""),
    }
}

/// Remove our handlers from a TOML hook group (`hooks = [...]` or `[[...hooks]]`).
fn toml_group_remove(group: &mut dyn TableLike, is_ours: &dyn Fn(&Value) -> bool) -> (bool, bool) {
    match group.get_mut("hooks") {
        Some(Item::ArrayOfTables(hs)) => {
            let n = hs.len();
            hs.retain(|t| !is_ours(&table_like_to_json(t)));
            (hs.len() != n, hs.is_empty())
        }
        Some(Item::Value(TVal::Array(hs))) => {
            let n = hs.len();
            hs.retain(|v| !is_ours(&tval_to_json(v)));
            (hs.len() != n, hs.is_empty())
        }
        _ => (false, false),
    }
}

/// Remove ours from one `hooks.<Event>` item; drops groups we emptied. Returns changed.
fn toml_event_remove(item: &mut Item, is_ours: &dyn Fn(&Value) -> bool) -> bool {
    let mut changed = false;
    match item {
        Item::ArrayOfTables(groups) => {
            let mut drop = Vec::new();
            for (i, g) in groups.iter_mut().enumerate() {
                let (c, empty) = toml_group_remove(g, is_ours);
                changed |= c;
                if c && empty {
                    drop.push(i);
                }
            }
            for i in drop.into_iter().rev() {
                groups.remove(i);
            }
        }
        Item::Value(TVal::Array(groups)) => {
            let mut drop = Vec::new();
            for (i, g) in groups.iter_mut().enumerate() {
                if let Some(t) = g.as_inline_table_mut() {
                    let (c, empty) = toml_group_remove(t, is_ours);
                    changed |= c;
                    if c && empty {
                        drop.push(i);
                    }
                }
            }
            for i in drop.into_iter().rev() {
                groups.remove(i);
            }
        }
        _ => {}
    }
    changed
}

fn toml_item_is_empty(item: &Item) -> bool {
    match item {
        Item::ArrayOfTables(a) => a.is_empty(),
        Item::Value(TVal::Array(a)) => a.is_empty(),
        _ => false,
    }
}

/// Merge Codex lifecycle hooks inline into `[hooks]` of a config.toml document.
fn merge_codex_toml_hooks(doc: &mut DocumentMut, desired: &[(String, Value)], is_ours: &dyn Fn(&Value) -> bool) -> anyhow::Result<()> {
    let root = doc.as_table_mut();
    if desired.is_empty() && !root.contains_key("hooks") {
        return Ok(());
    }
    let hooks_item = root.entry("hooks").or_insert_with(|| Item::Table(toml_edit::Table::new()));
    let Some(hooks) = hooks_item.as_table_like_mut() else {
        bail!("`hooks` in config.toml is not a table");
    };
    // Already exactly installed? (avoid churn on re-runs)
    let current: Value = table_like_to_json(&*hooks);
    let up_to_date = desired.iter().all(|(event, handler)| {
        let ours: Vec<Value> = current
            .get(event)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|g| g.get("hooks").and_then(Value::as_array))
            .flatten()
            .filter(|h| is_ours(h))
            .cloned()
            .collect();
        ours.len() == 1 && ours[0] == *handler
    }) && current.as_object().is_some_and(|m| {
        m.iter().all(|(event, groups)| {
            desired.iter().any(|(e, _)| e == event)
                || !groups.as_array().into_iter().flatten().filter_map(|g| g.get("hooks").and_then(Value::as_array)).flatten().any(is_ours)
        })
    });
    if up_to_date {
        return Ok(());
    }
    let keys: Vec<String> = hooks.iter().map(|(k, _)| k.to_string()).collect();
    for k in &keys {
        if let Some(item) = hooks.get_mut(k) {
            if toml_event_remove(item, is_ours) && toml_item_is_empty(item) && !desired.iter().any(|(e, _)| e == k) {
                hooks.remove(k);
            }
        }
    }
    for (event, handler) in desired {
        let group = json!({ "hooks": [handler] });
        match hooks.get_mut(event) {
            Some(Item::ArrayOfTables(groups)) => {
                let mut t = toml_edit::Table::new();
                if let TVal::InlineTable(it) = json_to_tval(&group) {
                    for (k, v) in it.iter() {
                        t.insert(k, Item::Value(v.clone()));
                    }
                }
                groups.push(t);
            }
            Some(Item::Value(TVal::Array(groups))) => groups.push(json_to_tval(&group)),
            Some(_) => bail!("`hooks.{event}` in config.toml is neither an array nor an array of tables"),
            None => {
                let mut arr = toml_edit::Array::new();
                arr.push(json_to_tval(&group));
                hooks.insert(event, Item::Value(TVal::Array(arr)));
            }
        }
    }
    if hooks.is_empty() {
        doc.as_table_mut().remove("hooks");
    }
    Ok(())
}

fn notify_is_ours(item: &Item, cmd: &HubCmd) -> bool {
    let v = item_to_json(item);
    let Some(arr) = v.as_array() else { return false };
    let parts: Vec<String> = arr.iter().filter_map(Value::as_str).map(|s| s.replace('\\', "/").to_lowercase()).collect();
    let joined = format!(" {}", parts.join(" "));
    cmd.names_hub(&joined) && joined.contains(" hook codex")
}

// ---------- plans ----------

struct FileChange {
    label: &'static str,
    path: PathBuf,
    old: Option<String>,
    /// None = delete the file (only files we own, after removing our hooks left nothing else).
    new: Option<String>,
    /// The content differs from `old` (a file that only needs reformatting is unchanged).
    changed: bool,
    /// Repo-scoped targets: the repository. Nothing between it and `path` may be a symlink.
    confine: Option<PathBuf>,
}

impl FileChange {
    fn text(label: &'static str, path: PathBuf, old: Option<String>, new: String) -> Self {
        let changed = old.as_deref() != Some(new.as_str());
        FileChange { label, path, old, new: Some(new), changed, confine: None }
    }
}

type Plan = (Vec<FileChange>, Vec<String>);

pub(crate) fn read_opt(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(String::from_utf8(b).with_context(|| format!("{} is not UTF-8", path.display()))?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn claude_plan(cmd: &HubCmd, path: PathBuf) -> anyhow::Result<Plan> {
    let old = read_opt(&path)?;
    let mut doc = parse_json_doc(&path, old.as_deref())?;
    let mut notes = Vec::new();
    let desired: Vec<(String, Value)> = CLAUDE_EVENTS
        .iter()
        .map(|(event, timeout)| {
            let handler = if cmd.shell_safe() {
                json!({ "type": "command", "command": cmd.plain("claude"), "timeout": timeout })
            } else {
                // Exec form: no shell quoting issues (Claude Code 2.1 hooks `args`).
                json!({ "type": "command", "command": cmd.native(), "args": ["hook", "claude"], "timeout": timeout })
            };
            (event.to_string(), handler)
        })
        .collect();
    if !cmd.shell_safe() {
        notes.push("hub path contains spaces/special characters: using exec-form hooks (`args`), which need a current Claude Code release".to_string());
    }
    let p = path.clone();
    let hooks = hooks_object(&mut doc, &path)?;
    merge_hook_map(hooks, &desired, true, &|h| cmd.is_ours(h, "claude"), &p)?;
    notes.push("Claude Code reloads hooks on the next session; check them with /hooks".to_string());
    Ok((vec![json_change("Claude Code settings", path, old, &doc)?], notes))
}

fn claude_remove_plan(cmd: &HubCmd, path: PathBuf) -> anyhow::Result<Plan> {
    let Some(old) = read_opt(&path)? else { return Ok((Vec::new(), Vec::new())) };
    let mut doc = parse_json_doc(&path, Some(&old))?;
    strip_json_hooks(&mut doc, true, &|h| cmd.is_ours(h, "claude"));
    let notes = vec!["Claude Code stops calling the hub from its next session".to_string()];
    Ok((vec![json_change("Claude Code settings", path, Some(old), &doc)?], notes))
}

fn codex_plan(cmd: &HubCmd, codex_home: PathBuf) -> anyhow::Result<Plan> {
    let cfg_path = codex_home.join("config.toml");
    let old = read_opt(&cfg_path)?;
    let mut doc: DocumentMut = old
        .as_deref()
        .unwrap_or("")
        .parse()
        .with_context(|| format!("{} is not valid TOML; fix it or add the hooks by hand", cfg_path.display()))?;
    let mut notes = Vec::new();

    let notify_value = {
        let mut a = toml_edit::Array::new();
        a.push(cmd.native());
        a.push("hook");
        a.push("codex");
        a
    };
    let notify_ours = match doc.get("notify") {
        None => {
            doc.insert("notify", Item::Value(TVal::Array(notify_value)));
            true
        }
        Some(item) if notify_is_ours(item, cmd) => {
            if item_to_json(item) != tval_to_json(&TVal::Array(notify_value.clone())) {
                doc.insert("notify", Item::Value(TVal::Array(notify_value)));
            }
            true
        }
        Some(_) => {
            notes.push("kept your existing `notify` program (Codex allows only one); turn-complete events come from a Codex `Stop` hook instead".to_string());
            false
        }
    };

    let mut desired: Vec<(String, Value)> = Vec::new();
    if cmd.shell_safe() {
        let mut events: Vec<(&str, u32)> = CODEX_EVENTS.to_vec();
        if !notify_ours {
            events.insert(5, ("Stop", 5));
        }
        desired = events
            .into_iter()
            .map(|(e, t)| (e.to_string(), json!({ "type": "command", "command": cmd.plain("codex"), "timeout": t })))
            .collect();
        notes.push("Codex runs new lifecycle hooks only after you review and trust them: open /hooks in Codex".to_string());
    } else {
        notes.push(format!(
            "hub path `{}` needs shell quoting, which differs between Codex's shells; only `notify` was configured",
            cmd.native()
        ));
    }

    let mut changes = Vec::new();
    let hooks_json = codex_home.join("hooks.json");
    let hooks_json_old = read_opt(&hooks_json)?;
    let is_ours = |h: &Value| cmd.is_ours(h, "codex");
    if let Some(text) = &hooks_json_old {
        // The user already keeps hooks in hooks.json: stay in that format (Codex warns when a
        // layer mixes hooks.json and inline [hooks]), and clean ours out of config.toml.
        let mut hdoc = parse_json_doc(&hooks_json, Some(text))?;
        let p = hooks_json.clone();
        let hooks = hooks_object(&mut hdoc, &hooks_json)?;
        merge_hook_map(hooks, &desired, true, &is_ours, &p)?;
        merge_codex_toml_hooks(&mut doc, &[], &is_ours)?;
        changes.push(json_change("Codex hooks.json", hooks_json, hooks_json_old.clone(), &hdoc)?);
    } else {
        merge_codex_toml_hooks(&mut doc, &desired, &is_ours)?;
    }
    let new = keep_newlines(old.as_deref(), doc.to_string());
    changes.insert(0, FileChange::text("Codex config.toml", cfg_path, old, new));
    Ok((changes, notes))
}

fn codex_remove_plan(cmd: &HubCmd, codex_home: PathBuf) -> anyhow::Result<Plan> {
    let mut changes = Vec::new();
    let cfg_path = codex_home.join("config.toml");
    let is_ours = |h: &Value| cmd.is_ours(h, "codex");
    if let Some(old) = read_opt(&cfg_path)? {
        let mut doc: DocumentMut = old
            .parse()
            .with_context(|| format!("{} is not valid TOML; fix it or remove the hooks by hand", cfg_path.display()))?;
        if doc.get("notify").is_some_and(|item| notify_is_ours(item, cmd)) {
            doc.remove("notify");
        }
        merge_codex_toml_hooks(&mut doc, &[], &is_ours)?;
        let new = keep_newlines(Some(&old), doc.to_string());
        changes.push(FileChange::text("Codex config.toml", cfg_path, Some(old), new));
    }
    let hooks_json = codex_home.join("hooks.json");
    if let Some(old) = read_opt(&hooks_json)? {
        let mut hdoc = parse_json_doc(&hooks_json, Some(&old))?;
        strip_json_hooks(&mut hdoc, true, &is_ours);
        changes.push(json_change("Codex hooks.json", hooks_json, Some(old), &hdoc)?);
    }
    Ok((changes, vec!["Codex stops calling the hub from its next session".to_string()]))
}

fn copilot_plan(cmd: &HubCmd, path: PathBuf, label: &'static str, repo_scoped: bool) -> anyhow::Result<Plan> {
    let old = read_opt(&path)?;
    let mut doc = parse_json_doc(&path, old.as_deref())?;
    let obj = doc.as_object_mut().expect("object");
    if !obj.contains_key("version") {
        // Keep `version` first in a new file.
        let rest = std::mem::take(obj);
        obj.insert("version".to_string(), json!(1));
        obj.extend(rest);
    }
    let desired: Vec<(String, Value)> = COPILOT_EVENTS
        .iter()
        .map(|event| {
            let handler = json!({
                "type": "command",
                "bash": cmd.posix("copilot"),
                "powershell": cmd.powershell("copilot"),
                "env": { HOOK_EVENT_ENV: event },
                "timeoutSec": 5
            });
            (event.to_string(), handler)
        })
        .collect();
    let p = path.clone();
    let hooks = hooks_object(&mut doc, &path)?;
    merge_hook_map(hooks, &desired, false, &|h| cmd.is_ours(h, "copilot"), &p)?;
    let mut notes = vec!["Copilot loads hook files when a session starts".to_string()];
    if repo_scoped {
        notes.push("this file lives in the repository and contains this machine's hub path: add it to .gitignore unless your team shares the same path".to_string());
        notes.push("Copilot CLI also reads .github/hooks; with both copilot-cli and copilot-vscode installed, CLI sessions in this repo report twice".to_string());
    }
    Ok((vec![json_change(label, path, old, &doc)?], notes))
}

/// Our `pawl.json` loses our handlers; once nothing but `version` is left, the file goes.
fn copilot_remove_plan(cmd: &HubCmd, path: PathBuf, label: &'static str) -> anyhow::Result<Plan> {
    let Some(old) = read_opt(&path)? else { return Ok((Vec::new(), Vec::new())) };
    let mut doc = parse_json_doc(&path, Some(&old))?;
    let changed = strip_json_hooks(&mut doc, false, &|h| cmd.is_ours(h, "copilot"));
    let only_version = doc.as_object().is_some_and(|o| o.keys().all(|k| k == "version"));
    let change = if changed && only_version {
        FileChange { label, path, old: Some(old), new: None, changed: true, confine: None }
    } else {
        json_change(label, path, Some(old), &doc)?
    };
    Ok((vec![change], vec!["Copilot stops calling the hub from its next session".to_string()]))
}

// ---------- apply / render ----------

/// `YYYYMMDD-HHMMSS` (UTC) for backup names.
pub(crate) fn utc_stamp(unix_secs: i64) -> String {
    let days = unix_secs.div_euclid(86_400);
    let secs = unix_secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", secs / 3600, (secs % 3600) / 60, secs % 60)
}

pub(crate) fn backup_path(path: &Path, stamp: &str) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "config".into());
    let mut candidate = path.with_file_name(format!("{name}.{stamp}.bak"));
    let mut n = 1;
    // symlink_metadata: a dangling link counts as taken too.
    while std::fs::symlink_metadata(&candidate).is_ok() {
        candidate = path.with_file_name(format!("{name}.{stamp}-{n}.bak"));
        n += 1;
    }
    candidate
}

/// Refuse when `path`, or any folder between it and `root`, is a symbolic link (or a Windows
/// junction). Missing parts are fine (they will be created as plain folders).
pub(crate) fn refuse_symlinks_below(root: &Path, path: &Path) -> anyhow::Result<()> {
    let mut cur = Some(path);
    while let Some(p) = cur {
        if p == root || !p.starts_with(root) {
            break;
        }
        if std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
            bail!("{} is a symbolic link; Pawl does not write hooks through links inside a repository", p.display());
        }
        cur = p.parent();
    }
    Ok(())
}

/// A temp-file suffix nobody can predict (per-process random keys of the std hasher + the clock).
pub(crate) fn random_suffix() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.write_u32(std::process::id());
    h.finish()
}

/// Create `path` exclusively (O_EXCL: never through an existing file or link).
/// Private to the user from the start on Unix (0600): a backup or temp copy of a config file may
/// hold secrets, and a write that fails halfway must not leave a readable copy. Callers give it the
/// source's permissions once it is complete.
pub(crate) fn create_new(path: &Path) -> std::io::Result<std::fs::File> {
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

/// How many of Pawl's backups of one config file are kept (each may hold secrets).
pub(crate) const KEEP_BACKUPS: usize = 3;

/// Keep the `keep` newest of Pawl's own backups of `path` (`<name>.<YYYYMMDD-HHMMSS>[-<n>].bak`, the
/// names [`backup_path`] makes) and delete the older ones. Other files are never touched. Returns
/// how many went.
pub(crate) fn prune_backups(path: &Path, keep: usize) -> usize {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().map(|n| n.to_string_lossy().into_owned())) else { return 0 };
    let Ok(entries) = std::fs::read_dir(if dir.as_os_str().is_empty() { Path::new(".") } else { dir }) else { return 0 };
    let prefix = format!("{name}.");
    let mut ours: Vec<((String, u32), PathBuf)> = Vec::new();
    for e in entries.flatten() {
        let file = e.file_name().to_string_lossy().into_owned();
        let Some(mid) = file.strip_prefix(&prefix).and_then(|r| r.strip_suffix(".bak")) else { continue };
        let (stamp, n) = match mid.get(15..) {
            Some("") => (mid, 0),
            Some(rest) => match rest.strip_prefix('-').and_then(|n| n.parse::<u32>().ok()) {
                Some(n) => (&mid[..15], n),
                None => continue,
            },
            None => continue,
        };
        let b = stamp.as_bytes();
        let stamp_ok = b.len() == 15 && b[8] == b'-' && b.iter().enumerate().all(|(i, c)| i == 8 || c.is_ascii_digit());
        if stamp_ok && e.file_type().is_ok_and(|t| t.is_file()) {
            ours.push(((stamp.to_string(), n), e.path()));
        }
    }
    ours.sort();
    let old = ours.len().saturating_sub(keep);
    ours.into_iter().take(old).filter(|(_, p)| std::fs::remove_file(p).is_ok()).count()
}

/// `new` (rendered with `\n`) in the line endings `old` uses: CRLF files stay CRLF.
pub(crate) fn keep_newlines(old: Option<&str>, new: String) -> String {
    if old.is_some_and(|o| o.contains("\r\n")) {
        new.replace("\r\n", "\n").replace('\n', "\r\n")
    } else {
        new
    }
}

/// Unix: give `dst` the permission bits of `src` (a settings file stays as private as it was).
/// Windows: the new file keeps the folder's inherited ACL.
pub(crate) fn copy_permissions(src: &Path, dst: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    if let Ok(m) = std::fs::metadata(src) {
        std::fs::set_permissions(dst, m.permissions()).with_context(|| format!("setting permissions on {}", dst.display()))?;
    }
    #[cfg(not(unix))]
    let _ = (src, dst);
    Ok(())
}

/// Back `src` up to `dst`, created exclusively with `src`'s permissions.
pub(crate) fn backup_copy(src: &Path, dst: &Path) -> anyhow::Result<()> {
    let data = std::fs::read(src).with_context(|| format!("reading {}", src.display()))?;
    let mut f = create_new(dst).with_context(|| format!("creating {}", dst.display()))?;
    std::io::Write::write_all(&mut f, &data)?;
    f.sync_all()?;
    drop(f);
    copy_permissions(src, dst)
}

/// Replace `path` atomically: a randomly named temp file next to it, created exclusively, with the
/// original's permissions, then renamed over it. `confine`: the repository nothing under may be a
/// link (checked before the folders are created and again before the rename).
fn write_atomic(path: &Path, content: &str, confine: Option<&Path>) -> anyhow::Result<()> {
    if let Some(root) = confine {
        refuse_symlinks_below(root, path)?;
    }
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
        copy_permissions(path, &tmp)?;
        if let Some(root) = confine {
            refuse_symlinks_below(root, path)?;
        }
        std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
    })();
    drop(file);
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

fn render_diff(c: &FileChange) -> String {
    let old = c.old.as_deref().unwrap_or("");
    let new = c.new.as_deref().unwrap_or("");
    let diff = similar::TextDiff::from_lines(old, new);
    let path = c.path.display().to_string();
    let a = if c.old.is_some() { path.as_str() } else { "/dev/null" };
    let b = if c.new.is_some() { path.as_str() } else { "/dev/null" };
    diff.unified_diff().context_radius(3).header(a, b).to_string()
}

fn run(changes: Vec<FileChange>, notes: Vec<String>, apply: bool, change: HookChange, title: &str) -> anyhow::Result<HookOutcome> {
    let mut out = String::new();
    let verb = match change {
        HookChange::Install => "Pawl hooks for",
        HookChange::Remove => "Removing Pawl hooks for",
    };
    out.push_str(&format!("{verb} {title}{}\n", if apply { "" } else { " (dry run: nothing written)" }));
    let stamp = utc_stamp(pawl_protocol::now_ms() / 1000);
    let mut any_change = false;
    if changes.is_empty() {
        out.push_str("\nno hook files found; nothing to change\n");
    }
    for c in &changes {
        out.push_str(&format!("\n== {}: {} ==\n", c.label, c.path.display()));
        if !c.changed {
            out.push_str(match change {
                HookChange::Install => "already up to date; nothing to change\n",
                HookChange::Remove => "no Pawl hooks here; nothing to change\n",
            });
            continue;
        }
        any_change = true;
        if apply {
            let backup = match &c.old {
                Some(_) => {
                    let bak = backup_path(&c.path, &stamp);
                    if let Some(root) = c.confine.as_deref() {
                        refuse_symlinks_below(root, &c.path)?;
                    }
                    backup_copy(&c.path, &bak).with_context(|| format!("backing up {} to {}", c.path.display(), bak.display()))?;
                    prune_backups(&c.path, KEEP_BACKUPS);
                    Some(bak)
                }
                None => None,
            };
            match (&c.new, backup) {
                (Some(new), Some(bak)) => {
                    write_atomic(&c.path, new, c.confine.as_deref())?;
                    out.push_str(&format!("updated (backup: {})\n", bak.display()));
                }
                (Some(new), None) => {
                    write_atomic(&c.path, new, c.confine.as_deref())?;
                    out.push_str("created\n");
                }
                (None, bak) => {
                    std::fs::remove_file(&c.path).with_context(|| format!("deleting {}", c.path.display()))?;
                    match bak {
                        Some(b) => out.push_str(&format!("deleted (backup: {})\n", b.display())),
                        None => out.push_str("deleted\n"),
                    }
                }
            }
        }
        out.push_str(&render_diff(c));
    }
    if any_change && !notes.is_empty() {
        out.push_str("\nNotes:\n");
        for n in &notes {
            out.push_str(&format!("- {n}\n"));
        }
    }
    Ok(HookOutcome { text: out, changed: any_change })
}

/// The agent config file a target's hooks live in (Codex: config.toml, even when the lifecycle
/// hooks sit in hooks.json next to it).
pub fn config_path_with_home(target: &HookTarget, home: &Path, env: &InstallEnv) -> PathBuf {
    match target {
        HookTarget::Claude => env.claude_config_dir.clone().unwrap_or_else(|| home.join(".claude")).join("settings.json"),
        HookTarget::Codex => env.codex_home.clone().unwrap_or_else(|| home.join(".codex")).join("config.toml"),
        HookTarget::CopilotCli => {
            env.copilot_home.clone().unwrap_or_else(|| home.join(".copilot")).join("hooks").join("pawl.json")
        }
        HookTarget::CopilotVscode { repo } => repo.join(".github").join("hooks").join("pawl.json"),
    }
}

fn plan(target: &HookTarget, cmd: &HubCmd, change: HookChange, home: &Path, env: &InstallEnv) -> anyhow::Result<(Plan, String)> {
    let path = config_path_with_home(target, home, env);
    Ok(match (target, change) {
        (HookTarget::Claude, HookChange::Install) => (claude_plan(cmd, path)?, "Claude Code".to_string()),
        (HookTarget::Claude, HookChange::Remove) => (claude_remove_plan(cmd, path)?, "Claude Code".to_string()),
        (HookTarget::Codex, c) => {
            let dir = env.codex_home.clone().unwrap_or_else(|| home.join(".codex"));
            let p = if c == HookChange::Install { codex_plan(cmd, dir)? } else { codex_remove_plan(cmd, dir)? };
            (p, "Codex".to_string())
        }
        (HookTarget::CopilotCli, HookChange::Install) => {
            (copilot_plan(cmd, path, "Copilot CLI hooks", false)?, "GitHub Copilot CLI".to_string())
        }
        (HookTarget::CopilotCli, HookChange::Remove) => {
            (copilot_remove_plan(cmd, path, "Copilot CLI hooks")?, "GitHub Copilot CLI".to_string())
        }
        (HookTarget::CopilotVscode { repo }, c) => {
            if !repo.is_dir() {
                bail!("repository folder {} does not exist", repo.display());
            }
            // A repository is not trusted: refuse before reading (a linked .github could make us
            // show, back up or overwrite a file outside it).
            refuse_symlinks_below(repo, &path)?;
            let label = "Copilot agent hooks (VS Code)";
            let (mut changes, notes) =
                if c == HookChange::Install { copilot_plan(cmd, path, label, true)? } else { copilot_remove_plan(cmd, path, label)? };
            for ch in &mut changes {
                ch.confine = Some(repo.clone());
            }
            ((changes, notes), format!("Copilot in VS Code ({})", repo.display()))
        }
    })
}

/// Install or remove our hooks with an explicit home directory and env overrides (tests use temp
/// dirs). `apply = false` writes nothing and returns the exact change as a diff; `apply = true`
/// backs up every touched file first.
pub fn change_with_home(
    target: &HookTarget,
    hub_exe: &Path,
    change: HookChange,
    apply: bool,
    home: &Path,
    env: &InstallEnv,
) -> anyhow::Result<HookOutcome> {
    let cmd = HubCmd::new(hub_exe)?;
    let ((changes, notes), title) = plan(target, &cmd, change, home, env)?;
    run(changes, notes, apply, change, &title)
}

/// [`super::install`] with an explicit home directory and env overrides (tests use temp dirs).
pub fn install_with_home(target: HookTarget, hub_exe: &Path, apply: bool, home: &Path, env: &InstallEnv) -> anyhow::Result<String> {
    change_with_home(&target, hub_exe, HookChange::Install, apply, home, env).map(|o| o.text)
}

/// Is `target` hooked up to this hub? Reads the agent's config only (never writes):
/// - installed: installing would change nothing;
/// - outdated: some of our entries are there, but installing would change them (another hub
///   path, missing events);
/// - missing: none of our entries are there;
/// - unsupported: the config cannot be read or merged (invalid JSON/TOML, missing repo folder),
///   or nothing can be installed for this setup; `detail` says why.
pub fn status_with_home(target: &HookTarget, hub_exe: &Path, home: &Path, env: &InstallEnv) -> HookCheck {
    let config_path = config_path_with_home(target, home, env);
    let checked = HubCmd::new(hub_exe).and_then(|cmd| {
        let ((install, notes), _) = plan(target, &cmd, HookChange::Install, home, env)?;
        let ((remove, _), _) = plan(target, &cmd, HookChange::Remove, home, env)?;
        Ok((install.iter().any(|c| c.changed), remove.iter().any(|c| c.changed), notes))
    });
    let (state, detail) = match checked {
        Err(e) => (HookState::Unsupported, Some(format!("{e:#}"))),
        Ok((false, true, _)) => (HookState::Installed, None),
        Ok((true, true, _)) => {
            (HookState::Outdated, Some("some Pawl hooks are missing or point at another pawl executable".to_string()))
        }
        Ok((true, false, _)) => (HookState::Missing, None),
        Ok((false, false, notes)) => (HookState::Unsupported, Some(notes.join("; ")).filter(|s| !s.is_empty())),
    };
    HookCheck { state, config_path, detail }
}

#[cfg(test)]
pub(crate) mod test_support {
    pub fn utc_stamp(secs: i64) -> String {
        super::utc_stamp(secs)
    }
}
