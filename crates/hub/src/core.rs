//! Hub core: in-memory cache over the store; merges the ACP host feed (owned sessions) and agent
//! hook feeds into sessions + events; applies permission rules; answers every `ClientMsg`.

mod host;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use pet_acp::HostEvent;
use pet_brain::chat::{Brain, BrainAction, BrainContext, BrainReply};
use pet_brain::watchdog::Watchdog;
use pet_protocol::{
    Agent, AgentCapabilities, AgentId, AgentSource, ChatMessage, ChatRole, ClientKind, ClientMsg, ErrorPayload,
    EventType, HistorySession, HookSource, HubEvent, NotificationLevel, PermissionOptionKind, PermissionOutcome,
    PermissionRequest, PermissionResolvedPayload, Pet, PetId, PetPatch, RequestId, ResolvedBy, Rule, RuleAction,
    ServerMsg, Session, SessionId, SessionSource, SessionStatus, StopPayload, UnixMs, Welcome, PROTOCOL_VERSION,
};
use serde_json::{json, Value};
use tokio::sync::{broadcast, mpsc, watch};

use crate::bus::Bus;
use crate::config::{HubConfig, Paths};
use crate::notify;
use crate::store::{AuditEntry, AuditOutcome, Store};
use host::Host;

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

/// Who asked for something (recorded in the audit log).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    pub label: String,
}

impl Actor {
    pub fn client(kind: ClientKind, conn_id: u64) -> Self {
        let k = match kind {
            ClientKind::Desktop => "desktop",
            ClientKind::Vscode => "vscode",
            ClientKind::Mobile => "mobile",
            ClientKind::Cli => "cli",
        };
        Self { label: format!("{k}#{conn_id}") }
    }

    pub fn internal(label: impl Into<String>) -> Self {
        Self { label: label.into() }
    }

    fn via_chat(&self) -> Self {
        Self { label: format!("chat:{}", self.label) }
    }

    fn rule(action: RuleAction) -> Self {
        let a = match action {
            RuleAction::AutoAllow => "auto_allow",
            RuleAction::Ask => "ask",
            RuleAction::Deny => "deny",
        };
        Self { label: format!("rule:{a}") }
    }
}

pub struct HubOptions {
    pub paths: Paths,
    pub config: HubConfig,
    pub token: String,
    /// Daemon mode: run initial discovery, the watchdog and the daily report.
    /// In-process CLI use (`pet-hub start` without a running hub) passes false.
    pub background: bool,
}

#[derive(Default)]
struct State {
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
}

impl State {
    fn index_session(&mut self, s: &Session) {
        if let Some(ext) = &s.external_id {
            self.by_external.insert((s.agent_id.clone(), ext.clone()), s.id.clone());
        }
    }

    fn pet_for_agent_mut(&mut self, agent_id: &str) -> Option<&mut Pet> {
        self.pets.values_mut().find(|p| p.agent_id == agent_id)
    }

    fn pet_for_agent(&self, agent_id: &str) -> Option<&Pet> {
        self.pets.values().find(|p| p.agent_id == agent_id)
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
    config: HubConfig,
    token: String,
    store: Store,
    bus: Bus,
    host: Host,
    brain: Brain,
    state: Mutex<State>,
    watchdog: Mutex<Watchdog>,
    discovery: tokio::sync::Mutex<()>,
    shutdown: watch::Sender<bool>,
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
        let store = tokio::task::spawn_blocking(move || Store::open(&db)).await.context("open store")??;
        let (tx, rx) = mpsc::unbounded_channel();
        let hub = Self::build(opts, store, Host::acp(tx)).await?;
        tokio::spawn(hub.clone().host_loop(rx));
        Ok(hub)
    }

