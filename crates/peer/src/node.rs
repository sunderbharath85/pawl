//! The peer node: listener, dialer, relay client, pairing and live links.
//!
//! A link is logical, one per paired device, on one of two paths: its own LAN WebSocket (dialed by
//! either side), or the shared relay connection (`node::relay`). LAN is preferred: a LAN link that
//! comes up replaces a relay link, and a LAN link that is lost falls back to the relay (no `LinkDown`)
//! when the relay is up. Every path change is reported as `LinkUp` (resync) followed by `LinkPath`.
//!
//! Lock order (never take an earlier one while holding a later one): `gate` -> roster (the hub's own
//! storage lock) -> `links`/`dialers`/`invites`/`relay`/the relay bookkeeping. Roster methods are never
//! called while one of the later locks is held, so the hub may call any `PeerNode` method while holding
//! its storage lock, except `revoke`, `disconnect`, `join`, `connect_known`, `set_relay` and `shutdown`,
//! which call into the roster.

use crate::envelope::{parse_pk, seal_plaintext};
use crate::invite::{new_code, peer_addr_ok, valid_addr, CODE_LEN};
use crate::io::{spawn_io, SendFail, WsIo, WsTx, MAX_FRAME};
use crate::keys::wipe;
use crate::limits::{ConnLimits, PairKey, PairLimiter, PeerConn, PeerListener};
use crate::relay::{address, RelayConfig, RelayConfigError};
use crate::{b64, open, seal, DeviceKeys, Invite, InviteError, Roster, RosterEntry};
use anyhow::{bail, Context};
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use krypto_protocol::{
    is_valid_machine_id, now_ms, ByeReason, Device, DeviceKind, LinkPath, Machine, PairRejectReason, PeerBody,
    PeerEnvelope, Presence, RelayClientFrame, RelayStatusInfo, UnixMs, MIRROR_FRAME_MAX_BYTES, PEER_PATH, PRESENCE_INTERVAL_MS, PRESENCE_OFFLINE_MS,
};
use relay::{PendingJoin, RelayConn, RelayState};
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use subtle::ConstantTimeEq;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot, watch, Notify};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Instant};
use tracing::{debug, info, warn};

mod relay;

/// Whole `join` (dial + pair_request + reply).
const JOIN_TIMEOUT: Duration = Duration::from_secs(10);
/// `join` tries the invite's LAN addr this long before falling back to the relay.
const JOIN_LAN_TIMEOUT: Duration = Duration::from_secs(3);
/// A pair_request that waited in our relay mailbox longer than JOIN_TIMEOUT plus this (clock skew)
/// is refused: its joiner gave up long ago, so pairing now would be one-sided.
const STALE_PAIR_SKEW: Duration = Duration::from_secs(60);
/// Live invites kept at once (the oldest is dropped beyond this).
const MAX_INVITES: usize = 16;
/// Live invites holding a relay pairing slot (the relay keeps 4 and evicts the oldest; so do we).
const MAX_RELAY_INVITES: usize = 4;
const INVITE_TTL_MIN: Duration = Duration::from_secs(60);
const INVITE_TTL_MAX: Duration = Duration::from_secs(3600);
/// Bodies queued per link for `send` (frames waiting for the socket). Overflow closes the link.
const LINK_QUEUE: usize = 1024;
/// Serialized body bytes queued per link for `send`. Exceeding it is handled like LINK_QUEUE overflow.
const LINK_QUEUE_BYTES: usize = 16 * 1024 * 1024;
/// Control commands (presence push, close) queued per link.
const CTL_QUEUE: usize = 16;
/// Relay frames waiting for one link's task: a relay queue flush is at most 256 frames (and the task
/// only decrypts and emits, so it keeps up). Bounds the memory one peer can pin to ~80 MiB.
const RELAY_IN_QUEUE: usize = 256;
/// Largest serialized body `PeerNode::send` accepts: MIRROR_FRAME_MAX_BYTES plus 16 KiB headroom.
pub const MAX_SEND_BODY_BYTES: usize = MIRROR_FRAME_MAX_BYTES + 16 * 1024;
/// Hello / presence stamped further than this from our clock are dropped.
const TS_SKEW_MAX: Duration = Duration::from_secs(3600);
/// A revoked device that comes back is told so (`bye{revoked}`) at most this often.
const REVOKED_ANSWER_EVERY: Duration = Duration::from_secs(60);
/// Revoked devices remembered for that rate limit.
const MAX_REVOKED_ANSWERS: usize = 1024;
/// How long `revoke`/`shutdown` wait for a link to flush its `bye`.
const CLOSE_WAIT: Duration = Duration::from_secs(2);

/// What the node reports to the hub.
#[derive(Debug, Clone, PartialEq)]
pub enum PeerEvent {
    /// A pairing completed (`we_invited`: we were the inviter). Followed by `LinkUp`
    /// (also when re-pairing over a live link).
    Paired { device: Device, machine: Machine, we_invited: bool },
    /// We rejected a pairing attempt (audit it). `remote_addr` is the socket's peer address.
    PairRejected { remote_addr: String, reason: PairRejectReason },
    /// A link to the device became live: the device was offline, a new link replaced the current
    /// one (re-pair, reconnect, duplicate-link tie-break), or the link changed path (LAN takeover,
    /// relay fallback, a peer that re-dialed over the relay). Every `Body` is preceded by the `LinkUp`
    /// of the link it arrived on, so (re)sync on every `LinkUp`; it is idempotent for an online device.
    /// `LinkPath` follows every `LinkUp`.
    LinkUp { device_id: String, machine: Machine },
    /// The device's last live link closed (socket closed, bye, offline timeout, revoke, shutdown).
    LinkDown { device_id: String },
    /// An authenticated presence heartbeat.
    Presence { device_id: String, presence: Presence },
    /// Any other authenticated body (mirror, sync_request, mirror_snapshot, mirror_events, sync_done,
    /// and later steps' bodies), in arrival order per link. Pair, hello, presence and bye never come here.
    Body { device_id: String, body: PeerBody },
    /// The device was revoked, here (`by_peer: false`) or by the peer's `bye{revoked}`. Comes after
    /// the `LinkDown` of its link, if it had one.
    Revoked { device_id: String, by_peer: bool },
    /// The path the device's live link uses; right after every `LinkUp`.
    LinkPath { device_id: String, path: LinkPath },
    /// The relay connection changed (url, connected or last_error; codes in `RelayStatusInfo` docs).
    RelayStatus(RelayStatusInfo),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerTimings {
    pub presence_interval: Duration,
    pub offline_after: Duration,
    pub handshake_timeout: Duration,
    pub backoff_min: Duration,
    pub backoff_max: Duration,
}

impl Default for PeerTimings {
    fn default() -> Self {
        PeerTimings {
            presence_interval: Duration::from_millis(PRESENCE_INTERVAL_MS as u64),
            offline_after: Duration::from_millis(PRESENCE_OFFLINE_MS as u64),
            handshake_timeout: Duration::from_secs(5),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
        }
    }
}

/// `PeerNode::send` errors. `Display` is a snake_case code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// No live link to that device.
    #[error("not_linked")]
    NotLinked,
    /// The serialized body is over `MAX_SEND_BODY_BYTES`; nothing was sent.
    #[error("too_large")]
    TooLarge,
    /// The link's outbound queue was full (LINK_QUEUE bodies or LINK_QUEUE_BYTES bytes): the link is
    /// being closed (the peer resyncs on reconnect).
    #[error("backpressure")]
    Backpressure,
    /// pair_request / pair_accept / pair_reject / hello / presence / bye belong to the node itself
    /// (presence goes through `set_presence`); `send` refuses them.
    #[error("reserved")]
    Reserved,
}

/// `join` errors. `Display` is the wire error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum JoinError {
    #[error("bad_uri")]
    BadUri,
    #[error("self_invite")]
    SelfInvite,
    #[error("unreachable")]
    Unreachable,
    #[error("invite_rejected")]
    Rejected,
    #[error("timeout")]
    Timeout,
}

/// A completed `PeerNode::join_invite`.
#[derive(Debug, Clone, PartialEq)]
pub struct Joined {
    pub device: Device,
    pub machine: Machine,
    /// Set when the join turned on the invite's relay because none was configured. The node keeps
    /// using it until `set_relay` changes it: store it as the relay setting, or turn it off.
    pub adopted_relay: Option<RelayConfig>,
}

/// Hub-to-hub peer node. Cheap to clone (shared state). Methods that start background work
/// (`listen`, `join`, `connect_known`) must run inside a Tokio runtime.
#[derive(Clone)]
pub struct PeerNode {
    inner: Arc<Inner>,
}

struct Inner {
    keys: DeviceKeys,
    me: Mutex<Machine>,
    roster: Arc<dyn Roster>,
    events: UnboundedSender<PeerEvent>,
    timings: Mutex<PeerTimings>,
    presence: Mutex<Option<Presence>>,
    invites: Mutex<Vec<LiveInvite>>,
    /// Last explicit invite address; advertised in hello when the listener binds 0.0.0.0 / ::.
    advertise: Mutex<Option<String>>,
    listener: Mutex<Option<ListenerState>>,
    links: Mutex<HashMap<String, LinkEntry>>,
    /// Bumped on every link added, removed or moved to another path; dial loops wait on it.
    links_changed: watch::Sender<u64>,
    dialers: Mutex<HashMap<String, JoinHandle<()>>>,
    /// Connection caps and the unauthenticated deadline of the listener.
    conn_limits: Arc<ConnLimits>,
    /// Failed pair attempts per remote IP (or relay sender), and `PairRejected` coalescing.
    pair_limiter: Mutex<PairLimiter>,
    /// Devices the hub disconnected: until the instant, no dialing and no hello accepted.
    cooldowns: Mutex<HashMap<String, Instant>>,
    /// When we last told a revoked device so (`answer_revoked`).
    revoked_answers: Mutex<HashMap<String, Instant>>,
    /// The relay client: URL, status and live session (see `node::relay`).
    relay: Mutex<RelayState>,
    /// Bumped whenever a relay session starts or ends, or the relay refuses a frame; dial loops wait on it.
    relay_changed: watch::Sender<u64>,
    /// Devices we said hello to over the current relay session and have no link with yet.
    relay_pending: Mutex<HashSet<String>>,
    /// Devices whose frame the relay refused (`not_allowed`): the dial loop retries its hello with backoff.
    relay_refused: Mutex<HashSet<String>>,
    /// Joins waiting for the inviter's reply over the relay, by inviter machine id.
    relay_joins: Mutex<HashMap<String, PendingJoin>>,
    /// Serializes "is this device still paired?" against revocation (see module docs).
    gate: Mutex<()>,
    seq_lock: Mutex<()>,
    rx_lock: Mutex<()>,
    next_link_id: AtomicU64,
    next_relay_session: AtomicU64,
    /// Envelopes opened (box decryptions), to prove the cheap checks run first.
    #[cfg(test)]
    opens: AtomicU64,
    closing: AtomicBool,
    shutdown_tx: watch::Sender<bool>,
}

