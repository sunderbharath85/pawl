//! Read-only mirror of paired hubs (PLAN step 3).
//!
//! **Outbound (owner).** Every local `session` / `event` / `pet` / `permission_request` /
//! `permission_resolved` / `notification` broadcast for an item of this machine is copied by the bus
//! tap and sent to every linked hub as `mirror{update}`. Items of other machines are never forwarded.
//! A peer's `sync_request{sinceSeq}` gets `mirror_snapshot`, `mirror_events` batches and `sync_done`.
//!
//! **Inbound (mirror).** Bodies from a paired hub are validated item by item: an item is dropped
//! (warn log with the device id, never the payload) unless its machineId and the machine prefix of
//! every id it carries are the sender's device id, and an event's id is `machine:seq`. The store
//! re-checks the same rules and refuses this hub's own machine id. Valid items are stored in the
//! normal tables under the owner's machine id and broadcast to local clients as ordinary
//! `ServerMsg`s. Rules, the watchdog and auto-approve never see them (they only read `State`).

use std::collections::{HashMap, HashSet};

use pawl_peer::SendError;
use pawl_protocol::{
    truncated_payload, DeviceKind, MirrorUpdate, PeerBody, MIRROR_FRAME_MAX_BYTES, MIRROR_PAYLOAD_MAX_BYTES,
    MIRROR_SYNC_MAX_EVENTS,
};
use tokio::sync::mpsc::UnboundedReceiver;

use super::*;
use crate::store::MIRROR_RETENTION_MS;

/// Sessions sent in a `mirror_snapshot`.
pub const SYNC_MAX_SESSIONS: u32 = 200;
/// Room left in a `mirror_events` frame for the envelope around the events.
const BATCH_HEADROOM: usize = 4096;
/// How often mirrored sessions that ended long ago are pruned.
const MIRROR_PRUNE_EVERY: Duration = Duration::from_secs(3600);
/// More bodies than this from one link within one second is a flood: the link is closed.
pub const MIRROR_FLOOD_FRAMES: u32 = 1000;
/// How long a flooding peer stays disconnected (it resyncs when it comes back).
pub const MIRROR_FLOOD_COOLDOWN: Duration = Duration::from_secs(30);
/// At most one sync is answered per peer per this interval; requests meanwhile are coalesced.
#[cfg(not(test))]
pub const SYNC_COOLDOWN: Duration = Duration::from_secs(10);
#[cfg(test)]
pub const SYNC_COOLDOWN: Duration = Duration::from_millis(300);
/// Automatic renames of this hub's pets that one peer's pets may cause per hour.
pub const MAX_PEER_RENAMES_PER_HOUR: usize = 10;
const HOUR_MS: i64 = 3_600_000;
/// Peer timestamps are clamped to [0, now + this].
const MAX_FUTURE_MS: i64 = DAY_MS;

/// Owner side: answering one peer's sync requests.
#[derive(Default)]
struct SyncServe {
    /// A sync is being answered, or its cooldown has not passed.
    busy: bool,
    /// The latest request that came in while busy (answered next; older extras are dropped).
    pending: Option<i64>,
}

/// What this hub knows of its paired hubs' state (a copy of their rows in the store).
#[derive(Default)]
pub(super) struct MirrorCache {
    /// Open and recently active mirrored sessions (same window as local ones).
    pub(super) sessions: HashMap<SessionId, Session>,
    pub(super) pets: HashMap<PetId, Pet>,
    pub(super) pending: HashMap<RequestId, PermissionRequest>,
    /// Links whose sync finished since their latest LinkUp: live events then advance the cursor.
    synced: HashSet<String>,
    /// Links with a `sync_request` of ours outstanding: only they may finish a sync.
    sync_outstanding: HashSet<String>,
    /// Inbound bodies per link: (start of the current one-second window, bodies in it).
    rate: HashMap<String, (tokio::time::Instant, u32)>,
    /// Owner side: sync answering per peer.
    serving: HashMap<String, SyncServe>,
    /// Syncs answered (all peers); tests read it.
    pub(super) syncs_answered: u64,
    /// When this hub's pets were renamed because of each peer's pets (last hour).
    renames: HashMap<String, Vec<UnixMs>>,
    /// The cross-machine name clashes the last settle left in place.
    last_clashes: Option<Vec<(String, String)>>,
}

impl MirrorCache {
    fn clear_machine(&mut self, machine_id: &str) {
        self.sessions.retain(|_, s| s.machine_id != machine_id);
        self.pets.retain(|_, p| p.machine_id != machine_id);
        self.pending.retain(|_, r| r.machine_id != machine_id);
        self.synced.remove(machine_id);
        self.sync_outstanding.remove(machine_id);
        self.rate.remove(machine_id);
        self.serving.remove(machine_id);
    }
}

// ---------------------------------------------------------------- validation

/// `id` is a global id of `machine_id`.
fn owned_by(id: &str, machine_id: &str) -> bool {
    parse_global_id(id).is_some_and(|(m, _)| m == machine_id)
}

/// Why an inbound item from `from` is refused (None = accept).
pub(crate) fn check_session(s: &Session, from: &str) -> Option<&'static str> {
    if s.machine_id != from {
        return Some("machineId is not the sender");
    }
    if !owned_by(&s.id, from) {
        return Some("session id is not the sender's");
    }
    if s.cwd.chars().count() > crate::repos::PEER_REPO_PATH_MAX || s.cwd.chars().any(char::is_control) {
        return Some("session cwd is over 1024 characters or has control characters");
    }
    None
}

pub(crate) fn check_event(e: &HubEvent, from: &str) -> Option<&'static str> {
    if e.machine_id != from {
        return Some("machineId is not the sender");
    }
    if e.seq <= 0 || e.id != global_id(from, e.seq) {
        return Some("event id is not machineId:seq");
    }
    if !owned_by(&e.session_id, from) {
        return Some("sessionId is not the sender's");
    }
    if payload_len(&e.payload) > MIRROR_PAYLOAD_MAX_BYTES {
        return Some("payload over MIRROR_PAYLOAD_MAX_BYTES");
    }
    None
}

