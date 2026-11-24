//! Relay transport against an in-process mock relay that speaks the step-6 frames: access key,
//! challenge/auth (Host bound), allow, allow_pairing (one-shot, max 4), send -> msg or not_allowed,
//! queueing while the recipient is offline, ping/pong, close 4000 when a newer socket takes a mailbox.

use super::*;
use crate::relay::parse_address;
use crypto_box::aead::Aead;
use krypto_protocol::{RelayErrorCode, RelayServerFrame, RELAY_AUTH_CONTEXT};
use std::collections::VecDeque;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request as WsRequest, Response as WsResponse};
use tokio_tungstenite::tungstenite::http;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;

const KEY: &str = "test-access_key";

#[derive(Default)]
struct Mailbox {
    live: Option<(u64, mpsc::UnboundedSender<Message>)>,
    allowed: HashSet<String>,
    slots: Vec<(String, i64)>,
    queue: VecDeque<String>,
}

/// A relay that tries to make the auth box reuse an envelope's (key, nonce).
#[derive(Debug, Clone, Copy)]
enum Hostile {
    /// A challenge nonce in the envelope domain (byte 15 = 1).
    EnvelopeNonce,
    /// This key as the per-connection relay key.
    RelayPk([u8; 32]),
}

/// What a mailbox's socket sent the relay, in order.
#[derive(Debug, Clone, PartialEq)]
enum Sent {
    Allow(HashSet<String>),
    Send(String),
}

#[derive(Default)]
struct MockState {
    boxes: HashMap<String, Mailbox>,
    hostile: Option<Hostile>,
    challenges: usize,
    auth_frames: usize,
    log: Vec<(String, Sent)>,
    next_socket: u64,
    /// Successful auths per mailbox.
    auths: HashMap<String, usize>,
    /// The Host the relay expects in the auth text instead of the request's.
    host_override: Option<String>,
}

#[derive(Clone)]
struct MockRelay {
    addr: SocketAddr,
    state: Arc<Mutex<MockState>>,
}

impl MockRelay {
    async fn start() -> MockRelay {
        let listener = TcpListener::bind(local()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(MockState::default()));
        let st = state.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                tokio::spawn(serve(st.clone(), tcp));
            }
        });
        MockRelay { addr, state }
    }

    fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    fn config(&self) -> RelayConfig {
        RelayConfig { url: self.url(), access_key: Some(KEY.into()) }
    }

    fn allowed(&self, pk: &[u8; 32]) -> HashSet<String> {
        lock(&self.state).boxes.get(&address(pk)).map(|m| m.allowed.clone()).unwrap_or_default()
    }

    fn auths(&self, pk: &[u8; 32]) -> usize {
        lock(&self.state).auths.get(&address(pk)).copied().unwrap_or(0)
    }

    fn live_slots(&self, pk: &[u8; 32]) -> usize {
        let now = now_ms();
        lock(&self.state).boxes.get(&address(pk)).map_or(0, |m| m.slots.iter().filter(|(_, e)| *e > now).count())
    }

    fn online(&self, pk: &[u8; 32]) -> bool {
        lock(&self.state).boxes.get(&address(pk)).is_some_and(|m| m.live.is_some())
    }

    /// Close every socket, like a relay restart (allow lists, slots and queues persist).
    fn kick_all(&self) {
        let mut st = lock(&self.state);
        for mb in st.boxes.values_mut() {
            if let Some((_, tx)) = mb.live.take() {
                let _ = tx.send(Message::Close(Some(CloseFrame { code: CloseCode::Away, reason: "".into() })));
            }
        }
    }

    /// Hand `to` a msg frame as if the relay had authenticated `from` as its sender.
    fn inject(&self, to: &[u8; 32], from: &str, env: &PeerEnvelope) {
        let frame = serde_json::to_string(&RelayServerFrame::Msg { from: from.into(), env: env.clone() }).unwrap();
        let st = lock(&self.state);
        let (_, tx) = st.boxes[&address(to)].live.as_ref().expect("mailbox online");
        tx.send(Message::text(frame)).unwrap();
    }

    /// Forget what waits in a mailbox (as if it expired).
    fn clear_queue(&self, pk: &[u8; 32]) {
        if let Some(mb) = lock(&self.state).boxes.get_mut(&address(pk)) {
            mb.queue.clear();
        }
    }

    fn set_hostile(&self, h: Option<Hostile>) {
        lock(&self.state).hostile = h;
    }

    /// (challenges sent, auth frames received), all mailboxes.
    fn handshakes(&self) -> (usize, usize) {
        let st = lock(&self.state);
        (st.challenges, st.auth_frames)
    }

    fn sent_by(&self, pk: &[u8; 32]) -> Vec<Sent> {
        let me = address(pk);
        lock(&self.state).log.iter().filter(|(from, _)| *from == me).map(|(_, s)| s.clone()).collect()
    }

    fn set_host_override(&self, host: Option<&str>) {
        lock(&self.state).host_override = host.map(String::from);
    }
}

fn server_frame(f: &RelayServerFrame) -> Message {
    Message::text(serde_json::to_string(f).unwrap())
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == key).map(|(_, v)| v.to_string()))
}

