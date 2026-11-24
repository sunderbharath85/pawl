//! Phones (migration step 7): a phone pairs through a relay-only invite and links over the relay only;
//! hubs never dial it, but may wake it; paired hubs introduce devices with `roster_intro`.

use super::*;
use crate::intro::MAX_INTRO_DEVICES;
use krypto_protocol::{ClientKind, Intent, IntentStatus};

const P: &str = "pppppppppppp";
const D: &str = "dddddddddddd";

/// A phone (machine os ios) on `relay`, built from `secret` so it can come back with the same key.
async fn phone_on(relay: &MockRelay, id: &str, secret: [u8; 32], roster: Arc<MemoryRoster>) -> TestNode {
    let (tx, rx) = unbounded_channel();
    let me = Machine { os: MachineOs::Ios, hub_version: None, ..machine(id, "iPhone") };
    let node = PeerNode::new(DeviceKeys::from_secret_bytes(secret), me, roster.clone(), tx).with_timings(fast());
    let mut n = TestNode { id: id.into(), node, roster, events: rx, log: Vec::new() };
    n.node.set_relay(Some(relay.config())).unwrap();
    n.expect("relay connected", is_relay_up).await;
    n
}

/// Wait until `pk` has sent the relay nothing new for a moment; the number of frames it sent.
async fn quiet_log(relay: &MockRelay, pk: &[u8; 32]) -> usize {
    let deadline = Instant::now() + WAIT;
    let mut seen = relay.sent_by(pk).len();
    loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let now = relay.sent_by(pk).len();
        if now == seen {
            return now;
        }
        assert!(Instant::now() < deadline, "the relay log never went quiet");
        seen = now;
    }
}

/// `p` scans a relay-only invite of `a`; both links end up on the relay path.
async fn pair_phone(relay: &MockRelay, a: &mut TestNode, p: &mut TestNode) {
    let slots = relay.live_slots(&a.pk());
    let inv = a.node.create_phone_invite(TTL).unwrap();
    eventually("the pairing slot", || relay.live_slots(&a.pk()) > slots).await;
    p.node.join(&inv.to_uri()).await.unwrap();
    let (aid, pid) = (a.id.clone(), p.id.clone());
    a.expect("relay path", is_path(&pid, LinkPath::Relay)).await;
    p.expect("relay path", is_path(&aid, LinkPath::Relay)).await;
}

fn intro_of(e: &RosterEntry) -> IntroDevice {
    let d = &e.device;
    IntroDevice {
        id: d.id.clone(),
        kind: d.kind,
        name: d.name.clone(),
        public_key: d.public_key.clone(),
        addr: d.addr.clone(),
        os: d.os,
    }
}

/// What the hub does with an intro it accepts: store the device (introduced by `from`), then tell the node.
fn store_intro(n: &TestNode, from: &str, d: &IntroDevice) {
    n.roster.upsert(RosterEntry {
        device: Device {
            id: d.id.clone(),
            kind: d.kind,
            name: d.name.clone(),
            public_key: d.public_key.clone(),
            paired_at: now_ms(),
            last_seen: None,
            addr: d.addr.clone(),
            path: None,
            introduced_by: Some(from.into()),
            os: d.os,
        },
        machine: Machine {
            id: d.id.clone(),
            name: d.name.clone(),
            os: d.os.unwrap_or(MachineOs::Other),
            last_seen: 0,
            online: false,
            hub_version: None,
            user_active_at: None,
        },
    });
    n.node.roster_changed();
}

fn is_intro(e: &PeerEvent) -> bool {
    matches!(e, PeerEvent::RosterIntro { .. })
}