pub(crate) fn check_pet(p: &Pet, from: &str) -> Option<&'static str> {
    if p.machine_id != from {
        return Some("machineId is not the sender");
    }
    if !owned_by(&p.id, from) {
        return Some("pet id is not the sender's");
    }
    // The same rules as this hub's own pets (names also feed replies and LLM prompts).
    if validate_pet_name(&p.name).is_err() {
        return Some("pet name is not 1-40 characters without control characters");
    }
    let bad = |v: &str, max: usize| v.chars().count() > max || v.chars().any(char::is_control);
    if p.skin.trim().is_empty() || bad(&p.skin, 40) || p.agent_id.is_empty() || bad(&p.agent_id, 128) {
        return Some("pet skin or agent id is invalid");
    }
    if p.personality.chars().count() > 300 {
        return Some("pet personality is longer than 300 characters");
    }
    None
}

/// A peer's timestamp, clamped to [0, now + 1 day] so arithmetic on it never overflows and a
/// wild clock cannot pin an item to the top of every list.
pub(crate) fn clamp_ts(t: UnixMs, now: UnixMs) -> UnixMs {
    t.clamp(0, now.saturating_add(MAX_FUTURE_MS))
}

fn clamp_session(mut s: Session, now: UnixMs) -> Session {
    s.started_at = clamp_ts(s.started_at, now);
    s.last_event_at = clamp_ts(s.last_event_at, now);
    s.ended_at = s.ended_at.map(|t| clamp_ts(t, now));
    // Only a known purpose is kept.
    s.purpose = s.purpose.filter(|p| p == super::browser::PURPOSE);
    s
}

fn clamp_event(mut e: HubEvent, now: UnixMs) -> HubEvent {
    e.ts = clamp_ts(e.ts, now);
    e
}

fn clamp_request(mut r: PermissionRequest, now: UnixMs) -> PermissionRequest {
    r.created_at = clamp_ts(r.created_at, now);
    // Only a well-formed voice code (two digits) counts; anything else means "answer on the screen".
    r.voice_code = r.voice_code.filter(|c| pawl_brain::chat::spoken::is_voice_code(c));
    r
}

fn clean_pet(mut p: Pet) -> Pet {
    p.name = p.name.trim().to_string();
    p
}

pub(crate) fn check_request(r: &PermissionRequest, from: &str) -> Option<&'static str> {
    if r.machine_id != from {
        return Some("machineId is not the sender");
    }
    if !owned_by(&r.request_id, from) || !owned_by(&r.session_id, from) {
        return Some("requestId or sessionId is not the sender's");
    }
    None
}

pub(crate) fn check_resolved(request_id: &str, session_id: &str, from: &str) -> Option<&'static str> {
    if !owned_by(request_id, from) || !owned_by(session_id, from) {
        return Some("requestId or sessionId is not the sender's");
    }
    None
}

pub(crate) fn check_notification(n: &HubNotification, from: &str) -> Option<&'static str> {
    if n.machine_id != from {
        return Some("machineId is not the sender");
    }
    if !owned_by(&n.id, from) || n.session_id.as_deref().is_some_and(|s| !owned_by(s, from)) {
        return Some("notification ids are not the sender's");
    }
    None
}

fn dropped(from: &str, item: &str, why: &str) {
    tracing::warn!(device = %from, item, reason = why, "dropped a mirrored item");
}

fn payload_len(v: &Value) -> usize {
    serde_json::to_vec(v).map(|b| b.len()).unwrap_or(usize::MAX)
}

/// An event as peers receive it: payloads over MIRROR_PAYLOAD_MAX_BYTES become a TruncatedPayload.
pub(crate) fn mirror_event(mut e: HubEvent) -> HubEvent {
    let n = payload_len(&e.payload);
    if n > MIRROR_PAYLOAD_MAX_BYTES {
        e.payload = truncated_payload(n);
    }
    e
}

fn body_len(b: &PeerBody) -> usize {
    serde_json::to_vec(b).map(|v| v.len()).unwrap_or(usize::MAX)
}

impl Hub {
    pub(super) fn mirror(&self) -> MutexGuard<'_, MirrorCache> {
        self.inner.mirror.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Fill the cache from the store (startup, after pruning).
    pub(super) fn set_mirror(&self, sessions: Vec<Session>, pets: Vec<Pet>, pending: Vec<PermissionRequest>) {
        let mut m = self.mirror();
        m.sessions = sessions.into_iter().map(|s| (s.id.clone(), s)).collect();
        m.pets = pets.into_iter().map(|p| (p.id.clone(), p)).collect();
        m.pending = pending.into_iter().map(|r| (r.request_id.clone(), r)).collect();
    }

    /// Tests: behave as if a sync_request of ours to `device_id` were outstanding.
    #[cfg(test)]
    pub(super) fn expect_sync(&self, device_id: &str) {
        self.mirror().sync_outstanding.insert(device_id.to_string());
    }

    #[cfg(test)]
    pub(super) fn syncs_answered(&self) -> u64 {
        self.mirror().syncs_answered
    }

    #[cfg(test)]
    pub(super) fn is_synced(&self, device_id: &str) -> bool {
        self.mirror().synced.contains(device_id)
    }

    /// Mirrored sessions matching `filter`, most recent activity first.
    pub(super) fn mirror_sessions(&self, filter: impl Fn(&Session) -> bool) -> Vec<Session> {
        self.mirror().sessions.values().filter(|s| filter(s)).cloned().collect()
    }

    pub(super) fn mirror_pets(&self) -> Vec<Pet> {
        self.mirror().pets.values().cloned().collect()
    }

    pub(super) fn mirror_pending(&self) -> Vec<PermissionRequest> {
        self.mirror().pending.values().cloned().collect()
    }

    /// Whether `session_id` is a session mirrored from a paired hub.
    pub(super) async fn is_mirrored_session(&self, session_id: &str) -> Result<bool> {
        if self.mirror().sessions.contains_key(session_id) {
            return Ok(true);
        }
        let id = session_id.to_string();
        self.inner.store.run(move |s| s.is_mirrored_session(&id)).await
    }