async fn serve(state: Arc<Mutex<MockState>>, tcp: TcpStream) {
    let mut host = String::new();
    let mut pk = String::new();
    // The signature is tungstenite's handshake callback.
    #[allow(clippy::result_large_err)]
    let callback = |req: &WsRequest, resp: WsResponse| -> Result<WsResponse, ErrorResponse> {
        let query = req.uri().query().unwrap_or_default();
        if req.uri().path() != "/v1/connect" || query_param(query, "k").as_deref() != Some(KEY) {
            return Err(http::Response::builder().status(401).body(None).unwrap());
        }
        host = req.headers().get("host").and_then(|h| h.to_str().ok()).unwrap_or_default().to_string();
        pk = query_param(query, "pk").unwrap_or_default();
        Ok(resp)
    };
    let Ok(ws) = tokio_tungstenite::accept_hdr_async(tcp, callback).await else { return };
    let Some(device_pk) = parse_address(&pk) else { return };
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let close = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || close {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let relay_keys = DeviceKeys::generate();
    let mut nonce = [0u8; 24];
    getrandom::fill(&mut nonce).unwrap();
    // The auth nonce domain: byte 15 >= 2 (envelope nonces have 0/1 there).
    nonce[15] = 2 + nonce[15] % 254;
    let mut relay_pk = relay_keys.public_key();
    {
        let mut st = lock(&state);
        st.challenges += 1;
        match st.hostile {
            Some(Hostile::EnvelopeNonce) => nonce[15] = 1,
            Some(Hostile::RelayPk(pk)) => relay_pk = pk,
            None => {}
        }
    }
    let nonce_b64 = b64::encode(&nonce);
    let challenge = RelayServerFrame::Challenge { nonce: nonce_b64.clone(), relay_pk: b64::encode(&relay_pk), v: 1 };
    let _ = tx.send(server_frame(&challenge));
    let authed = match stream.next().await {
        Some(Ok(Message::Text(t))) => match serde_json::from_str::<RelayClientFrame>(t.as_str()) {
            Ok(RelayClientFrame::Auth { ciphertext }) => {
                lock(&state).auth_frames += 1;
                let expect_host = lock(&state).host_override.clone().unwrap_or(host);
                let text = format!("{RELAY_AUTH_CONTEXT}:{nonce_b64}:{expect_host}");
                b64::decode(&ciphertext)
                    .ok()
                    .and_then(|ct| {
                        relay_keys.salsa_box(&device_pk).decrypt(crypto_box::Nonce::from_slice(&nonce), ct.as_slice()).ok()
                    })
                    .is_some_and(|plain| plain == text.as_bytes())
            }
            _ => false,
        },
        _ => false,
    };
    if !authed {
        let _ = tx.send(server_frame(&RelayServerFrame::Error { code: RelayErrorCode::BadAuth, to: None }));
        let _ = tx.send(Message::Close(Some(CloseFrame { code: CloseCode::Policy, reason: "".into() })));
        return;
    }
    let id = {
        let mut st = lock(&state);
        st.next_socket += 1;
        let id = st.next_socket;
        *st.auths.entry(pk.clone()).or_default() += 1;
        let mb = st.boxes.entry(pk.clone()).or_default();
        if let Some((_, old)) = mb.live.replace((id, tx.clone())) {
            let _ = old.send(Message::Close(Some(CloseFrame { code: CloseCode::Library(4000), reason: "".into() })));
        }
        // ok + the queue flush, before anything else can be delivered.
        let queued: Vec<String> = mb.queue.drain(..).collect();
        let _ = tx.send(server_frame(&RelayServerFrame::Ok { queued: queued.len() as u32 }));
        for q in queued {
            let _ = tx.send(Message::text(q));
        }
        id
    };
    while let Some(Ok(msg)) = stream.next().await {
        let text = match msg {
            Message::Text(t) => t.as_str().to_owned(),
            Message::Close(_) => break,
            _ => continue,
        };
        match serde_json::from_str::<RelayClientFrame>(&text) {
            Ok(RelayClientFrame::Allow { pks }) => {
                let mut st = lock(&state);
                let set: HashSet<String> = pks.into_iter().collect();
                st.log.push((pk.clone(), Sent::Allow(set.clone())));
                st.boxes.entry(pk.clone()).or_default().allowed = set;
            }
            Ok(RelayClientFrame::AllowPairing { token, exp }) => {
                let mut st = lock(&state);
                let mb = st.boxes.entry(pk.clone()).or_default();
                let now = now_ms();
                mb.slots.retain(|(t, e)| *e > now && *t != token);
                mb.slots.push((token, exp));
                while mb.slots.len() > 4 {
                    mb.slots.remove(0);
                }
            }
            Ok(RelayClientFrame::Send { to, env, pairing }) => {
                lock(&state).log.push((pk.clone(), Sent::Send(to.clone())));
                deliver(&state, &pk, &tx, to, env, pairing)
            }
            Ok(RelayClientFrame::Ping { ts }) => {
                let _ = tx.send(server_frame(&RelayServerFrame::Pong { ts }));
            }
            Ok(_) => {}
            Err(_) => {
                let _ = tx.send(server_frame(&RelayServerFrame::Error { code: RelayErrorCode::BadFrame, to: None }));
            }
        }
    }
    let mut st = lock(&state);
    if let Some(mb) = st.boxes.get_mut(&pk) {
        if mb.live.as_ref().is_some_and(|(i, _)| *i == id) {
            mb.live = None;
        }
    }
}

fn deliver(
    state: &Mutex<MockState>,
    from: &str,
    sender: &mpsc::UnboundedSender<Message>,
    to: String,
    env: PeerEnvelope,
    pairing: Option<String>,
) {
    let mut st = lock(state);
    let now = now_ms();
    let mb = st.boxes.entry(to.clone()).or_default();
    let mut allowed = mb.allowed.contains(from);
    if let (false, Some(token)) = (allowed, pairing) {
        if let Some(i) = mb.slots.iter().position(|(t, e)| *t == token && *e > now) {
            mb.slots.remove(i);
            allowed = true;
        }
    }
    if !allowed {
        let _ = sender.send(server_frame(&RelayServerFrame::Error { code: RelayErrorCode::NotAllowed, to: Some(to) }));
        return;
    }
    let frame = serde_json::to_string(&RelayServerFrame::Msg { from: from.to_string(), env }).unwrap();
    match &mb.live {
        Some((_, tx)) if tx.send(Message::text(frame.clone())).is_ok() => {}
        _ => mb.queue.push_back(frame),
    }
}

// ---------- helpers ----------

fn is_relay_up(e: &PeerEvent) -> bool {
    matches!(e, PeerEvent::RelayStatus(s) if s.connected)
}

fn is_relay_down(e: &PeerEvent) -> bool {
    matches!(e, PeerEvent::RelayStatus(s) if !s.connected)
}

fn is_relay_error<'a>(code: &'a str) -> impl Fn(&PeerEvent) -> bool + 'a {
    move |e| matches!(e, PeerEvent::RelayStatus(s) if s.last_error.as_deref() == Some(code))
}