type Invalid = Vec<(IntroDevice, &'static str)>;

/// The next RosterIntro, which must come from `from`.
async fn next_intro(n: &mut TestNode, from: &str) -> (Vec<IntroDevice>, Invalid) {
    match n.expect("RosterIntro", is_intro).await {
        PeerEvent::RosterIntro { from: f, devices, invalid } => {
            assert_eq!(f, from);
            (devices, invalid)
        }
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_phone_pairs_through_a_relay_only_invite_and_talks_over_the_relay() {
    let relay = MockRelay::start().await;
    // The hub listens on the LAN too: the phone's invite carries no addr anyway.
    let (mut a, _) = listening(A, fast()).await;
    assert_eq!(a.node.create_phone_invite(TTL), Err(InviteError::RelayOff));
    a.node.set_relay(Some(relay.config())).unwrap();
    a.expect("relay connected", is_relay_up).await;
    assert_eq!(a.node.create_phone_invite(Duration::from_secs(5)), Err(InviteError::Invalid));
    let inv = a.node.create_phone_invite(TTL).unwrap();
    assert_eq!(inv.addr, None);
    assert_eq!(inv.relay.as_deref(), Some(relay.url().as_str()));
    assert!(inv.relay_token.is_some());
    assert_eq!(inv.relay_key.as_deref(), Some(KEY));
    assert!(!inv.to_uri().contains("addr="));
    eventually("the pairing slot", || relay.live_slots(&a.pk()) == 1).await;

    // Same wire as a hub joiner; the inviter files it as a phone from its machine's os.
    let mut p = phone_on(&relay, P, [0x51; 32], Arc::new(MemoryRoster::new())).await;
    let joined = p.node.join_invite(&inv.to_uri()).await.unwrap();
    assert_eq!((joined.device.id.as_str(), joined.device.kind), (A, DeviceKind::Hub));
    assert_eq!(joined.device.os, Some(MachineOs::current()));
    match a.expect("Paired", |e| matches!(e, PeerEvent::Paired { .. })).await {
        PeerEvent::Paired { device, machine, we_invited } => {
            assert!(we_invited);
            assert_eq!((device.id.as_str(), device.kind, device.os), (P, DeviceKind::Phone, Some(MachineOs::Ios)));
            assert_eq!((device.addr, device.introduced_by), (None, None));
            assert_eq!(machine.os, MachineOs::Ios);
        }
        _ => unreachable!(),
    }
    assert_eq!(a.roster.get(P).unwrap().device.kind, DeviceKind::Phone);
    a.expect("relay path", is_path(P, LinkPath::Relay)).await;
    p.expect("relay path", is_path(A, LinkPath::Relay)).await;
    assert_eq!(relay.allowed(&a.pk()), HashSet::from([address(&p.pk())]));
    assert_eq!(relay.allowed(&p.pk()), HashSet::from([address(&a.pk())]));

    // Presence both ways; sync / intent from the phone, mirror / intent_ack from the hub.
    a.expect("Presence", is_presence(P)).await;
    p.expect("Presence", is_presence(A)).await;
    let intent = PeerBody::Intent {
        intent_id: "i1".into(),
        client: ClientKind::Mobile,
        confirmed: true,
        intent: Intent::Approve { request_id: "r1".into(), option_id: "allow".into() },
        ts: now_ms(),
    };
    assert_eq!(p.node.send(A, sync_request(0)), Ok(()));
    assert_eq!(p.node.send(A, intent.clone()), Ok(()));
    assert_eq!(a.bodies(P, 2).await, vec![sync_request(0), intent]);
    let snapshot = PeerBody::MirrorSnapshot { sessions: vec![], pets: vec![], pending_permissions: vec![] };
    let ack = PeerBody::IntentAck { intent_id: "i1".into(), status: IntentStatus::Applied, reason: None, result: None };
    assert_eq!(a.node.send(P, snapshot.clone()), Ok(()));
    assert_eq!(a.node.send(P, ack.clone()), Ok(()));
    assert_eq!(p.bodies(A, 2).await, vec![snapshot, ack]);
    assert_eq!(a.node.wake_phone(P), Err(WakeError::Linked));

    // The phone goes away. The hub never dials or greets it: a frame in its mailbox would make the
    // relay push to it.
    let phone_roster = p.roster.clone();
    p.node.shutdown().await;
    a.expect("LinkDown", is_link_down(P)).await;
    // The relay has seen the phone's socket close (a frame handed to a closing socket is lost), and
    // what the link queued before it ended has reached the relay.
    eventually("the phone off the relay", || !relay.online(&p.pk())).await;
    let sent = quiet_log(&relay, &a.pk()).await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let log = relay.sent_by(&a.pk());
    assert!(!log[sent..].contains(&Sent::Send(address(&p.pk()))), "{log:#?}");
    assert!(lock(&a.node.inner.dialers).get(P).is_none_or(|h| h.is_finished()), "nobody dials the phone");

    // Offline wake: one presence in its mailbox, at most once a minute.
    assert_eq!(a.node.wake_phone(B), Err(WakeError::NotPhone));
    assert_eq!(a.node.wake_phone(P), Ok(()));
    let wake_seq = a.roster.last_tx_seq(P);
    eventually("the wake frame", || relay.sent_by(&a.pk())[sent..].contains(&Sent::Send(address(&p.pk())))).await;
    assert_eq!(a.node.wake_phone(P), Err(WakeError::RateLimited));

    // The phone comes back: the wake is what it reads first, sealed and sequenced like any frame.
    let mut p = phone_on(&relay, P, [0x51; 32], phone_roster).await;
    p.node.connect_known();
    p.expect("LinkUp", is_link_up(A)).await;
    a.expect("LinkUp", is_link_up(P)).await;
    assert!(p.roster.last_rx_seq(A) >= wake_seq, "the phone opened the wake");
    link_works(&a, &mut p).await;

    // Without a relay session there is no wake.
    p.node.shutdown().await;
    a.expect("LinkDown", is_link_down(P)).await;
    eventually("the phone off the relay", || !relay.online(&p.pk())).await;
    a.node.set_relay(None).unwrap();
    assert_eq!(a.node.wake_phone(P), Err(WakeError::RelayOff));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn introduced_hubs_are_reported_and_once_stored_they_link() {
    let relay = MockRelay::start().await;
    let (mut a, _) = listening(A, fast()).await;
    let (mut b, _) = listening(B, fast()).await;
    let (mut c, _) = listening(C, fast()).await;
    for n in [&mut a, &mut b, &mut c] {
        n.node.set_relay(Some(relay.config())).unwrap();
        n.expect("relay connected", is_relay_up).await;
    }
    pair(&mut a, &mut b).await;
    pair(&mut a, &mut c).await;
    let (b_on_a, c_on_a) = (intro_of(&a.roster.get(B).unwrap()), intro_of(&a.roster.get(C).unwrap()));
    assert!(b_on_a.addr.is_some() && c_on_a.addr.is_some(), "LAN addrs travel in the intro");

    assert_eq!(a.node.send_roster_intro(B, vec![c_on_a.clone()]), Ok(()));
    assert_eq!(a.node.send_roster_intro(C, vec![b_on_a.clone()]), Ok(()));
    assert_eq!(next_intro(&mut b, A).await, (vec![c_on_a.clone()], vec![]));
    assert_eq!(next_intro(&mut c, A).await, (vec![b_on_a.clone()], vec![]));
    // The node stores nothing and lets nobody new in by itself.
    assert!(b.roster.get(C).is_none() && c.roster.get(B).is_none());
    assert!(!relay.allowed(&b.pk()).contains(&address(&c.pk())));

    // The hubs store them: the allow lists and the LAN listeners take them, and they link.
    store_intro(&b, A, &c_on_a);
    store_intro(&c, A, &b_on_a);
    eventually("b to allow c", || relay.allowed(&b.pk()).contains(&address(&c.pk()))).await;
    eventually("c to allow b", || relay.allowed(&c.pk()).contains(&address(&b.pk()))).await;
    b.expect("LinkUp", is_link_up(C)).await;
    c.expect("LinkUp", is_link_up(B)).await;
    settle(&b, &c).await;
    assert_eq!(b.node.link_path(C), Some(LinkPath::Lan));
    link_works(&b, &mut c).await;
    link_works(&c, &mut b).await;
    assert_eq!(b.roster.get(C).unwrap().device.introduced_by.as_deref(), Some(A));
    assert!(!b.log.iter().any(|e| matches!(e, PeerEvent::Body { body: PeerBody::RosterIntro { .. }, .. })));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn roster_intros_are_checked_and_only_a_paired_hub_may_send_one() {
    let relay = MockRelay::start().await;
    let mut a = on_relay(A, &relay, fast()).await;
    let mut b = on_relay(B, &relay, fast()).await;
    pair_relay(&relay, &mut a, &mut b).await;
    let key = || b64::encode(&DeviceKeys::generate().public_key());
    let hub = IntroDevice {
        id: D.into(),
        kind: DeviceKind::Hub,
        name: "Desk".into(),
        public_key: key(),
        addr: Some("192.168.1.9:47801".into()),
        os: Some(MachineOs::Linux),
    };
    let phone = IntroDevice { id: P.into(), kind: DeviceKind::Phone, name: "iPhone".into(), public_key: key(), addr: None, os: Some(MachineOs::Ios) };
    let bad_id = IntroDevice { id: "Not-An-Id".into(), ..hub.clone() };
    let bad_name = IntroDevice { id: "eeeeeeeeeeee".into(), name: "a\u{7}".into(), ..hub.clone() };
    let low_order = IntroDevice { id: "ffffffffffff".into(), public_key: b64::encode(&[0u8; 32]), ..hub.clone() };
    let phone_addr = IntroDevice { id: "gggggggggggg".into(), addr: Some("192.168.1.10:9".into()), ..phone.clone() };
    let public_addr = IntroDevice { id: "hhhhhhhhhhhh".into(), addr: Some("8.8.8.8:47801".into()), ..hub.clone() };
    let bidi_name = IntroDevice { id: "iiiiiiiiiiii".into(), name: "Desk\u{202E}".into(), ..hub.clone() };
    let hub_on_ios = IntroDevice { id: "jjjjjjjjjjjj".into(), public_key: key(), os: Some(MachineOs::Ios), ..hub.clone() };
    let phone_on_linux = IntroDevice { id: "kkkkkkkkkkkk".into(), public_key: key(), os: Some(MachineOs::Linux), ..phone.clone() };
    // The node's own checks: never the receiver, never the sender, each id and key once.
    let a_entry = intro_of(&b.roster.get(A).unwrap());
    let receiver_id = IntroDevice { id: B.into(), public_key: key(), ..hub.clone() };
    let receiver_key = IntroDevice { id: "llllllllllll".into(), public_key: b64::encode(&b.pk()), ..hub.clone() };
    let sender_id = IntroDevice { id: A.into(), public_key: key(), ..hub.clone() };
    let sender_key = IntroDevice { id: "mmmmmmmmmmmm".into(), public_key: a_entry.public_key.clone(), ..hub.clone() };
    let dup_id = IntroDevice { public_key: key(), ..hub.clone() };
    let dup_key = IntroDevice { id: "nnnnnnnnnnnn".into(), ..phone.clone() };
    let all = vec![
        hub.clone(),
        bad_id.clone(),
        bad_name.clone(),
        phone.clone(),
        low_order.clone(),
        phone_addr.clone(),
        public_addr.clone(),
        bidi_name.clone(),
        hub_on_ios.clone(),
        phone_on_linux.clone(),
        receiver_id.clone(),
        receiver_key.clone(),
        sender_id.clone(),
        a_entry.clone(),
        sender_key.clone(),
        dup_id.clone(),
        dup_key.clone(),
    ];
    a.node.send_roster_intro(B, all).unwrap();
    let (devices, invalid) = next_intro(&mut b, A).await;
    assert_eq!(devices, vec![hub.clone(), phone.clone()]);
    assert_eq!(
        invalid,
        vec![
            (bad_id, "bad_id"),
            (bad_name, "bad_name"),
            (low_order, "bad_key"),
            (phone_addr, "bad_addr"),
            (public_addr, "bad_addr"),
            (bidi_name, "bad_name"),
            (hub_on_ios, "bad_kind"),
            (phone_on_linux, "bad_kind"),
            (receiver_id, "own_device"),
            (receiver_key, "own_device"),
            (sender_id, "sender_device"),
            (a_entry, "sender_device"),
            (sender_key, "sender_device"),
            (dup_id, "duplicate"),
            (dup_key, "duplicate"),
        ]
    );
    assert!(b.roster.get(D).is_none(), "reported, not stored");

    // More than MAX_INTRO_DEVICES: refused by send_roster_intro, dropped whole by the receiver.
    let distinct = |i: usize| IntroDevice { id: format!("dev{i:09}"), public_key: key(), ..hub.clone() };
    let many: Vec<IntroDevice> = (0..=MAX_INTRO_DEVICES).map(distinct).collect();
    assert_eq!(a.node.send_roster_intro(B, many.clone()), Err(SendError::TooLarge));
    assert_eq!(a.node.send_roster_intro(B, many[..MAX_INTRO_DEVICES].to_vec()), Ok(()));
    assert_eq!(next_intro(&mut b, A).await, (many[..MAX_INTRO_DEVICES].to_vec(), vec![]));
    a.node.send(B, PeerBody::RosterIntro { devices: many }).unwrap();
    a.node.send(B, sync_request(1)).unwrap();
    assert_eq!(b.bodies(A, 1).await, vec![sync_request(1)]);
    assert_eq!(b.log.iter().filter(|e| is_intro(e)).count(), 2, "the oversized intro made no event");

    // A phone may not introduce anyone.
    let mut p = phone_on(&relay, P, [0x52; 32], Arc::new(MemoryRoster::new())).await;
    pair_phone(&relay, &mut b, &mut p).await;
    p.node.send(B, PeerBody::RosterIntro { devices: vec![hub] }).unwrap();
    p.node.send(B, sync_request(2)).unwrap();
    assert_eq!(b.bodies(P, 1).await, vec![sync_request(2)]);
    assert_eq!(b.log.iter().filter(|e| is_intro(e)).count(), 2, "nothing from the phone");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_invite_decides_whether_a_joiner_is_a_phone_or_a_hub() {
    let relay = MockRelay::start().await;
    let mut a = on_relay(A, &relay, fast()).await;

    // A phone invite (the QR code): a joiner claiming any other os is refused, and the code stays live.
    let slots = relay.live_slots(&a.pk());
    let inv = a.node.create_phone_invite(TTL).unwrap();
    eventually("the pairing slot", || relay.live_slots(&a.pk()) > slots).await;
    let mut linux = node(B, fast());
    linux.node.set_machine(Machine { os: MachineOs::Linux, ..machine(B, "Box") });
    linux.node.set_relay(Some(relay.config())).unwrap();
    linux.expect("relay connected", is_relay_up).await;
    assert_eq!(linux.node.join(&inv.to_uri()).await, Err(JoinError::Rejected));
    a.expect("PairRejected", |e| matches!(e, PeerEvent::PairRejected { reason: PairRejectReason::InvalidInvite, .. })).await;
    assert!(a.roster.get(B).is_none() && linux.roster.get(A).is_none());
    assert_eq!(a.node.test_live_invites(), 1, "the refused attempt did not use up the invite");
    // A phone pairs with a fresh phone invite (the attempt above spent the one-shot relay slot).
    a.node.test_clear_pair_limits();
    let mut p = phone_on(&relay, P, [0x53; 32], Arc::new(MemoryRoster::new())).await;
    pair_phone(&relay, &mut a, &mut p).await;
    assert_eq!(a.roster.get(P).unwrap().device.kind, DeviceKind::Phone);

    // A hub invite: a joiner claiming ios / android is refused; a hub is taken, as a hub.
    let slots = relay.live_slots(&a.pk());
    let inv = a.node.create_invite(None, TTL).unwrap();
    eventually("the pairing slot", || relay.live_slots(&a.pk()) > slots).await;
    let mut q = phone_on(&relay, D, [0x54; 32], Arc::new(MemoryRoster::new())).await;
    assert_eq!(q.node.join(&inv.to_uri()).await, Err(JoinError::Rejected));
    assert!(a.roster.get(D).is_none());
    q.expect_none(Duration::from_millis(200), "a link", is_link_up(A)).await;
    let mut c = on_relay(C, &relay, fast()).await;
    a.node.test_clear_pair_limits();
    pair_relay(&relay, &mut a, &mut c).await;
    assert_eq!(a.roster.get(C).unwrap().device.kind, DeviceKind::Hub);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_wake_for_a_disconnected_or_revoked_phone() {
    let relay = MockRelay::start().await;
    let mut a = on_relay(A, &relay, fast()).await;
    let mut p = phone_on(&relay, P, [0x55; 32], Arc::new(MemoryRoster::new())).await;
    pair_phone(&relay, &mut a, &mut p).await;
    p.node.shutdown().await;
    a.expect("LinkDown", is_link_down(P)).await;
    eventually("the phone off the relay", || !relay.online(&p.pk())).await;
    let before = quiet_log(&relay, &a.pk()).await;

    a.node.disconnect(P, Duration::from_secs(30));
    assert_eq!(a.node.wake_phone(P), Err(WakeError::Disconnected));
    a.node.revoke(P).await;
    assert_eq!(a.node.wake_phone(P), Err(WakeError::NotPhone));
    quiet_log(&relay, &a.pk()).await;
    let log = relay.sent_by(&a.pk());
    assert!(log[before..].iter().all(|s| matches!(s, Sent::Allow(_))), "only allow-list updates, no wake: {log:#?}");
}