    /// Re-read one machine's mirror from the store into the cache.
    async fn reload_mirror(&self, machine_id: &str) -> Result<()> {
        let since = pawl_protocol::now_ms() - MIRROR_RETENTION_MS;
        let mid = machine_id.to_string();
        let (sessions, pets, pending) = self
            .inner
            .store
            .run(move |s| {
                let sessions: Vec<Session> = s.mirror_sessions(since)?.into_iter().filter(|x| x.machine_id == mid).collect();
                let pets: Vec<Pet> = s.pets()?.into_iter().filter(|p| p.machine_id == mid).collect();
                let pending: Vec<PermissionRequest> =
                    s.mirror_pending()?.into_iter().filter(|r| r.machine_id == mid).collect();
                Ok((sessions, pets, pending))
            })
            .await?;
        let mut m = self.mirror();
        m.sessions.retain(|_, s| s.machine_id != machine_id);
        m.pets.retain(|_, p| p.machine_id != machine_id);
        m.pending.retain(|_, r| r.machine_id != machine_id);
        m.sessions.extend(sessions.into_iter().map(|s| (s.id.clone(), s)));
        m.pets.extend(pets.into_iter().map(|p| (p.id.clone(), p)));
        m.pending.extend(pending.into_iter().map(|r| (r.request_id.clone(), r)));
        Ok(())
    }

    /// A peer's pets arrived: if one of ours shares a name with a pet of a machine whose id sorts
    /// first, ours takes a new name (the other hub applies the same rule, so exactly one side
    /// renames; see [`dedupe_pet_names`]). Runs only when the set of clashes changed, and one peer
    /// can cause at most MAX_PEER_RENAMES_PER_HOUR renames: beyond that the clash stays (warn).
    /// Renames are saved, broadcast and mirrored back.
    pub(super) async fn settle_pet_names(&self) {
        let now = pawl_protocol::now_ms();
        let mid = self.inner.machine_id.clone();
        let foreign = self.mirror_pets();
        let local = self.st().sorted_pets();
        // name key -> the machine (sorting before ours) whose pet holds it
        let mut winners: HashMap<String, String> = HashMap::new();
        for p in foreign.iter().filter(|p| p.machine_id.as_str() < mid.as_str()) {
            let e = winners.entry(name_key(&p.name)).or_insert_with(|| p.machine_id.clone());
            if p.machine_id < *e {
                *e = p.machine_id.clone();
            }
        }
        let clash_of = |p: &Pet| winners.get(&name_key(&p.name)).map(|m| (m.clone(), name_key(&p.name)));
        let mut clashes: Vec<(String, String)> = local.iter().filter_map(clash_of).collect();
        clashes.sort();
        let mut taken: Vec<String> = foreign.iter().chain(local.iter()).map(|p| p.name.clone()).collect();
        let mut plan: Vec<(PetId, String)> = Vec::new();
        {
            let mut m = self.mirror();
            if m.last_clashes.as_ref() == Some(&clashes) {
                return;
            }
            let mut left = Vec::new();
            for p in local.iter() {
                let Some((peer, key)) = clash_of(p) else { continue };
                let budget = m.renames.entry(peer.clone()).or_default();
                budget.retain(|t| now.saturating_sub(*t) < HOUR_MS);
                if budget.len() >= MAX_PEER_RENAMES_PER_HOUR {
                    tracing::warn!(
                        device = %peer,
                        "pet {} keeps a name a paired machine's pet also has: {MAX_PEER_RENAMES_PER_HOUR} renames this hour already",
                        p.id
                    );
                    left.push((peer, key));
                    continue;
                }
                budget.push(now);
                let name = pawl_brain::growth::default_name(&mid, &p.agent_id, &taken);
                taken.push(name.clone());
                plan.push((p.id.clone(), name));
            }
            left.sort();
            m.last_clashes = Some(left);
        }
        let renamed: Vec<Pet> = {
            let mut st = self.st();
            let mut out = Vec::new();
            for (id, name) in plan {
                if let Some(slot) = st.pets.get_mut(&id) {
                    slot.name = name;
                    out.push(slot.clone());
                    st.dirty_pets.remove(&id);
                }
            }
            out
        };
        if renamed.is_empty() {
            return;
        }
        let r2 = renamed.clone();
        if let Err(e) = self
            .inner
            .store
            .run(move |s| {
                for p in &r2 {
                    s.upsert_pet(p)?;
                }
                Ok(())
            })
            .await
        {
            tracing::warn!("persist renamed pets: {e:#}");
        }
        for pet in renamed {
            tracing::info!("pet {} ({}) renamed to {:?}: a paired machine's pet has its name", pet.id, pet.agent_id, pet.name);
            self.inner.bus.send(ServerMsg::Pet { pet });
        }
        self.refresh_presence();
    }

    /// A machine was revoked (here or by the peer): delete every mirrored row of it.
    pub(super) async fn drop_mirror(&self, machine_id: &str) {
        if machine_id == self.inner.machine_id {
            return;
        }
        self.mirror().clear_machine(machine_id);
        let mid = machine_id.to_string();
        if let Err(e) = self.inner.store.run(move |s| s.delete_mirror(&mid)).await {
            tracing::error!("delete the mirror of {machine_id}: {e:#}");
        }
    }

    pub(super) async fn mirror_prune_loop(self) {
        let mut shutdown = self.shutdown_rx();
        let mut iv = tokio::time::interval_at(tokio::time::Instant::now() + MIRROR_PRUNE_EVERY, MIRROR_PRUNE_EVERY);
        loop {
            tokio::select! {
                _ = iv.tick() => {
                    let now = pawl_protocol::now_ms();
                    if let Err(e) = self.inner.store.run(move |s| s.prune_intent_log(now, crate::store::INTENT_LOG_MAX_ROWS)).await {
                        tracing::warn!("prune the intent log: {e:#}");
                    }
                    match self.inner.store.run(move |s| s.prune_mirror(now)).await {
                        Ok(0) => {}
                        Ok(n) => {
                            tracing::info!("pruned {n} mirrored sessions");
                            let ids: Vec<String> = self.devices().into_iter().map(|d| d.id).collect();
                            for id in ids {
                                if let Err(e) = self.reload_mirror(&id).await {
                                    tracing::warn!("reload the mirror of {id}: {e:#}");
                                }
                            }
                        }
                        Err(e) => tracing::warn!("prune the mirror: {e:#}"),
                    }
                }
                _ = shutdown.changed() => break,
            }
        }
    }

    // ------------------------------------------------------------ outbound (owner)

    /// Linked paired devices, hubs and phones (the devices mirror updates go to).
    fn linked_devices(&self) -> Vec<String> {
        let paired: HashSet<String> = self.devices().into_iter().map(|d| d.id).collect();
        self.inner.peer.online().into_iter().map(|(id, _)| id).filter(|id| paired.contains(id)).collect()
    }

