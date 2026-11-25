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
//!
//! Targets (migration step 5), recognized in commands, status questions and approvals:
//! - machine: display name or unique prefix, in context ("on the mac mini", "the mac mini's claude",
//!   "tell the mac mini to …", "what is the mac mini doing?"); "here" / "on this machine" = local.
//! - pet: any known pet name ("tell Biscuit to …", "Biscuit, run the tests", "what's Rex doing?");
//!   a pet fixes machine + agent.
//! - repo: names from every machine's repo index and sessions; the caller resolves them per machine.
//!
//! The default pet (the chat with no pet attached) orchestrates every agent: a plain instruction that
//! starts with a task verb ("fix the failing test in repo api", "run the tests on the Laptop", "start a
//! session in repo web") is a command with no agent; the caller picks one and only proposes it.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use pawl_protocol::{Agent, AgentId, MachineId, Pet, Session};

use crate::util::{basename, clean_name, path_key};

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
    /// A name some machine's repo index (or a session cwd) matches. `name` as typed, `key` normalized,
    /// `explicit` = said "repo"/"project"/… (allows a fuzzy match). The caller resolves it per machine.
    Named { name: String, key: String, explicit: bool },
    Unknown { name: String },
}

/// A machine the user named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MachineRef {
    /// "here", "on this machine".
    Here,
    /// By display name or unique prefix (may be this machine's own id).
    Id(MachineId),
}

/// A pet the user named: it fixes the machine and the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PetRef {
    pub id: String,
    pub name: String,
    pub machine_id: MachineId,
    pub agent_id: AgentId,
}

/// Where the user pointed a command, status question or approval.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Target {
    pub machine: Option<MachineRef>,
    pub pet: Option<PetRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub intent: Intent,
    pub target: Target,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommandIntent {
    /// None when only a machine or pet was named ("tell the mac mini to run the tests").
    pub agent: Option<AgentRef>,
    pub repo: Option<RepoRef>,
    pub task: String,
    /// "start …" / "new session": always start a session instead of prompting one.
    pub force_new: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Intent {
    Help,
    /// `explicit`: an approve / deny verb was said ("approve", "allow codex"), not just "yes" / "sure".
    Resolve { approve: bool, always: bool, agent: Option<AgentRef>, pick: Option<Pick>, explicit: bool },
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
    /// Ends in a possessive "'s" ("mac mini's").
    pub poss: bool,
}

fn make_tok(orig: String, sep: bool, quoted: bool) -> Tok {
    let lower = orig.to_lowercase();
    let norm = lower
        .trim_start_matches(|c: char| "\"'`([{<@*#".contains(c))
        .trim_end_matches(|c: char| "\"'`)]}>.,!?;:*".contains(c))
        .to_string();
    let plain = norm.replace('\'', "");
    let stripped = norm.strip_suffix("'s").filter(|k| !k.is_empty());
    let poss = stripped.is_some() && !quoted;
    let key = stripped.unwrap_or(&norm).to_string();
    Tok { orig, plain, key, sep, quoted, poss }
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
    "working", "stuck", "summary", "summarize", "agents", "overview", "running",
];
/// Prepositions that introduce a machine ("on the mac mini", "at windows pc").
const MACHINE_PREPS: &[&str] = &["on", "at"];
/// Nouns that may follow a machine name ("the mac mini machine").
const MACHINE_NOUNS: &[&str] = &["machine", "computer", "box", "laptop", "desktop", "pc", "device", "host", "server"];
/// Words that may follow a machine phrase ("tell the mac mini to …", "on the mac mini in repo x",
/// "is the mac mini busy?"); anything else means the name is part of a longer phrase.
const MACHINE_FOLLOW: &[&str] = &[
    "to", "in", "and", "for", "with", "then", "please", "now", "right", "doing", "up", "is", "has", "busy", "working",
    "running", "status",
];
const QUESTION_WORDS: &[&str] = &[
    "what", "whats", "how", "hows", "why", "who", "whos", "where", "wheres", "when", "is", "are", "was", "were", "do",
    "does", "did", "can", "could", "will", "would", "should", "any", "anything", "anyone",
];
const STATUS_PHRASES: &[&str] = &["up to", "working on", "going on", "check on", "whats new", "where are we"];
/// First words of a plain instruction for the default pet ("fix the failing test", "run the tests on the
/// laptop"): verbs that clearly mean coding work. A message must start with one to become an agent-less
/// command (which the user still confirms: it never runs by itself).
const TASK_VERBS: &[&str] = &[
    "fix", "debug", "refactor", "implement", "lint", "test", "retest", "run", "rerun", "build", "rebuild", "compile",
    "typecheck", "deploy", "commit", "rebase", "revert", "bisect", "benchmark", "migrate", "reproduce", "review",
];
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
    /// (on this machine, installed, last seen): a tie between this machine's agent and a paired
    /// machine's goes to this machine's.
    rank: (bool, bool, i64),
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
    /// `is_local` tells this machine's ids apart: its agents win alias ties.
    pub fn build(agents: &[Agent], sessions: &[Session], is_local: impl Fn(&str) -> bool) -> Self {
        let last_seen = |id: &str| sessions.iter().filter(|s| s.agent_id == id).map(|s| s.last_event_at).max().unwrap_or(0);
        let mut aliases = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        let mut agents: Vec<&Agent> = agents.iter().collect();
        // This machine's entry of an id first (it is the one that can run here).
        agents.sort_by_key(|a| !is_local(&a.machine_id));
        for a in agents {
            if seen.insert(a.id.as_str()) {
                let rank = (is_local(&a.machine_id), a.installed, last_seen(&a.id));
                for words in aliases_for(&a.id, Some(&a.name)) {
                    aliases.push(Alias { words, agent_id: a.id.clone(), rank });
                }
            }
        }
        for s in sessions {
            if seen.insert(s.agent_id.as_str()) {
                let local = sessions.iter().any(|x| x.agent_id == s.agent_id && is_local(&x.machine_id));
                let rank = (local, false, last_seen(&s.agent_id));
                for words in aliases_for(&s.agent_id, None) {
                    aliases.push(Alias { words, agent_id: s.agent_id.clone(), rank });
                }
            }
        }
        Self { aliases }
    }

    /// Longest alias starting at token `i` (ties: this machine's agent, installed, then most recently seen).
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

// ---------- machine and pet names ----------

#[derive(Debug, Clone)]
struct NamedMachine {
    id: MachineId,
    alnum: String,
    local: bool,
}

#[derive(Debug, Clone)]
struct NamedPet {
    words: Vec<String>,
    pet: PetRef,
    /// (the pet being chatted with, on this machine): wins a tie between equal names.
    rank: (bool, bool),
}

/// Machine and pet names users can type (from the brain context).
#[derive(Debug, Clone, Default)]
pub(crate) struct Targets {
    machines: Vec<NamedMachine>,
    pets: Vec<NamedPet>,
}

/// Machine names that would shadow words for this machine; such a machine can't be named in chat.
const RESERVED_MACHINE_NAMES: &[&str] = &[
    "here", "local", "locally", "localhost", "me", "my", "this", "thismachine", "thiscomputer", "thispc", "thislaptop",
    "thisdesktop", "thisbox", "thisone",
];

impl Targets {
    /// `machines`: (id, display name). `pets`: every known pet (repeats of an id are ignored).
    /// `local_id`: this machine's id; `chatted`: the id of the pet being chatted with.
    pub fn new<'a>(
        machines: impl IntoIterator<Item = (&'a str, &'a str)>,
        pets: impl IntoIterator<Item = &'a Pet>,
        local_id: &str,
        chatted: Option<&str>,
    ) -> Self {
        let is_local = |id: &str| local_id.is_empty() || id.is_empty() || id == local_id;
        let mut seen: HashSet<String> = HashSet::new();
        let machines = machines
            .into_iter()
            .filter(|(id, _)| !id.is_empty() && seen.insert((*id).to_string()))
            .map(|(id, name)| NamedMachine { id: id.to_string(), alnum: alnum(name), local: is_local(id) })
            .filter(|m| !m.alnum.is_empty() && !RESERVED_MACHINE_NAMES.contains(&m.alnum.as_str()))
            .collect();
        let mut seen: HashSet<String> = HashSet::new();
        let pets = pets
            .into_iter()
            .filter(|p| seen.insert(p.id.clone()))
            .map(|p| NamedPet {
                words: split_words(&p.name),
                pet: PetRef {
                    id: p.id.clone(),
                    name: p.name.trim().to_string(),
                    machine_id: p.machine_id.clone(),
                    agent_id: p.agent_id.clone(),
                },
                rank: (chatted == Some(p.id.as_str()), is_local(&p.machine_id)),
            })
            .filter(|p| !p.words.is_empty())
            .collect();
        Self { machines, pets }
    }

    /// Longest machine name, else a unique prefix of 3+ letters, starting at token `i`:
    /// (machine id, token length, full name matched).
    fn machine_at(&self, toks: &[Tok], i: usize) -> Option<(MachineId, usize, bool)> {
        let mut full: Option<(usize, Vec<&NamedMachine>)> = None;
        let mut prefix: Option<(usize, Vec<&NamedMachine>)> = None;
        let mut typed = String::new();
        for (k, t) in toks[i..].iter().enumerate() {
            if t.quoted {
                break;
            }
            typed.push_str(&alnum(&t.key));
            if typed.is_empty() {
                break;
            }
            let eq: Vec<&NamedMachine> = self.machines.iter().filter(|m| m.alnum == typed).collect();
            if !eq.is_empty() {
                full = Some((k + 1, eq));
            }
            let pre: Vec<&NamedMachine> =
                self.machines.iter().filter(|m| m.alnum.len() > typed.len() && m.alnum.starts_with(&typed)).collect();
            if pre.is_empty() {
                break;
            }
            if typed.len() >= 3 {
                prefix = Some((k + 1, pre));
            }
            // "mac mini's" / "mac mini," ends the name.
            if t.poss || t.sep {
                break;
            }
        }
        // One match, or this machine among several equal ones (a tie goes to this machine).
        let pick = |ms: Vec<&NamedMachine>| -> Option<MachineId> {
            match ms.as_slice() {
                [one] => Some(one.id.clone()),
                many => {
                    let local: Vec<&&NamedMachine> = many.iter().filter(|m| m.local).collect();
                    (local.len() == 1).then(|| local[0].id.clone())
                }
            }
        };
        if let Some((len, ms)) = full {
            return pick(ms).map(|id| (id, len, true));
        }
        let (len, ms) = prefix?;
        pick(ms).map(|id| (id, len, false))
    }

    /// A pet name starting at token `i`: (pet, token length). Equal names: the chatted pet, then this
    /// machine's pet.
    pub fn pet_at(&self, toks: &[Tok], i: usize) -> Option<(PetRef, usize)> {
        self.pets
            .iter()
            .filter(|p| {
                let n = p.words.len();
                i + n <= toks.len() && p.words.iter().zip(&toks[i..i + n]).all(|(w, t)| !t.quoted && *w == t.key)
            })
            .max_by_key(|p| (p.words.len(), p.rank))
            .map(|p| (p.pet.clone(), p.words.len()))
    }

    /// True if `p` is the pet being chatted with (by id).
    fn is_chatted(&self, p: &PetRef) -> bool {
        self.pets.iter().any(|n| n.rank.0 && n.pet.id == p.id)
    }

    /// First pet mention anywhere.
    fn find_pet(&self, toks: &[Tok]) -> Option<PetRef> {
        (0..toks.len()).find_map(|i| self.pet_at(toks, i).map(|(p, _)| p))
    }
}

