//! The relay client: one outbound WebSocket to the relay (`{url}/v1/connect?pk=<our address>&k=<key>`),
//! authenticated with the device key. It carries the envelopes of every link on the relay path, the
//! allow list (the keys of our paired devices) and the pairing slots of our relay invites.
//!
//! There is no socket per link on the relay, so the handshake is a convention: `hello` means "I just
//! (re)connected, answer me", and the answer is a presence, which never asks for one, so the two sides
//! cannot ping-pong. Any fresh, authenticated frame from a paired device that has no link registers one.
//! Every relayed frame must come from the roster key of the device it names (`msg.from`); after that the
//! usual open + seq + direction checks apply.

use super::*;
use crate::relay::{auth_answer, ensure_tls_provider, parse_address, parse_relay_pk, Pacer};
use krypto_protocol::{RelayErrorCode, RelayServerFrame, RELAY_FRAME_MAX_BYTES, RELAY_MAX_ALLOWED, RELAY_PROTOCOL_VERSION};
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// The relay must accept the connection, send its challenge and answer our auth within this.
const AUTH_WAIT: Duration = Duration::from_secs(10);
/// Frames waiting to go to the relay; every link on the relay path shares this queue.
const RELAY_OUT_QUEUE: usize = 256;
/// How long a link waits for room in the shared relay queue for a frame it must not skip.
const RELAY_SEND_WAIT: Duration = Duration::from_secs(10);
/// Largest frame from the relay: a `msg` is at most RELAY_FRAME_MAX_BYTES + 64 bytes.
const RELAY_INBOUND_MAX: usize = RELAY_FRAME_MAX_BYTES + 4096;
/// Close code of the relay when a newer socket took over our mailbox.
const CLOSE_REPLACED: u16 = 4000;
/// Close code of the relay for a failed or late auth (and abuse).
const CLOSE_POLICY: u16 = 1008;

#[derive(Default)]
pub(super) struct RelayState {
    pub config: Option<RelayConfig>,
    /// Turned on by a join for the invite's relay, until the pairing succeeds: the allow list holds only
    /// the inviter and no hellos go out (that relay is the invite's, not one the user chose).
    pub restricted: bool,
    pub status: RelayStatusInfo,
    /// Bumped by every `set_relay`; a client task only touches the state of its own generation.
    pub gen: u64,
    pub task: Option<JoinHandle<()>>,
    /// Graceful stop (shutdown): flush the queue, then close.
    pub stop: Option<watch::Sender<bool>>,
    /// The authenticated session, if any.
    pub session: Option<RelayConn>,
    /// The allow list last sent on `session`.
    pub sent_allow: Option<Vec<String>>,
}

/// A live relay session, as links use it.
#[derive(Clone)]
pub(super) struct RelayConn {
    pub id: u64,
    /// Normalized URL of the relay.
    pub url: String,
    pub tx: mpsc::Sender<String>,
}

impl RelayConn {
    pub fn send_frame(&self, frame: &RelayClientFrame) -> Result<(), SendFail> {
        let text = serde_json::to_string(frame).expect("RelayClientFrame serializes");
        self.tx.try_send(text).map_err(|e| match e {
            TrySendError::Full(_) => SendFail::Full,
            TrySendError::Closed(_) => SendFail::Closed,
        })
    }

    /// `send_frame`, waiting (bounded) for room in the shared queue.
    pub async fn send_wait(&self, frame: &RelayClientFrame) -> bool {
        let text = serde_json::to_string(frame).expect("RelayClientFrame serializes");
        matches!(timeout(RELAY_SEND_WAIT, self.tx.send(text)).await, Ok(Ok(())))
    }
}

pub(super) struct PendingJoin {
    pub pk: [u8; 32],
    pub tx: mpsc::Sender<JoinReply>,
}

pub(super) enum JoinReply {
    Env(PeerEnvelope),
    /// The relay refused our pair_request (slot used, expired or unknown).
    NotAllowed,
}

/// Removes a pending relay join (and the inviter's key from the allow list, unless it is paired now)
/// however the join ends, the caller's timeout included.
struct JoinGuard {
    inner: Arc<Inner>,
    mid: String,
}

