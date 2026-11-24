//! Permission rules engine.
//!
//! Invariants (PLAN.md "Bot behavior"):
//! - Safe reads are auto-approved by default **only inside the session cwd**: the default
//!   `read` and `search` rules use the path `{cwd}/**`. Path-less reads, reads with any
//!   location outside the cwd, and any read when the cwd is unknown ask.
//! - Sensitive paths (`.env*`, keys/certs, `id_rsa*`, `.ssh/**`, `.aws/**`, `.npmrc`,
//!   `*credential*`, the hub's own token, ...) are guarded in code: a request that touches
//!   one is never auto-approved, whatever the rules say (a deny rule still denies).
//! - Security hard floor ([`krypto_protocol::NEVER_AUTO_ALLOW`]): a request whose tool kind is
//!   not auto-allowable (`execute`, `delete`, `fetch`, `other`, or missing) is **never**
//!   auto-approved, whatever the rules say (Deny still wins). [`validate_rules`] rejects
//!   AutoAllow rules without a `toolKind` or with a floor kind; [`default_rules`] passes it.
//!   If such invalid rules reach evaluation anyway (e.g. from a hand-edited config) they
//!   are skipped, so the next rule decides (a later Deny applies) and otherwise Ask. As a
//!   second layer, any AutoAllow outcome for a non-allowable request kind becomes Ask.
//! - Commands, deletes and network access (plus edits, moves, mode switches and unknown
//!   tools) always ask by default. [`default_rules`] never auto-approves them.
//! - A path rule that auto-approves needs **every** location path to match its glob (and
//!   at least one path). Ask/deny path rules apply when **any** location path matches.
//! - First applicable rule wins; no applicable rule means [`RuleAction::Ask`].
//!
//! Rule paths are `globset` globs with `*` not crossing `/` (use `**` for that). A path may
//! start with the token `{cwd}`, which expands to the session cwd (normalized and
//! glob-escaped); without a cwd such a rule never matches. Request paths are
//! slash-normalized, `.`/`..` are resolved lexically, and relative paths are joined onto the
//! cwd. Matching is case-insensitive on Windows and macOS.

use std::fmt;
use std::sync::OnceLock;

use globset::{GlobBuilder, GlobMatcher, GlobSet, GlobSetBuilder};
use krypto_protocol::{
    is_auto_allowable, PermissionOptionKind, PermissionRequest, Rule, RuleAction, RuleMatch, ToolKind,
    NEVER_AUTO_ALLOW,
};
use serde_json::Value;

/// Token a rule path may start with; expands to the normalized session cwd.
pub const CWD_TOKEN: &str = "{cwd}";
/// Files (and the secret directories themselves) that hold secrets.
pub const SENSITIVE_FILES_GLOB: &str = "**/{.env*,*.env,*.pem,*.key,*.p12,*.pfx,*.jks,*.keystore,*.kdbx,id_rsa*,id_dsa*,id_ecdsa*,id_ed25519*,.npmrc,.pypirc,.netrc,_netrc,.git-credentials,.pgpass,*credential*,*secret*,.ssh,.aws,.azure,.gnupg,.kube}";
/// Contents of directories that hold secrets.
pub const SENSITIVE_DIRS_GLOB: &str = "**/{.ssh,.aws,.azure,.gnupg,.kube,.docker,gcloud,secrets}/**";
/// The hub's own token/config/db: an agent that reads the token could approve itself.
pub const HUB_SECRETS_GLOB: &str = "**/krypto/{token,config.toml,hub.db,hub.db-*}";

const CASE_INSENSITIVE: bool = cfg!(any(windows, target_os = "macos"));

/// Keys in a tool call's `rawInput` that carry file paths.
const PATH_KEYS: &[&str] = &[
    "path",
    "file_path",
    "filePath",
    "filepath",
    "notebook_path",
    "notebookPath",
    "abs_path",
    "absPath",
    "absolute_path",
    "absolutePath",
    "old_path",
    "oldPath",
    "new_path",
    "newPath",
    "source_path",
    "sourcePath",
    "destination_path",
    "destinationPath",
    "target_path",
    "targetPath",
    "dir",
    "directory",
    "cwd",
];
const PATH_LIST_KEYS: &[&str] = &["paths", "files", "file_paths", "filePaths"];

fn rule(id: &str, tool_kind: Option<ToolKind>, path: Option<&str>, action: RuleAction) -> Rule {
    Rule {
        id: id.to_string(),
        matcher: RuleMatch { tool_kind, path: path.map(str::to_string), agent: None },
        action,
    }
}

/// Safe defaults: auto-allow read/search inside the session cwd (`{cwd}/**`) and think;
/// ask for edit/move/delete/execute/fetch/switch_mode/other. Sensitive paths are guarded
/// in code (see [`is_sensitive_path`]), not by rules.
pub fn default_rules() -> Vec<Rule> {
    use RuleAction::{Ask, AutoAllow};
    vec![
        rule("default-read", Some(ToolKind::Read), Some("{cwd}/**"), AutoAllow),
        rule("default-search", Some(ToolKind::Search), Some("{cwd}/**"), AutoAllow),
        rule("default-think", Some(ToolKind::Think), None, AutoAllow),
        rule("default-edit", Some(ToolKind::Edit), None, Ask),
        rule("default-move", Some(ToolKind::Move), None, Ask),
        rule("default-delete", Some(ToolKind::Delete), None, Ask),
        rule("default-execute", Some(ToolKind::Execute), None, Ask),
        rule("default-fetch", Some(ToolKind::Fetch), None, Ask),
        rule("default-switch-mode", Some(ToolKind::SwitchMode), None, Ask),
        rule("default-other", Some(ToolKind::Other), None, Ask),
    ]
}