fn is_question(toks: &[Tok]) -> bool {
    toks.first().is_some_and(|t| QUESTION_WORDS.contains(&t.plain.as_str()))
        || toks.last().is_some_and(|t| t.orig.trim_end().ends_with('?'))
        || toks.iter().any(|t| STATUS_WORDS.contains(&t.plain.as_str()))
}

/// "here" or "this machine" at token `i`: (token length, needs context). "this machine" counts only
/// after "on"/"from", possessive or before an agent name, so a task like "wipe this machine" keeps
/// its words. "locally" is never taken: "run the build locally" means something to the agent.
fn local_at(toks: &[Tok], i: usize) -> Option<(usize, bool)> {
    let w = |k: usize| toks.get(k).filter(|t| !t.quoted).map(|t| t.plain.as_str()).unwrap_or("");
    match w(i) {
        "this" if ["machine", "computer", "pc", "laptop", "desktop", "box"].contains(&toks.get(i + 1).map_or("", |t| t.key.as_str())) => {
            Some((2, true))
        }
        "here" => {
            let prev = if i > 0 { w(i - 1) } else { "" };
            let tail = i + 1 == toks.len() || toks[i].sep || ["in", "on", "to", "and"].contains(&w(i + 1));
            let spatial = ["in", "from", "over", "right", "near", "out", "up", "down", "come", "get", "go"].contains(&prev);
            (tail && !spatial).then_some((1, false))
        }
        _ => None,
    }
}

