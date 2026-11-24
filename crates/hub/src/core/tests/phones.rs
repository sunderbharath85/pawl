//! Step 7 over the hub: phones in the roster, the group roster (intros sent and received), phone
//! links (mirror + intents as `mobile`), phone invites and the offline wake.

use super::*;
use pawl_peer::{DeviceKeys, MemoryRoster, PeerNode};
use pawl_protocol::{IntroDevice, LinkPath, MachineOs};

const PHONE: &str = "pppppp000001";
const PIXEL: &str = "pppppp000002";
const LAPTOP: &str = "llllll000001";
const STUDIO: &str = "ssssss000001";

fn entry(id: &str, kind: DeviceKind, name: &str) -> RosterEntry {
    let os = if kind == DeviceKind::Phone { MachineOs::Ios } else { MachineOs::Macos };
    RosterEntry {
        device: Device {
            id: id.into(),
            kind,
            name: name.into(),
            public_key: DeviceKeys::generate().public_b64(),
            paired_at: 1,
            last_seen: None,
            addr: None,
            path: None,
            introduced_by: None,
            os: Some(os),
        },
        machine: Machine {
            id: id.into(),
            name: name.into(),
            os,
            last_seen: 1,
            online: false,
            hub_version: None,
            user_active_at: None,
        },
    }
}

/// Put a device in the roster without a link (as the link layer stores a pairing).
async fn add(hub: &Hub, id: &str, kind: DeviceKind, name: &str) -> RosterEntry {
    let e = entry(id, kind, name);
    hub.store().upsert_device(&e).unwrap();
    hub.load_peers().await.unwrap();
    e
}

/// The link layer's report of a completed pairing with `e` (stored first, as the node does).
async fn paired(hub: &Hub, e: &RosterEntry) {
    hub.store().upsert_device(e).unwrap();
    hub.on_peer_event(PeerEvent::Paired { device: e.device.clone(), machine: e.machine.clone(), we_invited: true }).await;
}

fn pending(hub: &Hub, recipient: &str) -> Vec<String> {
    let mut v = hub.store().pending_roster_intros(recipient).unwrap();
    v.sort();
    v
}

fn as_intro(e: &RosterEntry) -> IntroDevice {
    IntroDevice {
        id: e.device.id.clone(),
        kind: e.device.kind,
        name: e.device.name.clone(),
        public_key: e.device.public_key.clone(),
        addr: e.device.addr.clone(),
        os: e.device.os,
    }
}

