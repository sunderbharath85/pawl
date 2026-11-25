//! Commands for another machine's agent (migration step 5).
//!
//! The bot never runs anything on another machine by itself. For a remote target it replies with
//! [`ChatProposal`]s; the user confirms one in a client, which sends the ordinary `start_session` /
//! `prompt` ClientMsg (validated by both hubs as usual). Proposals are built here only, from the
//! deterministic parse of the user's own message; local targets keep using [`super::BrainAction`], except a
//! command that named no agent, which is a proposal for this machine too ([`summary`] is shared).

use std::cmp::Reverse;
use std::collections::BTreeSet;

use pawl_protocol::{Agent, AgentId, ChatProposal, ChatProposalKind, MachineId, Session, SessionSource, SessionStatus, UnixMs};

use super::intent::{AgentRef, CommandIntent, Folder, MachineRef, RepoIndex, RepoRef, Target};
use super::voice::Voice;
use super::{is_local, say, BrainContext, BrainReply};
use crate::util::{agent_handle, agent_name, ago, clean_name, path_key, truncate};

/// At most this many proposals in one reply.
pub(super) const MAX_PROPOSALS: usize = 5;
/// Task text shown in a proposal's one-line summary.
const SUMMARY_TASK_CHARS: usize = 80;

/// Where a command runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Route {
    /// This machine: today's path (a [`super::BrainAction`] the hub executes).
    Local,
    /// Other machines: one = that machine, several = ask which one.
    Remote(Vec<MachineId>),
}

/// This machine's agents first, then every paired machine's (from presence).
pub(super) fn all_agents(ctx: &BrainContext) -> Vec<Agent> {
    ctx.agents.iter().chain(ctx.remote_agents.values().flatten()).cloned().collect()
}

/// Agents of one machine: this machine's discovery, or a paired machine's presence (plus any agent of
/// that machine the hub listed in `agents`). First entry per id wins.
pub(super) fn agents_on<'a>(ctx: &'a BrainContext, machine_id: &str) -> Vec<&'a Agent> {
    let mut seen = BTreeSet::new();
    let list: Vec<&Agent> = if is_local(ctx, machine_id) {
        ctx.agents.iter().filter(|a| is_local(ctx, &a.machine_id)).collect()
    } else {
        ctx.remote_agents
            .get(machine_id)
            .into_iter()
            .flatten()
            .chain(ctx.agents.iter().filter(|a| a.machine_id == machine_id))
            .collect()
    };
    list.into_iter().filter(|a| seen.insert(a.id.as_str())).collect()
}

/// Display name of a machine (its id when no name is known).
pub(super) fn machine_name(ctx: &BrainContext, machine_id: &str) -> String {
    if is_local(ctx, machine_id) && !ctx.machine.name.trim().is_empty() {
        return clean_name(&ctx.machine.name);
    }
    ctx.machines
        .iter()
        .find(|m| m.id == machine_id)
        .map(|m| clean_name(&m.name))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| machine_id.to_string())
}

/// The machine a target names: a pet's machine, else the named machine ("here" = this one).
pub(super) fn target_machine(ctx: &BrainContext, target: &Target) -> Option<MachineId> {
    if let Some(p) = &target.pet {
        return Some(p.machine_id.clone());
    }
    match target.machine.as_ref()? {
        MachineRef::Here => Some(ctx.machine.id.clone()),
        MachineRef::Id(id) => Some(id.clone()),
    }
}

/// True if `a` and `b` are the same machine (this machine counts for an unset id).
pub(super) fn same_machine(ctx: &BrainContext, a: &str, b: &str) -> bool {
    if is_local(ctx, a) || is_local(ctx, b) {
        is_local(ctx, a) && is_local(ctx, b)
    } else {
        a == b
    }
}

/// Built-in agents in the order the default pet prefers them when nothing else decides (ids and their
/// registry ids). Any other installed agent comes after these, by name.
const AGENT_ORDER: &[&[&str]] = &[
    &["claude-code", "claude-acp"],
    &["codex", "codex-acp"],
    &["gemini"],
    &["github-copilot", "copilot", "github-copilot-cli"],
    &["opencode"],
];

