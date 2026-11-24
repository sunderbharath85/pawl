//! Hub side of hub-to-hub pairing and presence (`krypto-peer`, PLAN step 2).
//!
//! `krypto-peer` owns the sockets, the crypto, invites and the roster writes (through
//! `impl Roster for Store`). This module keeps the hub's view of it: a cache of the roster, the
//! presence snapshot peers receive, the pairing `ClientMsg`s, and the translation of `PeerEvent`s
//! into `ServerMsg`s. Peer frames never reach `ClientMsg` handling: a peer can only pair, say
//! hello, send presence and say bye, and the hub only ever sees the resulting `PeerEvent`s.

use std::collections::{BTreeMap, HashMap};
use std::ops::RangeInclusive;

use krypto_peer::{PeerEvent, PeerNode, RosterEntry};
use krypto_protocol::{Device, DeviceKind, LinkPath, PairRejectReason, PeerStatus, Presence};
use tokio::sync::mpsc::UnboundedReceiver;

use super::*;
use crate::store::{PairingAuditEntry, PairingAuditKind};

pub const DEFAULT_INVITE_TTL_SECS: u32 = 600;
pub const INVITE_TTL_SECS: RangeInclusive<u32> = 60..=3600;

/// The hub's view of its paired devices.
#[derive(Default)]
pub(super) struct Peers {
    /// Paired, non-revoked devices by id, as last read from the roster.
    roster: BTreeMap<String, RosterEntry>,
    /// The last `Machine` sent to clients per paired hub. `ServerMsg::Machine` goes out only when
    /// a peer is added, renamed or goes online/offline, never for a heartbeat alone.
    sent: HashMap<String, Machine>,
    /// Latest presence per peer (step 3 shows its agents and pets).
    presence: HashMap<String, Presence>,
    /// The roster as last sent in `ServerMsg::Devices` (lastSeen stripped).
    devices_sent: Vec<Device>,
    /// Entries that left the roster before their `Revoked` event arrived (the link layer marks a
    /// peer's `bye{revoked}` in the store and reports `LinkDown` first); kept so that event can
    /// still audit them.
    departed: HashMap<String, RosterEntry>,
    /// The path of each live link (`PeerEvent::LinkPath`); gone on `LinkDown`. Shown as `Device.path`.
    paths: HashMap<String, LinkPath>,
}

/// A link-layer error as its bare snake_case wire code (`krypto-peer` errors display as the code).
fn wire(e: impl std::fmt::Display) -> anyhow::Error {
    anyhow!("{e}")
}

/// A roster entry with live state: online while a link is up, lastSeen = the latest authenticated
/// frame we know of, userActiveAt from its latest presence.
fn live_machine(e: &RosterEntry, online: bool, frame_ts: Option<UnixMs>, presence: Option<&Presence>) -> Machine {
    let mut m = e.machine.clone();
    m.id = e.device.id.clone();
    m.online = online;
    m.last_seen = m.last_seen.max(e.device.last_seen.unwrap_or(0)).max(frame_ts.unwrap_or(0));
    m.user_active_at = presence.and_then(presence_activity);
    m
}

/// A peer's presence in this hub's clock: its timestamps are shifted by `now - presence.ts` (the
/// peer's clock offset, as of arrival) and never land in the future.
pub(crate) fn presence_to_local_clock(mut p: Presence, now: UnixMs) -> Presence {
    let offset = now.saturating_sub(p.ts);
    let shift = |t: UnixMs| t.saturating_add(offset).clamp(0, now);
    p.user_active_at = p.user_active_at.map(shift);
    p.machine.user_active_at = p.machine.user_active_at.map(shift);
    p.machine.last_seen = shift(p.machine.last_seen);
    p.ts = now;
    p
}

/// A presence's userActiveAt (either copy; the later wins).
fn presence_activity(p: &Presence) -> Option<UnixMs> {
    p.user_active_at.max(p.machine.user_active_at)
}

