//! Settings in the UI (PLAN step 5b) and the relay setting (step 6 phase B), hub side.
//!
//! **Local only, by construction.** These functions are reached from exactly one place:
//! `Hub::dispatch`, which only ever gets `ClientMsg`s from local WebSocket clients (the bearer
//! token on 127.0.0.1). Peer frames arrive as `PeerEvent`s (`peers.rs`), intents run through
//! `intents.rs::apply_intent` (approve / deny / prompt / start / cancel only) and the mirror only
//! stores other machines' items; none of them can name a setting. `ServerMsg::Settings` and
//! `ServerMsg::OpenPanel` are sent with `send_local`, so they never reach the mirror tap either.
//! The only other writer is [`Hub::adopt_relay`]: a `join_peer` this machine's user started.
//!
//! Order of an update: validate everything (`crate::settings::apply_patch`), then the runtime
//! changes that can fail (peer listener, relay), then config.toml (atomic, comments kept), then
//! the rest of the runtime changes, the audit row and the `settings` broadcast. A failure before
//! config.toml is written undoes what was applied, so nothing changes.

use std::net::SocketAddr;
use std::path::Path;

use pawl_discovery::hooks::{self as hooks, HookChange, HookCheck, HookTarget, InstallEnv};
use pawl_peer::RelayConfig;
use pawl_protocol::{HookAction, HookState, HookStatus, HookTargetName, HubSettings, HubSettingsPatch, PanelView};

use super::*;
use crate::settings::{self as cfgset, InvalidSetting};
use crate::store::{SettingsAuditEntry, SettingsAuditKind};

/// copilot-vscode status is reported for at most this many repos of the repo index.
const MAX_HOOK_REPOS: usize = 20;
/// `restart_hub` waits this long after its ack before the hub stops.
const RESTART_ACK_GRACE: Duration = Duration::from_millis(300);

/// What `pawl serve` started with: the bound API port and the keys flags / env vars decide.
#[derive(Debug, Clone, Default)]
pub struct Launch {
    /// `pawl serve` runs this hub (it can restart itself).
    pub served: bool,
    pub port: u16,
    /// key -> flag or env var ("hub.port" -> "--port", "peer.listen" -> "--peer-listen").
    pub overrides: BTreeMap<String, String>,
}

/// Counts a connected local client while alive (see [`Hub::client_connected`]).
pub struct ClientGuard {
    hub: Hub,
    desktop: bool,
}

