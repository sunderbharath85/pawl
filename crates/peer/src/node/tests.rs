//! Node tests: real sockets on 127.0.0.1:0, in-memory rosters, short timings.

use super::*;
use crate::invite::new_code;
use crate::limits::{MAX_UNAUTH_PER_IP, MAX_UNAUTH_TOTAL, PAIR_MAX_FAILURES};
use crate::MemoryRoster;
use futures::{SinkExt, StreamExt};
use krypto_protocol::MachineOs;
use std::net::Ipv4Addr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const A: &str = "aaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbb";
const C: &str = "cccccccccccc";
const WAIT: Duration = Duration::from_secs(5);
const TTL: Duration = Duration::from_secs(600);

type RawWs = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn machine(id: &str, name: &str) -> Machine {
    Machine {
        id: id.into(),
        name: name.into(),
        os: MachineOs::current(),
        last_seen: 0,
        online: true,
        hub_version: Some("test".into()),
    }
}

fn fast() -> PeerTimings {
    PeerTimings {
        presence_interval: Duration::from_millis(200),
        offline_after: Duration::from_secs(5),
        handshake_timeout: Duration::from_millis(500),
        backoff_min: Duration::from_millis(50),
        backoff_max: Duration::from_millis(400),
    }
}

fn local() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

struct TestNode {
    id: String,
    node: PeerNode,
    roster: Arc<MemoryRoster>,
    events: UnboundedReceiver<PeerEvent>,
    /// Every event received so far, in order.
    log: Vec<PeerEvent>,
}

fn node(id: &str, t: PeerTimings) -> TestNode {
    node_with(id, DeviceKeys::generate(), Arc::new(MemoryRoster::new()), t)
}

fn node_with(id: &str, keys: DeviceKeys, roster: Arc<MemoryRoster>, t: PeerTimings) -> TestNode {
    let (tx, rx) = unbounded_channel();
    let node = PeerNode::new(keys, machine(id, &format!("Machine {id}")), roster.clone(), tx).with_timings(t);
    TestNode { id: id.into(), node, roster, events: rx, log: Vec::new() }
}

async fn listening(id: &str, t: PeerTimings) -> (TestNode, SocketAddr) {
    let n = node(id, t);
    let addr = n.node.listen(local()).await.unwrap();
    (n, addr)
}

impl TestNode {
    fn pk(&self) -> [u8; 32] {
        self.node.inner.keys.public_key()
    }

    /// Next event matching `pred` (others are logged and skipped).
    async fn expect(&mut self, what: &str, pred: impl Fn(&PeerEvent) -> bool) -> PeerEvent {
        let deadline = Instant::now() + WAIT;
        loop {
            match tokio::time::timeout_at(deadline, self.events.recv()).await {
                Ok(Some(e)) => {
                    self.log.push(e.clone());
                    if pred(&e) {
                        return e;
                    }
                }
                Ok(None) => panic!("{}: event channel closed waiting for {what}", self.id),
                Err(_) => panic!("{}: timed out waiting for {what}; got {:#?}", self.id, self.log),
            }
        }
    }

    /// No event matching `pred` for `d`.
    async fn expect_none(&mut self, d: Duration, what: &str, pred: impl Fn(&PeerEvent) -> bool) {
        let deadline = Instant::now() + d;
        loop {
            match tokio::time::timeout_at(deadline, self.events.recv()).await {
                Ok(Some(e)) => {
                    self.log.push(e.clone());
                    assert!(!pred(&e), "{}: unexpected {what}: {e:#?}", self.id);
                }
                Ok(None) | Err(_) => return,
            }
        }
    }

    fn position(&self, pred: impl Fn(&PeerEvent) -> bool) -> Option<usize> {
        self.log.iter().position(pred)
    }
}

fn is_link_up(id: &str) -> impl Fn(&PeerEvent) -> bool + '_ {
    move |e| matches!(e, PeerEvent::LinkUp { device_id, .. } if device_id == id)
}
fn is_link_down(id: &str) -> impl Fn(&PeerEvent) -> bool + '_ {
    move |e| matches!(e, PeerEvent::LinkDown { device_id } if device_id == id)
}
fn is_presence(id: &str) -> impl Fn(&PeerEvent) -> bool + '_ {
    move |e| matches!(e, PeerEvent::Presence { device_id, .. } if device_id == id)
}
fn is_presence_named<'a>(id: &'a str, name: &'a str) -> impl Fn(&PeerEvent) -> bool + 'a {
    move |e| matches!(e, PeerEvent::Presence { device_id, presence } if device_id == id && presence.machine.name == name)
}

/// Pair `b` with listening `a` and wait for both links.
async fn pair(a: &mut TestNode, b: &mut TestNode) {
    let inv = a.node.create_invite(None, TTL).unwrap();
    b.node.join(&inv.to_uri()).await.unwrap();
    let (aid, bid) = (a.id.clone(), b.id.clone());
    a.expect("LinkUp", is_link_up(&bid)).await;
    b.expect("LinkUp", is_link_up(&aid)).await;
}

fn presence_body(id: &str, name: &str) -> PeerBody {
    PeerBody::Presence {
        presence: Presence {
            machine: machine(id, name),
            agents: vec![],
            repos: vec![],
            pets: vec![],
            user_active_at: None,
            ts: 1,
        },
    }
}

fn hello_body(id: &str) -> PeerBody {
    PeerBody::Hello { machine: machine(id, "Raw"), addr: None, ts: now_ms() }
}

fn env_text(env: &PeerEnvelope) -> Message {
    Message::text(serde_json::to_string(env).unwrap())
}

async fn raw(addr: SocketAddr) -> RawWs {
    tokio_tungstenite::connect_async(format!("ws://{addr}{PEER_PATH}")).await.unwrap().0
}

#[derive(Debug)]
enum RawNext {
    Text(String),
    Closed,
    Silent,
}