async fn roster_intro(hub: &Hub, from: &str, devices: Vec<IntroDevice>, invalid: Vec<(IntroDevice, &'static str)>) {
    hub.on_peer_event(PeerEvent::RosterIntro { from: from.into(), devices, invalid }).await;
}

fn audit_kinds(hub: &Hub) -> Vec<(PairingAuditKind, Option<String>)> {
    let mut v: Vec<_> = hub.store().pairing_audit(100).unwrap().into_iter().map(|a| (a.kind, a.device_id)).collect();
    v.reverse();
    v
}

fn notifications(msgs: &[ServerMsg]) -> Vec<(NotificationLevel, String)> {
    msgs.iter()
        .filter_map(|m| match m {
            ServerMsg::Notification { notification } => Some((notification.level, notification.title.clone())),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn phones_are_devices_never_machines() {
    let mut f = fixture(None).await;
    add(&f.hub, LAPTOP, DeviceKind::Hub, "Laptop").await;
    add(&f.hub, PHONE, DeviceKind::Phone, "iPhone").await;
    drain(&mut f.rx);

    let devices = f.hub.devices();
    let phone = devices.iter().find(|d| d.id == PHONE).expect("phones are devices");
    assert_eq!((phone.kind, phone.os), (DeviceKind::Phone, Some(MachineOs::Ios)));
    let machines: Vec<String> = f.hub.machines().into_iter().map(|m| m.id).collect();
    assert_eq!(machines, [MID, LAPTOP], "never in machines");

    // Online / offline and the path like any link, but never a `machine` message.
    f.hub.on_peer_event(PeerEvent::LinkUp { device_id: PHONE.into(), machine: entry(PHONE, DeviceKind::Phone, "iPhone").machine }).await;
    f.hub.on_peer_event(PeerEvent::LinkPath { device_id: PHONE.into(), path: LinkPath::Relay }).await;
    assert_eq!(f.hub.devices().iter().find(|d| d.id == PHONE).and_then(|d| d.path), Some(LinkPath::Relay));
    // A phone's presence carries nothing we keep: no repos, agents, pets or activity.
    let now = pawl_protocol::now_ms();
    let presence = Presence {
        machine: Machine { online: true, user_active_at: Some(now), ..entry(PHONE, DeviceKind::Phone, "iPhone").machine },
        agents: vec![agent("codex")],
        repos: vec![RepoRef { name: "api".into(), path: "/srv/api".into(), last_used_at: None }],
        pets: vec![],
        user_active_at: Some(now),
        ts: now,
    };
    f.hub.on_peer_event(PeerEvent::Presence { device_id: PHONE.into(), presence }).await;
    assert_eq!(f.hub.presence_repos(PHONE), None);
    assert_eq!(f.hub.presence_agents(PHONE), None);
    assert_eq!(f.hub.store().device_repos(PHONE).unwrap(), None);
    f.hub.check_active_machine(false);
    let msgs = drain(&mut f.rx);
    assert!(!msgs.iter().any(|m| matches!(m, ServerMsg::Machine { machine } if machine.id == PHONE)), "{msgs:?}");
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Devices { devices } if devices.iter().any(|d| d.id == PHONE && d.path == Some(LinkPath::Relay)))));
    assert_eq!(f.hub.welcome().machines.iter().filter(|m| m.id == PHONE).count(), 0);
    f.hub.on_peer_event(PeerEvent::LinkDown { device_id: PHONE.into() }).await;
    assert_eq!(f.hub.devices().iter().find(|d| d.id == PHONE).and_then(|d| d.path), None, "offline: no path");
}

#[tokio::test]
async fn pairing_a_device_introduces_its_group() {
    let mut f = fixture(None).await;
    add(&f.hub, LAPTOP, DeviceKind::Hub, "Laptop").await;
    add(&f.hub, PHONE, DeviceKind::Phone, "iPhone").await;
    drain(&mut f.rx);

    // A new phone: hubs learn about it and it learns the hubs; phones never meet phones.
    let pixel = entry(PIXEL, DeviceKind::Phone, "Pixel");
    paired(&f.hub, &pixel).await;
    assert_eq!(pending(&f.hub, LAPTOP), [PIXEL]);
    assert!(pending(&f.hub, PHONE).is_empty());
    assert_eq!(pending(&f.hub, PIXEL), [LAPTOP]);
    let n = notifications(&drain(&mut f.rx));
    assert!(n.contains(&(NotificationLevel::Success, "Paired with Pixel".into())), "{n:?}");

    // A new hub meets everyone.
    let studio = entry(STUDIO, DeviceKind::Hub, "Studio");
    paired(&f.hub, &studio).await;
    assert_eq!(pending(&f.hub, LAPTOP), [PIXEL, STUDIO]);
    assert_eq!(pending(&f.hub, PHONE), [STUDIO]);
    assert_eq!(pending(&f.hub, PIXEL), [LAPTOP, STUDIO]);
    assert_eq!(pending(&f.hub, STUDIO), [LAPTOP, PHONE, PIXEL]);

    // Offline recipients keep theirs until a link can take them.
    f.hub.on_peer_event(PeerEvent::LinkUp { device_id: LAPTOP.into(), machine: entry(LAPTOP, DeviceKind::Hub, "Laptop").machine }).await;
    assert_eq!(pending(&f.hub, LAPTOP), [PIXEL, STUDIO], "no live link: still owed");

    // Revoking a device drops what it was owed and what others were owed about it.
    ack_result(f.hub.handle(ClientMsg::RevokeDevice { id: "r".into(), device_id: PIXEL.into() }, &actor()).await);
    assert!(pending(&f.hub, PIXEL).is_empty());
    assert_eq!(pending(&f.hub, LAPTOP), [STUDIO]);
    assert_eq!(pending(&f.hub, STUDIO), [LAPTOP, PHONE]);
}

#[tokio::test]
async fn received_intros_add_new_devices_once_and_never_replace() {
    let mut f = fixture(None).await;
    add(&f.hub, LAPTOP, DeviceKind::Hub, "Laptop").await;
    let phone = add(&f.hub, PHONE, DeviceKind::Phone, "iPhone").await;
    drain(&mut f.rx);

    let pixel = entry(PIXEL, DeviceKind::Phone, "Pixel");
    let mut studio = entry(STUDIO, DeviceKind::Hub, "Studio");
    studio.device.addr = Some("192.168.1.30:47801".into());
    let bad = IntroDevice { public_key: "AAAA".into(), ..as_intro(&entry("bbbbbb000009", DeviceKind::Phone, "Bad")) };
    roster_intro(&f.hub, LAPTOP, vec![as_intro(&pixel), as_intro(&studio)], vec![(bad, "bad_key")]).await;

    let devices = f.hub.devices();
    let got = devices.iter().find(|d| d.id == PIXEL).expect("added");
    assert_eq!((got.kind, got.os, got.introduced_by.as_deref()), (DeviceKind::Phone, Some(MachineOs::Ios), Some(LAPTOP)));
    assert_eq!(got.public_key, pixel.device.public_key);
    let got = devices.iter().find(|d| d.id == STUDIO).expect("added");
    assert_eq!((got.addr.as_deref(), got.introduced_by.as_deref()), (Some("192.168.1.30:47801"), Some(LAPTOP)));
    assert_eq!(
        audit_kinds(&f.hub),
        [
            (PairingAuditKind::Introduced, Some(PIXEL.into())),
            (PairingAuditKind::Introduced, Some(STUDIO.into())),
            (PairingAuditKind::IntroIgnored, Some("bbbbbb000009".into())),
        ]
    );
    let not_added = &f.hub.store().pairing_audit(1).unwrap()[0];
    assert!(not_added.detail.as_deref().unwrap().ends_with("malformed: Bad bbbbbb000009 (bad_key)"), "{not_added:?}");
    let audit = f.hub.store().pairing_audit(10).unwrap();
    assert!(audit.iter().all(|a| a.actor == "peer" && a.detail.as_deref().is_some_and(|d| d.contains("introduced by Laptop"))));
    let text = format!("{audit:?}");
    assert!(!text.contains(&pixel.device.public_key) && !text.contains(&studio.device.public_key), "never keys in the audit");
    let msgs = drain(&mut f.rx);
    let n = notifications(&msgs);
    assert!(n.contains(&(NotificationLevel::Success, "Laptop added Pixel and Studio".into())), "{n:?}");
    assert!(n.contains(&(NotificationLevel::Warning, "Laptop introduced Bad: not added".into())), "{n:?}");
    assert_eq!(n.len(), 2, "one notice for what was added, one for the rest");
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Devices { devices } if devices.iter().any(|d| d.id == PIXEL && d.introduced_by.as_deref() == Some(LAPTOP)))));
    // The wire shape the clients read.
    let wire = serde_json::to_value(f.hub.devices().into_iter().find(|d| d.id == PIXEL).unwrap()).unwrap();
    assert_eq!((wire["introducedBy"].as_str(), wire["os"].as_str(), wire["kind"].as_str()), (Some(LAPTOP), Some("ios"), Some("phone")));
    // Phones still never become machines, introduced or not; hubs do.
    assert!(f.hub.machines().iter().any(|m| m.id == STUDIO) && !f.hub.machines().iter().any(|m| m.id == PIXEL));

    // The same intro again: nothing new, nothing audited, nobody told.
    roster_intro(&f.hub, LAPTOP, vec![as_intro(&pixel)], vec![]).await;
    assert_eq!(f.hub.store().pairing_audit(100).unwrap().len(), 3);
    assert!(notifications(&drain(&mut f.rx)).is_empty());

    // Another key for a known id, a known key under a new id: conflicts, never replacements.
    let impostor = IntroDevice { public_key: DeviceKeys::generate().public_b64(), ..as_intro(&phone) };
    let thief = IntroDevice { id: "tttttt000001".into(), name: "Thief".into(), ..as_intro(&phone) };
    roster_intro(&f.hub, LAPTOP, vec![impostor, thief], vec![]).await;
    assert_eq!(f.hub.devices().iter().find(|d| d.id == PHONE).unwrap().public_key, phone.device.public_key);
    assert!(!f.hub.devices().iter().any(|d| d.id == "tttttt000001"));
    let kinds = audit_kinds(&f.hub);
    assert_eq!(&kinds[3..], [(PairingAuditKind::IntroConflict, None)], "one row for the whole intro");
    let detail = f.hub.store().pairing_audit(1).unwrap()[0].detail.clone().unwrap();
    assert!(detail.contains("iPhone pppppp000001 (known here with another key)") && detail.contains("Thief tttttt000001"), "{detail}");
    let msgs = drain(&mut f.rx);
    assert_eq!(notifications(&msgs), [(NotificationLevel::Warning, "Laptop introduced iPhone and Thief: not added".into())]);
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Devices { .. })), "clients get the roster with the notice");

    // A fourth intro within the window is dropped (burst 3, then one per 10 s): noted once, quietly after.
    roster_intro(&f.hub, LAPTOP, vec![as_intro(&entry("dddddd000001", DeviceKind::Phone, "Flood 1"))], vec![]).await;
    roster_intro(&f.hub, LAPTOP, vec![as_intro(&entry("dddddd000002", DeviceKind::Phone, "Flood 2"))], vec![]).await;
    assert!(!f.hub.devices().iter().any(|d| d.id.starts_with("dddddd")));
    let audit = f.hub.store().pairing_audit(100).unwrap();
    assert_eq!(audit.len(), 5, "one note for both drops");
    assert!(audit[0].kind == PairingAuditKind::IntroIgnored && audit[0].detail.as_deref().unwrap().contains("rate limited"));
    assert!(notifications(&drain(&mut f.rx)).is_empty(), "drops never notify");
    f.hub.reset_intro_budget();

    // A revoked device is never re-added by an intro.
    ack_result(f.hub.handle(ClientMsg::RevokeDevice { id: "r".into(), device_id: PIXEL.into() }, &actor()).await);
    roster_intro(&f.hub, LAPTOP, vec![as_intro(&pixel)], vec![]).await;
    assert!(!f.hub.devices().iter().any(|d| d.id == PIXEL));
    assert_eq!(audit_kinds(&f.hub).last(), Some(&(PairingAuditKind::IntroConflict, Some(PIXEL.into()))));
}