/// Find and remove one machine phrase ("on the mac mini", "the mac mini's", "here"). A machine name
/// counts only in context, so ordinary words are not taken for machines: after "on"/"at"/"from",
/// possessive, right before an agent or pet name, after a command verb ("tell the mac mini to …"),
/// addressed at the start ("Mac mini: …"), or (full name) in a question. A prefix must also end the
/// phrase ("tell the mac to …", not "on the mac address"). Returns the machine and whether the phrase
/// was a lead address ("on the mac mini, …").
fn take_machine(toks: &mut Vec<Tok>, targets: &Targets, cat: &AgentCatalog) -> Option<(MachineRef, bool)> {
    let question = is_question(toks);
    let w = |t: &[Tok], k: usize| t.get(k).map(|t| t.plain.clone()).unwrap_or_default();
    for i in 0..toks.len() {
        if toks[i].quoted {
            continue;
        }
        let (found, mut end, full, local_needs_context) = if let Some((len, needs)) = local_at(toks, i) {
            (MachineRef::Here, i + len, true, needs)
        } else if let Some((id, len, full)) = targets.machine_at(toks, i) {
            (MachineRef::Id(id), i + len, full, false)
        } else {
            continue;
        };
        let named = matches!(found, MachineRef::Id(_));
        // An agent or pet name typed here is the agent or pet, not a machine that happens to share it
        // ("tell claude to …" with a paired machine called "Claude"), so it can't redirect a command.
        if named {
            let len = end - i;
            let agent = cat.match_at(toks, i).is_some_and(|(_, l)| l >= len)
                || (len == 1 && KNOWN_AGENT_WORDS.contains(&toks[i].key.as_str()));
            let pet = targets.pet_at(toks, i).is_some_and(|(_, l)| l >= len);
            if agent || pet {
                continue;
            }
        }
        if named && !toks[end - 1].poss && !toks[end - 1].sep && MACHINE_NOUNS.contains(&w(toks, end).as_str()) {
            end += 1;
        }
        let (poss, sep) = (toks[end - 1].poss, toks[end - 1].sep);
        let mut start = i;
        while start > 0 && ["the", "my", "our", "that"].contains(&w(toks, start - 1).as_str()) {
            start -= 1;
        }
        let prep = start > 0 && MACHINE_PREPS.contains(&w(toks, start - 1).as_str());
        if prep {
            start -= 1;
        }
        let next = w(toks, end);
        let next_names = end < toks.len()
            && (cat.match_at(toks, end).is_some()
                || targets.pet_at(toks, end).is_some()
                || KNOWN_AGENT_WORDS.contains(&toks[end].key.as_str()));
        let after_verb = !prep && start > 0 && COMMAND_VERBS.contains(&w(toks, start - 1).as_str()) && (next == "to" || sep);
        let lead = start == 0 && sep && end < toks.len();
        let context = if named {
            prep || poss || next_names || after_verb || lead || (full && question)
        } else {
            !local_needs_context || prep || poss || next_names || after_verb
        };
        let boundary = end == toks.len() || poss || sep || next_names || MACHINE_FOLLOW.contains(&next.as_str());
        if !context || (named && !boundary) {
            continue;
        }
        if sep && start > 0 {
            toks[start - 1].sep = true;
        }
        toks.drain(start..end);
        return Some((found, lead));
    }
    None
}

// ---------- repo index ----------

/// One folder of a machine's repo index.
#[derive(Debug, Clone)]
pub(crate) struct RepoEntry {
    pub machine_id: MachineId,
    /// Lowercase folder name.
    base: String,
    /// Lowercase index name ("api (work)").
    name: String,
    alnum: String,
    name_alnum: String,
    /// The index name, shown in replies.
    display: String,
    /// `path_key` of `cwd`.
    pub key: String,
    /// The index path (canonical, existing when the hub built the index).
    cwd: String,
    /// Agents with a session in this folder on this machine (ranking only).
    agents: Vec<AgentId>,
    /// Latest use: the index's `lastUsedAt` or a session's last event, whichever is newer (ranking only).
    ts: i64,
}

/// The hubs' repo indexes (`BrainContext::repos`, this machine's included): the only source of repo
/// names and folders, on every machine. The hub drops folders that no longer exist, so a stale
/// session cwd never becomes a repo; sessions only rank the index's folders (agent, recency).
#[derive(Debug, Clone, Default)]
pub(crate) struct RepoIndex {
    entries: Vec<RepoEntry>,
}