/// An invite code held in memory only. Wiped on drop.
struct LiveInvite {
    code: [u8; CODE_LEN],
    expires_at: UnixMs,
    /// The LAN addr in the URI (sent back in pair_accept), if it has one.
    addr: Option<String>,
    /// The relay pairing slot in the URI, if it has one.
    relay: Option<RelaySlot>,
}

struct RelaySlot {
    url: String,
    token: [u8; CODE_LEN],
}

impl Drop for LiveInvite {
    fn drop(&mut self) {
        wipe(&mut self.code);
        if let Some(slot) = self.relay.as_mut() {
            wipe(&mut slot.token);
        }
    }
}

struct ListenerState {
    addr: SocketAddr,
    task: JoinHandle<()>,
    /// Stops this listener (gracefully) without shutting the node down.
    stop: watch::Sender<bool>,
}

struct LinkEntry {
    id: u64,
    /// The path in use (the link task updates it when the link falls back to the relay).
    path: LinkPath,
    /// Of the LAN socket: did we dial it (tie-break). False on the relay path.
    dialed_by_me: bool,
    ctl: mpsc::Sender<LinkCmd>,
    /// Serialized bodies from `send`.
    data: mpsc::Sender<Vec<u8>>,
    /// Close now, without bye (outbound queue overflow).
    kill: Arc<Notify>,
    /// Serialized bytes of the bodies waiting in `data`.
    queued_bytes: Arc<AtomicUsize>,
    last_auth: Arc<AtomicI64>,
    /// Frames for this device from the relay (sender already checked against the roster key).
    relay_in: mpsc::Sender<PeerEnvelope>,
}

enum LinkCmd {
    Presence,
    /// Close the link, after sending `bye` if given. `done` fires after the link is cleaned up
    /// (its `LinkDown` emitted).
    Close { bye: Option<ByeReason>, done: Option<oneshot::Sender<()>> },
    /// Another link to the same device won (tie-break or re-pair). Stop using this one, let the
    /// peer register the winner, then close quietly (no `LinkDown`: the device is still linked).
    Superseded,
    /// Close the LAN socket as if the network dropped it (tests): fall back to the relay if it is up.
    #[cfg(test)]
    DropLan,
}

/// A handshake that succeeded, ready to become a live link.
struct LinkSetup {
    device_id: String,
    peer_pk: [u8; 32],
    path: LinkPath,
    dialed_by_me: bool,
    /// The peer's machine as it introduced itself (online).
    machine: Machine,
    /// Pairing links replace any existing link unconditionally (the key may have changed).
    force: bool,
    /// Sent first on the link once it wins registration (pair_accept / hello reply).
    reply: Option<PeerBody>,
}

/// A registered link.
struct Link {
    id: u64,
    device_id: String,
    peer_pk: [u8; 32],
    /// The device's relay address (its public key, base64url).
    peer_addr: String,
    ctl_rx: mpsc::Receiver<LinkCmd>,
    data_rx: mpsc::Receiver<Vec<u8>>,
    relay_rx: mpsc::Receiver<PeerEnvelope>,
    kill: Arc<Notify>,
    queued_bytes: Arc<AtomicUsize>,
    last_auth: Arc<AtomicI64>,
    reply: Option<PeerBody>,
}

/// Where a new link's frames go.
enum Transport {
    Lan(WsIo),
    Relay(RelayConn),
}

impl Transport {
    fn path(&self) -> LinkPath {
        match self {
            Transport::Lan(_) => LinkPath::Lan,
            Transport::Relay(_) => LinkPath::Relay,
        }
    }
}

/// The sending side of a running link's current path.
enum Out {
    Lan(WsTx),
    Relay(RelayConn),
}

impl Out {
    fn capacity(&self) -> usize {
        match self {
            Out::Lan(tx) => tx.capacity(),
            Out::Relay(c) => c.tx.capacity(),
        }
    }

    /// Wait for room in the queue; false once it is gone.
    async fn room(&self) -> bool {
        match self {
            Out::Lan(tx) => tx.room().await,
            Out::Relay(c) => c.tx.reserve().await.is_ok(),
        }
    }

    /// Resolves once the relay session of a relay path has ended; never for LAN.
    async fn lost(&self) {
        match self {
            Out::Lan(_) => std::future::pending().await,
            Out::Relay(c) => c.tx.closed().await,
        }
    }

    fn send(&self, to: &str, env: PeerEnvelope) -> Result<(), SendFail> {
        match self {
            Out::Lan(tx) => tx.send(serde_json::to_string(&env).expect("PeerEnvelope serializes")),
            Out::Relay(c) => c.send_frame(&RelayClientFrame::Send { to: to.to_string(), env, pairing: None }),
        }
    }

    /// `send` for frames that must not be skipped: on the relay path it waits (bounded) for room in
    /// the shared queue. (A LAN link's own queue is checked before pulling the frame.)
    async fn send_wait(&self, to: &str, env: PeerEnvelope) -> bool {
        match self {
            Out::Lan(_) => self.send(to, env).is_ok(),
            Out::Relay(c) => c.send_wait(&RelayClientFrame::Send { to: to.to_string(), env, pairing: None }).await,
        }
    }

