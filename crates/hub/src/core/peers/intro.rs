//! Group roster (PLAN step 7): after pairing a device, this hub introduces it to the devices it
//! already pairs with (`roster_intro`), and introduces those to it. Hubs learn every device; phones
//! learn every hub; phones are never introduced to phones. Intros owed to an offline device are
//! kept in the store and sent on its next link.
//!
//! A received intro is applied only when it comes from a paired hub, `[peer] accept_roster_intros`
//! is on, and the device is new here: an intro never replaces a key, never re-adds a revoked id or
//! key, and never adds this hub or the sender. Every outcome is in the pairing audit (never a key
//! or secret) and shown to the user ("Mac mini added iPhone").
//!
//! Also here: the offline wake. When a local permission request appears, every paired phone that
//! is not linked gets one small envelope through its relay mailbox (so the relay sends it a
//! content-free push), at most once per phone per [`PHONE_WAKE_INTERVAL`].

use krypto_peer::{WakeError, MAX_INTRO_DEVICES};
use krypto_protocol::{IntroDevice, MachineOs};

use super::*;
/// At most one offline wake per phone this often.
pub const PHONE_WAKE_INTERVAL: Duration = Duration::from_secs(60);

/// The paired devices that learn about `new` and that `new` learns about: every other hub, plus
/// every phone when `new` is a hub (phones never learn about phones).
pub(crate) fn intro_group(new_id: &str, new_kind: DeviceKind, roster: &[Device]) -> Vec<String> {
    roster
        .iter()
        .filter(|d| d.id != new_id)
        .filter(|d| d.kind == DeviceKind::Hub || new_kind == DeviceKind::Hub)
        .map(|d| d.id.clone())
        .collect()
}

/// May `recipient` be told about `about`? Phones only learn hubs.
fn may_learn(recipient: DeviceKind, about: DeviceKind) -> bool {
    recipient == DeviceKind::Hub || about == DeviceKind::Hub
}

/// A roster entry as it goes out in `roster_intro`. Phones have no LAN address, and a hub's address
/// goes along only when receivers would take it (`peer_addr_ok`: a private / LAN IP literal).
pub(crate) fn intro_device(e: &RosterEntry) -> IntroDevice {
    let os = match (e.machine.os, e.device.os) {
        (MachineOs::Other, Some(os)) => Some(os),
        (os, _) => Some(os),
    };
    IntroDevice {
        id: e.device.id.clone(),
        kind: e.device.kind,
        name: e.device.name.clone(),
        public_key: e.device.public_key.clone(),
        addr: e.device.addr.clone().filter(|a| e.device.kind == DeviceKind::Hub && krypto_peer::peer_addr_ok(a)),
        os,
    }
}

/// What to do with one device of a received intro.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IntroOutcome {
    /// New here: add it (`public_key` canonical base64).
    Add { public_key: String },
    /// Already paired with this id and key: nothing to do.
    Known,
    /// Clashes with what we know (another key for the id, the key under another id, revoked here).
    Conflict(&'static str),
    /// Malformed, a repeat within the intro, this hub or the sender.
    Invalid(&'static str),
}

/// What this hub knows about a key or id, for judging intros.
pub(crate) struct Stored<'a> {
    pub entry: &'a RosterEntry,
    pub revoked: bool,
}

/// Decoded public key bytes (None if the stored string is not a 32-byte base64 key).
fn key_bytes(b64: &str) -> Option<[u8; 32]> {
    krypto_peer::b64::decode(b64).ok().and_then(|v| <[u8; 32]>::try_from(v.as_slice()).ok())
}

