//! The peer node: listener, dialer, pairing and live links.
//!
//! Lock order (never take an earlier one while holding a later one): `gate` -> roster (the hub's own
//! storage lock) -> `links`/`dialers`/`invites`. Roster methods are never called while `links` is held,
//! so the hub may call any `PeerNode` method while holding its storage lock, except `revoke`, `join`,
//! `connect_known` and `shutdown`, which call into the roster.

use crate::envelope::{parse_pk, seal_plaintext};
use crate::invite::{new_code, peer_addr_ok, valid_addr, CODE_LEN};
use crate::io::{spawn_io, SendFail, WsIo, MAX_FRAME};
use crate::keys::wipe;
use crate::limits::{ConnLimits, PairLimiter, PeerConn, PeerListener};
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
    is_valid_machine_id, now_ms, ByeReason, Device, DeviceKind, Machine, PairRejectReason, PeerBody, PeerEnvelope,
    Presence, UnixMs, MIRROR_FRAME_MAX_BYTES, PEER_PATH, PRESENCE_INTERVAL_MS, PRESENCE_OFFLINE_MS,
};
use std::collections::HashMap;
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

/// Whole `join` (dial + pair_request + reply).
const JOIN_TIMEOUT: Duration = Duration::from_secs(10);
/// Live invites kept at once (the oldest is dropped beyond this).
const MAX_INVITES: usize = 16;
const INVITE_TTL_MIN: Duration = Duration::from_secs(60);
const INVITE_TTL_MAX: Duration = Duration::from_secs(3600);
/// Bodies queued per link for `send` (frames waiting for the socket). Overflow closes the link.
const LINK_QUEUE: usize = 1024;
/// Serialized body bytes queued per link for `send`. Exceeding it is handled like LINK_QUEUE overflow.
const LINK_QUEUE_BYTES: usize = 16 * 1024 * 1024;
/// Control commands (presence push, close) queued per link.
const CTL_QUEUE: usize = 16;
/// Largest serialized body `PeerNode::send` accepts: MIRROR_FRAME_MAX_BYTES plus 16 KiB headroom.
pub const MAX_SEND_BODY_BYTES: usize = MIRROR_FRAME_MAX_BYTES + 16 * 1024;
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
    /// A link to the device became live: the device was offline, or a new link replaced the current
    /// one (re-pair, reconnect, duplicate-link tie-break). Every `Body` is preceded by the `LinkUp`
    /// of the link it arrived on, so (re)sync on every `LinkUp`; it is idempotent for an online device.
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
    /// Bumped on every link added or removed; dial loops wait on it.
    links_changed: watch::Sender<u64>,
    dialers: Mutex<HashMap<String, JoinHandle<()>>>,
    /// Connection caps and the unauthenticated deadline of the listener.
    conn_limits: Arc<ConnLimits>,
    /// Failed pair attempts per remote IP, and `PairRejected` coalescing.
    pair_limiter: Mutex<PairLimiter>,
    /// Devices the hub disconnected: until the instant, no dialing and no hello accepted.
    cooldowns: Mutex<HashMap<String, Instant>>,
    /// Serializes "is this device still paired?" against revocation (see module docs).
    gate: Mutex<()>,
    seq_lock: Mutex<()>,
    rx_lock: Mutex<()>,
    next_link_id: AtomicU64,
    closing: AtomicBool,
    shutdown_tx: watch::Sender<bool>,
}

/// An invite code held in memory only. Wiped on drop.
struct LiveInvite {
    code: [u8; CODE_LEN],
    expires_at: UnixMs,
    addr: String,
}

impl Drop for LiveInvite {
    fn drop(&mut self) {
        wipe(&mut self.code);
    }
}