/// Why a rule set was rejected by [`validate_rules`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleError {
    /// An AutoAllow rule without `match.toolKind` (it would cover every kind).
    AutoAllowWithoutToolKind { rule_id: String },
    /// An AutoAllow rule for a kind in [`NEVER_AUTO_ALLOW`].
    AutoAllowNotPermitted { rule_id: String, kind: ToolKind },
}

impl RuleError {
    pub fn rule_id(&self) -> &str {
        match self {
            RuleError::AutoAllowWithoutToolKind { rule_id } | RuleError::AutoAllowNotPermitted { rule_id, .. } => {
                rule_id
            }
        }
    }
}

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleError::AutoAllowWithoutToolKind { rule_id } => {
                write!(f, "rule \"{rule_id}\": auto_allow rules must name an explicit toolKind")
            }
            RuleError::AutoAllowNotPermitted { rule_id, kind } => write!(
                f,
                "rule \"{rule_id}\": toolKind \"{}\" can never be auto-allowed (security floor: execute, delete, fetch, other)",
                kind_name(*kind)
            ),
        }
    }
}

impl std::error::Error for RuleError {}

fn kind_name(kind: ToolKind) -> &'static str {
    match kind {
        ToolKind::Read => "read",
        ToolKind::Edit => "edit",
        ToolKind::Delete => "delete",
        ToolKind::Move => "move",
        ToolKind::Search => "search",
        ToolKind::Execute => "execute",
        ToolKind::Think => "think",
        ToolKind::Fetch => "fetch",
        ToolKind::SwitchMode => "switch_mode",
        ToolKind::Other => "other",
    }
}

/// Reject any AutoAllow rule whose `match.toolKind` is missing or in [`NEVER_AUTO_ALLOW`].
/// Returns the first offending rule. Ask/Deny rules are always valid.
pub fn validate_rules(rules: &[Rule]) -> Result<(), RuleError> {
    for r in rules.iter().filter(|r| r.action == RuleAction::AutoAllow) {
        match r.matcher.tool_kind {
            None => return Err(RuleError::AutoAllowWithoutToolKind { rule_id: r.id.clone() }),
            Some(kind) if NEVER_AUTO_ALLOW.contains(&kind) => {
                return Err(RuleError::AutoAllowNotPermitted { rule_id: r.id.clone(), kind })
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// Risky kinds: the [`NEVER_AUTO_ALLOW`] floor (never auto-approved) plus `switch_mode`
/// (only auto-approved by a rule that names it explicitly).
pub fn requires_explicit_kind(kind: ToolKind) -> bool {
    matches!(
        kind,
        ToolKind::Delete | ToolKind::Execute | ToolKind::Fetch | ToolKind::SwitchMode | ToolKind::Other
    )
}

/// Destructive kinds per PLAN.md (commands, deletes, network).
pub fn is_destructive(kind: ToolKind) -> bool {
    matches!(kind, ToolKind::Delete | ToolKind::Execute | ToolKind::Fetch)
}

/// Tool kind used for matching; a missing kind is treated as `other` (unknown).
pub fn effective_kind(req: &PermissionRequest) -> ToolKind {
    req.tool_call.kind.unwrap_or(ToolKind::Other)
}

/// Outcome of a rule evaluation, for audit logs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub action: RuleAction,
    /// The rule that matched; `None` when nothing matched (Ask).
    pub rule_id: Option<String>,
    /// True when the sensitive-path guard turned an AutoAllow into Ask.
    pub sensitive: bool,
    /// True when the security floor turned an AutoAllow into Ask (kind not auto-allowable).
    pub floor: bool,
}

/// [`evaluate_in`] without a session cwd: `{cwd}` rules never match, so default reads ask.
pub fn evaluate(rules: &[Rule], req: &PermissionRequest) -> RuleAction {
    evaluate_in(rules, req, None)
}

/// First matching rule wins; no match -> Ask. Never AutoAllow a kind outside
/// [`is_auto_allowable`] (execute/delete/fetch/other/missing), whatever the rules say, and
/// never AutoAllow a request that touches a sensitive path. Deny still wins.
/// `cwd` is the session's working directory.
pub fn evaluate_in(rules: &[Rule], req: &PermissionRequest, cwd: Option<&str>) -> RuleAction {
    decide(rules, req, cwd).action
}

/// Full decision: action, deciding rule id and which guard (if any) downgraded it.
pub fn decide(rules: &[Rule], req: &PermissionRequest, cwd: Option<&str>) -> Decision {
    let cwd = normalized_cwd(cwd);
    let paths = paths_in(req, cwd.as_deref());
    let (rule, skipped_unsafe) = find_rule(rules, req, &paths, cwd.as_deref());
    let action = rule.map(|r| r.action).unwrap_or(RuleAction::Ask);
    let rule_id = rule.map(|r| r.id.clone());
    if action != RuleAction::AutoAllow {
        return Decision { action, rule_id, sensitive: false, floor: skipped_unsafe };
    }
    let floor = !is_auto_allowable(req.tool_call.kind);
    let sensitive = paths.iter().any(|p| sensitive_match(p));
    if floor || sensitive {
        return Decision { action: RuleAction::Ask, rule_id, sensitive, floor };
    }
    Decision { action, rule_id, sensitive: false, floor: false }
}

/// The rule that decides `req` with no cwd, or `None` when it falls back to Ask.
pub fn matching_rule<'a>(rules: &'a [Rule], req: &PermissionRequest) -> Option<&'a Rule> {
    matching_rule_in(rules, req, None)
}

/// The rule that matches `req` in `cwd` (before the sensitive-path guard).
pub fn matching_rule_in<'a>(rules: &'a [Rule], req: &PermissionRequest, cwd: Option<&str>) -> Option<&'a Rule> {
    let cwd = normalized_cwd(cwd);
    let paths = paths_in(req, cwd.as_deref());
    find_rule(rules, req, &paths, cwd.as_deref()).0
}

