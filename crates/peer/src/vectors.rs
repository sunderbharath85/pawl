//! Cross-language test vectors: `crates/peer/tests/vectors/*.json` (format: docs/reports/sde-peer.md).
//!
//! `vectors_match_this_crate` regenerates every file in memory and compares it with the committed one
//! (`PAWL_REGEN_VECTORS=1` rewrites them instead). `vector_files_hold` then checks each committed
//! vector against the crate independently: envelopes open to their body, rejects are refused with the
//! stated error, invites parse to their fields, auth boxes open on the relay side.

use crate::b64;
use crate::envelope::{nonce_with, open, seal_with_nonce, NONCE_LEN, RANDOM_LEN};
use crate::intro::{check_intro_device, device_name_ok, MAX_INTRO_NAME_CHARS};
use crate::invite::{Invite, InviteError};
use crate::keys::DeviceKeys;
use crate::relay::{address, auth_answer};
use crypto_box::aead::Aead;
use pawl_protocol::{
    ByeReason, ClientKind, DeviceKind, Intent, IntentStatus, IntroDevice, Machine, MachineOs, PeerBody, PeerEnvelope,
    Presence, RELAY_AUTH_CONTEXT,
};
use serde_json::{json, Map, Value};
use std::path::PathBuf;

const REGEN_ENV: &str = "PAWL_REGEN_VECTORS";

/// A fixed key pair. The secret is `start, start+1, ..., start+31`.
struct Party {
    name: &'static str,
    id: &'static str,
    start: u8,
}

const HUB_A: Party = Party { name: "hubA", id: "hubaaaaaaaaa", start: 0x01 };
const HUB_B: Party = Party { name: "hubB", id: "hubbbbbbbbbb", start: 0x41 };
const PHONE: Party = Party { name: "phone", id: "phone0000001", start: 0x81 };
/// The relay's per-connection key (a real relay picks a fresh one per socket).
const RELAY: Party = Party { name: "relay", id: "", start: 0xc1 };
const PARTIES: [&Party; 4] = [&HUB_A, &HUB_B, &PHONE, &RELAY];

impl Party {
    fn secret(&self) -> [u8; 32] {
        std::array::from_fn(|i| self.start.wrapping_add(i as u8))
    }
    fn keys(&self) -> DeviceKeys {
        DeviceKeys::from_secret_bytes(self.secret())
    }
    fn pk(&self) -> [u8; 32] {
        self.keys().public_key()
    }
}

fn party(name: &str) -> &'static Party {
    PARTIES.into_iter().find(|p| p.name == name).unwrap_or_else(|| panic!("unknown party {name}"))
}

/// Seqs look like real ones (`now_ms * 1000`, below 2^53).
const SEQ_BASE: u64 = 1_764_000_000_000_000;
const TS: i64 = 1_764_000_000_000;

fn random_part(n: u8) -> [u8; RANDOM_LEN] {
    std::array::from_fn(|i| n.wrapping_mul(37).wrapping_add((i as u8).wrapping_mul(11)).wrapping_add(5))
}

fn machine(p: &Party, name: &str, os: MachineOs) -> Machine {
    Machine {
        id: p.id.into(),
        name: name.into(),
        os,
        last_seen: TS,
        online: true,
        hub_version: (os != MachineOs::Ios).then(|| "0.7.0".into()),
        user_active_at: None,
    }
}

fn hub_a_machine() -> Machine {
    machine(&HUB_A, "Mac mini", MachineOs::Macos)
}

fn phone_machine() -> Machine {
    machine(&PHONE, "Test iPhone", MachineOs::Ios)
}