fn alnum(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// A folder on one machine: (cwd, display name).
pub(crate) type Folder = (String, String);

impl RepoIndex {
    pub fn build(sessions: &[Session], index: &BTreeMap<MachineId, Vec<pawl_protocol::RepoRef>>) -> Self {
        let mut entries = Vec::new();
        for (machine_id, repos) in index {
            for r in repos.iter().filter(|r| !r.path.trim().is_empty()) {
                let key = path_key(&r.path);
                let used: Vec<&Session> =
                    sessions.iter().filter(|s| &s.machine_id == machine_id && path_key(&s.cwd) == key).collect();
                let mut agents: Vec<AgentId> = used.iter().map(|s| s.agent_id.clone()).collect();
                agents.sort();
                agents.dedup();
                let ts = used.iter().map(|s| s.last_event_at).chain(r.last_used_at).max().unwrap_or(0);
                // Index names and paths come from other machines too: bounded, no control characters.
                let display = if r.name.trim().is_empty() { basename(&r.path) } else { clean_name(&r.name) };
                let base = basename(&r.path).to_lowercase();
                let name = display.to_lowercase();
                entries.push(RepoEntry {
                    machine_id: machine_id.clone(),
                    alnum: alnum(&base),
                    name_alnum: alnum(&name),
                    base,
                    name,
                    display,
                    key,
                    cwd: r.path.clone(),
                    agents,
                    ts,
                });
            }
        }
        Self { entries }
    }

    /// Entries whose name matches, among those `keep` accepts: exact folder or index name, then
    /// alphanumeric-equal, then a unique prefix (3+ letters), then (if `fuzzy`) a unique substring.
    /// "Unique" counts distinct folder names, so one repo cloned on several machines is one match.
    pub fn matching(&self, name: &str, fuzzy: bool, keep: impl Fn(&RepoEntry) -> bool) -> Vec<&RepoEntry> {
        let name = name.trim().to_lowercase();
        let na = alnum(&name);
        if na.is_empty() {
            return Vec::new();
        }
        let scope: Vec<&RepoEntry> = self.entries.iter().filter(|e| keep(e)).collect();
        let tier = |f: &dyn Fn(&RepoEntry) -> bool| -> Vec<&RepoEntry> { scope.iter().copied().filter(|e| f(e)).collect() };
        fn unique(v: Vec<&RepoEntry>) -> Vec<&RepoEntry> {
            let names: BTreeSet<&str> = v.iter().map(|e| e.base.as_str()).collect();
            if names.len() == 1 {
                v
            } else {
                Vec::new()
            }
        }
        let exact = tier(&|e| e.base == name || e.name == name);
        if !exact.is_empty() {
            return exact;
        }
        let loose = tier(&|e| e.alnum == na || e.name_alnum == na);
        if !loose.is_empty() {
            return loose;
        }
        if na.len() < 3 {
            return Vec::new();
        }
        let prefix = unique(tier(&|e| e.alnum.starts_with(&na) || e.name_alnum.starts_with(&na)));
        if !prefix.is_empty() || !fuzzy {
            return prefix;
        }
        unique(tier(&|e| e.alnum.contains(&na) || (e.alnum.len() >= 3 && na.contains(&e.alnum))))
    }

    /// True if some machine knows a repo by that name.
    pub fn known(&self, name: &str, fuzzy: bool) -> bool {
        !self.matching(name, fuzzy, |_| true).is_empty()
    }

    /// One index path for a repo name among `keep`'s entries: a folder `prefer_agent` worked in first,
    /// then the most recently used.
    pub fn resolve(&self, name: &str, prefer_agent: Option<&str>, fuzzy: bool, keep: impl Fn(&RepoEntry) -> bool) -> Option<String> {
        self.matching(name, fuzzy, keep)
            .into_iter()
            .max_by_key(|e| (prefer_agent.is_some_and(|a| e.agents.iter().any(|x| x == a)), e.ts))
            .map(|e| e.cwd.clone())
    }

    /// Distinct folders on `machine_id` matching `name`, most recently used first.
    pub fn folders_on(&self, machine_id: &str, name: &str, fuzzy: bool) -> Vec<Folder> {
        dedupe(self.matching(name, fuzzy, |e| e.machine_id == machine_id))
    }

    /// The index folder with path key `key` among `keep`'s entries (e.g. a session's cwd), if indexed.
    pub fn folder_where(&self, key: &str, keep: impl Fn(&RepoEntry) -> bool) -> Option<Folder> {
        dedupe(self.entries.iter().filter(|e| e.key == key && keep(e)).collect()).into_iter().next()
    }

    /// The index folder with path key `key` on `machine_id`, if that machine knows it.
    pub fn folder_by_key(&self, machine_id: &str, key: &str) -> Option<Folder> {
        self.folder_where(key, |e| e.machine_id == machine_id)
    }

    /// Distinct repo names among `keep`'s entries, most recently used first (for hints).
    pub fn names(&self, max: usize, keep: impl Fn(&RepoEntry) -> bool) -> Vec<String> {
        dedupe(self.entries.iter().filter(|e| keep(e)).collect()).into_iter().take(max).map(|(_, name)| name).collect()
    }
}

/// One (cwd, display) per folder, most recently used first (ties by name).
fn dedupe(hits: Vec<&RepoEntry>) -> Vec<Folder> {
    let mut by_key: BTreeMap<&str, &RepoEntry> = BTreeMap::new();
    for e in hits {
        let slot = by_key.entry(e.key.as_str()).or_insert(e);
        if e.ts > slot.ts {
            *slot = e;
        }
    }
    let mut v: Vec<&RepoEntry> = by_key.into_values().collect();
    v.sort_by(|a, b| b.ts.cmp(&a.ts).then_with(|| a.display.cmp(&b.display)));
    v.into_iter().map(|e| (e.cwd.clone(), e.display.clone())).collect()
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

fn classify_repo(prep: Option<&str>, explicit: bool, tok: &Tok, repos: &RepoIndex) -> Option<RepoRef> {
    let raw = clean_orig(&tok.orig);
    if raw.is_empty() || NOT_NAMES.contains(&tok.plain.as_str()) || REPO_WORDS.contains(&tok.plain.as_str()) {
        return None;
    }
    if is_path_like(&raw) {
        let path_ok = explicit || prep.is_some_and(|p| PATH_PREPS.contains(&p));
        return path_ok.then(|| RepoRef::Path(expand_home(&raw)));
    }
    if repos.known(&tok.key, explicit) {
        Some(RepoRef::Named { name: raw, key: tok.key.clone(), explicit })
    } else if explicit {
        Some(RepoRef::Unknown { name: raw })
    } else {
        None
    }
}

/// Find "in repo X" / "in the X project" / "at ~/code/x" / "in X" (X a known repo on some machine).
/// Returns the last match and its token range.
fn extract_repo(toks: &[&Tok], repos: &RepoIndex) -> (Option<RepoRef>, Option<(usize, usize)>) {
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
        match classify_repo(prep, explicit, toks[j], repos) {
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

/// Strip lead fillers ("hey", "please", the chatted pet's name, "can you") and trailing ones. Returns the
/// pet addressed at the start: the chatted pet by name, or any known pet followed by "," / ":".
fn strip_fillers(toks: &mut Vec<Tok>, pet_name: Option<&str>, targets: &Targets) -> Option<PetRef> {
    let pet = pet_name.map(|n| n.trim().to_lowercase()).filter(|n| !n.is_empty());
    let mut addressed = None;
    loop {
        if toks.len() <= 1 {
            break;
        }
        let n0 = toks[0].key.clone();
        let n1 = toks[1].plain.clone();
        if LEAD_FILLERS.contains(&n0.as_str()) {
            toks.remove(0);
            continue;
        }
        if let Some((p, len)) = targets.pet_at(toks, 0) {
            // The chatted pet by id: a paired machine's pet with the same name is not it.
            let chatted = targets.is_chatted(&p);
            if len < toks.len() && (chatted || toks[len - 1].sep) {
                toks.drain(..len);
                addressed = Some(p);
                continue;
            }
        }
        if pet.as_deref() == Some(n0.as_str()) {
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
    addressed
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

fn parse_resolve(toks: &[Tok], words: &[&str], s: &str, cat: &AgentCatalog, targets: &Targets, target: &mut Target) -> Option<Intent> {
    let (approve, rest_start, explicit) = if APPROVE_WHOLE.contains(&s) {
        (true, toks.len(), false)
    } else if DENY_WHOLE.contains(&s) {
        (false, toks.len(), false)
    } else {
        let i = usize::from(ALWAYS.contains(&words[0]) && words.len() > 1);
        if APPROVE_VERBS.contains(&words[i]) {
            (true, i + 1, true)
        } else if DENY_VERBS.contains(&words[i]) {
            (false, i + 1, true)
        } else {
            return None;
        }
    };
    let always = words.iter().any(|w| ALWAYS.contains(w)) || s.contains("from now on") || s == "never";
    let rest = &toks[rest_start.min(toks.len())..];
    let mut agent = mention_anywhere(rest, cat);
    if agent.is_none() {
        if let Some(p) = targets.find_pet(rest) {
            agent = Some(AgentRef::Known(p.agent_id.clone()));
            target.pet = Some(p);
        }
    }
    let pick = rest.iter().find_map(|t| parse_pick(&t.plain));
    Some(Intent::Resolve { approve, always, agent, pick, explicit })
}

/// An agent or pet name at token `i` (a pet stands for its agent and fixes the target).
fn agent_or_pet_at(toks: &[Tok], i: usize, cat: &AgentCatalog, targets: &Targets, target: &mut Target) -> Option<(AgentRef, usize)> {
    if let Some(found) = agent_at(toks, i, cat) {
        return Some(found);
    }
    let (pet, len) = targets.pet_at(toks, i)?;
    let agent = AgentRef::Known(pet.agent_id.clone());
    target.pet = Some(pet);
    Some((agent, len))
}

/// True if `toks` read as an instruction ("run the tests"), not a question, status ask or small talk.
fn task_like(toks: &[Tok]) -> bool {
    let words: Vec<&str> = toks.iter().map(|t| t.plain.as_str()).collect();
    let Some(first) = words.first() else { return false };
    let s = words.join(" ");
    !QUESTION_WORDS.contains(first)
        && !toks.last().is_some_and(|t| t.orig.trim_end().ends_with('?'))
        && !is_status_phrase(&words, &s)
        && !is_daily(&words, &s)
        && !SMALLTALK_QUESTIONS.iter().any(|p| s == *p || s.starts_with(&format!("{p} ")))
        && talk_kind(&s) == Talk::Other
}

/// True if `toks` read as a plain instruction for the default pet: two words or more, starting with a
/// [`TASK_VERBS`] verb, and [`task_like`] (not a question, recap or small talk). A status word anywhere
/// ("run status", "test what's running") makes it a status ask instead.
fn is_instruction(toks: &[Tok]) -> bool {
    let Some(first) = toks.first().map(|t| t.plain.as_str()) else { return false };
    let words: Vec<&str> = toks.iter().map(|t| t.plain.as_str()).collect();
    let s = words.join(" ");
    toks.len() >= 2
        && TASK_VERBS.contains(&first)
        && !words.iter().any(|w| STATUS_WORDS.contains(w))
        && !STATUS_PHRASES.iter().any(|p| s.contains(p))
        && task_like(toks)
}

/// `orchestrate`: the default pet is chatted with ("start a session in repo api" needs no agent).
fn parse_command(
    toks: &[Tok],
    cat: &AgentCatalog,
    repos: &RepoIndex,
    targets: &Targets,
    target: &mut Target,
    addressed: Option<&PetRef>,
    orchestrate: bool,
) -> Option<Intent> {
    let w = |i: usize| toks.get(i).map(|t| t.plain.as_str()).unwrap_or("");
    let first = w(0);
    let rest_words = |from: usize| -> (Vec<&str>, String) {
        let v: Vec<&str> = toks[from.min(toks.len())..].iter().map(|t| t.plain.as_str()).collect();
        let s = v.join(" ");
        (v, s)
    };
    let machine = target.machine.is_some();
    let (agent, after, force_start) = if COMMAND_VERBS.contains(&first) {
        let mut i = 1;
        while DETERMINERS.contains(&w(i)) {
            i += 1;
        }
        let (agent, mut j) = match agent_or_pet_at(toks, i, cat, targets, target) {
            Some((a, len)) => (Some(a), i + len),
            // "tell the mac mini to …" (the machine phrase is already gone): the agent is picked later.
            None if machine && (w(i) == "to" || AGENT_NOUNS.contains(&w(i))) => (None, i),
            None => return None,
        };
        while AGENT_NOUNS.contains(&w(j)) {
            j += 1;
        }
        if first == "let" && w(j) == "know" {
            j += 1;
        }
        if first == "ask" && w(j) != "to" {
            let (v, s) = rest_words(j);
            if v.is_empty() || is_status_phrase(&v, &s) {
                return Some(Intent::Status { agent });
            }
        }
        (agent, j, false)
    } else if START_VERBS.contains(&first) {
        let mut i = 1;
        while START_SKIP.contains(&w(i)) {
            i += 1;
        }
        let (agent, mut j) = match agent_or_pet_at(toks, i, cat, targets, target) {
            Some((a, len)) => (Some(a), i + len),
            // "start a session on the mac mini in api" / "Biscuit, start a new session in api" / the default
            // pet's "start a session in repo api".
            None if (machine || addressed.is_some() || orchestrate) && AGENT_NOUNS.contains(&w(i)) => (None, i),
            None => return None,
        };
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
        (Some(agent), j, false)
    };
    let mut cmd = command_from(toks, after, agent, force_start, first == "tell", repos);
    if cmd.agent.is_none() {
        if let Some(p) = addressed {
            cmd.agent = Some(AgentRef::Known(p.agent_id.clone()));
            target.pet = Some(p.clone());
        }
    }
    Some(Intent::Command(cmd))
}

/// Repo, task and "new session" from the tokens after the agent.
fn command_from(toks: &[Tok], after: usize, agent: Option<AgentRef>, force_start: bool, after_tell: bool, repos: &RepoIndex) -> CommandIntent {
    let rest: Vec<&Tok> = toks[after.min(toks.len())..].iter().collect();
    let (rest, new_session) = take_new_session(rest);
    let (repo, range) = extract_repo(&rest, repos);
    let mut task: Vec<&Tok> = rest
        .iter()
        .enumerate()
        .filter(|(i, _)| range.is_none_or(|(a, b)| *i < a || *i >= b))
        .map(|(_, t)| *t)
        .collect();
    strip_connectors(&mut task, after_tell);
    CommandIntent { agent, repo, task: join_task(&task), force_new: force_start || new_session }
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

/// Parse the user's own message. `targets` names machines and pets; `pet_name` is the pet being chatted with,
/// None for the default pet (which also takes plain instructions as commands with no agent).
pub(crate) fn parse(text: &str, cat: &AgentCatalog, repos: &RepoIndex, targets: &Targets, pet_name: Option<&str>) -> Parsed {
    let mut toks = tokenize(text);
    let mut addressed = strip_fillers(&mut toks, pet_name, targets);
    let mut target = Target::default();
    let mut lead_machine = false;
    if let Some((m, lead)) = take_machine(&mut toks, targets, cat) {
        target.machine = Some(m);
        lead_machine = lead;
        if let Some(p) = strip_fillers(&mut toks, pet_name, targets) {
            addressed = Some(p);
        }
    }
    // Addressing alone ("Rex, good boy!") is small talk, judged on the whole message.
    let addressed_task = (addressed.is_some() || lead_machine) && talk_kind(text) == Talk::Other;
    let intent = classify(&toks, cat, repos, targets, &mut target, addressed.as_ref(), addressed_task, pet_name.is_none());
    Parsed { intent, target }
}

#[allow(clippy::too_many_arguments)]
fn classify(
    toks: &[Tok],
    cat: &AgentCatalog,
    repos: &RepoIndex,
    targets: &Targets,
    target: &mut Target,
    addressed: Option<&PetRef>,
    addressed_task: bool,
    orchestrate: bool,
) -> Intent {
    if toks.is_empty() {
        return Intent::SmallTalk;
    }
    let words: Vec<&str> = toks.iter().map(|t| t.plain.as_str()).collect();
    let s = words.join(" ");
    if is_help(&s, &words) {
        return Intent::Help;
    }
    if let Some(i) = parse_resolve(toks, &words, &s, cat, targets, target) {
        return i;
    }
    if let Some(i) = parse_command(toks, cat, repos, targets, target, addressed, orchestrate) {
        return i;
    }
    // "Biscuit, run the tests" / "on the mac mini, run the tests": the addressee is the agent.
    // A status word makes it a status ask ("Biscuit, update me", "on the mac mini, check what's running").
    if addressed_task && task_like(toks) && !words.iter().any(|w| STATUS_WORDS.contains(w)) {
        let agent = addressed.map(|p| AgentRef::Known(p.agent_id.clone()));
        if let Some(p) = addressed {
            target.pet = Some(p.clone());
        }
        return Intent::Command(command_from(toks, 0, agent, false, false, repos));
    }
    // The default pet: "fix the failing test in repo api" / "run the tests on the laptop" go to the agent
    // it picks for the target machine.
    if orchestrate && is_instruction(toks) {
        return Intent::Command(command_from(toks, 0, None, false, false, repos));
    }
    if is_daily(&words, &s) {
        return Intent::DailyReport;
    }
    let mention = cat.find(toks);
    let pet = (0..toks.len()).find_map(|i| targets.pet_at(toks, i).map(|(p, len)| (p, i, len)));
    let only_agent = mention.as_ref().is_some_and(|(_, idx, len)| *idx == 0 && *len == toks.len())
        || pet.as_ref().is_some_and(|(_, idx, len)| *idx == 0 && *len == toks.len());
    let has_agent = mention.is_some()
        || pet.is_some()
        || target.machine.is_some()
        || toks.iter().any(|t| KNOWN_AGENT_WORDS.contains(&t.key.as_str()));
    if is_status(&words, &s, has_agent, only_agent) {
        let mut agent = mention_anywhere(toks, cat);
        if agent.is_none() {
            if let Some((p, _, _)) = pet {
                agent = Some(AgentRef::Known(p.agent_id.clone()));
                target.pet = Some(p);
            }
        }
        return Intent::Status { agent };
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
    use pawl_protocol::{AgentSource, SessionSource, SessionStatus};

    fn agent(id: &str, name: &str) -> Agent {
        Agent {
            id: id.into(),
            machine_id: "abcdef012345".into(),
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
            machine_id: "abcdef012345".into(),
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
        // The hub's index: here the sessions' folders (it drops the ones that no longer exist).
        let index = BTreeMap::from([(
            "abcdef012345".to_string(),
            sessions.iter().map(|s| pawl_protocol::RepoRef { name: basename(&s.cwd), path: s.cwd.clone(), last_used_at: None }).collect(),
        )]);
        (AgentCatalog::build(&agents, &sessions, |m| m == "abcdef012345"), RepoIndex::build(&sessions, &index))
    }

    fn p(text: &str) -> Intent {
        let (c, r) = setup();
        parse(text, &c, &r, &Targets::default(), Some("Biscuit")).intent
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

    fn named(name: &str, explicit: bool) -> RepoRef {
        RepoRef::Named { name: name.into(), key: name.to_lowercase(), explicit }
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
        assert_eq!(c.agent, Some(known("codex")));
        assert_eq!(c.repo, Some(named("ai-pet", true)));
        assert_eq!(c.task, "run the tests");
        assert!(!c.force_new);

        let c = cmd("Tell Claude in the web-app repo to fix the failing build, please");
        assert_eq!(c.agent, Some(known("claude-code")));
        assert_eq!(c.repo, Some(named("web-app", true)));
        assert_eq!(c.task, "fix the failing build");

        let c = cmd("have codex add a README");
        assert_eq!((c.repo, c.task.as_str()), (None, "add a README"));

        let c = cmd("codex, write tests for the parser");
        assert_eq!(c.agent, Some(known("codex")));
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
        assert_eq!(c.agent, Some(known("gemini")));

        let c = cmd("tell cursor to do stuff");
        assert_eq!(c.agent, Some(AgentRef::Unknown("Cursor".into())));

        // A prepositional phrase that is not a repo stays in the task.
        let c = cmd("tell codex to fix the bug in the parser");
        assert_eq!((c.repo, c.task.as_str()), (None, "fix the bug in the parser"));
        // Fuzzy match on explicit repo word.
        let c = cmd("tell codex to deploy in repo web");
        assert_eq!(c.repo, Some(named("web", true)));
    }

    #[test]
    fn approvals() {
        let r = |approve, always, agent, pick, explicit| Intent::Resolve { approve, always, agent, pick, explicit };
        assert_eq!(p("approve"), r(true, false, None, None, true));
        assert_eq!(p("Yes!"), r(true, false, None, None, false));
        assert_eq!(p("sounds good"), r(true, false, None, None, false));
        assert_eq!(p("ok, deny it"), r(false, false, None, None, true));
        assert_eq!(p("no"), r(false, false, None, None, false));
        assert_eq!(p("always allow codex"), r(true, true, Some(known("codex")), None, true));
        assert_eq!(p("approve #2"), r(true, false, None, Some(Pick::Nth(2)), true));
        assert_eq!(p("reject the last one"), r(false, false, None, Some(Pick::Last), true));
        assert_eq!(p("never"), r(false, true, None, None, false));
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
        let all = |_: &RepoEntry| true;
        assert_eq!(r.resolve("AI-PET", None, false, all).as_deref(), Some("E:\\workspace\\ai-pet"));
        assert_eq!(r.resolve("aipet", None, false, all).as_deref(), Some("E:\\workspace\\ai-pet"));
        assert_eq!(r.resolve("pet", None, false, all), None);
        assert_eq!(r.resolve("pet", None, true, all).as_deref(), Some("E:\\workspace\\ai-pet"));
        assert_eq!(r.resolve("ap", None, true, all), None);
        // A unique prefix of 3+ letters matches without the "repo" word.
        assert_eq!(r.resolve("web", None, false, all).as_deref(), Some("/home/me/web-app"));
        assert_eq!(r.names(5, all).len(), 3);
    }

    const MAC: &str = "zz9988776655";
    const PC: &str = "abcdef012345";

    fn pet(id: &str, name: &str, machine: &str, agent: &str) -> Pet {
        Pet {
            id: id.into(),
            machine_id: machine.into(),
            agent_id: agent.into(),
            species: pawl_protocol::Species::Dog,
            skin: "default".into(),
            name: name.into(),
            personality: String::new(),
            mood: 50,
            xp: 0,
        }
    }

    /// Two machines (this PC and a Mac mini), a pet on each, and repo indexes on both.
    fn world() -> (AgentCatalog, RepoIndex, Targets) {
        let (c, _) = setup();
        let mut mac_session = session("m1", "claude-code", "/Users/me/api");
        mac_session.machine_id = MAC.into();
        let sessions = vec![session("s1", "claude-code", "E:\\workspace\\ai-pet"), mac_session];
        let mut index = BTreeMap::new();
        let repo = |name: &str, path: &str| pawl_protocol::RepoRef { name: name.into(), path: path.into(), last_used_at: Some(1) };
        index.insert(MAC.to_string(), vec![repo("api", "/Users/me/api"), repo("web (work)", "/Users/me/work/web")]);
        index.insert(PC.to_string(), vec![repo("ai-pet", "E:\\workspace\\ai-pet")]);
        let repos = RepoIndex::build(&sessions, &index);
        let pets = [pet("p1", "Biscuit", PC, "claude-code"), pet("p2", "Rex", MAC, "codex")];
        let targets = Targets::new([(PC, "Windows PC"), (MAC, "Mac mini"), ("aa0000000000", "MacBook Pro")], pets.iter(), PC, Some("p1"));
        (c, repos, targets)
    }

    fn pt(text: &str) -> Parsed {
        let (c, r, t) = world();
        parse(text, &c, &r, &t, Some("Biscuit"))
    }

    fn on(id: &str) -> Option<MachineRef> {
        Some(MachineRef::Id(id.into()))
    }

    #[test]
    fn machine_targets_in_commands() {
        for text in [
            "Tell the Mac mini's Claude to run the tests in repo api",
            "tell claude on the mac mini to run the tests in repo api",
            "on the Mac mini, tell claude to run the tests in repo api",
            "tell the mac mini claude to run the tests in api",
            "tell claude to run the tests in repo api on the mac mini",
            "tell claude on mac mini: run the tests in repo api",
        ] {
            let p = pt(text);
            assert_eq!(p.target.machine, on(MAC), "{text}");
            let Intent::Command(c) = p.intent else { panic!("{text}: {:?}", p.intent) };
            assert_eq!(c.agent, Some(known("claude-code")), "{text}");
            assert!(matches!(c.repo, Some(RepoRef::Named { ref key, .. }) if key == "api"), "{text}: {:?}", c.repo);
            assert_eq!(c.task, "run the tests", "{text}");
        }
        // Machine only: the agent is picked later.
        let p = pt("tell the mac mini to run the tests in api");
        assert_eq!(p.target.machine, on(MAC));
        assert!(matches!(p.intent, Intent::Command(CommandIntent { agent: None, ref task, .. }) if task == "run the tests"), "{:?}", p.intent);
        // Unique prefix ("mac" is ambiguous with MacBook Pro, "mini" is not a prefix).
        let p = pt("tell the macb claude to lint");
        assert_eq!(p.target.machine, on("aa0000000000"));
        let p = pt("tell claude on the mac to lint");
        assert_eq!(p.target.machine, None, "ambiguous prefix stays text");
        // This machine.
        for text in ["tell claude to lint here", "tell claude on this machine to lint", "tell this machine's claude to lint"] {
            let p = pt(text);
            assert_eq!(p.target.machine, Some(MachineRef::Here), "{text}");
            assert!(matches!(p.intent, Intent::Command(CommandIntent { ref task, .. }) if task == "lint"), "{text}: {:?}", p.intent);
        }
        let p = pt("tell claude on windows pc to lint");
        assert_eq!(p.target.machine, on(PC));
        // Ordinary words are not machines: "here" inside the task, no machine context.
        let p = pt("tell claude to fix the bug in here");
        assert_eq!(p.target.machine, None);
        assert!(matches!(p.intent, Intent::Command(CommandIntent { ref task, .. }) if task == "fix the bug in here"), "{:?}", p.intent);
        let p = pt("tell claude to say mac mini twice");
        assert_eq!(p.target.machine, None);
        let p = pt("tell claude to work on the mac mini setup script");
        assert_eq!(p.target.machine, None);
        assert!(matches!(p.intent, Intent::Command(CommandIntent { ref task, .. }) if task == "work on the mac mini setup script"), "{:?}", p.intent);
        assert_eq!(pt("is the mac mini busy?").target.machine, on(MAC));
        // Words of the task stay in the task.
        for (text, task) in [("tell claude to wipe this machine", "wipe this machine"), ("tell claude to run the build locally", "run the build locally")] {
            let p = pt(text);
            assert_eq!(p.target.machine, None, "{text}");
            assert!(matches!(p.intent, Intent::Command(CommandIntent { task: ref t, .. }) if t == task), "{text}: {:?}", p.intent);
        }
        // Quoted text is never a target.
        let p = pt("tell claude to run \"ssh mac mini\"");
        assert_eq!(p.target.machine, None);
    }

    #[test]
    fn pet_targets() {
        let p = pt("tell Rex to run the tests in api");
        assert_eq!(p.target.pet.as_ref().map(|p| p.machine_id.as_str()), Some(MAC));
        assert!(matches!(p.intent, Intent::Command(CommandIntent { agent: Some(AgentRef::Known(ref a)), ref task, .. }) if a == "codex" && task == "run the tests"), "{:?}", p.intent);
        let p = pt("Rex, run the tests in api");
        assert_eq!(p.target.pet.as_ref().map(|p| p.name.as_str()), Some("Rex"));
        assert!(matches!(p.intent, Intent::Command(CommandIntent { agent: Some(AgentRef::Known(ref a)), ref task, .. }) if a == "codex" && task == "run the tests"), "{:?}", p.intent);
        // The chatted pet addressed by name, then an instruction.
        let p = pt("Biscuit, fix the build in ai-pet");
        assert_eq!(p.target.pet.as_ref().map(|p| p.agent_id.as_str()), Some("claude-code"));
        assert!(matches!(p.intent, Intent::Command(CommandIntent { ref task, .. }) if task == "fix the build"), "{:?}", p.intent);
        let p = pt("Rex, start a new session in api");
        assert!(matches!(p.intent, Intent::Command(CommandIntent { force_new: true, ref task, .. }) if task.is_empty()), "{:?}", p.intent);
        // Addressing the chatted pet is not a target for questions, small talk or approvals.
        assert_eq!(pt("Biscuit, what is codex up to?").target, Target::default());
        assert_eq!(pt("Biscuit, how are you?").intent, Intent::SmallTalk);
        assert_eq!(pt("Rex, good boy!").intent, Intent::SmallTalk);
        let p = pt("Biscuit, approve");
        assert_eq!(
            (p.intent, p.target),
            (Intent::Resolve { approve: true, always: false, agent: None, pick: None, explicit: true }, Target::default())
        );
        // Status of a pet's agent.
        let p = pt("what's Rex doing?");
        assert_eq!(p.intent, Intent::Status { agent: Some(known("codex")) });
        assert_eq!(p.target.pet.map(|p| p.machine_id), Some(MAC.to_string()));
    }

    #[test]
    fn status_questions_with_targets() {
        let p = pt("what is the Mac mini's Claude doing?");
        assert_eq!((p.intent, p.target.machine), (Intent::Status { agent: Some(known("claude-code")) }, on(MAC)));
        let p = pt("what is the mac mini doing?");
        assert_eq!((p.intent, p.target.machine), (Intent::Status { agent: None }, on(MAC)));
        let p = pt("what's running on the mac mini");
        assert_eq!((p.intent, p.target.machine), (Intent::Status { agent: None }, on(MAC)));
        let p = pt("how is claude doing on windows pc?");
        assert_eq!((p.intent, p.target.machine), (Intent::Status { agent: Some(known("claude-code")) }, on(PC)));
        let p = pt("approve the mac mini's request");
        assert!(matches!(p.intent, Intent::Resolve { approve: true, .. }));
        assert_eq!(p.target.machine, on(MAC));
    }

    #[test]
    fn local_agent_wins_alias_ties() {
        // A paired machine's "Claude" (listed first, seen more recently) and this machine's Claude Code.
        let mut remote = agent("claude-acp", "Claude");
        remote.machine_id = MAC.into();
        let mut s = session("m1", "claude-acp", "/Users/me/api");
        s.machine_id = MAC.into();
        s.last_event_at = 99;
        let cat = AgentCatalog::build(&[remote, agent("claude-code", "Claude Code")], &[s], |m| m == PC);
        assert_eq!(cat.match_at(&tokenize("claude"), 0).map(|(id, _)| id).as_deref(), Some("claude-code"));
        // Equal machine names: this machine.
        let t = Targets::new([(MAC, "Studio"), (PC, "Studio")], [], PC, None);
        let mut toks = tokenize("tell claude on studio to lint");
        assert_eq!(take_machine(&mut toks, &t, &cat).map(|(m, _)| m), Some(MachineRef::Id(PC.into())));
        // A machine named like an agent is not taken for one.
        let t = Targets::new([(MAC, "Claude"), (PC, "Windows PC")], [], PC, None);
        let p = parse("tell claude to lint", &cat, &RepoIndex::default(), &t, None);
        assert_eq!(p.target.machine, None);
        assert!(matches!(p.intent, Intent::Command(CommandIntent { agent: Some(AgentRef::Known(ref a)), .. }) if a == "claude-code"));
        // A machine named "here" can't be named at all.
        let t = Targets::new([(MAC, "here")], [], PC, None);
        assert_eq!(parse("tell claude to lint here", &cat, &RepoIndex::default(), &t, None).target.machine, Some(MachineRef::Here));
    }

    #[test]
    fn repo_index_spans_machines() {
        let (_, r, _) = world();
        assert!(r.known("web", false), "index name \"web (work)\" matches its folder");
        assert_eq!(r.folders_on(MAC, "web", false), vec![("/Users/me/work/web".to_string(), "web (work)".to_string())]);
        // The index entry wins over the session cwd for the same folder.
        assert_eq!(r.folders_on(MAC, "api", false), vec![("/Users/me/api".to_string(), "api".to_string())]);
        assert!(r.folders_on(PC, "api", false).is_empty());
        assert_eq!(r.names(5, |e| e.machine_id == MAC), vec!["api".to_string(), "web (work)".to_string()]);
        assert!(r.folder_by_key(MAC, &path_key("/Users/me/api")).is_some());
    }

    #[test]
    fn default_pet_takes_plain_instructions_as_agentless_commands() {
        let (c, r, t) = world();
        let dp = |text: &str| parse(text, &c, &r, &t, None);
        // The default pet (no pet chatted with): a task verb first, no agent named.
        let p = dp("fix the failing test in repo api");
        assert_eq!(
            p.intent,
            Intent::Command(CommandIntent { agent: None, repo: Some(named("api", true)), task: "fix the failing test".into(), force_new: false })
        );
        let p = dp("please run the tests on the Mac mini");
        assert_eq!(p.target.machine, on(MAC));
        assert!(matches!(p.intent, Intent::Command(CommandIntent { agent: None, ref task, .. }) if task == "run the tests"), "{p:?}");
        let p = dp("can you refactor the parser");
        assert!(matches!(p.intent, Intent::Command(CommandIntent { agent: None, ref task, .. }) if task == "refactor the parser"), "{p:?}");
        let p = dp("start a new session in repo api");
        assert!(matches!(p.intent, Intent::Command(CommandIntent { agent: None, force_new: true, .. })), "{p:?}");
        // Named agents and pets still work as before.
        assert!(matches!(dp("tell codex to fix the build").intent, Intent::Command(CommandIntent { agent: Some(_), .. })));
        let p = dp("tell Rex to also run lint");
        assert_eq!(p.target.pet.as_ref().map(|p| p.name.as_str()), Some("Rex"));
        assert!(matches!(p.intent, Intent::Command(CommandIntent { ref task, .. }) if task == "also run lint"), "{p:?}");
        // Not instructions: one word, questions, small talk, statements, everyday verbs.
        for text in [
            "test", "fix?", "the build is broken", "play fetch with me", "make me a sandwich", "continue with the plan",
            "find my keys", "look at this", "try again later", "open the window", "new idea", "clean the kitchen",
            "update the calendar", "handle it", "move on",
        ] {
            assert!(!matches!(dp(text).intent, Intent::Command(_)), "{text}: {:?}", dp(text).intent);
        }
        // Status words win over a verb: these are status asks.
        for text in [
            "update me", "check what's running", "look at the pending sessions", "run status", "test what's running",
            "check on codex", "check the status", "what's everyone doing?", "review the progress",
        ] {
            assert!(matches!(dp(text).intent, Intent::Status { .. }), "{text}: {:?}", dp(text).intent);
        }
        assert!(matches!(dp("review what happened today").intent, Intent::DailyReport), "{:?}", dp("review what happened today").intent);
        // The same for an addressed pet or machine ("Biscuit, update me" is not an instruction for its agent).
        for text in ["Biscuit, update me", "Rex, check what's running", "on the mac mini, look at the pending sessions"] {
            assert!(matches!(pt(text).intent, Intent::Status { .. }), "{text}: {:?}", pt(text).intent);
        }
        // A pet's own chat keeps today's rules: a bare instruction there is not a command.
        assert_eq!(parse("fix the failing test in repo api", &c, &r, &t, Some("Biscuit")).intent, Intent::SmallTalk);
    }
}
