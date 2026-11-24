//! Deterministic intent parser. Every [`super::BrainAction`] originates here; LLM output
//! never reaches this code.
//!
//! Recognized (case-insensitive, punctuation tolerant, leading "hey"/pet name stripped):
//! - help: "help", "what can you do"
//! - approve / deny: "approve", "yes", "allow codex", "always allow", "deny 2", "no"
//! - commands: "tell codex to run the tests in repo X", "ask claude to …", "have codex …",
//!   "codex, fix the build", "start a new claude session in ~/code/app to …"
//! - daily report: "daily report", "recap", "what happened today"
//! - status: "what's Claude doing?", "status", "how is codex", "codex?"
//! - everything else is small talk.

use std::collections::{BTreeSet, HashSet};

use krypto_protocol::{Agent, AgentId, Session};

use crate::util::{basename, path_key};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AgentRef {
    /// An agent id from discovery or a session.
    Known(AgentId),
    /// A well-known agent word we have no agent for (display form, e.g. "Gemini").
    Unknown(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pick {
    /// 1-based position in the pending list (oldest first).
    Nth(usize),
    Last,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RepoRef {
    Path(String),
    Known { name: String, cwd: String },
    Unknown { name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandIntent {
    pub agent: AgentRef,
    pub repo: Option<RepoRef>,
    pub task: String,
    /// "start …" / "new session": always start a session instead of prompting one.
    pub force_new: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Intent {
    Help,
    Resolve { approve: bool, always: bool, agent: Option<AgentRef>, pick: Option<Pick> },
    Command(CommandIntent),
    Status { agent: Option<AgentRef> },
    DailyReport,
    SmallTalk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Talk {
    Greeting,
    Praise,
    Thanks,
    Feeling,
    Play,
    Bye,
    Other,
}

// ---------- tokens ----------

#[derive(Debug, Clone)]
pub(crate) struct Tok {
    /// Original text (quotes removed for quoted tokens).
    pub orig: String,
    /// Lowercase, surrounding punctuation trimmed, apostrophes removed ("what's" -> "whats").
    pub plain: String,
    /// Lowercase, punctuation trimmed, possessive "'s" removed (for agent / repo names).
    pub key: String,
    /// Followed by `,` or `:` (addressing: "codex, …").
    pub sep: bool,
    pub quoted: bool,
}

fn make_tok(orig: String, sep: bool, quoted: bool) -> Tok {
    let lower = orig.to_lowercase();
    let norm = lower
        .trim_start_matches(|c: char| "\"'`([{<@*#".contains(c))
        .trim_end_matches(|c: char| "\"'`)]}>.,!?;:*".contains(c))
        .to_string();
    let plain = norm.replace('\'', "");
    let key = norm.strip_suffix("'s").filter(|k| !k.is_empty()).unwrap_or(&norm).to_string();
    Tok { orig, plain, key, sep, quoted }
}

pub(crate) fn tokenize(text: &str) -> Vec<Tok> {
    let text = text.replace(['\u{2019}', '\u{2018}'], "'").replace(['\u{201c}', '\u{201d}'], "\"");
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        if chars[i] == '"' || chars[i] == '`' {
            let q = chars[i];
            if let Some(end) = chars[i + 1..].iter().position(|&c| c == q) {
                let inner: String = chars[i + 1..i + 1 + end].iter().collect();
                i += end + 2;
                let mut sep = false;
                while i < chars.len() && matches!(chars[i], ',' | ':' | ';' | '.' | '!' | '?') {
                    sep |= matches!(chars[i], ',' | ':');
                    i += 1;
                }
                if !inner.trim().is_empty() {
                    out.push(make_tok(inner.trim().to_string(), sep, true));
                }
                continue;
            }
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        let raw: String = chars[start..i].iter().collect();
        let sep = (raw.ends_with(',') || raw.ends_with(':')) && raw.len() > 1;
        out.push(make_tok(raw, sep, false));
    }
    // Drop punctuation-only tokens, keeping their addressing meaning ("codex : run").
    let mut kept: Vec<Tok> = Vec::with_capacity(out.len());
    for t in out {
        if t.plain.is_empty() {
            if t.orig.contains([',', ':']) {
                if let Some(prev) = kept.last_mut() {
                    prev.sep = true;
                }
            }
            continue;
        }
        kept.push(t);
    }
    kept
}

// ---------- word lists ----------

const LEAD_FILLERS: &[&str] = &[
    "hey", "hi", "hello", "yo", "ok", "okay", "please", "pls", "plz", "so", "um", "uh", "well", "pet", "buddy",
    "doggo", "pup", "puppy", "boy", "girl", "kitty", "birdie", "robo", "dear", "and",
];
const TRAIL_FILLERS: &[&str] = &["please", "pls", "plz", "thanks", "thx", "ty", "now", "asap", "buddy", "boy", "girl"];
const APPROVE_VERBS: &[&str] = &["approve", "approved", "allow", "accept", "permit", "grant", "confirm"];
const APPROVE_WHOLE: &[&str] = &[
    "yes", "y", "yep", "yeah", "yup", "ya", "sure", "go ahead", "do it", "lgtm", "ship it", "yes please", "go for it",
    "sounds good", "👍", "✅",
];
const DENY_VERBS: &[&str] = &["deny", "denied", "reject", "decline", "refuse", "disallow", "block", "veto"];
const DENY_WHOLE: &[&str] = &[
    "no", "n", "nope", "nah", "no way", "dont", "do not", "dont do it", "no thanks", "never", "👎", "❌",
];
const ALWAYS: &[&str] = &["always", "forever", "permanently"];
const COMMAND_VERBS: &[&str] = &["tell", "ask", "have", "get", "make", "instruct", "order", "let", "send", "message", "ping"];
const START_VERBS: &[&str] = &["start", "launch", "spin", "open", "kick", "fire", "begin", "create", "run", "boot", "spawn", "new"];
const START_SKIP: &[&str] = &["up", "off", "a", "an", "new", "fresh", "another", "the", "my"];
const DETERMINERS: &[&str] = &["the", "my", "our", "this", "that", "a", "an"];
const AGENT_NOUNS: &[&str] = &["agent", "bot", "session", "instance", "chat", "cli", "up"];
const PREPS: &[&str] = &["in", "on", "inside", "within", "for", "at", "under", "into"];
const PATH_PREPS: &[&str] = &["in", "on", "inside", "within", "at", "under", "into"];
const REPO_WORDS: &[&str] = &[
    "repo", "repos", "repository", "project", "folder", "dir", "directory", "workspace", "codebase", "worktree",
];
const NOT_NAMES: &[&str] = &[
    "it", "there", "here", "this", "that", "them", "me", "you", "order", "general", "parallel", "background",
    "detail", "full", "case", "place", "progress", "the", "a", "an", "all", "one", "time", "now",
];
/// Agent names we recognize even when discovery doesn't list them (for helpful replies).
const KNOWN_AGENT_WORDS: &[&str] = &[
    "claude", "codex", "gemini", "copilot", "goose", "opencode", "cursor", "aider", "qwen", "amp", "kimi", "cline",
    "augment", "junie", "kiro", "crush", "droid",
];
const GENERIC_NAME_WORDS: &[&str] = &[
    "the", "ai", "agent", "cli", "code", "open", "my", "new", "run", "tool", "assistant", "github", "google", "openai",
    "anthropic", "acp",
];
const STATUS_WORDS: &[&str] = &[
    "doing", "status", "progress", "happening", "update", "updates", "sessions", "pending", "waiting", "busy", "news",
    "working", "stuck", "summary", "summarize", "agents", "overview",
];
const STATUS_PHRASES: &[&str] = &["up to", "working on", "going on", "check on", "whats new", "where are we"];
const SMALLTALK_QUESTIONS: &[&str] = &[
    "how are you", "how are u", "how r u", "how you doing", "hows it going", "how is it going", "how do you feel",
    "are you ok", "you ok", "whats up", "sup", "wassup",
];

fn has(words: &[&str], w: &str) -> bool {
    words.contains(&w)
}

fn capitalize(w: &str) -> String {
    let mut c = w.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

// ---------- agent catalog ----------

#[derive(Debug, Clone)]
struct Alias {
    words: Vec<String>,
    agent_id: AgentId,
    rank: (bool, i64),
}

/// Names users can type for each agent (ids, display names, first words), from discovery
/// plus agents only seen through sessions (hooks).
#[derive(Debug, Clone, Default)]
pub(crate) struct AgentCatalog {
    aliases: Vec<Alias>,
}

fn split_words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| c.is_whitespace() || c == '-' || c == '_' || c == '.' || c == '/')
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

fn aliases_for(id: &str, name: Option<&str>) -> BTreeSet<Vec<String>> {
    let mut set: BTreeSet<Vec<String>> = BTreeSet::new();
    let idl = id.trim().to_lowercase();
    if idl.is_empty() {
        return set;
    }
    set.insert(vec![idl.clone()]);
    let mut add_words = |words: Vec<String>| {
        if words.is_empty() {
            return;
        }
        let core: Vec<String> = words.iter().filter(|w| !matches!(w.as_str(), "acp" | "cli" | "agent")).cloned().collect();
        set.insert(words.clone());
        if !core.is_empty() {
            set.insert(core.clone());
            set.insert(vec![core.join("-")]);
            if let Some(first) = core.first() {
                if first.len() >= 3 && !GENERIC_NAME_WORDS.contains(&first.as_str()) {
                    set.insert(vec![first.clone()]);
                }
            }
            if let Some(last) = core.last() {
                if last.len() >= 4 && !GENERIC_NAME_WORDS.contains(&last.as_str()) {
                    set.insert(vec![last.clone()]);
                }
            }
        }
    };
    add_words(split_words(&idl));
    if let Some(n) = name {
        add_words(split_words(n));
    }
    set
}

impl AgentCatalog {
    pub fn build(agents: &[Agent], sessions: &[Session]) -> Self {
        let last_seen = |id: &str| sessions.iter().filter(|s| s.agent_id == id).map(|s| s.last_event_at).max().unwrap_or(0);
        let mut aliases = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for a in agents {
            if seen.insert(a.id.as_str()) {
                let rank = (a.installed, last_seen(&a.id));
                for words in aliases_for(&a.id, Some(&a.name)) {
                    aliases.push(Alias { words, agent_id: a.id.clone(), rank });
                }
            }
        }
        for s in sessions {
            if seen.insert(s.agent_id.as_str()) {
                let rank = (false, last_seen(&s.agent_id));
                for words in aliases_for(&s.agent_id, None) {
                    aliases.push(Alias { words, agent_id: s.agent_id.clone(), rank });
                }
            }
        }
        Self { aliases }
    }

    /// Longest alias starting at token `i` (ties: installed, then most recently seen).
    pub fn match_at(&self, toks: &[Tok], i: usize) -> Option<(AgentId, usize)> {
        self.aliases
            .iter()
            .filter(|a| {
                let n = a.words.len();
                i + n <= toks.len() && a.words.iter().zip(&toks[i..i + n]).all(|(w, t)| *w == t.key)
            })
            .max_by(|a, b| a.words.len().cmp(&b.words.len()).then(a.rank.cmp(&b.rank)))
            .map(|a| (a.agent_id.clone(), a.words.len()))
    }

    /// First agent mention anywhere: (agent id, index, token length).
    pub fn find(&self, toks: &[Tok]) -> Option<(AgentId, usize, usize)> {
        (0..toks.len()).find_map(|i| self.match_at(toks, i).map(|(id, len)| (id, i, len)))
    }
}

fn agent_at(toks: &[Tok], i: usize, cat: &AgentCatalog) -> Option<(AgentRef, usize)> {
    if let Some((id, len)) = cat.match_at(toks, i) {
        return Some((AgentRef::Known(id), len));
    }
    let t = toks.get(i)?;
    KNOWN_AGENT_WORDS.contains(&t.key.as_str()).then(|| (AgentRef::Unknown(capitalize(&t.key)), 1))
}

fn mention_anywhere(toks: &[Tok], cat: &AgentCatalog) -> Option<AgentRef> {
    cat.find(toks).map(|(id, _, _)| AgentRef::Known(id)).or_else(|| {
        toks.iter()
            .find(|t| KNOWN_AGENT_WORDS.contains(&t.key.as_str()))
            .map(|t| AgentRef::Unknown(capitalize(&t.key)))
    })
}

// ---------- repo index ----------

#[derive(Debug, Clone)]
struct RepoEntry {
    base: String,
    alnum: String,
    display: String,
    key: String,
    cwd: String,
    agent_id: String,
    ts: i64,
}

/// Session working directories, matched by basename ("repo X").
#[derive(Debug, Clone, Default)]
pub(crate) struct RepoIndex {
    entries: Vec<RepoEntry>,
}

fn alnum(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

impl RepoIndex {
    pub fn build(sessions: &[Session]) -> Self {
        let entries = sessions
            .iter()
            .filter(|s| !s.cwd.trim().is_empty())
            .map(|s| {
                let display = basename(&s.cwd).to_string();
                let base = display.to_lowercase();
                RepoEntry {
                    alnum: alnum(&base),
                    base,
                    display,
                    key: path_key(&s.cwd),
                    cwd: s.cwd.clone(),
                    agent_id: s.agent_id.clone(),
                    ts: s.last_event_at,
                }
            })
            .collect();
        Self { entries }
    }

    /// Resolve a repo name to a session cwd: exact basename, then alphanumeric-equal, then
    /// (if `fuzzy`) a unique substring match. Prefers `prefer_agent`'s sessions, then recency.
    pub fn resolve(&self, name: &str, prefer_agent: Option<&str>, fuzzy: bool) -> Option<String> {
        let name = name.trim().to_lowercase();
        let na = alnum(&name);
        if na.is_empty() {
            return None;
        }
        let pick = |c: Vec<&RepoEntry>| {
            c.into_iter()
                .max_by_key(|e| (prefer_agent == Some(e.agent_id.as_str()), e.ts))
                .map(|e| e.cwd.clone())
        };
        let exact: Vec<&RepoEntry> = self.entries.iter().filter(|e| e.base == name).collect();
        if !exact.is_empty() {
            return pick(exact);
        }
        let loose: Vec<&RepoEntry> = self.entries.iter().filter(|e| e.alnum == na).collect();
        if !loose.is_empty() {
            return pick(loose);
        }
        if !fuzzy || na.len() < 3 {
            return None;
        }
        let fz: Vec<&RepoEntry> = self
            .entries
            .iter()
            .filter(|e| e.alnum.contains(&na) || (e.alnum.len() >= 3 && na.contains(&e.alnum)))
            .collect();
        let distinct: BTreeSet<&str> = fz.iter().map(|e| e.key.as_str()).collect();
        if distinct.len() == 1 {
            pick(fz)
        } else {
            None
        }
    }

    /// Distinct repo names, most recent first (for hints).
    pub fn names(&self, max: usize) -> Vec<String> {
        let mut e: Vec<&RepoEntry> = self.entries.iter().collect();
        e.sort_by_key(|x| std::cmp::Reverse(x.ts));
        let mut seen = HashSet::new();
        e.into_iter().filter(|x| seen.insert(x.base.clone())).take(max).map(|x| x.display.clone()).collect()
    }
}

fn clean_orig(s: &str) -> String {
    let s = s.trim().trim_matches(|c: char| c == '"' || c == '\'' || c == '`');
    let s = s.trim_end_matches([',', '.', ';', '!', '?']);
    let s = if s.len() > 2 { s.trim_end_matches(':') } else { s };
    s.to_string()
}

fn is_path_like(s: &str) -> bool {
    if s.contains("://") {
        return false;
    }
    let b = s.as_bytes();
    s.contains('/') || s.contains('\\') || s.starts_with('~') || (b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic())
}

fn expand_home(p: &str) -> String {
    let rest = if p == "~" {
        Some("")
    } else {
        p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\"))
    };
    match rest {
        Some(rest) => {
            let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_default();
            if home.is_empty() {
                p.to_string()
            } else if rest.is_empty() {
                home
            } else {
                let sep = if home.contains('\\') { '\\' } else { '/' };
                format!("{}{sep}{}", home.trim_end_matches(['/', '\\']), rest)
            }
        }
        None => p.to_string(),
    }
}

fn classify_repo(prep: Option<&str>, explicit: bool, tok: &Tok, repos: &RepoIndex, prefer: Option<&str>) -> Option<RepoRef> {
    let raw = clean_orig(&tok.orig);
    if raw.is_empty() || NOT_NAMES.contains(&tok.plain.as_str()) || REPO_WORDS.contains(&tok.plain.as_str()) {
        return None;
    }
    if is_path_like(&raw) {
        let path_ok = explicit || prep.is_some_and(|p| PATH_PREPS.contains(&p));
        return path_ok.then(|| RepoRef::Path(expand_home(&raw)));
    }
    match repos.resolve(&tok.key, prefer, explicit) {
        Some(cwd) => Some(RepoRef::Known { name: raw, cwd }),
        None if explicit => Some(RepoRef::Unknown { name: raw }),
        None => None,
    }
}

/// Find "in repo X" / "in the X project" / "at ~/code/x" / "in X" (X a known repo).
/// Returns the last match and its token range.
fn extract_repo(toks: &[&Tok], repos: &RepoIndex, prefer: Option<&str>) -> (Option<RepoRef>, Option<(usize, usize)>) {
    let n = toks.len();
    let w = |i: usize| toks.get(i).map(|t| t.plain.as_str()).unwrap_or("");
    let mut best = (None, None);
    let mut i = 0;
    while i < n {
        let word = w(i);
        let prep = if PREPS.contains(&word) {
            Some(word)
        } else if REPO_WORDS.contains(&word) && i + 1 < n {
            None
        } else {
            i += 1;
            continue;
        };
        let mut j = if prep.is_some() { i + 1 } else { i };
        let mut explicit = false;
        while j < n && DETERMINERS.contains(&w(j)) {
            j += 1;
        }
        if j < n && REPO_WORDS.contains(&w(j)) {
            explicit = true;
            j += 1;
        }
        if j >= n {
            i += 1;
            continue;
        }
        let mut k = j + 1;
        if k < n && REPO_WORDS.contains(&w(k)) {
            explicit = true;
            k += 1;
        }
        match classify_repo(prep, explicit, toks[j], repos, prefer) {
            Some(r) => {
                best = (Some(r), Some((i, k)));
                i = k;
            }
            None => i += 1,
        }
    }
    best
}

/// Remove "(in|with) (a) new/fresh session" from the task; true if present.
fn take_new_session(mut toks: Vec<&Tok>) -> (Vec<&Tok>, bool) {
    let w = |t: &[&Tok], i: usize| t.get(i).map(|t| t.plain.clone()).unwrap_or_default();
    for i in 0..toks.len() {
        if ["new", "fresh", "separate", "another"].contains(&w(&toks, i).as_str())
            && ["session", "chat", "instance", "conversation", "thread"].contains(&w(&toks, i + 1).as_str())
        {
            let mut start = i;
            if start > 0 && ["a", "an"].contains(&w(&toks, start - 1).as_str()) {
                start -= 1;
            }
            if start > 0 && ["in", "with", "using", "via", "as"].contains(&w(&toks, start - 1).as_str()) {
                start -= 1;
            }
            toks.drain(start..i + 2);
            return (toks, true);
        }
    }
    (toks, false)
}

fn strip_connectors(task: &mut Vec<&Tok>, after_tell: bool) {
    loop {
        let w = |i: usize| task.get(i).map(|t| t.plain.as_str()).unwrap_or("");
        let (w0, w1, w2) = (w(0), w(1), w(2));
        let prompt_words = ["prompt", "task", "instructions", "message"];
        if ["to", "and", "then", "saying"].contains(&w0) {
            task.remove(0);
        } else if w0 == "with" && prompt_words.contains(&w1) {
            task.drain(..2);
        } else if w0 == "with" && w1 == "the" && prompt_words.contains(&w2) {
            task.drain(..3);
        } else if after_tell && w0 == "that" {
            task.remove(0);
        } else {
            break;
        }
    }
}

fn join_task(toks: &[&Tok]) -> String {
    if toks.len() == 1 && toks[0].quoted {
        return toks[0].orig.trim().to_string();
    }
    let s = toks
        .iter()
        .map(|t| if t.quoted { format!("\"{}\"", t.orig) } else { t.orig.clone() })
        .collect::<Vec<_>>()
        .join(" ");
    s.trim().trim_end_matches([',', ';', ':']).trim().to_string()
}

fn is_status_phrase(words: &[&str], s: &str) -> bool {
    ["doing", "status", "progress", "happening"].iter().any(|w| has(words, w))
        || ["up to", "working on", "going on"].iter().any(|p| s.contains(p))
}

// ---------- parser ----------

fn strip_fillers(toks: &mut Vec<Tok>, pet_name: Option<&str>) {
    let pet = pet_name.map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty());
    loop {
        if toks.len() <= 1 {
            break;
        }
        let n0 = toks[0].key.clone();
        let n1 = toks[1].plain.clone();
        if LEAD_FILLERS.contains(&n0.as_str()) || pet.as_deref() == Some(n0.as_str()) {
            toks.remove(0);
        } else if (["can", "could", "would", "will"].contains(&n0.as_str()) && n1 == "you")
            || (["tell", "show", "give"].contains(&n0.as_str()) && n1 == "me")
        {
            if toks.len() <= 2 {
                break;
            }
            toks.drain(..2);
        } else {
            break;
        }
    }
    while toks.len() > 1 && TRAIL_FILLERS.contains(&toks[toks.len() - 1].plain.as_str()) {
        toks.pop();
    }
}

fn parse_pick(w: &str) -> Option<Pick> {
    if let Ok(n) = w.parse::<usize>() {
        return (1..=99).contains(&n).then_some(Pick::Nth(n));
    }
    match w {
        "first" | "1st" | "oldest" => Some(Pick::Nth(1)),
        "second" | "2nd" => Some(Pick::Nth(2)),
        "third" | "3rd" => Some(Pick::Nth(3)),
        "last" | "latest" | "newest" => Some(Pick::Last),
        _ => None,
    }
}

fn parse_resolve(toks: &[Tok], words: &[&str], s: &str, cat: &AgentCatalog) -> Option<Intent> {
    let (approve, rest_start) = if APPROVE_WHOLE.contains(&s) {
        (true, toks.len())
    } else if DENY_WHOLE.contains(&s) {
        (false, toks.len())
    } else {
        let i = usize::from(ALWAYS.contains(&words[0]) && words.len() > 1);
        if APPROVE_VERBS.contains(&words[i]) {
            (true, i + 1)
        } else if DENY_VERBS.contains(&words[i]) {
            (false, i + 1)
        } else {
            return None;
        }
    };
    let always = words.iter().any(|w| ALWAYS.contains(w)) || s.contains("from now on") || s == "never";
    let rest = &toks[rest_start.min(toks.len())..];
    let agent = mention_anywhere(rest, cat);
    let pick = rest.iter().find_map(|t| parse_pick(&t.plain));
    Some(Intent::Resolve { approve, always, agent, pick })
}

fn parse_command(toks: &[Tok], cat: &AgentCatalog, repos: &RepoIndex) -> Option<Intent> {
    let w = |i: usize| toks.get(i).map(|t| t.plain.as_str()).unwrap_or("");
    let first = w(0);
    let rest_words = |from: usize| -> (Vec<&str>, String) {
        let v: Vec<&str> = toks[from.min(toks.len())..].iter().map(|t| t.plain.as_str()).collect();
        let s = v.join(" ");
        (v, s)
    };
    let (agent, after, force_start) = if COMMAND_VERBS.contains(&first) {
        let mut i = 1;
        while DETERMINERS.contains(&w(i)) {
            i += 1;
        }
        let (agent, len) = agent_at(toks, i, cat)?;
        let mut j = i + len;
        while AGENT_NOUNS.contains(&w(j)) {
            j += 1;
        }
        if first == "let" && w(j) == "know" {
            j += 1;
        }
        if first == "ask" && w(j) != "to" {
            let (v, s) = rest_words(j);
            if v.is_empty() || is_status_phrase(&v, &s) {
                return Some(Intent::Status { agent: Some(agent) });
            }
        }
        (agent, j, false)
    } else if START_VERBS.contains(&first) {
        let mut i = 1;
        while START_SKIP.contains(&w(i)) {
            i += 1;
        }
        let (agent, len) = agent_at(toks, i, cat)?;
        let mut j = i + len;
        while AGENT_NOUNS.contains(&w(j)) {
            j += 1;
        }
        (agent, j, true)
    } else {
        let (agent, len) = agent_at(toks, 0, cat)?;
        let addressed = toks[len - 1].sep || toks[0].orig.trim_start().starts_with('@') || w(len) == "please";
        if !addressed || len >= toks.len() {
            return None;
        }
        let j = if w(len) == "please" { len + 1 } else { len };
        let (v, s) = rest_words(j);
        if is_status_phrase(&v, &s) {
            return Some(Intent::Status { agent: Some(agent) });
        }
        (agent, j, false)
    };

    let rest: Vec<&Tok> = toks[after.min(toks.len())..].iter().collect();
    let (rest, new_session) = take_new_session(rest);
    let prefer = match &agent {
        AgentRef::Known(id) => Some(id.as_str()),
        AgentRef::Unknown(_) => None,
    };
    let (repo, range) = extract_repo(&rest, repos, prefer);
    let mut task: Vec<&Tok> = rest
        .iter()
        .enumerate()
        .filter(|(i, _)| range.is_none_or(|(a, b)| *i < a || *i >= b))
        .map(|(_, t)| *t)
        .collect();
    strip_connectors(&mut task, first == "tell");
    Some(Intent::Command(CommandIntent { agent, repo, task: join_task(&task), force_new: force_start || new_session }))
}

fn is_help(s: &str, words: &[&str]) -> bool {
    matches!(
        s,
        "help" | "commands" | "menu" | "options" | "usage" | "what can you do" | "what do you do" | "what can i say"
            | "how do i use you" | "how does this work"
    ) || (words[0] == "help" && words.len() <= 3)
}

fn is_daily(words: &[&str], s: &str) -> bool {
    has(words, "daily")
        || s.contains("end of day")
        || has(words, "eod")
        || has(words, "recap")
        || has(words, "standup")
        || (has(words, "report") && !has(words, "status"))
        || (has(words, "today")
            && (s.contains("happen") || s.contains("summar") || has(words, "did") || s.contains("get done") || s.contains("accomplish")))
}

fn is_status(words: &[&str], s: &str, has_agent: bool, only_agent: bool) -> bool {
    if only_agent {
        return true;
    }
    if !has_agent && SMALLTALK_QUESTIONS.iter().any(|p| s == *p || s.starts_with(&format!("{p} "))) {
        return false;
    }
    STATUS_WORDS.iter().any(|w| has(words, w))
        || STATUS_PHRASES.iter().any(|p| s.contains(p))
        || (has_agent
            && (s.starts_with("how") || s.starts_with("is ") || s.starts_with("whats") || s.starts_with("what is")))
}

pub(crate) fn parse(text: &str, cat: &AgentCatalog, repos: &RepoIndex, pet_name: Option<&str>) -> Intent {
    let mut toks = tokenize(text);
    strip_fillers(&mut toks, pet_name);
    if toks.is_empty() {
        return Intent::SmallTalk;
    }
    let words: Vec<&str> = toks.iter().map(|t| t.plain.as_str()).collect();
    let s = words.join(" ");
    if is_help(&s, &words) {
        return Intent::Help;
    }
    if let Some(i) = parse_resolve(&toks, &words, &s, cat) {
        return i;
    }
    if let Some(i) = parse_command(&toks, cat, repos) {
        return i;
    }
    if is_daily(&words, &s) {
        return Intent::DailyReport;
    }
    let mention = cat.find(&toks);
    let only_agent = mention.as_ref().is_some_and(|(_, idx, len)| *idx == 0 && *len == toks.len());
    let has_agent = mention.is_some() || toks.iter().any(|t| KNOWN_AGENT_WORDS.contains(&t.key.as_str()));
    if is_status(&words, &s, has_agent, only_agent) {
        return Intent::Status { agent: mention_anywhere(&toks, cat) };
    }
    Intent::SmallTalk
}

pub(crate) fn talk_kind(text: &str) -> Talk {
    let toks = tokenize(text);
    let words: Vec<&str> = toks.iter().map(|t| t.plain.as_str()).collect();
    let s = words.join(" ");
    let any = |list: &[&str]| list.iter().any(|w| has(&words, w));
    let contains = |list: &[&str]| list.iter().any(|p| s.contains(p));
    if contains(&[
        "good boy", "good girl", "good dog", "good pet", "good job", "good bot", "good kitty", "good bird", "good robot",
        "whos a good", "love you", "you rock", "best pet", "cute", "well done", "nice work",
    ]) {
        Talk::Praise
    } else if any(&["thanks", "thank", "thx", "ty", "cheers", "appreciate"]) {
        Talk::Thanks
    } else if contains(&[
        "how are you", "how r u", "how you doing", "how do you feel", "are you ok", "you ok", "hows it going",
        "how is it going", "how are things",
    ]) {
        Talk::Feeling
    } else if any(&["treat", "treats", "walk", "fetch", "ball", "play", "belly", "pat", "scritches", "snack", "bone", "toy"]) {
        Talk::Play
    } else if any(&["bye", "goodbye", "goodnight", "cya", "later"]) || contains(&["see you", "good night"]) {
        Talk::Bye
    } else if words.first().is_some_and(|w| {
        ["hi", "hello", "hey", "yo", "howdy", "hiya", "sup", "morning", "evening", "afternoon", "greetings", "whats"].contains(w)
    }) || contains(&["good morning", "good evening", "good afternoon"])
    {
        Talk::Greeting
    } else {
        Talk::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krypto_protocol::{AgentSource, SessionSource, SessionStatus};

    fn agent(id: &str, name: &str) -> Agent {
        Agent {
            id: id.into(),
            name: name.into(),
            command: vec![],
            env: None,
            version: None,
            description: None,
            icon: None,
            installed: true,
            source: AgentSource::Registry,
            capabilities: None,
        }
    }

    fn session(id: &str, agent: &str, cwd: &str) -> Session {
        Session {
            id: id.into(),
            agent_id: agent.into(),
            acp_session_id: None,
            external_id: None,
            cwd: cwd.into(),
            title: None,
            source: SessionSource::Owned,
            status: SessionStatus::Idle,
            started_at: 0,
            last_event_at: 0,
            ended_at: None,
        }
    }

    fn setup() -> (AgentCatalog, RepoIndex) {
        let agents = vec![agent("claude-code", "Claude Code"), agent("codex", "Codex CLI"), agent("gemini", "Gemini CLI")];
        let sessions = vec![
            session("s1", "claude-code", "E:\\workspace\\ai-pet"),
            session("s2", "codex", "/home/me/web-app"),
            session("s3", "copilot", "/home/me/api"),
        ];
        (AgentCatalog::build(&agents, &sessions), RepoIndex::build(&sessions))
    }

    fn p(text: &str) -> Intent {
        let (c, r) = setup();
        parse(text, &c, &r, Some("Biscuit"))
    }

    fn cmd(text: &str) -> CommandIntent {
        match p(text) {
            Intent::Command(c) => c,
            other => panic!("{text:?} -> {other:?}"),
        }
    }

    fn known(id: &str) -> AgentRef {
        AgentRef::Known(id.into())
    }

    #[test]
    fn tokenizer_keeps_quotes_and_addressing() {
        let t = tokenize("Codex: run \"cargo test --all\", then  what's up?");
        assert_eq!(t[0].key, "codex");
        assert!(t[0].sep);
        assert!(t[2].quoted);
        assert_eq!(t[2].orig, "cargo test --all");
        assert_eq!(t[4].plain, "whats");
        assert_eq!(tokenize("claude's")[0].key, "claude");
    }

    #[test]
    fn status_questions() {
        assert_eq!(p("What's Claude doing?"), Intent::Status { agent: Some(known("claude-code")) });
        assert_eq!(p("hey Biscuit, what is codex up to"), Intent::Status { agent: Some(known("codex")) });
        assert_eq!(p("status"), Intent::Status { agent: None });
        assert_eq!(p("how's claude code going?"), Intent::Status { agent: Some(known("claude-code")) });
        assert_eq!(p("codex?"), Intent::Status { agent: Some(known("codex")) });
        assert_eq!(p("is copilot done"), Intent::Status { agent: Some(known("copilot")) });
        assert_eq!(p("ask claude what it's doing"), Intent::Status { agent: Some(known("claude-code")) });
        assert_eq!(p("what's goose doing?"), Intent::Status { agent: Some(AgentRef::Unknown("Goose".into())) });
        assert_eq!(p("how are you?"), Intent::SmallTalk);
    }

    #[test]
    fn commands_resolve_agent_repo_and_task() {
        let c = cmd("tell codex to run the tests in repo ai-pet");
        assert_eq!(c.agent, known("codex"));
        assert_eq!(c.repo, Some(RepoRef::Known { name: "ai-pet".into(), cwd: "E:\\workspace\\ai-pet".into() }));
        assert_eq!(c.task, "run the tests");
        assert!(!c.force_new);

        let c = cmd("Tell Claude in the web-app repo to fix the failing build, please");
        assert_eq!(c.agent, known("claude-code"));
        assert_eq!(c.repo, Some(RepoRef::Known { name: "web-app".into(), cwd: "/home/me/web-app".into() }));
        assert_eq!(c.task, "fix the failing build");

        let c = cmd("have codex add a README");
        assert_eq!((c.repo, c.task.as_str()), (None, "add a README"));

        let c = cmd("codex, write tests for the parser");
        assert_eq!(c.agent, known("codex"));
        assert_eq!(c.repo, None);
        assert_eq!(c.task, "write tests for the parser");

        let c = cmd("ask codex to bump deps in repo unknown-thing");
        assert_eq!(c.repo, Some(RepoRef::Unknown { name: "unknown-thing".into() }));
        assert_eq!(c.task, "bump deps");

        let c = cmd("start a new claude session in \"C:\\My Code\\app\" to refactor auth");
        assert!(c.force_new);
        assert_eq!(c.repo, Some(RepoRef::Path("C:\\My Code\\app".into())));
        assert_eq!(c.task, "refactor auth");

        let c = cmd("tell codex in a new session to lint ai-pet");
        assert!(c.force_new);
        assert_eq!(c.task, "lint ai-pet");

        let c = cmd("tell gemini to explain the code");
        assert_eq!(c.agent, known("gemini"));

        let c = cmd("tell cursor to do stuff");
        assert_eq!(c.agent, AgentRef::Unknown("Cursor".into()));

        // A prepositional phrase that is not a repo stays in the task.
        let c = cmd("tell codex to fix the bug in the parser");
        assert_eq!((c.repo, c.task.as_str()), (None, "fix the bug in the parser"));
        // Fuzzy match on explicit repo word.
        let c = cmd("tell codex to deploy in repo web");
        assert_eq!(c.repo, Some(RepoRef::Known { name: "web".into(), cwd: "/home/me/web-app".into() }));
    }

    #[test]
    fn approvals() {
        assert_eq!(p("approve"), Intent::Resolve { approve: true, always: false, agent: None, pick: None });
        assert_eq!(p("Yes!"), Intent::Resolve { approve: true, always: false, agent: None, pick: None });
        assert_eq!(p("ok, deny it"), Intent::Resolve { approve: false, always: false, agent: None, pick: None });
        assert_eq!(p("no"), Intent::Resolve { approve: false, always: false, agent: None, pick: None });
        assert_eq!(
            p("always allow codex"),
            Intent::Resolve { approve: true, always: true, agent: Some(known("codex")), pick: None }
        );
        assert_eq!(p("approve #2"), Intent::Resolve { approve: true, always: false, agent: None, pick: Some(Pick::Nth(2)) });
        assert_eq!(p("reject the last one"), Intent::Resolve { approve: false, always: false, agent: None, pick: Some(Pick::Last) });
        assert_eq!(p("never"), Intent::Resolve { approve: false, always: true, agent: None, pick: None });
        assert_eq!(p("oh no"), Intent::SmallTalk);
        assert_eq!(p("never mind"), Intent::SmallTalk);
    }

    #[test]
    fn help_report_and_small_talk() {
        assert_eq!(p("help"), Intent::Help);
        assert_eq!(p("what can you do?"), Intent::Help);
        assert_eq!(p("daily report please"), Intent::DailyReport);
        assert_eq!(p("give me the end of day recap"), Intent::DailyReport);
        assert_eq!(p("what happened today?"), Intent::DailyReport);
        assert_eq!(p("status report"), Intent::Status { agent: None });
        assert_eq!(p("hi"), Intent::SmallTalk);
        assert_eq!(p("tell me a joke"), Intent::SmallTalk);
        assert_eq!(p("claude is slow today"), Intent::SmallTalk);
        assert_eq!(talk_kind("who's a good boy?"), Talk::Praise);
        assert_eq!(talk_kind("hello there"), Talk::Greeting);
        assert_eq!(talk_kind("thank you!"), Talk::Thanks);
        assert_eq!(talk_kind("how are you"), Talk::Feeling);
        assert_eq!(talk_kind("wanna play fetch?"), Talk::Play);
        assert_eq!(talk_kind("purple"), Talk::Other);
    }

    #[test]
    fn repo_resolution() {
        let (_, r) = setup();
        assert_eq!(r.resolve("AI-PET", None, false).as_deref(), Some("E:\\workspace\\ai-pet"));
        assert_eq!(r.resolve("aipet", None, false).as_deref(), Some("E:\\workspace\\ai-pet"));
        assert_eq!(r.resolve("pet", None, false), None);
        assert_eq!(r.resolve("pet", None, true).as_deref(), Some("E:\\workspace\\ai-pet"));
        assert_eq!(r.resolve("ap", None, true), None);
        assert_eq!(r.names(5).len(), 3);
    }
}