    async fn build(opts: HubOptions, store: Store, host: Host) -> Result<Hub> {
        let HubOptions { paths, config, token, background } = opts;
        let now = pet_protocol::now_ms();
        let retention_days = config.event_retention_days;
        let (agents, mut sessions, pets, db_rules) = store
            .run(move |s| {
                if retention_days > 0 {
                    let n = s.prune_events_before(now - retention_days as i64 * DAY_MS)?;
                    if n > 0 {
                        tracing::info!("pruned {n} events older than {retention_days} days");
                    }
                }
                Ok((s.agents()?, s.sessions_active_since(now - CACHE_WINDOW_MS)?, s.pets()?, s.rules()?))
            })
            .await?;

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

        let rules = db_rules.or_else(|| config.rules.clone()).unwrap_or_else(pet_brain::rules::default_rules);
        let mut state = State { agents, rules, ..Default::default() };
        for s in sessions {
            state.index_session(&s);
            state.sessions.insert(s.id.clone(), s);
        }
        for p in pets {
            state.pets.insert(p.id.clone(), p);
        }

        let (shutdown, _) = watch::channel(false);
        let hub = Hub {
            inner: Arc::new(Inner {
                brain: Brain::new(config.brain.to_brain_config()),
                watchdog: Mutex::new(Watchdog::new(config.watchdog.to_watchdog_config())),
                paths,
                config,
                token,
                store,
                bus: Bus::default(),
                host,
                state: Mutex::new(state),
                discovery: tokio::sync::Mutex::new(()),
                shutdown,
            }),
        };

        tokio::spawn(hub.clone().pet_flush_loop());
        if background {
            let h = hub.clone();
            tokio::spawn(async move {
                if let Err(e) = h.refresh_agents(false).await {
                    tracing::warn!("agent discovery failed: {e:#}");
                }
            });
            tokio::spawn(hub.clone().watchdog_loop());
            tokio::spawn(hub.clone().daily_report_loop());
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

    pub fn config(&self) -> &HubConfig {
        &self.inner.config
    }

    pub fn paths(&self) -> &Paths {
        &self.inner.paths
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
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

    pub fn welcome(&self) -> Welcome {
        let now = pet_protocol::now_ms();
        let st = self.st();
        Welcome {
            protocol_version: PROTOCOL_VERSION,
            hub_version: crate::HUB_VERSION.to_string(),
            agents: st.agents.clone(),
            sessions: st.sessions_sorted(|s| {
                s.ended_at.is_none() || now - s.last_event_at.max(s.ended_at.unwrap_or(0)) <= WELCOME_ENDED_WINDOW_MS
            }),
            pets: st.sorted_pets(),
            pending_permissions: st.sorted_pending(),
            rules: st.rules.clone(),
        }
    }

    /// Stop background tasks, flush pets and shut down every agent connection.
    pub async fn shutdown(&self) {
        let _ = self.inner.shutdown.send(true);
        self.flush_pets().await;
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
            Err(e) => ServerMsg::ack_err(id, format!("{e:#}")),
        }
    }

    async fn dispatch(&self, msg: ClientMsg, actor: &Actor) -> Result<Value> {
        Ok(match msg {
            ClientMsg::Hello { .. } => serde_json::to_value(self.welcome())?,
            ClientMsg::ListAgents { refresh, .. } => {
                let agents = if refresh == Some(true) { self.refresh_agents(true).await? } else { self.agents() };
                json!({ "agents": agents })
            }
            ClientMsg::ListSessions { include_ended, .. } => {
                json!({ "sessions": self.list_sessions(include_ended.unwrap_or(false)).await? })
            }
            ClientMsg::GetEvents { session_id, since_id, limit, .. } => {
                json!({ "events": self.events(&session_id, since_id, limit).await? })
            }
            ClientMsg::StartSession { agent_id, cwd, prompt, .. } => {
                json!({ "session": self.start_session(&agent_id, &cwd, prompt).await? })
            }
            ClientMsg::Prompt { session_id, text, .. } => {
                self.prompt(&session_id, text).await?;
                json!({})
            }
            ClientMsg::Cancel { session_id, .. } => {
                self.cancel(&session_id, actor).await?;
                json!({})
            }
            ClientMsg::ResolvePermission { request_id, option_id, .. } => {
                self.resolve_permission(&request_id, option_id, ResolvedBy::User, actor).await?;
                json!({})
            }
            ClientMsg::ListHistory { agent_id, cwd, .. } => {
                json!({ "sessions": self.list_history(&agent_id, cwd).await? })
            }
            ClientMsg::ResumeSession { agent_id, acp_session_id, cwd, .. } => {
                json!({ "session": self.resume_session(&agent_id, acp_session_id, &cwd).await? })
            }
            ClientMsg::Chat { pet_id, text, .. } => json!({ "message": self.chat(pet_id, text, actor).await? }),
            ClientMsg::UpdatePet { pet_id, patch, .. } => json!({ "pet": self.update_pet(&pet_id, patch).await? }),
            ClientMsg::SetRules { rules, .. } => json!({ "rules": self.set_rules(rules).await? }),
        })
    }

    pub async fn list_sessions(&self, include_ended: bool) -> Result<Vec<Session>> {
        if !include_ended {
            return Ok(self.st().sessions_sorted(|s| s.ended_at.is_none()));
        }
        let mut stored = self.inner.store.run(|s| s.recent_sessions(500)).await?;
        let st = self.st();
        for s in stored.iter_mut() {
            if let Some(c) = st.sessions.get(&s.id) {
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

    pub async fn events(&self, session_id: &str, since_id: Option<i64>, limit: Option<u32>) -> Result<Vec<HubEvent>> {
        let limit = limit.unwrap_or(DEFAULT_EVENTS_LIMIT).clamp(1, MAX_EVENTS_LIMIT);
        let sid = session_id.to_string();
        self.inner.store.run(move |s| s.events(&sid, since_id, limit)).await
    }

    /// Start an owned ACP session. `prompt` (if any) is logged and sent right after `session/new`.
    pub async fn start_session(&self, agent_id: &str, cwd: &str, prompt: Option<String>) -> Result<Session> {
        let agent = self.find_agent(agent_id).await?;
        let cwd = resolve_cwd(cwd)?;
        let id = uuid::Uuid::new_v4().to_string();
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
        if let Some(p) = prompt.filter(|p| !p.trim().is_empty()) {
            self.prompt(&session.id, p).await.context("session started, but sending the prompt failed")?;
        }
        Ok(self.session(&session.id).unwrap_or(session))
    }

    fn owned_open_session(&self, session_id: &str) -> Result<Session> {
        let s = self.session(session_id).ok_or_else(|| anyhow!("unknown session {session_id}"))?;
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

    pub async fn prompt(&self, session_id: &str, text: String) -> Result<()> {
        self.owned_open_session(session_id)?;
        if text.trim().is_empty() {
            bail!("prompt text is empty");
        }
        self.record_event(session_id, EventType::UserMessage, json!({ "text": text })).await?;
        self.inner.host.prompt(session_id, text).await
    }

    pub async fn cancel(&self, session_id: &str, actor: &Actor) -> Result<()> {
        self.owned_open_session(session_id)?;
        self.inner.host.cancel(session_id).await?;
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
        self.inner.host.close_session(session_id).await?;
        let mut ended = self.session(session_id).unwrap_or(s);
        if ended.ended_at.is_none() {
            ended.ended_at = Some(pet_protocol::now_ms());
            if ended.status != SessionStatus::Error {
                ended.status = SessionStatus::Done;
            }
            self.merge_session(ended).await;
        }
        Ok(())
    }

    pub async fn list_history(&self, agent_id: &str, cwd: Option<String>) -> Result<Vec<HistorySession>> {
        let agent = self.find_agent(agent_id).await?;
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
        let cwd = resolve_cwd(cwd)?;
        let id = uuid::Uuid::new_v4().to_string();
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
        let pet = {
            let mut st = self.st();
            let pet = st.pets.get_mut(pet_id).ok_or_else(|| anyhow!("unknown pet {pet_id}"))?;
            let mut next = pet.clone();
            if let Some(sp) = patch.species {
                next.species = sp;
            }
            if let Some(n) = &patch.name {
                next.name = clean("name", n, 40, false)?;
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
        Ok(pet)
    }

    pub async fn set_rules(&self, rules: Vec<Rule>) -> Result<Vec<Rule>> {
        let mut seen = HashSet::new();
        for r in &rules {
            if r.id.trim().is_empty() {
                bail!("every rule needs a non-empty id");
            }
            if !seen.insert(r.id.as_str()) {
                bail!("duplicate rule id {:?}", r.id);
            }
        }
        let r2 = rules.clone();
        self.inner.store.run(move |s| s.set_rules(&r2)).await?;
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

    // ------------------------------------------------------------ agents

    /// Re-run discovery (`force` bypasses the 24h registry cache), keep known capabilities,
    /// persist, broadcast `agents`, and give every installed agent its pet.
    pub async fn refresh_agents(&self, force: bool) -> Result<Vec<Agent>> {
        let _guard = self.inner.discovery.lock().await;
        let mut found = discover_agents(&self.inner.paths, &self.inner.config, force).await?;
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
    }

    // ------------------------------------------------------------ pets

    /// Every agent with activity gets a dog (pet_brain::growth::default_pet).
    async fn ensure_pet(&self, agent_id: &str) {
        let created = {
            let mut st = self.st();
            if st.pet_for_agent(agent_id).is_some() {
                None
            } else {
                let agent = st.agents.iter().find(|a| a.id == agent_id).cloned().unwrap_or_else(|| placeholder_agent(agent_id));
                let mut pet = pet_brain::growth::default_pet(&agent);
                pet.agent_id = agent_id.to_string();
                if pet.id.trim().is_empty() || st.pets.contains_key(&pet.id) {
                    pet.id = format!("pet-{}", uuid::Uuid::new_v4());
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

    /// Upsert a session reported by the ACP host (owned sessions).
    async fn merge_session(&self, s: Session) -> Session {
        let (merged, stale) = {
            let mut st = self.st();
            let mut m = s;
            if let Some(prev) = st.sessions.get(&m.id) {
                if m.title.is_none() {
                    m.title = prev.title.clone();
                }
                m.started_at = m.started_at.min(prev.started_at);
                m.last_event_at = m.last_event_at.max(prev.last_event_at);
            }
            st.index_session(&m);
            st.sessions.insert(m.id.clone(), m.clone());
            let finished = m.ended_at.is_some() || matches!(m.status, SessionStatus::Done | SessionStatus::Error);
            let stale = if finished { st.take_pending_for_session(&m.id) } else { Vec::new() };
            (m, stale)
        };
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
        self.inner.bus.send(ServerMsg::Session { session: s.clone() });
    }

    /// Store + broadcast an event, bump the session's `lastEventAt` and grow its pet.
    async fn record_event(&self, session_id: &str, kind: EventType, payload: Value) -> Result<HubEvent> {
        let ts = pet_protocol::now_ms();
        let sid = session_id.to_string();
        let p = payload.clone();
        let event = self.inner.store.run(move |s| s.insert_event(&sid, kind, &p, ts)).await?;
        {
            let mut st = self.st();
            let agent_id = st.sessions.get_mut(session_id).map(|s| {
                s.last_event_at = s.last_event_at.max(ts);
                s.agent_id.clone()
            });
            if let Some(agent_id) = agent_id {
                if let Some(pet) = st.pet_for_agent_mut(&agent_id) {
                    if pet_brain::growth::on_event(pet, kind, &payload) {
                        let id = pet.id.clone();
                        st.dirty_pets.insert(id);
                    }
                }
            }
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
        let event = self.record_event(session_id, kind, payload).await?;
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
    fn rule_action(&self, rules: &[Rule], req: &PermissionRequest) -> RuleAction {
        let cwd = self.session(&req.session_id).map(|s| s.cwd).filter(|c| !c.is_empty());
        pet_brain::rules::evaluate_in(rules, req, cwd.as_deref())
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
        let Some(req) = self.st().pending.remove(&p.request_id) else { return };
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
        self.audit(&req, p.by, &Actor::internal("acp-host"), option_id, option_kind, outcome, Some("resolved by the agent host".into()))
            .await;
        self.after_resolved(&req, p.outcome, p.by).await;
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
        let action = self.rule_action(&rules, &req);
        if action != RuleAction::Ask {
            if let Some(opt) = auto_option(&req, action) {
                self.st().pending.insert(req.request_id.clone(), req.clone());
                match self.resolve_permission(&req.request_id, opt, ResolvedBy::Rule, &Actor::rule(action)).await {
                    Ok(()) => return,
                    Err(e) => tracing::warn!("rule {action:?} could not resolve {}: {e:#}; asking instead", req.request_id),
                }
            }
        }
        self.st().pending.insert(req.request_id.clone(), req.clone());
        if let Err(e) = self.record_event(&req.session_id, EventType::RequestPermission, serde_json::to_value(&req).unwrap_or_default()).await {
            tracing::warn!("record permission request: {e:#}");
        }
        self.inner.bus.send(ServerMsg::PermissionRequest { request: req.clone() });
        let (agent_name, pet_name) = {
            let st = self.st();
            (st.agent_name(&req.agent_id), st.pet_for_agent(&req.agent_id).map(|p| p.name.clone()))
        };
        let title = req.tool_call.title.clone().unwrap_or_else(|| "a tool call".to_string());
        let notification = notify::for_permission(&agent_name, pet_name.as_deref(), &title, &req.session_id, &req.agent_id);
        self.inner.bus.send(ServerMsg::Notification { notification });
    }

    /// Answer a pending permission request (user, chat or rule). Audited either way.
    pub async fn resolve_permission(&self, request_id: &str, option_id: Option<String>, by: ResolvedBy, actor: &Actor) -> Result<()> {
        // Take it out first so two clients answering at once cannot both resolve it.
        let req = self
            .st()
            .pending
            .remove(request_id)
            .ok_or_else(|| anyhow!("no pending permission request {request_id}"))?;
        let option_kind = match &option_id {
            Some(o) => match req.options.iter().find(|x| &x.option_id == o) {
                Some(x) => Some(x.kind),
                None => {
                    self.st().pending.insert(req.request_id.clone(), req);
                    bail!("unknown optionId {o:?} for permission request {request_id}");
                }
            },
            None => None,
        };
        let result = self.inner.host.resolve_permission(request_id, option_id.clone()).await;
        let outcome = match (&result, option_kind) {
            (Err(_), _) => AuditOutcome::Failed,
            (Ok(()), None) => AuditOutcome::Cancelled,
            (Ok(()), Some(PermissionOptionKind::AllowOnce | PermissionOptionKind::AllowAlways)) => AuditOutcome::Allowed,
            (Ok(()), Some(_)) => AuditOutcome::Rejected,
        };
        self.audit(&req, by, actor, option_id.clone(), option_kind, outcome, result.as_ref().err().map(|e| format!("{e:#}")))
            .await;
        if let Err(e) = result {
            self.st().pending.insert(req.request_id.clone(), req);
            return Err(e.context("the agent did not accept the permission answer"));
        }
        let outcome = match option_id {
            Some(option_id) => PermissionOutcome::Selected { option_id },
            None => PermissionOutcome::Cancelled,
        };
        self.after_resolved(&req, outcome, by).await;
        Ok(())
    }

    /// Drop a pending request that can no longer be answered normally (turn cancelled, session over).
    async fn close_permission(&self, req: PermissionRequest, by: ResolvedBy, actor: &Actor, why: &str) {
        let r = self.inner.host.resolve_permission(&req.request_id, None).await;
        let detail = match &r {
            Ok(()) => why.to_string(),
            Err(e) => format!("{why} (agent: {e:#})"),
        };
        self.audit(&req, by, actor, None, None, AuditOutcome::Cancelled, Some(detail)).await;
        self.after_resolved(&req, PermissionOutcome::Cancelled, by).await;
    }

    async fn after_resolved(&self, req: &PermissionRequest, outcome: PermissionOutcome, by: ResolvedBy) {
        let payload = PermissionResolvedPayload { request_id: req.request_id.clone(), outcome: outcome.clone(), by };
        let mut payload = serde_json::to_value(&payload).unwrap_or_else(|_| json!({}));
        if let (Value::Object(m), Ok(tc)) = (&mut payload, serde_json::to_value(&req.tool_call)) {
            m.insert("toolCall".into(), tc);
        }
        if let Err(e) = self.record_event(&req.session_id, EventType::PermissionResolved, payload).await {
            tracing::warn!("record permission resolution: {e:#}");
        }
        self.inner.bus.send(ServerMsg::PermissionResolved {
            request_id: req.request_id.clone(),
            session_id: req.session_id.clone(),
            outcome,
            by,
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
    ) {
        let entry = AuditEntry {
            id: 0,
            ts: pet_protocol::now_ms(),
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
        };
        tracing::info!(
            request = %entry.request_id, session = %entry.session_id, actor = %entry.actor,
            outcome = ?entry.outcome, "permission decision"
        );
        if let Err(e) = self.inner.store.run(move |s| s.insert_audit(&entry)).await {
            tracing::error!("audit log write failed: {e:#}");
        }
    }

    // ------------------------------------------------------------ hooks

    /// Merge one raw hook payload (`POST /hooks/{source}`) into the session/event feed.
    pub async fn ingest_hook(&self, source: HookSource, raw: Value) -> Result<()> {
        let Some(n) = pet_discovery::hooks::normalize(source, raw) else {
            return Ok(());
        };
        let now = pet_protocol::now_ms();
        let session = {
            let mut st = self.st();
            let key = (n.agent_id.clone(), n.external_session_id.clone());
            let existing = st.by_external.get(&key).and_then(|id| st.sessions.get(id)).cloned();
            let mut s = existing.unwrap_or_else(|| Session {
                id: uuid::Uuid::new_v4().to_string(),
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
            });
            if let Some(status) = n.status {
                s.status = status;
            }
            if let Some(t) = n.title.as_ref().map(|t| t.trim()).filter(|t| !t.is_empty()) {
                s.title = Some(t.to_string());
            }
            if let Some(cwd) = n.cwd.as_ref().filter(|c| !c.is_empty()) {
                if s.cwd.is_empty() {
                    s.cwd = cwd.clone();
                }
            }
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
                let notification = notify::make(level, title, body, Some(session.id.clone()), Some(session.agent_id.clone()));
                self.inner.bus.send(ServerMsg::Notification { notification });
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ chat

    /// Store + broadcast the user's line, answer asynchronously as a `chat` ServerMsg.
    pub async fn chat(&self, pet_id: Option<PetId>, text: String, actor: &Actor) -> Result<ChatMessage> {
        let text = text.trim().to_string();
        if text.is_empty() {
            bail!("chat text is empty");
        }
        if text.chars().count() > MAX_CHAT_CHARS {
            bail!("chat text is longer than {MAX_CHAT_CHARS} characters");
        }
        let pet = match &pet_id {
            Some(id) => Some(self.st().pets.get(id).cloned().ok_or_else(|| anyhow!("unknown pet {id}"))?),
            None => self.default_chat_pet(),
        };
        let msg = ChatMessage {
            id: uuid::Uuid::new_v4().to_string(),
            pet_id: pet.as_ref().map(|p| p.id.clone()),
            role: ChatRole::User,
            text,
            ts: pet_protocol::now_ms(),
            session_id: None,
        };
        let m2 = msg.clone();
        self.inner.store.run(move |s| s.insert_chat(&m2)).await?;
        self.inner.bus.send(ServerMsg::Chat { message: msg.clone() });
        let hub = self.clone();
        let (m3, actor) = (msg.clone(), actor.clone());
        tokio::spawn(async move { hub.chat_reply(m3, pet, actor).await });
        Ok(msg)
    }

    /// Pet of the agent with the most recent open session, else the first pet.
    fn default_chat_pet(&self) -> Option<Pet> {
        let st = self.st();
        let active = st.sessions_sorted(|s| s.ended_at.is_none());
        active
            .iter()
            .find_map(|s| st.pet_for_agent(&s.agent_id).cloned())
            .or_else(|| st.sorted_pets().into_iter().next())
    }

    async fn brain_context(&self, pet: Option<Pet>) -> BrainContext {
        let (agents, sessions, pending) = {
            let st = self.st();
            let now = pet_protocol::now_ms();
            (
                st.agents.clone(),
                st.sessions_sorted(|s| s.ended_at.is_none() || now - s.last_event_at <= DAY_MS),
                st.sorted_pending(),
            )
        };
        // Recent milestones plus the latest streaming text, so "what's Claude doing?" has both.
        let recent_events = self
            .inner
            .store
            .run(|s| {
                let mut v = s.recent_events(200, true)?;
                v.extend(s.recent_events(200, false)?);
                v.sort_by_key(|e| e.id);
                v.dedup_by_key(|e| e.id);
                Ok(v)
            })
            .await
            .unwrap_or_default();
        BrainContext { agents, sessions, recent_events, pending, pet }
    }

    async fn chat_reply(&self, user: ChatMessage, pet: Option<Pet>, actor: Actor) {
        let ctx = self.brain_context(pet.clone()).await;
        let reply = match self.inner.brain.handle(&user.text, &ctx).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("brain failed: {e:#}");
                BrainReply { text: "*whimper* My brain got tangled. Try me again in a moment?".into(), action: None }
            }
        };
        let mut text = reply.text;
        let mut session_id = None;
        if let Some(action) = reply.action {
            match self.execute_action(action, &actor).await {
                Ok(sid) => session_id = sid,
                Err(e) => text.push_str(&format!("\n\n(I couldn't do that: {e:#})")),
            }
        }
        let msg = ChatMessage {
            id: uuid::Uuid::new_v4().to_string(),
            pet_id: pet.map(|p| p.id),
            role: ChatRole::Pet,
            text,
            ts: pet_protocol::now_ms(),
            session_id,
        };
        let m2 = msg.clone();
        if let Err(e) = self.inner.store.run(move |s| s.insert_chat(&m2)).await {
            tracing::warn!("persist chat reply: {e:#}");
        }
        self.inner.bus.send(ServerMsg::Chat { message: msg });
    }

    async fn execute_action(&self, action: BrainAction, actor: &Actor) -> Result<Option<SessionId>> {
        match action {
            BrainAction::StartSession { agent_id, cwd, prompt } => {
                let prompt = Some(prompt).filter(|p| !p.trim().is_empty());
                let s = self.start_session(&agent_id, &cwd, prompt).await?;
                Ok(Some(s.id))
            }
            BrainAction::Prompt { session_id, text } => {
                self.prompt(&session_id, text).await?;
                Ok(Some(session_id))
            }
            BrainAction::ResolvePermission { request_id, option_id } => {
                let sid = self.st().pending.get(&request_id).map(|p| p.session_id.clone());
                self.resolve_permission(&request_id, option_id, ResolvedBy::User, &actor.via_chat()).await?;
                Ok(sid)
            }
        }
    }

    // ------------------------------------------------------------ watchdog & daily report

    async fn watchdog_loop(self) {
        let mut shutdown = self.shutdown_rx();
        let secs = self.inner.config.watchdog.tick_secs.max(5);
        let mut iv = tokio::time::interval(Duration::from_secs(secs));
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = iv.tick() => self.watchdog_tick().await,
                _ = shutdown.changed() => break,
            }
        }
    }

    pub(crate) async fn watchdog_tick(&self) {
        let now = pet_protocol::now_ms();
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
        let stale_ms = self.inner.config.hook_session_stale_hours as i64 * 3_600_000;
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
        let idle_ms = self.inner.config.owned_session_idle_hours as i64 * 3_600_000;
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

    async fn daily_report_loop(self) {
        let Some(minutes) = self.inner.config.daily_report_minutes() else { return };
        let mut shutdown = self.shutdown_rx();
        let mut iv = tokio::time::interval(Duration::from_secs(60));
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = iv.tick() => {}
                _ = shutdown.changed() => break,
            }
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
        let since = pet_protocol::now_ms() - DAY_MS;
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
        let text = match self.inner.brain.daily_report(&sessions, &events).await {
            Ok(t) if !t.trim().is_empty() => t,
            Ok(_) => fallback_report(&sessions),
            Err(e) => {
                tracing::warn!("brain daily report failed: {e:#}");
                fallback_report(&sessions)
            }
        };
        let notification = notify::make(NotificationLevel::Info, "Daily report", notify::truncate(&text, 1000), None, None);
        self.inner.bus.send(ServerMsg::Notification { notification });
        let pet = self.default_chat_pet();
        let msg = ChatMessage {
            id: uuid::Uuid::new_v4().to_string(),
            pet_id: pet.map(|p| p.id),
            role: ChatRole::Pet,
            text,
            ts: pet_protocol::now_ms(),
            session_id: None,
        };
        let m2 = msg.clone();
        if let Err(e) = self.inner.store.run(move |s| s.insert_chat(&m2)).await {
            tracing::warn!("persist daily report: {e:#}");
        }
        self.inner.bus.send(ServerMsg::Chat { message: msg });
    }
}

/// Rule decision → option to send. None = no suitable option (fall back to asking).
fn auto_option(req: &PermissionRequest, action: RuleAction) -> Option<Option<String>> {
    match (action, pet_brain::rules::pick_option(req, action)) {
        (RuleAction::Ask, _) => None,
        (_, Some(o)) => Some(Some(o)),
        // Deny without a reject option: answering `cancelled` still refuses the tool call.
        (RuleAction::Deny, None) => Some(None),
        (RuleAction::AutoAllow, None) => None,
    }
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
pub async fn discover_agents(paths: &Paths, cfg: &HubConfig, force: bool) -> Result<Vec<Agent>> {
    let dcfg = cfg.discovery_config(paths);
    if !force || cfg.offline {
        return pet_discovery::discover(&dcfg).await;
    }
    let cache = paths.registry_cache.clone();
    let backup = cache.with_extension("json.bak");
    let moved = cache.exists() && std::fs::rename(&cache, &backup).is_ok();
    let result = pet_discovery::discover(&dcfg).await;
    if moved {
        if cache.exists() {
            let _ = std::fs::remove_file(&backup);
        } else {
            let _ = std::fs::rename(&backup, &cache);
            return pet_discovery::discover(&dcfg).await;
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

fn placeholder_agent(agent_id: &str) -> Agent {
    Agent {
        id: agent_id.to_string(),
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
