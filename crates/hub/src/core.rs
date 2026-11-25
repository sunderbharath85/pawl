//! Hub core: in-memory cache over the store; merges the ACP host feed (owned sessions) and agent
//! hook feeds into sessions + events; applies permission rules; answers every `ClientMsg`.

pub(crate) mod a2a;
pub(crate) mod a2a_agents;
mod a2a_connect;
pub(crate) mod a2a_peer;
pub(crate) mod a2a_server;
mod brain_session;
pub(crate) mod browser;
mod host;
mod intents;
mod mirror;
mod peers;
mod repo_index;
mod settings;
mod voice;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use pawl_acp::HostEvent;
use pawl_brain::chat::{Brain, BrainAction, BrainContext, BrainReply};
use pawl_brain::watchdog::Watchdog;
use pawl_peer::PeerNode;
use pawl_protocol::{
    global_id, is_valid_machine_id, parse_global_id, Agent, AgentCapabilities, AgentId, AgentSource, ChatImage, ChatMessage,
    ChatRole, ClientKind, ClientMsg, ErrorPayload, EventType, HistorySession, HookSource, HubEvent, HubNotification,
    Machine, MachineId, NotificationLevel, PermissionOptionKind, PermissionOutcome, PermissionRequest,
    PermissionResolvedPayload, Pet, PetId, PetPatch, RequestId, ResolvedBy, Rule, RuleAction, ServerMsg, Session,
    SessionId, SessionSource, SessionStatus, StopPayload, UnixMs, Welcome, PROTOCOL_VERSION, USER_ACTIVE_WINDOW_MS,
};
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, watch};

use crate::bus::Bus;
use crate::config::{HubConfig, Paths};
use crate::machine::{self, MachineIdentity};
use crate::notify;
use crate::store::{AuditEntry, AuditOrigin, AuditOutcome, Store};
use crate::titles;
use host::Host;
pub use settings::ClientGuard;

const DAY_MS: i64 = 86_400_000;
/// Sessions kept in memory: open ones plus those active in this window.
const CACHE_WINDOW_MS: i64 = 7 * DAY_MS;
/// Ended sessions still listed in `hello`'s Welcome.
const WELCOME_ENDED_WINDOW_MS: i64 = DAY_MS;
const DEFAULT_EVENTS_LIMIT: u32 = 500;
const MAX_EVENTS_LIMIT: u32 = 5000;
const MAX_CHAT_CHARS: usize = 4000;
const PET_FLUSH_EVERY: Duration = Duration::from_secs(2);
const SETTING_DAILY_REPORT: &str = "daily_report_date";
const HOOK_ALERT_DEDUPE_MS: i64 = 10_000;
/// Longest pet name, in characters (after trimming).
pub const MAX_PET_NAME_CHARS: usize = 40;
/// userActiveAt changes smaller than this are not worth a presence push or a `machine` broadcast.
pub const ACTIVITY_STEP_MS: i64 = 30_000;
/// At most this many proposals in one chat reply.
const MAX_PROPOSALS: usize = 5;
/// Events handed to the brain: up to this many of this machine's, plus up to half as many mirrored.
const BRAIN_EVENTS: u32 = 400;
/// A2A tasks handed to the brain (the latest one of each session, newest first).
const BRAIN_TASKS: usize = 100;
/// Longest a chat reply waits for a configured pet model that is still starting (agent discovery at start).
const MODEL_WAIT: Duration = Duration::from_secs(5);

/// This machine's user activity (ClientMsg `user_activity`) and what was last announced of it.
#[derive(Default)]
struct Activity {
    /// Latest report.
    user_active_at: Option<UnixMs>,
    /// The value in the last presence pushed to peers.
    pushed: Option<UnixMs>,
    /// The value in the last `machine` broadcast to local clients.
    announced: Option<UnixMs>,
    /// The active machine as last announced.
    active: Option<MachineId>,
}

/// The machine the user is at: the online machine with the latest userActiveAt within
/// USER_ACTIVE_WINDOW_MS, else `local` (each machine then treats itself as active).
pub fn active_machine(machines: &[Machine], local: &str, now: UnixMs) -> MachineId {
    machines
        .iter()
        .filter(|m| m.online || m.id == local)
        .filter_map(|m| m.user_active_at.filter(|t| (0..=now).contains(t) && now.saturating_sub(*t) <= USER_ACTIVE_WINDOW_MS).map(|t| (t, m)))
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| b.1.id.cmp(&a.1.id)))
        .map(|(_, m)| m.id.clone())
        .unwrap_or_else(|| local.to_string())
}

/// Machine-readable ack errors: when one is anywhere in a request's error chain, the `ack` error is
/// exactly this code (clients switch on it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AckCode {
    /// The id belongs to another machine (step 4 will route it to its owner).
    #[error("not_owner")]
    NotOwner,
    /// Unknown id, or a legacy id without a machine prefix.
    #[error("not_found")]
    NotFound,
    /// Malformed input (e.g. a machine name outside 1-40 characters).
    #[error("invalid")]
    Invalid,
    /// Another known pet already has this name (names are unique, case-insensitively).
    #[error("name_taken")]
    NameTaken,
    /// The permission request existed but was already answered (here or from another machine).
    #[error("already_decided")]
    AlreadyDecided,
    /// Approving or starting on another machine needs `confirmed: true` from the client.
    #[error("unconfirmed")]
    Unconfirmed,
    /// The owner does not accept intents (`[peer] accept_intents = false`).
    #[error("forbidden")]
    Forbidden,
    /// The owner is getting too many intents from this hub.
    #[error("rate_limited")]
    RateLimited,
    /// The owning machine is paired but not linked right now.
    #[error("offline")]
    Offline,
    /// The owner did not answer within INTENT_TIMEOUT_MS (the outcome may still arrive by mirror).
    #[error("timeout")]
    Timeout,
}

impl AckCode {
    /// The code in `e`'s chain, if any.
    pub fn find(e: &anyhow::Error) -> Option<AckCode> {
        e.chain().find_map(|c| c.downcast_ref::<AckCode>().copied())
    }
}

/// A decision that came from another machine (an intent), see [`Hub::decide`].
pub(crate) struct RemoteDecision {
    /// approve (allow_* option) vs deny (reject_* option or none).
    pub approve: bool,
    pub origin: AuditOrigin,
}

/// Who asked for something (recorded in the audit log).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub label: String,
    /// The local client kind, when a client asked (forwarded intents carry it to the owner).
    pub kind: Option<ClientKind>,
}

pub(crate) fn client_label(kind: ClientKind) -> &'static str {
    match kind {
        ClientKind::Desktop => "desktop",
        ClientKind::Vscode => "vscode",
        ClientKind::Mobile => "mobile",
        ClientKind::Cli => "cli",
    }
}

impl Actor {
    pub fn client(kind: ClientKind, conn_id: u64) -> Self {
        Self { label: format!("{}#{conn_id}", client_label(kind)), kind: Some(kind) }
    }

    pub fn internal(label: impl Into<String>) -> Self {
        Self { label: label.into(), kind: None }
    }

    fn via_chat(&self) -> Self {
        Self { label: format!("chat:{}", self.label), kind: self.kind }
    }

    fn rule(action: RuleAction) -> Self {
        let a = match action {
            RuleAction::AutoAllow => "auto_allow",
            RuleAction::Ask => "ask",
            RuleAction::Deny => "deny",
        };
        Self { label: format!("rule:{a}"), kind: None }
    }
}

pub struct HubOptions {
    pub paths: Paths,
    pub config: HubConfig,
    pub token: String,
    /// This machine (`machine.toml`, see [`machine::load_or_create`]).
    pub machine: MachineIdentity,
    /// Daemon mode: run initial discovery, the watchdog and the daily report.
    /// In-process CLI use (`pawl start` without a running hub) passes false.
    pub background: bool,
}

#[derive(Default)]
struct State {
    /// This hub's machine.
    machine_id: MachineId,
    agents: Vec<Agent>,
    sessions: HashMap<SessionId, Session>,
    /// (agentId, externalId) -> hub session id, for hook-tracked sessions.
    by_external: HashMap<(AgentId, String), SessionId>,
    pets: HashMap<PetId, Pet>,
    pending: HashMap<RequestId, PermissionRequest>,
    rules: Vec<Rule>,
    dirty_pets: HashSet<PetId>,
    /// Last alert notification per hook session (dedupe window).
    last_hook_alert: HashMap<SessionId, UnixMs>,
    /// Requests answered while this hub runs (newest DECIDED_KEEP), so a late answer gets
    /// `already_decided`; older ones are found in the audit log.
    decided: HashSet<RequestId>,
    decided_order: VecDeque<RequestId>,
    /// Requests whose answer is with the agent right now (see [`Hub::decide`]).
    deciding: HashSet<RequestId>,
    /// Sessions of this machine started or prompted by another machine: rules never auto-approve
    /// their requests (a human always answers).
    remote_driven: HashSet<SessionId>,
}

/// One pass of [`Hub::decide`]'s check.
enum Step {
    Take(Box<PermissionRequest>, Option<PermissionOptionKind>),
    Wait,
    Gone(bool),
}

/// A remote decision's option: approve only `allow_once` (`allow_always` is chosen on the owner:
/// `forbidden`), deny only a reject option or none (`invalid` otherwise).
fn check_remote_option(approve: bool, kind: Option<PermissionOptionKind>) -> Result<()> {
    use PermissionOptionKind::*;
    match (approve, kind) {
        (true, Some(AllowOnce)) | (false, None | Some(RejectOnce | RejectAlways)) => Ok(()),
        (true, Some(AllowAlways)) => {
            Err(anyhow::Error::new(AckCode::Forbidden).context("allow_always can only be chosen on this machine"))
        }
        (true, _) => Err(anyhow::Error::new(AckCode::Invalid).context("approve needs an allow option")),
        (false, _) => Err(anyhow::Error::new(AckCode::Invalid).context("deny needs a reject option or none")),
    }
}

/// Recently decided request ids kept in memory.
const DECIDED_KEEP: usize = 10_000;

impl State {
    fn mark_decided(&mut self, request_id: &str) {
        if self.decided.insert(request_id.to_string()) {
            self.decided_order.push_back(request_id.to_string());
            while self.decided_order.len() > DECIDED_KEEP {
                if let Some(old) = self.decided_order.pop_front() {
                    self.decided.remove(&old);
                }
            }
        }
    }


    fn index_session(&mut self, s: &Session) {
        if let Some(ext) = &s.external_id {
            self.by_external.insert((s.agent_id.clone(), ext.clone()), s.id.clone());
        }
    }

    /// The pet of `agent_id` on `machine_id` (one per agent per machine).
    fn pet_for_mut(&mut self, machine_id: &str, agent_id: &str) -> Option<&mut Pet> {
        self.pets.values_mut().find(|p| p.machine_id == machine_id && p.agent_id == agent_id)
    }

    fn pet_for(&self, machine_id: &str, agent_id: &str) -> Option<&Pet> {
        self.pets.values().find(|p| p.machine_id == machine_id && p.agent_id == agent_id)
    }

    fn agent_name(&self, agent_id: &str) -> String {
        self.agents
            .iter()
            .find(|a| a.id == agent_id)
            .map(|a| a.name.clone())
            .unwrap_or_else(|| pretty_agent_name(agent_id))
    }

    fn sorted_pets(&self) -> Vec<Pet> {
        let mut v: Vec<Pet> = self.pets.values().cloned().collect();
        v.sort_by(|a, b| a.agent_id.cmp(&b.agent_id).then(a.id.cmp(&b.id)));
        v
    }

    fn sorted_pending(&self) -> Vec<PermissionRequest> {
        let mut v: Vec<PermissionRequest> = self.pending.values().cloned().collect();
        v.sort_by_key(|p| p.created_at);
        v
    }

    fn take_pending_for_session(&mut self, session_id: &str) -> Vec<PermissionRequest> {
        let ids: Vec<RequestId> =
            self.pending.values().filter(|p| p.session_id == session_id).map(|p| p.request_id.clone()).collect();
        let mut v: Vec<PermissionRequest> = ids.iter().filter_map(|id| self.pending.remove(id)).collect();
        v.sort_by_key(|p| p.created_at);
        v
    }

    fn sessions_sorted(&self, filter: impl Fn(&Session) -> bool) -> Vec<Session> {
        let mut v: Vec<Session> = self.sessions.values().filter(|s| filter(s)).cloned().collect();
        v.sort_by_key(|s| std::cmp::Reverse(s.last_event_at));
        v
    }
}