/// What clients are told about with `ServerMsg::Machine`: lastSeen alone is no reason, and
/// userActiveAt only when it moved by ACTIVITY_STEP_MS.
fn same_view(a: &Machine, b: &Machine) -> bool {
    let activity_close = match (a.user_active_at, b.user_active_at) {
        (Some(x), Some(y)) => x.abs_diff(y) < ACTIVITY_STEP_MS as u64,
        (None, None) => true,
        _ => false,
    };
    a.name == b.name && a.os == b.os && a.hub_version == b.hub_version && a.online == b.online && activity_close
}

/// The roster as clients see it, by name, with each live link's path.
fn sorted_devices(roster: &BTreeMap<String, RosterEntry>, paths: &HashMap<String, LinkPath>) -> Vec<Device> {
    let mut v: Vec<Device> =
        roster.values().map(|e| Device { path: paths.get(&e.device.id).copied(), ..e.device.clone() }).collect();
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
    v
}

/// The roster as clients see it in `devices`, minus lastSeen (which changes on every heartbeat).
fn devices_key(roster: &BTreeMap<String, RosterEntry>, paths: &HashMap<String, LinkPath>) -> Vec<Device> {
    sorted_devices(roster, paths).into_iter().map(|d| Device { last_seen: None, ..d }).collect()
}


/// A presence snapshot without its timestamps, to tell whether anything peers see changed.
fn presence_key(p: &Presence) -> Value {
    let mut p = p.clone();
    p.ts = 0;
    p.machine.last_seen = 0;
    // Activity has its own push rule (see `Hub::user_activity`).
    p.user_active_at = None;
    p.machine.user_active_at = None;
    serde_json::to_value(&p).unwrap_or(Value::Null)
}

fn reject_label(r: PairRejectReason) -> &'static str {
    match r {
        PairRejectReason::InvalidInvite => "invalid_invite",
        PairRejectReason::BadRequest => "bad_request",
    }
}

impl Hub {
    /// The link layer for this hub: `device.key` (created on first start) and the store as roster.
    pub(super) fn new_peer_node(
        paths: &Paths,
        me: Machine,
        store: &Store,
    ) -> Result<(PeerNode, UnboundedReceiver<PeerEvent>)> {
        let keys = krypto_peer::DeviceKeys::load_or_create(&paths.device_key)
            .with_context(|| format!("load device key {}", paths.device_key.display()))?;
        let (tx, rx) = mpsc::unbounded_channel();
        let roster: Arc<dyn krypto_peer::Roster> = Arc::new(store.clone());
        Ok((PeerNode::new(keys, me, roster, tx), rx))
    }

    fn peers(&self) -> MutexGuard<'_, Peers> {
        self.inner.peers.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Load the roster cache at startup; every paired hub starts offline.
    pub(super) async fn load_peers(&self) -> Result<()> {
        let roster = self.inner.store.run(|s| s.roster()).await?;
        let mut peers = self.peers();
        peers.roster = roster.into_iter().map(|e| (e.device.id.clone(), e)).collect();
        let sent: HashMap<String, Machine> = peers
            .roster
            .values()
            .filter(|e| e.device.kind == DeviceKind::Hub)
            .map(|e| (e.device.id.clone(), live_machine(e, false, None, None)))
            .collect();
        peers.sent = sent;
        peers.devices_sent = devices_key(&peers.roster, &peers.paths);
        Ok(())
    }