/// Why an agent the user did not name was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Picked {
    /// The `bot.defaultAgent` setting.
    Setting,
    /// The agent of the most recent session on that machine.
    Recent,
    /// The first installed agent ([`AGENT_ORDER`], then by name).
    First,
}

/// The agent for a command that names none, on machine `m`: the `bot.defaultAgent` setting if it is
/// installed there; else the agent of the most recent session there (when installed: a hook-only agent
/// cannot be started); else the first installed one in [`AGENT_ORDER`], then by name. None = nothing
/// is installed there.
pub(super) fn pick_agent(ctx: &BrainContext, m: &str) -> Option<(AgentId, Picked)> {
    let installed: Vec<&Agent> = agents_on(ctx, m).into_iter().filter(|a| a.installed).collect();
    let has = |id: &str| installed.iter().any(|a| a.id == id);
    if let Some(id) = ctx.default_agent.as_deref().map(str::trim).filter(|id| has(id)) {
        return Some((id.to_string(), Picked::Setting));
    }
    let recent = ctx
        .sessions
        .iter()
        .filter(|s| same_machine(ctx, &s.machine_id, m) && has(&s.agent_id))
        .max_by_key(|s| (s.last_event_at, s.ended_at.is_none()));
    if let Some(s) = recent {
        return Some((s.agent_id.clone(), Picked::Recent));
    }
    let rank = |a: &Agent| AGENT_ORDER.iter().position(|ids| ids.contains(&a.id.as_str())).unwrap_or(AGENT_ORDER.len());
    installed
        .iter()
        .min_by_key(|a| (rank(a), a.name.trim().to_lowercase(), a.id.clone()))
        .map(|a| (a.id.clone(), Picked::First))
}

/// " I picked Codex (your default agent); for another, name it: “tell claude to …”." when `m` has
/// more than one installed agent (with one, nothing was chosen).
pub(super) fn picked_note(ctx: &BrainContext, m: &str, id: &str, why: Picked) -> String {
    let installed: Vec<&Agent> = agents_on(ctx, m).into_iter().filter(|a| a.installed).collect();
    let Some(other) = installed.iter().find(|a| a.id != id) else { return String::new() };
    let everyone = all_agents(ctx);
    let mname = machine_name(ctx, m);
    let reason = match why {
        Picked::Setting => "your default agent".to_string(),
        Picked::Recent if is_local(ctx, m) => "it had the last session here".to_string(),
        Picked::Recent => format!("it had the last session on {mname}"),
        Picked::First => "the first one installed".to_string(),
    };
    let handle = agent_handle(&other.id, &everyone);
    let example = if is_local(ctx, m) { format!("tell {handle} to …") } else { format!("tell the {mname}'s {handle} to …") };
    format!(" I picked {} ({reason}); for another, name it: “{example}”.", agent_name(id, &everyone))
}

/// Paired machines (not this one) that have `agent_id` installed.
fn machines_with_agent(ctx: &BrainContext, agent_id: &str) -> Vec<MachineId> {
    let ids: BTreeSet<&str> = ctx
        .remote_agents
        .keys()
        .map(String::as_str)
        .chain(ctx.agents.iter().map(|a| a.machine_id.as_str()))
        .filter(|m| !is_local(ctx, m))
        .collect();
    ids.into_iter()
        .filter(|m| agents_on(ctx, m).iter().any(|a| a.id == agent_id && a.installed))
        .map(str::to_string)
        .collect()
}