struct ListenerState {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

struct LinkEntry {
    id: u64,
    dialed_by_me: bool,
    ctl: mpsc::Sender<LinkCmd>,
    /// Serialized bodies from `send`.
    data: mpsc::Sender<Vec<u8>>,
    /// Close now, without bye (outbound queue overflow).
    kill: Arc<Notify>,
    /// Serialized bytes of the bodies waiting in `data`.
    queued_bytes: Arc<AtomicUsize>,
    last_auth: Arc<AtomicI64>,
}

enum LinkCmd {
    Presence,
    /// Close the link, after sending `bye` if given. `done` fires after the link is cleaned up
    /// (its `LinkDown` emitted).
    Close { bye: Option<ByeReason>, done: Option<oneshot::Sender<()>> },
    /// Another link to the same device won (tie-break or re-pair). Stop using this one, let the
    /// peer register the winner, then close quietly (no `LinkDown`: the device is still linked).
    Superseded,
}

/// A handshake that succeeded, ready to become a live link.
struct LinkSetup {
    device_id: String,
    peer_pk: [u8; 32],
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
    ctl_rx: mpsc::Receiver<LinkCmd>,
    data_rx: mpsc::Receiver<Vec<u8>>,
    kill: Arc<Notify>,
    queued_bytes: Arc<AtomicUsize>,
    last_auth: Arc<AtomicI64>,
    reply: Option<PeerBody>,
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

/// Uniform in [d/2, d).
fn jitter(d: Duration) -> Duration {
    let half = d / 2;
    let span = half.as_nanos().max(1) as u64;
    half + Duration::from_nanos(getrandom::u64().unwrap_or(0) % span)
}

impl PeerNode {
    pub fn new(keys: DeviceKeys, me: Machine, roster: Arc<dyn Roster>, events: UnboundedSender<PeerEvent>) -> Self {
        let (links_changed, _) = watch::channel(0u64);
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
                gate: Mutex::new(()),
                seq_lock: Mutex::new(()),
                rx_lock: Mutex::new(()),
                next_link_id: AtomicU64::new(1),
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
    /// listener binds 0.0.0.0 / ::). `ttl` must be 60..=3600 s.
    pub fn create_invite(&self, advertise: Option<String>, ttl: Duration) -> Result<Invite, InviteError> {
        self.inner.create_invite(advertise, ttl)
    }

    /// Pair with the hub that issued `uri`. On success the link is live and kept up.
    pub async fn join(&self, uri: &str) -> Result<(Device, Machine), JoinError> {
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

    /// Idempotent: start a dial loop for every roster hub with an addr that has none running.
    pub fn connect_known(&self) {
        if tokio::runtime::Handle::try_current().is_err() {
            warn!("connect_known called outside a Tokio runtime; ignored");
            return;
        }
        for entry in self.inner.roster.list() {
            if entry.device.kind == DeviceKind::Hub && entry.device.addr.is_some() {
                self.inner.ensure_dialer(&entry.device.id);
            }
        }
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

    /// Queue a body on the device's live link (in order). `TooLarge` bodies are never sent; on
    /// `Backpressure` (LINK_QUEUE frames already waiting) the link is closed and the peer resyncs when
    /// it reconnects. Never blocks.
    pub fn send(&self, device_id: &str, body: PeerBody) -> Result<(), SendError> {
        self.inner.send(device_id, &body)
    }

    /// Revoke a device: `bye{revoked}` on its live link, close it, mark it revoked, stop dialing.
    /// No-op for an id that is neither paired nor linked.
    pub async fn revoke(&self, device_id: &str) {
        self.inner.revoke(device_id).await
    }

    /// Linked device ids and the time of their last authenticated frame.
    /// Close the device's live link without revoking it and without bye (`LinkDown` follows). For
    /// `cooldown` we neither dial it nor accept its hello (closed silently); then the dialer resumes
    /// with its usual backoff. A new pairing with the device ends the cooldown. No-op for an id that is
    /// neither paired nor linked.
    pub fn disconnect(&self, device_id: &str, cooldown: Duration) {
        self.inner.disconnect(device_id, cooldown)
    }

    pub fn online(&self) -> Vec<(String, UnixMs)> {
        let mut v: Vec<(String, UnixMs)> =
            lock(&self.inner.links).iter().map(|(id, l)| (id.clone(), l.last_auth.load(Ordering::Relaxed))).collect();
        v.sort();
        v
    }

    /// `bye{shutdown}` to every link, stop dialing, stop the listener. Idempotent.
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

    fn seal_text(&self, to: &str, to_pk: &[u8; 32], body: &PeerBody) -> String {
        let seq = self.next_tx_seq(to);
        let env = seal(&self.keys, &self.me_id(), to, to_pk, seq, body, false);
        serde_json::to_string(&env).expect("PeerEnvelope serializes")
    }

    // ---------- invites ----------

    fn create_invite(&self, advertise: Option<String>, ttl: Duration) -> Result<Invite, InviteError> {
        let Some(bound) = self.listener_addr() else { return Err(InviteError::ListenerOff) };
        if !(INVITE_TTL_MIN..=INVITE_TTL_MAX).contains(&ttl) {
            return Err(InviteError::Invalid);
        }
        let explicit = advertise.is_some();
        let addr = match advertise {
            Some(a) => {
                let a = a.trim().to_string();
                if !valid_addr(&a) {
                    return Err(InviteError::Invalid);
                }
                a
            }
            None if bound.ip().is_unspecified() => return Err(InviteError::Invalid),
            None => bound.to_string(),
        };
        let (code_bytes, code) = new_code();
        let expires_at = now_ms() + ttl.as_millis() as i64;
        {
            let mut invites = lock(&self.invites);
            let now = now_ms();
            invites.retain(|i| i.expires_at > now);
            if invites.len() >= MAX_INVITES {
                invites.remove(0);
            }
            invites.push(LiveInvite { code: code_bytes, expires_at, addr: addr.clone() });
        }
        if explicit {
            *lock(&self.advertise) = Some(addr.clone());
        }
        info!(addr = %addr, expires_at, "peer invite created");
        Ok(Invite { machine_id: self.me_id(), public_key: self.keys.public_key(), addr, code, expires_at })
    }

    fn has_live_invite(&self) -> bool {
        let mut invites = lock(&self.invites);
        let now = now_ms();
        invites.retain(|i| i.expires_at > now);
        !invites.is_empty()
    }

    /// Constant-time match against every live invite. A match consumes that invite and returns its
    /// advertised addr. A miss consumes nothing: anyone who knows our public key can send a wrong
    /// code, so burning invites would let them block pairing. Guessing is the rate limiter's problem
    /// (and infeasible against 128-bit codes).
    fn consume_invite(&self, code: &str) -> Option<String> {
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
        let mut stop = self.shutdown_tx.subscribe();
        let task = tokio::spawn(async move {
            let server = axum::serve(listener, app.into_make_service_with_connect_info::<PeerConn>())
                .with_graceful_shutdown(async move { closed(&mut stop).await });
            if let Err(e) = server.await {
                warn!(error = %e, "peer listener stopped");
            }
        });
        let mut slot = lock(&self.listener);
        if slot.is_some() {
            task.abort();
            bail!("peer listener already running");
        }
        *slot = Some(ListenerState { addr: bound, task });
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
        let setup = if env.pk.is_some() { self.inbound_pair(&io, env, remote) } else { self.inbound_hello(env, remote) };
        let Some(setup) = setup else { return io.close() };
        let device_id = setup.device_id.clone();
        let Some(link) = self.register(setup) else { return io.close() };
        // Authenticated: lift the connection deadline and free the unauthenticated slot.
        conn.shared.authenticated();
        // Keep the link up from our side too when the peer has a listener.
        if self.roster.get(&device_id).is_some_and(|e| e.device.addr.is_some()) {
            self.ensure_dialer(&device_id);
        }
        self.run_link(io, link).await
    }

    fn inbound_pair(&self, io: &WsIo, env: PeerEnvelope, remote: SocketAddr) -> Option<LinkSetup> {
        if lock(&self.pair_limiter).blocked(remote.ip(), std::time::Instant::now()) {
            debug!(remote = %remote, "pair attempt dropped: too many failures from this address");
            return None;
        }
        if !self.has_live_invite() {
            debug!(remote = %remote, "pair attempt closed: no live invite");
            return None;
        }
        let pk = parse_pk(env.pk.as_deref()?)?;
        if pk == self.keys.public_key() {
            return None;
        }
        let me_id = self.me_id();
        let reject = |reason: PairRejectReason| -> Option<LinkSetup> {
            let failure = lock(&self.pair_limiter).failure(remote.ip(), std::time::Instant::now());
            if failure.blocked {
                warn!(remote = %remote, "too many failed pair attempts; ignoring this address for a while");
            }
            // At most one event (audit entry) per address per minute.
            if let Some(coalesced) = failure.emit {
                info!(remote = %remote, ?reason, coalesced, "pair attempt rejected");
                self.emit(PeerEvent::PairRejected { remote_addr: remote.to_string(), reason });
            } else {
                debug!(remote = %remote, ?reason, "pair attempt rejected (event coalesced)");
            }
            // Reject replies do not touch the roster's seq counters (the sender is not paired).
            let body = PeerBody::PairReject { reason, ts: now_ms() };
            let env = seal(&self.keys, &me_id, &env.from, &pk, time_seq(), &body, false);
            let _ = io.send(serde_json::to_string(&env).expect("PeerEnvelope serializes"));
            None
        };
        let Ok(body) = open(&self.keys, &env, &pk) else { return reject(PairRejectReason::BadRequest) };
        let PeerBody::PairRequest { code, machine, addr, .. } = body else {
            return reject(PairRejectReason::BadRequest);
        };
        if machine.id != env.from || !is_valid_machine_id(&env.from) || env.from == me_id {
            return reject(PairRejectReason::BadRequest);
        }
        // Never move last_rx backwards (that would re-open old frames from this id to replay), even
        // for a revoked or re-pairing device. Checked before the code so a mismatch costs no invite.
        if env.seq <= self.roster.last_rx_seq(&env.from) {
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
        };
        let online = Machine { last_seen: now, online: true, ..machine };
        self.roster.upsert(RosterEntry { device: device.clone(), machine: Machine { online: false, ..online.clone() } });
        // Above the stored value (checked), so this never lowers it.
        self.roster.set_last_rx_seq(&device.id, env.seq);
        info!(device = %device.id, remote = %remote, "paired (we invited)");
        self.emit(PeerEvent::Paired { device: device.clone(), machine: online.clone(), we_invited: true });
        Some(LinkSetup {
            device_id: device.id,
            peer_pk: pk,
            dialed_by_me: false,
            machine: online,
            force: true,
            // req_seq ties the accept to this request: an old captured accept cannot be replayed.
            reply: Some(PeerBody::PairAccept { machine: self.me(), addr: Some(invite_addr), req_seq: env.seq, ts: now }),
        })
    }

    fn inbound_hello(&self, env: PeerEnvelope, remote: SocketAddr) -> Option<LinkSetup> {
        if self.cooldown_until(&env.from).is_some() {
            debug!(remote = %remote, device = %env.from, "peer socket closed: device is cooling down after a disconnect");
            return None;
        }
        let Some(entry) = self.roster.get(&env.from) else {
            debug!(remote = %remote, "peer socket closed: unknown or revoked sender");
            return None;
        };
        let pk = parse_pk(&entry.device.public_key)?;
        let Ok(PeerBody::Hello { machine, addr, .. }) = open(&self.keys, &env, &pk) else {
            debug!(remote = %remote, device = %env.from, "peer socket closed: hello did not authenticate");
            return None;
        };
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

    async fn join(self: Arc<Self>, invite: Invite) -> Result<(Device, Machine), JoinError> {
        let mut io = Self::connect(&invite.addr, JOIN_TIMEOUT).await.map_err(|e| match e {
            DialError::Unreachable => JoinError::Unreachable,
            DialError::Timeout => JoinError::Timeout,
        })?;
        let mid = invite.machine_id.clone();
        let me_id = self.me_id();
        let req_seq = self.next_tx_seq(&mid);
        let req = PeerBody::PairRequest {
            code: invite.code.clone(),
            machine: self.me(),
            addr: self.advertised_addr(),
            ts: now_ms(),
        };
        let env = seal(&self.keys, &me_id, &mid, &invite.public_key, req_seq, &req, true);
        if io.send(serde_json::to_string(&env).expect("PeerEnvelope serializes")).is_err() {
            return Err(JoinError::Unreachable);
        }
        // Closed without a reply: no live invite there (expired or used), or not a Krypto listener.
        let Some(text) = io.rx.recv().await else { return Err(JoinError::Rejected) };
        let reply = serde_json::from_str::<PeerEnvelope>(&text).map_err(|_| JoinError::Rejected)?;
        if reply.from != mid || reply.to != me_id || reply.pk.is_some() {
            return Err(JoinError::Rejected);
        }
        // Only the holder of the URI's key can produce this.
        let body = open(&self.keys, &reply, &invite.public_key).map_err(|_| JoinError::Rejected)?;
        let PeerBody::PairAccept { machine, req_seq: echoed, .. } = body else {
            if let PeerBody::PairReject { reason, .. } = body {
                info!(device = %mid, ?reason, "pairing rejected by inviter");
            }
            return Err(JoinError::Rejected);
        };
        // The accept must answer this very request (an old captured accept carries an old req_seq),
        // and must not move last_rx backwards, even for a revoked or unknown row.
        if machine.id != mid || echoed != req_seq || reply.seq <= self.roster.last_rx_seq(&mid) {
            debug!(device = %mid, "pair_accept rejected: identity, req_seq or seq check failed");
            return Err(JoinError::Rejected);
        }
        let now = now_ms();
        let device = Device {
            id: mid.clone(),
            kind: DeviceKind::Hub,
            name: machine.name.clone(),
            public_key: b64::encode(&invite.public_key),
            paired_at: now,
            last_seen: Some(now),
            addr: Some(invite.addr.clone()),
        };
        let online = Machine { last_seen: now, online: true, ..machine };
        self.roster.upsert(RosterEntry { device: device.clone(), machine: Machine { online: false, ..online.clone() } });
        self.roster.set_last_rx_seq(&mid, reply.seq);
        self.roster.set_last_tx_seq(&mid, req_seq);
        info!(device = %mid, "paired (we joined)");
        self.emit(PeerEvent::Paired { device: device.clone(), machine: online.clone(), we_invited: false });
        let setup = LinkSetup {
            device_id: mid.clone(),
            peer_pk: invite.public_key,
            dialed_by_me: true,
            machine: online.clone(),
            force: true,
            reply: None,
        };
        if let Some(link) = self.register(setup) {
            tokio::spawn(self.clone().run_link(io, link));
        }
        self.ensure_dialer(&mid);
        Ok((device, online))
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

    fn has_link(&self, device_id: &str) -> bool {
        lock(&self.links).contains_key(device_id)
    }

    /// Keeps one link up to a roster hub: dials while there is no live link (in either direction),
    /// backing off `backoff_min` -> `backoff_max` with jitter. Ends when the device is revoked, loses its
    /// addr, or the node shuts down.
    async fn dial_loop(self: Arc<Self>, device_id: String) {
        let mut changed = self.links_changed.subscribe();
        let mut shutdown = self.shutdown_tx.subscribe();
        let mut backoff = self.timings().backoff_min;
        let mut delay: Option<Duration> = None;
        let mut linked_at: Option<Instant> = None;
        loop {
            let t = self.timings();
            if self.is_closing() {
                return;
            }
            let Some(entry) = self.roster.get(&device_id) else { return };
            let Some(addr) = entry.device.addr else { return };
            if let Some(until) = self.cooldown_until(&device_id) {
                tokio::select! {
                    _ = tokio::time::sleep_until(until) => {}
                    _ = closed(&mut shutdown) => return,
                }
                continue;
            }
            changed.borrow_and_update();
            if self.has_link(&device_id) {
                tokio::select! {
                    r = changed.changed() => if r.is_err() { return },
                    _ = closed(&mut shutdown) => return,
                }
                continue;
            }
            if let Some(at) = linked_at.take() {
                // Our link just ended: redial soon if it was stable, else keep backing off.
                backoff = if at.elapsed() >= t.offline_after { t.backoff_min } else { (backoff * 2).min(t.backoff_max) };
                delay = Some(backoff);
            }
            if let Some(d) = delay.take() {
                tokio::select! {
                    _ = tokio::time::sleep(jitter(d)) => {}
                    _ = closed(&mut shutdown) => return,
                }
                continue;
            }
            match self.dial_once(&device_id, &addr).await {
                Ok(()) => linked_at = Some(Instant::now()),
                Err(()) => {
                    delay = Some(backoff);
                    backoff = (backoff * 2).min(t.backoff_max);
                }
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
        let Ok(PeerBody::Hello { machine, addr: their_addr, .. }) = open(&self.keys, &env, &pk) else { return Err(()) };
        if machine.id != device_id || !self.accept_rx_seq(device_id, env.seq) {
            return Err(());
        }
        let now = now_ms();
        let their_addr = their_addr.filter(|a| peer_addr_ok(a));
        self.roster.update_seen(device_id, &machine, their_addr.as_deref(), now);
        let setup = LinkSetup {
            device_id: device_id.to_string(),
            peer_pk: pk,
            dialed_by_me: true,
            machine: Machine { last_seen: now, online: true, ..machine },
            force: false,
            reply: None,
        };
        let link = self.register(setup).ok_or(())?;
        tokio::spawn(self.clone().run_link(io, link));
        Ok(())
    }

    // ---------- links ----------

    /// Register a handshaken link. Two links to one peer: keep the one dialed by the hub with the
    /// smaller machine id (both sides decide the same way); a reconnect by the same dialer, or a
    /// pairing link, replaces the old one. None: the new link lost, the device is no longer paired,
    /// or the node is closing.
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
        let kill = Arc::new(Notify::new());
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let last_auth = Arc::new(AtomicI64::new(now_ms()));
        let id = self.next_link_id.fetch_add(1, Ordering::Relaxed);
        let was_up = {
            let mut links = lock(&self.links);
            let was_up = match links.get(&setup.device_id) {
                None => false,
                Some(old) => {
                    let new_wins = setup.force
                        || old.dialed_by_me == setup.dialed_by_me
                        || setup.dialed_by_me == (me_id.as_str() < setup.device_id.as_str());
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
                    dialed_by_me: setup.dialed_by_me,
                    ctl: ctl_tx,
                    data: data_tx,
                    kill: kill.clone(),
                    queued_bytes: queued_bytes.clone(),
                    last_auth: last_auth.clone(),
                },
            );
            was_up
        };
        self.links_changed.send_modify(|v| *v = v.wrapping_add(1));
        // Also over an existing link: bodies queued on the replaced link may be lost, so the hub resyncs.
        info!(device = %setup.device_id, dialed_by_me = setup.dialed_by_me, replaced = was_up, "peer link up");
        self.emit(PeerEvent::LinkUp { device_id: setup.device_id.clone(), machine: setup.machine });
        Some(Link {
            id,
            device_id: setup.device_id,
            peer_pk: setup.peer_pk,
            ctl_rx,
            data_rx,
            kill,
            queued_bytes,
            last_auth,
            reply: setup.reply,
        })
    }

    fn send_on(&self, io: &WsIo, link: &Link, body: &PeerBody) -> Result<(), SendFail> {
        io.send(self.seal_text(&link.device_id, &link.peer_pk, body))
    }

    /// Presence is best effort: a full socket queue skips this beat instead of closing the link.
    fn send_presence(&self, io: &WsIo, link: &Link) -> bool {
        !matches!(self.send_on(io, link, &self.presence_body()), Err(SendFail::Closed))
    }

    async fn run_link(self: Arc<Self>, io: WsIo, mut link: Link) {
        let mut io = io;
        let t = self.timings();
        let mut ok = true;
        if let Some(reply) = link.reply.take() {
            ok = self.send_on(&io, &link, &reply).is_ok();
        }
        ok = ok && self.send_presence(&io, &link);
        let mut ticker = tokio::time::interval_at(Instant::now() + t.presence_interval, t.presence_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_auth = Instant::now();
        let mut done: Option<oneshot::Sender<()>> = None;
        let mut revoked_by_peer = false;
        let mut superseded = false;
        let mut overflow = false;
        while ok {
            // Bodies move from the link queue to the socket queue only while the socket has room, so a
            // slow peer backs up into the link queue (whose overflow closes the link, see `send`).
            let socket_full = io.tx.capacity() == 0;
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
                    Some(LinkCmd::Presence) => ok = self.send_presence(&io, &link),
                    Some(LinkCmd::Close { bye, done: d }) => {
                        if let Some(reason) = bye {
                            let _ = self.send_on(&io, &link, &PeerBody::Bye { reason, ts: now_ms() });
                        }
                        done = d;
                        break;
                    }
                },
                body = link.data_rx.recv(), if !socket_full => match body {
                    None => {
                        superseded = true;
                        break;
                    }
                    Some(plaintext) => {
                        link.queued_bytes.fetch_sub(plaintext.len(), Ordering::SeqCst);
                        let seq = self.next_tx_seq(&link.device_id);
                        let env = seal_plaintext(&self.keys, &self.me_id(), &link.device_id, &link.peer_pk, seq, &plaintext, false);
                        ok = io.send(serde_json::to_string(&env).expect("PeerEnvelope serializes")).is_ok();
                    }
                },
                // Wake when the socket drains a slot (the permit is dropped right away).
                room = io.tx.reserve(), if socket_full => ok = room.is_ok(),
                frame = io.rx.recv() => match frame {
                    None => {
                        debug!(device = %link.device_id, "peer socket closed");
                        break;
                    }
                    Some(text) => match self.on_frame(&link, &text) {
                        Frame::Ok => {
                            last_auth = Instant::now();
                            link.last_auth.store(now_ms(), Ordering::Relaxed);
                        }
                        Frame::Dropped => {}
                        Frame::Bye(reason) => {
                            info!(device = %link.device_id, ?reason, "peer said bye");
                            if reason == ByeReason::Revoked {
                                let _g = lock(&self.gate);
                                self.roster.revoke(&link.device_id);
                                self.stop_dialer(&link.device_id);
                                revoked_by_peer = true;
                            }
                            break;
                        }
                    },
                },
                _ = ticker.tick() => ok = self.send_presence(&io, &link),
                _ = tokio::time::sleep_until(last_auth + t.offline_after) => {
                    info!(device = %link.device_id, "peer offline: no authenticated frame in {:?}", t.offline_after);
                    break;
                }
            }
        }
        if superseded {
            // Give the peer time to finish registering the winning link before this socket closes,
            // so its side never sees a gap (no LinkDown/LinkUp flap). Frames here are ignored.
            let grace = tokio::time::sleep(t.handshake_timeout);
            tokio::pin!(grace);
            loop {
                tokio::select! {
                    f = io.rx.recv() => if f.is_none() { break },
                    _ = &mut grace => break,
                }
            }
        }
        if overflow {
            io.abort();
        } else {
            io.close();
        }
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

    /// Every frame after the first: from/to fixed per link, opened with the link's roster key, seq
    /// strictly above the persisted last_rx. Anything else is dropped (the link stays up).
    fn on_frame(&self, link: &Link, text: &str) -> Frame {
        let Ok(env) = serde_json::from_str::<PeerEnvelope>(text) else {
            debug!(device = %link.device_id, "dropped malformed frame");
            return Frame::Dropped;
        };
        if env.from != link.device_id || env.to != self.me_id() || env.pk.is_some() {
            debug!(device = %link.device_id, "dropped misaddressed frame");
            return Frame::Dropped;
        }
        let body = match open(&self.keys, &env, &link.peer_pk) {
            Ok(b) => b,
            Err(e) => {
                debug!(device = %link.device_id, error = %e, "dropped frame that did not open");
                return Frame::Dropped;
            }
        };
        if !self.accept_rx_seq(&link.device_id, env.seq) {
            debug!(device = %link.device_id, seq = env.seq, "dropped replayed frame");
            return Frame::Dropped;
        }
        let now = now_ms();
        match body {
            PeerBody::Presence { presence } => {
                if presence.machine.id != link.device_id {
                    debug!(device = %link.device_id, "ignored presence for another machine");
                    return Frame::Ok;
                }
                self.roster.update_seen(&link.device_id, &presence.machine, None, now);
                self.emit(PeerEvent::Presence { device_id: link.device_id.clone(), presence });
                Frame::Ok
            }
            PeerBody::Hello { machine, addr, .. } => {
                if machine.id == link.device_id {
                    let addr = addr.filter(|a| peer_addr_ok(a));
                    self.roster.update_seen(&link.device_id, &machine, addr.as_deref(), now);
                }
                Frame::Ok
            }
            PeerBody::Bye { reason, .. } => Frame::Bye(reason),
            // Pairing bodies mean nothing on a live link.
            PeerBody::PairRequest { .. } | PeerBody::PairAccept { .. } | PeerBody::PairReject { .. } => Frame::Ok,
            // Everything else is the hub's (mirror, sync, ...). Never logged: it may carry user data.
            body => {
                self.emit(PeerEvent::Body { device_id: link.device_id.clone(), body });
                Frame::Ok
            }
        }
    }

    /// Close a link with `bye`, waiting until it is cleaned up (its LinkDown emitted). The control
    /// queue drains quickly; if it does not, the link is killed without bye.
    async fn close_link(&self, (ctl, kill): (mpsc::Sender<LinkCmd>, Arc<Notify>), bye: ByeReason) {
        let (tx, rx) = oneshot::channel();
        let sent = timeout(CLOSE_WAIT, ctl.send(LinkCmd::Close { bye: Some(bye), done: Some(tx) })).await;
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
            self.close_link(handle, ByeReason::Revoked).await;
        }
        info!(device = device_id, "device revoked");
        self.emit(PeerEvent::Revoked { device_id: device_id.to_string(), by_peer: false });
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
        futures::future::join_all(links.into_iter().map(|h| self.close_link(h, ByeReason::Shutdown))).await;
        lock(&self.invites).clear();
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