/// (name, sender, receiver, body, include_pk)
fn envelope_cases() -> Vec<(&'static str, &'static Party, &'static Party, PeerBody, bool)> {
    let presence = |m: Machine| PeerBody::Presence {
        presence: Presence { machine: m, agents: vec![], repos: vec![], pets: vec![], user_active_at: None, ts: TS + 2 },
    };
    vec![
        (
            "pair_request phone to hub (carries pk)",
            &PHONE,
            &HUB_A,
            PeerBody::PairRequest { code: "ESIzRFVmd4iZqrvM3e7_AA".into(), machine: phone_machine(), addr: None, ts: TS },
            true,
        ),
        (
            "pair_accept hub to phone",
            &HUB_A,
            &PHONE,
            PeerBody::PairAccept { machine: hub_a_machine(), addr: None, req_seq: SEQ_BASE + 1, ts: TS + 1 },
            false,
        ),
        ("hello phone to hub", &PHONE, &HUB_A, PeerBody::Hello { machine: phone_machine(), addr: None, ts: TS + 2 }, false),
        ("presence hub to phone", &HUB_A, &PHONE, presence(hub_a_machine()), false),
        ("presence phone to hub", &PHONE, &HUB_A, presence(phone_machine()), false),
        ("sync_request phone to hub", &PHONE, &HUB_A, PeerBody::SyncRequest { since_seq: 0 }, false),
        (
            "mirror_snapshot hub to phone",
            &HUB_A,
            &PHONE,
            PeerBody::MirrorSnapshot { sessions: vec![], pets: vec![], pending_permissions: vec![] },
            false,
        ),
        (
            "intent phone to hub (approve)",
            &PHONE,
            &HUB_A,
            PeerBody::Intent {
                intent_id: "01JD7Q8ZK3M5N7P9R1T3V5X7Z9".into(),
                client: ClientKind::Mobile,
                confirmed: true,
                intent: Intent::Approve { request_id: "req-42".into(), option_id: "allow_once".into() },
                ts: TS + 3,
            },
            false,
        ),
        (
            "intent_ack hub to phone",
            &HUB_A,
            &PHONE,
            PeerBody::IntentAck {
                intent_id: "01JD7Q8ZK3M5N7P9R1T3V5X7Z9".into(),
                status: IntentStatus::Applied,
                reason: None,
                result: None,
            },
            false,
        ),
        (
            "roster_intro hub to phone (the other hubs)",
            &HUB_A,
            &PHONE,
            PeerBody::RosterIntro {
                devices: vec![IntroDevice {
                    id: HUB_B.id.into(),
                    kind: DeviceKind::Hub,
                    name: "Studio PC".into(),
                    public_key: b64::encode(&HUB_B.pk()),
                    addr: Some("192.168.1.21:47801".into()),
                    os: Some(MachineOs::Windows),
                }],
            },
            false,
        ),
        (
            "roster_intro hub to hub (the new phone)",
            &HUB_A,
            &HUB_B,
            PeerBody::RosterIntro {
                devices: vec![IntroDevice {
                    id: PHONE.id.into(),
                    kind: DeviceKind::Phone,
                    name: "Test iPhone".into(),
                    public_key: b64::encode(&PHONE.pk()),
                    addr: None,
                    os: Some(MachineOs::Ios),
                }],
            },
            false,
        ),
        ("bye phone to hub", &PHONE, &HUB_A, PeerBody::Bye { reason: ByeReason::Shutdown, ts: TS + 4 }, false),
    ]
}

fn hex(b: &[u8]) -> String {
    hex::encode(b)
}

fn keys_file() -> Value {
    let keys: Vec<Value> = PARTIES
        .iter()
        .map(|p| {
            let pk = p.pk();
            json!({
                "name": p.name,
                "id": if p.id.is_empty() { Value::Null } else { p.id.into() },
                "secretKeyHex": hex(&p.secret()),
                "publicKeyHex": hex(&pk),
                "publicKey": b64::encode(&pk),
                "relayAddress": address(&pk),
            })
        })
        .collect();
    json!({
        "description": "Fixed X25519 key pairs (secret = 32 consecutive bytes). publicKey = standard padded base64 \
                        (Device.publicKey, envelope pk); relayAddress = base64url unpadded (relay ?pk=, allow, send.to, msg.from). \
                        id = device id. For tests only.",
        "keys": keys,
    })
}

fn base64_file() -> Value {
    let samples: Vec<Vec<u8>> = vec![
        vec![],
        b"f".to_vec(),
        b"fo".to_vec(),
        b"foo".to_vec(),
        b"foob".to_vec(),
        vec![0xfb, 0xff],
        vec![0xfb, 0xff, 0xbf],
        vec![0x00; 16],
        (0u8..16).map(|i| i.wrapping_mul(0x11)).collect(),
        (0u8..24).map(|i| 0xff - i).collect(),
        HUB_A.pk().to_vec(),
    ];
    let samples: Vec<Value> =
        samples.iter().map(|b| json!({ "hex": hex(b), "std": b64::encode(b), "url": b64::encode_url(b) })).collect();
    json!({
        "description": "std = RFC 4648 base64, padded; url = base64url, no padding. Decoders are strict: the invalid \
                        strings must be refused.",
        "samples": samples,
        "invalidStd": ["Zg", "Zg=", "Zh==", "Zm9=", "Z===", "Zm9v=", "-_8=", "Zm 9v", "Zm9v\n"],
        "invalidUrl": ["Zg==", "Zh", "Zm9", "+/8", "Z", "Zm 9v"],
    })
}