/// Judge each device of an intro from `sender` (already known to be a paired hub). `structural`
/// is the link layer's check of one entry (`krypto_peer::check_intro_device`: id, name, addr, key
/// of 32 bytes and not low-order); it returns the key bytes. Everything that needs this hub's
/// roster is checked here: never us, never the sender, never a replacement, never a revoked id or
/// key.
pub(crate) fn judge_intro(
    devices: &[IntroDevice],
    me: (&str, &[u8; 32]),
    sender: &Device,
    stored: &[Stored<'_>],
    structural: impl Fn(&IntroDevice) -> std::result::Result<[u8; 32], &'static str>,
) -> Vec<IntroOutcome> {
    let sender_key = key_bytes(&sender.public_key);
    let mut seen_ids: HashSet<&str> = HashSet::new();
    let mut seen_keys: HashSet<[u8; 32]> = HashSet::new();
    devices
        .iter()
        .map(|d| {
            let key = match structural(d) {
                Ok(k) => k,
                Err(why) => return IntroOutcome::Invalid(why),
            };
            if !seen_ids.insert(d.id.as_str()) || !seen_keys.insert(key) {
                return IntroOutcome::Invalid("listed twice");
            }
            if d.id == me.0 || &key == me.1 {
                return IntroOutcome::Invalid("is this machine");
            }
            if d.id == sender.id || Some(key) == sender_key {
                return IntroOutcome::Invalid("is the sender");
            }
            if let Some(s) = stored.iter().find(|s| s.entry.device.id == d.id) {
                return if s.revoked {
                    IntroOutcome::Conflict("revoked here")
                } else if key_bytes(&s.entry.device.public_key) != Some(key) {
                    IntroOutcome::Conflict("known here with another key")
                } else if s.entry.device.kind != d.kind {
                    IntroOutcome::Conflict("known here as another kind of device")
                } else {
                    IntroOutcome::Known
                };
            }
            if let Some(s) = stored.iter().find(|s| key_bytes(&s.entry.device.public_key) == Some(key)) {
                return IntroOutcome::Conflict(if s.revoked { "its key was revoked here" } else { "its key belongs to another device here" });
            }
            IntroOutcome::Add { public_key: krypto_peer::b64::encode(&key) }
        })
        .collect()
}

/// `roster_intro`s one paired hub may send: a token bucket of INTRO_BURST, one token back per
/// INTRO_REFILL.
pub const INTRO_BURST: u32 = 3;
pub const INTRO_REFILL: Duration = Duration::from_secs(10);
/// Dropped (rate-limited) intros from one hub are audited at most once per this window.
pub const INTRO_DROP_NOTE_EVERY: Duration = Duration::from_secs(60);
/// Intros never grow the stored roster (paired and revoked rows) past this.
pub const MAX_ROSTER_DEVICES: usize = 64;
/// Most devices (paired and revoked rows) one hub may have introduced here.
pub const MAX_INTRODUCED_PER_HUB: usize = 32;
/// Entries named per group in a not-added audit detail.
const DETAIL_NAMES: usize = 8;

/// One sender's intro budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IntroBucket {
    tokens: u32,
    refilled_at: tokio::time::Instant,
    /// Dropped since the last audit note.
    dropped: u32,
    noted_at: Option<tokio::time::Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntroAdmit {
    Admit,
    /// Over budget. `log`: Some(n) = audit this drop now (n more were dropped since the last note).
    Drop { log: Option<u32> },
}

/// Spend one of `from`'s intro tokens (see [`INTRO_BURST`]).
pub(crate) fn take_intro(buckets: &mut HashMap<String, IntroBucket>, from: &str, now: tokio::time::Instant) -> IntroAdmit {
    let b = buckets
        .entry(from.to_string())
        .or_insert(IntroBucket { tokens: INTRO_BURST, refilled_at: now, dropped: 0, noted_at: None });
    let back = (now.saturating_duration_since(b.refilled_at).as_millis() / INTRO_REFILL.as_millis()) as u32;
    if back > 0 {
        b.tokens = (b.tokens.saturating_add(back)).min(INTRO_BURST);
        b.refilled_at = if b.tokens == INTRO_BURST { now } else { b.refilled_at + INTRO_REFILL * back };
    }
    if b.tokens > 0 {
        b.tokens -= 1;
        return IntroAdmit::Admit;
    }
    match b.noted_at {
        Some(t) if now.saturating_duration_since(t) < INTRO_DROP_NOTE_EVERY => {
            b.dropped = b.dropped.saturating_add(1);
            IntroAdmit::Drop { log: None }
        }
        _ => {
            let n = b.dropped;
            b.dropped = 0;
            b.noted_at = Some(now);
            IntroAdmit::Drop { log: Some(n) }
        }
    }
}

/// Why an introduced device was not added.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkipWhy {
    Conflict,
    Malformed,
    Full,
    Off,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Skipped {
    pub id: String,
    pub kind: DeviceKind,
    pub name: String,
    pub why: SkipWhy,
    pub reason: &'static str,
}

/// The one audit detail and notification for everything an intro did not add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkipReport {
    pub detail: String,
    pub level: NotificationLevel,
    pub title: String,
    pub body: String,
}