struct Inner {
    paths: Paths,
    /// The running configuration: config.toml + defaults + startup flags, then every
    /// `update_settings` (see `settings.rs`).
    config: RwLock<HubConfig>,
    /// Port the local API was bound to and the keys startup flags / env vars decide (set by
    /// `pawl serve`, see [`Hub::set_launch`]).
    launch: Mutex<settings::Launch>,
    /// Serializes settings changes (validate, apply, write config.toml, audit, announce).
    settings_write: tokio::sync::Mutex<()>,
    /// Woken after a settings change so the watchdog / daily report loops pick it up at once.
    settings_changed: tokio::sync::Notify,
    /// Local desktop clients connected right now (`open_panel` reports whether one got it).
    desktop_clients: AtomicUsize,
    /// Tests only: the home directory hook status / install use instead of the real one.
    hooks_home: Mutex<Option<PathBuf>>,
    token: String,
    /// This hub's machine id (never changes while the hub runs).
    machine_id: MachineId,
    /// This machine's identity as last saved to machine.toml.
    machine: Mutex<MachineIdentity>,
    /// Serializes machine.toml writes.
    machine_write: tokio::sync::Mutex<()>,
    store: Store,
    bus: Bus,
    host: Host,
    /// Rebuilt when the bot settings change or agents are rediscovered (see [`Hub::rebuild_brain`]).
    brain: RwLock<Brain>,
    /// The "acp" provider's hidden agent session (see `brain_session.rs`).
    brain_session: Mutex<brain_session::BrainSessionState>,
    /// One brain-session turn at a time.
    brain_turn: tokio::sync::Mutex<()>,
    /// Agent discovery has finished at least once since start (the "acp" provider depends on it). True
    /// from the start for hubs that do not discover in the background.
    agents_discovered: watch::Sender<bool>,
    state: Mutex<State>,
    watchdog: Mutex<Watchdog>,
    discovery: tokio::sync::Mutex<()>,
    shutdown: watch::Sender<bool>,
    /// Set by `restart_hub`: `pawl serve` stops, starts a replacement process and exits.
    restart: watch::Sender<bool>,
    /// Warnings raised while loading (e.g. dropped unsafe rules); replayed to every new client
    /// until the condition is fixed.
    startup_notices: Mutex<Vec<HubNotification>>,
    /// Hub-to-hub links (pairing, presence).
    peer: PeerNode,
    /// The hub's view of the pairing roster (see `peers.rs`).
    peers: Mutex<peers::Peers>,
    /// The last presence snapshot handed to the link layer, minus timestamps.
    presence_pushed: Mutex<Option<Value>>,
    /// Serializes roster changes (peer events, revoke) so each is audited and announced once.
    peer_ops: tokio::sync::Mutex<()>,
    /// Read-only copy of paired hubs' sessions, pets and pending requests (see `mirror.rs`).
    mirror: Mutex<mirror::MirrorCache>,
    /// Intents waiting for acks (origin) and answered intents (owner), see `intents.rs`.
    intents: Mutex<intents::IntentState>,
    /// Woken whenever a decision in flight finishes (see [`Hub::decide`]).
    decide_done: tokio::sync::Notify,
    /// This machine's repo index (see `repo_index.rs`).
    repos: Mutex<Vec<pawl_protocol::RepoRef>>,
    /// Session folders seen since startup (a new one triggers an index rebuild).
    seen_cwds: Mutex<HashSet<String>>,
    activity: Mutex<Activity>,
    /// The A2A view of this machine's owned sessions: one task per prompt turn (see `core/a2a.rs`).
    tasks: pawl_a2a::TaskRegistry,
    /// The localhost A2A server (`core/a2a_server.rs`) and its live state for the settings view.
    a2a_runtime: tokio::sync::Mutex<a2a_server::A2aRuntime>,
    a2a_status: Mutex<a2a_server::A2aStatus>,
    /// Outside callers' generations and open streams / waits (`core/a2a_server.rs`).
    a2a_callers: Mutex<a2a_server::A2aCallerBook>,
    #[cfg(test)]
    a2a_test_state: Mutex<Option<Arc<pawl_a2a::server::ServerState>>>,
    /// Outside A2A agents' sessions (`core/a2a_agents.rs`); they report like the ACP host.
    remote: Arc<a2a_agents::RemoteAgents>,
    /// A2A calls over the peer link (`core/a2a_peer.rs`): waiting (origin) and answered (owner).
    a2a_peer: Mutex<a2a_peer::A2aPeerState>,
    /// Approvals by voice: read-backs per chat and wrong codes (`core/voice.rs`).
    voice: Mutex<voice::VoiceState>,
    /// Running browser tasks' turn clocks (core/browser.rs).
    browser: Mutex<browser::BrowserState>,
}

/// The hub. Cheap to clone; all clones share state.
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

impl Hub {
    /// Open the store, load the cache and spawn the hub's tasks.
    pub async fn start(opts: HubOptions) -> Result<Hub> {
        opts.paths.ensure()?;
        let db = opts.paths.db.clone();
        let mid = opts.machine.id.clone();
        let store = tokio::task::spawn_blocking(move || Store::open(&db, &mid)).await.context("open store")??;
        let (tx, rx) = mpsc::unbounded_channel();
        let host = Host::acp(store.machine_id().to_string(), tx);
        let hub = Self::build(opts, store, host).await?;
        tokio::spawn(hub.clone().host_loop(rx));
        Ok(hub)
    }

    async fn build(opts: HubOptions, store: Store, host: Host) -> Result<Hub> {
        let HubOptions { paths, config, token, machine, background } = opts;
        if !is_valid_machine_id(&machine.id) {
            bail!("invalid machine id {:?}", machine.id);
        }
        if store.machine_id() != machine.id {
            bail!("store belongs to machine {:?}, not {:?}", store.machine_id(), machine.id);
        }
        let machine_id = machine.id.clone();
        let now = pawl_protocol::now_ms();
        let retention_days = config.event_retention_days;
        let (agents, mut sessions, mut pets, db_rules, mirror_sessions, mirror_pending, remote_driven) = store
            .run(move |s| {
                if retention_days > 0 {
                    let n = s.prune_events_before(now - retention_days as i64 * DAY_MS)?;
                    if n > 0 {
                        tracing::info!("pruned {n} events older than {retention_days} days");
                    }
                }
                // Mirror rows of machines that are no longer paired (revoked while this hub was
                // down) go; ended mirrored sessions past retention go.
                let paired: Vec<String> = s.roster()?.into_iter().map(|e| e.device.id).collect();
                let n = s.delete_unpaired_mirrors(&paired)?;
                if n > 0 {
                    tracing::info!("deleted {n} mirror rows of machines that are no longer paired");
                }
                s.prune_mirror(now)?;
                s.prune_intent_log(now, crate::store::INTENT_LOG_MAX_ROWS)?;
                Ok((
                    s.agents()?,
                    s.sessions_active_since(now - CACHE_WINDOW_MS)?,
                    s.pets()?,
                    s.rules()?,
                    s.mirror_sessions(now - crate::store::MIRROR_RETENTION_MS)?,
                    s.mirror_pending()?,
                    s.remote_driven_sessions()?,
                ))
            })
            .await?;

        // Titles stored before injected text was cleaned out ("<task-notification>…", "[Image #40]"): cleaned
        // once at start. Only titles that start like one are read, so this is cheap after the first run.
        let names = agents.clone();
        let retitled: Vec<Session> = store
            .run(move |s| {
                let mut out = Vec::new();
                for mut x in s.sessions_with_injected_titles()? {
                    let agent_name =
                        names.iter().find(|a| a.id == x.agent_id).map(|a| a.name.clone()).unwrap_or_else(|| pretty_agent_name(&x.agent_id));
                    let old = x.title.clone().unwrap_or_default();
                    let new = titles::next(Some(&old), titles::from_prompt(&old), true, &agent_name, &x.cwd);
                    if new != old {
                        x.title = Some(new);
                        s.upsert_session(&x)?;
                        out.push(x);
                    }
                }
                Ok(out)
            })
            .await?;
        if !retitled.is_empty() {
            tracing::info!("cleaned {} session title(s) that held injected text", retitled.len());
        }
        for r in &retitled {
            if let Some(s) = sessions.iter_mut().find(|s| s.id == r.id) {
                s.title = r.title.clone();
            }
        }

        // Owned sessions die with the hub process that owned their agent connection.
        let mut closed = Vec::new();
        for s in sessions.iter_mut().filter(|s| s.source == SessionSource::Owned && s.ended_at.is_none()) {
            s.ended_at = Some(now);
            if matches!(s.status, SessionStatus::Thinking | SessionStatus::Working | SessionStatus::WaitingPermission) {
                s.status = SessionStatus::Error;
            }
            closed.push(s.clone());
        }
        if !closed.is_empty() {
            store
                .run(move |s| {
                    for x in &closed {
                        s.upsert_session(x)?;
                    }
                    Ok(())
                })
                .await?;
        }

        // v1 hubs picked pet names without checking for collisions; names are unique now.
        let renamed: Vec<Pet> = dedupe_pet_names(&mut pets, &machine_id).into_iter().map(|i| pets[i].clone()).collect();
        if !renamed.is_empty() {
            for p in &renamed {
                tracing::info!("pet {} ({}) renamed to {:?}: its name was already taken", p.id, p.agent_id, p.name);
            }
            let r2 = renamed.clone();
            store
                .run(move |s| {
                    for p in &r2 {
                        s.upsert_pet(p)?;
                    }
                    Ok(())
                })
                .await?;
        }

        // Stored rules that are exactly the old defaults (saved, never customised) move to the new defaults
        // once: reads and searches ask now. Customised rule sets are left alone.
        let migrated_defaults = db_rules.as_ref().is_some_and(|r| *r == pawl_brain::rules::legacy_default_rules());
        let db_rules = if migrated_defaults {
            let new = pawl_brain::rules::default_rules();
            let n2 = new.clone();
            store.run(move |s| s.set_rules(&n2)).await?;
            tracing::info!("default permission rules updated: reads and searches now ask");
            Some(new)
        } else {
            db_rules
        };
        // Security floor: rules that fail validation never reach the engine.
        let (rules_source, loaded) = match (db_rules, config.rules.clone()) {
            (Some(r), _) => ("hub.db", r),
            (None, Some(r)) => ("config.toml", r),
            (None, None) => ("the built-in defaults", pawl_brain::rules::default_rules()),
        };
        let (rules, dropped) = sanitize_rules(loaded);
        let mut startup_notices = Vec::new();
        if migrated_defaults {
            startup_notices.push(notify::make(
                &machine_id,
                NotificationLevel::Info,
                "Default rules now ask before reads",
                "Agents now ask before reading or searching files. Choose “Always allow” on a request, or add a rule, to allow it again.",
                None,
                None,
            ));
        }
        if !dropped.is_empty() {
            let list = dropped.iter().map(|(id, why)| format!("{id:?} ({why})")).collect::<Vec<_>>().join("; ");
            tracing::warn!("ignoring {} invalid permission rule(s) from {rules_source}: {list}", dropped.len());
            startup_notices.push(notify::make(
                &machine_id,
                NotificationLevel::Warning,
                "Unsafe permission rules ignored",
                format!("Dropped {} rule(s) from {rules_source}: {list}", dropped.len()),
                None,
                None,
            ));
        }
        let mut state = State {
            machine_id: machine_id.clone(),
            agents,
            rules,
            remote_driven: remote_driven.into_iter().collect(),
            ..Default::default()
        };
        for s in sessions {
            state.index_session(&s);
            state.sessions.insert(s.id.clone(), s);
        }
        // Pets of every machine were deduped together; only this machine's are the hub's own.
        let (pets, mirror_pets): (Vec<Pet>, Vec<Pet>) = pets.into_iter().partition(|p| p.machine_id == machine_id);
        for p in pets {
            state.pets.insert(p.id.clone(), p);
        }

        let (peer, peer_rx) = Self::new_peer_node(&paths, machine.local_machine(), &store)?;
        let (shutdown, _) = watch::channel(false);
        let (restart, _) = watch::channel(false);
        let tasks = pawl_a2a::TaskRegistry::new(machine_id.clone());
        let (remote_tx, remote_rx) = mpsc::unbounded_channel();
        let remote = Arc::new(a2a_agents::RemoteAgents::new(machine_id.clone(), remote_tx));
        let hub = Hub {
            inner: Arc::new(Inner {
                brain: RwLock::new(Brain::new(config.brain.to_brain_config())),
                brain_session: Mutex::new(brain_session::BrainSessionState::default()),
                brain_turn: tokio::sync::Mutex::new(()),
                agents_discovered: watch::channel(!background).0,
                watchdog: Mutex::new(Watchdog::new(config.watchdog.to_watchdog_config(), machine_id.clone())),
                paths,
                launch: Mutex::new(settings::Launch { served: false, port: config.port, overrides: BTreeMap::new() }),
                config: RwLock::new(config),
                settings_write: tokio::sync::Mutex::new(()),
                settings_changed: tokio::sync::Notify::new(),
                desktop_clients: AtomicUsize::new(0),
                hooks_home: Mutex::new(None),
                token,
                machine_id,
                machine: Mutex::new(machine),
                machine_write: tokio::sync::Mutex::new(()),
                store,
                bus: Bus::default(),
                host,
                state: Mutex::new(state),
                discovery: tokio::sync::Mutex::new(()),
                shutdown,
                restart,
                startup_notices: Mutex::new(startup_notices.clone()),
                peer,
                peers: Mutex::new(peers::Peers::default()),
                presence_pushed: Mutex::new(None),
                peer_ops: tokio::sync::Mutex::new(()),
                mirror: Mutex::new(mirror::MirrorCache::default()),
                intents: Mutex::new(intents::IntentState::default()),
                decide_done: tokio::sync::Notify::new(),
                repos: Mutex::new(Vec::new()),
                seen_cwds: Mutex::new(HashSet::new()),
                activity: Mutex::new(Activity::default()),
                tasks,
                a2a_runtime: tokio::sync::Mutex::new(a2a_server::A2aRuntime::default()),
                a2a_status: Mutex::new(a2a_server::A2aStatus::default()),
                a2a_callers: Mutex::new(a2a_server::A2aCallerBook::default()),
                #[cfg(test)]
                a2a_test_state: Mutex::new(None),
                remote,
                a2a_peer: Mutex::new(a2a_peer::A2aPeerState::default()),
                voice: Mutex::new(voice::VoiceState::default()),
                browser: Mutex::new(browser::BrowserState::default()),
            }),
        };
        hub.set_mirror(mirror_sessions, mirror_pets, mirror_pending);
        // Now that the hub exists the brain can hold the "acp" provider's session handle.
        hub.rebuild_brain();
        tokio::spawn(hub.clone().brain_session_loop());
        // Browser tasks end with the run that started them: their folders go, and running ones are timed.
        hub.sweep_browser_dirs();
        tokio::spawn(hub.clone().browser_loop());
        hub.rebuild_repo_index().await;
        tokio::spawn(hub.clone().repo_index_loop());
        let tap = hub.inner.bus.tap();
        tokio::spawn(hub.clone().mirror_out_loop(tap));
        tokio::spawn(hub.clone().mirror_prune_loop());
        hub.load_peers().await?;
        if migrated_defaults {
            hub.audit_rules_migration().await;
        }
        hub.ensure_default_pet_name().await;
        hub.refresh_presence();
        tokio::spawn(hub.clone().peer_loop(peer_rx));
        // About this hub's config: never mirrored to peers.
        for notification in startup_notices {
            hub.inner.bus.send_local(ServerMsg::Notification { notification });
        }
        for pet in renamed {
            hub.inner.bus.send(ServerMsg::Pet { pet });
        }

        tokio::spawn(hub.clone().pet_flush_loop());
        // Outside A2A agents report through their own channel, handled like the ACP host's events.
        tokio::spawn(hub.clone().host_loop(remote_rx));
        if background {
            hub.start_peer_links().await?;
            let h = hub.clone();
            tokio::spawn(async move {
                if let Err(e) = h.refresh_agents(false).await {
                    tracing::warn!("agent discovery failed: {e:#}");
                }
                // Done either way: a chat waiting for the "acp" provider stops waiting.
                h.inner.agents_discovered.send_replace(true);
                h.log_pet_model();
            });
            tokio::spawn(hub.clone().watchdog_loop());
            tokio::spawn(hub.clone().daily_report_loop());
            // The localhost A2A server, when `[a2a] server = true` (off by default).
            hub.sync_a2a_server().await;
            // Outside A2A agents' cards (ETag-cached), so they are listed as agents.
            if !hub.cfg().a2a.agents.is_empty() {
                let h = hub.clone();
                tokio::spawn(async move { h.refresh_a2a_cards().await });
            }
        }
        Ok(hub)
    }