fn envelope_vector(i: usize, name: &str, from: &Party, to: &Party, body: &PeerBody, include_pk: bool) -> Value {
    let seq = SEQ_BASE + i as u64;
    let random = random_part(i as u8);
    let nonce = nonce_with(&random, &from.pk(), &to.pk(), seq);
    let plaintext = serde_json::to_string(body).expect("PeerBody serializes");
    let env = seal_with_nonce(&from.keys(), from.id, to.id, &to.pk(), seq, plaintext.as_bytes(), include_pk, &nonce);
    json!({
        "name": name,
        "sender": from.name,
        "receiver": to.name,
        "seq": seq,
        "randomHex": hex(&random),
        "dir": nonce[RANDOM_LEN],
        "nonceHex": hex(&nonce),
        "plaintext": plaintext,
        "body": serde_json::to_value(body).unwrap(),
        "envelope": serde_json::to_value(&env).unwrap(),
    })
}

fn envelopes_file() -> Value {
    let vectors: Vec<Value> = envelope_cases()
        .iter()
        .enumerate()
        .map(|(i, (name, from, to, body, pk))| envelope_vector(i + 1, name, from, to, body, *pk))
        .collect();
    // Rejects, derived from vector "presence hub to phone" (receiver: phone).
    let base: PeerEnvelope = serde_json::from_value(vectors[3]["envelope"].clone()).unwrap();
    let mut reflected = base.clone();
    (reflected.from, reflected.to) = (base.to.clone(), base.from.clone());
    let mut bumped = base.clone();
    bumped.seq += 1;
    let mut tampered = base.clone();
    let mut ct = b64::decode(&base.ciphertext).unwrap();
    ct[20] ^= 0x01;
    tampered.ciphertext = b64::encode(&ct);
    let mut renonced = base.clone();
    let mut n = b64::decode(&base.nonce).unwrap();
    n[0] ^= 0x01;
    renonced.nonce = b64::encode(&n);
    let mut version = base.clone();
    version.v = 2;
    let mut short = base.clone();
    short.nonce = b64::encode(&n[..NONCE_LEN - 1]);
    let reject = |name: &str, receiver: &Party, sender: &Party, env: &PeerEnvelope, error: &str| {
        json!({ "name": name, "receiver": receiver.name, "sender": sender.name, "envelope": env, "error": error })
    };
    let rejects = vec![
        reject("reflected back to its sender (direction byte)", &HUB_A, &PHONE, &reflected, "bad_nonce"),
        reject("seq changed (no longer the one in the nonce)", &PHONE, &HUB_A, &bumped, "bad_nonce"),
        reject("box tampered", &PHONE, &HUB_A, &tampered, "decrypt_failed"),
        reject("random part of the nonce changed", &PHONE, &HUB_A, &renonced, "decrypt_failed"),
        reject("unknown envelope version", &PHONE, &HUB_A, &version, "bad_version"),
        reject("nonce of 23 bytes", &PHONE, &HUB_A, &short, "bad_encoding"),
        // Fails on the direction byte or the box, depending on how the keys compare.
        reject("opened with the wrong sender key", &PHONE, &HUB_B, &base, &open(&PHONE.keys(), &base, &HUB_B.pk()).unwrap_err().to_string()),
    ];
    let replay = json!({
        "name": "replay: the receiver's last accepted seq from this sender is the envelope's own",
        "receiver": PHONE.name,
        "sender": HUB_A.name,
        "lastRxSeq": base.seq,
        "envelope": base,
        "error": "replay",
    });
    json!({
        "description": "PeerEnvelope = NaCl box (XSalsa20-Poly1305, tweetnacl nacl.box) of the UTF-8 plaintext (the JSON \
                        PeerBody, byte for byte as given) from the sender's key to the receiver's. nonce = random (15 bytes) \
                        || dir (1 byte: 1 if senderPk < receiverPk bytewise, else 0) || seq (u64 big-endian). Sealing \
                        `plaintext` with nonceHex must give envelope.box exactly; opening envelope must give body. \
                        A receiver checks v == 1, nonce length 24, dir and seq against the nonce, then opens; it drops any \
                        seq <= its last accepted seq from that sender before opening. Error codes are crates/peer OpenError.",
        "vectors": vectors,
        "rejects": rejects,
        "replay": replay,
    })
}

fn invite(addr: Option<&str>, relay: Option<(&str, &str)>, key: Option<&str>) -> Invite {
    Invite {
        machine_id: HUB_A.id.into(),
        public_key: HUB_A.pk(),
        addr: addr.map(Into::into),
        code: b64::encode_url(&std::array::from_fn::<u8, 16, _>(|i| 0x10 + i as u8)),
        expires_at: TS + 600_000,
        relay: relay.map(|(url, _)| url.into()),
        relay_token: relay.map(|(_, rt)| rt.into()),
        relay_key: key.map(Into::into),
    }
}