async fn raw_next(ws: &mut RawWs, within: Duration) -> RawNext {
    let deadline = Instant::now() + within;
    loop {
        match tokio::time::timeout_at(deadline, ws.next()).await {
            Err(_) => return RawNext::Silent,
            Ok(None) | Ok(Some(Err(_))) | Ok(Some(Ok(Message::Close(_)))) => return RawNext::Closed,
            Ok(Some(Ok(Message::Text(t)))) => return RawNext::Text(t.as_str().to_owned()),
            Ok(Some(Ok(_))) => {}
        }
    }
}

/// A raw peer socket from a chosen loopback source address; None if this OS cannot bind it.
async fn raw_from(src: Ipv4Addr, addr: SocketAddr) -> Option<RawWs> {
    let sock = TcpSocket::new_v4().ok()?;
    sock.bind(SocketAddr::new(src.into(), 0)).ok()?;
    let tcp = sock.connect(addr).await.ok()?;
    let (ws, _) = tokio_tungstenite::client_async(format!("ws://{addr}{PEER_PATH}"), MaybeTlsStream::Plain(tcp)).await.ok()?;
    Some(ws)
}

fn is_pair_rejected(e: &PeerEvent) -> bool {
    matches!(e, PeerEvent::PairRejected { .. })
}

/// A listener that impersonates inviter `a` (it holds a's key, like an attacker replaying a's frames
/// would hold a's old ciphertexts): answers the first pair_request with `reply(a, request)`.
async fn fake_inviter(
    a: PeerNode,
    reply: impl FnOnce(&PeerNode, &PeerEnvelope) -> PeerEnvelope + Send + 'static,
) -> SocketAddr {
    let l = TcpListener::bind(local()).await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        let (tcp, _) = l.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(tcp).await.unwrap();
        let Some(Ok(Message::Text(t))) = ws.next().await else { return };
        let req: PeerEnvelope = serde_json::from_str(t.as_str()).unwrap();
        let out = reply(&a, &req);
        let _ = ws.send(env_text(&out)).await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    });
    addr
}

/// Wait until the socket is closed without any text frame.
async fn assert_closed_silently(ws: &mut RawWs, within: Duration) {
    match raw_next(ws, within).await {
        RawNext::Closed => {}
        other => panic!("expected close without reply, got {other:?}"),
    }
}

/// Register a pairing directly in `roster` (as if `id` had paired with key `pk`).
fn add_paired(roster: &MemoryRoster, id: &str, pk: &[u8; 32], addr: Option<String>) {
    roster.upsert(RosterEntry {
        device: Device {
            id: id.into(),
            kind: DeviceKind::Hub,
            name: "Raw".into(),
            public_key: b64::encode(pk),
            paired_at: 1,
            last_seen: None,
            addr,
        },
        machine: machine(id, "Raw"),
    });
}

async fn http_status(addr: SocketAddr, request: &str) -> String {
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(request.as_bytes()).await.unwrap();
    let mut buf = vec![0u8; 256];
    let n = timeout(WAIT, s.read(&mut buf)).await.unwrap().unwrap();
    String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or_default().to_string()
}

fn ws_http_status(err: WsError) -> u16 {
    match err {
        WsError::Http(resp) => resp.status().as_u16(),
        other => panic!("expected an HTTP error, got {other}"),
    }
}

