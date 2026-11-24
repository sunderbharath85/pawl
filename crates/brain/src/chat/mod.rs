//! Chat bot brain.
//!
//! [`Brain::handle`] parses the message with a deterministic, rule-based intent parser
//! (status, commands, approve/deny, daily report, help). Only that parser produces
//! [`BrainAction`]s. An optional LLM (Anthropic Messages API or Ollama) words summaries and
//! small talk in the pet's voice; with no model configured, or when a call fails, replies
//! fall back to deterministic text.

mod intent;
mod llm;
mod summary;
mod voice;

use std::cmp::Reverse;
use std::collections::HashSet;
use std::fmt;

use krypto_protocol::{
    now_ms, Agent, AgentId, HubEvent, PermissionOptionKind, PermissionRequest, Pet, RequestId, Session, SessionId,
    SessionSource, SessionStatus, UnixMs,
};
use serde::{Deserialize, Serialize};

use crate::growth;
use crate::util::{agent_handle, agent_name, ago, basename, fnv1a, path_key, plural, truncate};
use intent::{AgentCatalog, AgentRef, CommandIntent, Intent, Pick, RepoIndex, RepoRef, Talk};
use llm::Llm;
use voice::Voice;

/// Default Anthropic model for chatter and summaries.
pub const DEFAULT_MODEL: &str = "claude-haiku-5-5";

const SUMMARY_MAX_TOKENS: u32 = 220;
const CHAT_MAX_TOKENS: u32 = 180;
const DIGEST_LINES: usize = 30;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrainConfig {
    /// Enables the Anthropic Messages API path. Never logged.
    pub anthropic_api_key: Option<String>,
    /// Anthropic model id (default "claude-haiku-5-5").
    pub model: String,
    /// Ollama base URL, e.g. "http://127.0.0.1:11434". Used when `ollama_model` is set too.
    pub ollama_url: Option<String>,
    pub ollama_model: Option<String>,
}

impl Default for BrainConfig {
    fn default() -> Self {
        Self { anthropic_api_key: None, model: DEFAULT_MODEL.to_string(), ollama_url: None, ollama_model: None }
    }
}

impl fmt::Debug for BrainConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrainConfig")
            .field("anthropic_api_key", &self.anthropic_api_key.as_ref().map(|_| "<redacted>"))
            .field("model", &self.model)
            .field("ollama_url", &self.ollama_url)
            .field("ollama_model", &self.ollama_model)
            .finish()
    }
}

impl BrainConfig {
    /// Read `ANTHROPIC_API_KEY`, `KRYPTO_HUB_BRAIN_MODEL`, `KRYPTO_HUB_OLLAMA_URL`, `KRYPTO_HUB_OLLAMA_MODEL`.
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        Self {
            anthropic_api_key: var("ANTHROPIC_API_KEY"),
            model: var("KRYPTO_HUB_BRAIN_MODEL").unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            ollama_url: var("KRYPTO_HUB_OLLAMA_URL"),
            ollama_model: var("KRYPTO_HUB_OLLAMA_MODEL"),
        }
    }

    /// True when some model is configured (Anthropic key, or Ollama URL + model).
    pub fn has_model(&self) -> bool {
        let set = |o: &Option<String>| o.as_deref().is_some_and(|s| !s.trim().is_empty());
        set(&self.anthropic_api_key) || (set(&self.ollama_url) && set(&self.ollama_model))
    }
}