/// Pick the machine(s) for a command. A pet fixes the machine; else a named machine; else infer:
/// a repo in this machine's index stays local (today's behaviour), a repo found on exactly one other
/// machine goes there, several are narrowed to the machines where the agent already worked in that
/// repo. With no repo, an agent installed only on paired machines goes there. Everything else is local.
pub(super) fn route(ctx: &BrainContext, repos: &RepoIndex, cmd: &CommandIntent, target: &Target) -> Route {
    if let Some(m) = target_machine(ctx, target) {
        return if is_local(ctx, &m) { Route::Local } else { Route::Remote(vec![m]) };
    }
    let agent_id = match &cmd.agent {
        Some(AgentRef::Known(id)) => Some(id.as_str()),
        _ => None,
    };
    match &cmd.repo {
        Some(RepoRef::Named { key, explicit, .. }) => {
            let hits = repos.matching(key, *explicit, |_| true);
            if hits.iter().any(|e| is_local(ctx, &e.machine_id)) {
                return Route::Local;
            }
            let machines: BTreeSet<&str> = hits.iter().map(|e| e.machine_id.as_str()).collect();
            if machines.len() <= 1 {
                return machines.into_iter().next().map_or(Route::Local, |m| Route::Remote(vec![m.to_string()]));
            }
            if let Some(id) = agent_id {
                let worked: Vec<&str> = machines
                    .iter()
                    .copied()
                    .filter(|m| {
                        ctx.sessions.iter().any(|s| {
                            s.machine_id == *m
                                && s.agent_id == id
                                && hits.iter().any(|e| e.machine_id == *m && e.key == path_key(&s.cwd))
                        })
                    })
                    .collect();
                if worked.len() == 1 {
                    return Route::Remote(vec![worked[0].to_string()]);
                }
            }
            Route::Remote(machines.into_iter().map(str::to_string).collect())
        }
        Some(RepoRef::Unknown { .. }) => Route::Local,
        Some(RepoRef::Path(_)) | None => {
            let Some(id) = agent_id else { return Route::Local };
            let here = ctx.agents.iter().any(|a| a.id == id && a.installed && is_local(ctx, &a.machine_id));
            let there = machines_with_agent(ctx, id);
            if here || there.is_empty() {
                Route::Local
            } else {
                Route::Remote(there)
            }
        }
    }
}

/// Why no proposal could be made for a machine.
struct Refusal {
    text: String,
    /// A plain fact (offline, not installed) rather than a question back to the user.
    sad: bool,
}

fn ask(text: String) -> Refusal {
    Refusal { text, sad: false }
}

fn sorry(text: String) -> Refusal {
    Refusal { text, sad: true }
}

/// " Mac mini has: api, web." / " I don't know any repos on Mac mini yet."
pub(super) fn repos_hint(ctx: &BrainContext, repos: &RepoIndex, machine_id: &str) -> String {
    let name = machine_name(ctx, machine_id);
    let names = repos.names(5, |e| same_machine(ctx, &e.machine_id, machine_id));
    if names.is_empty() {
        format!(" I don't know any repos on {name} yet.")
    } else {
        format!(" {name} has: {}.", names.join(", "))
    }
}

fn agents_hint(agents: &[&Agent]) -> String {
    let names: Vec<String> = agents.iter().filter(|a| a.installed).map(|a| clean_name(&a.name)).take(6).collect();
    if names.is_empty() {
        String::new()
    } else {
        format!(" It has: {}.", names.join(", "))
    }
}

/// One-line button text: "Start Claude Code on Mac mini in ai-pet: run the tests" /
/// "Send to Claude Code on Mac mini (ai-pet): run the tests". `machine` None: no "on …" (this machine
/// while it is the only one known).
pub(super) fn summary(kind: ChatProposalKind, agent: &str, machine: Option<&str>, repo: &str, task: &str) -> String {
    let task = truncate(task, SUMMARY_TASK_CHARS);
    let on = machine.map(|m| format!(" on {m}")).unwrap_or_default();
    match (kind, task.is_empty()) {
        (ChatProposalKind::Start, true) => format!("Start {agent}{on} in {repo}"),
        (ChatProposalKind::Start, false) => format!("Start {agent}{on} in {repo}: {task}"),
        (ChatProposalKind::Prompt, _) => format!("Send to {agent}{on} ({repo}): {task}"),
    }
}