fn phones(n: usize, prefix: char) -> Vec<IntroDevice> {
    (0..n).map(|i| as_intro(&entry(&format!("{prefix}{i:011}"), DeviceKind::Phone, &format!("Phone {i}")))).collect()
}

#[tokio::test]
async fn intros_never_grow_the_roster_past_its_caps() {
    let mut f = fixture(None).await;
    add(&f.hub, LAPTOP, DeviceKind::Hub, "Laptop").await;
    add(&f.hub, STUDIO, DeviceKind::Hub, "Studio").await;
    // One hub may have introduced at most 32 devices here (revoked ones count).
    roster_intro(&f.hub, LAPTOP, phones(32, 'q'), vec![]).await;
    assert_eq!(f.hub.devices().len(), 34);
    ack_result(f.hub.handle(ClientMsg::RevokeDevice { id: "r".into(), device_id: "q00000000000".into() }, &actor()).await);
    drain(&mut f.rx);
    let before = f.hub.store().pairing_audit(1000).unwrap().len();
    roster_intro(&f.hub, LAPTOP, phones(2, 'r'), vec![]).await;
    assert!(!f.hub.devices().iter().any(|d| d.id.starts_with('r')), "Laptop's share is used up");
    let audit = f.hub.store().pairing_audit(1000).unwrap();
    assert_eq!(audit.len(), before + 1, "one row for the intro");
    assert!(audit[0].detail.as_deref().unwrap().contains("this machine introduced too many devices"), "{:?}", audit[0]);
    let n = notifications(&drain(&mut f.rx));
    assert_eq!(n.len(), 1, "{n:?}");
    assert_eq!(n[0], (NotificationLevel::Warning, "Laptop introduced Phone 0 and Phone 1: not added".into()));

    // The whole roster (paired + revoked rows) stops at 64 for intros.
    for i in 0..29 {
        let e = entry(&format!("h{i:011}"), DeviceKind::Hub, &format!("Hub {i}"));
        f.hub.store().upsert_device(&e).unwrap();
    }
    f.hub.load_peers().await.unwrap();
    assert_eq!(f.hub.store().roster_with_revoked().unwrap().len(), 63);
    roster_intro(&f.hub, STUDIO, phones(3, 's'), vec![]).await;
    let added: Vec<String> = f.hub.devices().into_iter().map(|d| d.id).filter(|id| id.starts_with('s') && id.len() == 12 && id != STUDIO).collect();
    assert_eq!(added, ["s00000000000"], "one more fits, then the roster is full");
    let last = &f.hub.store().pairing_audit(1).unwrap()[0];
    assert!(last.detail.as_deref().unwrap().contains("the roster is full"), "{last:?}");
}