/// True if `path` names a secret (env files, keys, credentials, `.ssh`/`.aws`, hub token).
pub fn is_sensitive_path(path: &str) -> bool {
    let p = normalize_path(path);
    !p.is_empty() && sensitive_match(&p)
}

/// Matches an already-normalized path. If the guard globs failed to build (never expected),
/// everything counts as sensitive so nothing is auto-approved.
fn sensitive_match(normalized: &str) -> bool {
    static SET: OnceLock<Option<GlobSet>> = OnceLock::new();
    let set = SET.get_or_init(|| {
        let mut b = GlobSetBuilder::new();
        for pat in [SENSITIVE_FILES_GLOB, SENSITIVE_DIRS_GLOB, HUB_SECRETS_GLOB] {
            b.add(builder(pat).build().ok()?);
        }
        b.build().ok()
    });
    set.as_ref().is_none_or(|g| g.is_match(normalized))
}

fn normalized_cwd(cwd: Option<&str>) -> Option<String> {
    cwd.map(normalize_path).filter(|c| !c.is_empty() && is_absolute(c))
}

fn is_absolute(p: &str) -> bool {
    p.starts_with('/') || p.split('/').next().is_some_and(is_drive)
}

/// Request paths, with relative ones joined onto the cwd.
fn paths_in(req: &PermissionRequest, cwd: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in request_paths(req) {
        let p = match cwd {
            Some(c) if !is_absolute(&p) => normalize_path(&format!("{}/{p}", c.trim_end_matches('/'))),
            _ => p,
        };
        if !out.contains(&p) {
            out.push(p);
        }
    }
    out
}

/// True for an AutoAllow rule that [`validate_rules`] would reject.
fn is_unsafe_auto_allow(rule: &Rule) -> bool {
    rule.action == RuleAction::AutoAllow && !is_auto_allowable(rule.matcher.tool_kind)
}

/// First applicable rule, plus whether an unsafe AutoAllow rule matched (and was skipped)
/// before it.
fn find_rule<'a>(
    rules: &'a [Rule],
    req: &PermissionRequest,
    paths: &[String],
    cwd: Option<&str>,
) -> (Option<&'a Rule>, bool) {
    let kind = effective_kind(req);
    let mut skipped = false;
    for r in rules {
        if !rule_applies(r, req, kind, paths, cwd) {
            continue;
        }
        if is_unsafe_auto_allow(r) {
            skipped = true;
            continue;
        }
        return (Some(r), skipped);
    }
    (None, skipped)
}

fn rule_applies(rule: &Rule, req: &PermissionRequest, kind: ToolKind, paths: &[String], cwd: Option<&str>) -> bool {
    let m = &rule.matcher;
    if let Some(k) = m.tool_kind {
        if k != kind {
            return false;
        }
    }
    if let Some(agent) = m.agent.as_deref().map(str::trim).filter(|a| !a.is_empty() && *a != "*") {
        if agent != req.agent_id {
            return false;
        }
    }
    if let Some(pat) = m.path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        let Some(glob) = expand_pattern(pat, cwd).and_then(|p| compile(&p)) else {
            return false;
        };
        if paths.is_empty() {
            return false;
        }
        let ok = if rule.action == RuleAction::AutoAllow {
            paths.iter().all(|p| glob.is_match(p))
        } else {
            paths.iter().any(|p| glob.is_match(p))
        };
        if !ok {
            return false;
        }
    }
    true
}

/// Escape glob metacharacters so a literal path matches only itself.
fn escape_glob(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if "*?[]{}".contains(c) {
            out.push('[');
            out.push(c);
            out.push(']');
        } else {
            out.push(c);
        }
    }
    out
}

/// Slash-normalize a rule path and expand a leading `{cwd}` (case-insensitive).
/// `None` when the rule needs a cwd we don't have.
fn expand_pattern(pattern: &str, cwd: Option<&str>) -> Option<String> {
    let pattern = pattern.replace('\\', "/");
    let has_token = pattern.get(..CWD_TOKEN.len()).is_some_and(|p| p.eq_ignore_ascii_case(CWD_TOKEN));
    if !has_token {
        return Some(pattern);
    }
    let base = escape_glob(cwd?.trim_end_matches('/'));
    Some(format!("{base}{}", &pattern[CWD_TOKEN.len()..]))
}

fn builder(pattern: &str) -> GlobBuilder<'_> {
    let mut b = GlobBuilder::new(pattern);
    b.literal_separator(true).backslash_escape(false).case_insensitive(CASE_INSENSITIVE).empty_alternates(true);
    b
}

fn compile(pattern: &str) -> Option<GlobMatcher> {
    match builder(pattern).build() {
        Ok(g) => Some(g.compile_matcher()),
        Err(e) => {
            tracing::warn!(pattern = %pattern, error = %e, "invalid rule glob; rule ignored");
            None
        }
    }
}