    // ------------------------------------------------------------ accessors

    pub fn bus(&self) -> &Bus {
        &self.inner.bus
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerMsg> {
        self.inner.bus.subscribe()
    }

    pub fn token(&self) -> &str {
        &self.inner.token
    }

    /// The running configuration (a copy).
    pub fn config(&self) -> HubConfig {
        self.cfg().clone()
    }

    /// The running configuration; never hold the guard across an await.
    fn cfg(&self) -> RwLockReadGuard<'_, HubConfig> {
        self.inner.config.read().unwrap_or_else(|p| p.into_inner())
    }

    /// The pet brain for the current bot settings.
    fn brain(&self) -> Brain {
        self.inner.brain.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The default (orchestrator) pet's name (`bot.petName`), once picked.
    pub(crate) fn default_pet_name(&self) -> Option<String> {
        self.cfg().brain.pet_name.clone().map(|n| n.trim().to_string()).filter(|n| !n.is_empty())
    }

    /// A brain for the current bot settings, with the "acp" provider's session while its agent is installed.
    pub(crate) fn rebuild_brain(&self) {
        let brain = Brain::with_session(self.cfg().brain.to_brain_config(), self.acp_model());
        *self.inner.brain.write().unwrap_or_else(|p| p.into_inner()) = brain;
        if *self.inner.agents_discovered.borrow() {
            self.log_pet_model();
        }
    }

    /// One line about the pet's model (at start once discovery is done, and after each change).
    fn log_pet_model(&self) {
        match self.brain().model_label() {
            Some(m) => tracing::info!(model = %m, "pet model"),
            None => tracing::info!("pet model: none configured; deterministic replies"),
        }
    }

    /// The "acp" provider may still appear: discovery has not finished, the provider allows acp, and no
    /// other model answers.
    fn pet_model_pending(&self) -> bool {
        let cfg = self.cfg();
        !*self.inner.agents_discovered.borrow()
            && matches!(cfg.brain.provider, pawl_protocol::BotProvider::Auto | pawl_protocol::BotProvider::Acp)
            && cfg.brain.effective_acp_agent() == brain_session::LOCKED_AGENT
            && !self.brain().has_llm()
    }

    /// The brain for a chat reply: while the configured model is still resolving (startup discovery), wait
    /// for it, at most [`MODEL_WAIT`], instead of answering as if none were configured.
    async fn ready_brain(&self) -> (Brain, bool) {
        if self.pet_model_pending() {
            let mut rx = self.inner.agents_discovered.subscribe();
            let _ = tokio::time::timeout(MODEL_WAIT, rx.wait_for(|done| *done)).await;
        }
        (self.brain(), self.pet_model_pending())
    }

    pub fn paths(&self) -> &Paths {
        &self.inner.paths
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    /// This hub's machine id.
    pub fn machine_id(&self) -> &str {
        &self.inner.machine_id
    }

    /// This machine, as reported to clients right now (online, lastSeen = now).
    pub fn machine(&self) -> Machine {
        let mut m = self.inner.machine.lock().unwrap_or_else(|p| p.into_inner()).local_machine();
        m.user_active_at = self.activity().user_active_at;
        m
    }

    fn activity(&self) -> MutexGuard<'_, Activity> {
        self.inner.activity.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The machine the user is at right now (see [`active_machine`]).
    pub fn active_machine_id(&self) -> MachineId {
        active_machine(&self.machines(), &self.inner.machine_id, pawl_protocol::now_ms())
    }

    /// ClientMsg `user_activity`: stamp this machine's userActiveAt. Peers hear it with the next
    /// heartbeat, or at once when it moved by ACTIVITY_STEP_MS (the user came back after a pause);
    /// local clients get `machine` under the same rule or when the active machine changes.
    pub fn user_activity(&self) {
        let now = pawl_protocol::now_ms();
        let (push, announce) = {
            let mut a = self.activity();
            a.user_active_at = Some(now);
            let push = a.pushed.is_none_or(|t| now.saturating_sub(t) >= ACTIVITY_STEP_MS);
            if push {
                a.pushed = Some(now);
            }
            let announce = a.announced.is_none_or(|t| now.saturating_sub(t) >= ACTIVITY_STEP_MS);
            if announce {
                a.announced = Some(now);
            }
            (push, announce)
        };
        if push {
            self.push_presence();
        }
        if announce {
            self.inner.bus.send(ServerMsg::Machine { machine: self.machine() });
        }
        self.check_active_machine(announce);
    }

    /// Announce both machines when the active machine changed. `local_sent`: this machine's
    /// `machine` was just broadcast.
    pub(super) fn check_active_machine(&self, local_sent: bool) {
        let machines = self.machines();
        let active = active_machine(&machines, &self.inner.machine_id, pawl_protocol::now_ms());
        let previous = {
            let mut a = self.activity();
            if a.active.as_deref() == Some(active.as_str()) {
                return;
            }
            a.active.replace(active.clone())
        };
        let Some(previous) = previous else { return }; // first computation: nothing changed for clients
        for m in machines.into_iter().filter(|m| m.id == active || m.id == previous) {
            if local_sent && m.id == self.inner.machine_id {
                continue;
            }
            self.inner.bus.send(ServerMsg::Machine { machine: m });
        }
    }

    /// Every known machine: this one first, then every paired (not revoked) hub with its online
    /// state from the link layer.
    pub fn machines(&self) -> Vec<Machine> {
        let mut v = vec![self.machine()];
        v.extend(self.peer_machines());
        v
    }

    /// Ok if `id` is a global id owned by this machine; else `not_owner` (another machine's prefix)
    /// or `not_found` (no valid prefix).
    fn check_owned(&self, id: &str) -> Result<()> {
        match parse_global_id(id) {
            Some((m, _)) if m == self.inner.machine_id => Ok(()),
            Some(_) => Err(AckCode::NotOwner.into()),
            None => Err(AckCode::NotFound.into()),
        }
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn agents(&self) -> Vec<Agent> {
        self.st().agents.clone()
    }

    pub fn session(&self, id: &str) -> Option<Session> {
        self.st().sessions.get(id).cloned()
    }

    pub fn pending_permissions(&self) -> Vec<PermissionRequest> {
        self.st().sorted_pending()
    }

    pub fn pets(&self) -> Vec<Pet> {
        self.st().sorted_pets()
    }

    pub fn rules(&self) -> Vec<Rule> {
        self.st().rules.clone()
    }

    /// Load-time warnings (e.g. unsafe rules dropped) that every newly connected client should see.
    pub fn startup_notices(&self) -> Vec<HubNotification> {
        self.inner.startup_notices.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn welcome(&self) -> Welcome {
        let now = pawl_protocol::now_ms();
        let machine = self.machine();
        let machines = self.machines();
        let devices = self.devices();
        let peer = self.peer_status();
        let shown = |s: &Session| {
            s.ended_at.is_none() || now - s.last_event_at.max(s.ended_at.unwrap_or(0)) <= WELCOME_ENDED_WINDOW_MS
        };
        let (mut sessions, mut pets, mut pending) =
            (self.mirror_sessions(shown), self.mirror_pets(), self.mirror_pending());
        let st = self.st();
        sessions.extend(st.sessions_sorted(shown));
        sessions.sort_by_key(|s| std::cmp::Reverse(s.last_event_at));
        pets.extend(st.sorted_pets());
        sort_pets(&mut pets);
        pending.extend(st.sorted_pending());
        pending.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.request_id.cmp(&b.request_id)));
        Welcome {
            protocol_version: PROTOCOL_VERSION,
            hub_version: crate::HUB_VERSION.to_string(),
            machine,
            machines,
            devices,
            peer,
            agents: st.agents.clone(),
            sessions,
            pets,
            pending_permissions: pending,
            rules: st.rules.clone(),
        }
    }

    /// Stop background tasks, flush pets, say `bye{shutdown}` to every paired hub and shut down
    /// every agent connection.
    pub async fn shutdown(&self) {
        let _ = self.inner.shutdown.send(true);
        self.stop_a2a_server().await;
        self.inner.remote.shutdown();
        self.flush_pets().await;
        self.inner.peer.shutdown().await;
        self.inner.host.shutdown().await;
    }

    fn shutdown_rx(&self) -> watch::Receiver<bool> {
        self.inner.shutdown.subscribe()
    }

    // ------------------------------------------------------------ client messages

    /// Answer one client message with exactly one `ack`.
    pub async fn handle(&self, msg: ClientMsg, actor: &Actor) -> ServerMsg {
        let id = msg.id().to_string();
        match self.dispatch(msg, actor).await {
            Ok(v) => ServerMsg::ack_ok(id, v),
            Err(e) => match AckCode::find(&e) {
                Some(code) => ServerMsg::ack_err(id, code.to_string()),
                None => ServerMsg::ack_err(id, format!("{e:#}")),
            },
        }
    }

    async fn dispatch(&self, msg: ClientMsg, actor: &Actor) -> Result<Value> {
        Ok(match msg {
            ClientMsg::Hello { .. } => serde_json::to_value(self.welcome())?,
            ClientMsg::ListAgents { refresh, machine_id, .. } => {
                let agents = match machine_id.filter(|m| *m != self.inner.machine_id) {
                    Some(m) => self.peer_agents(&m).await?,
                    None if refresh == Some(true) => self.refresh_agents(true).await?,
                    None => self.agents(),
                };
                json!({ "agents": agents })
            }
            ClientMsg::ListSessions { include_ended, .. } => {
                json!({ "sessions": self.list_sessions(include_ended.unwrap_or(false)).await? })
            }
            ClientMsg::GetEvents { session_id, since_seq, limit, .. } => {
                json!({ "events": self.events(&session_id, since_seq, limit).await? })
            }
            // Another machine's ids (or machineId) become an intent to that machine (intents.rs).
            ClientMsg::StartSession { agent_id, cwd, prompt, machine_id, confirmed, .. } => {
                match machine_id.filter(|m| *m != self.inner.machine_id) {
                    Some(m) => self.start_remote(&m, &agent_id, &cwd, prompt, confirmed, actor).await?,
                    None => {
                        let (session, warning) = self.start_session(&agent_id, &cwd, prompt).await?;
                        match warning {
                            Some(w) => json!({ "session": session, "warning": w }),
                            None => json!({ "session": session }),
                        }
                    }
                }
            }
            // A picture never leaves this machine: a prompt with one goes to a local ACP agent that takes images.
            ClientMsg::Prompt { session_id, text, images, .. } if !images.is_empty() => match self.foreign_owner(&session_id) {
                Some(_) => {
                    return Err(anyhow::Error::new(AckCode::Invalid)
                        .context("a picture is never sent to another machine: attach it to a session on this machine"))
                }
                None => {
                    self.prompt_with_images(&session_id, text, images).await?;
                    json!({})
                }
            },
            ClientMsg::Prompt { session_id, text, .. } => match self.foreign_owner(&session_id) {
                Some(m) => self.forward(&m, actor, false, pawl_protocol::Intent::Prompt { session_id, text }).await?,
                None => {
                    self.prompt(&session_id, text).await?;
                    json!({})
                }
            },
            ClientMsg::StartBrowserTask { task, confirmed, .. } => self.start_browser_task(&task, confirmed).await?,
            ClientMsg::Cancel { session_id, .. } => match self.foreign_owner(&session_id) {
                Some(m) => self.forward(&m, actor, false, pawl_protocol::Intent::Cancel { session_id }).await?,
                None => {
                    self.cancel(&session_id, actor).await?;
                    json!({})
                }
            },
            ClientMsg::ResolvePermission { request_id, option_id, confirmed, .. } => match self.foreign_owner(&request_id) {
                Some(m) => self.resolve_remote(&m, &request_id, option_id, confirmed, actor).await?,
                None => {
                    self.resolve_permission(&request_id, option_id, ResolvedBy::User, actor).await?;
                    json!({})
                }
            },
            ClientMsg::ListHistory { agent_id, cwd, .. } => {
                json!({ "sessions": self.list_history(&agent_id, cwd).await? })
            }
            ClientMsg::ResumeSession { agent_id, acp_session_id, cwd, .. } => {
                json!({ "session": self.resume_session(&agent_id, acp_session_id, &cwd).await? })
            }
            ClientMsg::Chat { pet_id, text, via, images, .. } => {
                // `via: "voice"`: a speech transcript (approvals follow the voice rules, see core/voice.rs).
                let spoken = match via.as_deref() {
                    None => false,
                    Some(voice::VOICE) => true,
                    Some(_) => return Err(anyhow::Error::new(AckCode::Invalid).context("chat via must be \"voice\" or absent")),
                };
                let image = one_image(images)?;
                json!({ "message": self.chat_with(pet_id, text, spoken, image, actor).await? })
            }
            ClientMsg::UpdatePet { pet_id, patch, .. } => json!({ "pet": self.update_pet(&pet_id, patch).await? }),
            ClientMsg::SetRules { rules, .. } => json!({ "rules": self.set_rules(rules).await? }),
            ClientMsg::UpdateMachine { machine_id, name, .. } => {
                json!({ "machine": self.update_machine(&machine_id, &name).await? })
            }
            ClientMsg::CreateInvite { ttl_secs, addr, for_phone, .. } => {
                self.create_invite(ttl_secs, addr, for_phone.unwrap_or(false))?
            }
            ClientMsg::JoinPeer { uri, .. } => self.join_peer(&uri, actor).await?,
            ClientMsg::RevokeDevice { device_id, .. } => self.revoke_device(&device_id, actor).await?,
            ClientMsg::UserActivity { .. } => {
                self.user_activity();
                json!({})
            }
            ClientMsg::ListRepos { machine_id, .. } => json!({ "repos": self.repos_of(machine_id.as_deref()).await? }),
            // Settings: only local clients send ClientMsgs (peers, intents and the mirror never
            // reach this function), see settings.rs.
            ClientMsg::GetSettings { .. } => self.get_settings(),
            ClientMsg::UpdateSettings { patch, .. } => self.update_settings(&patch, actor).await?,
            ClientMsg::HookStatus { .. } => json!({ "hooks": self.hook_status().await? }),
            ClientMsg::InstallHooks { target, repo, action, .. } => self.install_hooks(target, repo, action, actor).await?,
            ClientMsg::OpenPanel { view, .. } => self.open_panel(view),
            ClientMsg::RestartHub { .. } => self.restart_hub(actor).await?,
            // A2A: the token is shown once; the tasks are this machine's delegations.
            ClientMsg::A2aRotateToken { caller_id, .. } => self.a2a_rotate_token(&caller_id, actor).await?,
            ClientMsg::ListTasks { session_id, .. } => json!({ "tasks": self.list_task_summaries(session_id.as_deref()) }),
            // Connect coding agents (Settings > A2A): local only like every settings message.
            ClientMsg::A2aConnectStatus { .. } => json!({ "targets": self.a2a_connect_status().await? }),
            ClientMsg::A2aConnect { target, action, .. } => self.a2a_connect(target, action, actor).await?,
        })
    }

    /// Sessions of every machine (this hub's and the mirror's), newest activity first.
    pub async fn list_sessions(&self, include_ended: bool) -> Result<Vec<Session>> {
        if !include_ended {
            let mut v = self.mirror_sessions(|s| s.ended_at.is_none());
            v.extend(self.st().sessions_sorted(|s| s.ended_at.is_none()));
            v.sort_by_key(|s| std::cmp::Reverse(s.last_event_at));
            return Ok(v);
        }
        let mut stored = self.inner.store.run(|s| s.recent_sessions(500)).await?;
        let mirrored: HashMap<SessionId, Session> =
            self.mirror_sessions(|_| true).into_iter().map(|s| (s.id.clone(), s)).collect();
        let st = self.st();
        for s in stored.iter_mut() {
            if let Some(c) = st.sessions.get(&s.id).or_else(|| mirrored.get(&s.id)) {
                *s = c.clone();
            }
        }
        let known: HashSet<&str> = stored.iter().map(|s| s.id.as_str()).collect();
        let mut extra: Vec<Session> = st.sessions.values().filter(|s| !known.contains(s.id.as_str())).cloned().collect();
        drop(known);
        stored.append(&mut extra);
        stored.sort_by_key(|s| std::cmp::Reverse(s.last_event_at));
        Ok(stored)
    }

    /// Events of a session of this machine, or of a session mirrored from a paired hub (read-only);
    /// `since_seq` is the `HubEvent.seq` cursor (the owner's seq).
    pub async fn events(&self, session_id: &str, since_seq: Option<i64>, limit: Option<u32>) -> Result<Vec<HubEvent>> {
        let limit = limit.unwrap_or(DEFAULT_EVENTS_LIMIT).clamp(1, MAX_EVENTS_LIMIT);
        if let Err(e) = self.check_owned(session_id) {
            if AckCode::find(&e) == Some(AckCode::NotOwner) && self.is_mirrored_session(session_id).await? {
                let sid = session_id.to_string();
                return self.inner.store.run(move |s| s.events(&sid, since_seq, limit)).await;
            }
            return Err(e);
        }
        let sid = session_id.to_string();
        let cached = self.st().sessions.contains_key(session_id);
        self.inner
            .store
            .run(move |s| {
                if !cached && !s.session_exists(&sid)? {
                    return Err(AckCode::NotFound.into());
                }
                s.events(&sid, since_seq, limit)
            })
            .await
    }

    /// Start an owned ACP session, then send `prompt` (if any) as its first task. The orchestrator
    /// delegates through A2A (`SendMessage` with delegate metadata, see `core/a2a.rs`). A first
    /// prompt that cannot be sent leaves the session running and is reported as a warning instead of
    /// failing the start.
    pub async fn start_session(&self, agent_id: &str, cwd: &str, prompt: Option<String>) -> Result<(Session, Option<String>)> {
        let req = a2a::start_request(agent_id, cwd, prompt.as_deref());
        let d = self.a2a_send(req, &pawl_a2a::Caller::Local, &Actor::internal("orchestrator"), a2a::Answers::Any).await?;
        let session = d.session.ok_or_else(|| anyhow!("the start did not report a session"))?;
        Ok((session, d.warning))
    }

    /// Start an owned ACP session without a prompt (the ACP executor behind the A2A facade).
    pub(super) async fn launch_session(&self, agent_id: &str, cwd: &str) -> Result<Session> {
        let agent = self.find_agent(agent_id).await?;
        if agent.source == AgentSource::A2a {
            // An outside A2A agent works where it runs; no folder here.
            return self.launch_remote(&agent).await;
        }
        let cwd = resolve_cwd(cwd)?;
        let id = self.new_id();
        self.ensure_pet(&agent.id).await;
        let started = self
            .inner
            .host
            .start_session(&agent, cwd.clone(), id.clone(), None)
            .await
            .with_context(|| format!("could not start a {} session", agent.name))?;
        let session = self.merge_session(started).await;
        self.record_event(&session.id, EventType::SessionStart, json!({ "agentId": agent.id, "cwd": session.cwd }))
            .await?;
        Ok(self.session(&session.id).unwrap_or(session))
    }

    /// A fresh global id on this machine.
    fn new_id(&self) -> String {
        global_id(&self.inner.machine_id, uuid::Uuid::new_v4())
    }

    fn owned_open_session(&self, session_id: &str) -> Result<Session> {
        self.check_owned(session_id)?;
        let s = self.session(session_id).ok_or(AckCode::NotFound)?;
        if s.source != SessionSource::Owned {
            bail!(
                "session {session_id} is tracked through {} hooks; prompt it from its own app",
                self.st().agent_name(&s.agent_id)
            );
        }
        if s.ended_at.is_some() {
            bail!("session {session_id} has ended; resume it to continue");
        }
        Ok(s)
    }

    /// Prompt one of this machine's owned sessions: a new A2A task in its context (`core/a2a.rs`).
    pub async fn prompt(&self, session_id: &str, text: String) -> Result<()> {
        let req = a2a::prompt_request(session_id, &text);
        self.a2a_send(req, &pawl_a2a::Caller::Local, &Actor::internal("orchestrator"), a2a::Answers::Any).await.map(|_| ())
    }

    /// Prompt one of this machine's owned sessions with a picture (v0.2 item 4): only an ACP agent that
    /// advertises `promptCapabilities.image`; never an outside A2A agent (the picture would leave the
    /// machine). The picture is sent once and never stored (the `user_message` event says `image: true`).
    pub async fn prompt_with_images(&self, session_id: &str, text: String, images: Vec<ChatImage>) -> Result<()> {
        let image = one_image(images)?.ok_or_else(|| anyhow::Error::new(AckCode::Invalid).context("no picture"))?;
        let s = self.owned_open_session(session_id)?;
        if self.inner.remote.owns_session(session_id) || s.agent_id.starts_with(pawl_protocol::A2A_AGENT_PREFIX) {
            return Err(anyhow::Error::new(AckCode::Invalid)
                .context("pictures go only to this machine's own agents, never to an outside A2A agent"));
        }
        // A session another machine or an outside A2A caller drives could hand the picture on.
        let driven = self.st().remote_driven.contains(session_id);
        if driven || self.tasks().owner_of(session_id) != pawl_a2a::Caller::Local {
            return Err(anyhow::Error::new(AckCode::Invalid)
                .context("pictures go only to sessions this machine's own clients drive; this one is driven from another machine or an A2A caller"));
        }
        if !self.inner.host.accepts_images(session_id) {
            let name = self.st().agent_name(&s.agent_id);
            return Err(anyhow::Error::new(AckCode::Invalid).context(format!("{name} does not take pictures")));
        }
        self.a2a_prompt_with(session_id, text, vec![image], &pawl_a2a::Caller::Local).await.map(|_| ())
    }

    /// Stop the running turn of an owned session (A2A `CancelTask` on its task, `core/a2a.rs`).
    pub async fn cancel(&self, session_id: &str, actor: &Actor) -> Result<()> {
        self.cancel_as(session_id, actor, &pawl_a2a::Caller::Local).await
    }

    /// `session/cancel` to the agent of an owned session (the ACP executor behind the A2A facade).
    pub(super) async fn acp_cancel(&self, session_id: &str, actor: &Actor) -> Result<()> {
        self.owned_open_session(session_id)?;
        self.exec_cancel(session_id).await?;
        // ACP: after session/cancel the client answers every pending permission request with `cancelled`.
        let stale = self.st().take_pending_for_session(session_id);
        for req in stale {
            self.close_permission(req, ResolvedBy::User, actor, "turn cancelled").await;
        }
        Ok(())
    }

    /// End an owned session for good (`AcpHost::close_session`); the agent process stops once it
    /// serves no other session. The hub marks it ended right away; the host's upsert follows.
    pub async fn close_session(&self, session_id: &str) -> Result<()> {
        let s = self.owned_open_session(session_id)?;
        self.exec_close(session_id).await?;
        let mut ended = self.session(session_id).unwrap_or(s);
        if ended.ended_at.is_none() {
            ended.ended_at = Some(pawl_protocol::now_ms());
            if ended.status != SessionStatus::Error {
                ended.status = SessionStatus::Done;
            }
            self.merge_session(ended).await;
        }
        Ok(())
    }

    pub async fn list_history(&self, agent_id: &str, cwd: Option<String>) -> Result<Vec<HistorySession>> {
        let agent = self.find_agent(agent_id).await?;
        if agent.source == AgentSource::A2a {
            return Ok(Vec::new()); // outside agents keep their own history
        }
        let cwd = match cwd.filter(|c| !c.trim().is_empty()) {
            Some(c) => Some(resolve_cwd(&c)?),
            None => None,
        };
        self.inner.host.list_history(&agent, cwd).await
    }

    pub async fn resume_session(&self, agent_id: &str, acp_session_id: String, cwd: &str) -> Result<Session> {
        if let Some(s) = self.st().sessions.values().find(|s| {
            s.agent_id == agent_id
                && s.source == SessionSource::Owned
                && s.ended_at.is_none()
                && s.acp_session_id.as_deref() == Some(acp_session_id.as_str())
        }) {
            return Ok(s.clone());
        }
        let agent = self.find_agent(agent_id).await?;
        if agent.source == AgentSource::A2a {
            bail!("{} sessions cannot be resumed; start a new one", agent.name);
        }
        let cwd = resolve_cwd(cwd)?;
        let id = self.new_id();
        self.ensure_pet(&agent.id).await;
        let resumed = self
            .inner
            .host
            .resume_session(&agent, acp_session_id.clone(), cwd, id)
            .await
            .with_context(|| format!("could not resume {} session {acp_session_id}", agent.name))?;
        let session = self.merge_session(resumed).await;
        self.record_event(
            &session.id,
            EventType::SessionStart,
            json!({ "agentId": agent.id, "cwd": session.cwd, "resumed": true, "acpSessionId": acp_session_id }),
        )
        .await?;
        Ok(self.session(&session.id).unwrap_or(session))
    }

    pub async fn update_pet(&self, pet_id: &str, patch: PetPatch) -> Result<Pet> {
        fn clean(field: &str, v: &str, max: usize, allow_empty: bool) -> Result<String> {
            let v = v.trim();
            if v.is_empty() && !allow_empty {
                bail!("{field} must not be empty");
            }
            if v.chars().count() > max {
                bail!("{field} is longer than {max} characters");
            }
            Ok(v.to_string())
        }
        self.check_owned(pet_id)?;
        let mut remote_names = self.remote_pet_names();
        // The default pet's name is taken too.
        remote_names.extend(self.default_pet_name());
        let pet = {
            let mut st = self.st();
            let name = match &patch.name {
                Some(n) => {
                    let n = validate_pet_name(n)?;
                    let key = name_key(&n);
                    if st.pets.values().any(|p| p.id != pet_id && name_key(&p.name) == key)
                        || remote_names.iter().any(|r| name_key(r) == key)
                    {
                        return Err(AckCode::NameTaken.into());
                    }
                    Some(n)
                }
                None => None,
            };
            let pet = st.pets.get_mut(pet_id).ok_or(AckCode::NotFound)?;
            let mut next = pet.clone();
            if let Some(sp) = patch.species {
                next.species = sp;
            }
            if let Some(n) = name {
                next.name = n;
            }
            if let Some(s) = &patch.skin {
                next.skin = clean("skin", s, 40, false)?;
            }
            if let Some(p) = &patch.personality {
                next.personality = clean("personality", p, 300, true)?;
            }
            *pet = next.clone();
            st.dirty_pets.remove(pet_id);
            next
        };
        let p = pet.clone();
        self.inner.store.run(move |s| s.upsert_pet(&p)).await?;
        self.inner.bus.send(ServerMsg::Pet { pet: pet.clone() });
        self.refresh_presence();
        Ok(pet)
    }

    /// Validate (security floor first), then persist and apply. Any error changes nothing.
    pub async fn set_rules(&self, rules: Vec<Rule>) -> Result<Vec<Rule>> {
        pawl_brain::rules::validate_rules(&rules).map_err(|e| anyhow!("invalid_rule: {e}"))?;
        let mut seen = HashSet::new();
        for r in &rules {
            if r.id.trim().is_empty() {
                bail!("invalid_rule: every rule needs a non-empty id");
            }
            if !seen.insert(r.id.as_str()) {
                bail!("invalid_rule: duplicate rule id {:?}", r.id);
            }
        }
        let r2 = rules.clone();
        self.inner.store.run(move |s| s.set_rules(&r2)).await?;
        // A valid rule set replaces whatever was dropped at startup.
        self.inner.startup_notices.lock().unwrap_or_else(|p| p.into_inner()).clear();
        let pending = {
            let mut st = self.st();
            st.rules = rules.clone();
            st.sorted_pending()
        };
        // Requests already waiting on the user may now match an automatic rule.
        for req in pending {
            let action = self.rule_action(&rules, &req);
            if action != RuleAction::Ask {
                if let Some(opt) = auto_option(&req, action) {
                    if let Err(e) = self.resolve_permission(&req.request_id, opt, ResolvedBy::Rule, &Actor::rule(action)).await {
                        tracing::warn!("re-evaluating {} under new rules failed: {e:#}", req.request_id);
                    }
                }
            }
        }
        Ok(rules)
    }

    /// Rename this machine: validate (trimmed, 1-40 chars), persist machine.toml, broadcast
    /// `machine`. A malformed machine id is `invalid`; another machine's id is `not_owner`.
    pub async fn update_machine(&self, machine_id: &str, name: &str) -> Result<Machine> {
        if !is_valid_machine_id(machine_id) {
            return Err(AckCode::Invalid.into());
        }
        if machine_id != self.inner.machine_id {
            return Err(AckCode::NotOwner.into());
        }
        let name = machine::validate_name(name)?;
        let _write = self.inner.machine_write.lock().await;
        let next = MachineIdentity { id: self.inner.machine_id.clone(), name };
        let (paths, saved) = (self.inner.paths.clone(), next.clone());
        tokio::task::spawn_blocking(move || machine::save(&paths, &saved)).await.context("save machine.toml")??;
        *self.inner.machine.lock().unwrap_or_else(|p| p.into_inner()) = next;
        let machine = self.machine();
        tracing::info!("machine renamed to {:?}", machine.name);
        self.inner.peer.set_machine(machine.clone());
        self.refresh_presence();
        self.inner.bus.send(ServerMsg::Machine { machine: machine.clone() });
        Ok(machine)
    }

    // ------------------------------------------------------------ agents

    /// Re-run discovery (`force` bypasses the 24h registry cache), keep known capabilities,
    /// persist, broadcast `agents`, and give every installed agent its pet.
    pub async fn refresh_agents(&self, force: bool) -> Result<Vec<Agent>> {
        let _guard = self.inner.discovery.lock().await;
        let cfg = self.config();
        let mut found = discover_agents(&self.inner.paths, &cfg, &self.inner.machine_id, force).await?;
        // Outside A2A agents (Settings > A2A agents) next to the ACP ones.
        found.extend(self.a2a_agent_list());
        let agents = {
            let mut st = self.st();
            for a in found.iter_mut() {
                if a.capabilities.is_none() {
                    a.capabilities = st.agents.iter().find(|o| o.id == a.id).and_then(|o| o.capabilities.clone());
                }
            }
            st.agents = found.clone();
            found
        };
        let a2 = agents.clone();
        self.inner.store.run(move |s| s.replace_agents(&a2)).await?;
        self.inner.bus.send(ServerMsg::Agents { agents: agents.clone() });
        for a in agents.iter().filter(|a| a.installed) {
            self.ensure_pet(&a.id).await;
        }
        // The "acp" provider exists only while its agent is installed.
        self.rebuild_brain();
        self.refresh_presence();
        Ok(agents)
    }

    async fn find_agent(&self, agent_id: &str) -> Result<Agent> {
        if let Some(a) = self.st().agents.iter().find(|a| a.id == agent_id) {
            return Ok(a.clone());
        }
        let agents = self.refresh_agents(false).await?;
        agents.into_iter().find(|a| a.id == agent_id).ok_or_else(|| {
            let known: Vec<String> = self.st().agents.iter().map(|a| a.id.clone()).collect();
            anyhow!("unknown agent {agent_id:?} (known: {})", known.join(", "))
        })
    }

    async fn on_capabilities(&self, agent_id: AgentId, caps: AgentCapabilities) {
        let (agent, agents) = {
            let mut st = self.st();
            let Some(a) = st.agents.iter_mut().find(|a| a.id == agent_id) else { return };
            if a.capabilities.as_ref() == Some(&caps) {
                return;
            }
            a.capabilities = Some(caps);
            let a = a.clone();
            (a, st.agents.clone())
        };
        if let Err(e) = self.inner.store.run(move |s| s.upsert_agent(&agent)).await {
            tracing::warn!("persist agent capabilities: {e:#}");
        }
        self.inner.bus.send(ServerMsg::Agents { agents });
        self.refresh_presence();
    }

    // ------------------------------------------------------------ pets

    /// Every agent with activity gets a dog (pawl_brain::growth::default_pet), named so it never
    /// repeats the name of any pet this hub knows (its own and paired hubs').
    async fn ensure_pet(&self, agent_id: &str) {
        let mid = self.inner.machine_id.clone();
        let remote_names = self.remote_pet_names();
        let default_name = self.default_pet_name();
        let created = {
            let mut st = self.st();
            if st.pet_for(&mid, agent_id).is_some() {
                None
            } else {
                let mut agent =
                    st.agents.iter().find(|a| a.id == agent_id).cloned().unwrap_or_else(|| placeholder_agent(agent_id, &mid));
                agent.machine_id = mid.clone();
                let mut taken: Vec<String> = st.pets.values().map(|p| p.name.clone()).collect();
                taken.extend(remote_names);
                taken.extend(default_name.clone());
                let mut pet = pawl_brain::growth::default_pet(&agent, &taken);
                pet.agent_id = agent_id.to_string();
                pet.machine_id = mid.clone();
                if parse_global_id(&pet.id).is_none_or(|(m, _)| m != mid) || st.pets.contains_key(&pet.id) {
                    pet.id = global_id(&mid, format!("pet-{}", uuid::Uuid::new_v4()));
                }
                st.pets.insert(pet.id.clone(), pet.clone());
                Some(pet)
            }
        };
        if let Some(pet) = created {
            let p = pet.clone();
            if let Err(e) = self.inner.store.run(move |s| s.upsert_pet(&p)).await {
                tracing::warn!("persist pet: {e:#}");
            }
            self.inner.bus.send(ServerMsg::Pet { pet });
            self.refresh_presence();
        }
    }

    async fn flush_pets(&self) {
        let pets: Vec<Pet> = {
            let mut st = self.st();
            let ids: Vec<PetId> = st.dirty_pets.drain().collect();
            ids.iter().filter_map(|id| st.pets.get(id).cloned()).collect()
        };
        if pets.is_empty() {
            return;
        }
        let p2 = pets.clone();
        if let Err(e) = self
            .inner
            .store
            .run(move |s| {
                for p in &p2 {
                    s.upsert_pet(p)?;
                }
                Ok(())
            })
            .await
        {
            tracing::warn!("persist pets: {e:#}");
        }
        for pet in pets {
            self.inner.bus.send(ServerMsg::Pet { pet });
        }
        // Mood/xp changes reach peers at most once per flush interval.
        self.refresh_presence();
    }

    async fn pet_flush_loop(self) {
        let mut shutdown = self.shutdown_rx();
        let mut iv = tokio::time::interval(PET_FLUSH_EVERY);
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = iv.tick() => self.flush_pets().await,
                _ = shutdown.changed() => break,
            }
        }
    }

