//! Group roster (`roster_intro`, migration step 7): the structural checks a received introduction must
//! pass. The node applies them (and the sender rules) before it reports an intro; the hub then checks
//! conflicts, revocations and keys it knows, and stores what it accepts.

use crate::b64;
use crate::invite::peer_addr_ok;
use krypto_protocol::{is_valid_machine_id, DeviceKind, IntroDevice, MachineOs};

/// Most devices one `roster_intro` may carry; a bigger intro is dropped whole.
pub const MAX_INTRO_DEVICES: usize = 32;
/// Longest device name in an intro, in `char`s (after trimming).
pub const MAX_INTRO_NAME_CHARS: usize = 64;

/// X25519 points of small order, as libsodium's blocklist lists them (bit 255 cleared; X25519 ignores
/// it). A box to such a key has a shared secret anyone can compute.
const LOW_ORDER: [[u8; 32]; 7] = [
    // 0 (order 4)
    [0; 32],
    // 1 (order 1)
    [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    // order 8
    [
        0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f, 0xc4, 0x6a, 0xda, 0x09,
        0x8d, 0xeb, 0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16, 0x5f, 0x49, 0xb8, 0x00,
    ],
    // order 8
    [
        0x5f, 0x9c, 0x95, 0xbc, 0xa3, 0x50, 0x8c, 0x24, 0xb1, 0xd0, 0xb1, 0x55, 0x9c, 0x83, 0xef, 0x5b, 0x04, 0x44,
        0x5c, 0xc4, 0x58, 0x1c, 0x8e, 0x86, 0xd8, 0x22, 0x4e, 0xdd, 0xd0, 0x9f, 0x11, 0x57,
    ],
    // p - 1 (order 2)
    [
        0xec, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f,
    ],
    // p (= 0, non-canonical)
    [
        0xed, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f,
    ],
    // p + 1 (= 1, non-canonical)
    [
        0xee, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f,
    ],
];

/// True for an X25519 public key of small order (in any encoding X25519 accepts).
pub(crate) fn is_low_order(pk: &[u8; 32]) -> bool {
    let mut k = *pk;
    k[31] &= 0x7f;
    LOW_ORDER.contains(&k)
}

/// A public key as `Device.publicKey` carries it: standard padded base64 of exactly 32 bytes, in its
/// canonical encoding, and not of small order. Returns the key bytes.
pub fn public_key_ok(public_key: &str) -> Option<[u8; 32]> {
    let pk: [u8; 32] = b64::decode(public_key).ok()?.try_into().ok()?;
    (b64::encode(&pk) == public_key && !is_low_order(&pk)).then_some(pk)
}

/// Characters that change how text around them is shown without being visible themselves: bidi
/// marks / embeddings / overrides / isolates (U+061C, U+200E-200F, U+202A-202E, U+2066-2069), zero-width
/// spaces and joiners (U+200B-200D), the word joiner and invisible operators (U+2060-2064), line and
/// paragraph separators (U+2028-2029), the BOM (U+FEFF).
fn invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// A device or machine name a peer may give us (pair_request, pair_accept, hello, presence,
/// roster_intro): 1..=MAX_INTRO_NAME_CHARS Unicode scalar values once trimmed (Unicode White_Space),
/// and nowhere in the untrimmed name a control character (Cc) or one of `invisible_format`'s. Peers
/// refuse frames naming a machine otherwise, so a hub should hold its own name to this rule too.
/// Vectors: `tests/vectors/names.json`.
pub fn device_name_ok(name: &str) -> bool {
    let len = name.trim().chars().count();
    (1..=MAX_INTRO_NAME_CHARS).contains(&len) && !name.chars().any(|c| c.is_control() || invisible_format(c))
}

/// Phones (and only phones) run iOS or Android.
pub(crate) fn phone_os(os: MachineOs) -> bool {
    matches!(os, MachineOs::Ios | MachineOs::Android)
}

/// Structural check of one introduced device. Returns its key bytes, or the reason code:
/// - `bad_id`: not a device id (12 chars `[a-z0-9]`, like machine ids);
/// - `bad_name`: fails `device_name_ok`;
/// - `bad_kind`: a phone without an ios / android os, or a hub with one;
/// - `bad_key`: not canonical standard base64 of 32 bytes, or a key of small order;
/// - `bad_addr`: an addr that `peer_addr_ok` refuses (private / LAN IP literal "ip:port" only), or any
///   addr on a phone (phones are reached over the relay only).
///
/// It does not know the roster: conflicts with known ids or keys and revoked ids or keys are the
/// receiving hub's checks (the node also refuses our own and the sender's id / key and repeats within
/// one intro). Introducers should send only devices that pass it (drop an addr that fails
/// `peer_addr_ok` rather than the device).
pub fn check_intro_device(device: &IntroDevice) -> Result<[u8; 32], &'static str> {
    if !is_valid_machine_id(&device.id) {
        return Err("bad_id");
    }
    if !device_name_ok(&device.name) {
        return Err("bad_name");
    }
    let is_phone = device.kind == DeviceKind::Phone;
    if is_phone != device.os.is_some_and(phone_os) {
        return Err("bad_kind");
    }
    let pk = public_key_ok(&device.public_key).ok_or("bad_key")?;
    if let Some(addr) = &device.addr {
        if device.kind == DeviceKind::Phone || !peer_addr_ok(addr) {
            return Err("bad_addr");
        }
    }
    Ok(pk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeviceKeys;
    use crypto_box::aead::Aead;
    use krypto_protocol::MachineOs;

    fn dev(id: &str, name: &str, pk: &str, addr: Option<&str>) -> IntroDevice {
        IntroDevice {
            id: id.into(),
            kind: DeviceKind::Hub,
            name: name.into(),
            public_key: pk.into(),
            addr: addr.map(Into::into),
            os: Some(MachineOs::Linux),
        }
    }

    /// Do two of `n` random secret keys get the same box key with `point`? (A box sealed with one
    /// opens with the other.)
    fn shared_key_collides(point: &[u8; 32], n: usize) -> bool {
        let nonce = crypto_box::Nonce::from_slice(&[9u8; 24]).to_owned();
        let keys: Vec<DeviceKeys> = (0..n).map(|_| DeviceKeys::generate()).collect();
        let boxes: Vec<Vec<u8>> = keys.iter().map(|k| k.salsa_box(point).encrypt(&nonce, &b"x"[..]).unwrap()).collect();
        keys.iter().enumerate().any(|(i, k)| {
            boxes.iter().skip(i + 1).any(|ct| k.salsa_box(point).decrypt(&nonce, ct.as_slice()).is_ok())
        })
    }

    #[test]
    fn low_order_points_really_have_a_guessable_shared_secret() {
        // With a point of order <= 8 the box key takes at most 8 values whatever the secret key (crypto_box
        // reduces the clamped scalar mod the group order, so it is not always the all-zero secret of
        // RFC 7748): among 9 random secret keys two always share it. That is what makes these keys
        // worthless, and proves the table.
        for p in LOW_ORDER {
            for top in [0u8, 0x80] {
                let mut point = p;
                point[31] |= top;
                assert!(is_low_order(&point));
                assert!(shared_key_collides(&point, 9), "{point:02x?}");
            }
        }
        // An ordinary key is not on the list, and its box keys never collide.
        let k = DeviceKeys::generate().public_key();
        assert!(!is_low_order(&k));
        assert!(!shared_key_collides(&k, 9));
    }

    #[test]
    fn public_key_rules() {
        let pk = DeviceKeys::generate().public_key();
        let b = b64::encode(&pk);
        assert_eq!(public_key_ok(&b), Some(pk));
        assert_eq!(public_key_ok(&b64::encode_url(&pk)), None, "url alphabet / no padding");
        assert_eq!(public_key_ok(&b64::encode(&[1u8; 31])), None);
        assert_eq!(public_key_ok(&b64::encode(&[1u8; 33])), None);
        assert_eq!(public_key_ok(""), None);
        assert_eq!(public_key_ok(&format!(" {b}")), None);
        assert_eq!(public_key_ok(&b64::encode(&[0u8; 32])), None, "low order");
        let mut p = LOW_ORDER[4];
        p[31] |= 0x80;
        assert_eq!(public_key_ok(&b64::encode(&p)), None, "low order with bit 255 set");
    }

    #[test]
    fn intro_device_rules() {
        let pk = b64::encode(&DeviceKeys::generate().public_key());
        let ok = dev("abcdefghij12", "Mac mini", &pk, Some("192.168.1.20:47801"));
        assert!(check_intro_device(&ok).is_ok());
        assert!(check_intro_device(&dev("abcdefghij12", "  Mac mini  ", &pk, None)).is_ok());
        assert!(check_intro_device(&dev("abcdefghij12", &"é".repeat(64), &pk, None)).is_ok());

        assert_eq!(check_intro_device(&dev("ABCDEFGHIJ12", "x", &pk, None)), Err("bad_id"));
        assert_eq!(check_intro_device(&dev("abc", "x", &pk, None)), Err("bad_id"));
        assert_eq!(check_intro_device(&dev("abcdefghij12", "", &pk, None)), Err("bad_name"));
        assert_eq!(check_intro_device(&dev("abcdefghij12", "   ", &pk, None)), Err("bad_name"));
        assert_eq!(check_intro_device(&dev("abcdefghij12", &"a".repeat(65), &pk, None)), Err("bad_name"));
        assert_eq!(check_intro_device(&dev("abcdefghij12", "a\nb", &pk, None)), Err("bad_name"));
        assert_eq!(check_intro_device(&dev("abcdefghij12", "a\u{7}", &pk, None)), Err("bad_name"));
        for c in ['\u{200B}', '\u{200D}', '\u{200F}', '\u{202A}', '\u{202E}', '\u{2066}', '\u{2069}', '\u{FEFF}'] {
            assert_eq!(check_intro_device(&dev("abcdefghij12", &format!("Mac{c}mini"), &pk, None)), Err("bad_name"), "{c:?}");
        }
        assert!(check_intro_device(&dev("abcdefghij12", "Sam\u{2019}s Mac \u{1F600}", &pk, None)).is_ok(), "visible Unicode is fine");
        // Kind and os agree: phones run ios / android, hubs do not.
        let hub_on_ios = IntroDevice { os: Some(MachineOs::Ios), ..dev("abcdefghij12", "x", &pk, None) };
        assert_eq!(check_intro_device(&hub_on_ios), Err("bad_kind"));
        let hub_on_android = IntroDevice { os: Some(MachineOs::Android), ..hub_on_ios.clone() };
        assert_eq!(check_intro_device(&hub_on_android), Err("bad_kind"));
        assert!(check_intro_device(&IntroDevice { os: None, ..hub_on_ios.clone() }).is_ok(), "a hub's os may be unknown");
        let phone_no_os = IntroDevice { kind: DeviceKind::Phone, os: None, ..hub_on_ios.clone() };
        assert_eq!(check_intro_device(&phone_no_os), Err("bad_kind"));
        let phone_on_linux = IntroDevice { os: Some(MachineOs::Linux), ..phone_no_os.clone() };
        assert_eq!(check_intro_device(&phone_on_linux), Err("bad_kind"));
        assert!(check_intro_device(&IntroDevice { os: Some(MachineOs::Android), ..phone_no_os }).is_ok());
        assert_eq!(check_intro_device(&dev("abcdefghij12", "x", "nope", None)), Err("bad_key"));
        assert_eq!(check_intro_device(&dev("abcdefghij12", "x", &b64::encode(&LOW_ORDER[2]), None)), Err("bad_key"));
        assert_eq!(check_intro_device(&dev("abcdefghij12", "x", &pk, Some("8.8.8.8:47801"))), Err("bad_addr"));
        assert_eq!(check_intro_device(&dev("abcdefghij12", "x", &pk, Some("host.lan:47801"))), Err("bad_addr"));
        let phone = IntroDevice { kind: DeviceKind::Phone, os: Some(MachineOs::Ios), ..ok.clone() };
        assert_eq!(check_intro_device(&phone), Err("bad_addr"), "phones have no addr");
        assert!(check_intro_device(&IntroDevice { addr: None, ..phone }).is_ok());
    }
}