#[tokio::test]
async fn revoking_an_introducer_lists_what_it_added() {
    let f = fixture(None).await;
    add(&f.hub, LAPTOP, DeviceKind::Hub, "Laptop").await;
    add(&f.hub, PHONE, DeviceKind::Phone, "iPhone").await;
    let pixel = entry(PIXEL, DeviceKind::Phone, "Pixel");
    let studio = entry(STUDIO, DeviceKind::Hub, "Studio");
    roster_intro(&f.hub, LAPTOP, vec![as_intro(&studio), as_intro(&pixel)], vec![]).await;
    let v = ack_result(f.hub.handle(ClientMsg::RevokeDevice { id: "r".into(), device_id: LAPTOP.into() }, &actor()).await);
    assert_eq!(
        v,
        json!({ "introduced": [{ "id": PIXEL, "name": "Pixel", "kind": "phone" }, { "id": STUDIO, "name": "Studio", "kind": "hub" }] }),
        "still paired, by name; the client offers to remove them"
    );
    assert!(f.hub.devices().iter().any(|d| d.id == PIXEL), "nothing else is revoked on its own");
    // Revoke one of them first: the list holds only what is still paired.
    let v = ack_result(f.hub.handle(ClientMsg::RevokeDevice { id: "r".into(), device_id: PIXEL.into() }, &actor()).await);
    assert_eq!(v, json!({ "introduced": [] }));
    let v = ack_result(f.hub.handle(ClientMsg::RevokeDevice { id: "r".into(), device_id: PHONE.into() }, &actor()).await);
    assert_eq!(v, json!({ "introduced": [] }), "a device that introduced nothing");
}