fn invite_json(name: &str, inv: &Invite) -> Value {
    json!({
        "name": name,
        "uri": inv.to_uri(),
        "machineId": inv.machine_id,
        "publicKey": b64::encode(&inv.public_key),
        "addr": inv.addr,
        "code": inv.code,
        "expiresAt": inv.expires_at,
        "relay": inv.relay,
        "relayToken": inv.relay_token,
        "relayKey": inv.relay_key,
    })
}

fn invites_file() -> Value {
    let rt = b64::encode_url(&std::array::from_fn::<u8, 16, _>(|i| 0xa0 + i as u8));
    let valid = vec![
        invite_json("LAN only", &invite(Some("192.168.1.20:47801"), None, None)),
        invite_json(
            "LAN plus relay",
            &invite(Some("192.168.1.20:47801"), Some(("wss://relay.example.com", &rt)), Some("k3y_example-123")),
        ),
        invite_json(
            "relay only (phone QR)",
            &invite(None, Some(("wss://relay.example.com", &rt)), Some("k3y_example-123")),
        ),
        invite_json(
            "IPv6 addr, relay with port and path, no access key",
            &invite(Some("[fe80::1]:47801"), Some(("wss://relay.example.com:8443/pawl", &rt)), None),
        ),
        invite_json("local dev relay (ws on loopback)", &invite(None, Some(("ws://127.0.0.1:8787", &rt)), Some("devkey"))),
    ];
    let good = invite(Some("192.168.1.20:47801"), Some(("wss://relay.example.com", &rt)), Some("k3y_example-123")).to_uri();
    let relay_only = invite(None, Some(("wss://relay.example.com", &rt)), Some("k3y_example-123")).to_uri();
    let pk = format!("pk={}", b64::encode_url(&HUB_A.pk()));
    let bad = |name: &str, uri: String| json!({ "name": name, "uri": uri });
    let invalid = vec![
        bad("other scheme", good.replace("pawl://peer?", "https://peer?")),
        bad("version 2", good.replace("v=1", "v=2")),
        bad("upper-case machine id", good.replace(HUB_A.id, "HUBAAAAAAAAA")),
        bad("short public key", good.replace(&pk, "pk=AAAA")),
        bad("public key of small order", good.replace(&pk, &format!("pk={}", b64::encode_url(&[0u8; 32])))),
        bad("code of 15 bytes", good.replace("code=EBESExQVFhcYGRobHB0eHw", "code=EBESExQVFhcYGRobHB0e")),
        bad("expiry not a number", good.replace("&exp=", "&exp=x")),
        bad("duplicate parameter", format!("{good}&v=1")),
        bad("no addr and no relay", relay_only.split("&relay=").next().unwrap().to_string()),
        bad("relay without rt", good.replace(&format!("&rt={rt}"), "")),
        bad("rk without relay", format!("{}&rk=abc", invite(Some("192.168.1.20:47801"), None, None).to_uri())),
        bad("ws relay off loopback", good.replace("wss%3A%2F%2Frelay.example.com", "ws%3A%2F%2Frelay.example.com")),
        bad("addr without port", good.replace("192.168.1.20%3A47801", "192.168.1.20").replace("192.168.1.20:47801", "192.168.1.20")),
        bad("fragment", format!("{good}#x")),
    ];
    json!({
        "description": "Invite URIs: pawl://peer?v=1&mid=&pk=(base64url)&addr=(host:port, optional with relay)&code=(base64url \
                        16 bytes)&exp=(unix ms)[&relay=(percent-encoded URL)&rt=(base64url 16 bytes)[&rk=(access key)]]. \
                        Parsing is strict (any invalid or duplicate known parameter refuses the URI; unknown ones are ignored). \
                        publicKey here is the standard base64 form of pk. relay is the normalized URL.",
        "valid": valid,
        "invalid": invalid,
    })
}

fn challenge_nonce(byte15: u8) -> String {
    let mut n: [u8; 24] = std::array::from_fn(|i| 0x30 + i as u8);
    n[RANDOM_LEN] = byte15;
    b64::encode(&n)
}

fn auth_vector(name: &str, device: &Party, host: &str, byte15: u8, url: &str, key: &str) -> Value {
    let nonce = challenge_nonce(byte15);
    let relay_pk = b64::encode(&RELAY.pk());
    let auth = auth_answer(&device.keys(), &nonce, &relay_pk, host).expect("a valid challenge");
    json!({
        "name": name,
        "device": device.name,
        "relayUrl": url,
        "accessKey": key,
        "connectUrl": format!("{url}/v1/connect?pk={}&k={key}", address(&device.pk())),
        "host": host,
        "challenge": { "type": "challenge", "nonce": nonce, "relayPk": relay_pk, "v": 1 },
        "plaintext": format!("{RELAY_AUTH_CONTEXT}:{nonce}:{host}"),
        "auth": { "type": "auth", "box": auth },
    })
}