    /// Start the link layer: the listener when configured (`--peer-listen` / `[peer] listen`),
    /// the relay when `[relay] url` is set, then dial loops to every paired hub with a known address.
    pub async fn start_peer_links(&self) -> Result<()> {
        let (listen, relay) = {
            let cfg = self.cfg();
            (cfg.peer.listen_addr().context("[peer] listen")?, settings::relay_config(&cfg))
        };
        if relay.is_some() {
            // A bad relay setting must not keep the hub from starting: it says so and runs without.
            if let Err(e) = self.inner.peer.set_relay(relay) {
                tracing::warn!("ignoring the [relay] setting in config.toml: {e}");
                let notification = notify::make(
                    &self.inner.machine_id,
                    NotificationLevel::Warning,
                    "Relay setting ignored",
                    format!("The relay setting in config.toml was refused ({e}); fix it in Settings."),
                    None,
                    None,
                );
                self.inner.startup_notices.lock().unwrap_or_else(|p| p.into_inner()).push(notification.clone());
                self.inner.bus.send_local(ServerMsg::Notification { notification });
            }
        }
        if let Some(addr) = listen {
            if addr.ip().is_unspecified() {
                tracing::warn!(
                    "the peer listener on {addr} accepts connections on every interface; \
                     invites need --addr <LAN-IP:port> so the other hub knows where to dial"
                );
            }
            // A replacement hub (restart_hub) may find the port still held by the old one briefly.
            let deadline = tokio::time::Instant::now() + crate::restart::bind_retry();
            let bound = loop {
                match self.inner.peer.listen(addr).await {
                    Ok(b) => break b,
                    Err(_) if tokio::time::Instant::now() < deadline => tokio::time::sleep(Duration::from_millis(250)).await,
                    Err(e) => return Err(e.context(format!("start the peer listener on {addr}"))),
                }
            };
            tracing::info!("peer listener on {bound}");
            self.inner.bus.send(ServerMsg::PeerStatus { peer: self.peer_status() });
        }
        self.inner.peer.connect_known();
        Ok(())
    }

    /// This hub's peer-link status (listener address, public key, relay connection).
    pub fn peer_status(&self) -> PeerStatus {
        PeerStatus {
            listening: self.inner.peer.listening().map(|a| a.to_string()),
            public_key: self.inner.peer.public_b64(),
            relay: self.inner.peer.relay_status(),
        }
    }

    /// The pairing roster (never this hub), by name, with each live link's path.
    pub fn devices(&self) -> Vec<Device> {
        let peers = self.peers();
        sorted_devices(&peers.roster, &peers.paths)
    }