/// Proposals for one paired machine and, when the agent was picked for the user, a note saying which;
/// or why there are none.
fn plan_on(ctx: &BrainContext, repos: &RepoIndex, cmd: &CommandIntent, m: &str, now: UnixMs) -> Result<(Vec<ChatProposal>, String), Refusal> {
    let mname = machine_name(ctx, m);
    match ctx.machines.iter().find(|x| x.id == m) {
        None => return Err(sorry(format!("{mname} isn't paired with this machine, so I can't send it anything."))),
        Some(x) if !x.online => {
            return Err(sorry(format!("{mname} is offline (last seen {}), so I can't send it anything right now.", ago(x.last_seen, now))))
        }
        Some(_) => {}
    }
    let agents = agents_on(ctx, m);
    let everyone = all_agents(ctx);
    let mut note = String::new();
    let agent_id = match &cmd.agent {
        Some(AgentRef::Known(id)) => {
            let name = agent_name(id, &everyone);
            match agents.iter().find(|a| &a.id == id) {
                Some(a) if a.installed => id.clone(),
                Some(_) => return Err(sorry(format!("{name} isn't installed on {mname}.{}", agents_hint(&agents)))),
                None if ctx.sessions.iter().any(|s| s.machine_id == m && &s.agent_id == id && s.source != SessionSource::Owned) => {
                    return Err(sorry(format!(
                        "I only see {name} on {mname} through its hooks, so I can't drive it from here. Ask it in its own app!"
                    )))
                }
                None => return Err(sorry(format!("{name} isn't installed on {mname}.{}", agents_hint(&agents)))),
            }
        }
        Some(AgentRef::Unknown(name)) => return Err(sorry(format!("I don't see {name} on {mname}.{}", agents_hint(&agents)))),
        None => match pick_agent(ctx, m) {
            Some((id, why)) => {
                note = picked_note(ctx, m, &id, why);
                id
            }
            None => return Err(sorry(format!("{mname} has no agents I can start."))),
        },
    };
    let aname = agent_name(&agent_id, &everyone);
    let handle = agent_handle(&agent_id, &everyone);
    // Folders of the open sessions of that agent there that this hub's peer owns, newest first (only
    // folders the machine's index lists): with no repo named ("tell Rex to also run lint"), each is a
    // place to send the task to.
    let mut open_owned: Vec<&Session> = ctx
        .sessions
        .iter()
        .filter(|s| {
            s.machine_id == m
                && s.agent_id == agent_id
                && s.source == SessionSource::Owned
                && s.ended_at.is_none()
                && s.status != SessionStatus::Error
        })
        .collect();
    open_owned.sort_by_key(|s| Reverse(s.last_event_at));
    let mut seen = BTreeSet::new();
    let open_folders: Vec<Folder> =
        open_owned.iter().filter_map(|s| repos.folder_by_key(m, &path_key(&s.cwd))).filter(|(cwd, _)| seen.insert(path_key(cwd))).collect();
    let folders: Vec<Folder> = match &cmd.repo {
        Some(RepoRef::Named { name, key, explicit }) => {
            let f = repos.folders_on(m, key, *explicit);
            if f.is_empty() {
                return Err(ask(format!("{mname} has no repo called “{name}”.{}", repos_hint(ctx, repos, m))));
            }
            f
        }
        Some(RepoRef::Unknown { name }) => {
            return Err(ask(format!("{mname} has no repo called “{name}”.{}", repos_hint(ctx, repos, m))))
        }
        Some(RepoRef::Path(p)) => match repos.folder_by_key(m, &path_key(p)) {
            Some(f) => vec![f],
            None => {
                return Err(ask(format!(
                    "I can only start agents on {mname} in folders it knows.{} Pick one by name.",
                    repos_hint(ctx, repos, m)
                )))
            }
        },
        None if !cmd.force_new && !open_folders.is_empty() => open_folders,
        None => {
            // That agent's most recent session folder there, if the machine's index still has it.
            let mut used: Vec<&Session> =
                ctx.sessions.iter().filter(|s| s.machine_id == m && s.agent_id == agent_id).collect();
            used.sort_by_key(|s| Reverse((s.ended_at.is_none(), s.last_event_at)));
            match used.iter().find_map(|s| repos.folder_by_key(m, &path_key(&s.cwd))) {
                Some(f) => vec![f],
                None => {
                    return Err(ask(format!(
                        "Which repo on {mname} should {aname} work in?{} Say “tell {handle} on {mname} to … in repo <name>”.",
                        repos_hint(ctx, repos, m)
                    )))
                }
            }
        }
    };
    let task = cmd.task.trim().to_string();
    let mut out = Vec::new();
    for (cwd, repo) in folders.into_iter().take(MAX_PROPOSALS) {
        let key = path_key(&cwd);
        // Prompt only an open session this hub's peer owns and that is free: not a hook session, not
        // ended, idle or done, and not waiting on a permission.
        let open = (!cmd.force_new)
            .then(|| {
                ctx.sessions
                    .iter()
                    .filter(|s| {
                        s.machine_id == m
                            && s.agent_id == agent_id
                            && s.source == SessionSource::Owned
                            && s.ended_at.is_none()
                            && matches!(s.status, SessionStatus::Idle | SessionStatus::Done)
                            && path_key(&s.cwd) == key
                            && !ctx.pending.iter().any(|r| r.session_id == s.id)
                    })
                    .max_by_key(|s| s.last_event_at)
            })
            .flatten();
        if task.is_empty() && !cmd.force_new {
            return Err(ask(match open {
                Some(_) => format!("What should I tell {aname} on {mname} in {repo}?"),
                None => format!("What should {aname} do on {mname} in {repo}?"),
            }));
        }
        let kind = if open.is_some() { ChatProposalKind::Prompt } else { ChatProposalKind::Start };
        out.push(ChatProposal {
            id: uuid::Uuid::new_v4().to_string(),
            kind,
            machine_id: m.to_string(),
            agent_id: agent_id.clone(),
            cwd: (kind == ChatProposalKind::Start).then(|| cwd.clone()),
            repo_name: Some(repo.clone()),
            session_id: open.map(|s| s.id.clone()),
            text: task.clone(),
            summary: summary(kind, &aname, Some(&mname), &repo, &task),
        });
    }
    Ok((out, note))
}