pub(crate) fn skip_report(by: &str, skipped: &[Skipped]) -> SkipReport {
    let of = |w: SkipWhy| skipped.iter().filter(|x| x.why == w).collect::<Vec<_>>();
    let names = |v: &[&Skipped]| name_list(&v.iter().map(|x| x.name.clone()).collect::<Vec<_>>());
    let mut detail = Vec::new();
    let mut body = Vec::new();
    for (w, label) in [(SkipWhy::Conflict, "conflict"), (SkipWhy::Malformed, "malformed"), (SkipWhy::Full, "full"), (SkipWhy::Off, "off")] {
        let v = of(w);
        if v.is_empty() {
            continue;
        }
        let mut items: Vec<String> = v.iter().take(DETAIL_NAMES).map(|x| format!("{} {} ({})", x.name, x.id, x.reason)).collect();
        if v.len() > DETAIL_NAMES {
            items.push(format!("and {} more", v.len() - DETAIL_NAMES));
        }
        detail.push(format!("{label}: {}", items.join(", ")));
        body.push(match w {
            SkipWhy::Conflict => format!(
                "{} clash{} with devices this machine knows (another key, or revoked here); pair directly if you trust {}.",
                names(&v),
                if v.len() == 1 { "es" } else { "" },
                if v.len() == 1 { "it" } else { "them" }
            ),
            SkipWhy::Malformed => format!("{} malformed entr{} ignored.", v.len(), if v.len() == 1 { "y" } else { "ies" }),
            SkipWhy::Full => format!(
                "{} not added: this machine keeps at most {MAX_ROSTER_DEVICES} devices, {MAX_INTRODUCED_PER_HUB} from one machine; revoke devices you no longer use.",
                names(&v)
            ),
            SkipWhy::Off => format!("{} not added: adding devices that paired machines introduce is off in Settings.", names(&v)),
        });
    }
    let all: Vec<&Skipped> = skipped.iter().collect();
    let only_off = skipped.iter().all(|x| x.why == SkipWhy::Off);
    SkipReport {
        detail: detail.join("; "),
        level: if only_off { NotificationLevel::Info } else { NotificationLevel::Warning },
        title: if only_off { format!("{by} introduced {}", names(&all)) } else { format!("{by} introduced {}: not added", names(&all)) },
        body: body.join(" "),
    }
}

/// "iPhone", "iPhone and Laptop", "iPhone, Laptop and 2 more".
pub(crate) fn name_list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [a] => a.clone(),
        [a, b] => format!("{a} and {b}"),
        [a, b, rest @ ..] => format!("{a}, {b} and {} more", rest.len()),
    }
}

/// One offline wake per phone per [`PHONE_WAKE_INTERVAL`]: true (and noted) if `id` may be woken now.
pub(crate) fn take_wake(wakes: &mut HashMap<String, tokio::time::Instant>, id: &str, now: tokio::time::Instant) -> bool {
    match wakes.get(id) {
        Some(last) if now.saturating_duration_since(*last) < PHONE_WAKE_INTERVAL => false,
        _ => {
            wakes.insert(id.to_string(), now);
            true
        }
    }
}

impl Hub {
    // ------------------------------------------------------------ sending intros

    /// After pairing `new_id`: owe it an intro of its group and owe each of them an intro of it
    /// (see [`intro_group`]), then send what can go now. Offline devices get theirs on LinkUp.
    pub(super) async fn introduce_new_device(&self, new_id: &str) {
        let (new_kind, group) = {
            let peers = self.peers();
            let Some(new) = peers.roster.get(new_id) else { return };
            let devices: Vec<Device> = peers.roster.values().map(|e| e.device.clone()).collect();
            (new.device.kind, intro_group(new_id, new.device.kind, &devices))
        };
        if group.is_empty() {
            return;
        }
        let (new, list) = (new_id.to_string(), group.clone());
        let now = krypto_protocol::now_ms();
        let queued = self
            .inner
            .store
            .run(move |s| {
                for id in &list {
                    s.queue_roster_intros(id, std::slice::from_ref(&new), now)?;
                }
                s.queue_roster_intros(&new, &list, now)
            })
            .await;
        if let Err(e) = queued {
            tracing::warn!("queue roster intros for {new_id}: {e:#}");
            return;
        }
        tracing::info!(
            "introducing {new_id} ({}) to {} paired device(s) and them to it",
            if new_kind == DeviceKind::Hub { "hub" } else { "phone" },
            group.len()
        );
        let online: HashSet<String> = self.inner.peer.online().into_iter().map(|(id, _)| id).collect();
        for id in std::iter::once(new_id.to_string()).chain(group) {
            if online.contains(&id) {
                self.flush_intros(&id).await;
            }
        }
    }