fn is_path(id: &str, p: LinkPath) -> impl Fn(&PeerEvent) -> bool + '_ {
    move |e| matches!(e, PeerEvent::LinkPath { device_id, path } if device_id == id && *path == p)
}

/// Wait (bounded) until `cond` holds.
async fn eventually(what: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A node (no listener) connected to `relay`.
async fn on_relay(id: &str, relay: &MockRelay, t: PeerTimings) -> TestNode {
    let mut n = node(id, t);
    n.node.set_relay(Some(relay.config())).unwrap();
    n.expect("relay connected", is_relay_up).await;
    n
}

/// Pair `b` with `a` through the relay (the invite's LAN addr, if any, is left out) and wait for both
/// links on the relay path.
async fn pair_relay(relay: &MockRelay, a: &mut TestNode, b: &mut TestNode) {
    let slots = relay.live_slots(&a.pk());
    let mut inv = a.node.create_invite(None, TTL).unwrap();
    inv.addr = None;
    // The slot travels on a's socket, the pair_request on b's: let the relay see the slot first (a
    // user pasting the URI elsewhere never wins that race).
    eventually("the pairing slot", || relay.live_slots(&a.pk()) > slots).await;
    b.node.join(&inv.to_uri()).await.unwrap();
    let (aid, bid) = (a.id.clone(), b.id.clone());
    a.expect("relay path", is_path(&bid, LinkPath::Relay)).await;
    b.expect("relay path", is_path(&aid, LinkPath::Relay)).await;
}

// ---------- tests ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_auth_status_and_config_rules() {
    let relay = MockRelay::start().await;
    let mut a = node(A, fast());
    assert_eq!(a.node.relay_status(), RelayStatusInfo::default());
    let cfg = |url: &str, key: Option<&str>| Some(RelayConfig { url: url.into(), access_key: key.map(String::from) });
    assert_eq!(a.node.set_relay(cfg("ws://relay.example", Some(KEY))), Err(RelayConfigError::BadUrl));
    assert_eq!(a.node.set_relay(cfg(&relay.url(), Some("not a key"))), Err(RelayConfigError::BadKey));
    assert_eq!(a.node.relay_status(), RelayStatusInfo::default(), "a refused config changes nothing");

    a.node.set_relay(Some(relay.config())).unwrap();
    let first = a.expect("status", |e| matches!(e, PeerEvent::RelayStatus(_))).await;
    assert_eq!(
        first,
        PeerEvent::RelayStatus(RelayStatusInfo { url: Some(relay.url()), connected: false, last_error: None })
    );
    a.expect("connected", is_relay_up).await;
    assert_eq!(
        a.node.relay_status(),
        RelayStatusInfo { url: Some(relay.url()), connected: true, last_error: None }
    );
    assert_eq!(relay.auths(&a.pk()), 1);
    assert!(relay.allowed(&a.pk()).is_empty(), "nothing paired yet");

    // The same config again is a no-op (no reconnect).
    a.node.set_relay(cfg(&format!("{}/", relay.url()), Some(KEY))).unwrap();
    a.expect_none(Duration::from_millis(300), "a status change", |e| matches!(e, PeerEvent::RelayStatus(_))).await;
    assert_eq!(relay.auths(&a.pk()), 1);

    // Pings keep the session alive well past offline_after.
    let t = PeerTimings { offline_after: Duration::from_millis(800), ..fast() };
    let mut b = node(B, t);
    b.node.set_relay(Some(relay.config())).unwrap();
    b.expect("connected", is_relay_up).await;
    b.expect_none(Duration::from_millis(2000), "a disconnect", is_relay_down).await;
    assert_eq!(relay.auths(&b.pk()), 1);

    // Off.
    a.node.set_relay(None).unwrap();
    a.expect("off", |e| matches!(e, PeerEvent::RelayStatus(s) if s.url.is_none() && !s.connected)).await;
    assert_eq!(a.node.relay_status(), RelayStatusInfo::default());
    eventually("the relay socket to close", || !relay.online(&a.pk())).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_refusals_are_reported_and_retried() {
    let relay = MockRelay::start().await;
    let mut a = node(A, fast());
    a.node.set_relay(Some(RelayConfig { url: relay.url(), access_key: Some("wrong".into()) })).unwrap();
    a.expect("bad_key", is_relay_error("bad_key")).await;
    assert_eq!(relay.auths(&a.pk()), 0);

    // An auth for another host (a relay replaying our answer elsewhere) does not open.
    relay.set_host_override(Some("relay.example"));
    a.node.set_relay(Some(relay.config())).unwrap();
    a.expect("bad_auth", is_relay_error("bad_auth")).await;
    assert_eq!(relay.auths(&a.pk()), 0);
    assert!(!a.node.relay_status().connected);

    // It keeps retrying with backoff and gets in once the relay agrees.
    relay.set_host_override(None);
    a.expect("connected", is_relay_up).await;
    assert_eq!(a.node.relay_status().last_error, None);

    // A newer socket with the same key takes the mailbox: the old one reports `replaced`.
    let path = std::env::temp_dir().join(format!("krypto-relay-key-{}", hex::encode(getrandom::u64().unwrap().to_be_bytes())));
    let k1 = DeviceKeys::load_or_create(&path).unwrap();
    let mut b1 = node_with(B, k1, Arc::new(MemoryRoster::new()), fast());
    b1.node.set_relay(Some(relay.config())).unwrap();
    b1.expect("connected", is_relay_up).await;
    let mut b2 = node_with(B, DeviceKeys::load_or_create(&path).unwrap(), Arc::new(MemoryRoster::new()), fast());
    b2.node.set_relay(Some(relay.config())).unwrap();
    b2.expect("connected", is_relay_up).await;
    b1.expect("replaced", is_relay_error("replaced")).await;
    b1.node.set_relay(None).unwrap();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_only_pairing_link_bodies_and_allow_lists() {
    let relay = MockRelay::start().await;
    let mut a = on_relay(A, &relay, fast()).await;
    let mut b = on_relay(B, &relay, fast()).await;
    let inv = a.node.create_invite(None, TTL).unwrap();
    assert_eq!(inv.addr, None, "no listener: relay only");
    assert_eq!(inv.relay.as_deref(), Some(relay.url().as_str()));
    assert!(inv.relay_token.is_some());
    assert_eq!(inv.relay_key.as_deref(), Some(KEY));
    eventually("the pairing slot", || relay.live_slots(&a.pk()) == 1).await;
    let uri = inv.to_uri();

    let joined = b.node.join_invite(&uri).await.unwrap();
    assert_eq!(joined.device.id, A);
    assert_eq!(joined.device.public_key, a.node.public_b64());
    assert_eq!(joined.device.addr, None);
    assert_eq!(joined.adopted_relay, None, "b was on this relay already");
    assert_eq!(relay.live_slots(&a.pk()), 0, "the pairing slot is one-shot");
    match a.expect("Paired", |e| matches!(e, PeerEvent::Paired { .. })).await {
        PeerEvent::Paired { device, we_invited, .. } => {
            assert_eq!(device.id, B);
            assert!(we_invited);
        }
        _ => unreachable!(),
    }
    a.expect("LinkUp", is_link_up(B)).await;
    a.expect("relay path", is_path(B, LinkPath::Relay)).await;
    b.expect("LinkUp", is_link_up(A)).await;
    b.expect("relay path", is_path(A, LinkPath::Relay)).await;
    assert_eq!(a.node.link_path(B), Some(LinkPath::Relay));
    assert_eq!(b.node.link_path(A), Some(LinkPath::Relay));
    assert!(a.position(is_link_up(B)) < a.position(is_path(B, LinkPath::Relay)));

    // Each mailbox lets the other in, and nobody else.
    assert_eq!(relay.allowed(&a.pk()), HashSet::from([address(&b.pk())]));
    assert_eq!(relay.allowed(&b.pk()), HashSet::from([address(&a.pk())]));

    // Bodies both ways, in order; presence flows.
    for i in 0..60 {
        assert_eq!(a.node.send(B, sync_request(i)), Ok(()));
        assert_eq!(b.node.send(A, sync_request(i)), Ok(()));
    }
    let want: Vec<PeerBody> = (0..60).map(sync_request).collect();
    assert_eq!(b.bodies(A, 60).await, want);
    assert_eq!(a.bodies(B, 60).await, want);
    a.expect("Presence", is_presence(B)).await;
    b.expect("Presence", is_presence(A)).await;
    assert_eq!(a.node.send(B, body_of_size(MAX_SEND_BODY_BYTES)), Ok(()), "the largest body fits a relay frame");
    assert_eq!(b.bodies(A, 1).await.len(), 1);

    // The slot and the code were single use: a second joiner is refused at once (not_allowed).
    let mut c = on_relay(C, &relay, fast()).await;
    let started = Instant::now();
    assert_eq!(c.node.join(&uri).await, Err(JoinError::Rejected));
    assert!(started.elapsed() < JOIN_TIMEOUT, "refused only after {:?}", started.elapsed());
    assert!(c.roster.list().is_empty());
    c.expect_none(Duration::from_millis(300), "a link", is_link_up(A)).await;
    eventually("c to drop a's key from its allow list", || relay.allowed(&c.pk()).is_empty()).await;

    // Without a listener or a relay there is nothing to invite to.
    a.node.set_relay(None).unwrap();
    assert_eq!(a.node.create_invite(None, TTL), Err(InviteError::ListenerOff));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_restart_drops_relay_links_then_they_come_back() {
    let relay = MockRelay::start().await;
    let mut a = on_relay(A, &relay, fast()).await;
    let mut b = on_relay(B, &relay, fast()).await;
    pair_relay(&relay, &mut a, &mut b).await;

    relay.kick_all();
    a.expect("relay down", is_relay_down).await;
    a.expect("LinkDown", is_link_down(B)).await;
    b.expect("LinkDown", is_link_down(A)).await;
    // Both reconnect, say hello, and the link is back on the relay path.
    a.expect("relay up", is_relay_up).await;
    a.expect("LinkUp again", is_link_up(B)).await;
    a.expect("relay path", is_path(B, LinkPath::Relay)).await;
    b.expect("LinkUp again", is_link_up(A)).await;
    assert!(relay.auths(&a.pk()) >= 2 && relay.auths(&b.pk()) >= 2);
    assert_eq!(a.node.send(B, sync_request(5)), Ok(()));
    assert_eq!(b.bodies(A, 1).await, [sync_request(5)]);

    // Turning the relay off drops the relay links at once.
    a.node.set_relay(None).unwrap();
    a.expect("LinkDown", is_link_down(B)).await;
    assert_eq!(a.node.link_path(B), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_restart_over_relay_relinks_without_link_down() {
    let relay = MockRelay::start().await;
    let mut a = on_relay(A, &relay, fast()).await;
    let path = std::env::temp_dir().join(format!("krypto-relay-restart-{}", hex::encode(getrandom::u64().unwrap().to_be_bytes())));
    let roster_b = Arc::new(MemoryRoster::new());
    let mut b1 = node_with(B, DeviceKeys::load_or_create(&path).unwrap(), roster_b.clone(), fast());
    b1.node.set_relay(Some(relay.config())).unwrap();
    b1.expect("connected", is_relay_up).await;
    pair_relay(&relay, &mut a, &mut b1).await;
    a.log.clear();

    // b "crashes" (no bye): its relay socket goes away; a still has the link (it cannot tell yet).
    b1.node.set_relay(None).unwrap();
    // The same identity and roster come back and say hello over the relay.
    let mut b2 = node_with(B, DeviceKeys::load_or_create(&path).unwrap(), roster_b, fast());
    b2.node.set_relay(Some(relay.config())).unwrap();
    b2.node.connect_known();
    b2.expect("LinkUp", is_link_up(A)).await;
    // a resyncs (LinkUp on the live link) without ever reporting it down.
    a.expect("LinkUp (peer re-dialed)", is_link_up(B)).await;
    assert!(a.position(is_link_down(B)).is_none());
    assert_eq!(b2.node.send(A, sync_request(9)), Ok(()));
    assert_eq!(a.bodies(B, 1).await, [sync_request(9)]);
    assert_eq!(a.node.send(B, sync_request(10)), Ok(()));
    assert_eq!(b2.bodies(A, 1).await, [sync_request(10)]);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lan_takes_over_and_falls_back_to_relay_without_link_down() {
    let relay = MockRelay::start().await;
    let mut a = on_relay(A, &relay, fast()).await;
    a.node.listen(local()).await.unwrap();
    let mut b = on_relay(B, &relay, fast()).await;
    b.node.listen(local()).await.unwrap();
    // Paired over the relay; b advertised its listener in the pair_request, so a dials it and the LAN
    // takes over on both sides.
    pair_relay(&relay, &mut a, &mut b).await;
    a.expect("LAN path", is_path(B, LinkPath::Lan)).await;
    b.expect("LAN path", is_path(A, LinkPath::Lan)).await;
    assert_eq!(a.node.link_path(B), Some(LinkPath::Lan));

    // The LAN socket drops: a carries on over the relay (LinkUp + LinkPath relay, no LinkDown), then
    // redials and the LAN takes over again. (Bodies in flight across a path switch or a tie-break may
    // be lost; the LinkUp of every switch makes the hub resync.)
    assert!(a.node.test_drop_lan(B));
    a.expect("relay path", is_path(B, LinkPath::Relay)).await;
    a.expect("LAN again", is_path(B, LinkPath::Lan)).await;
    b.expect("LAN again", is_path(A, LinkPath::Lan)).await;
    link_works(&a, &mut b).await;
    link_works(&b, &mut a).await;
    assert!(a.position(is_link_down(B)).is_none(), "{:#?}", a.log);
    assert!(b.position(is_link_down(A)).is_none(), "{:#?}", b.log);
    // Whoever dialed first keeps the link (a dial loop does not dial while a LAN link is up; the id
    // tie-break only settles simultaneous dials). Wait until the two sides agree and stay put.
    settle(&a, &b).await;
    let a_dialed = a.node.test_link_dialed_by_me(B) == Some(true);
    let (dialer, acceptor) = if a_dialed { (&mut a, &mut b) } else { (&mut b, &mut a) };
    let (did, aid) = (dialer.id.clone(), acceptor.id.clone());

    // Now take the LAN away for good. The dialer stops listening first: its link is its own dial, so
    // it stays. Then the acceptor stops listening: the link it accepted closes (LinkDown there, as
    // stop_listening promises); the dialer falls back to the relay without a LinkDown, and nobody can
    // dial anymore.
    dialer.node.stop_listening().await;
    assert_eq!(dialer.node.link_path(&aid), Some(LinkPath::Lan));
    dialer.log.clear();
    acceptor.node.stop_listening().await;
    acceptor.expect("LinkDown", is_link_down(&did)).await;
    dialer.expect("relay path", is_path(&aid, LinkPath::Relay)).await;
    eventually("the acceptor back on the relay", || acceptor.node.link_path(&did) == Some(LinkPath::Relay)).await;
    link_works(dialer, acceptor).await;
    link_works(acceptor, dialer).await;
    dialer.expect_none(Duration::from_millis(500), "a LinkDown", is_link_down(&aid)).await;
    assert_eq!(dialer.node.link_path(&aid), Some(LinkPath::Relay));
    assert_eq!(acceptor.node.link_path(&did), Some(LinkPath::Relay));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relayed_frame_must_come_from_the_roster_key() {
    let relay = MockRelay::start().await;
    let mut a = on_relay(A, &relay, fast()).await;
    let mut b = on_relay(B, &relay, fast()).await;
    pair_relay(&relay, &mut a, &mut b).await;
    let c = DeviceKeys::generate();

    // A genuine envelope from b, but the relay names another sender: dropped.
    // 2 s ahead of b's own seq, so b's presence frames do not overtake it while the test waits (they
    // are dropped as replays until then, well inside offline_after).
    let env = seal(&b.node.inner.keys, B, A, &a.pk(), time_seq() + 2_000_000, &sync_request(99), false);
    relay.inject(&a.pk(), &address(&c.public_key()), &env);
    a.expect_none(Duration::from_millis(400), "a Body from a forged sender", is_body_from(B)).await;
    // The same envelope from b's own address goes through: only the binding stopped it.
    relay.inject(&a.pk(), &address(&b.pk()), &env);
    assert_eq!(a.bodies(B, 1).await, [sync_request(99)]);

    // A pair_request whose envelope key is not its relay sender: no reply, no event.
    let live = a.node.create_invite(None, TTL).unwrap();
    let req = PeerBody::PairRequest { code: live.code.clone(), machine: machine(C, "C"), addr: None, ts: now_ms() };
    let forged = seal(&c, C, A, &a.pk(), time_seq(), &req, true);
    relay.inject(&a.pk(), &address(&b.pk()), &forged);
    a.expect_none(Duration::from_millis(400), "pairing with a forged sender", |e| {
        matches!(e, PeerEvent::Paired { .. } | PeerEvent::PairRejected { .. })
    })
    .await;
    assert!(a.roster.get(C).is_none());
    assert_eq!(a.node.test_live_invites(), 1, "the invite was not used");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn join_falls_back_to_the_invites_relay_and_adopts_it() {
    let relay = MockRelay::start().await;
    let mut a = on_relay(A, &relay, fast()).await;
    a.node.listen(local()).await.unwrap();
    let mut inv = a.node.create_invite(None, TTL).unwrap();
    // A LAN addr nobody answers on (refused at once).
    let free = std::net::TcpListener::bind(local()).unwrap().local_addr().unwrap();
    inv.addr = Some(free.to_string());
    eventually("the pairing slot", || relay.live_slots(&a.pk()) == 1).await;

    // b has no relay: it uses the invite's for the join and keeps it.
    let mut b = node(B, fast());
    let joined = b.node.join_invite(&inv.to_uri()).await.unwrap();
    assert_eq!(joined.adopted_relay, Some(relay.config()));
    assert_eq!(
        b.node.relay_status(),
        RelayStatusInfo { url: Some(relay.url()), connected: true, last_error: None }
    );
    b.expect("relay path", is_path(A, LinkPath::Relay)).await;
    a.expect("relay path", is_path(B, LinkPath::Relay)).await;
    assert_eq!(b.roster.get(A).unwrap().device.addr, Some(free.to_string()));

    // A relay that does not let us in: the join fails and the adopted relay is off again.
    let mut inv = a.node.create_invite(None, TTL).unwrap();
    inv.addr = None;
    inv.relay_key = Some("wrong".into());
    let c = node(C, fast());
    assert_eq!(c.node.join(&inv.to_uri()).await, Err(JoinError::Unreachable));
    assert_eq!(c.node.relay_status().url, None);

    // A joiner on another relay does not switch relays.
    let other = MockRelay::start().await;
    let d = on_relay("dddddddddddd", &other, fast()).await;
    assert_eq!(d.node.join(&inv.to_uri()).await, Err(JoinError::Unreachable));
    assert_eq!(d.node.relay_status().url, Some(other.url()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoke_over_relay_reaches_the_peer_and_the_allow_lists() {
    let relay = MockRelay::start().await;
    let mut a = on_relay(A, &relay, fast()).await;
    let mut b = on_relay(B, &relay, fast()).await;
    pair_relay(&relay, &mut a, &mut b).await;
    a.node.revoke(B).await;
    a.expect("Revoked", |e| matches!(e, PeerEvent::Revoked { by_peer: false, .. })).await;
    b.expect("Revoked by peer", |e| matches!(e, PeerEvent::Revoked { by_peer: true, .. })).await;
    assert!(b.roster.get(A).is_none());
    eventually("a to drop b from its allow list", || relay.allowed(&a.pk()).is_empty()).await;
    eventually("b to drop a from its allow list", || relay.allowed(&b.pk()).is_empty()).await;
    a.expect_none(Duration::from_millis(500), "a relink", is_link_up(B)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_drops_relay_frames_during_the_cooldown() {
    let relay = MockRelay::start().await;
    let mut a = on_relay(A, &relay, fast()).await;
    let mut b = on_relay(B, &relay, fast()).await;
    pair_relay(&relay, &mut a, &mut b).await;
    a.log.clear();

    let cooldown = Duration::from_millis(2500);
    let start = Instant::now();
    a.node.disconnect(B, cooldown);
    a.expect("LinkDown", is_link_down(B)).await;
    // b does not notice (no socket to close) and keeps sending presence over the relay; a drops it.
    let quiet = cooldown.saturating_sub(start.elapsed()).saturating_sub(Duration::from_millis(400));
    a.expect_none(quiet, "LinkUp during the cooldown", is_link_up(B)).await;
    assert!(a.node.online().is_empty());
    // Afterwards b's next frame brings the link back; a's hello makes b resync.
    a.expect("LinkUp after the cooldown", is_link_up(B)).await;
    assert!(start.elapsed() >= cooldown, "relinked after {:?}", start.elapsed());
    assert_eq!(a.node.send(B, sync_request(3)), Ok(()));
    assert_eq!(b.bodies(A, 1).await, [sync_request(3)]);
}

/// Against a real relay (ignored by default): `KRYPTO_RELAY_SMOKE_URL=ws://127.0.0.1:8787` (e.g.
/// `wrangler dev` in services/relay) and `KRYPTO_RELAY_SMOKE_KEY=<access key>`, then
/// `cargo test -p krypto-peer -- --ignored relay_smoke`. Auth, allow, relay-only pairing through the
/// pairing slot, bodies both ways, a refused second join, revoke.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a running relay"]
async fn relay_smoke_against_a_real_relay() {
    let url = std::env::var("KRYPTO_RELAY_SMOKE_URL").expect("KRYPTO_RELAY_SMOKE_URL");
    let key = std::env::var("KRYPTO_RELAY_SMOKE_KEY").ok();
    let config = RelayConfig { url, access_key: key };
    let t = PeerTimings { offline_after: Duration::from_secs(30), ..fast() };
    let up = |id: &'static str| {
        let config = config.clone();
        async move {
            let mut n = node(id, t);
            n.node.set_relay(Some(config)).unwrap();
            n.expect("relay connected", is_relay_up).await;
            n
        }
    };
    let mut a = up(A).await;
    let mut b = up(B).await;
    let inv = a.node.create_invite(None, TTL).unwrap();
    assert!(inv.addr.is_none() && inv.relay.is_some() && inv.relay_token.is_some());
    // The slot goes over a's socket; give the relay a moment before b's pair_request arrives.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let joined = b.node.join_invite(&inv.to_uri()).await.unwrap();
    assert_eq!(joined.device.id, A);
    a.expect("relay path", is_path(B, LinkPath::Relay)).await;
    b.expect("relay path", is_path(A, LinkPath::Relay)).await;
    for i in 0..20 {
        assert_eq!(a.node.send(B, sync_request(i)), Ok(()));
        assert_eq!(b.node.send(A, sync_request(i)), Ok(()));
    }
    let want: Vec<PeerBody> = (0..20).map(sync_request).collect();
    assert_eq!(b.bodies(A, 20).await, want);
    assert_eq!(a.bodies(B, 20).await, want);
    a.expect("Presence over the relay", is_presence(B)).await;
    assert_eq!(a.node.send(B, body_of_size(MAX_SEND_BODY_BYTES)), Ok(()));
    assert_eq!(b.bodies(A, 1).await.len(), 1);

    let c = up(C).await;
    assert_eq!(c.node.join(&inv.to_uri()).await, Err(JoinError::Rejected), "the slot is one-shot");

    a.node.revoke(B).await;
    b.expect("Revoked by peer", |e| matches!(e, PeerEvent::Revoked { by_peer: true, .. })).await;
    for n in [&a, &b, &c] {
        n.node.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoked_while_off_the_relay_the_peer_still_learns_it() {
    let relay = MockRelay::start().await;
    let mut a = on_relay(A, &relay, fast()).await;

    // b is off the relay when a revokes it: the bye waits in b's mailbox and reaches b when it is back.
    let mut b = on_relay(B, &relay, fast()).await;
    pair_relay(&relay, &mut a, &mut b).await;
    b.node.set_relay(None).unwrap();
    b.expect("LinkDown", is_link_down(A)).await;
    a.node.revoke(B).await;
    b.node.set_relay(Some(relay.config())).unwrap();
    b.expect("Revoked by peer", |e| matches!(e, PeerEvent::Revoked { device_id, by_peer: true } if device_id == A))
        .await;
    assert!(b.roster.is_revoked(A));

    // The queued bye is gone (expired): c's own frames get through only while a's allow list lags,
    // and then a answers them with bye{revoked} (forged senders get nothing).
    let mut c = on_relay(C, &relay, fast()).await;
    pair_relay(&relay, &mut a, &mut c).await;
    c.node.set_relay(None).unwrap();
    c.expect("LinkDown", is_link_down(A)).await;
    a.node.revoke(C).await;
    eventually("a to drop c from its allow list", || !relay.allowed(&a.pk()).contains(&address(&c.pk()))).await;
    relay.clear_queue(&c.pk());
    c.node.set_relay(Some(relay.config())).unwrap();
    c.expect("relay up", is_relay_up).await;
    c.expect_none(Duration::from_millis(400), "Revoked (nothing reached c yet)", |e| {
        matches!(e, PeerEvent::Revoked { .. })
    })
    .await;
    let hello = PeerBody::Hello { machine: machine(C, "C"), addr: None, ts: now_ms() };
    let env = seal(&c.node.inner.keys, C, A, &a.pk(), time_seq() + 2_000_000, &hello, false);
    relay.inject(&a.pk(), &address(&DeviceKeys::generate().public_key()), &env);
    c.expect_none(Duration::from_millis(400), "Revoked from a forged sender", |e| {
        matches!(e, PeerEvent::Revoked { .. })
    })
    .await;
    relay.inject(&a.pk(), &address(&c.pk()), &env);
    c.expect("Revoked by peer", |e| matches!(e, PeerEvent::Revoked { device_id, by_peer: true } if device_id == A))
        .await;
    assert!(c.roster.is_revoked(A));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_relay_challenges_are_refused() {
    let relay = MockRelay::start().await;
    let mut a = node(A, fast());
    let peer = DeviceKeys::generate();
    add_paired(&a.roster, B, &peer.public_key(), None);
    // A nonce from the envelope domain, our own key, a peer's key: no auth box is ever made.
    relay.set_hostile(Some(Hostile::EnvelopeNonce));
    a.node.set_relay(Some(relay.config())).unwrap();
    a.expect("bad_auth", is_relay_error("bad_auth")).await;
    for hostile in [Hostile::EnvelopeNonce, Hostile::RelayPk(a.pk()), Hostile::RelayPk(peer.public_key())] {
        relay.set_hostile(Some(hostile));
        let (challenges, _) = relay.handshakes();
        eventually("another refused challenge", || relay.handshakes().0 >= challenges + 2).await;
        assert_eq!(relay.handshakes().1, 0, "{hostile:?}: the client answered");
        assert!(!a.node.relay_status().connected);
        assert_eq!(a.node.relay_status().last_error.as_deref(), Some("bad_auth"));
    }
    // An honest challenge again: in.
    relay.set_hostile(None);
    a.expect("connected", is_relay_up).await;
    assert_eq!(relay.handshakes().1, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_adopted_relay_sees_only_the_inviter_until_pairing_succeeds() {
    let relay = MockRelay::start().await;
    let a = on_relay(A, &relay, fast()).await;
    let inv = a.node.create_invite(None, TTL).unwrap();
    eventually("the pairing slot", || relay.live_slots(&a.pk()) == 1).await;
    // b has another peer, x, and no relay of its own.
    let mut b = node(B, fast());
    let x = DeviceKeys::generate();
    add_paired(&b.roster, C, &x.public_key(), None);
    let joined = b.node.join_invite(&inv.to_uri()).await.unwrap();
    assert_eq!(joined.adopted_relay, Some(relay.config()));
    b.expect("relay path", is_path(A, LinkPath::Relay)).await;
    let (a_addr, x_addr) = (address(&a.pk()), address(&x.public_key()));
    // Afterwards the relay is b's: x is allowed and greeted.
    eventually("x allowed", || relay.allowed(&b.pk()).contains(&x_addr)).await;
    eventually("a hello to x", || relay.sent_by(&b.pk()).contains(&Sent::Send(x_addr.clone()))).await;
    // Before the pairing went through, nothing about x reached the invite's relay.
    let log = relay.sent_by(&b.pk());
    let mentions_x = |s: &Sent| match s {
        Sent::Allow(set) => set.contains(&x_addr),
        Sent::Send(to) => *to == x_addr,
    };
    let first_x = log.iter().position(mentions_x).unwrap();
    let pair_request = log.iter().position(|s| *s == Sent::Send(a_addr.clone())).unwrap();
    assert!(pair_request < first_x, "{log:#?}");
    assert!(matches!(&log[first_x], Sent::Allow(_)), "x is allowed before it is greeted: {log:#?}");
    assert!(
        log[..pair_request].iter().all(|s| matches!(s, Sent::Allow(set) if set.iter().all(|k| *k == a_addr))),
        "only the inviter was allowed before the pair_request: {log:#?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relay_pair_attempts_share_one_bucket_and_spare_the_lan() {
    let relay = MockRelay::start().await;
    let mut a = on_relay(A, &relay, fast()).await;
    a.node.listen(local()).await.unwrap();
    let inv = a.node.create_invite(None, TTL).unwrap();
    assert!(inv.addr.is_some() && inv.relay.is_some());
    // Bad codes from as many made-up relay senders as it takes to block one IP: they all count together.
    for i in 0..PAIR_MAX_FAILURES {
        let k = DeviceKeys::generate();
        let id = format!("relay{i:07}");
        let req = PeerBody::PairRequest { code: new_code().1, machine: machine(&id, "X"), addr: None, ts: now_ms() };
        relay.inject(&a.pk(), &address(&k.public_key()), &seal(&k, &id, A, &a.pk(), time_seq(), &req, true));
    }
    a.expect("PairRejected", |e| matches!(e, PeerEvent::PairRejected { remote_addr, .. } if remote_addr == "relay")).await;
    eventually("the relay bucket to block", || {
        lock(&a.node.inner.pair_limiter).blocked(PairKey::Relay, std::time::Instant::now())
    })
    .await;
    // Even the right code from yet another relay sender is dropped now...
    let k = DeviceKeys::generate();
    let req = PeerBody::PairRequest { code: inv.code.clone(), machine: machine(C, "C"), addr: None, ts: now_ms() };
    relay.inject(&a.pk(), &address(&k.public_key()), &seal(&k, C, A, &a.pk(), time_seq(), &req, true));
    a.expect_none(Duration::from_millis(400), "pairing while blocked", |e| matches!(e, PeerEvent::Paired { .. })).await;
    assert_eq!(a.node.test_live_invites(), 1);
    // ...while the LAN listener is unaffected: b pairs with the same invite there.
    let mut b = node(B, fast());
    b.node.join(&inv.to_uri()).await.unwrap();
    a.expect("Paired over the LAN", |e| matches!(e, PeerEvent::Paired { device, .. } if device.id == B)).await;
    b.expect("LAN path", is_path(A, LinkPath::Lan)).await;
}