    // ------------------------------------------------------------ sessions & events

    /// A user prompt of a session whose title is still a placeholder (the fallback, "Pasted text", or
    /// empty): a prompt with real text becomes its title, and the session update is broadcast.
    async fn retitle_from_prompt(&self, session_id: &str, text: &str) {
        let updated = {
            let mut st = self.st();
            let Some((agent_id, cwd, title)) = st.sessions.get(session_id).map(|s| (s.agent_id.clone(), s.cwd.clone(), s.title.clone())) else {
                return;
            };
            let agent_name = st.agent_name(&agent_id);
            if !titles::is_placeholder(title.as_deref(), &agent_name, &cwd) {
                return;
            }
            let new = titles::next(title.as_deref(), titles::from_prompt(text), false, &agent_name, &cwd);
            if title.as_deref() == Some(new.as_str()) {
                return;
            }
            let s = st.sessions.get_mut(session_id).expect("looked up above");
            s.title = Some(new);
            s.clone()
        };
        self.persist_and_broadcast_session(&updated).await;
    }

    /// Upsert a session reported by the ACP host (owned sessions).
    async fn merge_session(&self, s: Session) -> Session {
        let (merged, stale) = {
            let mut st = self.st();
            let mut m = s;
            let prev_title = st.sessions.get(&m.id).and_then(|p| p.title.clone());
            if let Some(prev) = st.sessions.get(&m.id) {
                m.started_at = m.started_at.min(prev.started_at);
                m.last_event_at = m.last_event_at.max(prev.last_event_at);
                // The host's snapshots do not know what a session is for (a browser task): kept from the start.
                if m.purpose.is_none() {
                    m.purpose = prev.purpose.clone();
                }
            }
            // The agent's title (its first prompt, or one it set) when it is real text; else what we had,
            // else "<agent> in <folder>" until a real prompt arrives (see `retitle_from_prompt`).
            let candidate = m.title.as_deref().map(titles::from_prompt).unwrap_or(titles::Candidate::Nothing);
            let agent_name = st.agent_name(&m.agent_id);
            m.title = Some(titles::next(prev_title.as_deref(), candidate, true, &agent_name, &m.cwd));
            st.index_session(&m);
            st.sessions.insert(m.id.clone(), m.clone());
            let finished = m.ended_at.is_some() || matches!(m.status, SessionStatus::Done | SessionStatus::Error);
            let stale = if finished { st.take_pending_for_session(&m.id) } else { Vec::new() };
            (m, stale)
        };
        if merged.ended_at.is_some() {
            self.inner.tasks.on_session_ended(&merged.id);
            if merged.purpose.as_deref() == Some(browser::PURPOSE) && merged.machine_id == self.inner.machine_id {
                self.on_browser_session_ended(&merged);
            }
        }
        self.persist_and_broadcast_session(&merged).await;
        self.ensure_pet(&merged.agent_id).await;
        for req in stale {
            self.close_permission(req, ResolvedBy::System, &Actor::internal("hub:session_finished"), "session finished")
                .await;
        }
        merged
    }