    /// Send `recipient` the intros it is owed (on every LinkUp, and right after a pairing). Entries
    /// about devices no longer paired, or that it may not learn, are dropped.
    pub(super) async fn flush_intros(&self, recipient: &str) {
        let id = recipient.to_string();
        let pending = match self.inner.store.run(move |s| s.pending_roster_intros(&id)).await {
            Ok(p) => p,
            Err(e) => return tracing::warn!("read the roster intros owed to {recipient}: {e:#}"),
        };
        if pending.is_empty() {
            return;
        }
        let (send, stale): (Vec<IntroDevice>, Vec<String>) = {
            let peers = self.peers();
            let Some(to) = peers.roster.get(recipient).map(|e| e.device.kind) else { return };
            let mut send = Vec::new();
            let mut stale = Vec::new();
            for about in &pending {
                match peers.roster.get(about) {
                    Some(e) if about != recipient && may_learn(to, e.device.kind) => send.push(intro_device(e)),
                    _ => stale.push(about.clone()),
                }
            }
            (send, stale)
        };
        let mut done = stale;
        for chunk in send.chunks(MAX_INTRO_DEVICES) {
            match self.inner.peer.send_roster_intro(recipient, chunk.to_vec()) {
                Ok(()) => done.extend(chunk.iter().map(|d| d.id.clone())),
                Err(krypto_peer::SendError::NotLinked) => break,
                Err(e) => {
                    tracing::warn!(device = %recipient, "roster_intro not sent: {e}");
                    break;
                }
            }
        }
        let sent = done.len();
        if sent > 0 {
            let id = recipient.to_string();
            if let Err(e) = self.inner.store.run(move |s| s.clear_roster_intros(&id, &done)).await {
                tracing::warn!("clear the roster intros sent to {recipient}: {e:#}");
            }
            tracing::info!(device = %recipient, "sent a roster intro ({} device(s))", send.len().min(sent));
        }
    }

    // ------------------------------------------------------------ receiving intros