#[derive(Debug, Clone, Default)]
pub struct BrainContext {
    pub agents: Vec<Agent>,
    pub sessions: Vec<Session>,
    pub recent_events: Vec<HubEvent>,
    pub pending: Vec<PermissionRequest>,
    pub pet: Option<Pet>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum BrainAction {
    /// Start a session. `prompt` may be empty ("start codex in repo X"): start without a turn.
    StartSession { agent_id: AgentId, cwd: String, prompt: String },
    Prompt { session_id: SessionId, text: String },
    /// `option_id: None` = cancelled outcome.
    ResolvePermission { request_id: RequestId, option_id: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrainReply {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<BrainAction>,
}

fn say(text: impl Into<String>) -> BrainReply {
    BrainReply { text: text.into(), action: None }
}

#[derive(Clone)]
pub struct Brain {
    cfg: BrainConfig,
    llm: Option<Llm>,
}

impl fmt::Debug for Brain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Brain").field("cfg", &self.cfg).field("llm", &self.llm).finish()
    }
}

fn status_rank(s: SessionStatus) -> u8 {
    match s {
        SessionStatus::WaitingPermission => 0,
        SessionStatus::Working => 1,
        SessionStatus::Thinking => 2,
        SessionStatus::Error => 3,
        SessionStatus::Done => 4,
        SessionStatus::Idle => 5,
    }
}

fn is_busy(s: SessionStatus) -> bool {
    matches!(s, SessionStatus::Thinking | SessionStatus::Working | SessionStatus::WaitingPermission)
}

fn status_word(s: SessionStatus) -> &'static str {
    match s {
        SessionStatus::Idle => "idle",
        SessionStatus::Thinking => "thinking",
        SessionStatus::Working => "working",
        SessionStatus::WaitingPermission => "waiting for your OK",
        SessionStatus::Done => "done",
        SessionStatus::Error => "error",
    }
}

/// Distinct cwds, most recent session first.
fn distinct_cwds<'a>(sessions: impl Iterator<Item = &'a Session>) -> Vec<String> {
    let mut v: Vec<&Session> = sessions.collect();
    v.sort_by_key(|s| Reverse(s.last_event_at));
    let mut seen = HashSet::new();
    v.into_iter().filter(|s| seen.insert(path_key(&s.cwd))).map(|s| s.cwd.clone()).collect()
}

impl Brain {
    pub fn new(cfg: BrainConfig) -> Self {
        let llm = Llm::from_config(&cfg);
        match &llm {
            Some(l) => tracing::info!(providers = %l.describe(), "brain: language model enabled"),
            None => tracing::info!("brain: no language model configured; deterministic replies"),
        }
        Self { cfg, llm }
    }

    pub fn config(&self) -> &BrainConfig {
        &self.cfg
    }

    /// True when replies may be worded by an LLM.
    pub fn has_llm(&self) -> bool {
        self.llm.is_some()
    }

    /// Rule-based intent parse first ("what's claude doing", "tell codex to run the tests in repo X",
    /// "approve"/"deny"), LLM for summaries/small talk, deterministic fallback when no model configured.
    /// Actions come only from the deterministic parser.
    pub async fn handle(&self, text: &str, ctx: &BrainContext) -> anyhow::Result<BrainReply> {
        let voice = Voice::for_pet(ctx.pet.as_ref());
        let seed = fnv1a(text);
        let catalog = AgentCatalog::build(&ctx.agents, &ctx.sessions);
        let repos = RepoIndex::build(&ctx.sessions);
        let intent = intent::parse(text, &catalog, &repos, ctx.pet.as_ref().map(|p| p.name.as_str()));
        tracing::debug!(?intent, "brain: parsed intent");
        let now = now_ms();
        let reply = match intent {
            Intent::Help => say(help_text(&voice, seed, &repos)),
            Intent::Resolve { approve, always, agent, pick } => resolve_request(ctx, &voice, seed, approve, always, agent, pick),
            Intent::Command(cmd) => command(ctx, &voice, seed, &repos, cmd),
            Intent::Status { agent } => self.status(ctx, &voice, seed, agent, now).await,
            Intent::DailyReport => {
                say(self.daily_inner(&ctx.sessions, &ctx.recent_events, &ctx.agents, &voice, seed, now).await)
            }
            Intent::SmallTalk => say(self.small_talk(text, ctx, &voice, seed, now).await),
        };
        Ok(reply)
    }

    /// Short summary of one session (LLM-worded if configured, else deterministic).
    pub async fn summarize_session(&self, session: &Session, events: &[HubEvent]) -> anyhow::Result<String> {
        Ok(self.summarize_inner(session, events, &[], &[], &Voice::default(), false, now_ms()).await)
    }

    /// End-of-day report of sessions active in the last 24 h.
    pub async fn daily_report(&self, sessions: &[Session], events: &[HubEvent]) -> anyhow::Result<String> {
        let voice = Voice::default();
        Ok(self.daily_inner(sessions, events, &[], &voice, fnv1a("daily"), now_ms()).await)
    }