    async fn persist_and_broadcast_session(&self, s: &Session) {
        let s2 = s.clone();
        if let Err(e) = self.inner.store.run(move |st| st.upsert_session(&s2)).await {
            tracing::warn!("persist session {}: {e:#}", s.id);
        }
        if s.machine_id == self.inner.machine_id {
            self.note_session_folder(&s.cwd);
        }
        self.inner.bus.send(ServerMsg::Session { session: s.clone() });
    }

    /// Store + broadcast an event, bump the session's `lastEventAt` and grow its pet.
    async fn record_event(&self, session_id: &str, kind: EventType, payload: Value) -> Result<HubEvent> {
        let ts = pawl_protocol::now_ms();
        let sid = session_id.to_string();
        let p = payload.clone();
        let event = self.inner.store.run(move |s| s.insert_event(&sid, kind, &p, ts)).await?;
        let owned = {
            let mut st = self.st();
            let owner = st.sessions.get_mut(session_id).map(|s| {
                s.last_event_at = s.last_event_at.max(ts);
                (s.machine_id.clone(), s.agent_id.clone(), s.source == SessionSource::Owned)
            });
            let owned = owner.as_ref().is_some_and(|(m, _, owned)| *owned && *m == self.inner.machine_id);
            let owner = owner.map(|(m, a, _)| (m, a));
            if let Some((machine_id, agent_id)) = owner {
                if let Some(pet) = st.pet_for_mut(&machine_id, &agent_id) {
                    if pawl_brain::growth::on_event(pet, kind, &payload) {
                        let id = pet.id.clone();
                        st.dirty_pets.insert(id);
                    }
                }
            }
            owned
        };
        // The A2A view of the session (its tasks) follows every event, in the order recorded.
        if owned {
            self.inner.tasks.on_event(&event);
        }
        self.inner.bus.send(ServerMsg::Event { event: event.clone() });
        Ok(event)
    }

