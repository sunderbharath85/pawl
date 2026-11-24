//! Unit tests for the group roster rules (pure: no hub, no links).

use super::*;
use krypto_peer::DeviceKeys;

const ME: &str = "abcdef012345";
const SENDER: &str = "bbbbbb000002";

fn key() -> String {
    DeviceKeys::generate().public_b64()
}

fn device(id: &str, kind: DeviceKind, name: &str, pk: &str) -> Device {
    Device {
        id: id.into(),
        kind,
        name: name.into(),
        public_key: pk.into(),
        paired_at: 1,
        last_seen: None,
        addr: None,
        path: None,
        introduced_by: None,
        os: None,
    }
}

fn entry(d: Device, os: MachineOs) -> RosterEntry {
    let machine = Machine {
        id: d.id.clone(),
        name: d.name.clone(),
        os,
        last_seen: 1,
        online: false,
        hub_version: None,
        user_active_at: None,
    };
    RosterEntry { device: d, machine }
}

/// Phones run ios (the node's `bad_kind` rule); a hub's os may be unknown.
fn intro(id: &str, kind: DeviceKind, name: &str, pk: &str) -> IntroDevice {
    let os = (kind == DeviceKind::Phone).then_some(MachineOs::Ios);
    IntroDevice { id: id.into(), kind, name: name.into(), public_key: pk.into(), addr: None, os }
}

#[test]
fn intro_groups_follow_the_fan_out_rules() {
    use DeviceKind::{Hub, Phone};
    let roster = [
        device("hhhhhh000001", Hub, "Mac mini", "k1"),
        device("hhhhhh000002", Hub, "Laptop", "k2"),
        device("pppppp000001", Phone, "iPhone", "k3"),
        device("pppppp000002", Phone, "Pixel", "k4"),
    ];
    // A new hub meets every other device (hubs learn every device; phones learn every hub).
    let mut with_new_hub = roster.to_vec();
    with_new_hub.push(device("nnnnnn000001", Hub, "Studio", "k5"));
    assert_eq!(
        intro_group("nnnnnn000001", Hub, &with_new_hub),
        ["hhhhhh000001", "hhhhhh000002", "pppppp000001", "pppppp000002"]
    );
    // A new phone meets hubs only: phones are never introduced to phones.
    let mut with_new_phone = roster.to_vec();
    with_new_phone.push(device("nnnnnn000002", Phone, "iPad", "k6"));
    assert_eq!(intro_group("nnnnnn000002", Phone, &with_new_phone), ["hhhhhh000001", "hhhhhh000002"]);
    // Nobody else: nothing to introduce.
    assert!(intro_group("hhhhhh000001", Hub, &roster[..1]).is_empty());
    assert!(may_learn(Hub, Phone) && may_learn(Hub, Hub) && may_learn(Phone, Hub));
    assert!(!may_learn(Phone, Phone));
}

#[test]
fn intro_devices_carry_only_what_receivers_take() {
    let mut hub = device("hhhhhh000001", DeviceKind::Hub, "Mac mini", &key());
    hub.addr = Some("192.168.1.20:47801".into());
    let out = intro_device(&entry(hub.clone(), MachineOs::Macos));
    assert_eq!(out.addr.as_deref(), Some("192.168.1.20:47801"));
    assert_eq!(out.os, Some(MachineOs::Macos));
    assert!(krypto_peer::check_intro_device(&out).is_ok(), "what we send passes the receiver's checks");
    // A public host name is not something a receiver would dial: left out, the device still goes.
    hub.addr = Some("mac.example.com:47801".into());
    let out = intro_device(&entry(hub, MachineOs::Macos));
    assert_eq!(out.addr, None);
    assert!(krypto_peer::check_intro_device(&out).is_ok());
    // Phones never carry an address; their os comes from the device when the machine view has none.
    let mut phone = device("pppppp000001", DeviceKind::Phone, "iPhone", &key());
    phone.addr = Some("10.0.0.9:47801".into());
    phone.os = Some(MachineOs::Ios);
    let out = intro_device(&entry(phone, MachineOs::Other));
    assert_eq!((out.addr, out.os, out.kind), (None, Some(MachineOs::Ios), DeviceKind::Phone));
}