#[tokio::test]
async fn intros_wait_for_the_setting_and_never_come_from_phones() {
    let mut f = fixture_config(|c| c.peer.accept_roster_intros = false).await;
    add(&f.hub, LAPTOP, DeviceKind::Hub, "Laptop").await;
    add(&f.hub, PHONE, DeviceKind::Phone, "iPhone").await;
    drain(&mut f.rx);
    let pixel = entry(PIXEL, DeviceKind::Phone, "Pixel");

    roster_intro(&f.hub, LAPTOP, vec![as_intro(&pixel)], vec![]).await;
    assert!(!f.hub.devices().iter().any(|d| d.id == PIXEL), "off: shown, not added");
    let a = &f.hub.store().pairing_audit(1).unwrap()[0];
    assert_eq!((a.kind, a.device_id.as_deref()), (PairingAuditKind::IntroIgnored, Some(PIXEL)));
    assert!(a.detail.as_deref().unwrap().contains("accept_roster_intros is off"));
    let msgs = drain(&mut f.rx);
    assert!(notifications(&msgs).contains(&(NotificationLevel::Info, "Laptop introduced Pixel".into())));
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::Devices { .. })));

    // Turned on from Settings (audited like any setting), the next intro is applied.
    let patch: pawl_protocol::HubSettingsPatch = serde_json::from_value(json!({ "peer": { "acceptRosterIntros": true } })).unwrap();
    let v = ack_result(f.hub.handle(ClientMsg::UpdateSettings { id: "u".into(), patch: Box::new(patch) }, &actor()).await);
    assert_eq!(v["settings"]["peer"]["acceptRosterIntros"], true);
    assert_eq!(v["applied"], json!(["peer.acceptRosterIntros"]));
    assert!(std::fs::read_to_string(&f.hub.paths().config).unwrap().contains("accept_roster_intros = true"));
    assert_eq!(f.hub.store().settings_audit(1).unwrap()[0].keys, ["peer.acceptRosterIntros"]);
    roster_intro(&f.hub, LAPTOP, vec![as_intro(&pixel)], vec![]).await;
    assert!(f.hub.devices().iter().any(|d| d.id == PIXEL));

    // A phone never introduces anyone (the node drops it; the hub refuses it too).
    let ipad = entry("iiiiii000001", DeviceKind::Phone, "iPad");
    let before = f.hub.store().pairing_audit(100).unwrap().len();
    roster_intro(&f.hub, PHONE, vec![as_intro(&ipad)], vec![]).await;
    assert!(!f.hub.devices().iter().any(|d| d.id == "iiiiii000001"));
    let audit = f.hub.store().pairing_audit(100).unwrap();
    assert_eq!(audit.len(), before + 1);
    assert_eq!(audit[0].kind, PairingAuditKind::IntroIgnored);
}