    /// Paired hubs with live state from the link layer, by name.
    pub(super) fn peer_machines(&self) -> Vec<Machine> {
        let online: HashMap<String, UnixMs> = self.inner.peer.online().into_iter().collect();
        let peers = self.peers();
        let mut v: Vec<Machine> = peers
            .roster
            .values()
            .filter(|e| e.device.kind == DeviceKind::Hub)
            .map(|e| {
                let ts = online.get(&e.device.id).copied();
                live_machine(e, ts.is_some(), ts, peers.presence.get(&e.device.id))
            })
            .collect();
        v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.id.cmp(&b.id)));
        v
    }

    /// A paired hub's repo index from its latest presence (None until one arrived).
    pub(super) fn presence_repos(&self, machine_id: &str) -> Option<Vec<krypto_protocol::RepoRef>> {
        self.peers().presence.get(machine_id).map(|p| p.repos.clone())
    }

    /// A paired hub's agents from its latest presence (None until one arrived).
    pub(super) fn presence_agents(&self, machine_id: &str) -> Option<Vec<Agent>> {
        self.peers().presence.get(machine_id).map(|p| p.agents.clone())
    }

    /// Pet names on other machines, from the mirror and their latest presence (pet names are
    /// unique across all known machines).
    pub(super) fn remote_pet_names(&self) -> Vec<String> {
        let mid = &self.inner.machine_id;
        let mut names: Vec<String> = self
            .peers()
            .presence
            .values()
            .flat_map(|p| p.pets.iter())
            .filter(|p| &p.machine_id != mid)
            .map(|p| p.name.clone())
            .collect();
        names.extend(self.mirror_pets().into_iter().filter(|p| &p.machine_id != mid).map(|p| p.name));
        names.sort();
        names.dedup();
        names
    }

    // ------------------------------------------------------------ presence

    /// What paired hubs see of this one: the machine, its agents and its own pets.
    /// `repos` stays empty until PLAN step 5.
    fn presence_snapshot(&self) -> Presence {
        let machine = self.machine();
        let st = self.st();
        let mut pets: Vec<Pet> = st.pets.values().filter(|p| p.machine_id == st.machine_id).cloned().collect();
        pets.sort_by(|a, b| a.agent_id.cmp(&b.agent_id).then_with(|| a.id.cmp(&b.id)));
        Presence {
            user_active_at: machine.user_active_at,
            machine,
            agents: st.agents.clone(),
            repos: self.repos(),
            pets,
            ts: krypto_protocol::now_ms(),
        }
    }

    /// Hand the link layer a fresh snapshot when something peers see changed (rename, agents,
    /// pets); it pushes it at once and then on every heartbeat.
    pub(super) fn refresh_presence(&self) {
        self.hand_presence(false);
    }

    /// Push the current snapshot now even if nothing but activity changed.
    pub(super) fn push_presence(&self) {
        self.hand_presence(true);
    }

    fn hand_presence(&self, force: bool) {
        // Held across snapshot + hand-off so two callers cannot push an older snapshot last.
        let mut pushed = self.inner.presence_pushed.lock().unwrap_or_else(|p| p.into_inner());
        let p = self.presence_snapshot();
        let key = presence_key(&p);
        if !force && pushed.as_ref() == Some(&key) {
            return;
        }
        *pushed = Some(key);
        self.inner.peer.set_presence(p);
    }

    // ------------------------------------------------------------ client messages

    /// `create_invite`: a single-use invite URI for another hub to join. Needs the listener.
    pub fn create_invite(&self, ttl_secs: Option<u32>, addr: Option<String>) -> Result<Value> {
        let ttl = ttl_secs.unwrap_or(DEFAULT_INVITE_TTL_SECS);
        if !INVITE_TTL_SECS.contains(&ttl) {
            return Err(AckCode::Invalid.into());
        }
        let addr = addr.map(|a| a.trim().to_string()).filter(|a| !a.is_empty());
        let invite = self.inner.peer.create_invite(addr, Duration::from_secs(ttl.into())).map_err(wire)?;
        // Never log the URI: it carries the one-time code.
        let advertising = invite.addr.as_deref().unwrap_or("relay only");
        tracing::info!("created a pairing invite (valid {ttl} s, advertising {advertising})");
        Ok(json!({ "uri": invite.to_uri(), "expiresAt": invite.expires_at }))
    }

    /// `join_peer`: pair with the hub that created `uri`. The link layer stores it and reports
    /// `Paired`, which updates clients. When this hub had no relay and the join used the
    /// invite's, that relay becomes this hub's relay setting (config.toml, audited, announced).
    pub async fn join_peer(&self, uri: &str, actor: &Actor) -> Result<Value> {
        let joined = self.inner.peer.join_invite(uri.trim()).await.map_err(wire)?;
        let (device, mut machine) = (joined.device, joined.machine);
        machine.online = true; // the pairing socket is now the live link
        tracing::info!("joined {:?} ({})", machine.name, machine.id);
        if let Some(relay) = joined.adopted_relay {
            if let Err(e) = self.adopt_relay(relay, actor).await {
                // The node keeps using the relay until the next restart; say so.
                tracing::warn!("joined through the invite's relay, but could not save it as the relay setting: {e:#}");
            }
        }
        Ok(json!({ "device": device, "machine": machine }))
    }

    /// `revoke_device`: `bye{revoked}` on its live link, close it, mark it revoked, stop dialing.
    pub async fn revoke_device(&self, device_id: &str, actor: &Actor) -> Result<()> {
        let _ops = self.inner.peer_ops.lock().await;
        let cached = self.peers().roster.contains_key(device_id);
        let known = cached || {
            let id = device_id.to_string();
            self.inner.store.run(move |s| s.roster_entry(&id)).await?.is_some()
        };
        if !known {
            return Err(AckCode::NotFound.into());
        }
        // The link layer first: it still needs the roster entry to say bye.
        self.inner.peer.revoke(device_id).await;
        self.remove_device(device_id, false, &actor.label).await;
        Ok(())
    }

    // ------------------------------------------------------------ peer events

    pub(super) async fn peer_loop(self, mut rx: UnboundedReceiver<PeerEvent>) {
        let mut shutdown = self.shutdown_rx();
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Some(ev) => self.on_peer_event(ev).await,
                    None => break,
                },
                _ = shutdown.changed() => break,
            }
        }
    }

    pub(super) async fn on_peer_event(&self, ev: PeerEvent) {
        let _ops = self.inner.peer_ops.lock().await;
        match ev {
            PeerEvent::Paired { device, mut machine, we_invited } => {
                // The peer chose these names: the roster stores them sanitized, and so do we here.
                machine.name = crate::store::sanitize_machine_name(&machine.name);
                let device = Device { name: crate::store::sanitize_machine_name(&device.name), ..device };
                tracing::info!(
                    "paired with {:?} ({}, {})",
                    machine.name,
                    device.id,
                    if we_invited { "accepted our invite" } else { "joined their invite" }
                );
                let detail = if we_invited { "accepted our invite" } else { "joined their invite" };
                self.audit_pairing(PairingAuditKind::Paired, Some(&device), device.addr.clone(), "peer", Some(detail.into()))
                    .await;
                let notification = notify::make(
                    &self.inner.machine_id,
                    NotificationLevel::Success,
                    format!("Paired with {}", machine.name),
                    format!("This machine and {} can now see each other.", machine.name),
                    None,
                    None,
                );
                self.inner.bus.send_local(ServerMsg::Notification { notification });
                self.sync_peers(Some((&device.id, true))).await;
            }
            PeerEvent::PairRejected { remote_addr, reason } => {
                tracing::warn!("refused a pairing attempt from {remote_addr} ({})", reject_label(reason));
                self.audit_pairing(
                    PairingAuditKind::PairRejected,
                    None,
                    Some(remote_addr),
                    "peer",
                    Some(reject_label(reason).into()),
                )
                .await;
            }
            PeerEvent::LinkUp { device_id, .. } => {
                tracing::info!("link up: {device_id}");
                self.sync_peers(Some((&device_id, true))).await;
                // On every LinkUp: a new link can replace a live one and lose what was queued on it.
                if self.peers().roster.get(&device_id).is_some_and(|e| e.device.kind == DeviceKind::Hub) {
                    self.request_sync(&device_id).await;
                }
            }
            PeerEvent::LinkDown { device_id } => {
                tracing::info!("link down: {device_id}");
                self.peers().paths.remove(&device_id);
                self.link_lost(&device_id);
                self.sync_peers(Some((&device_id, false))).await;
            }
            PeerEvent::Body { device_id, body } => self.on_peer_body(&device_id, body).await,
            PeerEvent::Presence { device_id, presence } => {
                if !self.peers().roster.contains_key(&device_id) {
                    // Not (or no longer) paired: never keep its data.
                    return;
                }
                let mut presence = presence_to_local_clock(presence, krypto_protocol::now_ms());
                // Its repo index and agents feed "in repo X", "tell its codex" and the pickers: only
                // well-formed entries.
                presence.repos = crate::repos::validate_peer_repos(std::mem::take(&mut presence.repos));
                presence.agents = crate::repos::validate_peer_agents(std::mem::take(&mut presence.agents));
                let repos_changed =
                    self.peers().presence.get(&device_id).is_none_or(|old| old.repos != presence.repos);
                if repos_changed {
                    let (id, repos) = (device_id.clone(), presence.repos.clone());
                    if let Err(e) = self.inner.store.run(move |s| s.set_device_repos(&id, &repos)).await {
                        tracing::warn!("save the repos of {device_id}: {e:#}");
                    }
                }
                let agents_changed =
                    self.peers().presence.get(&device_id).is_none_or(|old| old.agents != presence.agents);
                if agents_changed {
                    // Kept for list_agents{machineId} while that hub is offline (or after a restart here).
                    let (id, agents) = (device_id.clone(), presence.agents.clone());
                    if let Err(e) = self.inner.store.run(move |s| s.set_device_agents(&id, &agents)).await {
                        tracing::warn!("save the agents of {device_id}: {e:#}");
                    }
                }
                self.peers().presence.insert(device_id.clone(), presence);
                self.sync_peers(Some((&device_id, true))).await;
            }
            PeerEvent::Revoked { device_id, by_peer } => {
                self.remove_device(&device_id, by_peer, if by_peer { "peer" } else { "hub" }).await;
            }
            PeerEvent::LinkPath { device_id, path } => {
                let known = {
                    let mut peers = self.peers();
                    let known = peers.roster.contains_key(&device_id);
                    if known && peers.paths.insert(device_id.clone(), path) != Some(path) {
                        tracing::info!("link to {device_id} now goes over the {} path", if path == LinkPath::Lan { "LAN" } else { "relay" });
                    }
                    known
                };
                if known {
                    self.sync_peers(None).await;
                }
            }
            PeerEvent::RelayStatus(info) => {
                tracing::info!("relay {}: connected={} last_error={:?}", info.url.as_deref().unwrap_or("off"), info.connected, info.last_error);
                let mut peer = self.peer_status();
                peer.relay = info;
                self.inner.bus.send_local(ServerMsg::PeerStatus { peer });
            }
        }
    }

    /// A device left the roster (revoked here, or by the peer with `bye{revoked}`): make sure the
    /// store says so, audit it once, and tell clients.
    async fn remove_device(&self, device_id: &str, by_peer: bool, actor: &str) {
        // Taking the entry makes this once-only: a later event for the same revoke finds nothing.
        let entry = {
            let mut peers = self.peers();
            let e = peers.roster.remove(device_id);
            e.or_else(|| peers.departed.remove(device_id))
        };
        let id = device_id.to_string();
        let now = krypto_protocol::now_ms();
        if let Err(e) = self.inner.store.run(move |s| s.revoke_device(&id, now)).await {
            tracing::error!("revoke {device_id} in hub.db: {e:#}");
        }
        // Revoke (either side) deletes everything mirrored from that machine.
        self.drop_mirror(device_id).await;
        if let Some(e) = entry {
            let (kind, verb) =
                if by_peer { (PairingAuditKind::RevokedByPeer, "unpaired by") } else { (PairingAuditKind::Revoked, "unpaired") };
            tracing::info!("{verb} {:?} ({})", e.device.name, e.device.id);
            self.audit_pairing(kind, Some(&e.device), e.device.addr.clone(), actor, None).await;
            if by_peer {
                let notification = notify::make(
                    &self.inner.machine_id,
                    NotificationLevel::Info,
                    format!("{} unpaired this machine", e.device.name),
                    format!("{} revoked the pairing; pair again with a new invite.", e.device.name),
                    None,
                    None,
                );
                self.inner.bus.send_local(ServerMsg::Notification { notification });
            }
        }
        self.sync_peers(None).await;
    }

    /// Re-read the roster and tell clients what changed: `devices` when the roster changed (not
    /// for lastSeen alone), `machine` when a hub was added, renamed or went online/offline,
    /// `machine_removed` when one left. `touched` = (device, online) from the event at hand;
    /// every other hub's state comes from the link layer.
    async fn sync_peers(&self, touched: Option<(&str, bool)>) {
        let roster = match self.inner.store.run(|s| s.roster()).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("read the pairing roster: {e:#}");
                return;
            }
        };
        let online: HashMap<String, UnixMs> = self.inner.peer.online().into_iter().collect();
        let mut out = Vec::new();
        {
            let mut guard = self.peers();
            let peers = &mut *guard;
            let next: BTreeMap<String, RosterEntry> = roster.into_iter().map(|e| (e.device.id.clone(), e)).collect();
            let old = std::mem::replace(&mut peers.roster, next);
            for (id, e) in old {
                if !peers.roster.contains_key(&id) {
                    peers.departed.insert(id, e);
                }
            }
            // Re-paired after leaving: no longer departed.
            peers.departed.retain(|id, _| !peers.roster.contains_key(id));
            let roster_ids: HashSet<String> = peers.roster.keys().cloned().collect();
            peers.paths.retain(|id, _| roster_ids.contains(id));
            let gone: Vec<String> = peers.sent.keys().filter(|id| !peers.roster.contains_key(*id)).cloned().collect();
            for id in gone {
                peers.sent.remove(&id);
                out.push(ServerMsg::MachineRemoved { machine_id: id });
            }
            peers.presence.retain(|id, _| peers.roster.contains_key(id));
            let devices = devices_key(&peers.roster, &peers.paths);
            if devices != peers.devices_sent {
                peers.devices_sent = devices;
                out.push(ServerMsg::Devices { devices: sorted_devices(&peers.roster, &peers.paths) });
            }
            for e in peers.roster.values().filter(|e| e.device.kind == DeviceKind::Hub) {
                let id = &e.device.id;
                let is_online = match touched {
                    Some((t, on)) if t == id => on,
                    _ => online.contains_key(id),
                };
                let m = live_machine(e, is_online, online.get(id).copied(), peers.presence.get(id));
                if peers.sent.get(id).is_none_or(|s| !same_view(s, &m)) {
                    peers.sent.insert(id.clone(), m.clone());
                    out.push(ServerMsg::Machine { machine: m });
                }
            }
        }
        for msg in out {
            self.inner.bus.send(msg);
        }
        self.check_active_machine(false);
    }

    async fn audit_pairing(
        &self,
        kind: PairingAuditKind,
        device: Option<&Device>,
        remote_addr: Option<String>,
        actor: &str,
        detail: Option<String>,
    ) {
        let entry = PairingAuditEntry {
            id: 0,
            ts: krypto_protocol::now_ms(),
            machine_id: self.inner.machine_id.clone(),
            kind,
            device_id: device.map(|d| d.id.clone()),
            device_name: device.map(|d| d.name.clone()),
            remote_addr,
            actor: actor.to_string(),
            detail,
        };
        if let Err(e) = self.inner.store.run(move |s| s.insert_pairing_audit(&entry)).await {
            tracing::error!("pairing audit write failed: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krypto_protocol::MachineOs;

    fn presence(ts: UnixMs, active: Option<UnixMs>) -> Presence {
        Presence {
            machine: Machine {
                id: "zyxwvu987654".into(),
                name: "Mac mini".into(),
                os: MachineOs::Macos,
                last_seen: ts,
                online: true,
                hub_version: None,
                user_active_at: active,
            },
            agents: vec![],
            repos: vec![],
            pets: vec![],
            user_active_at: active,
            ts,
        }
    }

    #[test]
    fn peer_clocks_are_converted_to_ours() {
        let now = 1_000_000;
        // Peer clock 5 min ahead: its "10 s ago" is our "10 s ago", not "5 min in the future".
        let p = presence_to_local_clock(presence(now + 300_000, Some(now + 290_000)), now);
        assert_eq!((p.user_active_at, p.machine.user_active_at), (Some(now - 10_000), Some(now - 10_000)));
        assert_eq!((p.ts, p.machine.last_seen), (now, now));
        // Peer clock 1 h behind.
        let p = presence_to_local_clock(presence(now - 3_600_000, Some(now - 3_600_000 - 2_000)), now);
        assert_eq!(p.user_active_at, Some(now - 2_000));
        // Activity stamped after the presence (peer races its own clock) is clamped to now.
        let p = presence_to_local_clock(presence(now, Some(now + 5_000)), now);
        assert_eq!(p.user_active_at, Some(now));
        assert_eq!(presence_to_local_clock(presence(now, None), now).user_active_at, None);
        // Absurd values never overflow and land in [0, now].
        let p = presence_to_local_clock(presence(i64::MIN, Some(i64::MAX)), now);
        assert_eq!(p.user_active_at, Some(now));
        let p = presence_to_local_clock(presence(i64::MAX, Some(i64::MIN)), now);
        assert_eq!((p.user_active_at, p.machine.last_seen), (Some(0), now), "lastSeen = the presence time = now");
    }

    #[test]
    fn machine_diffs_never_overflow() {
        let m = |at: Option<UnixMs>| presence(0, at).machine;
        assert!(!same_view(&m(Some(i64::MIN)), &m(Some(i64::MAX))));
        assert!(same_view(&m(Some(i64::MAX)), &m(Some(i64::MAX - 1))));
        assert!(!same_view(&m(Some(0)), &m(None)));
        assert!(same_view(&m(Some(1_000)), &m(Some(1_000 + ACTIVITY_STEP_MS - 1))));
        assert!(!same_view(&m(Some(1_000)), &m(Some(1_000 + ACTIVITY_STEP_MS))));
    }
}