/// Every path a permission request touches (tool call locations plus well-known
/// `rawInput` path keys), normalized with [`normalize_path`] and de-duplicated.
/// Relative paths stay relative here; [`evaluate_in`] joins them onto the cwd.
pub fn request_paths(req: &PermissionRequest) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |p: &str| {
        let n = normalize_path(p);
        if !n.is_empty() && !out.contains(&n) {
            out.push(n);
        }
    };
    for loc in req.tool_call.locations.iter().flatten() {
        push(&loc.path);
    }
    if let Some(Value::Object(map)) = &req.tool_call.raw_input {
        for k in PATH_KEYS {
            if let Some(Value::String(s)) = map.get(*k) {
                push(s);
            }
        }
        for k in PATH_LIST_KEYS {
            if let Some(Value::Array(items)) = map.get(*k) {
                for s in items.iter().filter_map(Value::as_str) {
                    push(s);
                }
            }
        }
    }
    out
}

fn is_drive(seg: &str) -> bool {
    let b = seg.as_bytes();
    b.len() == 2 && b[1] == b':' && b[0].is_ascii_alphabetic()
}

/// Forward slashes, no `\\?\` prefix, lexical `.`/`..` resolution (so `repo/src/../../x`
/// cannot sneak past a `repo/**` glob), no duplicate or trailing slashes.
pub fn normalize_path(p: &str) -> String {
    let mut s = p.trim().replace('\\', "/");
    if let Some(rest) = s.strip_prefix("//?/") {
        s = rest.to_string();
    } else if let Some(rest) = s.strip_prefix("file://") {
        s = rest.to_string();
    }
    if s.is_empty() {
        return s;
    }
    let unc = s.starts_with("//");
    let absolute = s.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for seg in s.split('/') {
        match seg {
            "" | "." => {}
            ".." => match parts.last() {
                Some(&last) if last != ".." && !is_drive(last) => {
                    parts.pop();
                }
                Some(&last) if is_drive(last) => {}
                _ if absolute => {}
                _ => parts.push(".."),
            },
            other => parts.push(other),
        }
    }
    let body = parts.join("/");
    if unc {
        format!("//{body}")
    } else if absolute {
        format!("/{body}")
    } else {
        body
    }
}