    /// The mirror update for a local broadcast, if it is about an item of this machine.
    pub(crate) fn outbound_update(&self, msg: ServerMsg) -> Option<MirrorUpdate> {
        let me = self.inner.machine_id.as_str();
        match msg {
            ServerMsg::Session { session } if session.machine_id == me => Some(MirrorUpdate::Session { session }),
            ServerMsg::Event { event } if event.machine_id == me => Some(MirrorUpdate::Event { event: mirror_event(event) }),
            ServerMsg::Pet { pet } if pet.machine_id == me => Some(MirrorUpdate::Pet { pet }),
            ServerMsg::PermissionRequest { request } if request.machine_id == me => {
                Some(MirrorUpdate::PermissionRequest { request })
            }
            ServerMsg::PermissionResolved { request_id, session_id, outcome, by, by_machine_id } if owned_by(&request_id, me) => {
                Some(MirrorUpdate::PermissionResolved { request_id, session_id, outcome, by, by_machine_id })
            }
            ServerMsg::Notification { notification } if notification.machine_id == me => {
                Some(MirrorUpdate::Notification { notification })
            }
            _ => None,
        }
    }

    /// Queue a body for a peer. False (and logged, without the body) if it could not be queued.
    fn send_peer(&self, device_id: &str, body: PeerBody, what: &str) -> bool {
        match self.inner.peer.send(device_id, body) {
            Ok(()) => true,
            Err(SendError::NotLinked) => false,
            Err(SendError::TooLarge) => {
                tracing::warn!(device = %device_id, what, "not mirrored: body over the frame limit");
                false
            }
            Err(e) => {
                tracing::warn!(device = %device_id, what, "not mirrored: {e}");
                false
            }
        }
    }

    pub(super) async fn mirror_out_loop(self, mut rx: UnboundedReceiver<ServerMsg>) {
        let mut shutdown = self.shutdown_rx();
        loop {
            let msg = tokio::select! {
                m = rx.recv() => match m { Some(m) => m, None => break },
                _ = shutdown.changed() => break,
            };
            let Some(update) = self.outbound_update(msg) else { continue };
            let peers = self.linked_devices();
            if peers.is_empty() {
                continue;
            }
            let mut body = PeerBody::Mirror { update };
            if body_len(&body) > MIRROR_FRAME_MAX_BYTES {
                body = shrink_update(body);
            }
            for id in peers {
                self.send_peer(&id, body.clone(), "mirror");
            }
        }
    }