fn relay_auth_file() -> Value {
    let valid = vec![
        auth_vector("phone on a deployed relay", &PHONE, "relay.example.com", 2, "wss://relay.example.com", "k3y_example-123"),
        auth_vector("hub on wrangler dev", &HUB_A, "127.0.0.1:8787", 0xc8, "ws://127.0.0.1:8787", "devkey"),
    ];
    let refused = |name: &str, nonce: String, relay_pk: String| {
        json!({ "name": name, "device": PHONE.name, "host": "relay.example.com", "nonce": nonce, "relayPk": relay_pk, "error": "bad_auth" })
    };
    let relay_pk = b64::encode(&RELAY.pk());
    let refused = vec![
        refused("nonce byte 15 = 0 (envelope nonce domain)", challenge_nonce(0), relay_pk.clone()),
        refused("nonce byte 15 = 1 (envelope nonce domain)", challenge_nonce(1), relay_pk),
        refused("relayPk is the device's own key", challenge_nonce(2), b64::encode(&PHONE.pk())),
    ];
    json!({
        "description": "Relay auth: answer challenge{nonce, relayPk} with auth{box}, box = NaCl box of the UTF-8 plaintext \
                        `${RELAY_AUTH_CONTEXT}:${nonce}:${host}` (nonce exactly as received, host = the Host header sent) \
                        with the challenge nonce, from the device key to relayPk (relay secret: keys.json relay). nonce, relayPk \
                        and box are standard padded base64. Refuse (error bad_auth, close, back off) a nonce whose byte 15 is \
                        0 or 1 and a relayPk equal to the device's own key or any paired device's key.",
        "valid": valid,
        "refused": refused,
    })
}

fn name_case(name: &str, note: &str) -> Value {
    json!({ "name": name, "utf8Hex": hex(name.as_bytes()), "note": note })
}