    async fn host_loop(self, mut rx: mpsc::UnboundedReceiver<HostEvent>) {
        let mut shutdown = self.shutdown_rx();
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Some(ev) => self.on_host_event(ev).await,
                    None => break,
                },
                _ = shutdown.changed() => break,
            }
        }
    }

    pub(crate) async fn on_host_event(&self, ev: HostEvent) {
        // The brain's own agent session: collected for the brain, never a hub session.
        let brain = match &ev {
            HostEvent::Session(s) => brain_session::is_brain_session(&s.id),
            HostEvent::Event { session_id, .. } => brain_session::is_brain_session(session_id),
            HostEvent::PermissionRequest(r) => brain_session::is_brain_session(&r.session_id),
            HostEvent::AgentCapabilities { .. } => false,
        };
        if brain {
            self.on_brain_event(ev).await;
            return;
        }
        match ev {
            HostEvent::Session(s) => {
                self.merge_session(s).await;
            }
            HostEvent::Event { session_id, kind, payload } => {
                if let Err(e) = self.on_acp_event(&session_id, kind, payload).await {
                    tracing::warn!("record {} event for {session_id}: {e:#}", kind.as_str());
                }
            }
            HostEvent::PermissionRequest(req) => self.on_permission_request(req).await,
            HostEvent::AgentCapabilities { agent_id, capabilities } => self.on_capabilities(agent_id, capabilities).await,
        }
    }

    async fn on_acp_event(&self, session_id: &str, kind: EventType, payload: Value) -> Result<()> {
        if kind == EventType::PermissionResolved {
            self.on_host_permission_resolved(payload).await;
            return Ok(());
        }
        if kind == EventType::UserMessage {
            if let Some(text) = payload.get("text").and_then(Value::as_str) {
                self.retitle_from_prompt(session_id, text).await;
            }
        }
        let event = self.record_event(session_id, kind, payload).await?;
        // A browser task ends with its answer: the session is closed once its turn stops.
        if kind == EventType::Stop && self.is_browser_session(session_id) {
            let (hub, id) = (self.clone(), session_id.to_string());
            tokio::spawn(async move { hub.end_browser_session(&id, "answered").await });
        }
        let Some(session) = self.session(session_id) else { return Ok(()) };
        let agent_name = self.st().agent_name(&session.agent_id);
        let note = match kind {
            EventType::Stop => serde_json::from_value::<StopPayload>(event.payload.clone())
                .ok()
                .and_then(|stop| notify::for_stop(&agent_name, &session, &stop)),
            EventType::Error => {
                let msg = serde_json::from_value::<ErrorPayload>(event.payload.clone())
                    .map(|e| e.message)
                    .unwrap_or_else(|_| event.payload.to_string());
                Some(notify::for_error(&agent_name, &session, &msg))
            }
            _ => None,
        };
        if let Some(notification) = note {
            self.inner.bus.send(ServerMsg::Notification { notification });
        }
        Ok(())
    }

    // ------------------------------------------------------------ permissions

    /// Rule decision for a request. Relative tool paths are judged against the session's cwd.
    /// A session driven from another machine never gets auto_allow (deny rules still apply): a human
    /// answers every request it raises.
    fn rule_action(&self, rules: &[Rule], req: &PermissionRequest) -> RuleAction {
        let cwd = self.session(&req.session_id).map(|s| s.cwd).filter(|c| !c.is_empty());
        let action = pawl_brain::rules::evaluate_in(rules, req, cwd.as_deref());
        if action == RuleAction::AutoAllow && self.st().remote_driven.contains(&req.session_id) {
            return RuleAction::Ask;
        }
        action
    }

    /// Mark one of this machine's sessions as driven by another machine (persisted).
    pub(super) async fn mark_remote_driven(&self, session_id: &str) {
        if !self.st().remote_driven.insert(session_id.to_string()) {
            return;
        }
        let id = session_id.to_string();
        if let Err(e) = self.inner.store.run(move |s| s.mark_remote_driven(&id)).await {
            tracing::warn!("mark session {session_id} remote-driven: {e:#}");
        }
    }

    /// The host cancelled a pending request itself (turn cancelled/ended, agent withdrew it,
    /// process exit). If the hub still has it pending: audit, log, broadcast. If the hub already
    /// closed it (e.g. its own cancel path won the race) the echo is dropped.
    async fn on_host_permission_resolved(&self, payload: Value) {
        let p: PermissionResolvedPayload = match serde_json::from_value(payload) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("unparseable permission_resolved from the ACP host: {e}");
                return;
            }
        };
        let Some(req) = ({
            let mut st = self.st();
            let r = st.pending.remove(&p.request_id);
            if r.is_some() {
                st.mark_decided(&p.request_id);
            }
            r
        }) else {
            return;
        };
        let (option_id, option_kind, outcome) = match &p.outcome {
            PermissionOutcome::Cancelled => (None, None, AuditOutcome::Cancelled),
            PermissionOutcome::Selected { option_id } => {
                let kind = req.options.iter().find(|o| &o.option_id == option_id).map(|o| o.kind);
                let outcome = match kind {
                    Some(PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways) => AuditOutcome::Allowed,
                    Some(_) => AuditOutcome::Rejected,
                    None => AuditOutcome::Cancelled,
                };
                (Some(option_id.clone()), kind, outcome)
            }
        };
        self.audit(&req, p.by, &Actor::internal("acp-host"), option_id, option_kind, outcome, Some("resolved by the agent host".into()), None)
            .await;
        self.after_resolved(&req, p.outcome, p.by, None).await;
    }

    /// Rules first: auto_allow/deny resolve immediately (broadcast `permission_resolved` by "rule");
    /// ask → pending + `permission_request` + an alert notification.
    async fn on_permission_request(&self, req: PermissionRequest) {
        let rules = {
            let st = self.st();
            if st.pending.contains_key(&req.request_id) {
                return;
            }
            st.rules.clone()
        };
        let mut action = self.rule_action(&rules, &req);
        let mut actor = Actor::rule(action);
        let mut req = req;
        // A browser task's step: its own approval kinds (core/browser.rs), never a rule's auto-allow.
        if self.is_browser_session(&req.session_id) {
            let (card, verdict) = self.browser_verdict(req, action);
            req = card;
            action = verdict.action;
            actor = Self::browser_actor(&verdict);
        }
        if action != RuleAction::Ask {
            if let Some(opt) = auto_option(&req, action) {
                self.insert_pending(req.clone());
                match self.resolve_permission(&req.request_id, opt, ResolvedBy::Rule, &actor).await {
                    Ok(()) => return,
                    Err(e) => tracing::warn!("rule {action:?} could not resolve {}: {e:#}; asking instead", req.request_id),
                }
            }
        }
        let req = self.insert_pending(req);
        // History never keeps the voice code (it is only good while the request waits).
        let mut payload = serde_json::to_value(&req).unwrap_or_default();
        if let Value::Object(m) = &mut payload {
            m.remove("voiceCode");
        }
        if let Err(e) = self.record_event(&req.session_id, EventType::RequestPermission, payload).await {
            tracing::warn!("record permission request: {e:#}");
        }
        self.inner.bus.send(ServerMsg::PermissionRequest { request: req.clone() });
        // Paired phones that are offline get a content-free push through the relay to come and see it.
        self.wake_offline_phones();
        let (agent_name, pet_name) = {
            let st = self.st();
            (st.agent_name(&req.agent_id), st.pet_for(&req.machine_id, &req.agent_id).map(|p| p.name.clone()))
        };
        let notification = notify::for_permission(&agent_name, pet_name.as_deref(), &req);
        self.inner.bus.send(ServerMsg::Notification { notification });
    }

    /// Answer a pending permission request (user, chat or rule). Audited either way.
    /// Answer one of this hub's permission requests (local clients, rules, chat).
    pub async fn resolve_permission(&self, request_id: &str, option_id: Option<String>, by: ResolvedBy, actor: &Actor) -> Result<()> {
        self.decide(request_id, option_id, by, actor, None).await
    }

    /// The single decision path, shared by local clients, rules, chat and intents from paired
    /// hubs. **First valid decision wins**: the pending check, the option check and taking the
    /// request out happen under one lock, so of two answers racing for a request exactly one is
    /// applied and the other gets `already_decided`. A request that is not pending is
    /// `already_decided` if it was ever answered here (memory, then the audit log), else `not_found`.
    /// `remote`: an intent's decision (option kind checked: approve needs allow_*, deny reject_* or
    /// none; the audit entry and `permission_resolved` name the origin).
    pub(super) async fn decide(
        &self,
        request_id: &str,
        option_id: Option<String>,
        by: ResolvedBy,
        actor: &Actor,
        remote: Option<RemoteDecision>,
    ) -> Result<()> {
        self.decide_as(request_id, option_id, by, actor, remote, None).await
    }

    /// [`Hub::decide`]; `transcript`: the decision was spoken (audited with `via: "voice"` and the
    /// transcript, and announced with `via: "voice"`).
    pub(super) async fn decide_as(
        &self,
        request_id: &str,
        option_id: Option<String>,
        by: ResolvedBy,
        actor: &Actor,
        remote: Option<RemoteDecision>,
        transcript: Option<&str>,
    ) -> Result<()> {
        self.check_owned(request_id)?;
        // A request being decided right now (agent call in flight) is waited for: its real outcome
        // decides what a concurrent answer gets (applied after a rollback, else already_decided).
        let (req, option_kind) = loop {
            let wake = self.inner.decide_done.notified();
            tokio::pin!(wake);
            wake.as_mut().enable();
            let step = {
                let mut st = self.st();
                if let Some(req) = st.pending.get(request_id) {
                    let option_kind = match &option_id {
                        Some(o) => match req.options.iter().find(|x| &x.option_id == o) {
                            Some(x) => Some(x.kind),
                            None => {
                                return Err(anyhow::Error::new(AckCode::Invalid)
                                    .context(format!("unknown optionId {o:?} for permission request {request_id}")));
                            }
                        },
                        None => None,
                    };
                    if let Some(r) = &remote {
                        check_remote_option(r.approve, option_kind)?;
                    }
                    let req = st.pending.remove(request_id).expect("checked above");
                    st.deciding.insert(request_id.to_string());
                    Step::Take(Box::new(req), option_kind)
                } else if st.deciding.contains(request_id) {
                    Step::Wait
                } else {
                    Step::Gone(st.decided.contains(request_id))
                }
            };
            match step {
                Step::Take(req, kind) => break (*req, kind),
                Step::Wait => wake.await,
                Step::Gone(true) => return Err(AckCode::AlreadyDecided.into()),
                Step::Gone(false) => {
                    let rid = request_id.to_string();
                    let decided = self.inner.store.run(move |s| s.request_was_decided(&rid)).await.unwrap_or(false);
                    return Err(if decided { AckCode::AlreadyDecided } else { AckCode::NotFound }.into());
                }
            }
        };
        let result = self.exec_resolve(request_id, option_id.clone()).await;
        let outcome = match (&result, option_kind) {
            (Err(_), _) => AuditOutcome::Failed,
            (Ok(()), None) => AuditOutcome::Cancelled,
            (Ok(()), Some(PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways)) => AuditOutcome::Allowed,
            (Ok(()), Some(_)) => AuditOutcome::Rejected,
        };
        let origin = remote.as_ref().map(|r| r.origin.clone());
        self.audit_as(&req, by, actor, option_id.clone(), option_kind, outcome, result.as_ref().err().map(|e| format!("{e:#}")), origin, transcript)
            .await;
        if let Err(e) = result {
            {
                let mut st = self.st();
                st.deciding.remove(&req.request_id);
                st.pending.insert(req.request_id.clone(), req);
            }
            self.inner.decide_done.notify_waiters();
            return Err(e.context("the agent did not accept the permission answer"));
        }
        {
            let mut st = self.st();
            st.deciding.remove(&req.request_id);
            st.mark_decided(&req.request_id);
        }
        self.inner.decide_done.notify_waiters();
        let outcome = match option_id {
            Some(option_id) => PermissionOutcome::Selected { option_id },
            None => PermissionOutcome::Cancelled,
        };
        let via = transcript.map(|_| voice::VOICE);
        self.after_resolved_via(&req, outcome, by, remote.map(|r| r.origin.machine_id), via).await;
        Ok(())
    }

    /// Drop a pending request that can no longer be answered normally (turn cancelled, session over).
    async fn close_permission(&self, req: PermissionRequest, by: ResolvedBy, actor: &Actor, why: &str) {
        let r = self.exec_resolve(&req.request_id, None).await;
        let detail = match &r {
            Ok(()) => why.to_string(),
            Err(e) => format!("{why} (agent: {e:#})"),
        };
        self.st().mark_decided(&req.request_id);
        self.audit(&req, by, actor, None, None, AuditOutcome::Cancelled, Some(detail), None).await;
        self.after_resolved(&req, PermissionOutcome::Cancelled, by, None).await;
    }

    /// `by_machine_id`: the machine the decision came from when it was not this one.
    async fn after_resolved(&self, req: &PermissionRequest, outcome: PermissionOutcome, by: ResolvedBy, by_machine_id: Option<MachineId>) {
        self.after_resolved_via(req, outcome, by, by_machine_id, None).await
    }

    /// `via`: Some("voice") when the user answered by speaking.
    async fn after_resolved_via(
        &self,
        req: &PermissionRequest,
        outcome: PermissionOutcome,
        by: ResolvedBy,
        by_machine_id: Option<MachineId>,
        via: Option<&str>,
    ) {
        let payload = PermissionResolvedPayload {
            request_id: req.request_id.clone(),
            outcome: outcome.clone(),
            by,
            by_machine_id: by_machine_id.clone(),
            via: via.map(str::to_string),
        };
        let mut payload = serde_json::to_value(&payload).unwrap_or_else(|_| json!({}));
        if let (Value::Object(m), Ok(tc)) = (&mut payload, serde_json::to_value(&req.tool_call)) {
            m.insert("toolCall".into(), tc);
            // The kind of the chosen option, so history can say "approved"/"denied" after a reload.
            if let PermissionOutcome::Selected { option_id } = &outcome {
                if let Some(o) = req.options.iter().find(|o| &o.option_id == option_id) {
                    m.insert("optionKind".into(), serde_json::to_value(o.kind).unwrap_or(Value::Null));
                }
            }
        }
        if let Err(e) = self.record_event(&req.session_id, EventType::PermissionResolved, payload).await {
            tracing::warn!("record permission resolution: {e:#}");
        }
        self.inner.bus.send(ServerMsg::PermissionResolved {
            request_id: req.request_id.clone(),
            session_id: req.session_id.clone(),
            outcome,
            by,
            by_machine_id,
        });
        // The agent is unblocked; it reports its real status with the next update.
        let flipped = {
            let mut st = self.st();
            let still_waiting = st.pending.values().any(|p| p.session_id == req.session_id);
            match st.sessions.get_mut(&req.session_id) {
                Some(s) if !still_waiting && s.status == SessionStatus::WaitingPermission && s.ended_at.is_none() => {
                    s.status = SessionStatus::Working;
                    Some(s.clone())
                }
                _ => None,
            }
        };
        if let Some(s) = flipped {
            self.persist_and_broadcast_session(&s).await;
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn audit(
        &self,
        req: &PermissionRequest,
        by: ResolvedBy,
        actor: &Actor,
        option_id: Option<String>,
        option_kind: Option<PermissionOptionKind>,
        outcome: AuditOutcome,
        detail: Option<String>,
        origin: Option<AuditOrigin>,
    ) {
        self.audit_as(req, by, actor, option_id, option_kind, outcome, detail, origin, None).await
    }

    /// [`Hub::audit`]; `transcript`: a spoken decision (recorded with `via: "voice"`).
    #[allow(clippy::too_many_arguments)]
    async fn audit_as(
        &self,
        req: &PermissionRequest,
        by: ResolvedBy,
        actor: &Actor,
        option_id: Option<String>,
        option_kind: Option<PermissionOptionKind>,
        outcome: AuditOutcome,
        detail: Option<String>,
        origin: Option<AuditOrigin>,
        transcript: Option<&str>,
    ) {
        let entry = AuditEntry {
            id: 0,
            ts: pawl_protocol::now_ms(),
            machine_id: req.machine_id.clone(),
            request_id: req.request_id.clone(),
            session_id: req.session_id.clone(),
            agent_id: req.agent_id.clone(),
            tool_call_id: Some(req.tool_call.tool_call_id.clone()).filter(|t| !t.is_empty()),
            tool_kind: req.tool_call.kind,
            tool_title: req.tool_call.title.clone(),
            locations: req.tool_call.locations.iter().flatten().map(|l| l.path.clone()).collect(),
            decided_by: by,
            actor: actor.label.clone(),
            option_id,
            option_kind,
            outcome,
            detail,
            origin,
            via: transcript.map(|_| voice::VOICE.to_string()),
            transcript: transcript.map(voice::cut_transcript),
        };
        tracing::info!(
            request = %entry.request_id, session = %entry.session_id, actor = %entry.actor,
            outcome = ?entry.outcome, origin = ?entry.origin.as_ref().map(|o| &o.machine_id), "permission decision"
        );
        if let Err(e) = self.inner.store.run(move |s| s.insert_audit(&entry)).await {
            tracing::error!("audit log write failed: {e:#}");
        }
    }

    // ------------------------------------------------------------ hooks

    /// Merge one raw hook payload (`POST /hooks/{source}`) into the session/event feed.
    pub async fn ingest_hook(&self, source: HookSource, raw: Value) -> Result<()> {
        let Some(n) = pawl_discovery::hooks::normalize(source, raw) else {
            return Ok(());
        };
        // The brain's agent session runs with hooks off; a report from its folder is dropped all the same.
        if n.cwd.as_deref().is_some_and(|c| brain_session::in_brain_dir(&self.inner.paths.data_dir, c)) {
            return Ok(());
        }
        // A browser task is an owned session already (hooks are off in its folder too).
        if n.cwd.as_deref().is_some_and(|c| browser::in_browser_dir(&self.inner.paths.data_dir, c)) {
            return Ok(());
        }
        let now = pawl_protocol::now_ms();
        let session = {
            let mut st = self.st();
            let key = (n.agent_id.clone(), n.external_session_id.clone());
            let existing = st.by_external.get(&key).and_then(|id| st.sessions.get(id)).cloned();
            let mut s = existing.unwrap_or_else(|| Session {
                id: global_id(&st.machine_id, uuid::Uuid::new_v4()),
                machine_id: st.machine_id.clone(),
                agent_id: n.agent_id.clone(),
                acp_session_id: None,
                external_id: Some(n.external_session_id.clone()),
                cwd: String::new(),
                title: None,
                source: n.source,
                status: SessionStatus::Idle,
                started_at: now,
                last_event_at: now,
                ended_at: None,
                purpose: None,
            });
            if let Some(status) = n.status {
                s.status = status;
            }
            if let Some(cwd) = n.cwd.as_ref().filter(|c| !c.is_empty()) {
                if s.cwd.is_empty() {
                    s.cwd = cwd.clone();
                }
            }
            // The hook's title (an explicit session title, else the prompt, clipped), else the full prompt
            // text of a prompt event; injected text never counts. Real text follows the latest prompt.
            let mut candidate = n.title.as_deref().map(titles::from_prompt).unwrap_or(titles::Candidate::Nothing);
            let is_prompt = matches!(n.payload.hook_event.to_ascii_lowercase().as_str(), "userpromptsubmit" | "userpromptsubmitted");
            if is_prompt && !matches!(candidate, titles::Candidate::Real(_)) {
                match n.payload.message.as_deref().map(titles::from_prompt) {
                    Some(c @ titles::Candidate::Real(_)) => candidate = c,
                    Some(titles::Candidate::Pasted) => candidate = titles::Candidate::Pasted,
                    _ => {}
                }
            }
            let agent_name = st.agent_name(&s.agent_id);
            s.title = Some(titles::next(s.title.as_deref(), candidate, true, &agent_name, &s.cwd));
            if n.source == SessionSource::VscodeHook {
                s.source = SessionSource::VscodeHook;
            }
            if n.ended {
                s.ended_at = Some(now);
                if !matches!(s.status, SessionStatus::Done | SessionStatus::Error) {
                    s.status = SessionStatus::Done;
                }
            } else if s.ended_at.is_some() {
                s.ended_at = None; // the same external session came back (e.g. resumed)
            }
            s.last_event_at = s.last_event_at.max(now);
            st.index_session(&s);
            st.sessions.insert(s.id.clone(), s.clone());
            s
        };
        self.persist_and_broadcast_session(&session).await;
        self.ensure_pet(&session.agent_id).await;
        self.record_event(&session.id, n.event, serde_json::to_value(&n.payload)?).await?;
        if let Some((level, title, body)) = n.notification {
            // Claude can report one permission prompt twice (PermissionRequest, then
            // Notification(permission_prompt) a few seconds later): alert once per window.
            let duplicate = level == NotificationLevel::Alert && {
                let mut st = self.st();
                let last = st.last_hook_alert.insert(session.id.clone(), now);
                last.is_some_and(|t| now - t < HOOK_ALERT_DEDUPE_MS)
            };
            if !duplicate {
                let notification =
                    notify::make(&session.machine_id, level, title, body, Some(session.id.clone()), Some(session.agent_id.clone()));
                self.inner.bus.send(ServerMsg::Notification { notification });
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ chat

    /// Store + broadcast the user's line, answer asynchronously as a `chat` ServerMsg.
    pub async fn chat(&self, pet_id: Option<PetId>, text: String, actor: &Actor) -> Result<ChatMessage> {
        self.chat_as(pet_id, text, false, actor).await
    }

    /// [`Hub::chat`]; `spoken`: the text is a speech transcript (`via: "voice"`).
    pub async fn chat_as(&self, pet_id: Option<PetId>, text: String, spoken: bool, actor: &Actor) -> Result<ChatMessage> {
        self.chat_with(pet_id, text, spoken, None, actor).await
    }

    /// [`Hub::chat_as`] with at most one picture (checked by the caller, [`one_image`]). The picture is used
    /// for the reply only: never stored (the message says `image: true`), mirrored or logged. With a picture
    /// the text may be empty.
    pub async fn chat_with(&self, pet_id: Option<PetId>, text: String, spoken: bool, image: Option<ChatImage>, actor: &Actor) -> Result<ChatMessage> {
        let text = text.trim().to_string();
        if text.is_empty() && image.is_none() {
            bail!("chat text is empty");
        }
        if text.chars().count() > MAX_CHAT_CHARS {
            bail!("chat text is longer than {MAX_CHAT_CHARS} characters");
        }
        let pet = match &pet_id {
            Some(id) => {
                self.check_owned(id)?;
                Some(self.st().pets.get(id).cloned().ok_or(AckCode::NotFound)?)
            }
            // No pet attached: the default pet, which orchestrates every agent (replies carry no pet id).
            None => None,
        };
        let msg = ChatMessage {
            id: self.new_id(),
            machine_id: self.inner.machine_id.clone(),
            pet_id: pet.as_ref().map(|p| p.id.clone()),
            role: ChatRole::User,
            text,
            ts: pawl_protocol::now_ms(),
            session_id: None,
            proposals: Vec::new(),
            image: image.is_some(),
        };
        let m2 = msg.clone();
        self.inner.store.run(move |s| s.insert_chat(&m2)).await?;
        self.inner.bus.send(ServerMsg::Chat { message: msg.clone() });
        let hub = self.clone();
        let (m3, actor) = (msg.clone(), actor.clone());
        tokio::spawn(async move { hub.chat_reply(m3, pet, actor, spoken, image).await });
        Ok(msg)
    }

    /// Pet of the agent with the most recent open session, else the first pet (the daily report's voice).
    fn default_chat_pet(&self) -> Option<Pet> {
        let st = self.st();
        let active = st.sessions_sorted(|s| s.ended_at.is_none());
        active
            .iter()
            .find_map(|s| st.pet_for(&s.machine_id, &s.agent_id).cloned())
            .or_else(|| st.sorted_pets().into_iter().next())
    }

    async fn brain_context(&self, pet: Option<Pet>) -> BrainContext {
        let now = pawl_protocol::now_ms();
        let recent = |s: &Session| s.ended_at.is_none() || now - s.last_event_at <= DAY_MS;
        // Mirrored sessions and requests too: status replies name their machine. Actions on them
        // are refused by the brain (remote control is a later step) and by `check_owned`.
        let (mut sessions, mut pending) = (self.mirror_sessions(recent), self.mirror_pending());
        let agents = {
            let st = self.st();
            sessions.extend(st.sessions_sorted(recent));
            pending.extend(st.sorted_pending());
            st.agents.clone()
        };
        sessions.sort_by_key(|s| std::cmp::Reverse(s.last_event_at));
        pending.sort_by_key(|p| p.created_at);
        // Recent milestones plus the latest streaming text, so "what's Claude doing?" has both.
        let recent_events = self
            .inner
            .store
            .run(|s| {
                // This machine's events first; mirrored ones get at most half the budget, so a
                // chatty peer cannot crowd out local context (or flood the prompt with its text).
                let dedupe = |mut v: Vec<HubEvent>| {
                    // seq is per machine: order by time, dedupe by the global id.
                    v.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.cmp(&b.id)));
                    v.dedup_by(|a, b| a.id == b.id);
                    v
                };
                let mut local = s.recent_events_of(true, BRAIN_EVENTS / 2, true)?;
                local.extend(s.recent_events_of(true, BRAIN_EVENTS / 2, false)?);
                let mut remote = s.recent_events_of(false, BRAIN_EVENTS / 4, true)?;
                remote.extend(s.recent_events_of(false, BRAIN_EVENTS / 4, false)?);
                let mut v = dedupe(local);
                v.extend(dedupe(remote));
                Ok(dedupe(v))
            })
            .await
            .unwrap_or_default();
        // Paired hubs' agents and every machine's repo index, so "tell the Mac mini's Claude to ... in
        // repo X" resolves; every machine's pets, so "tell Biscuit to ..." does. The brain only ever
        // proposes commands for them (the user confirms in a client).
        let mut remote_agents = std::collections::BTreeMap::new();
        let mut repos = std::collections::BTreeMap::new();
        repos.insert(self.inner.machine_id.clone(), self.repos());
        let hubs: Vec<String> =
            self.devices().into_iter().filter(|d| d.kind == pawl_protocol::DeviceKind::Hub).map(|d| d.id).collect();
        for id in hubs {
            remote_agents.insert(id.clone(), self.peer_agents(&id).await.unwrap_or_default());
            repos.insert(id.clone(), self.repos_of(Some(&id)).await.unwrap_or_default());
        }
        let mut pets = self.pets();
        pets.extend(self.mirror_pets());
        sort_pets(&mut pets);
        BrainContext {
            machine: self.machine(),
            machines: self.machines(),
            agents,
            remote_agents,
            repos,
            pets,
            sessions,
            recent_events,
            pending,
            pet,
            default_agent: self.cfg().brain.default_agent.clone().filter(|a| !a.trim().is_empty()),
            model_pending: false,
            pet_name: self.default_pet_name(),
            // The orchestrator's status reads the A2A tasks its delegations created.
            tasks: self
                .inner
                .tasks
                .latest_per_context()
                .into_iter()
                .take(BRAIN_TASKS)
                .map(|t| pawl_brain::chat::TaskBrief {
                    session_id: t.context_id.clone(),
                    task_id: t.id.clone(),
                    state: pawl_a2a::mapping::state_name(&t.status.state).to_string(),
                    text: t.status.message.as_ref().and_then(|m| m.text()).map(|s| s.chars().take(200).collect()),
                })
                .collect(),
            // Typed chat; `chat_reply` sets these for a speech transcript.
            spoken: false,
            read_back: None,
            voice_locked: false,
            browser: self.browser_ready(),
        }
    }

    async fn chat_reply(&self, user: ChatMessage, pet: Option<Pet>, actor: Actor, spoken: bool, image: Option<ChatImage>) {
        let (brain, pending) = self.ready_brain().await;
        let mut ctx = self.brain_context(pet.clone()).await;
        ctx.model_pending = pending;
        if let Some(image) = image {
            // A question about a picture: answered by a model that can see, never an action. The picture is
            // dropped when this returns.
            let reply = brain.look(&user.text, &image, &ctx).await;
            drop(image);
            self.deliver_reply(pet, reply, &actor, None).await;
            return;
        }
        // A read-back is good for the chat's next message only, and only a spoken one uses it.
        let now = pawl_protocol::now_ms();
        let pet_id = pet.as_ref().map(|p| p.id.clone());
        let read_back = self.take_read_back(pet_id.as_ref(), now);
        if spoken {
            ctx.spoken = true;
            ctx.read_back = read_back;
            ctx.voice_locked = self.voice_locked(now);
        }
        let reply = match brain.handle(&user.text, &ctx).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("brain failed: {e:#}");
                BrainReply { text: "*whimper* My brain got tangled. Try me again in a moment?".into(), ..Default::default() }
            }
        };
        if spoken {
            self.note_voice_reply(pet_id.as_ref(), reply.read_back.clone(), reply.code_miss, now);
        }
        let transcript = spoken.then_some(user.text.as_str());
        self.deliver_reply(pet, reply, &actor, transcript).await;
    }

    /// A brain reply becomes the pet's chat message: a local action runs here (as before);
    /// commands for another machine are only proposed: they are stored and broadcast with the
    /// message, and the user confirms one in a client, which sends the ordinary start_session /
    /// prompt (step-4 rules apply).
    /// `transcript`: the user spoke the message (`via: "voice"`): approvals follow the voice rules.
    pub(super) async fn deliver_reply(&self, pet: Option<Pet>, reply: BrainReply, actor: &Actor, transcript: Option<&str>) -> ChatMessage {
        let mut text = reply.text;
        let mut session_id = None;
        let mut proposals = reply.proposals;
        proposals.truncate(MAX_PROPOSALS);
        if let Some(action) = reply.action {
            match self.execute_action(action, actor, transcript).await {
                Ok(sid) => session_id = sid,
                Err(e) => text.push_str(&format!("\n\n(I couldn't do that: {e:#})")),
            }
        }
        let msg = ChatMessage {
            id: self.new_id(),
            machine_id: self.inner.machine_id.clone(),
            pet_id: pet.map(|p| p.id),
            role: ChatRole::Pet,
            text,
            ts: pawl_protocol::now_ms(),
            session_id,
            proposals,
            image: false,
        };
        let m2 = msg.clone();
        if let Err(e) = self.inner.store.run(move |s| s.insert_chat(&m2)).await {
            tracing::warn!("persist chat reply: {e:#}");
        }
        self.inner.bus.send(ServerMsg::Chat { message: msg.clone() });
        msg
    }

    async fn execute_action(&self, action: BrainAction, actor: &Actor, transcript: Option<&str>) -> Result<Option<SessionId>> {
        match action {
            BrainAction::StartSession { agent_id, cwd, prompt } => {
                let prompt = Some(prompt).filter(|p| !p.trim().is_empty());
                let (s, _warning) = self.start_session(&agent_id, &cwd, prompt).await?;
                Ok(Some(s.id))
            }
            BrainAction::Prompt { session_id, text } => {
                self.prompt(&session_id, text).await?;
                Ok(Some(session_id))
            }
            BrainAction::ResolvePermission { request_id, option_id } if transcript.is_some() => {
                self.resolve_by_voice(&request_id, option_id, &actor.via_chat(), transcript.unwrap_or_default()).await
            }
            BrainAction::ResolvePermission { request_id, option_id } => {
                let sid = self.st().pending.get(&request_id).map(|p| p.session_id.clone());
                self.resolve_permission(&request_id, option_id, ResolvedBy::User, &actor.via_chat()).await?;
                Ok(sid)
            }
        }
    }

    // ------------------------------------------------------------ watchdog & daily report

    /// Ticks every `[watchdog] tick_secs` (re-read after every settings change).
    async fn watchdog_loop(self) {
        let mut shutdown = self.shutdown_rx();
        loop {
            let secs = self.cfg().watchdog.tick_secs.clamp(5, 3600);
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(secs)) => self.watchdog_tick().await,
                _ = self.inner.settings_changed.notified() => {}
                _ = shutdown.changed() => break,
            }
        }
    }

    pub(crate) async fn watchdog_tick(&self) {
        let now = pawl_protocol::now_ms();
        let (sessions, pending) = {
            let st = self.st();
            (st.sessions_sorted(|s| s.ended_at.is_none()), st.sorted_pending())
        };
        let notes = {
            let mut wd = self.inner.watchdog.lock().unwrap_or_else(|p| p.into_inner());
            wd.tick(&sessions, &pending, now)
        };
        for notification in notes {
            self.inner.bus.send(ServerMsg::Notification { notification });
        }

        // Hook sessions never get a final word if the app was killed; close them after a while.
        let stale_ms = self.cfg().hook_session_stale_hours as i64 * 3_600_000;
        let stale: Vec<Session> = if stale_ms > 0 {
            let mut st = self.st();
            st.sessions
                .values_mut()
                .filter(|s| s.source != SessionSource::Owned && s.ended_at.is_none() && now - s.last_event_at > stale_ms)
                .map(|s| {
                    s.ended_at = Some(now);
                    if s.status != SessionStatus::Error {
                        s.status = SessionStatus::Done;
                    }
                    s.clone()
                })
                .collect()
        } else {
            Vec::new()
        };
        for s in &stale {
            self.persist_and_broadcast_session(s).await;
        }

        // Idle owned sessions hold an agent process; close them after a while.
        let idle_ms = self.cfg().owned_session_idle_hours as i64 * 3_600_000;
        if idle_ms > 0 {
            let idle: Vec<SessionId> = {
                let st = self.st();
                st.sessions
                    .values()
                    .filter(|s| {
                        s.source == SessionSource::Owned
                            && s.ended_at.is_none()
                            && matches!(s.status, SessionStatus::Idle | SessionStatus::Done | SessionStatus::Error)
                            && now - s.last_event_at > idle_ms
                            && !st.pending.values().any(|p| p.session_id == s.id)
                    })
                    .map(|s| s.id.clone())
                    .collect()
            };
            for id in idle {
                tracing::info!("closing idle owned session {id}");
                if let Err(e) = self.close_session(&id).await {
                    tracing::warn!("close idle session {id}: {e:#}");
                }
            }
        }

        // Keep the cache bounded.
        let mut st = self.st();
        let old: Vec<(SessionId, Option<String>, AgentId)> = st
            .sessions
            .values()
            .filter(|s| s.ended_at.is_some() && now - s.last_event_at > CACHE_WINDOW_MS)
            .map(|s| (s.id.clone(), s.external_id.clone(), s.agent_id.clone()))
            .collect();
        for (id, ext, agent) in old {
            st.sessions.remove(&id);
            st.last_hook_alert.remove(&id);
            if let Some(ext) = ext {
                if st.by_external.get(&(agent.clone(), ext.clone())) == Some(&id) {
                    st.by_external.remove(&(agent, ext));
                }
            }
        }
    }

    /// Checks every minute against `daily_report_time` (re-read each time: settings can change it).
    async fn daily_report_loop(self) {
        let mut shutdown = self.shutdown_rx();
        let mut iv = tokio::time::interval(Duration::from_secs(60));
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = iv.tick() => {}
                _ = shutdown.changed() => break,
            }
            let Some(minutes) = self.cfg().daily_report_minutes() else { continue };
            use chrono::Timelike;
            let now = chrono::Local::now();
            let cur = now.hour() * 60 + now.minute();
            // Only inside the hour after the configured time, once per local date.
            if cur < minutes || cur >= minutes + 60 {
                continue;
            }
            let today = now.format("%Y-%m-%d").to_string();
            let t2 = today.clone();
            let already = self
                .inner
                .store
                .run(move |s| {
                    let done = s.setting(SETTING_DAILY_REPORT)?.as_deref() == Some(t2.as_str());
                    if !done {
                        s.set_setting(SETTING_DAILY_REPORT, &t2)?;
                    }
                    Ok(done)
                })
                .await;
            match already {
                Ok(false) => self.send_daily_report().await,
                Ok(true) => {}
                Err(e) => tracing::warn!("daily report bookkeeping: {e:#}"),
            }
        }
    }

    /// End-of-day report of every session active in the last 24h, as a notification + pet chat line.
    pub async fn send_daily_report(&self) {
        let since = pawl_protocol::now_ms() - DAY_MS;
        let data = self
            .inner
            .store
            .run(move |s| {
                let sessions: Vec<Session> =
                    s.sessions_active_since(since)?.into_iter().filter(|x| x.last_event_at >= since).collect();
                Ok((sessions, s.events_since(since, 5000)?))
            })
            .await;
        let (sessions, events) = match data {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("daily report query: {e:#}");
                return;
            }
        };
        let text = match self.brain().daily_report(&sessions, &events).await {
            Ok(t) if !t.trim().is_empty() => t,
            Ok(_) => fallback_report(&sessions),
            Err(e) => {
                tracing::warn!("brain daily report failed: {e:#}");
                fallback_report(&sessions)
            }
        };
        let notification = notify::make(
            &self.inner.machine_id,
            NotificationLevel::Info,
            "Daily report",
            notify::truncate(&text, 1000),
            None,
            None,
        );
        self.inner.bus.send(ServerMsg::Notification { notification });
        let pet = self.default_chat_pet();
        let msg = ChatMessage {
            id: self.new_id(),
            machine_id: self.inner.machine_id.clone(),
            pet_id: pet.map(|p| p.id),
            role: ChatRole::Pet,
            text,
            ts: pawl_protocol::now_ms(),
            session_id: None,
            proposals: Vec::new(),
            image: false,
        };
        let m2 = msg.clone();
        if let Err(e) = self.inner.store.run(move |s| s.insert_chat(&m2)).await {
            tracing::warn!("persist daily report: {e:#}");
        }
        self.inner.bus.send(ServerMsg::Chat { message: msg });
    }
}