    #[allow(clippy::too_many_arguments)]
    async fn summarize_inner(
        &self,
        session: &Session,
        events: &[HubEvent],
        agents: &[Agent],
        pending: &[PermissionRequest],
        voice: &Voice,
        decorate: bool,
        now: UnixMs,
    ) -> String {
        let evs = summary::session_events(&session.id, events);
        let facts = summary::facts(&evs);
        let det = summary::deterministic_summary(session, &facts, agents, pending, now);
        if let Some(llm) = &self.llm {
            let digest = summary::event_digest(&evs, DIGEST_LINES);
            let agent = agent_name(&session.agent_id, agents);
            let user = format!(
                "<context>\nFacts: {det}\nRecent events (oldest first):\n{}\n</context>\n\n\
                 In 1-2 short sentences, tell the user what {agent} is doing right now. \
                 Mention it if it needs their approval.",
                if digest.is_empty() { "(none)".to_string() } else { digest.iter().map(|l| format!("- {l}")).collect::<Vec<_>>().join("\n") },
            );
            match llm.complete(&voice.system_prompt(), &user, SUMMARY_MAX_TOKENS).await {
                Ok(t) => return t,
                Err(e) => tracing::debug!(error = %e, "brain: summary falls back to deterministic text"),
            }
        }
        if decorate {
            let seed = fnv1a(&session.id);
            let action = if session.ended_at.is_some() { voice.sleepy(seed) } else { voice.for_status(session.status, seed) };
            format!("{action} {det}")
        } else {
            det
        }
    }

    async fn daily_inner(
        &self,
        sessions: &[Session],
        events: &[HubEvent],
        agents: &[Agent],
        voice: &Voice,
        seed: u64,
        now: UnixMs,
    ) -> String {
        if let Some(llm) = &self.llm {
            let plain = summary::daily_report_text(sessions, events, agents, None, now);
            if !plain.starts_with("Quiet day") {
                let user = format!(
                    "<context>\n{plain}\n</context>\n\nWrite a 1-2 sentence, in-character wrap-up of today's agent work: \
                     highlights and anything that still needs the user. Do not list every session."
                );
                match llm.complete(&voice.system_prompt(), &user, CHAT_MAX_TOKENS).await {
                    Ok(intro) => return format!("{intro}\n\n{plain}"),
                    Err(e) => tracing::debug!(error = %e, "brain: daily report falls back to deterministic text"),
                }
            }
        }
        summary::daily_report_text(sessions, events, agents, Some((voice, seed)), now)
    }

    async fn status(&self, ctx: &BrainContext, voice: &Voice, seed: u64, agent: Option<AgentRef>, now: UnixMs) -> BrainReply {
        match agent {
            Some(AgentRef::Known(id)) => say(self.agent_status(ctx, voice, seed, &id, now).await),
            Some(AgentRef::Unknown(name)) => say(format!(
                "{} I haven't seen {name} around. {}",
                voice.thinking(seed),
                overview(ctx, voice, seed, now, false)
            )),
            None => say(overview(ctx, voice, seed, now, true)),
        }
    }

    async fn agent_status(&self, ctx: &BrainContext, voice: &Voice, seed: u64, id: &str, now: UnixMs) -> String {
        let name = agent_name(id, &ctx.agents);
        let mut live: Vec<&Session> = ctx.sessions.iter().filter(|s| s.agent_id == id && s.ended_at.is_none()).collect();
        live.sort_by_key(|s| (status_rank(s.status), Reverse(s.last_event_at)));
        let Some(primary) = live.first().copied() else {
            return match ctx.sessions.iter().filter(|s| s.agent_id == id).max_by_key(|s| s.last_event_at) {
                Some(last) => format!(
                    "{} {name} is resting. Last seen in {} {}.",
                    voice.sleepy(seed),
                    basename(&last.cwd),
                    ago(last.last_event_at, now)
                ),
                None => format!(
                    "{} I haven't seen {name} yet. Say “tell {} to … in repo <name>” to put it to work.",
                    voice.sound(seed),
                    agent_handle(id, &ctx.agents)
                ),
            };
        };
        let mut text =
            self.summarize_inner(primary, &ctx.recent_events, &ctx.agents, &ctx.pending, voice, true, now).await;
        if live.len() > 1 {
            let others: Vec<String> = live[1..]
                .iter()
                .take(4)
                .map(|s| format!("{} ({})", basename(&s.cwd), status_word(s.status)))
                .collect();
            text.push_str(&format!(" Also: {}.", others.join(", ")));
        }
        let mut waiting: Vec<&PermissionRequest> = ctx.pending.iter().filter(|r| r.agent_id == id).collect();
        waiting.sort_by_key(|r| r.created_at);
        match waiting.len() {
            0 => {}
            1 => text.push_str(&format!(
                " Needs your OK: {}. Say “approve” or “deny”.",
                summary::tool_label(&waiting[0].tool_call)
            )),
            n => text.push_str(&format!(" {n} requests need your OK. Say “approve {}” to pick one.", agent_handle(id, &ctx.agents))),
        }
        text
    }