#[test]
fn intros_are_judged_against_what_this_hub_knows() {
    let me_pk = DeviceKeys::generate();
    let me_key: [u8; 32] = me_pk.public_key();
    let sender = device(SENDER, DeviceKind::Hub, "Laptop", &key());
    let phone_pk = key();
    let revoked_pk = key();
    let paired_phone = entry(device("pppppp000001", DeviceKind::Phone, "iPhone", &phone_pk), MachineOs::Ios);
    let revoked = entry(device("rrrrrr000001", DeviceKind::Phone, "Old phone", &revoked_pk), MachineOs::Android);
    let sender_entry = entry(sender.clone(), MachineOs::Macos);
    let stored = [
        Stored { entry: &sender_entry, revoked: false },
        Stored { entry: &paired_phone, revoked: false },
        Stored { entry: &revoked, revoked: true },
    ];
    let new_pk = key();
    let devices = [
        intro("nnnnnn000001", DeviceKind::Phone, "Pixel", &new_pk),
        intro("pppppp000001", DeviceKind::Phone, "iPhone", &phone_pk),
        intro("rrrrrr000001", DeviceKind::Phone, "Old phone", &key()),
        intro("yyyyyy000001", DeviceKind::Phone, "Old key", &revoked_pk),
        intro(ME, DeviceKind::Hub, "Me", &key()),
        intro("zzzzzz000001", DeviceKind::Hub, "My key", &me_pk.public_b64()),
        intro(SENDER, DeviceKind::Hub, "Laptop", &key()),
        intro("zzzzzz000002", DeviceKind::Hub, "Sender key", &sender.public_key),
        intro("nnnnnn000001", DeviceKind::Phone, "Pixel again", &key()),
        intro("BAD", DeviceKind::Phone, "Bad id", &key()),
        intro("pppppp000001", DeviceKind::Hub, "iPhone as hub", &phone_pk),
    ];
    let got = judge_intro(&devices, (ME, &me_key), &sender, &stored, krypto_peer::check_intro_device);
    use IntroOutcome::*;
    assert_eq!(
        got,
        [
            Add { public_key: new_pk },
            Known,
            Conflict("revoked here"),
            Conflict("its key was revoked here"),
            Invalid("is this machine"),
            Invalid("is this machine"),
            Invalid("is the sender"),
            Invalid("is the sender"),
            Invalid("listed twice"),
            Invalid("bad_id"),
            // Same id and key as before in this intro: a repeat, whatever the kind says.
            Invalid("listed twice"),
        ]
    );
    // Against the roster: another key for a known id, a known key under another id, another kind.
    let one = |d: IntroDevice| judge_intro(&[d], (ME, &me_key), &sender, &stored, krypto_peer::check_intro_device);
    assert_eq!(one(intro("pppppp000001", DeviceKind::Phone, "iPhone", &key())), [Conflict("known here with another key")]);
    assert_eq!(one(intro("xxxxxx000001", DeviceKind::Phone, "Thief", &phone_pk)), [Conflict("its key belongs to another device here")]);
    assert_eq!(one(intro("pppppp000001", DeviceKind::Hub, "iPhone", &phone_pk)), [Conflict("known here as another kind of device")]);
    // A key that does not even decode is the structural check's (bad_key).
    assert_eq!(one(intro("nnnnnn000009", DeviceKind::Phone, "Pixel", "AAAA")), [Invalid("bad_key")]);
}