/// Rule decision → option to send. None = no suitable option (fall back to asking).
/// At most one picture per message, checked ([`pawl_brain::chat::vision::check_image`]): PNG or JPEG, at most
/// 4 MiB and 2048 px on its long side. Errors never quote the picture.
pub(crate) fn one_image(images: Vec<ChatImage>) -> Result<Option<ChatImage>> {
    use pawl_brain::chat::vision;
    if images.len() > vision::MAX_IMAGES {
        return Err(anyhow::Error::new(AckCode::Invalid).context("at most one picture per message"));
    }
    let Some(image) = images.into_iter().next() else { return Ok(None) };
    vision::check_image(&image).map_err(|e| anyhow::Error::new(AckCode::Invalid).context(e))?;
    Ok(Some(image))
}

fn auto_option(req: &PermissionRequest, action: RuleAction) -> Option<Option<String>> {
    match (action, pawl_brain::rules::pick_option(req, action)) {
        (RuleAction::Ask, _) => None,
        (_, Some(o)) => Some(Some(o)),
        // Deny without a reject option: answering `cancelled` still refuses the tool call.
        (RuleAction::Deny, None) => Some(None),
        (RuleAction::AutoAllow, None) => None,
    }
}

/// Load-time guard: keep rules that pass `pawl_brain::rules::validate_rules` (the security floor)
/// and have a unique, non-empty id. Returns the kept rules and `(id, reason)` for each dropped one.
pub fn sanitize_rules(rules: Vec<Rule>) -> (Vec<Rule>, Vec<(String, String)>) {
    let mut kept = Vec::with_capacity(rules.len());
    let mut dropped = Vec::new();
    let mut seen = HashSet::new();
    for r in rules {
        if let Err(e) = pawl_brain::rules::validate_rules(std::slice::from_ref(&r)) {
            dropped.push((r.id.clone(), e.to_string()));
        } else if r.id.trim().is_empty() {
            dropped.push((r.id.clone(), "empty rule id".into()));
        } else if !seen.insert(r.id.clone()) {
            dropped.push((r.id.clone(), "duplicate rule id".into()));
        } else {
            kept.push(r);
        }
    }
    (kept, dropped)
}