impl Drop for JoinGuard {
    fn drop(&mut self) {
        lock(&self.inner.relay_joins).remove(&self.mid);
        self.inner.sync_relay_allow();
    }
}

/// Turns an adopted relay off again unless the join that adopted it succeeded.
struct AdoptGuard {
    inner: Arc<Inner>,
    config: Option<RelayConfig>,
}

impl Drop for AdoptGuard {
    fn drop(&mut self) {
        let Some(config) = self.config.take() else { return };
        if lock(&self.inner.relay).config.as_ref() == Some(&config) {
            self.inner.set_relay(None);
        }
    }
}

enum SessionEnd {
    /// `set_relay` or shutdown stopped this client.
    Stopped,
    Failed(&'static str),
}

fn close_reason(code: Option<u16>, authenticating: bool) -> &'static str {
    match code {
        Some(CLOSE_REPLACED) => "replaced",
        Some(CLOSE_POLICY) if authenticating => "bad_auth",
        _ => "closed",
    }
}

impl Inner {
    /// The authenticated relay session, if any.
    pub(super) fn relay_conn(&self) -> Option<RelayConn> {
        lock(&self.relay).session.clone().filter(|c| !c.tx.is_closed())
    }

    pub(super) fn relay_config(&self) -> Option<RelayConfig> {
        lock(&self.relay).config.clone()
    }

    pub(super) fn relay_status(&self) -> RelayStatusInfo {
        lock(&self.relay).status.clone()
    }

    /// See `RelayState::restricted`.
    pub(super) fn relay_restricted(&self) -> bool {
        lock(&self.relay).restricted
    }

    /// The join that adopted the relay succeeded: the relay is ours now.
    fn lift_relay_restriction(&self) {
        let was = std::mem::replace(&mut lock(&self.relay).restricted, false);
        if was {
            self.sync_relay_allow();
            self.relay_changed.send_modify(|v| *v = v.wrapping_add(1));
        }
    }

    pub(super) fn link_path(&self, device_id: &str) -> Option<LinkPath> {
        lock(&self.links).get(device_id).map(|l| l.path)
    }

    /// Apply `f` to the state of generation `gen` (false for a client `set_relay` replaced), and
    /// report a status change.
    fn update_relay(&self, gen: u64, f: impl FnOnce(&mut RelayState)) -> bool {
        let mut st = lock(&self.relay);
        if st.gen != gen {
            return false;
        }
        let before = st.status.clone();
        f(&mut st);
        if st.status != before {
            self.emit(PeerEvent::RelayStatus(st.status.clone()));
        }
        true
    }

    /// `config` is normalized. Same config: no-op. Anything else stops the current client (its relay
    /// links go down with its session) and starts a new one.
    pub(super) fn set_relay(self: &Arc<Self>, config: Option<RelayConfig>) {
        self.set_relay_mode(config, false)
    }

    fn set_relay_mode(self: &Arc<Self>, config: Option<RelayConfig>, restricted: bool) {
        if self.is_closing() {
            return;
        }
        {
            let mut st = lock(&self.relay);
            if st.config == config {
                return;
            }
            st.restricted = restricted && config.is_some();
            if let Some(task) = st.task.take() {
                task.abort();
            }
            st.gen = st.gen.wrapping_add(1);
            st.stop = None;
            st.session = None;
            st.sent_allow = None;
            st.config = config.clone();
            st.status = RelayStatusInfo { url: config.as_ref().map(|c| c.url.clone()), connected: false, last_error: None };
            self.emit(PeerEvent::RelayStatus(st.status.clone()));
            if let Some(cfg) = config.clone() {
                let (stop_tx, stop_rx) = watch::channel(false);
                st.stop = Some(stop_tx);
                st.task = Some(tokio::spawn(self.clone().relay_loop(cfg, st.gen, stop_rx)));
            }
        }
        // Never the access key.
        info!(relay = ?config.as_ref().map(|c| c.url.as_str()), "relay set");
        self.relay_session_changed();
        if config.is_some() {
            for e in self.roster.list() {
                if e.device.kind == DeviceKind::Hub {
                    self.ensure_dialer(&e.device.id);
                }
            }
        }
    }

    fn relay_session_changed(&self) {
        lock(&self.relay_pending).clear();
        lock(&self.relay_refused).clear();
        self.relay_changed.send_modify(|v| *v = v.wrapping_add(1));
    }