fn names_file() -> Value {
    let a = |n: usize| "a".repeat(n);
    let valid = vec![
        name_case("Mac mini", "plain"),
        name_case("x", "one character"),
        name_case(&a(64), "64 characters"),
        name_case(&"\u{00E9}".repeat(64), "64 two-byte characters"),
        name_case(&"\u{1F600}".repeat(64), "64 astral characters (128 UTF-16 units): count code points"),
        name_case(&format!("  {}  ", a(64)), "64 characters once trimmed"),
        name_case(&format!("\u{3000}{}\u{00A0}", a(64)), "ideographic space and NBSP are trimmed (White_Space)"),
        name_case("Sam\u{2019}s Mac \u{1F600}", "visible punctuation and emoji"),
        name_case("B\u{00FC}ro-PC \u{5DE5}\u{4F5C}\u{7AD9}", "Latin-1 and CJK"),
        name_case("\u{062D}\u{0627}\u{0633}\u{0648}\u{0628}", "right-to-left letters (no explicit bidi control)"),
        name_case("Mac  mini", "inner spaces are kept"),
    ];
    let mut invalid = vec![
        name_case("", "empty"),
        name_case("   ", "only spaces"),
        name_case(&a(65), "65 characters"),
        name_case(&"\u{1F600}".repeat(65), "65 astral characters"),
        name_case("\tMac", "control character, even where trimming would remove it"),
        name_case("Mac\n", "trailing line feed"),
        name_case("a\u{0007}b", "BEL"),
        name_case("a\u{007F}b", "DEL"),
        name_case("a\u{0085}b", "NEL (C1 control)"),
        name_case("a\u{009B}b", "CSI (C1 control)"),
        name_case("\u{FEFF}Mac", "leading BOM (check before trimming: JS trim() removes it)"),
        name_case("Mac\u{200B}", "trailing zero-width space (not White_Space, so not trimmed)"),
    ];
    let invisible: Vec<u32> = [0x061C]
        .into_iter()
        .chain(0x200B..=0x200F)
        .chain(0x2028..=0x202E)
        .chain(0x2060..=0x2064)
        .chain(0x2066..=0x2069)
        .chain([0xFEFF])
        .collect();
    for cp in invisible {
        let c = char::from_u32(cp).unwrap();
        invalid.push(name_case(&format!("Mac{c}mini"), &format!("U+{cp:04X} inside")));
    }
    let pk = |p: &Party| b64::encode(&p.pk());
    let dev = |kind: DeviceKind, os: Option<MachineOs>, addr: Option<&str>, name: &str, key: String| IntroDevice {
        id: "dev000000001".into(),
        kind,
        name: name.into(),
        public_key: key,
        addr: addr.map(Into::into),
        os,
    };
    let hub = |os: Option<MachineOs>, addr: Option<&str>| dev(DeviceKind::Hub, os, addr, "Studio PC", pk(&HUB_B));
    let phone = |os: Option<MachineOs>, addr: Option<&str>| dev(DeviceKind::Phone, os, addr, "Test iPhone", pk(&PHONE));
    let case = |d: IntroDevice, note: &str| {
        let result = match check_intro_device(&d) {
            Ok(_) => "ok",
            Err(code) => code,
        };
        json!({ "device": d, "result": result, "note": note })
    };
    let intro_devices = vec![
        case(hub(Some(MachineOs::Windows), Some("192.168.1.21:47801")), "hub with a LAN addr"),
        case(hub(None, None), "hub, os unknown"),
        case(hub(Some(MachineOs::Linux), Some("100.101.102.103:47801")), "Tailscale CGNAT addr"),
        case(hub(Some(MachineOs::Macos), Some("[fd00::1]:47801")), "IPv6 ULA addr"),
        case(phone(Some(MachineOs::Ios), None), "phone on iOS"),
        case(phone(Some(MachineOs::Android), None), "phone on Android"),
        case(hub(Some(MachineOs::Ios), None), "a hub claiming iOS"),
        case(hub(Some(MachineOs::Android), None), "a hub claiming Android"),
        case(phone(None, None), "a phone without os"),
        case(phone(Some(MachineOs::Linux), None), "a phone claiming Linux"),
        case(phone(Some(MachineOs::Ios), Some("192.168.1.30:47801")), "a phone with an addr"),
        case(hub(Some(MachineOs::Linux), Some("8.8.8.8:47801")), "public IP addr"),
        case(hub(Some(MachineOs::Linux), Some("studio.local:47801")), "DNS name addr"),
        case(hub(Some(MachineOs::Linux), Some("192.168.1.21")), "addr without port"),
        case(IntroDevice { id: "Dev000000001".into(), ..hub(None, None) }, "upper-case id"),
        case(IntroDevice { id: "dev00000001".into(), ..hub(None, None) }, "11-character id"),
        case(IntroDevice { name: "Studio\u{202E}PC".into(), ..hub(None, None) }, "bidi override in the name"),
        case(IntroDevice { name: a(65), ..hub(None, None) }, "65-character name"),
        case(IntroDevice { public_key: b64::encode(&[0u8; 32]), ..hub(None, None) }, "key of small order"),
        case(IntroDevice { public_key: b64::encode_url(&HUB_B.pk()), ..hub(None, None) }, "key in base64url"),
        case(IntroDevice { public_key: b64::encode(&HUB_B.pk()[..31]), ..hub(None, None) }, "31-byte key"),
    ];
    json!({
        "description": "Name and introduced-device rules (crates/peer intro.rs). A name is valid iff (1) no character anywhere in \
                        the untrimmed name is a control character (general category Cc: U+0000-001F, U+007F-009F) or one of \
                        U+061C, U+200B-200F, U+2028-202E, U+2060-2064, U+2066-2069, U+FEFF, and (2) after trimming Unicode \
                        White_Space from both ends it has 1..=maxChars Unicode scalar values (code points, not UTF-16 units). \
                        utf8Hex is the exact name. introDevices: check_intro_device(device) gives result (ok or a code); codes \
                        are checked in the order bad_id, bad_name, bad_kind, bad_key, bad_addr. bad_kind: kind phone iff os is \
                        ios / android (a hub may have no os; a phone must have one). The receiving node also refuses \
                        own_device / sender_device / duplicate, which depend on context and are not listed here.",
        "maxChars": MAX_INTRO_NAME_CHARS,
        "valid": valid,
        "invalid": invalid,
        "introDevices": intro_devices,
    })
}

fn generate() -> Vec<(&'static str, Value)> {
    vec![
        ("names.json", names_file()),
        ("keys.json", keys_file()),
        ("base64.json", base64_file()),
        ("envelopes.json", envelopes_file()),
        ("invites.json", invites_file()),
        ("relay_auth.json", relay_auth_file()),
    ]
}

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("vectors")
}

/// Objects with sorted keys, whatever serde_json's map type: stable files.
fn sorted(v: Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut entries: Vec<(String, Value)> = m.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().map(|(k, v)| (k, sorted(v))).collect::<Map<_, _>>())
        }
        Value::Array(a) => Value::Array(a.into_iter().map(sorted).collect()),
        v => v,
    }
}