/// Comparison key for pet names: trimmed, case-insensitive.
fn sort_pets(pets: &mut [Pet]) {
    pets.sort_by(|a, b| a.machine_id.cmp(&b.machine_id).then(a.agent_id.cmp(&b.agent_id)).then(a.id.cmp(&b.id)));
}

fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Trimmed pet name, 1..=40 characters, no control or invisible formatting characters (pet names
/// reach paired devices: the same rule as device names, `pawl_peer::device_name_ok`); else `invalid`.
pub fn validate_pet_name(name: &str) -> Result<String> {
    let name = name.trim();
    let len = name.chars().count();
    if len == 0 || len > MAX_PET_NAME_CHARS || !name.chars().all(machine::name_char_ok) {
        return Err(anyhow::Error::new(AckCode::Invalid).context(format!(
            "pet name must be 1-{MAX_PET_NAME_CHARS} characters without control or invisible characters (bidi controls, zero-width)"
        )));
    }
    Ok(name.to_string())
}

/// Make pet names unique (trimmed, case-insensitive). `pets` is oldest first: the oldest pet keeps a
/// name, later duplicates (and blank names) of this machine's pets get
/// `growth::default_name(machine, agent, taken)`. Other machines' pets are never renamed here (their
/// owner names them). A clash with another machine's pet is settled the same way on both hubs: the
/// pet of the machine whose id sorts first keeps the name, so exactly one owner renames. Returns the
/// indices of renamed pets.
pub fn dedupe_pet_names(pets: &mut [Pet], local_machine_id: &str) -> Vec<usize> {
    let mut taken: Vec<String> = pets.iter().map(|p| p.name.clone()).collect();
    let mut kept: HashSet<String> = pets
        .iter()
        .filter(|p| p.machine_id != local_machine_id && p.machine_id.as_str() < local_machine_id)
        .map(|p| name_key(&p.name))
        .collect();
    let mut renamed = Vec::new();
    for (i, pet) in pets.iter_mut().enumerate().filter(|(_, p)| p.machine_id == local_machine_id) {
        let key = name_key(&pet.name);
        if !key.is_empty() && kept.insert(key) {
            continue;
        }
        let name = pawl_brain::growth::default_name(&pet.machine_id, &pet.agent_id, &taken);
        kept.insert(name_key(&name));
        taken.push(name.clone());
        pet.name = name;
        renamed.push(i);
    }
    renamed
}

fn fallback_report(sessions: &[Session]) -> String {
    if sessions.is_empty() {
        return "Quiet day: no agent sessions in the last 24 hours. *naps*".to_string();
    }
    let done = sessions.iter().filter(|s| s.status == SessionStatus::Done).count();
    let errors = sessions.iter().filter(|s| s.status == SessionStatus::Error).count();
    let open = sessions.iter().filter(|s| s.ended_at.is_none()).count();
    let mut lines = vec![format!(
        "{} session(s) in the last 24 hours: {done} done, {errors} with errors, {open} still open.",
        sessions.len()
    )];
    for s in sessions.iter().take(10) {
        lines.push(format!("- {} ({}): {:?}", notify::session_label(s), s.agent_id, s.status));
    }
    lines.join("\n")
}

/// Discovery with an optional cache bypass: the 24h registry cache is moved aside so the registry is
/// fetched again, and restored if that fetch did not produce a new cache.
pub async fn discover_agents(paths: &Paths, cfg: &HubConfig, machine_id: &str, force: bool) -> Result<Vec<Agent>> {
    let dcfg = cfg.discovery_config(paths, machine_id);
    if !force || cfg.offline {
        return pawl_discovery::discover(&dcfg).await;
    }
    let cache = paths.registry_cache.clone();
    let backup = cache.with_extension("json.bak");
    let moved = cache.exists() && std::fs::rename(&cache, &backup).is_ok();
    let result = pawl_discovery::discover(&dcfg).await;
    if moved {
        if cache.exists() {
            let _ = std::fs::remove_file(&backup);
        } else {
            let _ = std::fs::rename(&backup, &cache);
            return pawl_discovery::discover(&dcfg).await;
        }
    }
    result
}

/// Absolute, existing directory (`~` expanded). Never canonicalized (avoids `\\?\` paths on Windows).
pub fn resolve_cwd(cwd: &str) -> Result<PathBuf> {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        bail!("cwd is required");
    }
    let expanded = if cwd == "~" || cwd.starts_with("~/") || cwd.starts_with("~\\") {
        let home = dirs_home().context("no home directory to expand ~")?;
        home.join(cwd[1..].trim_start_matches(['/', '\\']))
    } else {
        PathBuf::from(cwd)
    };
    let abs = std::path::absolute(&expanded).with_context(|| format!("bad cwd {cwd:?}"))?;
    if !abs.is_dir() {
        bail!("cwd {} is not an existing directory", abs.display());
    }
    Ok(abs)
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn placeholder_agent(agent_id: &str, machine_id: &str) -> Agent {
    Agent {
        id: agent_id.to_string(),
        machine_id: machine_id.to_string(),
        name: pretty_agent_name(agent_id),
        command: Vec::new(),
        env: None,
        version: None,
        description: None,
        icon: None,
        installed: false,
        source: AgentSource::Custom,
        capabilities: None,
    }
}

/// "claude-code" → "Claude Code".
pub fn pretty_agent_name(agent_id: &str) -> String {
    agent_id
        .split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[allow(dead_code)]
fn _assert_send_sync() {
    fn is<T: Send + Sync + Clone + 'static>() {}
    is::<Hub>();
}

#[cfg(test)]
mod tests;