    /// Answer a peer's `sync_request`: snapshot, then the events it lacks, then `sync_done`.
    pub(super) async fn answer_sync(self, device_id: String, since_seq: i64) {
        let since_seq = since_seq.max(0);
        let now = pawl_protocol::now_ms();
        let stored = match self.inner.store.run(move |s| s.sync_sessions(now - MIRROR_RETENTION_MS, SYNC_MAX_SESSIONS)).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("sync for {device_id}: read sessions: {e:#}");
                return;
            }
        };
        let (sessions, pets, pending) = {
            let st = self.st();
            // The cache is fresher than the store for open sessions.
            let sessions: Vec<Session> = stored.into_iter().map(|s| st.sessions.get(&s.id).cloned().unwrap_or(s)).collect();
            let pets: Vec<Pet> = st.sorted_pets().into_iter().filter(|p| p.machine_id == st.machine_id).collect();
            (sessions, pets, st.sorted_pending())
        };
        let snapshot = fit_snapshot(sessions, pets, pending);
        if !self.send_peer(&device_id, snapshot, "mirror_snapshot") {
            return;
        }
        let (events, truncated) =
            match self.inner.store.run(move |s| s.local_events_after(since_seq, MIRROR_SYNC_MAX_EVENTS)).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("sync for {device_id}: read events: {e:#}");
                    return;
                }
            };
        let mut max_sent: Option<i64> = None;
        let mut batch: Vec<HubEvent> = Vec::new();
        let mut size = 0;
        let budget = MIRROR_FRAME_MAX_BYTES - BATCH_HEADROOM;
        for e in events {
            let e = mirror_event(e);
            let n = serde_json::to_vec(&e).map(|v| v.len()).unwrap_or(budget);
            if !batch.is_empty() && size + n + 1 > budget {
                if !self.send_peer(&device_id, PeerBody::MirrorEvents { events: std::mem::take(&mut batch) }, "mirror_events") {
                    return;
                }
                size = 0;
            }
            max_sent = Some(max_sent.unwrap_or(0).max(e.seq));
            size += n + 1;
            batch.push(e);
        }
        if !batch.is_empty() && !self.send_peer(&device_id, PeerBody::MirrorEvents { events: batch }, "mirror_events") {
            return;
        }
        // Nothing newer than since_seq: report our real highest seq, so a peer whose cursor is
        // ahead of it can tell our seq space restarted.
        let max_seq = match max_sent {
            Some(s) => s,
            None => self.inner.store.run(|s| s.local_max_seq()).await.unwrap_or(0),
        };
        self.send_peer(&device_id, PeerBody::SyncDone { seq: max_seq, truncated }, "sync_done");
        tracing::info!("sent {device_id} a mirror sync (events up to seq {max_seq}{})", if truncated { ", older skipped" } else { "" });
    }

    /// Right after a LinkUp (every one: a new link may replace a live one and lose queued bodies).
    pub(super) async fn request_sync(&self, device_id: &str) {
        let id = device_id.to_string();
        let since_seq = match self.inner.store.run(move |s| s.mirror_cursor(&id)).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("read the mirror cursor of {device_id}: {e:#}");
                0
            }
        };
        self.send_sync_request(device_id, since_seq);
    }

    fn send_sync_request(&self, device_id: &str, since_seq: i64) {
        {
            let mut m = self.mirror();
            m.synced.remove(device_id);
            m.sync_outstanding.insert(device_id.to_string());
        }
        if !self.send_peer(device_id, PeerBody::SyncRequest { since_seq }, "sync_request") {
            self.mirror().sync_outstanding.remove(device_id);
        }
    }

    pub(super) fn link_lost(&self, device_id: &str) {
        let mut m = self.mirror();
        m.synced.remove(device_id);
        m.sync_outstanding.remove(device_id);
        m.rate.remove(device_id);
    }

    /// Count one inbound body of `from`. False once the link sent more than MIRROR_FLOOD_FRAMES in a
    /// second: the link is closed for MIRROR_FLOOD_COOLDOWN and resyncs when it comes back.
    fn admit_body(&self, from: &str) -> bool {
        let now = tokio::time::Instant::now();
        let n = {
            let mut m = self.mirror();
            let w = m.rate.entry(from.to_string()).or_insert((now, 0));
            if now.duration_since(w.0) >= Duration::from_secs(1) {
                *w = (now, 0);
            }
            w.1 = w.1.saturating_add(1);
            w.1
        };
        if n <= MIRROR_FLOOD_FRAMES {
            return true;
        }
        if n == MIRROR_FLOOD_FRAMES + 1 {
            tracing::warn!(
                device = %from,
                "more than {MIRROR_FLOOD_FRAMES} mirror frames in one second: closing the link for {} s",
                MIRROR_FLOOD_COOLDOWN.as_secs()
            );
            {
                let mut m = self.mirror();
                m.synced.remove(from);
                m.sync_outstanding.remove(from);
            }
            self.inner.peer.disconnect(from, MIRROR_FLOOD_COOLDOWN);
        }
        false
    }

    /// A peer asked for a sync: answer at once if idle, else keep the latest request for after the
    /// current answer and its SYNC_COOLDOWN (older extras are dropped).
    fn on_sync_request(&self, from: &str, since_seq: i64) {
        let start = {
            let mut m = self.mirror();
            let s = m.serving.entry(from.to_string()).or_default();
            if s.busy {
                if s.pending.replace(since_seq).is_some() {
                    tracing::debug!(device = %from, "dropped an extra sync_request (one answer per {SYNC_COOLDOWN:?})");
                } else {
                    tracing::debug!(device = %from, "deferred a sync_request (one answer per {SYNC_COOLDOWN:?})");
                }
                false
            } else {
                s.busy = true;
                true
            }
        };
        if start {
            tokio::spawn(self.clone().serve_syncs(from.to_string(), since_seq));
        }
    }

    async fn serve_syncs(self, from: String, mut since_seq: i64) {
        loop {
            let started = tokio::time::Instant::now();
            self.clone().answer_sync(from.clone(), since_seq).await;
            self.mirror().syncs_answered += 1;
            tokio::time::sleep_until(started + SYNC_COOLDOWN).await;
            let next = {
                let mut m = self.mirror();
                match m.serving.get_mut(&from) {
                    Some(s) => match s.pending.take() {
                        Some(p) => Some(p),
                        None => {
                            s.busy = false;
                            None
                        }
                    },
                    None => None,
                }
            };
            match next {
                Some(p) => since_seq = p,
                None => break,
            }
        }
    }

    /// `sync_done` from `from`: honoured only for a sync_request of ours on this link. The cursor
    /// becomes the highest seq actually stored. A `seq` below our cursor means the peer's seq space
    /// restarted (its hub.db was reset): its mirrored events are dropped and a full sync requested.
    async fn finish_sync(&self, from: &str, seq: i64, truncated: bool) {
        if !self.mirror().sync_outstanding.remove(from) {
            tracing::debug!(device = %from, "ignoring a sync_done nobody asked for");
            return;
        }
        let store = &self.inner.store;
        let mid = from.to_string();
        let before = match store.run(move |s| s.mirror_cursor(&mid)).await {
            Ok(v) => v,
            Err(e) => return tracing::warn!("read the mirror cursor of {from}: {e:#}"),
        };
        if seq < before {
            tracing::warn!(
                device = %from,
                "its event seq went back ({seq} < our cursor {before}): its hub.db was reset? resyncing its events from 0"
            );
            let mid = from.to_string();
            if let Err(e) = store.run(move |s| s.reset_mirror_events(&mid)).await {
                return tracing::warn!("reset the mirror of {from}: {e:#}");
            }
            self.send_sync_request(from, 0);
            return;
        }
        let mid = from.to_string();
        match store.run(move |s| s.finish_mirror_sync(&mid)).await {
            Ok(cursor) => {
                self.mirror().synced.insert(from.to_string());
                tracing::info!(
                    "mirror of {from} in sync up to seq {cursor}{}",
                    if truncated { " (older events were not sent)" } else { "" }
                );
            }
            Err(e) => tracing::warn!("finish the sync of {from}: {e:#}"),
        }
    }

    // ------------------------------------------------------------ inbound (mirror)

    /// A body from a linked device (pair/hello/presence/bye never come here). Called in arrival order.
    pub(super) async fn on_peer_body(&self, from: &str, body: PeerBody) {
        let kind = self.devices().into_iter().find(|d| d.id == from).map(|d| d.kind);
        let Some(kind) = kind.filter(|_| from != self.inner.machine_id && is_valid_machine_id(from)) else {
            tracing::warn!(device = %from, "ignoring a peer body from a device that is not paired");
            return;
        };
        if !self.admit_body(from) {
            return;
        }
        if kind == DeviceKind::Phone {
            return self.on_phone_body(from, body).await;
        }
        match body {
            PeerBody::Mirror { update } => self.apply_update(from, update).await,
            PeerBody::SyncRequest { since_seq } => self.on_sync_request(from, since_seq),
            PeerBody::MirrorSnapshot { sessions, pets, pending_permissions } => {
                self.apply_snapshot(from, sessions, pets, pending_permissions).await
            }
            PeerBody::MirrorEvents { events } => self.apply_events(from, events).await,
            PeerBody::SyncDone { seq, truncated } => self.finish_sync(from, seq, truncated).await,
            PeerBody::Intent { intent_id, client, confirmed, intent, .. } => {
                self.on_intent(from, intent_id, client, confirmed, intent)
            }
            PeerBody::IntentAck { intent_id, status, reason, result } => {
                self.on_intent_ack(from, &intent_id, super::intents::Ack { status, reason, result })
            }
            // A2A over the link (protocol v9): start / prompt / cancel and their answers.
            PeerBody::A2a { call_id, frame } => self.on_a2a_frame(from, call_id, frame),
            PeerBody::RosterIntro { .. }
            | PeerBody::PairRequest { .. }
            | PeerBody::PairAccept { .. }
            | PeerBody::PairReject { .. }
            | PeerBody::Hello { .. }
            | PeerBody::Presence { .. }
            | PeerBody::Bye { .. } => {
                tracing::debug!(device = %from, "ignoring a link-layer body delivered as data");
            }
        }
    }

    /// A body from a paired phone. A phone reads this hub's own items (sync_request, then the live
    /// mirror) and acts through intents, always as client `mobile` whatever it claims; it keeps no
    /// mirror we could ask for (and the node drops its intros). Everything else is dropped.
    async fn on_phone_body(&self, from: &str, body: PeerBody) {
        match body {
            PeerBody::SyncRequest { since_seq } => self.on_sync_request(from, since_seq),
            PeerBody::Intent { intent_id, confirmed, intent, .. } => {
                self.on_intent(from, intent_id, ClientKind::Mobile, confirmed, intent)
            }
            other => {
                let kind = serde_json::to_value(&other).ok().and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_string));
                tracing::debug!(device = %from, body = kind.as_deref().unwrap_or("?"), "ignoring a body a phone does not send");
            }
        }
    }

    async fn apply_update(&self, from: &str, update: MirrorUpdate) {
        let store = &self.inner.store;
        match update {
            MirrorUpdate::Session { session } => {
                if let Some(why) = check_session(&session, from) {
                    return dropped(from, "session", why);
                }
                let session = clamp_session(session, pawl_protocol::now_ms());
                let s = session.clone();
                let evicted = match store.run(move |st| st.mirror_upsert_session(&s)).await {
                    Ok(v) => v,
                    Err(e) => return dropped(from, "session", &format!("{e:#}")),
                };
                let over = {
                    let mut m = self.mirror();
                    for id in &evicted {
                        m.sessions.remove(id);
                    }
                    if !evicted.contains(&session.id) {
                        m.sessions.insert(session.id.clone(), session.clone());
                    }
                    m.sessions.values().filter(|x| x.machine_id == from).count() as i64 > crate::store::MIRROR_MAX_SESSIONS
                };
                if over {
                    // Never more in memory than the store keeps.
                    if let Err(e) = self.reload_mirror(from).await {
                        tracing::warn!(device = %from, "mirror: reload: {e:#}");
                    }
                }
                if !evicted.contains(&session.id) {
                    self.inner.bus.send_local(ServerMsg::Session { session });
                }
            }
            MirrorUpdate::Event { event } => self.apply_events(from, vec![event]).await,
            MirrorUpdate::Pet { pet } => {
                if let Some(why) = check_pet(&pet, from) {
                    return dropped(from, "pet", why);
                }
                let pet = clean_pet(pet);
                let p = pet.clone();
                if let Err(e) = store.run(move |st| st.mirror_upsert_pet(&p)).await {
                    return dropped(from, "pet", &format!("{e:#}"));
                }
                {
                    let mut m = self.mirror();
                    // One pet per agent per machine: a new id for the same agent replaces the old.
                    m.pets.retain(|id, p| !(p.machine_id == pet.machine_id && p.agent_id == pet.agent_id && *id != pet.id));
                    m.pets.insert(pet.id.clone(), pet.clone());
                }
                self.inner.bus.send_local(ServerMsg::Pet { pet });
                self.settle_pet_names().await;
            }
            MirrorUpdate::PermissionRequest { request } => {
                if let Some(why) = check_request(&request, from) {
                    return dropped(from, "permission_request", why);
                }
                let request = clamp_request(request, pawl_protocol::now_ms());
                let r = request.clone();
                if let Err(e) = store.run(move |st| st.mirror_put_pending(&r)).await {
                    return dropped(from, "permission_request", &format!("{e:#}"));
                }
                self.mirror().pending.insert(request.request_id.clone(), request.clone());
                self.inner.bus.send_local(ServerMsg::PermissionRequest { request });
            }
            MirrorUpdate::PermissionResolved { request_id, session_id, outcome, by, by_machine_id } => {
                if let Some(why) = check_resolved(&request_id, &session_id, from) {
                    return dropped(from, "permission_resolved", why);
                }
                // Who decided: informational, but only ever a well-formed machine id.
                let by_machine_id = by_machine_id.filter(|m| is_valid_machine_id(m));
                let (mid, rid) = (from.to_string(), request_id.clone());
                if let Err(e) = store.run(move |st| st.mirror_remove_pending(&mid, &rid)).await {
                    tracing::warn!("mirror: remove a pending request of {from}: {e:#}");
                }
                self.mirror().pending.remove(&request_id);
                self.inner.bus.send_local(ServerMsg::PermissionResolved { request_id, session_id, outcome, by, by_machine_id });
            }
            MirrorUpdate::Notification { notification } => {
                if let Some(why) = check_notification(&notification, from) {
                    return dropped(from, "notification", why);
                }
                let mut notification = notification;
                notification.ts = clamp_ts(notification.ts, pawl_protocol::now_ms());
                self.inner.bus.send_local(ServerMsg::Notification { notification });
            }
        }
    }

    async fn apply_events(&self, from: &str, events: Vec<HubEvent>) {
        let now = pawl_protocol::now_ms();
        let mut valid = Vec::with_capacity(events.len());
        for e in events {
            match check_event(&e, from) {
                Some(why) => dropped(from, "event", why),
                None => valid.push(clamp_event(e, now)),
            }
        }
        if valid.is_empty() {
            return;
        }
        let synced = self.mirror().synced.contains(from);
        let mid = from.to_string();
        let new = match self.inner.store.run(move |s| s.mirror_insert_events(&mid, &valid, synced)).await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(device = %from, "mirror: store events: {e:#}");
                return;
            }
        };
        {
            let mut m = self.mirror();
            for e in &new {
                if let Some(s) = m.sessions.get_mut(&e.session_id) {
                    s.last_event_at = s.last_event_at.max(e.ts);
                }
            }
        }
        for event in new {
            self.inner.bus.send_local(ServerMsg::Event { event });
        }
    }

    async fn apply_snapshot(&self, from: &str, sessions: Vec<Session>, pets: Vec<Pet>, pending: Vec<PermissionRequest>) {
        let now = pawl_protocol::now_ms();
        let sessions: Vec<Session> = sessions
            .into_iter()
            .filter(|s| match check_session(s, from) {
                Some(why) => {
                    dropped(from, "session", why);
                    false
                }
                None => true,
            })
            .map(|s| clamp_session(s, now))
            .collect();
        let pets: Vec<Pet> = pets
            .into_iter()
            .filter(|p| match check_pet(p, from) {
                Some(why) => {
                    dropped(from, "pet", why);
                    false
                }
                None => true,
            })
            .map(clean_pet)
            .collect();
        let pending: Vec<PermissionRequest> = pending
            .into_iter()
            .filter(|r| match check_request(r, from) {
                Some(why) => {
                    dropped(from, "permission_request", why);
                    false
                }
                None => true,
            })
            .map(|r| clamp_request(r, now))
            .collect();
        let before: HashMap<RequestId, SessionId> = self
            .mirror()
            .pending
            .values()
            .filter(|r| r.machine_id == from)
            .map(|r| (r.request_id.clone(), r.session_id.clone()))
            .collect();
        let (mid, s2, p2, r2) = (from.to_string(), sessions, pets, pending);
        if let Err(e) = self.inner.store.run(move |s| s.mirror_replace(&mid, &s2, &p2, &r2)).await {
            tracing::warn!(device = %from, "mirror: apply a snapshot: {e:#}");
            return;
        }
        if let Err(e) = self.reload_mirror(from).await {
            tracing::warn!(device = %from, "mirror: reload after a snapshot: {e:#}");
            return;
        }
        let (sessions, pets, pending) = {
            let m = self.mirror();
            let mut sessions: Vec<Session> = m.sessions.values().filter(|s| s.machine_id == from).cloned().collect();
            sessions.sort_by_key(|s| s.last_event_at);
            let pets: Vec<Pet> = m.pets.values().filter(|p| p.machine_id == from).cloned().collect();
            let pending: Vec<PermissionRequest> = m.pending.values().filter(|r| r.machine_id == from).cloned().collect();
            (sessions, pets, pending)
        };
        for session in sessions {
            self.inner.bus.send_local(ServerMsg::Session { session });
        }
        for pet in pets {
            self.inner.bus.send_local(ServerMsg::Pet { pet });
        }
        let now: HashSet<&str> = pending.iter().map(|r| r.request_id.as_str()).collect();
        for (request_id, session_id) in before.into_iter().filter(|(id, _)| !now.contains(id.as_str())) {
            // Resolved on the owner while we were apart: clients drop it.
            self.inner.bus.send_local(ServerMsg::PermissionResolved {
                request_id,
                session_id,
                outcome: PermissionOutcome::Cancelled,
                by: ResolvedBy::System,
                by_machine_id: None,
            });
        }
        for request in pending {
            self.inner.bus.send_local(ServerMsg::PermissionRequest { request });
        }
        self.settle_pet_names().await;
    }
}