fn regen() -> bool {
    std::env::var(REGEN_ENV).is_ok_and(|v| v == "1")
}

/// The committed file (while regenerating, the fresh value: the files are being rewritten meanwhile).
fn read(file: &str) -> Value {
    if regen() {
        return sorted(generate().into_iter().find(|(f, _)| *f == file).expect("a vector file").1);
    }
    let path = dir().join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e} (run with {REGEN_ENV}=1)", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn vectors_match_this_crate() {
    let regen = regen();
    if regen {
        std::fs::create_dir_all(dir()).unwrap();
    }
    for (file, value) in generate() {
        let value = sorted(value);
        if regen {
            let text = serde_json::to_string_pretty(&value).unwrap() + "\n";
            std::fs::write(dir().join(file), text).unwrap();
        } else {
            assert!(read(file) == value, "{file} differs from what pawl-peer produces; regenerate with {REGEN_ENV}=1");
        }
    }
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or_else(|| panic!("{k} not a string in {v}"))
}

fn opt(v: &Value, k: &str) -> Option<String> {
    v[k].as_str().map(String::from)
}

#[test]
fn vector_files_hold() {
    // Keys.
    for k in read("keys.json")["keys"].as_array().unwrap() {
        let p = party(s(k, "name"));
        assert_eq!(s(k, "secretKeyHex"), hex(&p.secret()));
        let pk = p.keys().public_key();
        assert_eq!(s(k, "publicKeyHex"), hex(&pk));
        assert_eq!(b64::decode(s(k, "publicKey")).unwrap(), pk);
        assert_eq!(b64::decode_url(s(k, "relayAddress")).unwrap(), pk);
    }

    // Base64.
    let f = read("base64.json");
    for v in f["samples"].as_array().unwrap() {
        let bytes = hex::decode(s(v, "hex")).unwrap();
        assert_eq!(b64::encode(&bytes), s(v, "std"));
        assert_eq!(b64::encode_url(&bytes), s(v, "url"));
        assert_eq!(b64::decode(s(v, "std")).unwrap(), bytes);
        assert_eq!(b64::decode_url(s(v, "url")).unwrap(), bytes);
    }
    for v in f["invalidStd"].as_array().unwrap() {
        assert!(b64::decode(v.as_str().unwrap()).is_err(), "{v}");
    }
    for v in f["invalidUrl"].as_array().unwrap() {
        assert!(b64::decode_url(v.as_str().unwrap()).is_err(), "{v}");
    }

    // Envelopes.
    let f = read("envelopes.json");
    for v in f["vectors"].as_array().unwrap() {
        let (from, to) = (party(s(v, "sender")), party(s(v, "receiver")));
        let env: PeerEnvelope = serde_json::from_value(v["envelope"].clone()).unwrap();
        assert_eq!((env.from.as_str(), env.to.as_str()), (from.id, to.id));
        let seq = v["seq"].as_u64().unwrap();
        assert_eq!(env.seq, seq);
        assert!(seq < (1 << 53), "a JS safe integer");
        let nonce = hex::decode(s(v, "nonceHex")).unwrap();
        assert_eq!(b64::decode(&env.nonce).unwrap(), nonce);
        assert_eq!(&nonce[..RANDOM_LEN], hex::decode(s(v, "randomHex")).unwrap().as_slice());
        assert_eq!(u64::from(nonce[RANDOM_LEN]), v["dir"].as_u64().unwrap());
        assert_eq!(nonce[RANDOM_LEN], u8::from(from.pk() < to.pk()));
        assert_eq!(nonce[RANDOM_LEN + 1..], seq.to_be_bytes());
        // Opening gives the body; the plaintext is its JSON; sealing the plaintext gives the box.
        let body = open(&to.keys(), &env, &from.pk()).unwrap();
        assert_eq!(serde_json::to_value(&body).unwrap(), v["body"]);
        assert_eq!(serde_json::from_str::<PeerBody>(s(v, "plaintext")).unwrap(), body);
        let ct = from.keys().salsa_box(&to.pk()).encrypt(crypto_box::Nonce::from_slice(&nonce), s(v, "plaintext").as_bytes());
        assert_eq!(b64::encode(&ct.unwrap()), env.ciphertext);
        assert_eq!(env.pk.is_some(), matches!(body, PeerBody::PairRequest { .. }));
        if let Some(pk) = &env.pk {
            assert_eq!(b64::decode(pk).unwrap(), from.pk());
        }
    }
    for v in f["rejects"].as_array().unwrap() {
        let (to, from) = (party(s(v, "receiver")), party(s(v, "sender")));
        let env: PeerEnvelope = serde_json::from_value(v["envelope"].clone()).unwrap();
        let err = open(&to.keys(), &env, &from.pk()).expect_err(s(v, "name"));
        assert_eq!(err.to_string(), s(v, "error"), "{}", s(v, "name"));
    }
    let replay = &f["replay"];
    let env: PeerEnvelope = serde_json::from_value(replay["envelope"].clone()).unwrap();
    assert!(env.seq <= replay["lastRxSeq"].as_u64().unwrap());
    assert!(open(&party(s(replay, "receiver")).keys(), &env, &party(s(replay, "sender")).pk()).is_ok(), "only the seq rule refuses it");

    // Invites.
    let f = read("invites.json");
    for v in f["valid"].as_array().unwrap() {
        let inv = Invite::parse(s(v, "uri")).unwrap_or_else(|e| panic!("{}: {e}", s(v, "name")));
        assert_eq!(inv.machine_id, s(v, "machineId"));
        assert_eq!(b64::encode(&inv.public_key), s(v, "publicKey"));
        assert_eq!(inv.addr, opt(v, "addr"));
        assert_eq!(inv.code, s(v, "code"));
        assert_eq!(inv.expires_at, v["expiresAt"].as_i64().unwrap());
        assert_eq!(inv.relay, opt(v, "relay"));
        assert_eq!(inv.relay_token, opt(v, "relayToken"));
        assert_eq!(inv.relay_key, opt(v, "relayKey"));
        assert_eq!(inv.to_uri(), s(v, "uri"), "canonical form");
    }
    for v in f["invalid"].as_array().unwrap() {
        assert_eq!(Invite::parse(s(v, "uri")), Err(InviteError::BadUri), "{}", s(v, "name"));
    }

    // Names and introduced devices.
    let f = read("names.json");
    assert_eq!(f["maxChars"].as_u64().unwrap() as usize, MAX_INTRO_NAME_CHARS);
    for (list, want) in [("valid", true), ("invalid", false)] {
        for v in f[list].as_array().unwrap() {
            let name = s(v, "name");
            assert_eq!(hex(name.as_bytes()), s(v, "utf8Hex"));
            assert_eq!(device_name_ok(name), want, "{}: {name:?}", s(v, "note"));
        }
    }
    for v in f["introDevices"].as_array().unwrap() {
        let d: IntroDevice = serde_json::from_value(v["device"].clone()).unwrap();
        let got = check_intro_device(&d).map(|_| "ok").unwrap_or_else(|code| code);
        assert_eq!(got, s(v, "result"), "{}", s(v, "note"));
    }

    // Relay auth.
    let f = read("relay_auth.json");
    for v in f["valid"].as_array().unwrap() {
        let device = party(s(v, "device"));
        let ch = &v["challenge"];
        let (nonce, relay_pk, host) = (s(ch, "nonce"), s(ch, "relayPk"), s(v, "host"));
        assert_eq!(b64::decode(relay_pk).unwrap(), RELAY.pk());
        assert_eq!(s(v, "plaintext"), format!("{RELAY_AUTH_CONTEXT}:{nonce}:{host}"));
        assert_eq!(auth_answer(&device.keys(), nonce, relay_pk, host).unwrap(), s(&v["auth"], "box"));
        // What the relay does: open with its secret and the device's key from ?pk=.
        let n: [u8; 24] = b64::decode(nonce).unwrap().try_into().unwrap();
        assert!(n[RANDOM_LEN] >= 2);
        let pt = RELAY
            .keys()
            .salsa_box(&device.pk())
            .decrypt(crypto_box::Nonce::from_slice(&n), b64::decode(s(&v["auth"], "box")).unwrap().as_slice())
            .unwrap();
        assert_eq!(String::from_utf8(pt).unwrap(), s(v, "plaintext"));
        let url = s(v, "connectUrl");
        assert!(url.starts_with(&format!("{}/v1/connect?pk={}&k=", s(v, "relayUrl"), address(&device.pk()))));
    }
    for v in f["refused"].as_array().unwrap() {
        let device = party(s(v, "device"));
        assert_eq!(auth_answer(&device.keys(), s(v, "nonce"), s(v, "relayPk"), s(v, "host")), Err("bad_auth"), "{}", s(v, "name"));
    }
}

#[test]
fn fixed_keys_cover_both_directions() {
    // The vectors need a sender below and one above its receiver.
    assert_ne!(HUB_A.pk() < PHONE.pk(), PHONE.pk() < HUB_A.pk());
    let dirs: Vec<u8> = envelope_cases().iter().map(|(_, f, t, _, _)| u8::from(f.pk() < t.pk())).collect();
    assert!(dirs.contains(&0) && dirs.contains(&1), "{dirs:?}");
}