#[tokio::test]
async fn a_phone_reads_the_mirror_and_acts_as_mobile() {
    let mut f = fixture(None).await;
    add(&f.hub, PHONE, DeviceKind::Phone, "iPhone").await;

    // Phones never feed the mirror.
    let session = remote_session(PHONE, "s1");
    let body = PeerBody::Mirror { update: MirrorUpdate::Session { session } };
    f.hub.on_peer_event(PeerEvent::Body { device_id: PHONE.into(), body }).await;
    assert!(f.hub.mirror_sessions(|_| true).is_empty());

    // Intents from a phone run as client "mobile" whatever it claims, under the step-4 rules.
    let req = pending_request(&f).await;
    drain(&mut f.rx);
    let body = PeerBody::Intent {
        intent_id: "m-1".into(),
        client: ClientKind::Desktop,
        confirmed: false,
        intent: approve(&req.request_id, "allow"),
        ts: pawl_protocol::now_ms(),
    };
    f.hub.on_peer_event(PeerEvent::Body { device_id: PHONE.into(), body }).await;
    let mut log = None;
    for _ in 0..100 {
        log = f.hub.store().intent_log(5).unwrap().into_iter().find(|l| l.intent_id == "m-1");
        if log.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let log = log.expect("the phone's intent was handled");
    assert_eq!((log.client.as_str(), log.peer_name.as_deref(), log.reason.as_deref()), ("mobile", Some("iPhone"), Some("unconfirmed")));
    assert_eq!(resolves(&f, &req.request_id), 0, "approve needs confirmed (the phone's biometric check)");

    // Confirmed: applied, audited and announced as "from iPhone".
    let a = f.hub.handle_intent(PHONE, "m-2", ClientKind::Mobile, true, &approve(&req.request_id, "allow")).await.unwrap();
    assert_eq!(a.status, IntentStatus::Applied, "{a:?}");
    let audit = &f.hub.store().audit_log(1).unwrap()[0];
    assert_eq!(audit.actor, "mobile@iPhone");
    assert_eq!(audit.origin.as_ref().map(|o| (o.client.as_str(), o.name.as_deref())), Some(("mobile", Some("iPhone"))));
    let msgs = drain(&mut f.rx);
    assert!(msgs.iter().any(|m| matches!(m, ServerMsg::PermissionResolved { by_machine_id: Some(by), .. } if by == PHONE)));
    assert!(notifications(&msgs).iter().any(|(_, t)| t == "Approved from iPhone"));

}

#[tokio::test]
async fn phone_invites_are_relay_only() {
    let f = fixture(None).await;
    let invite = |addr: Option<&str>| ClientMsg::CreateInvite { id: "i".into(), ttl_secs: None, addr: addr.map(Into::into), for_phone: Some(true) };
    assert_eq!(ack_error(f.hub.handle(invite(None), &actor()).await), "relay_off", "no relay connected");
    assert_eq!(ack_error(f.hub.handle(invite(Some("10.0.0.2:47801")), &actor()).await), "invalid", "a phone invite has no addr");
    let bad_ttl = ClientMsg::CreateInvite { id: "i".into(), ttl_secs: Some(5), addr: None, for_phone: Some(true) };
    assert_eq!(ack_error(f.hub.handle(bad_ttl, &actor()).await), "invalid");
}

#[tokio::test]
async fn local_requests_wake_offline_phones_once_a_minute() {
    let f = fixture(None).await;
    add(&f.hub, LAPTOP, DeviceKind::Hub, "Laptop").await;
    add(&f.hub, PHONE, DeviceKind::Phone, "iPhone").await;
    add(&f.hub, PIXEL, DeviceKind::Phone, "Pixel").await;
    assert!(f.hub.phone_wakes().is_empty());
    pending_request(&f).await;
    let first = f.hub.phone_wakes();
    let mut woken: Vec<&String> = first.keys().collect();
    woken.sort();
    assert_eq!(woken, [PHONE, PIXEL], "every offline phone, never a hub");
    pending_request(&f).await;
    assert_eq!(f.hub.phone_wakes(), first, "at most once per phone per minute");
    // A phone that left the roster is forgotten.
    ack_result(f.hub.handle(ClientMsg::RevokeDevice { id: "r".into(), device_id: PIXEL.into() }, &actor()).await);
    pending_request(&f).await;
    assert_eq!(f.hub.phone_wakes().keys().collect::<Vec<_>>(), [PHONE]);
}

/// A real link: a phone (a peer node that says it runs iOS) pairs with this hub over the LAN
/// listener, then reads this hub's mirror and sends an intent, as the mobile app does over the relay.
#[tokio::test]
async fn a_linked_phone_gets_this_hubs_items_only() {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let bound = format!("127.0.0.1:{port}");
    let listen = bound.clone();
    let f = fixture_config(move |c| c.peer.listen = Some(listen)).await;
    f.hub.start_peer_links().await.unwrap();
    // A hub this one already pairs with (offline): the phone is introduced to it and it to the phone.
    add(&f.hub, LAPTOP, DeviceKind::Hub, "Laptop").await;
    let uri = ack_result(f.hub.handle(ClientMsg::CreateInvite { id: "i".into(), ttl_secs: None, addr: Some(bound.clone()), for_phone: None }, &actor()).await)["uri"]
        .as_str()
        .unwrap()
        .to_string();
    let me = Machine { online: true, ..entry(PHONE, DeviceKind::Phone, "iPhone").machine };
    let keys = DeviceKeys::generate();
    let phone_pk = keys.public_b64();
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let roster = Arc::new(MemoryRoster::new());
    let phone = PeerNode::new(keys, me, roster.clone(), tx);
    // A hub invite is for hubs: a joiner that says it is a phone is refused (invite purpose).
    assert_eq!(phone.join_invite(&uri).await.unwrap_err(), pawl_peer::JoinError::Rejected);
    assert!(!f.hub.devices().iter().any(|d| d.id == PHONE));

    // Paired as a phone (as a phone invite over the relay would store it), with an intro owed to it.
    let mut stored = entry(PHONE, DeviceKind::Phone, "iPhone");
    stored.device.public_key = phone_pk;
    f.hub.store().upsert_device(&stored).unwrap();
    f.hub.store().queue_roster_intros(PHONE, &[LAPTOP.to_string()], 1).unwrap();
    f.hub.load_peers().await.unwrap();
    let hub_entry = RosterEntry {
        device: Device {
            id: MID.into(),
            kind: DeviceKind::Hub,
            name: "Test PC".into(),
            public_key: f.hub.peer_status().public_key,
            paired_at: 1,
            last_seen: None,
            addr: Some(bound),
            path: None,
            introduced_by: None,
            os: None,
        },
        machine: Machine {
            id: MID.into(),
            name: "Test PC".into(),
            os: MachineOs::Windows,
            last_seen: 1,
            online: false,
            hub_version: None,
            user_active_at: None,
        },
    };
    pawl_peer::Roster::upsert(&*roster, hub_entry);
    phone.connect_known();

    let mut linked = false;
    for _ in 0..200 {
        if f.hub.devices().iter().any(|d| d.id == PHONE && d.kind == DeviceKind::Phone && d.path == Some(LinkPath::Lan)) {
            linked = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(linked, "the phone's hello links it: {:?}", f.hub.devices());
    assert!(!f.hub.machines().iter().any(|m| m.id == PHONE));
    let kept = f.hub.store().roster_entry(PHONE).unwrap().unwrap();
    assert_eq!((kept.device.kind, kept.device.addr), (DeviceKind::Phone, None), "a hello never changes the kind");

    // A mirrored item of another machine must never be relayed to the phone.
    roster_peer(&f.hub, OTHER).await;
    let mirrored = remote_session(OTHER, "theirs");
    f.hub.on_peer_event(PeerEvent::Body { device_id: OTHER.into(), body: PeerBody::Mirror { update: MirrorUpdate::Session { session: mirrored } } }).await;

    // sync_request is answered like a hub's: this hub's own items, then sync_done.
    let s = start(&f).await;
    phone.send(MID, PeerBody::SyncRequest { since_seq: 0 }).unwrap();
    let mut snapshot = None;
    let mut done = false;
    let mut live = false;
    let mut intro: Option<Vec<String>> = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !(done && live) && tokio::time::Instant::now() < deadline {
        let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(200), events.recv()).await else {
            if done && !live {
                // A live update after the sync: a new local notification goes to linked phones too.
                f.hub.inner.bus.send(ServerMsg::Notification {
                    notification: notify::make(MID, NotificationLevel::Info, "hello phone", "body", None, None),
                });
            }
            continue;
        };
        if let PeerEvent::RosterIntro { from, devices, invalid } = &ev {
            assert_eq!((from.as_str(), invalid.len()), (MID, 0));
            intro = Some(devices.iter().map(|d| d.id.clone()).collect());
        }
        if let PeerEvent::Body { body, .. } = ev {
            match body {
                PeerBody::MirrorSnapshot { sessions, .. } => snapshot = Some(sessions),
                PeerBody::SyncDone { .. } => done = true,
                PeerBody::Mirror { update: MirrorUpdate::Notification { notification } } if notification.title == "hello phone" => live = true,
                PeerBody::Mirror { update: MirrorUpdate::Session { session } } => assert_eq!(session.machine_id, MID),
                _ => {}
            }
        }
    }
    let sessions = snapshot.expect("a snapshot");
    assert!(sessions.iter().any(|x| x.id == s.id));
    assert!(sessions.iter().all(|x| x.machine_id == MID), "only this hub's own items");
    assert!(done && live, "sync_done and live updates reach the phone");
    // The intro it was owed went out on its link (phones hello first; hubs never dial them).
    assert_eq!(intro.as_deref(), Some(&[LAPTOP.to_string()][..]));
    assert!(pending(&f.hub, PHONE).is_empty());

    // An intent over the link is answered on it; client "mobile".
    let body = PeerBody::Intent {
        intent_id: "p-1".into(),
        client: ClientKind::Desktop,
        confirmed: false,
        intent: Intent::Cancel { session_id: global_id(MID, "missing") },
        ts: pawl_protocol::now_ms(),
    };
    phone.send(MID, body).unwrap();
    let ack = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(PeerEvent::Body { body: PeerBody::IntentAck { intent_id, status, .. }, .. }) = events.recv().await {
                if intent_id == "p-1" {
                    break status;
                }
            }
        }
    })
    .await
    .expect("the phone gets intent_ack");
    assert_eq!(ack, IntentStatus::Rejected);
    let log = f.hub.store().intent_log(5).unwrap().into_iter().find(|l| l.intent_id == "p-1").unwrap();
    assert_eq!((log.client.as_str(), log.peer_name.as_deref()), ("mobile", Some("iPhone")));
    phone.shutdown().await;
    f.hub.shutdown().await;
}

/// Pet names reach paired devices (presence, mirror): the device-name rule applies to them too.
#[test]
fn pet_names_follow_the_device_name_rule() {
    assert_eq!(validate_pet_name("  Biscuit ").unwrap(), "Biscuit");
    assert_eq!(validate_pet_name("Bisc\u{00FC}it \u{1F436}").unwrap(), "Bisc\u{00FC}it \u{1F436}");
    for bad in ["Bis\u{200B}cuit", "\u{202E}tiucsiB", "Bis\u{2066}cuit", "Bis\ncuit", "", &"x".repeat(41)] {
        let e = validate_pet_name(bad).unwrap_err();
        assert_eq!(AckCode::find(&e), Some(AckCode::Invalid), "{bad:?}: {e:#}");
    }
    assert!(validate_pet_name("Bis\u{200B}cuit").unwrap_err().to_string().contains("invisible characters"));
}