    async fn small_talk(&self, text: &str, ctx: &BrainContext, voice: &Voice, seed: u64, now: UnixMs) -> String {
        if let Some(llm) = &self.llm {
            let brief = overview_plain(ctx, now);
            let user = format!(
                "<context>\n{brief}\nPending approvals: {}\nYour mood: {}/100 ({}), level {}.\n</context>\n\nThe user says: {}",
                ctx.pending.len(),
                voice.mood,
                growth::mood_label(voice.mood),
                growth::level(voice.xp),
                truncate(text, 1_000),
            );
            match llm.complete(&voice.system_prompt(), &user, CHAT_MAX_TOKENS).await {
                Ok(t) => return t,
                Err(e) => tracing::debug!(error = %e, "brain: small talk falls back to canned reply"),
            }
        }
        canned_small_talk(text, ctx, voice, seed, now)
    }
}

// ---------- deterministic replies ----------

fn help_text(voice: &Voice, seed: u64, repos: &RepoIndex) -> String {
    let repo = repos.names(1).into_iter().next().unwrap_or_else(|| "my-app".into());
    format!(
        "{} Here's what I can do:\n\
         • “what's Claude doing?” for a live status\n\
         • “tell codex to run the tests in repo {repo}” to start or prompt a session\n\
         • “approve” / “deny” (add “always” to remember) for a pending request\n\
         • “daily report” for today's recap",
        voice.sound(seed)
    )
}

/// Overview of live sessions. `full` lists them; otherwise one line.
fn overview(ctx: &BrainContext, voice: &Voice, seed: u64, now: UnixMs, full: bool) -> String {
    let mut live: Vec<&Session> = ctx.sessions.iter().filter(|s| s.ended_at.is_none()).collect();
    live.sort_by_key(|s| (status_rank(s.status), Reverse(s.last_event_at)));
    if live.is_empty() {
        let mut t = format!("{} All quiet: no agents are working right now.", voice.sleepy(seed));
        if let Some(last) = ctx.sessions.iter().max_by_key(|s| s.last_event_at) {
            t.push_str(&format!(
                " Last activity: {} in {} {}.",
                agent_name(&last.agent_id, &ctx.agents),
                basename(&last.cwd),
                ago(last.last_event_at, now)
            ));
        }
        return t;
    }
    let busy = live.iter().filter(|s| is_busy(s.status)).count();
    let pending_line = match ctx.pending.len() {
        0 => String::new(),
        1 => " 1 request needs your OK: say “approve” or “deny”.".to_string(),
        n => format!(" {n} requests need your OK: say “approve 1”, “deny 2”…"),
    };
    if !full {
        return format!("{} open, {} busy.{pending_line}", plural(live.len(), "session"), busy);
    }
    let lead = if ctx.pending.is_empty() { voice.sound(seed) } else { voice.alert(seed) };
    let mut t = format!("{lead} {} open, {} busy:", plural(live.len(), "session"), busy);
    for s in live.iter().take(6) {
        let evs = summary::session_events(&s.id, &ctx.recent_events);
        let f = summary::facts(&evs);
        t.push_str(&format!(
            "\n• {} · {}: {}",
            agent_name(&s.agent_id, &ctx.agents),
            basename(&s.cwd),
            summary::short_status(s, &f, &ctx.pending, now)
        ));
    }
    if live.len() > 6 {
        t.push_str(&format!("\n• …and {} more", live.len() - 6));
    }
    if !pending_line.is_empty() {
        t.push('\n');
        t.push_str(pending_line.trim_start());
    }
    t
}