#[test]
fn name_lists_read_naturally() {
    let n = |v: &[&str]| name_list(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
    assert_eq!(n(&[]), "");
    assert_eq!(n(&["iPhone"]), "iPhone");
    assert_eq!(n(&["iPhone", "Pixel"]), "iPhone and Pixel");
    assert_eq!(n(&["iPhone", "Pixel", "iPad", "Laptop"]), "iPhone, Pixel and 2 more");
}

#[test]
fn intros_are_rate_limited_per_sender_and_drops_noted_once_a_minute() {
    let mut b = HashMap::new();
    let t0 = tokio::time::Instant::now();
    for _ in 0..INTRO_BURST {
        assert_eq!(take_intro(&mut b, SENDER, t0), IntroAdmit::Admit, "a burst of {INTRO_BURST}");
    }
    assert_eq!(take_intro(&mut b, SENDER, t0), IntroAdmit::Drop { log: Some(0) }, "the first drop is noted");
    assert_eq!(take_intro(&mut b, SENDER, t0 + Duration::from_secs(1)), IntroAdmit::Drop { log: None });
    assert_eq!(take_intro(&mut b, "cccccc000003", t0), IntroAdmit::Admit, "each sender has its own budget");
    // One token back every 10 s.
    let t1 = t0 + INTRO_REFILL;
    assert_eq!(take_intro(&mut b, SENDER, t1), IntroAdmit::Admit);
    assert_eq!(take_intro(&mut b, SENDER, t1), IntroAdmit::Drop { log: None }, "still within the note window");
    // The next note (after 60 s) counts what was dropped quietly.
    assert_eq!(take_intro(&mut b, SENDER, t0 + Duration::from_secs(59)), IntroAdmit::Admit, "refilled meanwhile");
    let late = t0 + INTRO_DROP_NOTE_EVERY + Duration::from_millis(1);
    let mut admitted = 0;
    let mut noted = None;
    for _ in 0..10 {
        match take_intro(&mut b, SENDER, late) {
            IntroAdmit::Admit => admitted += 1,
            IntroAdmit::Drop { log: Some(n) } => noted = Some(n),
            IntroAdmit::Drop { log: None } => {}
        }
    }
    assert!(admitted <= INTRO_BURST as usize);
    assert_eq!(noted, Some(2), "the two quiet drops are counted in the next note");
    // Long idle: the budget is full again, never more than the burst.
    let idle = late + INTRO_REFILL * 100;
    let n = (0..10).filter(|_| take_intro(&mut b, SENDER, idle) == IntroAdmit::Admit).count();
    assert_eq!(n, INTRO_BURST as usize);
}

#[test]
fn not_added_outcomes_make_one_report() {
    let s = |id: &str, name: &str, why: SkipWhy, reason: &'static str| Skipped {
        id: id.into(),
        kind: DeviceKind::Phone,
        name: name.into(),
        why,
        reason,
    };
    let off = [s("pppppp000001", "Pixel", SkipWhy::Off, "accept_roster_intros is off")];
    let r = skip_report("Laptop", &off);
    assert_eq!((r.level, r.title.as_str()), (NotificationLevel::Info, "Laptop introduced Pixel"));
    assert!(r.body.contains("off in Settings"), "{}", r.body);
    let mut many: Vec<Skipped> = (0..20).map(|i| s(&format!("q{i:011}"), &format!("Phone {i}"), SkipWhy::Malformed, "bad_key")).collect();
    many.push(s("pppppp000001", "iPhone", SkipWhy::Conflict, "known here with another key"));
    many.push(s("pppppp000002", "Pixel", SkipWhy::Full, "the roster is full"));
    let r = skip_report("Laptop", &many);
    assert_eq!(r.level, NotificationLevel::Warning);
    assert_eq!(r.title, "Laptop introduced Phone 0, Phone 1 and 20 more: not added");
    assert!(r.body.contains("iPhone clashes") && r.body.contains("20 malformed entries") && r.body.contains("at most 64 devices"), "{}", r.body);
    assert!(r.detail.starts_with("conflict: iPhone pppppp000001 (known here with another key); malformed: Phone 0 q00000000000 (bad_key)"), "{}", r.detail);
    assert!(r.detail.contains("and 12 more") && r.detail.ends_with("full: Pixel pppppp000002 (the roster is full)"), "{}", r.detail);
}

#[test]
fn wakes_are_rate_limited_per_phone() {
    let mut wakes = HashMap::new();
    let t0 = tokio::time::Instant::now();
    assert!(take_wake(&mut wakes, "pppppp000001", t0));
    assert!(!take_wake(&mut wakes, "pppppp000001", t0 + Duration::from_secs(59)), "once per 60 s");
    assert!(take_wake(&mut wakes, "pppppp000002", t0 + Duration::from_secs(1)), "each phone has its own window");
    assert!(take_wake(&mut wakes, "pppppp000001", t0 + PHONE_WAKE_INTERVAL));
    assert!(!take_wake(&mut wakes, "pppppp000001", t0 + PHONE_WAKE_INTERVAL + Duration::from_secs(30)));
}