    /// A `roster_intro` from `from` (`PeerEvent::RosterIntro`: the node checked that it came from a
    /// linked paired hub and split it into entries that pass `check_intro_device` and `invalid`).
    /// Rate-limited per sender ([`take_intro`]); the roster never grows past [`MAX_ROSTER_DEVICES`]
    /// rows, nor [`MAX_INTRODUCED_PER_HUB`] from one introducer. Added devices get one audit row each;
    /// everything not added is one audit row and one notification per intro.
    pub(super) async fn on_roster_intro(&self, from: &str, devices: Vec<IntroDevice>, invalid: Vec<(IntroDevice, &'static str)>) {
        let admit = {
            let mut guard = self.peers();
            let peers = &mut *guard;
            peers.intro_buckets.retain(|id, _| peers.roster.contains_key(id) || id == from);
            take_intro(&mut peers.intro_buckets, from, tokio::time::Instant::now())
        };
        if let IntroAdmit::Drop { log } = admit {
            if let Some(skipped) = log {
                tracing::warn!(device = %from, "dropping roster_intros: more than {INTRO_BURST} in {} s", INTRO_REFILL.as_secs() * u64::from(INTRO_BURST));
                let detail = format!("from {from}: rate limited ({} more dropped since the last note)", skipped);
                self.audit_pairing(PairingAuditKind::IntroIgnored, None, None, "peer", Some(detail)).await;
            }
            return;
        }
        let sender = self.peers().roster.get(from).map(|e| e.device.clone());
        let Some(sender) = sender.filter(|d| d.kind == DeviceKind::Hub) else {
            tracing::warn!(device = %from, "ignoring a roster_intro from a device that is not a paired hub");
            self.audit_pairing(PairingAuditKind::IntroIgnored, None, None, "peer", Some(format!("from {from}: not a paired hub")))
                .await;
            return;
        };
        if devices.is_empty() && invalid.is_empty() {
            return;
        }
        if devices.len() + invalid.len() > MAX_INTRO_DEVICES {
            tracing::warn!(device = %from, "ignoring a roster_intro with {} devices (at most {MAX_INTRO_DEVICES})", devices.len() + invalid.len());
            let detail = format!("from {}: {} devices, at most {MAX_INTRO_DEVICES}", sender.name, devices.len() + invalid.len());
            self.audit_pairing(PairingAuditKind::IntroIgnored, Some(&sender), None, "peer", Some(detail)).await;
            return;
        }
        let stored = match self.inner.store.run(|s| s.roster_with_revoked()).await {
            Ok(v) => v,
            Err(e) => return tracing::warn!("read the roster for a roster_intro: {e:#}"),
        };
        let my_key = key_bytes(&self.inner.peer.public_b64()).unwrap_or([0; 32]);
        let known: Vec<Stored<'_>> = stored.iter().map(|(entry, revoked)| Stored { entry, revoked: *revoked }).collect();
        let mut outcomes = judge_intro(&devices, (&self.inner.machine_id, &my_key), &sender, &known, krypto_peer::check_intro_device);
        let mut devices = devices;
        for (d, why) in invalid {
            devices.push(d);
            outcomes.push(IntroOutcome::Invalid(why));
        }
        let mut total = stored.len();
        let mut by_sender = stored.iter().filter(|(e, _)| e.device.introduced_by.as_deref() == Some(from)).count();
        drop(known);
        let accept = self.cfg().peer.accept_roster_intros;
        let mut added: Vec<String> = Vec::new();
        let mut skipped: Vec<Skipped> = Vec::new();
        let introducer = format!("introduced by {} ({})", sender.name, sender.id);
        for (d, outcome) in devices.iter().zip(outcomes) {
            let name = crate::store::sanitize_machine_name(&d.name);
            let id = if is_valid_machine_id(&d.id) { d.id.clone() } else { "(invalid id)".into() };
            let skip = |why: SkipWhy, reason: &'static str| Skipped { id: id.clone(), kind: d.kind, name: name.clone(), why, reason };
            match outcome {
                IntroOutcome::Known => {}
                IntroOutcome::Invalid(why) => skipped.push(skip(SkipWhy::Malformed, why)),
                IntroOutcome::Conflict(why) => skipped.push(skip(SkipWhy::Conflict, why)),
                IntroOutcome::Add { .. } if !accept => skipped.push(skip(SkipWhy::Off, "accept_roster_intros is off")),
                IntroOutcome::Add { .. } if total >= MAX_ROSTER_DEVICES => skipped.push(skip(SkipWhy::Full, "the roster is full")),
                IntroOutcome::Add { .. } if by_sender >= MAX_INTRODUCED_PER_HUB => {
                    skipped.push(skip(SkipWhy::Full, "this machine introduced too many devices"))
                }
                IntroOutcome::Add { public_key } => {
                    let now = krypto_protocol::now_ms();
                    let addr = d.addr.clone().filter(|_| d.kind == DeviceKind::Hub);
                    let entry = RosterEntry {
                        device: Device {
                            id: d.id.clone(),
                            kind: d.kind,
                            name: name.clone(),
                            public_key,
                            paired_at: now,
                            last_seen: None,
                            addr: addr.clone(),
                            path: None,
                            introduced_by: Some(sender.id.clone()),
                            os: d.os,
                        },
                        machine: Machine {
                            id: d.id.clone(),
                            name: name.clone(),
                            os: d.os.unwrap_or(MachineOs::Other),
                            last_seen: now,
                            online: false,
                            hub_version: None,
                            user_active_at: None,
                        },
                    };
                    let e = entry.clone();
                    match self.inner.store.run(move |s| s.add_introduced_device(&e)).await {
                        Ok(true) => {
                            total += 1;
                            by_sender += 1;
                            tracing::info!("{} introduced {:?} ({}); added", sender.name, name, d.id);
                            self.audit_pairing(PairingAuditKind::Introduced, Some(&entry.device), addr, "peer", Some(introducer.clone()))
                                .await;
                            added.push(name);
                        }
                        // Stored meanwhile (a pairing raced the intro): never replaced.
                        Ok(false) => skipped.push(skip(SkipWhy::Conflict, "already stored")),
                        Err(e) => tracing::warn!("save the introduced device {}: {e:#}", d.id),
                    }
                }
            }
        }
        let by = &sender.name;
        if !added.is_empty() {
            let names = name_list(&added);
            self.notify_intro(NotificationLevel::Success, format!("{by} added {names}"),
                format!("{names} can now reach this machine ({by} introduced {}).", if added.len() == 1 { "it" } else { "them" }));
            // The link layer lets the new devices in (relay allow list) and dials new hubs.
            self.inner.peer.roster_changed();
        }
        if !skipped.is_empty() {
            tracing::warn!(device = %from, "roster_intro: {} of {} entries not added", skipped.len(), devices.len());
            let report = skip_report(by, &skipped);
            let audit_device = match skipped.as_slice() {
                [one] => Some(Device {
                    id: one.id.clone(),
                    kind: one.kind,
                    name: one.name.clone(),
                    public_key: String::new(),
                    paired_at: 0,
                    last_seen: None,
                    addr: None,
                    path: None,
                    introduced_by: Some(sender.id.clone()),
                    os: None,
                }),
                _ => None,
            };
            let kind = if skipped.iter().any(|x| x.why == SkipWhy::Conflict) { PairingAuditKind::IntroConflict } else { PairingAuditKind::IntroIgnored };
            self.audit_pairing(kind, audit_device.as_ref(), None, "peer", Some(format!("{introducer}: {}", report.detail))).await;
            self.notify_intro(report.level, report.title, report.body);
        }
        self.sync_peers(None).await;
        if added.is_empty() && !skipped.is_empty() {
            // Nothing changed in the roster, but clients still hear about it with the notification.
            self.inner.bus.send(ServerMsg::Devices { devices: self.devices() });
        }
    }

    fn notify_intro(&self, level: NotificationLevel, title: String, body: String) {
        let notification = notify::make(&self.inner.machine_id, level, title, body, None, None);
        self.inner.bus.send_local(ServerMsg::Notification { notification });
    }

    // ------------------------------------------------------------ offline phone wake

    /// A new local permission request: wake every paired phone that is not linked (once per phone
    /// per [`PHONE_WAKE_INTERVAL`]) so it reconnects and sees the request.
    pub(crate) fn wake_offline_phones(&self) {
        let online: HashSet<String> = self.inner.peer.online().into_iter().map(|(id, _)| id).collect();
        let now = tokio::time::Instant::now();
        let due: Vec<String> = {
            let mut guard = self.peers();
            let peers = &mut *guard;
            let phones: Vec<String> = peers
                .roster
                .values()
                .filter(|e| e.device.kind == DeviceKind::Phone && !online.contains(&e.device.id))
                .map(|e| e.device.id.clone())
                .collect();
            peers.wakes.retain(|id, _| peers.roster.contains_key(id));
            phones.into_iter().filter(|id| take_wake(&mut peers.wakes, id, now)).collect()
        };
        for id in due {
            match self.inner.peer.wake_phone(&id) {
                Ok(()) => tracing::debug!(device = %id, "woke an offline phone"),
                // Linked meanwhile (it gets the request normally), the relay is down, or we closed its
                // link for a while (flood cooldown).
                Err(e @ (WakeError::Linked | WakeError::RelayOff | WakeError::RateLimited | WakeError::Disconnected)) => {
                    tracing::debug!(device = %id, "phone not woken: {e}")
                }
                Err(e) => {
                    // Nothing was queued (backpressure): the next request may try again.
                    self.peers().wakes.remove(&id);
                    tracing::warn!(device = %id, "phone not woken: {e}");
                }
            }
        }
    }
}

#[cfg(test)]
impl Hub {
    /// Tests: when each phone was last woken.
    pub(crate) fn phone_wakes(&self) -> HashMap<String, tokio::time::Instant> {
        self.peers().wakes.clone()
    }

    /// Tests: give every sender a full intro budget again.
    pub(crate) fn reset_intro_budget(&self) {
        self.peers().intro_buckets.clear();
    }
}

#[cfg(test)]
mod tests;