    /// Stop the relay client after it flushed what the links queued (their byes).
    pub(super) async fn stop_relay(&self) {
        let task = {
            let mut st = lock(&self.relay);
            if let Some(stop) = st.stop.take() {
                let _ = stop.send(true);
            }
            st.task.take()
        };
        if let Some(mut task) = task {
            if timeout(CLOSE_WAIT * 2, &mut task).await.is_err() {
                task.abort();
            }
        }
    }

    async fn relay_loop(self: Arc<Self>, config: RelayConfig, gen: u64, mut stop: watch::Receiver<bool>) {
        let mut backoff = self.timings().backoff_min;
        loop {
            if self.is_closing() || *stop.borrow() {
                return;
            }
            let started = Instant::now();
            let code = match self.relay_session(&config, gen, &mut stop).await {
                SessionEnd::Stopped => return,
                SessionEnd::Failed(code) => code,
            };
            let t = self.timings();
            if started.elapsed() >= t.offline_after {
                backoff = t.backoff_min;
            }
            // Another socket with our key took the mailbox: do not fight it every second.
            let wait = if code == "replaced" { t.backoff_max } else { backoff };
            info!(relay = %config.url, error = code, "relay connection ended; reconnecting with backoff");
            tokio::select! {
                _ = tokio::time::sleep(jitter(wait)) => {}
                _ = stop.changed() => return,
            }
            backoff = (backoff * 2).min(t.backoff_max);
        }
    }

    fn relay_failed(&self, gen: u64, code: &'static str) -> SessionEnd {
        self.update_relay(gen, |st| {
            st.status.connected = false;
            st.status.last_error = Some(code.to_string());
        });
        // A join waiting for this relay gives up on it.
        self.relay_changed.send_modify(|v| *v = v.wrapping_add(1));
        SessionEnd::Failed(code)
    }

    async fn relay_session(
        self: &Arc<Self>,
        config: &RelayConfig,
        gen: u64,
        stop: &mut watch::Receiver<bool>,
    ) -> SessionEnd {
        let url = config.url.as_str();
        let mut connect = format!("{url}/v1/connect?pk={}", address(&self.keys.public_key()));
        if let Some(key) = &config.access_key {
            connect.push_str("&k=");
            connect.push_str(key);
        }
        // The request (it holds the access key) is never logged.
        let Ok(request) = connect.into_client_request() else { return self.relay_failed(gen, "bad_url") };
        // The auth text binds the Host header exactly as we send it.
        let Some(host) = request.headers().get(header::HOST).and_then(|h| h.to_str().ok()).map(str::to_owned) else {
            return self.relay_failed(gen, "bad_url");
        };
        if url.starts_with("wss://") {
            ensure_tls_provider();
        }
        use tungstenite::protocol::WebSocketConfig;
        let cfg = WebSocketConfig::default()
            .max_message_size(Some(RELAY_INBOUND_MAX))
            .max_frame_size(Some(RELAY_INBOUND_MAX));
        let ws = match timeout(AUTH_WAIT, tokio_tungstenite::connect_async_with_config(request, Some(cfg), true)).await {
            Ok(Ok((ws, _))) => ws,
            Ok(Err(tungstenite::Error::Http(response))) => {
                let code = match response.status().as_u16() {
                    401 | 403 => "bad_key",
                    429 => "rate_limited",
                    503 => "unavailable",
                    _ => "unreachable",
                };
                debug!(relay = %url, status = response.status().as_u16(), "relay refused the connection");
                return self.relay_failed(gen, code);
            }
            Ok(Err(e)) => {
                if !matches!(e, tungstenite::Error::Url(_)) {
                    debug!(relay = %url, error = %e, "relay connect failed");
                }
                return self.relay_failed(gen, "unreachable");
            }
            Err(_) => return self.relay_failed(gen, "timeout"),
        };
        let mut io = spawn_io(ws);
        let queued = match self.relay_auth(&mut io, &host).await {
            Ok(q) => q,
            Err(code) => {
                io.close();
                return self.relay_failed(gen, code);
            }
        };
        // Up. The allow list and the pairing slots go first, before any link can use the session.
        let (tx, mut out_rx) = mpsc::channel::<String>(RELAY_OUT_QUEUE);
        let conn = RelayConn { id: self.next_relay_session.fetch_add(1, Ordering::Relaxed), url: url.to_string(), tx };
        let allow = self.relay_allow_list();
        let _ = conn.send_frame(&RelayClientFrame::Allow { pks: allow.clone() });
        for (token, exp) in self.relay_slots(url) {
            let _ = conn.send_frame(&RelayClientFrame::AllowPairing { token, exp });
        }
        let current = self.update_relay(gen, |st| {
            st.session = Some(conn.clone());
            st.sent_allow = Some(allow);
            st.status.connected = true;
            st.status.last_error = None;
        });
        if !current {
            io.close();
            return SessionEnd::Stopped;
        }
        info!(relay = %url, queued, "relay connected");
        self.relay_session_changed();

        let result = self.relay_pump(&conn, &mut io, &mut out_rx, stop).await;
        io.close();
        self.update_relay(gen, |st| {
            if st.session.as_ref().is_some_and(|c| c.id == conn.id) {
                st.session = None;
                st.sent_allow = None;
            }
            st.status.connected = false;
            if let Err(code) = result {
                st.status.last_error = Some(code.to_string());
            }
        });
        // Dropping the queue ends the session for the links on it (`Out::lost`).
        drop(out_rx);
        self.relay_session_changed();
        info!(relay = %url, "relay disconnected");
        match result {
            Ok(()) => SessionEnd::Stopped,
            Err(code) => SessionEnd::Failed(code),
        }
    }