impl Drop for ClientGuard {
    fn drop(&mut self) {
        if self.desktop {
            self.hub.inner.desktop_clients.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// The relay the link layer should use for `cfg` (None = off).
pub(super) fn relay_config(cfg: &HubConfig) -> Option<RelayConfig> {
    let url = cfg.relay.url.as_deref().map(str::trim).filter(|u| !u.is_empty())?;
    let access_key = cfg.relay.access_key.as_deref().map(str::trim).filter(|k| !k.is_empty()).map(str::to_string);
    Some(RelayConfig { url: url.to_string(), access_key })
}

fn target_name(t: &HookTarget) -> HookTargetName {
    match t {
        HookTarget::Claude => HookTargetName::Claude,
        HookTarget::Codex => HookTargetName::Codex,
        HookTarget::CopilotCli => HookTargetName::CopilotCli,
        HookTarget::CopilotVscode { .. } => HookTargetName::CopilotVscode,
    }
}

fn target_label(t: HookTargetName) -> &'static str {
    match t {
        HookTargetName::Claude => "claude",
        HookTargetName::Codex => "codex",
        HookTargetName::CopilotCli => "copilot-cli",
        HookTargetName::CopilotVscode => "copilot-vscode",
    }
}

/// Hook status / change against `home` (tests) or the agents' real config.
fn hook_check(t: &HookTarget, exe: &Path, home: Option<&Path>) -> Result<HookCheck> {
    match home {
        Some(h) => Ok(hooks::status_with_home(t, exe, h, &InstallEnv::default())),
        None => hooks::status(t, exe),
    }
}

fn hook_change(t: &HookTarget, exe: &Path, change: HookChange, apply: bool, home: Option<&Path>) -> Result<hooks::HookOutcome> {
    match home {
        Some(h) => hooks::change_with_home(t, exe, change, apply, h, &InstallEnv::default()),
        None => hooks::change(t, exe, change, apply),
    }
}

fn hook_status_of(t: &HookTarget, check: HookCheck, agent_installed: bool) -> HookStatus {
    HookStatus {
        target: target_name(t),
        repo: match t {
            HookTarget::CopilotVscode { repo } => Some(repo.to_string_lossy().into_owned()),
            _ => None,
        },
        agent_installed,
        state: check.state,
        config_path: check.config_path.to_string_lossy().into_owned(),
        detail: check.detail,
    }
}

impl Hub {
    /// `pawl serve`: the port the local API is bound to and the keys its flags / env vars set.
    pub fn set_launch(&self, port: u16, overrides: BTreeMap<String, String>) {
        *self.inner.launch.lock().unwrap_or_else(|p| p.into_inner()) = Launch { served: true, port, overrides };
    }

    fn launch(&self) -> Launch {
        self.inner.launch.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// A local client connected (server.rs): counted until the guard drops.
    pub fn client_connected(&self, kind: ClientKind) -> ClientGuard {
        let desktop = kind == ClientKind::Desktop;
        if desktop {
            self.inner.desktop_clients.fetch_add(1, Ordering::Relaxed);
        }
        ClientGuard { hub: self.clone(), desktop }
    }

    /// Tests: hook status / install work on this home directory (and ignore CLAUDE_CONFIG_DIR etc.).
    #[cfg(test)]
    pub(crate) fn set_hooks_home(&self, home: PathBuf) {
        *self.inner.hooks_home.lock().unwrap_or_else(|p| p.into_inner()) = Some(home);
    }

    fn hooks_home(&self) -> Option<PathBuf> {
        self.inner.hooks_home.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn overrides(&self) -> cfgset::Overrides {
        cfgset::Overrides::with_env(self.launch().overrides)
    }

    /// The effective settings (never a secret value), with the A2A server's live state.
    pub fn settings(&self) -> HubSettings {
        let mut s = cfgset::to_settings(&self.cfg(), &self.overrides());
        let a2a = self.a2a_status();
        s.a2a.running = a2a.running;
        s.a2a.url = a2a.url;
        s.a2a.error = a2a.error;
        for agent in s.a2a.agents.iter_mut() {
            let (card, error) = self.a2a_card_summary(&agent.id);
            agent.card = card;
            agent.error = error;
        }
        s
    }

    /// Settings changed since start that only a restart applies.
    pub fn restart_required(&self) -> Vec<String> {
        let running = self.launch().port;
        if self.cfg().port != running {
            vec![cfgset::HUB_PORT.to_string()]
        } else {
            Vec::new()
        }
    }

    /// `get_settings`.
    pub fn get_settings(&self) -> Value {
        json!({
            "settings": self.settings(),
            "restartRequired": self.restart_required(),
            "configPath": self.inner.paths.config.to_string_lossy(),
        })
    }

    /// Agent ids custom agents may not take: the built-in agents (and their registry ids) and every
    /// registry agent discovery found.
    fn reserved_agent_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = pawl_discovery::BUILTIN_AGENTS
            .iter()
            .flat_map(|b| [b.id.to_string(), b.registry_id.to_string()])
            .collect();
        ids.extend(self.st().agents.iter().filter(|a| a.source != AgentSource::Custom).map(|a| a.id.clone()));
        ids.sort();
        ids.dedup();
        ids
    }

    /// `update_settings{patch}`: see the module doc for the order. Ack `{settings, restartRequired,
    /// applied}` (applied = changed keys the running hub uses now).
    pub async fn update_settings(&self, patch: &HubSettingsPatch, actor: &Actor) -> Result<Value> {
        let _one = self.inner.settings_write.lock().await;
        let cur = self.config();
        let (next, changed) = cfgset::apply_patch(&cur, patch, &self.reserved_agent_ids())?;
        if changed.is_empty() {
            return Ok(self.settings_reply(Vec::new()));
        }
        if changed.contains(&cfgset::BOT_PET_NAME) {
            let name = next.brain.pet_name.clone().unwrap_or_default();
            if self.pet_name_taken(&name) {
                return Err(InvalidSetting { field: cfgset::BOT_PET_NAME.into(), reason: format!("{name:?} is another pet's name") }.into());
            }
        }
        let applied = self.commit_settings(&cur, next, &changed, actor, SettingsAuditKind::Settings).await?;
        Ok(self.settings_reply(applied))
    }

    /// True if a pet of this or a paired machine has `name` (case-insensitive).
    fn pet_name_taken(&self, name: &str) -> bool {
        let key = name.trim().to_lowercase();
        let local = self.st().pets.values().any(|p| p.name.trim().to_lowercase() == key);
        local || self.remote_pet_names().iter().any(|n| n.trim().to_lowercase() == key)
    }

    /// The default pet's name, picked once (first start, or an empty setting): a name of the pet-name pool
    /// that is not "Pawl" and no pet's name. Saved to config.toml and announced like a setting.
    pub(super) async fn ensure_default_pet_name(&self) {
        if self.default_pet_name().is_some() {
            return;
        }
        let _one = self.inner.settings_write.lock().await;
        let mut taken: Vec<String> = self.st().pets.values().map(|p| p.name.clone()).collect();
        taken.extend(self.remote_pet_names());
        taken.push("Pawl".into());
        let name = pawl_brain::growth::default_name(&self.inner.machine_id, "default-pet", &taken);
        let mut next = self.config();
        next.brain.pet_name = Some(name.clone());
        if let Err(e) = self.write_config(&next, &[cfgset::BOT_PET_NAME]).await {
            tracing::warn!("could not save the default pet's name to config.toml: {e:#}");
        }
        *self.inner.config.write().unwrap_or_else(|p| p.into_inner()) = next;
        // Not a user's change: logged and announced, not an audit row.
        tracing::info!(name = %name, "default pet named");
        self.announce_settings();
    }

    /// The old default rules were replaced by the new ones at start (see `Hub::build`).
    pub(super) async fn audit_rules_migration(&self) {
        let actor = Actor::internal("hub:default-rules");
        let detail = Some("stored rules were the old defaults; reads and searches now ask".to_string());
        self.audit_settings(&actor, SettingsAuditKind::Settings, vec!["rules".to_string()], detail).await;
    }

    fn settings_reply(&self, applied: Vec<String>) -> Value {
        json!({ "settings": self.settings(), "restartRequired": self.restart_required(), "applied": applied })
    }

    /// Apply a validated change (caller holds `settings_write`). Returns the keys applied now.
    pub(super) async fn commit_settings(
        &self,
        cur: &HubConfig,
        next: HubConfig,
        changed: &[&'static str],
        actor: &Actor,
        kind: SettingsAuditKind,
    ) -> Result<Vec<String>> {
        let has = |k: &str| changed.contains(&k);
        // 1. What can fail at runtime, before anything is written. A new API port must be free now,
        //    or the restart that applies it would find it taken.
        if has(cfgset::HUB_PORT) && next.port != self.launch().port {
            if let Err(e) = std::net::TcpListener::bind((pawl_protocol::HUB_HOST, next.port)) {
                return Err(InvalidSetting { field: cfgset::HUB_PORT.into(), reason: format!("is in use on this machine ({e})") }.into());
            }
        }
        // The A2A server's port must be free when it turns on or moves (its own port is not "in use").
        let a2a_moves = next.a2a.server && (!cur.a2a.server || next.a2a.port != cur.a2a.port || !self.a2a_status().running);
        if (has(cfgset::A2A_SERVER) || has(cfgset::A2A_PORT)) && a2a_moves {
            if let Err(e) = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, next.a2a.port)) {
                return Err(InvalidSetting { field: cfgset::A2A_PORT.into(), reason: format!("is in use on this machine ({e})") }.into());
            }
        }
        let listener_before = self.inner.peer.listening();
        if has(cfgset::PEER_LISTEN) {
            let to = next.peer.listen_addr().map_err(|_| InvalidSetting { field: cfgset::PEER_LISTEN.into(), reason: "is not an address".into() })?;
            self.switch_listener(to).await?;
        }
        let relay_changed = has(cfgset::RELAY_URL) || has(cfgset::RELAY_KEY);
        if relay_changed {
            if let Err(e) = self.apply_relay(&next) {
                if has(cfgset::PEER_LISTEN) {
                    self.restore_listener(listener_before).await;
                }
                return Err(e);
            }
        }
        // 2. config.toml.
        if let Err(e) = self.write_config(&next, changed).await {
            if has(cfgset::PEER_LISTEN) {
                self.restore_listener(listener_before).await;
            }
            if relay_changed {
                if let Err(e) = self.apply_relay(cur) {
                    tracing::warn!("could not restore the previous relay setting: {e:#}");
                }
            }
            return Err(e.context("could not save config.toml; nothing was changed"));
        }
        // 3. The running hub.
        *self.inner.config.write().unwrap_or_else(|p| p.into_inner()) = next.clone();
        if has(cfgset::PEER_LISTEN) {
            // The setting now decides the listener, not the --peer-listen flag the hub started with.
            self.inner.launch.lock().unwrap_or_else(|p| p.into_inner()).overrides.remove(cfgset::PEER_LISTEN);
        }
        self.apply_runtime(&next, changed).await;
        // Key names only: never values (two of them are secrets).
        tracing::info!(keys = %changed.join(","), by = %actor.label, "settings changed");
        self.audit_settings(actor, kind, changed.iter().map(|k| k.to_string()).collect(), None).await;
        self.announce_settings();
        Ok(changed.iter().filter(|k| !cfgset::RESTART_KEYS.contains(k)).map(|k| k.to_string()).collect())
    }

    /// Settings the running hub reads at use time need nothing here; the others are rebuilt.
    async fn apply_runtime(&self, next: &HubConfig, changed: &[&'static str]) {
        let has = |k: &str| changed.contains(&k);
        if cfgset::A2A_KEYS.iter().any(|k| has(k)) {
            // Start, stop, move, or replace the callers (a removed or rotated token stops at once).
            self.sync_a2a_server().await;
        }
        if cfgset::A2A_AGENT_KEYS.iter().any(|k| has(k)) {
            // Fetch the outside agents' cards again and rebuild the agent list.
            let hub = self.clone();
            tokio::spawn(async move { hub.refresh_a2a_cards().await });
        }
        if cfgset::BRAIN_KEYS.iter().any(|k| has(k)) {
            self.rebuild_brain();
            if has(cfgset::BOT_ACP_AGENT) || has(cfgset::BOT_ACP_MODEL) || has(cfgset::BOT_PROVIDER) {
                // A different agent or model: the next turn starts a fresh session.
                self.reset_brain_session("settings changed").await;
            }
        }
        if has(cfgset::WD_NUDGE) || has(cfgset::WD_REPEAT) {
            // New timings, same memory of what was already nudged (no repeat nudges).
            self.inner.watchdog.lock().unwrap_or_else(|p| p.into_inner()).set_config(next.watchdog.to_watchdog_config());
        }
        if has(cfgset::WD_TICK) || has(cfgset::HUB_DAILY_REPORT) {
            self.inner.settings_changed.notify_waiters();
        }
        if has(cfgset::HUB_RETENTION) && next.event_retention_days > 0 {
            let before = pawl_protocol::now_ms() - next.event_retention_days as i64 * DAY_MS;
            match self.inner.store.run(move |s| s.prune_events_before(before)).await {
                Ok(n) if n > 0 => tracing::info!("pruned {n} events older than {} days", next.event_retention_days),
                Ok(_) => {}
                Err(e) => tracing::warn!("prune events: {e:#}"),
            }
        }
        if has(cfgset::REPOS_PATHS) {
            self.rebuild_repo_index().await;
        }
        if has(cfgset::CUSTOM_AGENTS) || has(cfgset::HUB_OFFLINE) {
            let hub = self.clone();
            tokio::spawn(async move {
                if let Err(e) = hub.refresh_agents(false).await {
                    tracing::warn!("agent discovery after a settings change failed: {e:#}");
                }
            });
        }
    }

    async fn write_config(&self, next: &HubConfig, changed: &[&'static str]) -> Result<()> {
        let path = self.inner.paths.config.clone();
        let (next, changed) = (next.clone(), changed.to_vec());
        tokio::task::spawn_blocking(move || {
            let old = match std::fs::read_to_string(&path) {
                Ok(t) => Some(t),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(anyhow::Error::new(e).context(format!("read {}", path.display()))),
            };
            let text = cfgset::edit_config(old.as_deref(), &next, &changed)?;
            cfgset::write_atomic(&path, &text)
        })
        .await
        .context("write config.toml")?
    }

    pub(super) fn announce_settings(&self) {
        self.inner.bus.send_local(ServerMsg::Settings { settings: self.settings(), restart_required: self.restart_required() });
    }

    async fn audit_settings(&self, actor: &Actor, kind: SettingsAuditKind, keys: Vec<String>, detail: Option<String>) {
        let entry = SettingsAuditEntry {
            id: 0,
            ts: pawl_protocol::now_ms(),
            machine_id: self.inner.machine_id.clone(),
            kind,
            actor: actor.label.clone(),
            client: actor.kind.map(|k| client_label(k).to_string()),
            keys,
            detail,
        };
        if let Err(e) = self.inner.store.run(move |s| s.insert_settings_audit(&entry)).await {
            tracing::error!("settings audit write failed: {e:#}");
        }
    }

    /// Start, stop or move the peer listener. A bind failure puts the previous listener back and
    /// is `invalid:peer.listen` (nothing changed).
    async fn switch_listener(&self, to: Option<SocketAddr>) -> Result<()> {
        let before = self.inner.peer.listening();
        if before == to {
            return Ok(());
        }
        self.inner.peer.stop_listening().await;
        if let Some(addr) = to {
            if let Err(e) = self.inner.peer.listen(addr).await {
                self.restore_listener(before).await;
                return Err(InvalidSetting { field: cfgset::PEER_LISTEN.into(), reason: format!("could not listen on {addr}: {e:#}") }.into());
            }
            if addr.ip().is_unspecified() {
                tracing::warn!("the peer listener on {addr} accepts connections on every interface; invites need an address to advertise");
            }
            tracing::info!("peer listener on {addr}");
        } else {
            tracing::info!("peer listener stopped");
        }
        self.inner.bus.send_local(ServerMsg::PeerStatus { peer: self.peer_status() });
        Ok(())
    }

    async fn restore_listener(&self, before: Option<SocketAddr>) {
        if self.inner.peer.listening() == before {
            return;
        }
        self.inner.peer.stop_listening().await;
        if let Some(addr) = before {
            if let Err(e) = self.inner.peer.listen(addr).await {
                tracing::warn!("could not restore the peer listener on {addr}: {e:#}");
            }
        }
        self.inner.bus.send_local(ServerMsg::PeerStatus { peer: self.peer_status() });
    }

    /// Hand the relay setting to the link layer; its refusals are field errors.
    fn apply_relay(&self, cfg: &HubConfig) -> Result<()> {
        self.inner.peer.set_relay(relay_config(cfg)).map_err(|e| {
            let (field, reason) = match e {
                pawl_peer::RelayConfigError::BadUrl => (cfgset::RELAY_URL, "must be wss://host[:port][/path] (ws:// only to this machine)"),
                pawl_peer::RelayConfigError::BadKey => (cfgset::RELAY_KEY, "must be 1-256 characters of A-Z, a-z, 0-9, - and _"),
            };
            anyhow::Error::new(InvalidSetting { field: field.into(), reason: reason.into() })
        })
    }

    /// A `join_peer` turned on the invite's relay (this hub had none): keep it as the relay
    /// setting. Never replaces a relay the user configured (the link layer only adopts when none is).
    pub(super) async fn adopt_relay(&self, relay: RelayConfig, actor: &Actor) -> Result<()> {
        let _one = self.inner.settings_write.lock().await;
        let cur = self.config();
        if cur.relay.url.as_deref().is_some_and(|u| !u.trim().is_empty()) {
            return Ok(());
        }
        // The invite came from another machine: the same checks as a value typed in Settings.
        let url = cfgset::check_relay_url(&relay.url)?;
        let access_key = match relay.access_key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
            Some(k) => Some(cfgset::check_relay_key(k)?),
            None => None,
        };
        let mut next = cur.clone();
        next.relay.url = Some(url);
        next.relay.access_key = access_key;
        let changed = cfgset::changed_keys(&cur, &next);
        if changed.is_empty() {
            return Ok(());
        }
        // The link layer already uses it; write it down without calling set_relay again.
        self.write_config(&next, &changed).await?;
        *self.inner.config.write().unwrap_or_else(|p| p.into_inner()) = next;
        tracing::info!(keys = %changed.join(","), by = %actor.label, "relay setting taken from a pairing invite");
        self.audit_settings(actor, SettingsAuditKind::Pairing, changed.iter().map(|k| k.to_string()).collect(), Some("relay from invite".into()))
            .await;
        self.announce_settings();
        Ok(())
    }

    // ------------------------------------------------------------ hooks

    fn hook_exe() -> Result<PathBuf> {
        std::env::current_exe().context("locate the pawl executable")
    }

    /// `hook_status`: claude, codex, copilot-cli, and copilot-vscode for the repo index's folders.
    pub async fn hook_status(&self) -> Result<Vec<HookStatus>> {
        let exe = Self::hook_exe()?;
        let home = self.hooks_home();
        let repos: Vec<PathBuf> = self.repos().into_iter().take(MAX_HOOK_REPOS).map(|r| PathBuf::from(r.path)).collect();
        tokio::task::spawn_blocking(move || {
            let env = pawl_discovery::detect::HostEnv::from_system();
            let mut targets = vec![HookTarget::Claude, HookTarget::Codex, HookTarget::CopilotCli];
            targets.extend(repos.into_iter().map(|repo| HookTarget::CopilotVscode { repo }));
            targets
                .iter()
                .map(|t| {
                    let check = hook_check(t, &exe, home.as_deref()).unwrap_or_else(|e| HookCheck {
                        state: HookState::Unsupported,
                        config_path: PathBuf::new(),
                        detail: Some(format!("{e:#}")),
                    });
                    Ok(hook_status_of(t, check, hooks::agent_found(t, &env)))
                })
                .collect::<Result<Vec<_>>>()
        })
        .await
        .context("hook status")?
    }

    /// `install_hooks{target, repo, action}`: the same code as `pawl install-hooks`. preview =
    /// the exact change, nothing written; apply / remove write it (backups first) and are audited.
    pub async fn install_hooks(&self, target: HookTargetName, repo: Option<String>, action: HookAction, actor: &Actor) -> Result<Value> {
        let t = match target {
            HookTargetName::Claude => HookTarget::Claude,
            HookTargetName::Codex => HookTarget::Codex,
            HookTargetName::CopilotCli => HookTarget::CopilotCli,
            HookTargetName::CopilotVscode => HookTarget::CopilotVscode { repo: self.hook_repo(repo.as_deref())? },
        };
        let (change, apply) = match action {
            HookAction::Preview => (HookChange::Install, false),
            HookAction::Apply => (HookChange::Install, true),
            HookAction::Remove => (HookChange::Remove, true),
        };
        let exe = Self::hook_exe()?;
        let home = self.hooks_home();
        let t2 = t.clone();
        let (outcome, status) = tokio::task::spawn_blocking(move || -> Result<_> {
            let outcome = hook_change(&t2, &exe, change, apply, home.as_deref())?;
            let check = hook_check(&t2, &exe, home.as_deref())?;
            let env = pawl_discovery::detect::HostEnv::from_system();
            Ok((outcome, hook_status_of(&t2, check, hooks::agent_found(&t2, &env))))
        })
        .await
        .context("install hooks")??;
        let applied = apply && outcome.changed;
        if applied {
            let verb = if change == HookChange::Remove { "remove" } else { "apply" };
            let detail = match &t {
                HookTarget::CopilotVscode { repo } => format!("{verb} {}", repo.display()),
                _ => verb.to_string(),
            };
            tracing::info!(target = target_label(target), action = verb, by = %actor.label, "agent hooks changed");
            self.audit_settings(actor, SettingsAuditKind::Hooks, vec![format!("hooks.{}", target_label(target))], Some(detail)).await;
        }
        Ok(json!({ "plan": outcome.text, "applied": applied, "status": status }))
    }

    /// The repository a copilot-vscode hook goes into: an absolute, local (no network share or
    /// device path), existing folder that is in this machine's repo index or has a `.git`.
    fn hook_repo(&self, repo: Option<&str>) -> Result<PathBuf> {
        let bad = |reason: &str| -> anyhow::Error { InvalidSetting { field: "repo".into(), reason: reason.into() }.into() };
        let repo = repo.map(str::trim).filter(|r| !r.is_empty()).ok_or_else(|| bad("copilot-vscode needs the repository folder"))?;
        let p = Path::new(repo);
        if !p.is_absolute() {
            return Err(bad("must be an absolute path"));
        }
        if crate::repos::plain_path(p).is_none() {
            return Err(bad("network shares and device paths are not allowed"));
        }
        let canon = crate::repos::canonical_dir(p).ok_or_else(|| bad("is not an existing local folder"))?;
        let key = crate::repos::path_key(&canon);
        let indexed = self.repos().iter().any(|r| crate::repos::path_key(Path::new(&r.path)) == key);
        if !indexed && !canon.join(".git").exists() {
            return Err(bad("is not a repository this machine knows (not in the repo index, no .git folder)"));
        }
        Ok(canon)
    }

    // ------------------------------------------------------------ restart

    /// `restart_hub`: only a hub `pawl serve` runs can replace itself. The ack goes out first;
    /// a moment later `pawl serve` sees [`Hub::restart_requested`], says bye to peers, stops,
    /// starts the replacement and exits (see `cli::run_serve`).
    pub async fn restart_hub(&self, actor: &Actor) -> Result<Value> {
        if !self.launch().served {
            bail!("this hub is not running as `pawl serve`; restart it the way it was started");
        }
        let pending = self.restart_required();
        tracing::info!(by = %actor.label, pending = %pending.join(","), "hub restart requested");
        self.audit_settings(actor, SettingsAuditKind::Restart, pending, None).await;
        let restart = self.inner.restart.clone();
        tokio::spawn(async move {
            // Let the ack reach the client before the server stops.
            tokio::time::sleep(RESTART_ACK_GRACE).await;
            let _ = restart.send(true);
        });
        Ok(json!({}))
    }

    /// A replacement hub could not take the new port and stayed on the old one: tell every client
    /// that connects (the setting still says the new port, so `restartRequired` shows it too).
    pub fn port_fallback_notice(&self, wanted: u16, using: u16) {
        let notification = notify::make(
            &self.inner.machine_id,
            NotificationLevel::Warning,
            "Hub port unchanged",
            format!("Port {wanted} was in use after the restart, so the hub stays on port {using}. Free the port or pick another one in Settings, then restart."),
            None,
            None,
        );
        self.inner.startup_notices.lock().unwrap_or_else(|p| p.into_inner()).push(notification.clone());
        self.inner.bus.send_local(ServerMsg::Notification { notification });
    }

    /// Becomes true once a client asked for a restart.
    pub fn restart_requested(&self) -> watch::Receiver<bool> {
        self.inner.restart.subscribe()
    }

    // ------------------------------------------------------------ panel

    /// `open_panel{view}`: ask the desktop app to show a view. Ack `{delivered}` = a desktop client
    /// is connected right now.
    pub fn open_panel(&self, view: PanelView) -> Value {
        let delivered = self.inner.desktop_clients.load(Ordering::Relaxed) > 0;
        self.inner.bus.send_local(ServerMsg::OpenPanel { view });
        json!({ "delivered": delivered })
    }
}

#[cfg(test)]
mod tests;