    fn close(self, abort: bool) {
        if let Out::Lan(tx) = self {
            if abort {
                tx.abort();
            } else {
                tx.close();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fallback {
    /// Now on the relay path.
    Moved,
    /// The relay is not up.
    NoRelay,
    /// The link is no longer the registered one, or the device is gone.
    Gone,
}

/// Where a pair_request came from.
#[derive(Debug, Clone, Copy)]
enum PairOrigin {
    Lan(SocketAddr),
    /// Through the relay (the sender key was already checked against the envelope's).
    Relay,
}

impl PairOrigin {
    fn key(self) -> PairKey {
        match self {
            PairOrigin::Lan(a) => a.ip().into(),
            PairOrigin::Relay => PairKey::Relay,
        }
    }

    /// For logs and `PairRejected::remote_addr`.
    fn label(self) -> String {
        match self {
            PairOrigin::Lan(a) => a.to_string(),
            PairOrigin::Relay => "relay".into(),
        }
    }

    fn path(self) -> LinkPath {
        match self {
            PairOrigin::Lan(_) => LinkPath::Lan,
            PairOrigin::Relay => LinkPath::Relay,
        }
    }
}

/// The inviter's answer to our pair_request.
enum PairReply {
    Accept(Machine),
    Reject,
    /// Not an answer to this request (or not authentic).
    Other,
}

enum Frame {
    /// Authenticated and handled.
    Ok,
    /// Not authentic, replayed, or malformed: ignored.
    Dropped,
    Bye(ByeReason),
}

enum DialError {
    Unreachable,
    Timeout,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while holding one of these locks leaves plain data behind; keep going.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Within TS_SKEW_MAX of our clock.
fn ts_fresh(ts: UnixMs) -> bool {
    ts.abs_diff(now_ms()) <= TS_SKEW_MAX.as_millis() as u64
}

/// A hello or presence stamped too far from our clock (other bodies carry no such stamp).
fn stale_stamp(body: &PeerBody) -> bool {
    match body {
        PeerBody::Hello { ts, .. } => !ts_fresh(*ts),
        PeerBody::Presence { presence } => !ts_fresh(presence.ts),
        _ => false,
    }
}

fn time_seq() -> u64 {
    (now_ms().max(0) as u64).saturating_mul(1000)
}

/// Resolves once the node starts shutting down.
async fn closed(rx: &mut watch::Receiver<bool>) {
    while !*rx.borrow_and_update() {
        if rx.changed().await.is_err() {
            return;
        }
    }
}

async fn sleep_opt(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}

/// The next frame from a link's LAN socket; never resolves without one.
async fn recv_lan(rx: &mut Option<mpsc::Receiver<String>>) -> Option<String> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

/// Uniform in [d/2, d).
fn jitter(d: Duration) -> Duration {
    let half = d / 2;
    let span = half.as_nanos().max(1) as u64;
    half + Duration::from_nanos(getrandom::u64().unwrap_or(0) % span)
}

impl PeerNode {
    pub fn new(keys: DeviceKeys, me: Machine, roster: Arc<dyn Roster>, events: UnboundedSender<PeerEvent>) -> Self {
        let (links_changed, _) = watch::channel(0u64);
        let (relay_changed, _) = watch::channel(0u64);
        let (shutdown_tx, _) = watch::channel(false);
        PeerNode {
            inner: Arc::new(Inner {
                keys,
                me: Mutex::new(me),
                roster,
                events,
                timings: Mutex::new(PeerTimings::default()),
                presence: Mutex::new(None),
                invites: Mutex::new(Vec::new()),
                advertise: Mutex::new(None),
                listener: Mutex::new(None),
                links: Mutex::new(HashMap::new()),
                links_changed,
                dialers: Mutex::new(HashMap::new()),
                conn_limits: ConnLimits::new(),
                pair_limiter: Mutex::new(PairLimiter::default()),
                cooldowns: Mutex::new(HashMap::new()),
                revoked_answers: Mutex::new(HashMap::new()),
                relay: Mutex::new(RelayState::default()),
                relay_changed,
                relay_pending: Mutex::new(HashSet::new()),
                relay_refused: Mutex::new(HashSet::new()),
                relay_joins: Mutex::new(HashMap::new()),
                gate: Mutex::new(()),
                seq_lock: Mutex::new(()),
                rx_lock: Mutex::new(()),
                next_link_id: AtomicU64::new(1),
                next_relay_session: AtomicU64::new(1),
                #[cfg(test)]
                opens: AtomicU64::new(0),
                closing: AtomicBool::new(false),
                shutdown_tx,
            }),
        }
    }

    pub fn with_timings(self, t: PeerTimings) -> Self {
        *lock(&self.inner.timings) = t;
        self
    }

    pub fn public_b64(&self) -> String {
        self.inner.keys.public_b64()
    }

    /// Start the peer listener (only `GET /peer`; see the contract). Returns the bound address.
    pub async fn listen(&self, addr: SocketAddr) -> anyhow::Result<SocketAddr> {
        self.inner.clone().listen(addr).await
    }

    pub fn listening(&self) -> Option<SocketAddr> {
        lock(&self.inner.listener).as_ref().map(|l| l.addr)
    }

    /// New single-use invite. `advertise` overrides the "host:port" put in the URI (required when the
    /// listener binds 0.0.0.0 / ::, unless the relay can carry the invite alone). While the relay is
    /// connected the URI also carries `relay`, `rt` (a one-shot relay pairing slot) and `rk` (its access
    /// key), and then needs no LAN addr. `ttl` must be 60..=3600 s. `ListenerOff`: neither the listener
    /// nor the relay is on.
    pub fn create_invite(&self, advertise: Option<String>, ttl: Duration) -> Result<Invite, InviteError> {
        self.inner.create_invite(advertise, ttl)
    }

    /// Pair with the hub that issued `uri` (see `join_invite`).
    pub async fn join(&self, uri: &str) -> Result<(Device, Machine), JoinError> {
        self.join_invite(uri).await.map(|j| (j.device, j.machine))
    }

    /// Pair with the hub that issued `uri`: over its LAN addr (only for 3 s when the URI also names a
    /// relay), else through the relay. With no relay configured, the invite's relay (with its access
    /// key) is turned on for this and kept (`Joined::adopted_relay`); a different configured relay is
    /// never switched. On success the link is live and kept up.
    pub async fn join_invite(&self, uri: &str) -> Result<Joined, JoinError> {
        let invite = Invite::parse(uri).map_err(|_| JoinError::BadUri)?;
        let inner = &self.inner;
        if invite.machine_id == inner.me_id() || invite.public_key == inner.keys.public_key() {
            return Err(JoinError::SelfInvite);
        }
        if inner.is_closing() {
            return Err(JoinError::Unreachable);
        }
        timeout(JOIN_TIMEOUT, inner.clone().join(invite)).await.map_err(|_| JoinError::Timeout)?
    }

    /// Idempotent: keep a dial loop running for every roster hub (LAN dialing where it has an addr,
    /// relay hellos while the relay is up), and resend the relay allow list if it changed.
    pub fn connect_known(&self) {
        if tokio::runtime::Handle::try_current().is_err() {
            warn!("connect_known called outside a Tokio runtime; ignored");
            return;
        }
        for entry in self.inner.roster.list() {
            if entry.device.kind == DeviceKind::Hub {
                self.inner.ensure_dialer(&entry.device.id);
            }
        }
        self.inner.sync_relay_allow();
    }

    /// Update this hub's machine (rename, version). The id never changes. Pushes presence.
    pub fn set_machine(&self, me: Machine) {
        self.inner.set_machine(me);
        self.inner.push_presence();
    }

    /// Latest presence snapshot; pushed now and every `presence_interval`. Its machine also updates
    /// this hub's machine (the id never changes); `ts` is set at send time.
    pub fn set_presence(&self, p: Presence) {
        self.inner.set_machine(p.machine.clone());
        *lock(&self.inner.presence) = Some(p);
        self.inner.push_presence();
    }

    /// Queue a body on the device's live link (in order, on whichever path it uses). `TooLarge` bodies
    /// are never sent; on `Backpressure` (LINK_QUEUE frames already waiting) the link is closed and the
    /// peer resyncs when it reconnects. Never blocks.
    pub fn send(&self, device_id: &str, body: PeerBody) -> Result<(), SendError> {
        self.inner.send(device_id, &body)
    }

    /// Revoke a device: `bye{revoked}` on its live link, close it, mark it revoked, stop dialing, drop
    /// it from the relay allow list. No-op for an id that is neither paired nor linked.
    pub async fn revoke(&self, device_id: &str) {
        self.inner.revoke(device_id).await
    }

    /// Close the device's live link without revoking it and without bye (`LinkDown` follows). For
    /// `cooldown` we neither dial it nor accept its hello or relay frames (dropped silently); then the
    /// dialer resumes with its usual backoff. A new pairing with the device ends the cooldown. No-op
    /// for an id that is neither paired nor linked.
    pub fn disconnect(&self, device_id: &str, cooldown: Duration) {
        self.inner.disconnect(device_id, cooldown)
    }

    /// Linked device ids and the time of their last authenticated frame.
    pub fn online(&self) -> Vec<(String, UnixMs)> {
        let mut v: Vec<(String, UnixMs)> =
            lock(&self.inner.links).iter().map(|(id, l)| (id.clone(), l.last_auth.load(Ordering::Relaxed))).collect();
        v.sort();
        v
    }

    /// The path of the device's live link.
    pub fn link_path(&self, device_id: &str) -> Option<LinkPath> {
        self.inner.link_path(device_id)
    }

    /// Use this relay (replacing the current one), or none. Connects in the background and keeps
    /// reconnecting; progress comes as `PeerEvent::RelayStatus`. Turning a relay on must happen inside a
    /// Tokio runtime. Turning it off (or switching) drops the links on the relay path.
    pub fn set_relay(&self, relay: Option<RelayConfig>) -> Result<(), RelayConfigError> {
        let relay = relay.map(|r| r.normalized()).transpose()?;
        if relay.is_some() && tokio::runtime::Handle::try_current().is_err() {
            warn!("set_relay called outside a Tokio runtime; ignored");
            return Ok(());
        }
        self.inner.set_relay(relay);
        Ok(())
    }

    /// The relay connection. `last_error` codes: `unreachable`, `timeout`, `bad_key` (the relay refused
    /// the access key), `unavailable` (the relay is not serving), `rate_limited`, `bad_auth`, `bad_frame`,
    /// `unsupported_version`, `replaced` (another socket with our key took over the mailbox), `closed`.
    pub fn relay_status(&self) -> RelayStatusInfo {
        self.inner.relay_status()
    }

    /// Stop the LAN listener (no-op when it is off). On return the listener is closed, its port is
    /// released and every link it accepted is closed (`LinkDown`; the dial loop may bring the device back
    /// over the relay). Links this hub dialed out and relay links stay up. `listen` works again afterwards.
    pub async fn stop_listening(&self) {
        self.inner.stop_listening().await
    }

    /// `bye{shutdown}` to every link, stop dialing, the listener and the relay. Idempotent.
    pub async fn shutdown(&self) {
        self.inner.shutdown().await
    }
}

impl Inner {
    fn timings(&self) -> PeerTimings {
        *lock(&self.timings)
    }

    fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }

    fn emit(&self, e: PeerEvent) {
        let _ = self.events.send(e);
    }

    fn me_id(&self) -> String {
        lock(&self.me).id.clone()
    }

    /// This hub's machine as sent to peers.
    fn me(&self) -> Machine {
        let mut m = lock(&self.me).clone();
        m.online = true;
        m.last_seen = now_ms();
        m
    }

    fn set_machine(&self, m: Machine) {
        let mut me = lock(&self.me);
        if m.id != me.id {
            warn!(current = %me.id, given = %m.id, "set_machine with a different machine id; keeping the id");
        }
        *me = Machine { id: me.id.clone(), ..m };
    }

    /// The "host:port" peers should dial us at, if we know one.
    fn advertised_addr(&self) -> Option<String> {
        match self.listener_addr() {
            Some(a) if !a.ip().is_unspecified() => Some(a.to_string()),
            Some(_) => lock(&self.advertise).clone(),
            None => None,
        }
    }

    fn listener_addr(&self) -> Option<SocketAddr> {
        lock(&self.listener).as_ref().map(|l| l.addr)
    }

    fn presence_body(&self) -> PeerBody {
        let me = self.me();
        let now = me.last_seen;
        let mut p = lock(&self.presence).clone().unwrap_or_else(|| Presence {
            machine: me.clone(),
            agents: Vec::new(),
            repos: Vec::new(),
            pets: Vec::new(),
            user_active_at: None,
            ts: now,
        });
        p.machine = me;
        p.ts = now;
        PeerBody::Presence { presence: p }
    }

    fn push_presence(&self) {
        for l in lock(&self.links).values() {
            let _ = l.ctl.try_send(LinkCmd::Presence);
        }
    }

    fn send(&self, device_id: &str, body: &PeerBody) -> Result<(), SendError> {
        if matches!(
            body,
            PeerBody::PairRequest { .. }
                | PeerBody::PairAccept { .. }
                | PeerBody::PairReject { .. }
                | PeerBody::Hello { .. }
                | PeerBody::Presence { .. }
                | PeerBody::Bye { .. }
        ) {
            return Err(SendError::Reserved);
        }
        let (data, kill, queued) = lock(&self.links)
            .get(device_id)
            .map(|l| (l.data.clone(), l.kill.clone(), l.queued_bytes.clone()))
            .ok_or(SendError::NotLinked)?;
        let plaintext = serde_json::to_vec(body).expect("PeerBody serializes");
        let len = plaintext.len();
        if len > MAX_SEND_BODY_BYTES {
            return Err(SendError::TooLarge);
        }
        let overflow = |what: &str| {
            warn!(device = device_id, "peer link outbound {what} full; closing the link (the peer resyncs on reconnect)");
            kill.notify_one();
            Err(SendError::Backpressure)
        };
        // Reserve the bytes first so concurrent senders cannot overshoot the budget together.
        if queued.fetch_add(len, Ordering::SeqCst) + len > LINK_QUEUE_BYTES {
            queued.fetch_sub(len, Ordering::SeqCst);
            return overflow("byte budget");
        }
        match data.try_send(plaintext) {
            Ok(()) => Ok(()),
            Err(e) => {
                queued.fetch_sub(len, Ordering::SeqCst);
                match e {
                    TrySendError::Full(_) => overflow("queue"),
                    TrySendError::Closed(_) => Err(SendError::NotLinked),
                }
            }
        }
    }

    // ---------- seq ----------

    /// `max(last_tx + 1, now_ms * 1000)`, persisted.
    fn next_tx_seq(&self, device_id: &str) -> u64 {
        let _g = lock(&self.seq_lock);
        let seq = self.roster.last_tx_seq(device_id).saturating_add(1).max(time_seq());
        self.roster.set_last_tx_seq(device_id, seq);
        seq
    }

    /// Accept `seq` only if it is above the persisted last_rx; persists it.
    fn accept_rx_seq(&self, device_id: &str, seq: u64) -> bool {
        let _g = lock(&self.rx_lock);
        if seq <= self.roster.last_rx_seq(device_id) {
            return false;
        }
        self.roster.set_last_rx_seq(device_id, seq);
        true
    }

    /// `open` (every inbound envelope goes through here).
    fn open_from(&self, env: &PeerEnvelope, sender_pk: &[u8; 32]) -> Result<PeerBody, crate::OpenError> {
        #[cfg(test)]
        self.opens.fetch_add(1, Ordering::Relaxed);
        open(&self.keys, env, sender_pk)
    }

    /// Cheap replay check before any decryption: `seq` must be above the last one accepted.
    fn seq_above_last(&self, device_id: &str, seq: u64) -> bool {
        seq > self.roster.last_rx_seq(device_id)
    }

    fn seal_text(&self, to: &str, to_pk: &[u8; 32], body: &PeerBody) -> String {
        let seq = self.next_tx_seq(to);
        let env = seal(&self.keys, &self.me_id(), to, to_pk, seq, body, false);
        serde_json::to_string(&env).expect("PeerEnvelope serializes")
    }

    // ---------- invites ----------

    fn create_invite(&self, advertise: Option<String>, ttl: Duration) -> Result<Invite, InviteError> {
        let bound = self.listener_addr();
        let relay = self.relay_conn();
        // An explicit LAN address needs the listener behind it.
        if bound.is_none() && (relay.is_none() || advertise.is_some()) {
            return Err(InviteError::ListenerOff);
        }
        if !(INVITE_TTL_MIN..=INVITE_TTL_MAX).contains(&ttl) {
            return Err(InviteError::Invalid);
        }
        let explicit = advertise.is_some();
        let addr = match (advertise, bound) {
            (Some(a), _) => {
                let a = a.trim().to_string();
                if !valid_addr(&a) {
                    return Err(InviteError::Invalid);
                }
                Some(a)
            }
            (None, Some(b)) if !b.ip().is_unspecified() => Some(b.to_string()),
            // Bound to all interfaces with nothing to advertise: only the relay can carry this invite.
            (None, Some(_)) if relay.is_some() => None,
            (None, Some(_)) => return Err(InviteError::Invalid),
            (None, None) => None,
        };
        let (code_bytes, code) = new_code();
        let expires_at = now_ms() + ttl.as_millis() as i64;
        // Register the relay slot first: if the relay queue cannot take it, the invite goes without it.
        let slot = relay.and_then(|conn| {
            let (token, token_b64) = new_code();
            match conn.send_frame(&RelayClientFrame::AllowPairing { token: token_b64.clone(), exp: expires_at }) {
                Ok(()) => Some((RelaySlot { url: conn.url.clone(), token }, token_b64)),
                Err(_) => {
                    warn!("relay queue full; invite created without a relay pairing slot");
                    None
                }
            }
        });
        if addr.is_none() && slot.is_none() {
            return Err(InviteError::ListenerOff);
        }
        let (relay_url, relay_token, relay_key) = match &slot {
            Some((s, token)) => {
                let key = self.relay_config().filter(|c| c.url == s.url).and_then(|c| c.access_key);
                (Some(s.url.clone()), Some(token.clone()), key)
            }
            None => (None, None, None),
        };
        {
            let mut invites = lock(&self.invites);
            let now = now_ms();
            invites.retain(|i| i.expires_at > now);
            if slot.is_some() && invites.iter().filter(|i| i.relay.is_some()).count() >= MAX_RELAY_INVITES {
                // The relay just evicted the oldest slot; drop that invite too.
                if let Some(oldest) = invites.iter().position(|i| i.relay.is_some()) {
                    invites.remove(oldest);
                }
            }
            if invites.len() >= MAX_INVITES {
                invites.remove(0);
            }
            invites.push(LiveInvite { code: code_bytes, expires_at, addr: addr.clone(), relay: slot.map(|(s, _)| s) });
        }
        if explicit {
            *lock(&self.advertise) = addr.clone();
        }
        info!(addr = ?addr, relay = relay_url.is_some(), expires_at, "peer invite created");
        Ok(Invite {
            machine_id: self.me_id(),
            public_key: self.keys.public_key(),
            addr,
            code,
            expires_at,
            relay: relay_url,
            relay_token,
            relay_key,
        })
    }

    fn has_live_invite(&self) -> bool {
        let mut invites = lock(&self.invites);
        let now = now_ms();
        invites.retain(|i| i.expires_at > now);
        !invites.is_empty()
    }

    /// Constant-time match against every live invite. A match consumes that invite and returns the
    /// LAN addr it advertised (if any). A miss consumes nothing: anyone who knows our public key can
    /// send a wrong code, so burning invites would let them block pairing. Guessing is the rate
    /// limiter's problem (and infeasible against 128-bit codes).
    fn consume_invite(&self, code: &str) -> Option<Option<String>> {
        let mut given = b64::decode_url(code).ok().filter(|c| c.len() == CODE_LEN);
        let mut invites = lock(&self.invites);
        let now = now_ms();
        invites.retain(|i| i.expires_at > now);
        let mut found = None;
        if let Some(g) = &given {
            for (idx, inv) in invites.iter().enumerate() {
                let eq: bool = inv.code[..].ct_eq(&g[..]).into();
                if eq && found.is_none() {
                    found = Some(idx);
                }
            }
        }
        if let Some(g) = given.as_mut() {
            wipe(g);
        }
        found.map(|i| invites.remove(i).addr.clone())
    }

    /// The pairing slots of live invites made on the relay at `url` (base64url token, expiry).
    fn relay_slots(&self, url: &str) -> Vec<(String, UnixMs)> {
        let now = now_ms();
        lock(&self.invites)
            .iter()
            .filter(|i| i.expires_at > now)
            .filter_map(|i| i.relay.as_ref().filter(|r| r.url == url).map(|r| (b64::encode_url(&r.token), i.expires_at)))
            .collect()
    }

    // ---------- listener ----------

    async fn listen(self: Arc<Self>, addr: SocketAddr) -> anyhow::Result<SocketAddr> {
        if self.is_closing() {
            bail!("peer node is shut down");
        }
        if let Some(a) = self.listener_addr() {
            bail!("peer listener already running on {a}");
        }
        let listener =
            tokio::net::TcpListener::bind(addr).await.with_context(|| format!("binding peer listener on {addr}"))?;
        let bound = listener.local_addr()?;
        if bound.ip().is_unspecified() {
            warn!(addr = %bound, "peer listener bound to all interfaces; prefer a LAN or Tailscale address");
        }
        let app = Router::new()
            .route(PEER_PATH, get(ws_handler).fallback(not_found))
            .fallback(not_found)
            .layer(middleware::from_fn(reject_origin))
            .with_state(self.clone());
        let listener = PeerListener::new(listener, self.conn_limits.clone(), self.timings().handshake_timeout);
        let mut node_stop = self.shutdown_tx.subscribe();
        let (stop, mut own_stop) = watch::channel(false);
        let task = tokio::spawn(async move {
            let server = axum::serve(listener, app.into_make_service_with_connect_info::<PeerConn>())
                .with_graceful_shutdown(async move {
                    tokio::select! {
                        _ = closed(&mut node_stop) => {}
                        _ = closed(&mut own_stop) => {}
                    }
                });
            if let Err(e) = server.await {
                warn!(error = %e, "peer listener stopped");
            }
        });
        let mut slot = lock(&self.listener);
        if slot.is_some() {
            task.abort();
            bail!("peer listener already running");
        }
        *slot = Some(ListenerState { addr: bound, task, stop });
        info!(addr = %bound, "peer listener on");
        Ok(bound)
    }

    async fn handle_inbound(self: Arc<Self>, io: WsIo, conn: PeerConn) {
        let mut io = io;
        let remote = conn.remote;
        let t = self.timings();
        let first = match timeout(t.handshake_timeout, io.rx.recv()).await {
            Ok(Some(text)) => text,
            _ => {
                debug!(remote = %remote, "peer socket closed: no first frame");
                return io.close();
            }
        };
        let Ok(env) = serde_json::from_str::<PeerEnvelope>(&first) else {
            debug!(remote = %remote, "peer socket closed: malformed first frame");
            return io.close();
        };
        let me_id = self.me_id();
        if env.to != me_id || env.from == me_id {
            debug!(remote = %remote, "peer socket closed: misaddressed first frame");
            return io.close();
        }
        // Handshake checks and link registration are synchronous, so a concurrent revoke is either
        // seen by `register` or sees the registered link.
        let setup = if env.pk.is_some() { self.inbound_pair(&io, env, remote) } else { self.inbound_hello(&io, env, remote) };
        let Some(setup) = setup else { return io.close() };
        let device_id = setup.device_id.clone();
        let Some(link) = self.register(setup) else { return io.close() };
        // Authenticated: lift the connection deadline and free the unauthenticated slot.
        conn.shared.authenticated();
        // The listener stopped while this handshake ran: close this link like the others it accepted.
        if self.listener_addr().is_none() {
            self.close_accepted_now();
        }
        // Keep the link up from our side too (LAN dialing when the peer has a listener, relay hellos).
        self.ensure_dialer(&device_id);
        self.run_link(Transport::Lan(io), link).await
    }

    fn inbound_pair(&self, io: &WsIo, env: PeerEnvelope, remote: SocketAddr) -> Option<LinkSetup> {
        let pk = parse_pk(env.pk.as_deref()?)?;
        self.pair_request(env, pk, PairOrigin::Lan(remote), &|reply| {
            let _ = io.send(serde_json::to_string(&reply).expect("PeerEnvelope serializes"));
        })
    }

    /// A pair_request (its envelope carries the sender key `pk`) from the listener or the relay.
    /// Rejections are answered through `reply`, rate limited and audited per origin. Success stores the
    /// device and returns the link to register, whose first frame is our pair_accept.
    fn pair_request(
        &self,
        env: PeerEnvelope,
        pk: [u8; 32],
        origin: PairOrigin,
        reply: &dyn Fn(PeerEnvelope),
    ) -> Option<LinkSetup> {
        let source = origin.label();
        if lock(&self.pair_limiter).blocked(origin.key(), std::time::Instant::now()) {
            debug!(remote = %source, "pair attempt dropped: too many failures from this address");
            return None;
        }
        if !self.has_live_invite() {
            debug!(remote = %source, "pair attempt closed: no live invite");
            return None;
        }
        if pk == self.keys.public_key() {
            return None;
        }
        if !lock(&self.pair_limiter).attempt(std::time::Instant::now()) {
            debug!(remote = %source, "pair attempt dropped: too many pair attempts this minute");
            return None;
        }
        let me_id = self.me_id();
        let reject = |reason: PairRejectReason| -> Option<LinkSetup> {
            let failure = lock(&self.pair_limiter).failure(origin.key(), std::time::Instant::now());
            if failure.blocked {
                warn!(remote = %source, "too many failed pair attempts; ignoring this address for a while");
            }
            // At most one event (audit entry) per address per minute.
            if let Some(coalesced) = failure.emit {
                info!(remote = %source, ?reason, coalesced, "pair attempt rejected");
                self.emit(PeerEvent::PairRejected { remote_addr: source.clone(), reason });
            } else {
                debug!(remote = %source, ?reason, "pair attempt rejected (event coalesced)");
            }
            // Reject replies do not touch the roster's seq counters (the sender is not paired).
            let body = PeerBody::PairReject { reason, ts: now_ms() };
            reply(seal(&self.keys, &me_id, &env.from, &pk, time_seq(), &body, false));
            None
        };
        // Never move last_rx backwards (that would re-open old frames from this id to replay), even
        // for a revoked or re-pairing device. Checked before decrypting and before the code, so a
        // replay costs neither.
        if !self.seq_above_last(&env.from, env.seq) {
            return reject(PairRejectReason::BadRequest);
        }
        let Ok(body) = self.open_from(&env, &pk) else { return reject(PairRejectReason::BadRequest) };
        let PeerBody::PairRequest { code, machine, addr, ts } = body else {
            return reject(PairRejectReason::BadRequest);
        };
        if machine.id != env.from || !is_valid_machine_id(&env.from) || env.from == me_id {
            return reject(PairRejectReason::BadRequest);
        }
        // A request can wait in our relay mailbox while we are offline; its joiner gave up after
        // JOIN_TIMEOUT, and pairing now would leave a one-sided pairing.
        let stale_before = now_ms() - (JOIN_TIMEOUT + STALE_PAIR_SKEW).as_millis() as i64;
        if matches!(origin, PairOrigin::Relay) && ts < stale_before {
            debug!(remote = %source, "stale pair_request from the relay");
            return reject(PairRejectReason::BadRequest);
        }
        let Some(invite_addr) = self.consume_invite(&code) else { return reject(PairRejectReason::InvalidInvite) };
        // A peer-reported addr is kept only if it is a private/LAN IP literal.
        let addr = addr.filter(|a| peer_addr_ok(a));

        let now = now_ms();
        let device = Device {
            id: env.from.clone(),
            kind: DeviceKind::Hub,
            name: machine.name.clone(),
            public_key: b64::encode(&pk),
            paired_at: now,
            last_seen: Some(now),
            addr,
            path: None,
        };
        let online = Machine { last_seen: now, online: true, ..machine };
        self.roster.upsert(RosterEntry { device: device.clone(), machine: Machine { online: false, ..online.clone() } });
        // Above the stored value (checked), so this never lowers it.
        self.roster.set_last_rx_seq(&device.id, env.seq);
        info!(device = %device.id, remote = %source, "paired (we invited)");
        self.emit(PeerEvent::Paired { device: device.clone(), machine: online.clone(), we_invited: true });
        Some(LinkSetup {
            device_id: device.id,
            peer_pk: pk,
            path: origin.path(),
            dialed_by_me: false,
            machine: online,
            force: true,
            // req_seq ties the accept to this request: an old captured accept cannot be replayed.
            reply: Some(PeerBody::PairAccept { machine: self.me(), addr: invite_addr, req_seq: env.seq, ts: now }),
        })
    }

    fn inbound_hello(&self, io: &WsIo, env: PeerEnvelope, remote: SocketAddr) -> Option<LinkSetup> {
        if self.cooldown_until(&env.from).is_some() {
            debug!(remote = %remote, device = %env.from, "peer socket closed: device is cooling down after a disconnect");
            return None;
        }
        let Some(entry) = self.roster.get(&env.from) else {
            let reply = |bye: PeerEnvelope| {
                let _ = io.send(serde_json::to_string(&bye).expect("PeerEnvelope serializes"));
            };
            if !self.answer_revoked(&env, None, true, &reply) {
                debug!(remote = %remote, "peer socket closed: unknown or revoked sender");
            }
            return None;
        };
        let pk = parse_pk(&entry.device.public_key)?;
        if !self.seq_above_last(&env.from, env.seq) {
            debug!(remote = %remote, device = %env.from, "peer socket closed: replayed hello");
            return None;
        }
        let Ok(PeerBody::Hello { machine, addr, ts }) = self.open_from(&env, &pk) else {
            debug!(remote = %remote, device = %env.from, "peer socket closed: hello did not authenticate");
            return None;
        };
        if !ts_fresh(ts) {
            debug!(remote = %remote, device = %env.from, "peer socket closed: hello stamped too far from our clock");
            return None;
        }
        if machine.id != env.from || !self.accept_rx_seq(&env.from, env.seq) {
            debug!(remote = %remote, device = %env.from, "peer socket closed: bad or replayed hello");
            return None;
        }
        let now = now_ms();
        let addr = addr.filter(|a| peer_addr_ok(a));
        self.roster.update_seen(&env.from, &machine, addr.as_deref(), now);
        Some(LinkSetup {
            device_id: env.from,
            peer_pk: pk,
            path: LinkPath::Lan,
            dialed_by_me: false,
            machine: Machine { last_seen: now, online: true, ..machine },
            force: false,
            reply: Some(PeerBody::Hello { machine: self.me(), addr: self.advertised_addr(), ts: now }),
        })
    }

    // ---------- dialing ----------

    async fn connect(addr: &str, limit: Duration) -> Result<WsIo, DialError> {
        use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
        let url = format!("ws://{addr}{PEER_PATH}");
        let cfg = WebSocketConfig::default().max_message_size(Some(MAX_FRAME)).max_frame_size(Some(MAX_FRAME));
        match timeout(limit, tokio_tungstenite::connect_async_with_config(url, Some(cfg), true)).await {
            Ok(Ok((ws, _))) => Ok(spawn_io(ws)),
            Ok(Err(e)) => {
                debug!(addr, error = %e, "peer dial failed");
                Err(DialError::Unreachable)
            }
            Err(_) => Err(DialError::Timeout),
        }
    }

    async fn join(self: Arc<Self>, invite: Invite) -> Result<Joined, JoinError> {
        // The LAN first; when the URI also names a relay, only briefly.
        let mut lan_error = None;
        if let Some(addr) = &invite.addr {
            let limit = if invite.relay.is_some() { JOIN_LAN_TIMEOUT } else { JOIN_TIMEOUT };
            match Self::connect(addr, limit).await {
                Ok(io) => {
                    let (device, machine) = self.join_lan(&invite, io).await?;
                    return Ok(Joined { device, machine, adopted_relay: None });
                }
                Err(e) => {
                    lan_error = Some(match e {
                        DialError::Unreachable => JoinError::Unreachable,
                        DialError::Timeout => JoinError::Timeout,
                    })
                }
            }
        }
        if invite.relay.is_some() {
            return self.join_relay(&invite).await;
        }
        Err(lan_error.unwrap_or(JoinError::Unreachable))
    }

    async fn join_lan(self: &Arc<Self>, invite: &Invite, mut io: WsIo) -> Result<(Device, Machine), JoinError> {
        let req_seq = self.next_tx_seq(&invite.machine_id);
        let env = self.pair_request_env(invite, req_seq);
        if io.send(serde_json::to_string(&env).expect("PeerEnvelope serializes")).is_err() {
            return Err(JoinError::Unreachable);
        }
        // Closed without a reply: no live invite there (expired or used), or not a Krypto listener.
        let Some(text) = io.rx.recv().await else { return Err(JoinError::Rejected) };
        let reply = serde_json::from_str::<PeerEnvelope>(&text).map_err(|_| JoinError::Rejected)?;
        let PairReply::Accept(machine) = self.open_pair_reply(invite, req_seq, &reply) else {
            return Err(JoinError::Rejected);
        };
        Ok(self.finish_join(invite, machine, reply.seq, req_seq, Transport::Lan(io)))
    }

    fn pair_request_env(&self, invite: &Invite, req_seq: u64) -> PeerEnvelope {
        let req = PeerBody::PairRequest {
            code: invite.code.clone(),
            machine: self.me(),
            addr: self.advertised_addr(),
            ts: now_ms(),
        };
        seal(&self.keys, &self.me_id(), &invite.machine_id, &invite.public_key, req_seq, &req, true)
    }

    /// Is `reply` the inviter's answer to our pair_request `req_seq`?
    fn open_pair_reply(&self, invite: &Invite, req_seq: u64, reply: &PeerEnvelope) -> PairReply {
        let mid = &invite.machine_id;
        if reply.from != *mid || reply.to != self.me_id() || reply.pk.is_some() {
            return PairReply::Other;
        }
        // Only the holder of the URI's key can produce this.
        if !self.seq_above_last(mid, reply.seq) {
            return PairReply::Other;
        }
        let Ok(body) = self.open_from(reply, &invite.public_key) else { return PairReply::Other };
        match body {
            PeerBody::PairAccept { machine, req_seq: echoed, .. } => {
                // The accept must answer this very request (an old captured accept carries an old
                // req_seq), and must not move last_rx backwards, even for a revoked or unknown row.
                if machine.id != *mid || echoed != req_seq || reply.seq <= self.roster.last_rx_seq(mid) {
                    debug!(device = %mid, "pair_accept rejected: identity, req_seq or seq check failed");
                    return PairReply::Other;
                }
                PairReply::Accept(machine)
            }
            PeerBody::PairReject { reason, .. } => {
                info!(device = %mid, ?reason, "pairing rejected by inviter");
                PairReply::Reject
            }
            _ => PairReply::Other,
        }
    }

    /// Store the inviter we joined and bring the link up on `transport`.
    fn finish_join(
        self: &Arc<Self>,
        invite: &Invite,
        machine: Machine,
        reply_seq: u64,
        req_seq: u64,
        transport: Transport,
    ) -> (Device, Machine) {
        let mid = invite.machine_id.clone();
        let now = now_ms();
        let device = Device {
            id: mid.clone(),
            kind: DeviceKind::Hub,
            name: machine.name.clone(),
            public_key: b64::encode(&invite.public_key),
            paired_at: now,
            last_seen: Some(now),
            addr: invite.addr.clone(),
            path: None,
        };
        let online = Machine { last_seen: now, online: true, ..machine };
        self.roster.upsert(RosterEntry { device: device.clone(), machine: Machine { online: false, ..online.clone() } });
        self.roster.set_last_rx_seq(&mid, reply_seq);
        self.roster.set_last_tx_seq(&mid, req_seq);
        let path = transport.path();
        info!(device = %mid, ?path, "paired (we joined)");
        self.emit(PeerEvent::Paired { device: device.clone(), machine: online.clone(), we_invited: false });
        let setup = LinkSetup {
            device_id: mid.clone(),
            peer_pk: invite.public_key,
            path,
            dialed_by_me: path == LinkPath::Lan,
            machine: online.clone(),
            force: true,
            reply: None,
        };
        if let Some(link) = self.register(setup) {
            tokio::spawn(self.clone().run_link(transport, link));
        }
        self.ensure_dialer(&mid);
        self.sync_relay_allow();
        (device, online)
    }

    fn ensure_dialer(self: &Arc<Self>, device_id: &str) {
        if self.is_closing() {
            return;
        }
        let mut dialers = lock(&self.dialers);
        if dialers.get(device_id).is_some_and(|h| !h.is_finished()) {
            return;
        }
        let task = tokio::spawn(self.clone().dial_loop(device_id.to_string()));
        dialers.insert(device_id.to_string(), task);
    }

    fn stop_dialer(&self, device_id: &str) {
        if let Some(h) = lock(&self.dialers).remove(device_id) {
            h.abort();
        }
    }

    /// End of the device's disconnect cooldown, if one is running (expired entries are dropped).
    fn cooldown_until(&self, device_id: &str) -> Option<Instant> {
        let mut cooldowns = lock(&self.cooldowns);
        match cooldowns.get(device_id) {
            Some(&until) if until > Instant::now() => Some(until),
            Some(_) => {
                cooldowns.remove(device_id);
                None
            }
            None => None,
        }
    }

    fn disconnect(&self, device_id: &str, cooldown: Duration) {
        let link = {
            let _g = lock(&self.gate);
            let known = self.roster.get(device_id).is_some();
            let link = lock(&self.links).get(device_id).map(|l| (l.ctl.clone(), l.kill.clone()));
            if !known && link.is_none() {
                return;
            }
            // Set under the gate, so a handshake finishing right now cannot register past it.
            if !cooldown.is_zero() {
                lock(&self.cooldowns).insert(device_id.to_string(), Instant::now() + cooldown);
            }
            link
        };
        info!(device = device_id, ?cooldown, "peer disconnected by the hub");
        if let Some((ctl, kill)) = link {
            // Close without bye; kill if the control queue is backed up.
            if ctl.try_send(LinkCmd::Close { bye: None, done: None }).is_err() {
                kill.notify_one();
            }
        }
    }

    /// Keeps a link up to a roster hub. While there is no LAN link it dials the hub's addr (also under a
    /// relay link, so the LAN can take over), backing off `backoff_min` -> `backoff_max` with jitter.
    /// While there is no link at all and the relay is up, it says hello over the relay (after a LAN dial
    /// failed, when there is an addr: the LAN is preferred) once per relay session (the relay queues
    /// frames for an offline peer, so repeating it would only fill the peer's mailbox), again after a
    /// link ends, and with backoff when the relay refused it (`not_allowed`).
    /// Ends when the device is revoked or the node shuts down.
    async fn dial_loop(self: Arc<Self>, device_id: String) {
        let mut changed = self.links_changed.subscribe();
        let mut relay = self.relay_changed.subscribe();
        let mut shutdown = self.shutdown_tx.subscribe();
        let mut backoff = self.timings().backoff_min;
        let mut next_dial = Instant::now();
        let mut linked_at: Option<Instant> = None;
        // The relay session we said hello on, and when to retry after a refusal.
        let mut hello_on: Option<u64> = None;
        let mut hello_backoff = self.timings().backoff_min;
        let mut hello_retry: Option<Instant> = None;
        // A LAN dial failed since the last link: the relay may say hello.
        let mut lan_failed = false;
        loop {
            let t = self.timings();
            if self.is_closing() {
                return;
            }
            let Some(entry) = self.roster.get(&device_id) else { return };
            if let Some(until) = self.cooldown_until(&device_id) {
                tokio::select! {
                    _ = tokio::time::sleep_until(until) => {}
                    _ = closed(&mut shutdown) => return,
                }
                // Our hello of this session may have been dropped by the peer meanwhile: say it again.
                hello_on = None;
                continue;
            }
            changed.borrow_and_update();
            relay.borrow_and_update();
            let path = self.link_path(&device_id);
            match (path, linked_at) {
                (Some(_), None) => linked_at = Some(Instant::now()),
                (None, Some(at)) => {
                    // Our link just ended: redial soon if it was stable, else keep backing off.
                    linked_at = None;
                    backoff = if at.elapsed() >= t.offline_after { t.backoff_min } else { (backoff * 2).min(t.backoff_max) };
                    next_dial = Instant::now() + jitter(backoff);
                    hello_on = None;
                }
                _ => {}
            }
            if path.is_some() {
                hello_backoff = t.backoff_min;
                hello_retry = None;
                lan_failed = false;
            } else if let Some(conn) = self.relay_conn().filter(|_| !self.relay_restricted()) {
                let now = Instant::now();
                if lock(&self.relay_refused).remove(&device_id) {
                    // The peer does not allow us (yet): try again later.
                    hello_retry = Some(now + jitter(hello_backoff));
                    hello_backoff = (hello_backoff * 2).min(t.backoff_max);
                }
                if hello_retry.is_some_and(|at| at <= now) {
                    hello_retry = None;
                    hello_on = None;
                }
                let lan_first = entry.device.addr.is_some() && !lan_failed;
                if !lan_first && hello_retry.is_none() && hello_on != Some(conn.id) && self.relay_hello(&entry, &conn) {
                    hello_on = Some(conn.id);
                }
            }
            let addr = entry.device.addr.filter(|_| path != Some(LinkPath::Lan));
            if let Some(addr) = &addr {
                if Instant::now() >= next_dial {
                    match self.dial_once(&device_id, addr).await {
                        Ok(()) => {
                            if linked_at.is_none() {
                                linked_at = Some(Instant::now());
                            }
                        }
                        Err(()) => {
                            lan_failed = true;
                            next_dial = Instant::now() + jitter(backoff);
                            backoff = (backoff * 2).min(t.backoff_max);
                        }
                    }
                    continue;
                }
            }
            let dial_at = addr.as_ref().map(|_| next_dial);
            tokio::select! {
                r = changed.changed() => if r.is_err() { return },
                r = relay.changed() => if r.is_err() { return },
                _ = sleep_opt(dial_at) => {}
                _ = sleep_opt(hello_retry) => {}
                _ = closed(&mut shutdown) => return,
            }
        }
    }

    /// One dial + hello handshake. On success the link is registered and running.
    async fn dial_once(self: &Arc<Self>, device_id: &str, addr: &str) -> Result<(), ()> {
        let t = self.timings();
        let entry = self.roster.get(device_id).ok_or(())?;
        let pk = parse_pk(&entry.device.public_key).ok_or(())?;
        let mut io = Self::connect(addr, t.handshake_timeout).await.map_err(|_| ())?;
        let hello = PeerBody::Hello { machine: self.me(), addr: self.advertised_addr(), ts: now_ms() };
        if io.send(self.seal_text(device_id, &pk, &hello)).is_err() {
            return Err(());
        }
        let text = match timeout(t.handshake_timeout, io.rx.recv()).await {
            Ok(Some(text)) => text,
            _ => {
                debug!(device = device_id, "peer closed or timed out before hello");
                return Err(());
            }
        };
        let env = serde_json::from_str::<PeerEnvelope>(&text).map_err(|_| ())?;
        if env.from != device_id || env.to != self.me_id() || env.pk.is_some() {
            return Err(());
        }
        if !self.seq_above_last(device_id, env.seq) {
            return Err(());
        }
        let (machine, their_addr) = match self.open_from(&env, &pk) {
            Ok(PeerBody::Hello { machine, addr, ts }) if ts_fresh(ts) => (machine, addr),
            // It revoked us while we were away, and says so now.
            Ok(PeerBody::Bye { reason: ByeReason::Revoked, .. }) if self.accept_rx_seq(device_id, env.seq) => {
                info!(device = device_id, "peer revoked us (answer to our hello)");
                self.revoked_by_peer(device_id);
                return Err(());
            }
            _ => return Err(()),
        };
        if machine.id != device_id || !self.accept_rx_seq(device_id, env.seq) {
            return Err(());
        }
        let now = now_ms();
        let their_addr = their_addr.filter(|a| peer_addr_ok(a));
        self.roster.update_seen(device_id, &machine, their_addr.as_deref(), now);
        let setup = LinkSetup {
            device_id: device_id.to_string(),
            peer_pk: pk,
            path: LinkPath::Lan,
            dialed_by_me: true,
            machine: Machine { last_seen: now, online: true, ..machine },
            force: false,
            reply: None,
        };
        let link = self.register(setup).ok_or(())?;
        tokio::spawn(self.clone().run_link(Transport::Lan(io), link));
        Ok(())
    }

    // ---------- links ----------

    /// Register a handshaken link. With a link already up: a pairing link replaces it unconditionally;
    /// a LAN link replaces a relay link; of two LAN links the one dialed by the hub with the smaller
    /// machine id wins (both sides decide the same way), and a reconnect by the same dialer replaces the
    /// old one; a relay link never replaces a live link. None: the new link lost, the device is no
    /// longer paired or is cooling down, or the node is closing.
    fn register(&self, setup: LinkSetup) -> Option<Link> {
        let _g = lock(&self.gate);
        if self.is_closing() || self.roster.get(&setup.device_id).is_none() {
            return None;
        }
        if setup.force {
            // A new pairing is an explicit user action: it ends any disconnect cooldown.
            lock(&self.cooldowns).remove(&setup.device_id);
        } else if self.cooldown_until(&setup.device_id).is_some() {
            debug!(device = %setup.device_id, "link refused: device is cooling down after a disconnect");
            return None;
        }
        let me_id = self.me_id();
        let (ctl_tx, ctl_rx) = mpsc::channel(CTL_QUEUE);
        let (data_tx, data_rx) = mpsc::channel(LINK_QUEUE);
        let (relay_tx, relay_rx) = mpsc::channel(RELAY_IN_QUEUE);
        let kill = Arc::new(Notify::new());
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let last_auth = Arc::new(AtomicI64::new(now_ms()));
        let id = self.next_link_id.fetch_add(1, Ordering::Relaxed);
        let was_up = {
            let mut links = lock(&self.links);
            let was_up = match links.get(&setup.device_id) {
                None => false,
                Some(old) => {
                    let lan = setup.path == LinkPath::Lan;
                    let new_wins = setup.force
                        || (lan && old.path == LinkPath::Relay)
                        || (lan
                            && (old.dialed_by_me == setup.dialed_by_me
                                || setup.dialed_by_me == (me_id.as_str() < setup.device_id.as_str())));
                    if !new_wins {
                        debug!(device = %setup.device_id, "duplicate link closed (tie-break)");
                        return None;
                    }
                    debug!(device = %setup.device_id, "link replaced (tie-break)");
                    let _ = old.ctl.try_send(LinkCmd::Superseded);
                    true
                }
            };
            links.insert(
                setup.device_id.clone(),
                LinkEntry {
                    id,
                    path: setup.path,
                    dialed_by_me: setup.dialed_by_me,
                    ctl: ctl_tx,
                    data: data_tx,
                    kill: kill.clone(),
                    queued_bytes: queued_bytes.clone(),
                    last_auth: last_auth.clone(),
                    relay_in: relay_tx,
                },
            );
            was_up
        };
        lock(&self.relay_pending).remove(&setup.device_id);
        self.links_changed.send_modify(|v| *v = v.wrapping_add(1));
        // Also over an existing link: bodies queued on the replaced link may be lost, so the hub resyncs.
        info!(
            device = %setup.device_id,
            path = ?setup.path,
            dialed_by_me = setup.dialed_by_me,
            replaced = was_up,
            "peer link up"
        );
        self.emit(PeerEvent::LinkUp { device_id: setup.device_id.clone(), machine: setup.machine });
        self.emit(PeerEvent::LinkPath { device_id: setup.device_id.clone(), path: setup.path });
        Some(Link {
            id,
            peer_addr: address(&setup.peer_pk),
            device_id: setup.device_id,
            peer_pk: setup.peer_pk,
            ctl_rx,
            data_rx,
            relay_rx,
            kill,
            queued_bytes,
            last_auth,
            reply: setup.reply,
        })
    }

    /// The running link changed path, or its peer re-dialed: record it and report `LinkUp` (resync)
    /// and `LinkPath`. False if the link is no longer the registered one or the device is gone.
    fn relink(&self, link: &Link, path: LinkPath) -> bool {
        // Under the gate, like `register`, so the events of the two stay in order.
        let _g = lock(&self.gate);
        let Some(entry) = self.roster.get(&link.device_id) else { return false };
        {
            let mut links = lock(&self.links);
            match links.get_mut(&link.device_id) {
                Some(l) if l.id == link.id => {
                    l.path = path;
                    if path == LinkPath::Relay {
                        l.dialed_by_me = false;
                    }
                }
                _ => return false,
            }
        }
        self.links_changed.send_modify(|v| *v = v.wrapping_add(1));
        info!(device = %link.device_id, ?path, "peer link up (path change or peer re-dialed)");
        let machine = Machine { last_seen: now_ms(), online: true, ..entry.machine };
        self.emit(PeerEvent::LinkUp { device_id: link.device_id.clone(), machine });
        self.emit(PeerEvent::LinkPath { device_id: link.device_id.clone(), path });
        true
    }

    /// The LAN socket is gone (or must go): move the link to the relay if it is up.
    fn fall_back(&self, link: &Link, lan_rx: &mut Option<mpsc::Receiver<String>>, out: &mut Out) -> Fallback {
        let Some(conn) = self.relay_conn() else { return Fallback::NoRelay };
        *lan_rx = None;
        std::mem::replace(out, Out::Relay(conn)).close(false);
        if self.relink(link, LinkPath::Relay) {
            Fallback::Moved
        } else {
            Fallback::Gone
        }
    }

    fn send_on(&self, out: &Out, link: &Link, body: &PeerBody) -> Result<(), SendFail> {
        out.send(&link.peer_addr, self.seal_for(link, body))
    }

    fn seal_for(&self, link: &Link, body: &PeerBody) -> PeerEnvelope {
        let seq = self.next_tx_seq(&link.device_id);
        seal(&self.keys, &self.me_id(), &link.device_id, &link.peer_pk, seq, body, false)
    }

    /// Presence is best effort: a full queue skips this beat instead of closing the link.
    fn send_presence(&self, out: &Out, link: &Link) -> bool {
        !matches!(self.send_on(out, link, &self.presence_body()), Err(SendFail::Closed))
    }

    async fn run_link(self: Arc<Self>, transport: Transport, mut link: Link) {
        let t = self.timings();
        let (mut lan_rx, mut out) = match transport {
            Transport::Lan(io) => {
                let (rx, tx) = io.split();
                (Some(rx), Out::Lan(tx))
            }
            Transport::Relay(conn) => (None, Out::Relay(conn)),
        };
        let mut ok = true;
        if let Some(reply) = link.reply.take() {
            ok = out.send_wait(&link.peer_addr, self.seal_for(&link, &reply)).await;
        }
        ok = ok && self.send_presence(&out, &link);
        let mut ticker = tokio::time::interval_at(Instant::now() + t.presence_interval, t.presence_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_auth = Instant::now();
        // Last authenticated frame on the LAN socket: a relay frame moves a LAN link to the relay only
        // once the LAN has been quiet for a presence interval (a frame the peer sent just before it
        // switched to the LAN is no reason to leave it).
        let mut lan_auth = Instant::now();
        // After falling back to the relay, the peer must be heard from by then.
        let mut fallback_by: Option<Instant> = None;
        let mut done: Option<oneshot::Sender<()>> = None;
        let mut revoked_by_peer = false;
        let mut superseded = false;
        let mut overflow = false;
        loop {
            if !ok {
                // The path stopped taking frames. On the relay that is the end of the session. A LAN
                // socket may still hold frames it read before it died (a bye, typically, whose close
                // ended our writer): handle those, then treat the socket as lost.
                if lan_rx.is_none() {
                    break;
                }
                if let Some(reason) = self.drain_lan(&link, &mut lan_rx) {
                    revoked_by_peer = self.on_bye(&link, reason);
                    break;
                }
                if self.fall_back(&link, &mut lan_rx, &mut out) != Fallback::Moved {
                    break;
                }
                info!(device = %link.device_id, "LAN socket lost; the link continues over the relay");
                fallback_by = Some(Instant::now() + t.handshake_timeout);
                ok = self.send_presence(&out, &link);
                continue;
            }
            // Bodies move from the link queue to the path's queue only while it has room, so a slow
            // peer backs up into the link queue (whose overflow closes the link, see `send`).
            let full = out.capacity() == 0;
            let quiet_at = last_auth + t.offline_after;
            let offline_at = fallback_by.map_or(quiet_at, |by| by.min(quiet_at));
            tokio::select! {
                _ = link.kill.notified() => {
                    warn!(device = %link.device_id, "peer link closed: outbound queue overflow");
                    overflow = true;
                    break;
                }
                cmd = link.ctl_rx.recv() => match cmd {
                    // Replaced by another link (the sender left the map).
                    None | Some(LinkCmd::Superseded) => {
                        superseded = true;
                        break;
                    }
                    Some(LinkCmd::Presence) => ok = self.send_presence(&out, &link),
                    Some(LinkCmd::Close { bye, done: d }) => {
                        if let Some(reason) = bye {
                            let _ = self.send_on(&out, &link, &PeerBody::Bye { reason, ts: now_ms() });
                        }
                        done = d;
                        break;
                    }
                    #[cfg(test)]
                    Some(LinkCmd::DropLan) => {
                        if lan_rx.is_some() {
                            info!(device = %link.device_id, "closing the LAN socket of this link");
                            if self.fall_back(&link, &mut lan_rx, &mut out) != Fallback::Moved {
                                break;
                            }
                            fallback_by = Some(Instant::now() + t.handshake_timeout);
                            ok = self.send_presence(&out, &link);
                        }
                    }
                },
                body = link.data_rx.recv(), if !full => match body {
                    None => {
                        superseded = true;
                        break;
                    }
                    Some(plaintext) => {
                        link.queued_bytes.fetch_sub(plaintext.len(), Ordering::SeqCst);
                        let seq = self.next_tx_seq(&link.device_id);
                        let env = seal_plaintext(&self.keys, &self.me_id(), &link.device_id, &link.peer_pk, seq, &plaintext, false);
                        ok = out.send_wait(&link.peer_addr, env).await;
                    }
                },
                // Wake when the queue drains a slot (the permit is dropped right away).
                room = out.room(), if full => ok = room,
                frame = recv_lan(&mut lan_rx) => match frame {
                    None => {
                        debug!(device = %link.device_id, "peer socket closed");
                        if self.fall_back(&link, &mut lan_rx, &mut out) != Fallback::Moved {
                            break;
                        }
                        info!(device = %link.device_id, "LAN socket lost; the link continues over the relay");
                        fallback_by = Some(Instant::now() + t.handshake_timeout);
                        ok = self.send_presence(&out, &link);
                    }
                    Some(text) => match self.on_frame(&link, &text) {
                        Frame::Ok => {
                            last_auth = Instant::now();
                            lan_auth = last_auth;
                            fallback_by = None;
                            link.last_auth.store(now_ms(), Ordering::Relaxed);
                        }
                        Frame::Dropped => {}
                        Frame::Bye(reason) => {
                            revoked_by_peer = self.on_bye(&link, reason);
                            break;
                        }
                    },
                },
                env = link.relay_rx.recv() => match env {
                    // The entry (holding the sender) left the map: replaced.
                    None => {
                        superseded = true;
                        break;
                    }
                    Some(env) => {
                        let Some(body) = self.check_frame(&link, &env) else { continue };
                        last_auth = Instant::now();
                        fallback_by = None;
                        link.last_auth.store(now_ms(), Ordering::Relaxed);
                        let hello = matches!(body, PeerBody::Hello { .. });
                        if lan_rx.is_some() {
                            // The peer is on the relay: it re-dialed there (hello), or it lost the LAN
                            // while our socket still looks open.
                            if hello || lan_auth.elapsed() > t.presence_interval {
                                match self.fall_back(&link, &mut lan_rx, &mut out) {
                                    Fallback::Moved => {
                                        info!(device = %link.device_id, "peer moved to the relay; following it");
                                        ok = self.send_presence(&out, &link);
                                    }
                                    Fallback::NoRelay => {}
                                    Fallback::Gone => break,
                                }
                            }
                        } else if hello {
                            // The peer re-dialed over the relay (it lost its link state): resync, answer.
                            if !self.relink(&link, LinkPath::Relay) {
                                break;
                            }
                            ok = self.send_presence(&out, &link);
                        }
                        if let Frame::Bye(reason) = self.on_body(&link.device_id, body) {
                            revoked_by_peer = self.on_bye(&link, reason);
                            break;
                        }
                    }
                },
                _ = out.lost() => {
                    info!(device = %link.device_id, "peer link lost its relay session");
                    break;
                }
                _ = ticker.tick() => ok = self.send_presence(&out, &link),
                _ = tokio::time::sleep_until(offline_at) => {
                    // Quiet on every path with the LAN socket still open: it may be half-open. Try the
                    // relay before giving up.
                    let moved = fallback_by.is_none()
                        && lan_rx.is_some()
                        && self.fall_back(&link, &mut lan_rx, &mut out) == Fallback::Moved;
                    if !moved {
                        if fallback_by.is_some() {
                            info!(device = %link.device_id, "peer did not answer over the relay");
                        } else {
                            info!(device = %link.device_id, "peer offline: no authenticated frame in {:?}", t.offline_after);
                        }
                        break;
                    }
                    info!(device = %link.device_id, "peer quiet on the LAN; trying the relay");
                    fallback_by = Some(Instant::now() + t.handshake_timeout);
                    ok = self.send_presence(&out, &link);
                }
            }
        }
        if superseded {
            if let Some(rx) = lan_rx.as_mut() {
                // Give the peer time to finish registering the winning link before this socket
                // closes, so its side never sees a gap (no LinkDown/LinkUp flap). Frames here are
                // ignored, except a bye{revoked}: a revoke sent while the two sides swap links must
                // not be lost.
                let grace = tokio::time::sleep(t.handshake_timeout);
                tokio::pin!(grace);
                loop {
                    tokio::select! {
                        f = rx.recv() => match f {
                            None => break,
                            Some(text) => {
                                if self.revoked_in_grace(&link, &text).await {
                                    revoked_by_peer = true;
                                    break;
                                }
                            }
                        },
                        _ = &mut grace => break,
                    }
                }
            }
        }
        out.close(overflow);
        let removed = {
            let mut links = lock(&self.links);
            if links.get(&link.device_id).is_some_and(|l| l.id == link.id) {
                links.remove(&link.device_id);
                true
            } else {
                false
            }
        };
        if removed {
            self.links_changed.send_modify(|v| *v = v.wrapping_add(1));
            info!(device = %link.device_id, "peer link down");
            self.emit(PeerEvent::LinkDown { device_id: link.device_id.clone() });
        }
        if revoked_by_peer {
            self.emit(PeerEvent::Revoked { device_id: link.device_id.clone(), by_peer: true });
        }
        if let Some(d) = done {
            let _ = d.send(());
        }
    }

    /// A frame on a running link: from/to fixed per link, opened with the link's roster key, seq
    /// strictly above the persisted last_rx (shared by both paths). None (dropped; the link stays up)
    /// otherwise.
    fn check_frame(&self, link: &Link, env: &PeerEnvelope) -> Option<PeerBody> {
        if env.from != link.device_id || env.to != self.me_id() || env.pk.is_some() {
            debug!(device = %link.device_id, "dropped misaddressed frame");
            return None;
        }
        if !self.seq_above_last(&link.device_id, env.seq) {
            debug!(device = %link.device_id, seq = env.seq, "dropped replayed frame");
            return None;
        }
        let body = match self.open_from(env, &link.peer_pk) {
            Ok(b) => b,
            Err(e) => {
                debug!(device = %link.device_id, error = %e, "dropped frame that did not open");
                return None;
            }
        };
        if stale_stamp(&body) {
            debug!(device = %link.device_id, "dropped a hello/presence stamped too far from our clock");
            return None;
        }
        if !self.accept_rx_seq(&link.device_id, env.seq) {
            debug!(device = %link.device_id, seq = env.seq, "dropped replayed frame");
            return None;
        }
        Some(body)
    }

    /// A text frame from the link's LAN socket.
    fn on_frame(&self, link: &Link, text: &str) -> Frame {
        let Ok(env) = serde_json::from_str::<PeerEnvelope>(text) else {
            debug!(device = %link.device_id, "dropped malformed frame");
            return Frame::Dropped;
        };
        match self.check_frame(link, &env) {
            Some(body) => self.on_body(&link.device_id, body),
            None => Frame::Dropped,
        }
    }

    /// An authenticated body from the device, on either path.
    fn on_body(&self, device_id: &str, body: PeerBody) -> Frame {
        let now = now_ms();
        match body {
            PeerBody::Presence { presence } => {
                if presence.machine.id != device_id {
                    debug!(device = %device_id, "ignored presence for another machine");
                    return Frame::Ok;
                }
                self.roster.update_seen(device_id, &presence.machine, None, now);
                self.emit(PeerEvent::Presence { device_id: device_id.to_string(), presence });
                Frame::Ok
            }
            PeerBody::Hello { machine, addr, .. } => {
                if machine.id == device_id {
                    let addr = addr.filter(|a| peer_addr_ok(a));
                    self.roster.update_seen(device_id, &machine, addr.as_deref(), now);
                }
                Frame::Ok
            }
            PeerBody::Bye { reason, .. } => Frame::Bye(reason),
            // Pairing bodies mean nothing on a live link.
            PeerBody::PairRequest { .. } | PeerBody::PairAccept { .. } | PeerBody::PairReject { .. } => Frame::Ok,
            // Everything else is the hub's (mirror, sync, ...). Never logged: it may carry user data.
            body => {
                self.emit(PeerEvent::Body { device_id: device_id.to_string(), body });
                Frame::Ok
            }
        }
    }

    /// Frames the LAN socket delivered before it died, handled in order. Returns a bye among them (the
    /// rest after it is ignored).
    fn drain_lan(&self, link: &Link, lan_rx: &mut Option<mpsc::Receiver<String>>) -> Option<ByeReason> {
        let rx = lan_rx.as_mut()?;
        while let Ok(text) = rx.try_recv() {
            if let Frame::Bye(reason) = self.on_frame(link, &text) {
                return Some(reason);
            }
        }
        None
    }

    /// A frame on a link that lost a swap: only an authentic bye{revoked} counts. Then the device is
    /// revoked here and the winning link closed (its LinkDown comes before our Revoked). True if so.
    async fn revoked_in_grace(&self, link: &Link, text: &str) -> bool {
        let Ok(env) = serde_json::from_str::<PeerEnvelope>(text) else { return false };
        let Some(PeerBody::Bye { reason: ByeReason::Revoked, .. }) = self.check_frame(link, &env) else { return false };
        self.on_bye(link, ByeReason::Revoked);
        let current = lock(&self.links).get(&link.device_id).map(|l| (l.ctl.clone(), l.kill.clone()));
        if let Some(handle) = current {
            self.close_link(handle, None).await;
        }
        true
    }

    /// The device told us, outside a live link, that it revoked us: revoke it here too.
    fn revoked_by_peer(&self, device_id: &str) {
        {
            let _g = lock(&self.gate);
            self.roster.revoke(device_id);
            self.stop_dialer(device_id);
        }
        self.sync_relay_allow();
        self.emit(PeerEvent::Revoked { device_id: device_id.to_string(), by_peer: true });
    }

    /// A frame from a device we revoked (it missed the revoke, e.g. it was offline then): if it is
    /// authentic (its stored key, and `sender_pk` too when the relay names one) and fresh, answer with
    /// `bye{revoked}` through `reply`, at most once per device per REVOKED_ANSWER_EVERY. The rate limit
    /// is checked before any crypto. `only_hello`: answer a hello only. True if answered.
    fn answer_revoked(
        &self,
        env: &PeerEnvelope,
        sender_pk: Option<&[u8; 32]>,
        only_hello: bool,
        reply: &dyn Fn(PeerEnvelope),
    ) -> bool {
        let answered_recently = |answers: &HashMap<String, Instant>, now: Instant| {
            answers.get(&env.from).is_some_and(|at| now.duration_since(*at) < REVOKED_ANSWER_EVERY)
        };
        if env.pk.is_some() || answered_recently(&lock(&self.revoked_answers), Instant::now()) {
            return false;
        }
        let Some(entry) = self.roster.get_revoked(&env.from) else { return false };
        let Some(pk) = parse_pk(&entry.device.public_key) else { return false };
        if sender_pk.is_some_and(|s| *s != pk) || !self.seq_above_last(&env.from, env.seq) {
            return false;
        }
        let Ok(body) = self.open_from(env, &pk) else { return false };
        let wanted = match body {
            _ if stale_stamp(&body) => false,
            PeerBody::Hello { .. } => true,
            PeerBody::Bye { .. } | PeerBody::PairRequest { .. } | PeerBody::PairAccept { .. } | PeerBody::PairReject { .. } => {
                false
            }
            _ => !only_hello,
        };
        // A replayed frame gets nothing (and the counter never goes down, even for a revoked row).
        if !wanted || !self.accept_rx_seq(&env.from, env.seq) {
            return false;
        }
        {
            let mut answers = lock(&self.revoked_answers);
            let now = Instant::now();
            if answered_recently(&answers, now) {
                return false;
            }
            answers.retain(|_, at| now.duration_since(*at) < REVOKED_ANSWER_EVERY);
            if answers.len() >= MAX_REVOKED_ANSWERS {
                return false;
            }
            answers.insert(env.from.clone(), now);
        }
        info!(device = %env.from, "revoked device came back; telling it with bye(revoked)");
        let bye = PeerBody::Bye { reason: ByeReason::Revoked, ts: now_ms() };
        reply(seal(&self.keys, &self.me_id(), &env.from, &pk, self.next_tx_seq(&env.from), &bye, false));
        true
    }

    /// The peer said bye on this link. True if it revoked us (now revoked here too).
    fn on_bye(&self, link: &Link, reason: ByeReason) -> bool {
        info!(device = %link.device_id, ?reason, "peer said bye");
        if reason != ByeReason::Revoked {
            return false;
        }
        {
            let _g = lock(&self.gate);
            self.roster.revoke(&link.device_id);
            self.stop_dialer(&link.device_id);
        }
        self.sync_relay_allow();
        true
    }

    /// Close a link (with `bye` if given), waiting until it is cleaned up (its LinkDown emitted). The
    /// control queue drains quickly; if it does not, the link is killed without bye.
    async fn close_link(&self, (ctl, kill): (mpsc::Sender<LinkCmd>, Arc<Notify>), bye: Option<ByeReason>) {
        let (tx, rx) = oneshot::channel();
        let sent = timeout(CLOSE_WAIT, ctl.send(LinkCmd::Close { bye, done: Some(tx) })).await;
        if matches!(sent, Ok(Ok(()))) {
            let _ = timeout(CLOSE_WAIT, rx).await;
        } else {
            kill.notify_one();
        }
    }

    async fn revoke(&self, device_id: &str) {
        let link = {
            let _g = lock(&self.gate);
            let known = self.roster.get(device_id).is_some();
            let link = lock(&self.links).get(device_id).map(|l| (l.ctl.clone(), l.kill.clone()));
            if !known && link.is_none() {
                return;
            }
            self.roster.revoke(device_id);
            link
        };
        self.stop_dialer(device_id);
        if let Some(handle) = link {
            self.close_link(handle, Some(ByeReason::Revoked)).await;
        }
        // After the bye: it went to the peer through the relay before the peer leaves our allow list.
        self.sync_relay_allow();
        info!(device = device_id, "device revoked");
        self.emit(PeerEvent::Revoked { device_id: device_id.to_string(), by_peer: false });
    }

    async fn stop_listening(&self) {
        let Some(mut l) = lock(&self.listener).take() else { return };
        let _ = l.stop.send(true);
        // The server task owns the TCP listener: once it is done the port is free.
        if timeout(CLOSE_WAIT, &mut l.task).await.is_err() {
            l.task.abort();
            let _ = (&mut l.task).await;
        }
        let accepted = self.accepted_links();
        futures::future::join_all(accepted.into_iter().map(|h| self.close_link(h, None))).await;
        info!(addr = %l.addr, "peer listener off");
    }

    /// Live links our listener accepted (LAN sockets the peer dialed).
    fn accepted_links(&self) -> Vec<(mpsc::Sender<LinkCmd>, Arc<Notify>)> {
        lock(&self.links)
            .values()
            .filter(|l| l.path == LinkPath::Lan && !l.dialed_by_me)
            .map(|l| (l.ctl.clone(), l.kill.clone()))
            .collect()
    }

    /// `stop_listening` without waiting (for a handshake that finished after the listener stopped).
    fn close_accepted_now(&self) {
        for (ctl, kill) in self.accepted_links() {
            if ctl.try_send(LinkCmd::Close { bye: None, done: None }).is_err() {
                kill.notify_one();
            }
        }
    }

    async fn shutdown(&self) {
        if self.closing.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.shutdown_tx.send(true);
        for (_, h) in lock(&self.dialers).drain() {
            h.abort();
        }
        let links: Vec<(mpsc::Sender<LinkCmd>, Arc<Notify>)> =
            lock(&self.links).values().map(|l| (l.ctl.clone(), l.kill.clone())).collect();
        futures::future::join_all(links.into_iter().map(|h| self.close_link(h, Some(ByeReason::Shutdown)))).await;
        lock(&self.invites).clear();
        // After the links: their byes over the relay are flushed before it closes.
        self.stop_relay().await;
        let listener = lock(&self.listener).take();
        if let Some(mut l) = listener {
            if timeout(CLOSE_WAIT, &mut l.task).await.is_err() {
                l.task.abort();
                let _ = l.task.await;
            }
            info!(addr = %l.addr, "peer listener off");
        }
    }
}

async fn reject_origin(req: Request, next: Next) -> Response {
    // Browsers always send Origin on WebSocket handshakes; peers never do.
    if req.headers().contains_key(header::ORIGIN) {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(req).await
}

async fn not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

async fn ws_handler(
    State(inner): State<Arc<Inner>>,
    ConnectInfo(conn): ConnectInfo<PeerConn>,
    ws: WebSocketUpgrade,
) -> Response {
    // Connection caps and the unauthenticated deadline are enforced by `PeerListener`.
    if inner.is_closing() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    ws.max_message_size(MAX_FRAME)
        .max_frame_size(MAX_FRAME)
        .on_upgrade(move |socket| inner.handle_inbound(spawn_io(socket), conn))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
impl PeerNode {
    pub(crate) async fn test_dial_once(&self, device_id: &str) -> bool {
        let Some(addr) = self.inner.roster.get(device_id).and_then(|e| e.device.addr) else { return false };
        self.inner.dial_once(device_id, &addr).await.is_ok()
    }

    pub(crate) fn test_queued_bytes(&self, device_id: &str) -> Option<usize> {
        lock(&self.inner.links).get(device_id).map(|l| l.queued_bytes.load(Ordering::SeqCst))
    }

    pub(crate) fn test_opens(&self) -> u64 {
        self.inner.opens.load(Ordering::Relaxed)
    }

    /// Close the device's LAN socket as if the network dropped it.
    pub(crate) fn test_drop_lan(&self, device_id: &str) -> bool {
        lock(&self.inner.links).get(device_id).is_some_and(|l| l.ctl.try_send(LinkCmd::DropLan).is_ok())
    }

    pub(crate) fn test_link_dialed_by_me(&self, device_id: &str) -> Option<bool> {
        lock(&self.inner.links).get(device_id).map(|l| l.dialed_by_me)
    }

    pub(crate) fn test_live_invites(&self) -> usize {
        let _ = self.inner.has_live_invite();
        lock(&self.inner.invites).len()
    }

    pub(crate) fn test_clear_pair_limits(&self) {
        lock(&self.inner.pair_limiter).clear();
    }

    pub(crate) fn test_expire_invites(&self) {
        for i in lock(&self.inner.invites).iter_mut() {
            i.expires_at = now_ms() - 1;
        }
    }
}