// ---------- pairing ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pair_two_nodes_link_up_and_presence_both_ways() {
    let (mut a, _) = listening(A, fast()).await;
    let mut b = node(B, fast());
    let inv = a.node.create_invite(None, TTL).unwrap();
    assert_eq!(inv.machine_id, A);
    assert_eq!(inv.public_key, a.pk());
    let (dev, m) = b.node.join(&inv.to_uri()).await.unwrap();
    assert_eq!(dev.id, A);
    assert_eq!(dev.public_key, a.node.public_b64());
    assert_eq!(dev.addr.as_deref(), Some(inv.addr.as_str()));
    assert_eq!(m.id, A);
    assert!(m.online);

    match b.expect("Paired", |e| matches!(e, PeerEvent::Paired { .. })).await {
        PeerEvent::Paired { device, we_invited, .. } => {
            assert_eq!(device.id, A);
            assert!(!we_invited);
        }
        _ => unreachable!(),
    }
    b.expect("LinkUp", is_link_up(A)).await;
    b.expect("Presence", is_presence(A)).await;
    match a.expect("Paired", |e| matches!(e, PeerEvent::Paired { .. })).await {
        PeerEvent::Paired { device, machine, we_invited } => {
            assert_eq!(device.id, B);
            assert_eq!(device.public_key, b.node.public_b64());
            assert_eq!(device.addr, None, "b has no listener");
            assert_eq!(machine.name, "Machine bbbbbbbbbbbb");
            assert!(we_invited);
        }
        _ => unreachable!(),
    }
    a.expect("LinkUp", is_link_up(B)).await;
    a.expect("Presence", is_presence(B)).await;
    // Paired comes before LinkUp.
    assert!(a.position(|e| matches!(e, PeerEvent::Paired { .. })) < a.position(is_link_up(B)));

    assert_eq!(a.node.test_live_invites(), 0, "invite is single use");
    assert_eq!(a.roster.get(B).unwrap().device.public_key, b.node.public_b64());
    assert_eq!(b.roster.get(A).unwrap().device.addr.as_deref(), Some(inv.addr.as_str()));
    assert_eq!(a.node.online().iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), [B]);
    assert_eq!(b.node.online().iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), [A]);

    // Heartbeats keep coming.
    b.expect("second Presence", is_presence(A)).await;

    // A snapshot change is pushed at once and renames the machine in the peer's roster.
    a.node.set_presence(Presence {
        machine: machine(A, "Renamed"),
        agents: vec![],
        repos: vec![],
        pets: vec![],
        user_active_at: Some(5),
        ts: 0,
    });
    match b.expect("renamed Presence", is_presence_named(A, "Renamed")).await {
        PeerEvent::Presence { presence, .. } => {
            assert_eq!(presence.user_active_at, Some(5));
            assert!(presence.ts > 0, "ts is set at send time");
        }
        _ => unreachable!(),
    }
    assert_eq!(b.roster.get(A).unwrap().device.name, "Renamed");
    // set_machine also pushes.
    a.node.set_machine(machine(A, "Renamed again"));
    b.expect("set_machine Presence", is_presence_named(A, "Renamed again")).await;

    // `send` reaches the live link only.
    assert!(a.node.send(B, presence_body(A, "Direct")));
    b.expect("direct", is_presence_named(A, "Direct")).await;
    assert!(!a.node.send(C, presence_body(A, "Nobody")));

    a.node.shutdown().await;
    b.expect("LinkDown after shutdown", is_link_down(A)).await;
    b.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_code_rejected_invite_still_usable() {
    let (mut a, _) = listening(A, fast()).await;
    let mut b = node(B, fast());
    let inv = a.node.create_invite(None, TTL).unwrap();
    let mut wrong = inv.clone();
    wrong.code = new_code().1;
    assert_eq!(b.node.join(&wrong.to_uri()).await, Err(JoinError::Rejected));
    match a.expect("PairRejected", is_pair_rejected).await {
        PeerEvent::PairRejected { reason, remote_addr } => {
            assert_eq!(reason, PairRejectReason::InvalidInvite);
            assert!(remote_addr.starts_with("127.0.0.1:"));
        }
        _ => unreachable!(),
    }
    assert!(a.roster.get(B).is_none());
    // A wrong code burns nothing (anyone knowing a's key could otherwise block pairing).
    assert_eq!(a.node.test_live_invites(), 1);
    b.node.join(&inv.to_uri()).await.unwrap();
    a.expect("LinkUp", is_link_up(B)).await;
    b.expect("LinkUp", is_link_up(A)).await;
    // A correct code consumes it.
    assert_eq!(a.node.test_live_invites(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pair_failures_rate_limited_per_ip_and_events_coalesced() {
    let (mut a, addr) = listening(A, fast()).await;
    let b = node(B, fast());
    let inv = a.node.create_invite(None, TTL).unwrap();
    let mut wrong = inv.clone();
    wrong.code = new_code().1;
    for _ in 0..PAIR_MAX_FAILURES {
        assert_eq!(b.node.join(&wrong.to_uri()).await, Err(JoinError::Rejected));
    }
    // One event (audit entry) for the whole burst.
    a.expect("PairRejected", is_pair_rejected).await;
    a.expect_none(Duration::from_millis(300), "a second PairRejected", is_pair_rejected).await;

    // Blocked now: pair requests from this IP are dropped without a reply, even with the right code,
    // and the invite survives.
    let c_keys = DeviceKeys::generate();
    let mut ws = raw(addr).await;
    let body = PeerBody::PairRequest { code: inv.code.clone(), machine: machine(C, "C"), addr: None, ts: 1 };
    ws.send(env_text(&seal(&c_keys, C, A, &a.pk(), time_seq(), &body, true))).await.unwrap();
    assert_closed_silently(&mut ws, WAIT).await;
    assert_eq!(b.node.join(&inv.to_uri()).await, Err(JoinError::Rejected));
    assert_eq!(a.node.test_live_invites(), 1);
    assert!(a.roster.list().is_empty());
    a.expect_none(Duration::from_millis(200), "PairRejected while blocked", is_pair_rejected).await;

    // Once the block lapses (cleared here instead of waiting a minute) the invite still works.
    a.node.test_clear_pair_limits();
    b.node.join(&inv.to_uri()).await.unwrap();
    a.expect("LinkUp", is_link_up(B)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_invite_rejected() {
    let (mut a, _) = listening(A, fast()).await;
    let b = node(B, fast());
    let inv = a.node.create_invite(None, TTL).unwrap();
    a.node.test_expire_invites();
    assert_eq!(b.node.join(&inv.to_uri()).await, Err(JoinError::Rejected));
    // No live invite: closed without a reply, nothing to audit.
    a.expect_none(Duration::from_millis(200), "PairRejected", |e| matches!(e, PeerEvent::PairRejected { .. })).await;
    assert!(a.roster.get(B).is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reused_invite_rejected() {
    let (mut a, _) = listening(A, fast()).await;
    let mut b = node(B, fast());
    let c = node(C, fast());
    let inv = a.node.create_invite(None, TTL).unwrap();
    b.node.join(&inv.to_uri()).await.unwrap();
    b.expect("LinkUp", is_link_up(A)).await;
    assert_eq!(c.node.join(&inv.to_uri()).await, Err(JoinError::Rejected));
    assert!(a.roster.get(C).is_none());
    assert_eq!(a.roster.list().len(), 1);
    // A second invite works for c.
    let inv2 = a.node.create_invite(None, TTL).unwrap();
    assert_ne!(inv2.code, inv.code);
    c.node.join(&inv2.to_uri()).await.unwrap();
    a.expect("LinkUp c", is_link_up(C)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn join_errors() {
    let (a, addr) = listening(A, fast()).await;
    let inv = a.node.create_invite(None, TTL).unwrap();
    assert_eq!(a.node.join(&inv.to_uri()).await, Err(JoinError::SelfInvite));
    // Our own machine id under another key is still a self invite.
    let mut other_key = inv.clone();
    other_key.public_key = DeviceKeys::generate().public_key();
    assert_eq!(a.node.join(&other_key.to_uri()).await, Err(JoinError::SelfInvite));
    let b = node(B, fast());
    assert_eq!(b.node.join("nonsense").await, Err(JoinError::BadUri));
    assert_eq!(b.node.join("krypto://peer?v=1").await, Err(JoinError::BadUri));
    assert_eq!(JoinError::BadUri.to_string(), "bad_uri");
    assert_eq!(JoinError::SelfInvite.to_string(), "self_invite");
    assert_eq!(JoinError::Unreachable.to_string(), "unreachable");
    assert_eq!(JoinError::Rejected.to_string(), "invite_rejected");
    assert_eq!(JoinError::Timeout.to_string(), "timeout");

    // Nothing listening there.
    let free = std::net::TcpListener::bind(local()).unwrap().local_addr().unwrap();
    let mut dead = inv.clone();
    dead.machine_id = C.into();
    dead.addr = free.to_string();
    assert_eq!(b.node.join(&dead.to_uri()).await, Err(JoinError::Unreachable));

    // A URI for machine C whose key is a's: the reply cannot come from C, so it is rejected.
    let mut spoofed = a.node.create_invite(None, TTL).unwrap();
    spoofed.machine_id = C.into();
    assert_eq!(b.node.join(&spoofed.to_uri()).await, Err(JoinError::Rejected));
    assert!(b.roster.list().is_empty());
    assert_eq!(a.node.listening(), Some(addr));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn create_invite_rules() {
    let off = node(A, fast());
    assert_eq!(off.node.create_invite(None, TTL), Err(InviteError::ListenerOff));

    let (a, addr) = listening(A, fast()).await;
    assert_eq!(a.node.listening(), Some(addr));
    assert_eq!(a.node.create_invite(None, Duration::from_secs(59)), Err(InviteError::Invalid));
    assert_eq!(a.node.create_invite(None, Duration::from_secs(3601)), Err(InviteError::Invalid));
    assert!(a.node.create_invite(None, Duration::from_secs(60)).is_ok());
    assert!(a.node.create_invite(None, Duration::from_secs(3600)).is_ok());
    assert_eq!(a.node.create_invite(Some("not an addr".into()), TTL), Err(InviteError::Invalid));
    assert_eq!(a.node.create_invite(Some("0.0.0.0:47801".into()), TTL), Err(InviteError::Invalid));
    let inv = a.node.create_invite(Some("192.168.1.9:47801".into()), TTL).unwrap();
    assert_eq!(inv.addr, "192.168.1.9:47801");
    let before = now_ms();
    let inv = a.node.create_invite(None, TTL).unwrap();
    assert_eq!(inv.addr, addr.to_string());
    assert!((inv.expires_at - before - 600_000).abs() < 5_000);
    assert!(a.node.listen(local()).await.is_err(), "second listen is refused");

    // Listening on all interfaces needs an explicit address.
    let any = node(B, fast());
    any.node.listen("0.0.0.0:0".parse().unwrap()).await.unwrap();
    assert_eq!(any.node.create_invite(None, TTL), Err(InviteError::Invalid));
    let port = any.node.listening().unwrap().port();
    let inv = any.node.create_invite(Some(format!("127.0.0.1:{port}")), TTL).unwrap();
    assert_eq!(inv.addr, format!("127.0.0.1:{port}"));
    assert_eq!(any.node.inner.advertised_addr(), Some(format!("127.0.0.1:{port}")));
}

// ---------- links ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpaired_key_closed_without_reply() {
    let (a, addr) = listening(A, fast()).await;
    let c_keys = DeviceKeys::generate();
    let mut ws = raw(addr).await;
    let env = seal(&c_keys, C, A, &a.pk(), time_seq(), &hello_body(C), false);
    ws.send(env_text(&env)).await.unwrap();
    assert_closed_silently(&mut ws, WAIT).await;

    // A paired id with the wrong key is closed too.
    add_paired(&a.roster, B, &DeviceKeys::generate().public_key(), None);
    let mut ws = raw(addr).await;
    let env = seal(&c_keys, B, A, &a.pk(), time_seq(), &hello_body(B), false);
    ws.send(env_text(&env)).await.unwrap();
    assert_closed_silently(&mut ws, WAIT).await;

    // A pair_request while no invite is live: closed without a reply.
    let mut ws = raw(addr).await;
    let body = PeerBody::PairRequest { code: crate::invite::new_code().1, machine: machine(C, "C"), addr: None, ts: 1 };
    let env = seal(&c_keys, C, A, &a.pk(), time_seq(), &body, true);
    ws.send(env_text(&env)).await.unwrap();
    assert_closed_silently(&mut ws, WAIT).await;

    // Garbage first frame.
    let mut ws = raw(addr).await;
    ws.send(Message::text("{}")).await.unwrap();
    assert_closed_silently(&mut ws, WAIT).await;
    assert!(a.node.online().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pair_request_with_live_invite_but_bad_body_gets_bad_request() {
    let (mut a, addr) = listening(A, fast()).await;
    let inv = a.node.create_invite(None, TTL).unwrap();
    let c_keys = DeviceKeys::generate();
    let mut ws = raw(addr).await;
    // machine.id differs from `from`.
    let body = PeerBody::PairRequest { code: inv.code.clone(), machine: machine(B, "B"), addr: None, ts: 1 };
    let env = seal(&c_keys, C, A, &a.pk(), time_seq(), &body, true);
    ws.send(env_text(&env)).await.unwrap();
    let RawNext::Text(t) = raw_next(&mut ws, WAIT).await else { panic!("expected pair_reject") };
    let reply: PeerEnvelope = serde_json::from_str(&t).unwrap();
    assert_eq!((reply.from.as_str(), reply.to.as_str()), (A, C));
    assert!(matches!(
        open(&c_keys, &reply, &a.pk()).unwrap(),
        PeerBody::PairReject { reason: PairRejectReason::BadRequest, .. }
    ));
    assert_closed_silently(&mut ws, WAIT).await;
    a.expect("PairRejected", |e| {
        matches!(e, PeerEvent::PairRejected { reason: PairRejectReason::BadRequest, .. })
    })
    .await;
    // Checks before the code do not consume the invite.
    assert_eq!(a.node.test_live_invites(), 1);

    // A pair_request whose seq is not above the stored last_rx for that id (e.g. a revoked device)
    // is a bad request too: re-pairing never moves last_rx backwards.
    add_paired(&a.roster, C, &DeviceKeys::generate().public_key(), None);
    a.roster.revoke(C);
    a.roster.set_last_rx_seq(C, time_seq() + 1_000_000_000);
    let mut ws = raw(addr).await;
    let body = PeerBody::PairRequest { code: inv.code.clone(), machine: machine(C, "C"), addr: None, ts: 1 };
    ws.send(env_text(&seal(&c_keys, C, A, &a.pk(), time_seq(), &body, true))).await.unwrap();
    let RawNext::Text(t) = raw_next(&mut ws, WAIT).await else { panic!("expected pair_reject") };
    let reply: PeerEnvelope = serde_json::from_str(&t).unwrap();
    assert!(matches!(
        open(&c_keys, &reply, &a.pk()).unwrap(),
        PeerBody::PairReject { reason: PairRejectReason::BadRequest, .. }
    ));
    assert_eq!(a.node.test_live_invites(), 1);
    assert!(a.roster.is_revoked(C));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replayed_old_pair_accept_rejected_on_rejoin() {
    let (mut a, _) = listening(A, fast()).await;
    let mut b = node(B, fast());
    pair(&mut a, &mut b).await;
    a.node.revoke(B).await;
    b.expect("Revoked by peer", |e| matches!(e, PeerEvent::Revoked { by_peer: true, .. })).await;
    let stored = b.roster.last_rx_seq(A);
    assert!(stored > 0, "last_rx is kept for the revoked row");
    let b_pk = b.pk();
    let uri = |addr: SocketAddr| {
        Invite {
            machine_id: A.into(),
            public_key: a.pk(),
            addr: addr.to_string(),
            code: new_code().1,
            expires_at: now_ms() + 60_000,
        }
        .to_uri()
    };
    let accept = |req_seq: u64| PeerBody::PairAccept { machine: machine(A, "A"), addr: None, req_seq, ts: 1 };

    // 1. An accept captured from an earlier pairing: old seq, old req_seq.
    let fake = fake_inviter(a.node.clone(), move |a, _req| {
        seal(&a.inner.keys, A, B, &b_pk, stored - 10, &accept(stored - 20), false)
    })
    .await;
    assert_eq!(b.node.join(&uri(fake)).await, Err(JoinError::Rejected));
    assert_eq!(b.roster.last_rx_seq(A), stored, "last_rx not rolled back");
    assert!(b.roster.get(A).is_none());

    // 2. Even an accept echoing this request's seq is refused if it would move last_rx backwards.
    let fake = fake_inviter(a.node.clone(), move |a, req| {
        seal(&a.inner.keys, A, B, &b_pk, stored, &accept(req.seq), false)
    })
    .await;
    assert_eq!(b.node.join(&uri(fake)).await, Err(JoinError::Rejected));
    assert_eq!(b.roster.last_rx_seq(A), stored);

    // 3. A fresh accept for a different request (wrong req_seq) is refused.
    let fake = fake_inviter(a.node.clone(), move |a, req| {
        seal(&a.inner.keys, A, B, &b_pk, stored + 5, &accept(req.seq + 1), false)
    })
    .await;
    assert_eq!(b.node.join(&uri(fake)).await, Err(JoinError::Rejected));
    assert!(b.roster.get(A).is_none());

    // Control: a fresh accept for this request is taken, so the checks above are what refused 1-3.
    let fake = fake_inviter(a.node.clone(), move |a, req| {
        seal(&a.inner.keys, A, B, &b_pk, stored + 10, &accept(req.seq), false)
    })
    .await;
    b.node.join(&uri(fake)).await.unwrap();
    assert_eq!(b.roster.last_rx_seq(A), stored + 10);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn peer_reported_public_or_dns_addr_ignored() {
    let (mut a, addr) = listening(A, fast()).await;
    let inv = a.node.create_invite(None, TTL).unwrap();
    let b_keys = DeviceKeys::generate();
    let a_pk = a.pk();
    let mut ws = raw(addr).await;
    let mut seq = time_seq();
    let body = PeerBody::PairRequest {
        code: inv.code.clone(),
        machine: machine(B, "B"),
        addr: Some("8.8.8.8:47801".into()),
        ts: 1,
    };
    ws.send(env_text(&seal(&b_keys, B, A, &a_pk, seq, &body, true))).await.unwrap();
    let RawNext::Text(t) = raw_next(&mut ws, WAIT).await else { panic!("expected pair_accept") };
    let reply: PeerEnvelope = serde_json::from_str(&t).unwrap();
    match open(&b_keys, &reply, &a_pk).unwrap() {
        PeerBody::PairAccept { req_seq, .. } => assert_eq!(req_seq, seq, "accept echoes the request seq"),
        other => panic!("expected pair_accept, got {other:?}"),
    }
    match a.expect("Paired", |e| matches!(e, PeerEvent::Paired { .. })).await {
        PeerEvent::Paired { device, .. } => assert_eq!(device.addr, None, "public addr not stored"),
        _ => unreachable!(),
    }
    assert_eq!(a.roster.get(B).unwrap().device.addr, None);

    // hello on the live link: public IPs and DNS names are ignored, a LAN IP literal is stored.
    let mut hello_then_sync = |addr: &str, tag: &str| {
        seq += 1;
        let h = seal(&b_keys, B, A, &a_pk, seq, &PeerBody::Hello { machine: machine(B, "B"), addr: Some(addr.into()), ts: 1 }, false);
        seq += 1;
        let p = seal(&b_keys, B, A, &a_pk, seq, &presence_body(B, tag), false);
        (env_text(&h), env_text(&p))
    };
    for (addr, tag, expect) in [
        ("evil.example.com:443", "t1", None),
        ("1.1.1.1:443", "t2", None),
        ("192.168.1.7:47801", "t3", Some("192.168.1.7:47801")),
        ("[2001:db8::1]:47801", "t4", Some("192.168.1.7:47801")),
        ("100.101.102.103:47801", "t5", Some("100.101.102.103:47801")),
    ] {
        let (h, p) = hello_then_sync(addr, tag);
        ws.send(h).await.unwrap();
        ws.send(p).await.unwrap();
        a.expect(tag, is_presence_named(B, tag)).await;
        assert_eq!(a.roster.get(B).unwrap().device.addr.as_deref(), expect, "{addr}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replayed_and_tampered_frames_dropped() {
    let (mut a, addr) = listening(A, fast()).await;
    let b_keys = DeviceKeys::generate();
    add_paired(&a.roster, B, &b_keys.public_key(), None);
    let a_pk = a.pk();
    let mut ws = raw(addr).await;
    let mut seq = time_seq();
    ws.send(env_text(&seal(&b_keys, B, A, &a_pk, seq, &hello_body(B), false))).await.unwrap();
    let RawNext::Text(t) = raw_next(&mut ws, WAIT).await else { panic!("expected hello reply") };
    let reply: PeerEnvelope = serde_json::from_str(&t).unwrap();
    assert_eq!((reply.from.as_str(), reply.to.as_str()), (A, B));
    assert!(matches!(open(&b_keys, &reply, &a_pk).unwrap(), PeerBody::Hello { machine, .. } if machine.id == A));
    a.expect("LinkUp", is_link_up(B)).await;

    seq += 1;
    let p1 = seal(&b_keys, B, A, &a_pk, seq, &presence_body(B, "p1"), false);
    ws.send(env_text(&p1)).await.unwrap();
    a.expect("p1", is_presence_named(B, "p1")).await;

    // Same frame again (same seq): dropped.
    ws.send(env_text(&p1)).await.unwrap();
    // Same box with a bumped seq: the nonce no longer matches, dropped.
    let mut bumped = p1.clone();
    bumped.seq = seq + 100;
    ws.send(env_text(&bumped)).await.unwrap();
    // Tampered box: dropped.
    seq += 1;
    let mut tampered = seal(&b_keys, B, A, &a_pk, seq, &presence_body(B, "p2"), false);
    let mut ct = b64::decode(&tampered.ciphertext).unwrap();
    ct[0] ^= 0x80;
    tampered.ciphertext = b64::encode(&ct);
    ws.send(env_text(&tampered)).await.unwrap();
    // Wrong `from` on an established link: dropped.
    seq += 1;
    let other = seal(&b_keys, C, A, &a_pk, seq, &presence_body(B, "p3"), false);
    ws.send(env_text(&other)).await.unwrap();
    // Not JSON: dropped.
    ws.send(Message::text("not json")).await.unwrap();
    a.expect_none(Duration::from_millis(300), "dropped frame", |e| {
        matches!(e, PeerEvent::Presence { .. } | PeerEvent::LinkDown { .. })
    })
    .await;
    // An older seq than the last accepted one is a replay too.
    let old = seal(&b_keys, B, A, &a_pk, seq - 5, &presence_body(B, "old"), false);
    ws.send(env_text(&old)).await.unwrap();

    // The link survived; a fresh frame still gets through.
    seq += 1;
    ws.send(env_text(&seal(&b_keys, B, A, &a_pk, seq, &presence_body(B, "p4"), false))).await.unwrap();
    a.expect("p4", is_presence_named(B, "p4")).await;
    assert!(a.position(is_presence_named(B, "old")).is_none());
    assert_eq!(a.roster.last_rx_seq(B), seq);
    assert_eq!(a.node.online().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replayed_hello_closed() {
    let (mut a, addr) = listening(A, fast()).await;
    let b_keys = DeviceKeys::generate();
    add_paired(&a.roster, B, &b_keys.public_key(), None);
    let hello = seal(&b_keys, B, A, &a.pk(), time_seq(), &hello_body(B), false);
    let mut ws = raw(addr).await;
    ws.send(env_text(&hello)).await.unwrap();
    assert!(matches!(raw_next(&mut ws, WAIT).await, RawNext::Text(_)));
    a.expect("LinkUp", is_link_up(B)).await;
    drop(ws);
    a.expect("LinkDown", is_link_down(B)).await;
    // The captured hello replayed on a new socket.
    let mut ws = raw(addr).await;
    ws.send(env_text(&hello)).await.unwrap();
    assert_closed_silently(&mut ws, WAIT).await;
    a.expect_none(Duration::from_millis(200), "LinkUp", is_link_up(B)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoke_sends_bye_both_sides_revoked_no_redial() {
    let (mut a, _) = listening(A, fast()).await;
    let mut b = node(B, fast());
    b.node.listen(local()).await.unwrap();
    pair(&mut a, &mut b).await;
    assert!(a.roster.get(B).unwrap().device.addr.is_some(), "b advertised its listener");

    a.node.revoke(B).await;
    a.expect("Revoked here", |e| matches!(e, PeerEvent::Revoked { device_id, by_peer: false } if device_id == B))
        .await;
    b.expect("Revoked by peer", |e| matches!(e, PeerEvent::Revoked { device_id, by_peer: true } if device_id == A))
        .await;
    // LinkDown comes first on both sides.
    assert!(a.position(is_link_down(B)).unwrap() < a.position(|e| matches!(e, PeerEvent::Revoked { .. })).unwrap());
    assert!(b.position(is_link_down(A)).unwrap() < b.position(|e| matches!(e, PeerEvent::Revoked { .. })).unwrap());
    assert!(a.roster.is_revoked(B) && a.roster.get(B).is_none());
    assert!(b.roster.is_revoked(A) && b.roster.get(A).is_none());

    // Nobody redials (backoff_max is 400 ms), even after connect_known.
    a.node.connect_known();
    b.node.connect_known();
    a.expect_none(Duration::from_millis(1200), "LinkUp", is_link_up(B)).await;
    b.expect_none(Duration::from_millis(100), "LinkUp", is_link_up(A)).await;
    assert!(a.node.online().is_empty() && b.node.online().is_empty());

    // Revoking again or an unknown id is a no-op.
    a.node.revoke(B).await;
    a.node.revoke(C).await;
    a.expect_none(Duration::from_millis(100), "Revoked", |e| matches!(e, PeerEvent::Revoked { .. })).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn revoke_while_offline_then_peer_is_refused() {
    let (mut a, _) = listening(A, fast()).await;
    let mut b = node(B, fast());
    pair(&mut a, &mut b).await;
    b.node.shutdown().await;
    a.expect("LinkDown", is_link_down(B)).await;
    a.node.revoke(B).await;
    a.expect("Revoked", |e| matches!(e, PeerEvent::Revoked { by_peer: false, .. })).await;
    assert!(a.position(is_link_down(B)).is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reconnect_after_listener_restart_with_backoff() {
    let dir = std::env::temp_dir().join(format!("krypto-peer-restart-{}", hex::encode(getrandom::u64().unwrap().to_be_bytes())));
    let key_path = dir.join("device.key");
    let roster_a = Arc::new(MemoryRoster::new());
    let mut a1 = node_with(A, DeviceKeys::load_or_create(&key_path).unwrap(), roster_a.clone(), fast());
    let addr = a1.node.listen(local()).await.unwrap();
    let mut b = node(B, fast());
    pair(&mut a1, &mut b).await;

    a1.node.shutdown().await;
    b.expect("LinkDown", is_link_down(A)).await;
    assert_eq!(a1.node.listening(), None);

    // Same identity and roster, same port, a moment later.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut a2 = node_with(A, DeviceKeys::load_or_create(&key_path).unwrap(), roster_a.clone(), fast());
    let mut bound = None;
    for _ in 0..40 {
        match a2.node.listen(addr).await {
            Ok(a) => {
                bound = Some(a);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    assert_eq!(bound, Some(addr));
    // b's dial loop finds it again.
    b.expect("LinkUp again", is_link_up(A)).await;
    a2.expect("LinkUp", is_link_up(B)).await;
    a2.expect("Presence", is_presence(B)).await;
    b.expect("Presence", is_presence(A)).await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplicate_link_tie_break_keeps_smaller_ids_dial() {
    // a < b: the link a dials wins on both sides.
    let (mut a, _) = listening(A, fast()).await;
    let mut b = node(B, fast());
    b.node.listen(local()).await.unwrap();
    pair(&mut a, &mut b).await;
    // The pairing link was dialed by b.
    assert_eq!(a.node.test_link_dialed_by_me(B), Some(false));
    assert_eq!(b.node.test_link_dialed_by_me(A), Some(true));

    // a dials as well: its link replaces b's on both sides.
    assert!(a.node.test_dial_once(B).await);
    let deadline = Instant::now() + WAIT;
    while b.node.test_link_dialed_by_me(A) != Some(false) {
        assert!(Instant::now() < deadline, "b never switched to a's link");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(a.node.test_link_dialed_by_me(B), Some(true));

    // b dialing again loses: a closes the new socket and keeps its own link.
    assert!(!b.node.test_dial_once(A).await);
    assert_eq!(a.node.test_link_dialed_by_me(B), Some(true));
    assert_eq!(b.node.test_link_dialed_by_me(A), Some(false));

    // No flap: the device never went down on either side, and the kept link carries presence.
    a.expect_none(Duration::from_millis(900), "LinkDown", is_link_down(B)).await;
    b.expect_none(Duration::from_millis(100), "LinkDown", is_link_down(A)).await;
    a.log.clear();
    a.expect("Presence on kept link", is_presence(B)).await;
    assert_eq!(a.node.online().len(), 1);
    assert_eq!(b.node.online().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offline_after_silence() {
    let t = PeerTimings { offline_after: Duration::from_millis(400), ..fast() };
    let (mut a, addr) = listening(A, t).await;
    let b_keys = DeviceKeys::generate();
    add_paired(&a.roster, B, &b_keys.public_key(), None);
    let mut ws = raw(addr).await;
    ws.send(env_text(&seal(&b_keys, B, A, &a.pk(), time_seq(), &hello_body(B), false))).await.unwrap();
    assert!(matches!(raw_next(&mut ws, WAIT).await, RawNext::Text(_)));
    a.expect("LinkUp", is_link_up(B)).await;
    let up = Instant::now();
    // a keeps sending presence, but b says nothing.
    a.expect("LinkDown", is_link_down(B)).await;
    assert!(up.elapsed() >= Duration::from_millis(350), "went offline too early: {:?}", up.elapsed());
    assert!(a.node.online().is_empty());
    // And the socket is closed.
    loop {
        match raw_next(&mut ws, WAIT).await {
            RawNext::Text(_) => continue,
            RawNext::Closed => break,
            RawNext::Silent => panic!("socket not closed"),
        }
    }
}

// ---------- listener ----------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listener_404_and_origin_403() {
    let (_a, addr) = listening(A, fast()).await;
    let err = tokio_tungstenite::connect_async(format!("ws://{addr}/other")).await.unwrap_err();
    assert_eq!(ws_http_status(err), 404);
    let err = tokio_tungstenite::connect_async(format!("ws://{addr}/peer/")).await.unwrap_err();
    assert_eq!(ws_http_status(err), 404);

    for path in ["/peer", "/other"] {
        let mut req = format!("ws://{addr}{path}").into_client_request().unwrap();
        req.headers_mut().insert("origin", "http://evil.example".parse().unwrap());
        let err = tokio_tungstenite::connect_async(req).await.unwrap_err();
        assert_eq!(ws_http_status(err), 403, "{path}");
    }
    assert!(http_status(addr, "GET / HTTP/1.1\r\nHost: x\r\n\r\n").await.contains(" 404"));
    assert!(http_status(addr, "POST /peer HTTP/1.1\r\nHost: x\r\nContent-Length: 0\r\n\r\n").await.contains(" 404"));
    let plain = http_status(addr, "GET /peer HTTP/1.1\r\nHost: x\r\n\r\n").await;
    assert!(plain.starts_with("HTTP/1.1 4"), "plain GET /peer must not upgrade: {plain}");
    assert!(http_status(addr, "GET /health HTTP/1.1\r\nHost: x\r\nOrigin: null\r\n\r\n").await.contains(" 403"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn first_frame_deadline_and_per_ip_unauthenticated_cap() {
    let t = PeerTimings { handshake_timeout: Duration::from_millis(1500), ..fast() };
    let (_a, addr) = listening(A, t).await;
    let mut idle = Vec::new();
    for _ in 0..MAX_UNAUTH_PER_IP {
        idle.push(raw(addr).await);
    }
    // One more from the same IP is dropped at accept, before any HTTP.
    match tokio_tungstenite::connect_async(format!("ws://{addr}{PEER_PATH}")).await {
        Ok(_) => panic!("per-IP cap not enforced"),
        Err(WsError::Http(r)) => panic!("expected a dropped connection, got HTTP {}", r.status()),
        Err(_) => {}
    }
    // Every silent socket is closed at the deadline, freeing the slots.
    let start = Instant::now();
    for ws in idle.iter_mut() {
        assert_closed_silently(ws, WAIT).await;
    }
    assert!(start.elapsed() >= Duration::from_millis(1000));
    let deadline = Instant::now() + WAIT;
    loop {
        match tokio_tungstenite::connect_async(format!("ws://{addr}{PEER_PATH}")).await {
            Ok(_) => break,
            Err(_) if Instant::now() < deadline => tokio::time::sleep(Duration::from_millis(20)).await,
            Err(e) => panic!("slots not freed: {e}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn total_unauthenticated_cap_across_ips() {
    let t = PeerTimings { handshake_timeout: Duration::from_millis(3000), ..fast() };
    let (_a, addr) = listening(A, t).await;
    let ips = MAX_UNAUTH_TOTAL / MAX_UNAUTH_PER_IP;
    let mut idle = Vec::new();
    for i in 0..ips {
        for _ in 0..MAX_UNAUTH_PER_IP {
            match raw_from(Ipv4Addr::new(127, 0, 0, 1 + i as u8), addr).await {
                Some(ws) => idle.push(ws),
                None if i > 0 && idle.len() == MAX_UNAUTH_PER_IP => {
                    eprintln!("skipping: cannot bind extra loopback source addresses on this OS");
                    return;
                }
                None => panic!("connection {} from 127.0.0.{} refused", idle.len(), 1 + i),
            }
        }
    }
    assert_eq!(idle.len(), MAX_UNAUTH_TOTAL);
    // A fresh IP is still refused: the total cap is full.
    assert!(raw_from(Ipv4Addr::new(127, 0, 0, 1 + ips as u8), addr).await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_http_cut_at_deadline() {
    let t = PeerTimings { handshake_timeout: Duration::from_millis(800), ..fast() };
    let (_a, addr) = listening(A, t).await;
    let closed_by_server = |r: std::io::Result<usize>| matches!(r, Ok(0) | Err(_));

    // Headers that never finish (slow-loris).
    let start = Instant::now();
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /peer HTTP/1.1\r\nHost: x\r\n").await.unwrap();
    let mut buf = [0u8; 512];
    let r = timeout(WAIT, s.read(&mut buf)).await.expect("server never closed the connection");
    assert!(closed_by_server(r), "unexpected response");
    assert!(start.elapsed() >= Duration::from_millis(600), "closed too early: {:?}", start.elapsed());

    // A finished request kept alive afterwards is cut at the same deadline.
    let start = Instant::now();
    let mut s = TcpStream::connect(addr).await.unwrap();
    s.write_all(b"GET /nope HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
    let n = timeout(WAIT, s.read(&mut buf)).await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).starts_with("HTTP/1.1 404"));
    let r = timeout(WAIT, s.read(&mut buf)).await.expect("idle keep-alive never closed");
    assert!(closed_by_server(r));
    assert!(start.elapsed() >= Duration::from_millis(600));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oversized_frame_closes_socket() {
    let (a, addr) = listening(A, fast()).await;
    let mut ws = raw(addr).await;
    ws.send(Message::text("x".repeat(MAX_FRAME + 1))).await.unwrap();
    assert_closed_silently(&mut ws, WAIT).await;
    assert!(a.node.online().is_empty());
}

// ---------- seq ----------

#[test]
fn seq_rules() {
    let n = node(A, fast());
    add_paired(&n.roster, B, &DeviceKeys::generate().public_key(), None);
    let s1 = n.node.inner.next_tx_seq(B);
    assert!(s1 >= time_seq() - 1_000_000);
    let s2 = n.node.inner.next_tx_seq(B);
    assert!(s2 > s1);
    assert_eq!(n.roster.last_tx_seq(B), s2);
    // A last_tx far in the future still increases by one.
    n.roster.set_last_tx_seq(B, u64::MAX / 2);
    assert_eq!(n.node.inner.next_tx_seq(B), u64::MAX / 2 + 1);

    assert!(n.node.inner.accept_rx_seq(B, 10));
    assert!(!n.node.inner.accept_rx_seq(B, 10));
    assert!(!n.node.inner.accept_rx_seq(B, 9));
    assert!(n.node.inner.accept_rx_seq(B, 11));
    assert_eq!(n.roster.last_rx_seq(B), 11);
}

#[test]
fn jitter_in_range() {
    for _ in 0..100 {
        let j = jitter(Duration::from_millis(100));
        assert!(j >= Duration::from_millis(50) && j < Duration::from_millis(100), "{j:?}");
    }
    assert_eq!(jitter(Duration::ZERO), Duration::ZERO);
}