/// Reply for a command routed to other machines: proposals and a question when there are several.
pub(super) fn remote_command(
    ctx: &BrainContext,
    voice: &Voice,
    seed: u64,
    repos: &RepoIndex,
    cmd: &CommandIntent,
    machines: &[MachineId],
    now: UnixMs,
) -> BrainReply {
    let mut proposals: Vec<ChatProposal> = Vec::new();
    let mut refusals: Vec<Refusal> = Vec::new();
    let mut picked = String::new();
    for m in machines {
        match plan_on(ctx, repos, cmd, m, now) {
            Ok((p, note)) => {
                proposals.extend(p);
                if !picked.contains(&note) {
                    picked.push_str(&note);
                }
            }
            Err(r) => refusals.push(r),
        }
    }
    proposals.truncate(MAX_PROPOSALS);
    if proposals.is_empty() {
        let lead = if refusals.iter().all(|r| r.sad) { voice.sad(seed) } else { voice.thinking(seed) };
        let why: Vec<String> = refusals.into_iter().map(|r| r.text).collect();
        let why = if why.is_empty() { "I couldn't find where to run that.".to_string() } else { why.join(" ") };
        return say(format!("{lead} {why}"));
    }
    let notes: String =
        picked + &refusals.iter().map(|r| format!(" ({})", r.text.trim_end_matches('.'))).collect::<String>();
    let text = if proposals.len() == 1 {
        let p = &proposals[0];
        format!(
            "{} Ready: {}. It runs on {}, so it needs your OK first: confirm it to go ahead.{notes}",
            voice.sound(seed),
            p.summary,
            machine_name(ctx, &p.machine_id)
        )
    } else {
        let places: Vec<String> = proposals
            .iter()
            .map(|p| format!("{} on {}", p.repo_name.as_deref().unwrap_or("?"), machine_name(ctx, &p.machine_id)))
            .collect();
        format!(
            "{} That matches more than one place: {}. Which one did you mean? Pick one to confirm.{notes}",
            voice.alert(seed),
            places.join(", ")
        )
    };
    BrainReply { text, action: None, proposals }
}

/// Sessions and requests of one machine, for status questions about it. Agents of every machine are
/// included so remote agents are named properly.
pub(super) fn scoped(ctx: &BrainContext, machine_id: &str) -> BrainContext {
    let mut c = ctx.clone();
    c.sessions.retain(|s| same_machine(ctx, &s.machine_id, machine_id));
    c.pending.retain(|r| same_machine(ctx, &r.machine_id, machine_id));
    c.agents = all_agents(ctx);
    c.sessions.sort_by_key(|s| Reverse(s.last_event_at));
    c
}