/// Plain fact sheet for the LLM.
fn overview_plain(ctx: &BrainContext, now: UnixMs) -> String {
    let mut live: Vec<&Session> = ctx.sessions.iter().filter(|s| s.ended_at.is_none()).collect();
    live.sort_by_key(|s| (status_rank(s.status), Reverse(s.last_event_at)));
    if live.is_empty() {
        return "No agent sessions are active.".into();
    }
    live.iter()
        .take(8)
        .map(|s| {
            let evs = summary::session_events(&s.id, &ctx.recent_events);
            let f = summary::facts(&evs);
            format!(
                "- {} in {}: {}",
                agent_name(&s.agent_id, &ctx.agents),
                basename(&s.cwd),
                summary::short_status(s, &f, &ctx.pending, now)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn canned_small_talk(text: &str, ctx: &BrainContext, voice: &Voice, seed: u64, now: UnixMs) -> String {
    let quick = overview(ctx, voice, seed, now, false);
    match intent::talk_kind(text) {
        Talk::Greeting => {
            let who = voice.name.as_deref().map(|n| format!(" {n} here.")).unwrap_or_default();
            format!("{} Hi!{who} {} {}", voice.sound(seed), voice.happy(seed), strip_lead(&quick))
        }
        Talk::Praise => format!("{} {} Best human ever!", voice.happy(seed), voice.sound(seed)),
        Talk::Thanks => format!("Anytime! {}", voice.happy(seed)),
        Talk::Feeling => {
            let mood = match voice.mood {
                80..=u8::MAX => format!("Feeling fantastic! {}", voice.happy(seed)),
                50..=79 => format!("Pretty good! {}", voice.sound(seed)),
                25..=49 => format!("A bit down… {} Finished turns cheer me up.", voice.sad(seed)),
                _ => format!("Not great. {} Too many errors lately.", voice.sad(seed)),
            };
            format!("{mood} {}", strip_lead(&quick))
        }
        Talk::Play => format!("{} {} Yes please! (I'll keep one eye on your agents.)", voice.happy(seed), voice.sound(seed)),
        Talk::Bye => format!("{} Bye! I'll keep watch and nudge you if anyone needs you.", voice.sleepy(seed)),
        Talk::Other => format!(
            "{} Not sure what you mean. Try “what's Claude doing?”, “tell codex to run the tests in repo X”, “approve”, or “daily report”.",
            voice.thinking(seed)
        ),
    }
}

/// Drop the leading sound/action of an overview line ("*naps* All quiet…" -> "All quiet…").
fn strip_lead(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix('*') {
        if let Some(i) = rest.find("* ") {
            return &rest[i + 2..];
        }
    }
    s
}

fn pending_list(pending: &[&PermissionRequest], ctx: &BrainContext) -> String {
    pending
        .iter()
        .enumerate()
        .take(6)
        .map(|(i, r)| {
            let repo = ctx
                .sessions
                .iter()
                .find(|s| s.id == r.session_id)
                .map(|s| format!(" ({})", basename(&s.cwd)))
                .unwrap_or_default();
            format!("\n{}. {}: {}{repo}", i + 1, agent_name(&r.agent_id, &ctx.agents), summary::tool_label(&r.tool_call))
        })
        .collect()
}

fn find_option(req: &PermissionRequest, kind: PermissionOptionKind) -> Option<String> {
    req.options.iter().find(|o| o.kind == kind).map(|o| o.option_id.clone())
}

fn resolve_request(
    ctx: &BrainContext,
    voice: &Voice,
    seed: u64,
    approve: bool,
    always: bool,
    agent: Option<AgentRef>,
    pick: Option<Pick>,
) -> BrainReply {
    let verb = if approve { "approve" } else { "deny" };
    let mut pending: Vec<&PermissionRequest> = ctx.pending.iter().collect();
    pending.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.request_id.cmp(&b.request_id)));
    if pending.is_empty() {
        return say(format!("{} Nothing is waiting for a yes or no right now.", voice.sound(seed)));
    }
    let chosen: &PermissionRequest = if let Some(pick) = pick {
        let idx = match pick {
            Pick::Nth(n) => n - 1,
            Pick::Last => pending.len() - 1,
        };
        match pending.get(idx) {
            Some(r) => r,
            None => {
                return say(format!(
                    "{} There's no request #{}. Waiting:{}",
                    voice.thinking(seed),
                    idx + 1,
                    pending_list(&pending, ctx)
                ))
            }
        }
    } else {
        let filtered: Vec<&PermissionRequest> = match &agent {
            Some(AgentRef::Known(id)) => pending.iter().copied().filter(|r| &r.agent_id == id).collect(),
            Some(AgentRef::Unknown(_)) => Vec::new(),
            None => pending.clone(),
        };
        match filtered.len() {
            1 => filtered[0],
            0 => {
                let who = match &agent {
                    Some(AgentRef::Known(id)) => agent_name(id, &ctx.agents),
                    Some(AgentRef::Unknown(n)) => n.clone(),
                    None => "Nobody".into(),
                };
                return say(format!(
                    "{} {who} isn't waiting on anything. Waiting:{}",
                    voice.thinking(seed),
                    pending_list(&pending, ctx)
                ));
            }
            _ => {
                let example = match &agent {
                    Some(AgentRef::Known(_)) => format!("“{verb} 1”"),
                    _ => format!("“{verb} 1” or “{verb} {}”", agent_handle(&filtered[0].agent_id, &ctx.agents)),
                };
                return say(format!(
                    "{} More than one request is waiting. Which one?{}\nSay {example}.",
                    voice.alert(seed),
                    pending_list(&pending, ctx)
                ));
            }
        }
    };
    let name = agent_name(&chosen.agent_id, &ctx.agents);
    let what = summary::tool_label(&chosen.tool_call);
    let request_id = chosen.request_id.clone();
    if approve {
        let (option, note) = if always {
            match find_option(chosen, PermissionOptionKind::AllowAlways) {
                Some(o) => (Some(o), ""),
                None => (find_option(chosen, PermissionOptionKind::AllowOnce), " (“always” wasn't offered, so just this once)"),
            }
        } else {
            match find_option(chosen, PermissionOptionKind::AllowOnce) {
                Some(o) => (Some(o), ""),
                None => (find_option(chosen, PermissionOptionKind::AllowAlways), " (only “always allow” was offered)"),
            }
        };
        let Some(option_id) = option else {
            return say(format!("{} {name}'s request has no allow option, so I can't approve it.", voice.sad(seed)));
        };
        let lead = if always && note.is_empty() { "Approved for good" } else { "Approved" };
        BrainReply {
            text: format!("{} {lead}! {name} can go ahead: {what}{note}. {}", voice.sound(seed), voice.happy(seed)),
            action: Some(BrainAction::ResolvePermission { request_id, option_id: Some(option_id) }),
        }
    } else {
        let option = if always {
            find_option(chosen, PermissionOptionKind::RejectAlways).or_else(|| find_option(chosen, PermissionOptionKind::RejectOnce))
        } else {
            find_option(chosen, PermissionOptionKind::RejectOnce)
        };
        let text = match &option {
            Some(_) if always => format!("{} Denied for good: {name} won't do “{what}”.", voice.alert(seed)),
            Some(_) => format!("{} Denied: {name} won't do “{what}”.", voice.alert(seed)),
            None => format!("{} Denied. {name} had no reject option, so I cancelled “{what}”.", voice.alert(seed)),
        };
        BrainReply { text, action: Some(BrainAction::ResolvePermission { request_id, option_id: option }) }
    }
}

fn default_cwd(ctx: &BrainContext, agent_id: &str) -> Option<String> {
    let sets: [Vec<String>; 3] = [
        distinct_cwds(ctx.sessions.iter().filter(|s| s.agent_id == agent_id && s.ended_at.is_none())),
        distinct_cwds(ctx.sessions.iter().filter(|s| s.agent_id == agent_id)),
        distinct_cwds(ctx.sessions.iter().filter(|s| s.ended_at.is_none())),
    ];
    for set in sets {
        match set.len() {
            0 => continue,
            1 => return set.into_iter().next(),
            _ => return None,
        }
    }
    None
}

fn command(ctx: &BrainContext, voice: &Voice, seed: u64, repos: &RepoIndex, cmd: CommandIntent) -> BrainReply {
    let known_names = || {
        let names: Vec<String> = ctx.agents.iter().filter(|a| a.installed).map(|a| a.name.clone()).take(6).collect();
        if names.is_empty() {
            String::new()
        } else {
            format!(" I know: {}.", names.join(", "))
        }
    };
    let agent_id = match cmd.agent {
        AgentRef::Known(id) => id,
        AgentRef::Unknown(name) => {
            return say(format!("{} I don't see {name} among your agents.{}", voice.thinking(seed), known_names()))
        }
    };
    let name = agent_name(&agent_id, &ctx.agents);
    let handle = agent_handle(&agent_id, &ctx.agents);
    let Some(agent) = ctx.agents.iter().find(|a| a.id == agent_id) else {
        return say(format!(
            "{} I only see {name} through its hooks, so I can't drive it from here. Ask it in its own app!",
            voice.sad(seed)
        ));
    };
    if !agent.installed {
        return say(format!("{} {name} isn't installed yet, so I can't start it.{}", voice.sad(seed), known_names()));
    }
    let task = cmd.task.trim().to_string();
    let example_task = if task.is_empty() { "run the tests".to_string() } else { truncate(&task, 40) };
    let cwd = match cmd.repo {
        Some(RepoRef::Path(p)) => p,
        Some(RepoRef::Known { cwd, .. }) => cwd,
        Some(RepoRef::Unknown { name: repo }) => {
            let known = repos.names(5);
            let hint = if known.is_empty() {
                " Give me a path instead, like “in C:\\code\\app”.".to_string()
            } else {
                format!(" I know: {}. Or give me a path.", known.join(", "))
            };
            return say(format!("{} I don't know a repo called “{repo}”.{hint}", voice.thinking(seed)));
        }
        None => match default_cwd(ctx, &agent_id) {
            Some(c) => c,
            None => {
                let known = repos.names(5);
                let hint = if known.is_empty() { String::new() } else { format!(" I know: {}.", known.join(", ")) };
                return say(format!(
                    "{} Which repo should {name} work in? Say “tell {handle} to {example_task} in repo <name>” or give a path.{hint}",
                    voice.thinking(seed)
                ));
            }
        },
    };
    let repo = basename(&cwd).to_string();
    let key = path_key(&cwd);
    if !cmd.force_new {
        let existing = ctx
            .sessions
            .iter()
            .filter(|s| {
                s.agent_id == agent_id
                    && s.source == SessionSource::Owned
                    && s.ended_at.is_none()
                    && s.status != SessionStatus::Error
                    && path_key(&s.cwd) == key
            })
            .max_by_key(|s| s.last_event_at);
        if let Some(s) = existing {
            if task.is_empty() {
                return say(format!("{} What should I tell {name} in {repo}?", voice.thinking(seed)));
            }
            if is_busy(s.status) {
                return say(format!(
                    "{} {name} is busy in {repo} right now ({}). Wait for it to finish, or say “start a new {handle} session in {repo} to {example_task}”.",
                    voice.alert(seed),
                    status_word(s.status)
                ));
            }
            return BrainReply {
                text: format!("{} Telling {name} in {repo}: “{}” {}", voice.sound(seed), truncate(&task, 120), voice.working(seed)),
                action: Some(BrainAction::Prompt { session_id: s.id.clone(), text: task }),
            };
        }
        if task.is_empty() {
            return say(format!("{} What should {name} do in {repo}?", voice.thinking(seed)));
        }
    }
    let text = if task.is_empty() {
        format!("{} Starting {name} in {repo}. {}", voice.sound(seed), voice.happy(seed))
    } else {
        format!("{} Fetching {name}! New session in {repo}: “{}” {}", voice.sound(seed), truncate(&task, 120), voice.happy(seed))
    };
    BrainReply { text, action: Some(BrainAction::StartSession { agent_id, cwd, prompt: task }) }
}

#[cfg(test)]
mod tests;