/// A mirror update that is over the frame limit, made smaller: a permission request loses its
/// tool input (the rest still tells the user what is waiting). Other updates are left as they are
/// (sending them fails with TooLarge and is logged).
fn shrink_update(body: PeerBody) -> PeerBody {
    match body {
        PeerBody::Mirror { update: MirrorUpdate::PermissionRequest { mut request } } => {
            request.tool_call.raw_input = None;
            PeerBody::Mirror { update: MirrorUpdate::PermissionRequest { request } }
        }
        other => other,
    }
}

/// A `mirror_snapshot` that fits one frame: first pending requests lose their tool input, then the
/// oldest sessions are left out.
fn fit_snapshot(mut sessions: Vec<Session>, pets: Vec<Pet>, mut pending: Vec<PermissionRequest>) -> PeerBody {
    let make = |s: &[Session], r: &[PermissionRequest]| PeerBody::MirrorSnapshot {
        sessions: s.to_vec(),
        pets: pets.clone(),
        pending_permissions: r.to_vec(),
    };
    let mut body = make(&sessions, &pending);
    if body_len(&body) <= MIRROR_FRAME_MAX_BYTES {
        return body;
    }
    for r in pending.iter_mut() {
        r.tool_call.raw_input = None;
    }
    body = make(&sessions, &pending);
    // Sessions are newest first: drop from the end.
    while body_len(&body) > MIRROR_FRAME_MAX_BYTES && !sessions.is_empty() {
        let keep = sessions.len() / 2;
        sessions.truncate(keep);
        body = make(&sessions, &pending);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawl_protocol::{PermissionOption, PermissionOptionKind, Species, ToolCallPayload};

    const PEER: &str = "zyxwvu987654";
    const ME: &str = "abcdef012345";
    const THIRD: &str = "cccccc333333";

    fn session(mid: &str, local: &str) -> Session {
        Session {
            id: global_id(mid, local),
            machine_id: mid.into(),
            agent_id: "codex".into(),
            acp_session_id: None,
            external_id: None,
            cwd: "/r".into(),
            title: None,
            source: SessionSource::Owned,
            status: SessionStatus::Working,
            started_at: 1,
            last_event_at: 1,
            ended_at: None,
            purpose: None,
        }
    }

    fn event(mid: &str, seq: i64, session: &str) -> HubEvent {
        HubEvent {
            id: global_id(mid, seq),
            seq,
            machine_id: mid.into(),
            session_id: session.into(),
            kind: EventType::ToolCall,
            payload: json!({"toolCallId": "t"}),
            ts: 2,
        }
    }

    fn request(mid: &str, rid: &str, sid: &str) -> PermissionRequest {
        PermissionRequest {
            request_id: rid.into(),
            machine_id: mid.into(),
            session_id: sid.into(),
            agent_id: "codex".into(),
            tool_call: ToolCallPayload {
                tool_call_id: "t".into(),
                raw_input: Some(json!({"x": "y".repeat(10)})),
                ..Default::default()
            },
            options: vec![PermissionOption { option_id: "ok".into(), name: "Allow".into(), kind: PermissionOptionKind::AllowOnce }],
            created_at: 3,
            voice_code: None,
        }
    }

    fn pet(mid: &str) -> Pet {
        Pet {
            id: global_id(mid, "pet-codex"),
            machine_id: mid.into(),
            agent_id: "codex".into(),
            species: Species::Dog,
            skin: "default".into(),
            name: "Biscuit".into(),
            personality: String::new(),
            mood: 50,
            xp: 0,
        }
    }

    #[test]
    fn items_must_carry_the_senders_machine_everywhere() {
        let sid = global_id(PEER, "s1");
        assert_eq!(check_session(&session(PEER, "s1"), PEER), None);
        for (s, why) in [(session(ME, "s1"), "own machine"), (session(THIRD, "s1"), "third machine")] {
            assert!(check_session(&s, PEER).is_some(), "{why}");
        }
        let mut spoof = session(PEER, "s1");
        spoof.id = global_id(ME, "s1");
        assert!(check_session(&spoof, PEER).is_some(), "id prefix of this hub");
        let mut long_cwd = session(PEER, "s1");
        long_cwd.cwd = "/".repeat(1025);
        assert!(check_session(&long_cwd, PEER).is_some(), "cwd over 1024 characters");
        let mut ctl_cwd = session(PEER, "s1");
        ctl_cwd.cwd = "/srv/api\u{1b}]0;x\u{7}".into();
        assert!(check_session(&ctl_cwd, PEER).is_some(), "control characters in cwd");

        assert_eq!(check_event(&event(PEER, 7, &sid), PEER), None);
        let mut e = event(PEER, 7, &sid);
        e.id = global_id(PEER, 8);
        assert!(check_event(&e, PEER).is_some(), "id != machine:seq");
        let mut e = event(PEER, 7, &sid);
        e.id = "7".into();
        assert!(check_event(&e, PEER).is_some(), "bare seq id");
        assert!(check_event(&event(PEER, 0, &sid), PEER).is_some(), "seq 0");
        assert!(check_event(&event(PEER, 7, &global_id(ME, "s1")), PEER).is_some(), "session of this hub");
        assert!(check_event(&event(THIRD, 7, &global_id(THIRD, "s1")), PEER).is_some(), "third machine");
        let mut big = event(PEER, 7, &sid);
        big.payload = json!({"text": "x".repeat(MIRROR_PAYLOAD_MAX_BYTES)});
        assert!(check_event(&big, PEER).is_some(), "oversize payload");

        let rid = global_id(PEER, "r1");
        assert_eq!(check_request(&request(PEER, &rid, &sid), PEER), None);
        assert!(check_request(&request(PEER, &global_id(ME, "r1"), &sid), PEER).is_some());
        assert!(check_request(&request(PEER, &rid, &global_id(THIRD, "s")), PEER).is_some());
        assert!(check_request(&request(ME, &rid, &sid), PEER).is_some());
        assert_eq!(check_resolved(&rid, &sid, PEER), None);
        assert!(check_resolved(&global_id(ME, "r1"), &sid, PEER).is_some());

        assert_eq!(check_pet(&pet(PEER), PEER), None);
        assert!(check_pet(&pet(ME), PEER).is_some());
        let mut p = pet(PEER);
        p.id = global_id(THIRD, "pet-codex");
        assert!(check_pet(&p, PEER).is_some());

        let n = notify::make(PEER, NotificationLevel::Info, "t", "b", Some(sid.clone()), None);
        assert_eq!(check_notification(&n, PEER), None);
        let n = notify::make(PEER, NotificationLevel::Info, "t", "b", Some(global_id(ME, "s")), None);
        assert!(check_notification(&n, PEER).is_some());
        let n = notify::make(ME, NotificationLevel::Info, "t", "b", None, None);
        assert!(check_notification(&n, PEER).is_some());
    }

    #[test]
    fn large_payloads_are_truncated_for_peers() {
        let sid = global_id(ME, "s1");
        let small = event(ME, 1, &sid);
        assert_eq!(mirror_event(small.clone()), small);
        let mut big = event(ME, 2, &sid);
        big.payload = json!({"text": "x".repeat(MIRROR_PAYLOAD_MAX_BYTES)});
        let n = payload_len(&big.payload);
        let m = mirror_event(big.clone());
        assert_eq!(m.payload, truncated_payload(n));
        assert_eq!((m.id, m.seq, m.session_id), (big.id, big.seq, big.session_id));
    }

    #[test]
    fn oversize_bodies_are_shrunk_to_fit_a_frame() {
        let sid = global_id(ME, "s1");
        let mut r = request(ME, &global_id(ME, "r1"), &sid);
        r.tool_call.raw_input = Some(json!({"content": "x".repeat(MIRROR_FRAME_MAX_BYTES)}));
        let PeerBody::Mirror { update: MirrorUpdate::PermissionRequest { request } } =
            shrink_update(PeerBody::Mirror { update: MirrorUpdate::PermissionRequest { request: r.clone() } })
        else {
            panic!("shape")
        };
        assert_eq!(request.tool_call.raw_input, None);
        assert_eq!(request.request_id, r.request_id);

        let mut sessions: Vec<Session> = (0..200).map(|i| session(ME, &format!("s{i}"))).collect();
        for s in sessions.iter_mut() {
            s.title = Some("t".repeat(2000)); // 200 x ~2.4 KiB: more than one frame
        }
        let body = fit_snapshot(sessions.clone(), vec![pet(ME)], vec![r]);
        assert!(body_len(&body) <= MIRROR_FRAME_MAX_BYTES);
        let PeerBody::MirrorSnapshot { sessions: kept, pets, pending_permissions } = body else { panic!("shape") };
        assert!(!kept.is_empty() && kept.len() < sessions.len());
        assert_eq!(kept[0].id, sessions[0].id, "the newest sessions are kept");
        assert_eq!(pets.len(), 1);
        assert_eq!(pending_permissions[0].tool_call.raw_input, None);
    }

    #[test]
    fn the_active_machine_is_the_latest_online_one() {
        let now = 10 * USER_ACTIVE_WINDOW_MS;
        let m = |id: &str, online: bool, at: Option<i64>| Machine {
            id: id.into(),
            name: id.into(),
            os: pawl_protocol::MachineOs::Linux,
            last_seen: now,
            online,
            hub_version: None,
            user_active_at: at,
        };
        assert_eq!(active_machine(&[m(ME, true, None), m(PEER, true, None)], ME, now), ME, "nobody active: self");
        assert_eq!(active_machine(&[m(ME, true, Some(now - 5000)), m(PEER, true, Some(now - 1000))], ME, now), PEER);
        assert_eq!(
            active_machine(&[m(ME, true, Some(now - 5000)), m(PEER, false, Some(now - 1000))], ME, now),
            ME,
            "an offline peer is never active"
        );
        let stale = now - USER_ACTIVE_WINDOW_MS - 1;
        assert_eq!(active_machine(&[m(ME, true, None), m(PEER, true, Some(stale))], ME, now), ME, "outside the window");
        assert_eq!(active_machine(&[m(ME, true, None), m(PEER, true, Some(now))], PEER, now), PEER);
    }
}