/// Pick the option for an auto decision: AutoAllow -> first allow_once (else
/// allow_always); Deny -> reject_once (None = send a cancelled outcome); Ask -> None.
pub fn pick_option(req: &PermissionRequest, action: RuleAction) -> Option<String> {
    let find = |kind: PermissionOptionKind| {
        req.options.iter().find(|o| o.kind == kind).map(|o| o.option_id.clone())
    };
    match action {
        RuleAction::AutoAllow => {
            find(PermissionOptionKind::AllowOnce).or_else(|| find(PermissionOptionKind::AllowAlways))
        }
        RuleAction::Deny => find(PermissionOptionKind::RejectOnce),
        RuleAction::Ask => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krypto_protocol::{PermissionOption, ToolCallPayload, ToolLocation};
    use serde_json::json;

    const ALL_KINDS: [ToolKind; 10] = [
        ToolKind::Read,
        ToolKind::Edit,
        ToolKind::Delete,
        ToolKind::Move,
        ToolKind::Search,
        ToolKind::Execute,
        ToolKind::Think,
        ToolKind::Fetch,
        ToolKind::SwitchMode,
        ToolKind::Other,
    ];
    const CWD: Option<&str> = Some("/work/repo");

    fn opt(id: &str, kind: PermissionOptionKind) -> PermissionOption {
        PermissionOption { option_id: id.into(), name: id.into(), kind }
    }

    fn req(kind: Option<ToolKind>, paths: &[&str]) -> PermissionRequest {
        PermissionRequest {
            request_id: "r1".into(),
            machine_id: "abcdef012345".into(),
            session_id: "s1".into(),
            agent_id: "codex".into(),
            tool_call: ToolCallPayload {
                tool_call_id: "t1".into(),
                title: Some("tool".into()),
                kind,
                status: None,
                locations: if paths.is_empty() {
                    None
                } else {
                    Some(paths.iter().map(|p| ToolLocation { path: (*p).into(), line: None }).collect())
                },
                raw_input: None,
            },
            options: vec![
                opt("allow", PermissionOptionKind::AllowOnce),
                opt("always", PermissionOptionKind::AllowAlways),
                opt("reject", PermissionOptionKind::RejectOnce),
                opt("never", PermissionOptionKind::RejectAlways),
            ],
            created_at: 0,
        }
    }

    fn custom(kind: Option<ToolKind>, path: Option<&str>, agent: Option<&str>, action: RuleAction) -> Rule {
        Rule {
            id: "custom".into(),
            matcher: RuleMatch { tool_kind: kind, path: path.map(Into::into), agent: agent.map(Into::into) },
            action,
        }
    }

    fn read_in(cwd: Option<&str>, paths: &[&str]) -> RuleAction {
        evaluate_in(&default_rules(), &req(Some(ToolKind::Read), paths), cwd)
    }

    #[test]
    fn defaults_auto_allow_only_safe_reads_inside_cwd() {
        let rules = default_rules();
        for kind in ALL_KINDS {
            let got = evaluate_in(&rules, &req(Some(kind), &["/work/repo/src/main.rs"]), CWD);
            let want = if matches!(kind, ToolKind::Read | ToolKind::Search | ToolKind::Think) {
                RuleAction::AutoAllow
            } else {
                RuleAction::Ask
            };
            assert_eq!(got, want, "kind {kind:?}");
        }
        assert_eq!(evaluate_in(&rules, &req(Some(ToolKind::Think), &[]), CWD), RuleAction::AutoAllow);
        assert_eq!(evaluate_in(&rules, &req(None, &[]), CWD), RuleAction::Ask);
    }

    #[test]
    fn reads_outside_cwd_pathless_or_without_cwd_ask() {
        assert_eq!(read_in(CWD, &["/work/repo/src/lib.rs", "/work/repo/README.md"]), RuleAction::AutoAllow);
        // Path-less read.
        assert_eq!(read_in(CWD, &[]), RuleAction::Ask);
        // Outside the cwd, or any one location outside.
        assert_eq!(read_in(CWD, &["/etc/hosts"]), RuleAction::Ask);
        assert_eq!(read_in(CWD, &["/work/repo/a.rs", "/work/other/b.rs"]), RuleAction::Ask);
        // Sibling directory sharing the prefix.
        assert_eq!(read_in(CWD, &["/work/repo-evil/a.rs"]), RuleAction::Ask);
        // The cwd itself is not "inside" it.
        assert_eq!(read_in(CWD, &["/work/repo"]), RuleAction::Ask);
        // `..` traversal out of the cwd.
        assert_eq!(read_in(CWD, &["/work/repo/src/../../other/x"]), RuleAction::Ask);
        // Relative paths are joined onto the cwd.
        assert_eq!(read_in(CWD, &["src/main.rs"]), RuleAction::AutoAllow);
        assert_eq!(read_in(CWD, &["../other/x.rs"]), RuleAction::Ask);
        // No cwd: every read asks, and evaluate() == evaluate_in(None).
        assert_eq!(read_in(None, &["/work/repo/a.rs"]), RuleAction::Ask);
        assert_eq!(read_in(Some(""), &["/work/repo/a.rs"]), RuleAction::Ask);
        assert_eq!(read_in(Some("relative/dir"), &["relative/dir/a.rs"]), RuleAction::Ask);
        assert_eq!(evaluate(&default_rules(), &req(Some(ToolKind::Read), &["/work/repo/a.rs"])), RuleAction::Ask);
        // Search follows the same scope.
        let rules = default_rules();
        assert_eq!(evaluate_in(&rules, &req(Some(ToolKind::Search), &["/work/repo/src"]), CWD), RuleAction::AutoAllow);
        assert_eq!(evaluate_in(&rules, &req(Some(ToolKind::Search), &["/home/me"]), CWD), RuleAction::Ask);
        assert_eq!(evaluate_in(&rules, &req(Some(ToolKind::Search), &[]), CWD), RuleAction::Ask);
    }

    #[test]
    fn cwd_token_is_normalized_and_escaped() {
        // Backslashes, trailing slashes and `\\?\` prefixes on either side.
        assert_eq!(read_in(Some("E:\\work\\repo\\"), &["E:/work/repo/src/a.rs"]), RuleAction::AutoAllow);
        assert_eq!(read_in(Some("E:/work/repo"), &["\\\\?\\E:\\work\\repo\\a.rs"]), RuleAction::AutoAllow);
        assert_eq!(read_in(Some("E:\\work\\repo"), &["E:\\work\\repo2\\a.rs"]), RuleAction::Ask);
        // Case-insensitive where the filesystem is.
        let mixed = read_in(Some("E:\\Work\\Repo"), &["e:\\work\\repo\\a.rs"]);
        if cfg!(any(windows, target_os = "macos")) {
            assert_eq!(mixed, RuleAction::AutoAllow);
        } else {
            assert_eq!(mixed, RuleAction::Ask);
        }
        // Glob metacharacters in the cwd are literal.
        assert_eq!(read_in(Some("/w/[x]{y}*"), &["/w/[x]{y}*/a.rs"]), RuleAction::AutoAllow);
        assert_eq!(read_in(Some("/w/[x]{y}*"), &["/w/x/a.rs"]), RuleAction::Ask);
        assert_eq!(read_in(Some("/w/a?c"), &["/w/abc/a.rs"]), RuleAction::Ask);
        // Root cwd.
        assert_eq!(read_in(Some("/"), &["/srv/a.rs"]), RuleAction::AutoAllow);
        // Token is case-insensitive; without a cwd it never matches a literal "cwd" dir.
        let rules = vec![custom(Some(ToolKind::Edit), Some("{CWD}/src/**"), None, RuleAction::AutoAllow)];
        assert_eq!(evaluate_in(&rules, &req(Some(ToolKind::Edit), &["/work/repo/src/a.rs"]), CWD), RuleAction::AutoAllow);
        assert_eq!(evaluate_in(&rules, &req(Some(ToolKind::Edit), &["/work/repo/docs/a.md"]), CWD), RuleAction::Ask);
        assert_eq!(evaluate_in(&rules, &req(Some(ToolKind::Edit), &["cwd/src/a.rs"]), None), RuleAction::Ask);
        let deny = vec![custom(None, Some("{cwd}/prod/**"), None, RuleAction::Deny)];
        assert_eq!(evaluate_in(&deny, &req(Some(ToolKind::Read), &["/work/repo/prod/x"]), CWD), RuleAction::Deny);
    }

    #[test]
    fn sensitive_guard_wins_over_auto_allow() {
        for p in [
            "/work/repo/.env",
            "/work/repo/.env.local",
            "/work/repo/.envrc",
            "/work/repo/certs/server.pem",
            "/work/repo/keys/signing.key",
            "/work/repo/id_rsa",
            "/work/repo/.ssh/config",
            "/work/repo/.ssh",
            "/work/repo/.aws/credentials",
            "/work/repo/.npmrc",
            "/work/repo/credentials.json",
            "/work/repo/config/secrets/prod.yaml",
            "C:\\Users\\me\\AppData\\Roaming\\krypto\\token",
        ] {
            assert!(is_sensitive_path(p), "{p}");
            let r = req(Some(ToolKind::Read), &[p]);
            assert_eq!(read_in(CWD, &[p]), RuleAction::Ask, "{p}");
            let d = decide(&default_rules(), &r, Some("/"));
            assert_eq!(d.action, RuleAction::Ask, "{p}");
            // A user rule that auto-allows everything still can't touch secrets.
            let open = vec![custom(Some(ToolKind::Read), Some("**"), None, RuleAction::AutoAllow)];
            let d = decide(&open, &r, CWD);
            assert_eq!(
                d,
                Decision { action: RuleAction::Ask, rule_id: Some("custom".into()), sensitive: true, floor: false },
                "{p}"
            );
        }
        // Relative sensitive path joined onto the cwd.
        assert_eq!(read_in(CWD, &[".env"]), RuleAction::Ask);
        // One sensitive location among safe ones.
        assert_eq!(read_in(CWD, &["/work/repo/src/lib.rs", "/work/repo/.env"]), RuleAction::Ask);
        // Sensitive paths in rawInput.
        let mut r = req(Some(ToolKind::Read), &["/work/repo/a.rs"]);
        r.tool_call.raw_input = Some(json!({ "file_path": "/work/repo/.env" }));
        assert_eq!(evaluate_in(&default_rules(), &r, CWD), RuleAction::Ask);
        // Explicit opt-in for commands is guarded too.
        let rules = vec![custom(Some(ToolKind::Execute), Some("{cwd}/**"), None, RuleAction::AutoAllow)];
        assert_eq!(evaluate_in(&rules, &req(Some(ToolKind::Execute), &["/work/repo/.env"]), CWD), RuleAction::Ask);
        // Deny still denies.
        let rules = vec![custom(None, Some("**/.env*"), None, RuleAction::Deny)];
        assert_eq!(evaluate_in(&rules, &req(Some(ToolKind::Read), &["/work/repo/.env"]), CWD), RuleAction::Deny);
        // Ordinary files are not sensitive.
        for p in ["/work/repo/src/main.rs", "/work/repo/docs/environment.md", "/work/repo/keyboard.rs"] {
            assert!(!is_sensitive_path(p), "{p}");
            assert_eq!(read_in(CWD, &[p]), RuleAction::AutoAllow, "{p}");
        }
    }

    #[test]
    fn defaults_never_auto_approve_destructive_or_writes() {
        let rules = default_rules();
        for r in &rules {
            if r.action == RuleAction::AutoAllow {
                let k = r.matcher.tool_kind.expect("default auto-allow rules name a kind");
                assert!(matches!(k, ToolKind::Read | ToolKind::Search | ToolKind::Think), "{k:?}");
            }
        }
        for kind in [ToolKind::Execute, ToolKind::Delete, ToolKind::Fetch, ToolKind::Edit, ToolKind::Move] {
            for paths in [&[][..], &["/work/repo/x"][..], &["/work/repo/a.rs", "/work/repo/b.rs"][..]] {
                for cwd in [None, CWD, Some("/")] {
                    assert_eq!(evaluate_in(&rules, &req(Some(kind), paths), cwd), RuleAction::Ask, "{kind:?} {paths:?}");
                }
            }
        }
    }

    /// Every evaluation entry point, every cwd variant: never AutoAllow.
    fn assert_never_auto_allows(rules: &[Rule], r: &PermissionRequest) {
        assert_ne!(evaluate(rules, r), RuleAction::AutoAllow, "evaluate {:?}", r.tool_call.kind);
        for cwd in [None, CWD, Some("/"), Some("/r")] {
            assert_ne!(evaluate_in(rules, r, cwd), RuleAction::AutoAllow, "evaluate_in {:?} {cwd:?}", r.tool_call.kind);
            assert_ne!(decide(rules, r, cwd).action, RuleAction::AutoAllow, "decide {:?} {cwd:?}", r.tool_call.kind);
        }
    }

    #[test]
    fn floor_kinds_are_never_auto_approved() {
        // Kinds outside the floor per krypto_protocol.
        for kind in ALL_KINDS {
            assert_eq!(is_auto_allowable(Some(kind)), !NEVER_AUTO_ALLOW.contains(&kind), "{kind:?}");
        }
        assert!(!is_auto_allowable(None));
        let paths_sets: [&[&str]; 4] = [&[], &["/r/a"], &["/work/repo/x"], &["relative/x"]];
        for kind in NEVER_AUTO_ALLOW.iter().copied().map(Some).chain([None]) {
            // Explicit user opt-in for that exact kind (any path / {cwd} / agent scope).
            let explicit = vec![
                custom(kind, None, None, RuleAction::AutoAllow),
                custom(kind, Some("**"), None, RuleAction::AutoAllow),
                custom(kind, Some("{cwd}/**"), Some("codex"), RuleAction::AutoAllow),
            ];
            // Kind-less rules, alone and in front of the defaults.
            let kindless = vec![custom(None, None, None, RuleAction::AutoAllow)];
            let mut kindless_first = vec![custom(None, Some("/r/**"), None, RuleAction::AutoAllow)];
            kindless_first.extend(default_rules());
            for paths in paths_sets {
                let r = req(kind, paths);
                assert_never_auto_allows(&explicit, &r);
                assert_never_auto_allows(&kindless, &r);
                assert_never_auto_allows(&kindless_first, &r);
                assert_never_auto_allows(&default_rules(), &r);
            }
        }
    }

    #[test]
    fn user_execute_auto_allow_rule_never_approves() {
        let rule = Rule {
            id: "yolo".into(),
            matcher: RuleMatch { tool_kind: Some(ToolKind::Execute), path: None, agent: None },
            action: RuleAction::AutoAllow,
        };
        // Exactly the JSON a client could send in set_rules.
        let from_wire: Rule =
            serde_json::from_value(json!({"id": "yolo", "match": {"toolKind": "execute"}, "action": "auto_allow"})).unwrap();
        assert_eq!(from_wire, rule);
        let rules = vec![rule];
        let r = req(Some(ToolKind::Execute), &["/work/repo/build.sh"]);
        assert_never_auto_allows(&rules, &r);
        let d = decide(&rules, &r, CWD);
        assert_eq!(d, Decision { action: RuleAction::Ask, rule_id: None, sensitive: false, floor: true });
        // Deny still wins: the unsafe rule is skipped and a later deny applies.
        let rules = vec![
            rules[0].clone(),
            custom(Some(ToolKind::Execute), None, None, RuleAction::Deny),
        ];
        assert_eq!(evaluate_in(&rules, &r, CWD), RuleAction::Deny);
        assert_eq!(decide(&rules, &r, CWD).rule_id.as_deref(), Some("custom"));
        assert!(validate_rules(&rules).is_err());
    }

    #[test]
    fn kindless_auto_allow_rule_never_approves() {
        let rules = vec![custom(None, None, None, RuleAction::AutoAllow)];
        for kind in ALL_KINDS.iter().copied().map(Some).chain([None]) {
            assert_never_auto_allows(&rules, &req(kind, &["/r/a"]));
        }
        // Skipped, so later rules decide: a valid read rule still works, deny still wins.
        let rules = vec![
            custom(None, Some("/r/**"), None, RuleAction::AutoAllow),
            custom(Some(ToolKind::Execute), None, None, RuleAction::Deny),
            custom(Some(ToolKind::Edit), Some("/r/**"), None, RuleAction::AutoAllow),
        ];
        assert_eq!(evaluate(&rules, &req(Some(ToolKind::Execute), &["/r/a"])), RuleAction::Deny);
        assert_eq!(evaluate(&rules, &req(Some(ToolKind::Edit), &["/r/a"])), RuleAction::AutoAllow);
        assert_eq!(evaluate(&rules, &req(Some(ToolKind::Read), &["/r/a"])), RuleAction::Ask);
        assert!(decide(&rules, &req(Some(ToolKind::Read), &["/r/a"]), None).floor);
    }

    #[test]
    fn explicit_allowable_kind_rules_still_work() {
        let rules = vec![custom(Some(ToolKind::Edit), Some("/r/**"), Some("codex"), RuleAction::AutoAllow)];
        assert_eq!(evaluate(&rules, &req(Some(ToolKind::Edit), &["/r/a"])), RuleAction::AutoAllow);
        assert_eq!(evaluate(&rules, &req(Some(ToolKind::Edit), &["/other/a"])), RuleAction::Ask);
        assert_eq!(evaluate(&rules, &req(Some(ToolKind::Delete), &["/r/a"])), RuleAction::Ask);
        let rules = vec![custom(Some(ToolKind::SwitchMode), None, None, RuleAction::AutoAllow)];
        assert_eq!(evaluate(&rules, &req(Some(ToolKind::SwitchMode), &[])), RuleAction::AutoAllow);
        assert!(validate_rules(&rules).is_ok());
    }

    #[test]
    fn validate_rules_rejects_unsafe_auto_allow() {
        assert_eq!(validate_rules(&default_rules()), Ok(()));
        assert_eq!(validate_rules(&[]), Ok(()));
        // Ask/Deny rules of any shape are fine.
        let ok = vec![
            custom(None, None, None, RuleAction::Ask),
            custom(Some(ToolKind::Execute), None, None, RuleAction::Deny),
            custom(None, Some("**/.env"), None, RuleAction::Deny),
            custom(Some(ToolKind::Read), Some("{cwd}/**"), None, RuleAction::AutoAllow),
        ];
        assert_eq!(validate_rules(&ok), Ok(()));
        let mut bad = ok.clone();
        bad.push(Rule {
            id: "kindless".into(),
            matcher: RuleMatch { tool_kind: None, path: Some("/r/**".into()), agent: None },
            action: RuleAction::AutoAllow,
        });
        let err = validate_rules(&bad).unwrap_err();
        assert_eq!(err, RuleError::AutoAllowWithoutToolKind { rule_id: "kindless".into() });
        assert_eq!(err.rule_id(), "kindless");
        assert!(err.to_string().contains("\"kindless\"") && err.to_string().contains("toolKind"), "{err}");
        for kind in NEVER_AUTO_ALLOW {
            let rules = vec![Rule {
                id: format!("allow-{kind:?}"),
                matcher: RuleMatch { tool_kind: Some(*kind), path: None, agent: None },
                action: RuleAction::AutoAllow,
            }];
            let err = validate_rules(&rules).unwrap_err();
            assert_eq!(err, RuleError::AutoAllowNotPermitted { rule_id: format!("allow-{kind:?}"), kind: *kind });
            let msg = err.to_string();
            assert!(msg.contains(&format!("\"allow-{kind:?}\"")) && msg.contains(kind_name(*kind)), "{msg}");
        }
        // First offender is reported.
        let two = vec![
            custom(Some(ToolKind::Fetch), None, None, RuleAction::AutoAllow),
            Rule { id: "second".into(), ..custom(None, None, None, RuleAction::AutoAllow) },
        ];
        assert_eq!(validate_rules(&two).unwrap_err().rule_id(), "custom");
        let boxed: Box<dyn std::error::Error> = Box::new(validate_rules(&two).unwrap_err());
        assert!(boxed.to_string().contains("fetch"));
    }

    #[test]
    fn auto_allow_path_rule_needs_every_location() {
        let rules = vec![custom(Some(ToolKind::Edit), Some("/work/repo/**"), None, RuleAction::AutoAllow)];
        assert_eq!(evaluate(&rules, &req(Some(ToolKind::Edit), &["/work/repo/a.rs"])), RuleAction::AutoAllow);
        assert_eq!(
            evaluate(&rules, &req(Some(ToolKind::Edit), &["/work/repo/a.rs", "/etc/passwd"])),
            RuleAction::Ask
        );
        assert_eq!(evaluate(&rules, &req(Some(ToolKind::Edit), &[])), RuleAction::Ask);
        assert_eq!(
            evaluate(&rules, &req(Some(ToolKind::Edit), &["/work/repo/src/../../other/x"])),
            RuleAction::Ask
        );
        // `*` does not cross directories.
        let rules = vec![custom(Some(ToolKind::Edit), Some("/work/repo/*"), None, RuleAction::AutoAllow)];
        assert_eq!(evaluate(&rules, &req(Some(ToolKind::Edit), &["/work/repo/a.rs"])), RuleAction::AutoAllow);
        assert_eq!(evaluate(&rules, &req(Some(ToolKind::Edit), &["/work/repo/src/a.rs"])), RuleAction::Ask);
    }

    #[test]
    fn windows_paths_are_normalized() {
        let rules = vec![custom(Some(ToolKind::Edit), Some("E:\\work\\repo\\**"), None, RuleAction::AutoAllow)];
        assert_eq!(
            evaluate(&rules, &req(Some(ToolKind::Edit), &["E:\\work\\repo\\src\\main.rs"])),
            RuleAction::AutoAllow
        );
        assert_eq!(
            evaluate(&rules, &req(Some(ToolKind::Edit), &["\\\\?\\E:\\work\\repo\\x.rs"])),
            RuleAction::AutoAllow
        );
        assert_eq!(normalize_path("E:\\a\\..\\..\\b"), "E:/b");
        assert_eq!(normalize_path("/a/./b//c/"), "/a/b/c");
        assert_eq!(normalize_path("../x"), "../x");
    }

    #[test]
    fn deny_path_rule_matches_any_location() {
        let rules = vec![custom(None, Some("**/prod/**"), None, RuleAction::Deny)];
        assert_eq!(
            evaluate(&rules, &req(Some(ToolKind::Read), &["/r/dev/a", "/r/prod/b"])),
            RuleAction::Deny
        );
        assert_eq!(evaluate(&rules, &req(Some(ToolKind::Read), &["/r/dev/a"])), RuleAction::Ask);
    }

    #[test]
    fn first_match_wins_and_agent_filter() {
        let rules = vec![
            custom(Some(ToolKind::Read), None, Some("claude-code"), RuleAction::Deny),
            custom(Some(ToolKind::Read), None, None, RuleAction::AutoAllow),
            custom(Some(ToolKind::Read), None, None, RuleAction::Deny),
        ];
        let mut r = req(Some(ToolKind::Read), &["/a"]);
        assert_eq!(evaluate(&rules, &r), RuleAction::AutoAllow);
        assert_eq!(matching_rule(&rules, &r).map(|x| x.id.as_str()), Some("custom"));
        r.agent_id = "claude-code".into();
        assert_eq!(evaluate(&rules, &r), RuleAction::Deny);
        assert_eq!(evaluate(&[], &r), RuleAction::Ask);
        assert_eq!(
            decide(&[], &r, CWD),
            Decision { action: RuleAction::Ask, rule_id: None, sensitive: false, floor: false }
        );
        assert_eq!(
            matching_rule_in(&default_rules(), &req(Some(ToolKind::Read), &["/work/repo/a"]), CWD).map(|x| x.id.as_str()),
            Some("default-read")
        );
    }

    #[test]
    fn raw_input_paths_are_checked() {
        let rules = default_rules();
        let mut r = req(Some(ToolKind::Read), &[]);
        r.tool_call.raw_input = Some(json!({ "file_path": "/home/me/project/.env" }));
        assert_eq!(request_paths(&r), vec!["/home/me/project/.env".to_string()]);
        assert_eq!(evaluate_in(&rules, &r, Some("/home/me/project")), RuleAction::Ask);
        r.tool_call.raw_input = Some(json!({ "paths": ["/p/a.rs", "/p/b.rs"], "pattern": "foo" }));
        assert_eq!(request_paths(&r).len(), 2);
        assert_eq!(evaluate_in(&rules, &r, Some("/p")), RuleAction::AutoAllow);
        assert_eq!(evaluate_in(&rules, &r, Some("/q")), RuleAction::Ask);
    }

    #[test]
    fn invalid_glob_never_matches() {
        let rules = vec![custom(Some(ToolKind::Read), Some("/r/[unclosed"), None, RuleAction::AutoAllow)];
        assert_eq!(evaluate(&rules, &req(Some(ToolKind::Read), &["/r/x"])), RuleAction::Ask);
    }

    #[test]
    fn pick_option_choices() {
        let r = req(Some(ToolKind::Read), &[]);
        assert_eq!(pick_option(&r, RuleAction::AutoAllow).as_deref(), Some("allow"));
        assert_eq!(pick_option(&r, RuleAction::Deny).as_deref(), Some("reject"));
        assert_eq!(pick_option(&r, RuleAction::Ask), None);
        let mut only_always = r.clone();
        only_always.options.retain(|o| o.kind == PermissionOptionKind::AllowAlways);
        assert_eq!(pick_option(&only_always, RuleAction::AutoAllow).as_deref(), Some("always"));
        assert_eq!(pick_option(&only_always, RuleAction::Deny), None);
    }
}