    /// Challenge -> auth -> ok. Returns how many queued frames the relay is about to flush.
    async fn relay_auth(&self, io: &mut WsIo, host: &str) -> Result<u32, &'static str> {
        let first = match timeout(AUTH_WAIT, io.rx.recv()).await {
            Ok(Some(text)) => text,
            Ok(None) => return Err(close_reason(io.close_code(), true)),
            Err(_) => return Err("timeout"),
        };
        let Ok(RelayServerFrame::Challenge { nonce, relay_pk, v }) = serde_json::from_str(&first) else {
            return Err("bad_frame");
        };
        if v != RELAY_PROTOCOL_VERSION {
            return Err("unsupported_version");
        }
        // The auth box must never be made with a peer's key (see `auth_answer`).
        let pk = parse_relay_pk(&relay_pk).ok_or("bad_frame")?;
        let peer_key = self.roster.list().iter().any(|e| parse_pk(&e.device.public_key) == Some(pk))
            || lock(&self.relay_joins).values().any(|j| j.pk == pk);
        if peer_key {
            warn!("relay challenge refused: its key is one of our peers' keys");
            return Err("bad_auth");
        }
        let answer = auth_answer(&self.keys, &nonce, &relay_pk, host).inspect_err(|code| {
            if *code == "bad_auth" {
                warn!("relay challenge refused: its nonce or key could reuse an envelope's");
            }
        })?;
        let auth = serde_json::to_string(&RelayClientFrame::Auth { ciphertext: answer }).expect("frame serializes");
        if io.send(auth).is_err() {
            return Err("closed");
        }
        let reply = match timeout(AUTH_WAIT, io.rx.recv()).await {
            Ok(Some(text)) => text,
            Ok(None) => return Err(close_reason(io.close_code(), true)),
            Err(_) => return Err("timeout"),
        };
        match serde_json::from_str::<RelayServerFrame>(&reply) {
            Ok(RelayServerFrame::Ok { queued }) => Ok(queued),
            Ok(RelayServerFrame::Error { code: RelayErrorCode::BadAuth, .. }) => Err("bad_auth"),
            _ => Err("bad_frame"),
        }
    }

    /// Move frames both ways until the session ends: `Ok` when stopped (after flushing the queue),
    /// else the reason.
    async fn relay_pump(
        self: &Arc<Self>,
        conn: &RelayConn,
        io: &mut WsIo,
        out_rx: &mut mpsc::Receiver<String>,
        stop: &mut watch::Receiver<bool>,
    ) -> Result<(), &'static str> {
        let t = self.timings();
        let mut pacer = Pacer::new(Instant::now());
        let mut ping = tokio::time::interval_at(Instant::now() + t.presence_interval, t.presence_interval);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut heard = Instant::now();
        loop {
            let now = Instant::now();
            let ready_at = pacer.ready_at(now);
            let paced = ready_at > now;
            let socket_full = io.tx.capacity() == 0;
            tokio::select! {
                frame = io.rx.recv() => match frame {
                    Some(text) => {
                        heard = Instant::now();
                        self.on_relay_frame(conn, &text);
                    }
                    None => return Err(close_reason(io.close_code(), false)),
                },
                out = out_rx.recv(), if !paced && !socket_full => match out {
                    Some(text) => {
                        pacer.take(Instant::now());
                        if io.send(text).is_err() {
                            return Err("closed");
                        }
                    }
                    None => return Err("closed"),
                },
                _ = tokio::time::sleep_until(ready_at), if paced => {}
                room = io.tx.reserve(), if !paced && socket_full => if room.is_err() { return Err("closed") },
                _ = ping.tick() => {
                    if heard.elapsed() >= t.offline_after {
                        return Err("timeout");
                    }
                    let _ = conn.send_frame(&RelayClientFrame::Ping { ts: now_ms() });
                    // Also retries an allow list a full queue could not take.
                    self.sync_relay_allow();
                }
                _ = stop.changed() => {
                    // Shutdown: flush what the links queued (their byes), then close.
                    out_rx.close();
                    let deadline = Instant::now() + CLOSE_WAIT;
                    while let Ok(Some(text)) = tokio::time::timeout_at(deadline, out_rx.recv()).await {
                        if !matches!(tokio::time::timeout_at(deadline, io.send_wait(text)).await, Ok(Ok(()))) {
                            break;
                        }
                    }
                    return Ok(());
                }
            }
        }
    }

    fn on_relay_frame(self: &Arc<Self>, conn: &RelayConn, text: &str) {
        let Ok(frame) = serde_json::from_str::<RelayServerFrame>(text) else {
            debug!("relay: dropped a malformed frame");
            return;
        };
        match frame {
            RelayServerFrame::Msg { from, env } => self.relay_msg(conn, &from, env),
            RelayServerFrame::Error { code, to } => self.relay_error(code, to.as_deref()),
            RelayServerFrame::Pong { .. } | RelayServerFrame::Ok { .. } | RelayServerFrame::Challenge { .. } => {}
        }
    }

    fn relay_error(&self, code: RelayErrorCode, to: Option<&str>) {
        let pk = to.and_then(parse_address);
        match code {
            RelayErrorCode::NotAllowed => {
                let Some(pk) = pk else { return };
                if let Some(join) = lock(&self.relay_joins).values().find(|j| j.pk == pk) {
                    let _ = join.tx.try_send(JoinReply::NotAllowed);
                }
                if let Some(id) = self.device_by_pk(&pk) {
                    debug!(device = %id, "relay: the device does not accept our frames (not paired there, or its allow list is behind)");
                    lock(&self.relay_refused).insert(id);
                    self.relay_changed.send_modify(|v| *v = v.wrapping_add(1));
                }
            }
            RelayErrorCode::QueueFull => debug!(to = ?to, "relay: the device's mailbox is full; its oldest frame was dropped"),
            RelayErrorCode::RateLimited => warn!(to = ?to, "relay: rate limited or mailbox unavailable; a frame was dropped"),
            RelayErrorCode::TooLarge => warn!(to = ?to, "relay: a frame was too large and was dropped"),
            RelayErrorCode::BadFrame => warn!(to = ?to, "relay: the relay rejected a frame as malformed"),
            RelayErrorCode::BadAuth => warn!("relay: unexpected bad_auth"),
        }
    }

    fn device_by_pk(&self, pk: &[u8; 32]) -> Option<String> {
        self.roster.list().into_iter().find(|e| parse_pk(&e.device.public_key).as_ref() == Some(pk)).map(|e| e.device.id)
    }

    /// A `msg` from the relay. `from` (the sender the relay authenticated) must be the roster key of
    /// `env.from`, or the envelope's own key for a pair_request; mismatches are dropped with a warning
    /// (never the payload).
    fn relay_msg(self: &Arc<Self>, conn: &RelayConn, from: &str, env: PeerEnvelope) {
        let Some(from_pk) = parse_address(from) else {
            debug!("relay: dropped a frame with a malformed sender");
            return;
        };
        let me_id = self.me_id();
        if self.is_closing()
            || env.to != me_id
            || env.from == me_id
            || !is_valid_machine_id(&env.from)
            || from_pk == self.keys.public_key()
        {
            debug!("relay: dropped a misaddressed frame");
            return;
        }
        if let Some(pk) = env.pk.as_deref() {
            if parse_pk(pk) != Some(from_pk) {
                warn!(device = %env.from, "relay: dropped a pairing frame whose key is not its relay sender");
                return;
            }
            return self.relay_pair_request(conn, env, from_pk);
        }
        // A join waiting for this inviter's answer.
        if let Some(join) = lock(&self.relay_joins).get(&env.from) {
            if join.pk == from_pk {
                let _ = join.tx.try_send(JoinReply::Env(env));
                return;
            }
        }
        if self.cooldown_until(&env.from).is_some() {
            debug!(device = %env.from, "relay: dropped a frame from a device cooling down after a disconnect");
            return;
        }
        let Some(entry) = self.roster.get(&env.from) else {
            // A device we revoked that missed it. It only reaches us while our allow list lags; else the
            // bye of the revoke itself waits in its mailbox.
            let reply = |bye: PeerEnvelope| {
                let _ = conn.send_frame(&RelayClientFrame::Send { to: from.to_string(), env: bye, pairing: None });
            };
            if !self.answer_revoked(&env, Some(&from_pk), false, &reply) {
                debug!(device = %env.from, "relay: dropped a frame from an unknown or revoked device");
            }
            return;
        };
        if parse_pk(&entry.device.public_key) != Some(from_pk) {
            warn!(device = %env.from, "relay: dropped a frame whose relay sender is not the device's roster key");
            return;
        }
        let relay_in = lock(&self.links).get(&env.from).map(|l| l.relay_in.clone());
        match relay_in {
            Some(tx) => {
                if let Err(TrySendError::Full(_)) = tx.try_send(env) {
                    warn!(device = %entry.device.id, "relay: link inbound queue full; dropped a frame");
                }
            }
            None => self.relay_first_frame(conn, env, entry, from_pk),
        }
    }

    /// A relayed frame from a paired device we have no link with. Fresh and authentic: register a
    /// link on the relay path, then handle the body after its `LinkUp`.
    fn relay_first_frame(self: &Arc<Self>, conn: &RelayConn, env: PeerEnvelope, entry: RosterEntry, pk: [u8; 32]) {
        let device_id = env.from.clone();
        if !self.seq_above_last(&device_id, env.seq) {
            debug!(device = %device_id, seq = env.seq, "relay: dropped a replayed frame");
            return;
        }
        let body = match self.open_from(&env, &pk) {
            Ok(b) => b,
            Err(e) => {
                debug!(device = %device_id, error = %e, "relay: dropped a frame that did not open");
                return;
            }
        };
        if stale_stamp(&body) {
            debug!(device = %device_id, "relay: dropped a hello/presence stamped too far from our clock");
            return;
        }
        if !self.accept_rx_seq(&device_id, env.seq) {
            debug!(device = %device_id, seq = env.seq, "relay: dropped a replayed frame");
            return;
        }
        let now = now_ms();
        let (machine, reply) = match &body {
            PeerBody::Bye { reason: ByeReason::Revoked, .. } => {
                // It revoked us while we had no link (the bye waited in our mailbox, or answers our hello).
                info!(device = %device_id, "peer revoked us (bye over the relay)");
                self.revoked_by_peer(&device_id);
                return;
            }
            PeerBody::Bye { .. } | PeerBody::PairRequest { .. } | PeerBody::PairAccept { .. } | PeerBody::PairReject { .. } => {
                return
            }
            PeerBody::Hello { machine, addr, .. } => {
                if machine.id != device_id || !device_name_ok(&machine.name) {
                    debug!(device = %device_id, "relay: dropped a hello for another machine or with a refused name");
                    return;
                }
                let addr = self.hello_addr(&device_id, addr.clone());
                self.roster.update_seen(&device_id, machine, addr.as_deref(), now);
                // Our first frame on the link (a presence) answers it.
                (machine.clone(), None)
            }
            _ => {
                // The peer thinks it is linked but we are not (we restarted, or our relay session is
                // new): say hello so it resyncs, unless this frame answers our own hello.
                let answered = lock(&self.relay_pending).remove(&device_id);
                let hello = PeerBody::Hello { machine: self.me(), addr: self.advertised_addr(), ts: now };
                (entry.machine.clone(), (!answered).then_some(hello))
            }
        };
        let setup = LinkSetup {
            device_id: device_id.clone(),
            peer_pk: pk,
            path: LinkPath::Relay,
            dialed_by_me: false,
            machine: Machine { last_seen: now, online: true, ..machine },
            force: false,
            reply,
        };
        let Some(link) = self.register(setup) else { return };
        if !matches!(body, PeerBody::Hello { .. }) {
            // Presence or a hub body, after the LinkUp. (Bye and pairing bodies returned above.)
            let _ = self.on_body(&device_id, body);
        }
        tokio::spawn(self.clone().run_link(Transport::Relay(conn.clone()), link));
        self.ensure_dialer(&device_id);
    }

    /// A pair_request through the relay (its sender key already matched the envelope's).
    fn relay_pair_request(self: &Arc<Self>, conn: &RelayConn, env: PeerEnvelope, pk: [u8; 32]) {
        let to = address(&pk);
        let reply = |e: PeerEnvelope| {
            let _ = conn.send_frame(&RelayClientFrame::Send { to: to.clone(), env: e, pairing: None });
        };
        let Some(setup) = self.pair_request(env, pk, PairOrigin::Relay, &reply) else { return };
        let device_id = setup.device_id.clone();
        // Allow the new device before our pair_accept leaves (same queue: the relay sees it first).
        self.sync_relay_allow();
        if let Some(link) = self.register(setup) {
            tokio::spawn(self.clone().run_link(Transport::Relay(conn.clone()), link));
        }
        self.ensure_dialer(&device_id);
    }

    /// Hello to a paired device over the relay (we have no link with it). False if not sent.
    pub(super) fn relay_hello(&self, entry: &RosterEntry, conn: &RelayConn) -> bool {
        let device_id = &entry.device.id;
        let Some(pk) = parse_pk(&entry.device.public_key) else { return false };
        let hello = PeerBody::Hello { machine: self.me(), addr: self.advertised_addr(), ts: now_ms() };
        let env = seal(&self.keys, &self.me_id(), device_id, &pk, self.next_tx_seq(device_id), &hello, false);
        lock(&self.relay_pending).insert(device_id.clone());
        match conn.send_frame(&RelayClientFrame::Send { to: address(&pk), env, pairing: None }) {
            Ok(()) => {
                debug!(device = %device_id, "hello sent over the relay");
                true
            }
            Err(_) => {
                lock(&self.relay_pending).remove(device_id);
                debug!(device = %device_id, "relay queue full; hello not sent");
                false
            }
        }
    }

    /// Who may send to our mailbox: every paired device (the most recently seen first if there are
    /// more than the relay allows) and the inviters of pending relay joins. Sorted.
    fn relay_allow_list(&self) -> Vec<String> {
        if self.relay_restricted() {
            // A join's adopted relay: only the inviter, never our other peers' keys.
            let mut pks: Vec<String> = lock(&self.relay_joins).values().map(|j| address(&j.pk)).collect();
            pks.sort();
            pks.dedup();
            return pks;
        }
        let mut rows: Vec<(UnixMs, String)> = self
            .roster
            .list()
            .iter()
            .filter_map(|e| {
                parse_pk(&e.device.public_key).map(|pk| (e.device.last_seen.unwrap_or(e.device.paired_at), address(&pk)))
            })
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.0));
        let mut pks: Vec<String> = lock(&self.relay_joins).values().map(|j| address(&j.pk)).collect();
        pks.extend(rows.into_iter().map(|(_, a)| a));
        let mut seen = HashSet::new();
        pks.retain(|a| seen.insert(a.clone()));
        if pks.len() > RELAY_MAX_ALLOWED {
            warn!(devices = pks.len(), max = RELAY_MAX_ALLOWED, "more devices than the relay allows; the least recently seen are left out");
            pks.truncate(RELAY_MAX_ALLOWED);
        }
        pks.sort();
        pks
    }

    /// Send the allow list if it changed since the last one on this session.
    pub(super) fn sync_relay_allow(&self) {
        let Some(conn) = self.relay_conn() else { return };
        let pks = self.relay_allow_list();
        let mut st = lock(&self.relay);
        if st.session.as_ref().map(|c| c.id) != Some(conn.id) || st.sent_allow.as_ref() == Some(&pks) {
            return;
        }
        match conn.send_frame(&RelayClientFrame::Allow { pks: pks.clone() }) {
            Ok(()) => st.sent_allow = Some(pks),
            Err(_) => warn!("relay queue full; the allow list goes out with the next ping"),
        }
    }

    /// The relay session for a join through `invite`'s relay: ours when it is that relay; with no relay
    /// configured, the invite's, turned on now (recorded in `adopt`). Waits for it to authenticate.
    async fn join_session(self: &Arc<Self>, invite: &Invite, adopt: &mut AdoptGuard) -> Result<RelayConn, JoinError> {
        let Some(url) = invite.relay.clone() else { return Err(JoinError::Unreachable) };
        match self.relay_config() {
            Some(c) if c.url == url => {}
            Some(_) => {
                info!("cannot join over the relay: the invite names a different relay than ours");
                return Err(JoinError::Unreachable);
            }
            None => {
                let config = RelayConfig { url, access_key: invite.relay_key.clone() };
                info!(relay = %config.url, "no relay configured: using the invite's relay");
                self.set_relay_mode(Some(config.clone()), true);
                adopt.config = Some(config);
            }
        }
        let mut changed = self.relay_changed.subscribe();
        loop {
            changed.borrow_and_update();
            if let Some(conn) = self.relay_conn() {
                return Ok(conn);
            }
            if adopt.config.is_some() && self.relay_status().last_error.is_some() {
                info!(error = ?self.relay_status().last_error, "the invite's relay did not connect");
                return Err(JoinError::Unreachable);
            }
            if changed.changed().await.is_err() {
                return Err(JoinError::Unreachable);
            }
        }
    }

    /// Pair through the relay: allow the inviter, send the pair_request with the invite's pairing token,
    /// wait for the answer.
    pub(super) async fn join_relay(self: &Arc<Self>, invite: &Invite) -> Result<Joined, JoinError> {
        // A relay this join turns on goes off again unless the join succeeds (`AdoptGuard`).
        let mut adopt = AdoptGuard { inner: self.clone(), config: None };
        let conn = self.join_session(invite, &mut adopt).await?;
        let mid = invite.machine_id.clone();
        let (tx, mut rx) = mpsc::channel(8);
        {
            let mut joins = lock(&self.relay_joins);
            if joins.contains_key(&mid) {
                return Err(JoinError::Unreachable);
            }
            joins.insert(mid.clone(), PendingJoin { pk: invite.public_key, tx });
        }
        let _join = JoinGuard { inner: self.clone(), mid: mid.clone() };
        // The inviter is not in our roster yet: allow it first, or its pair_accept is refused at our
        // own mailbox.
        self.sync_relay_allow();
        let req_seq = self.next_tx_seq(&mid);
        let frame = RelayClientFrame::Send {
            to: address(&invite.public_key),
            env: self.pair_request_env(invite, req_seq),
            pairing: invite.relay_token.clone(),
        };
        if !conn.send_wait(&frame).await {
            return Err(JoinError::Unreachable);
        }
        loop {
            let reply = tokio::select! {
                r = rx.recv() => r,
                _ = conn.tx.closed() => None,
            };
            match reply {
                None => return Err(JoinError::Unreachable),
                Some(JoinReply::NotAllowed) => {
                    info!(device = %mid, "the relay refused the pairing frame (invite used or expired)");
                    return Err(JoinError::Rejected);
                }
                Some(JoinReply::Env(env)) => match self.open_pair_reply(invite, req_seq, &env) {
                    PairReply::Accept(machine) => {
                        let (device, machine) = self.finish_join(invite, machine, env.seq, req_seq, Transport::Relay(conn));
                        self.lift_relay_restriction();
                        return Ok(Joined { device, machine, adopted_relay: adopt.config.take() });
                    }
                    PairReply::Reject => return Err(JoinError::Rejected),
                    PairReply::Other => {}
                },
            }
        }
    }
}
