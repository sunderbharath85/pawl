//! Sealing and opening `PeerEnvelope`s (NaCl `crypto_box`, XSalsa20-Poly1305).
//!
//! The box has no associated data, so the plaintext envelope fields are bound through the 24-byte
//! nonce, which is an authenticated input of the box:
//!
//! ```text
//! nonce = 15 random bytes || dir (1 byte) || seq (u64 big-endian)
//! dir   = 1 if sender_pk < receiver_pk (bytewise) else 0
//! ```
//!
//! `open` rejects an envelope whose `seq` differs from the one in its nonce (so a replayed box cannot
//! be given a fresh `seq`) or whose direction byte is wrong (a NaCl box is valid in both directions
//! of a key pair, so without it a frame could be reflected back to its sender). `from` and `to` are
//! bound by the keys themselves: the receiver opens with its own secret and the roster key of `from`.

use crate::{b64, keys::DeviceKeys};
use crypto_box::aead::Aead;
use krypto_protocol::{PeerBody, PeerEnvelope, PEER_PROTOCOL_VERSION};
use std::fmt;

/// Why an envelope did not open. Deliberately coarse: none of these is ever sent to the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenError {
    /// `v` is not `PEER_PROTOCOL_VERSION`.
    Version,
    /// `nonce`/`box` are not base64 of the right size.
    Encoding,
    /// The nonce does not carry this envelope's `seq` and direction.
    Nonce,
    /// Authentication failed: wrong key, or the box was tampered with.
    Decrypt,
    /// The plaintext is not a valid `PeerBody`.
    Body,
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            OpenError::Version => "bad_version",
            OpenError::Encoding => "bad_encoding",
            OpenError::Nonce => "bad_nonce",
            OpenError::Decrypt => "decrypt_failed",
            OpenError::Body => "bad_body",
        })
    }
}

impl std::error::Error for OpenError {}

const NONCE_LEN: usize = 24;
/// Index of the direction byte (after the random prefix). The relay auth nonce domain uses it too.
pub(crate) const RANDOM_LEN: usize = 15;
/// Poly1305 tag.
const TAG_LEN: usize = 16;

fn direction(sender_pk: &[u8; 32], receiver_pk: &[u8; 32]) -> u8 {
    u8::from(sender_pk < receiver_pk)
}

fn make_nonce(sender_pk: &[u8; 32], receiver_pk: &[u8; 32], seq: u64) -> [u8; NONCE_LEN] {
    let mut n = [0u8; NONCE_LEN];
    getrandom::fill(&mut n[..RANDOM_LEN]).expect("OS random number generator unavailable");
    n[RANDOM_LEN] = direction(sender_pk, receiver_pk);
    n[RANDOM_LEN + 1..].copy_from_slice(&seq.to_be_bytes());
    n
}

/// Seal `body` from this device (`from`) to `to` (public key `to_pk`). `include_pk` adds our public
/// key to the envelope; only a `pair_request` carries it.
pub fn seal(
    keys: &DeviceKeys,
    from: &str,
    to: &str,
    to_pk: &[u8; 32],
    seq: u64,
    body: &PeerBody,
    include_pk: bool,
) -> PeerEnvelope {
    let plaintext = serde_json::to_vec(body).expect("PeerBody serializes");
    seal_plaintext(keys, from, to, to_pk, seq, &plaintext, include_pk)
}

/// `seal` for a body that is already serialized (UTF-8 JSON of a `PeerBody`).
pub(crate) fn seal_plaintext(
    keys: &DeviceKeys,
    from: &str,
    to: &str,
    to_pk: &[u8; 32],
    seq: u64,
    plaintext: &[u8],
    include_pk: bool,
) -> PeerEnvelope {
    let nonce = make_nonce(&keys.public_key(), to_pk, seq);
    let ciphertext = keys
        .salsa_box(to_pk)
        .encrypt(crypto_box::Nonce::from_slice(&nonce), plaintext)
        .expect("crypto_box encryption cannot fail for in-memory buffers");
    PeerEnvelope {
        v: PEER_PROTOCOL_VERSION,
        from: from.to_string(),
        to: to.to_string(),
        seq,
        nonce: b64::encode(&nonce),
        ciphertext: b64::encode(&ciphertext),
        pk: include_pk.then(|| keys.public_b64()),
    }
}

/// Open an envelope sent to this device by the holder of `sender_pk`. Checks the version and the
/// nonce binding, then authenticates and decrypts. The caller checks `from`/`to`/`seq` policy.
pub fn open(keys: &DeviceKeys, env: &PeerEnvelope, sender_pk: &[u8; 32]) -> Result<PeerBody, OpenError> {
    if env.v != PEER_PROTOCOL_VERSION {
        return Err(OpenError::Version);
    }
    let nonce = b64::decode(&env.nonce).map_err(|_| OpenError::Encoding)?;
    if nonce.len() != NONCE_LEN {
        return Err(OpenError::Encoding);
    }
    let ciphertext = b64::decode(&env.ciphertext).map_err(|_| OpenError::Encoding)?;
    if ciphertext.len() < TAG_LEN {
        return Err(OpenError::Encoding);
    }
    if nonce[RANDOM_LEN] != direction(sender_pk, &keys.public_key())
        || nonce[RANDOM_LEN + 1..] != env.seq.to_be_bytes()
    {
        return Err(OpenError::Nonce);
    }
    let plaintext = keys
        .salsa_box(sender_pk)
        .decrypt(crypto_box::Nonce::from_slice(&nonce), ciphertext.as_slice())
        .map_err(|_| OpenError::Decrypt)?;
    serde_json::from_slice(&plaintext).map_err(|_| OpenError::Body)
}

/// Decode a base64 (standard) public key.
pub(crate) fn parse_pk(s: &str) -> Option<[u8; 32]> {
    b64::decode(s).ok()?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use krypto_protocol::ByeReason;

    fn body() -> PeerBody {
        PeerBody::Bye { reason: ByeReason::Shutdown, ts: 42 }
    }

    #[test]
    fn seal_open_round_trip() {
        let (a, b) = (DeviceKeys::generate(), DeviceKeys::generate());
        let env = seal(&a, "aaaaaaaaaaaa", "bbbbbbbbbbbb", &b.public_key(), 7, &body(), false);
        assert_eq!(env.v, PEER_PROTOCOL_VERSION);
        assert_eq!(env.seq, 7);
        assert!(env.pk.is_none());
        assert_eq!(open(&b, &env, &a.public_key()).unwrap(), body());
        let env = seal(&a, "aaaaaaaaaaaa", "bbbbbbbbbbbb", &b.public_key(), 8, &body(), true);
        assert_eq!(env.pk.as_deref(), Some(a.public_b64().as_str()));
        // Random part of the nonce differs per frame.
        let env2 = seal(&a, "aaaaaaaaaaaa", "bbbbbbbbbbbb", &b.public_key(), 8, &body(), true);
        assert_ne!(env.nonce, env2.nonce);
    }

    #[test]
    fn wrong_key_fails() {
        let (a, b, c) = (DeviceKeys::generate(), DeviceKeys::generate(), DeviceKeys::generate());
        let env = seal(&a, "a", "b", &b.public_key(), 1, &body(), false);
        // Receiver opens with the wrong sender key.
        // (The direction byte may already disagree, depending on how the random keys compare.)
        assert!(matches!(open(&b, &env, &c.public_key()), Err(OpenError::Decrypt | OpenError::Nonce)));
        // A third party cannot open it at all.
        assert!(open(&c, &env, &a.public_key()).is_err());
    }

    #[test]
    fn tamper_and_rebinding_fail() {
        let (a, b) = (DeviceKeys::generate(), DeviceKeys::generate());
        let env = seal(&a, "a", "b", &b.public_key(), 5, &body(), false);

        let mut bumped = env.clone();
        bumped.seq = 6;
        assert_eq!(open(&b, &bumped, &a.public_key()), Err(OpenError::Nonce));

        let mut ct = b64::decode(&env.ciphertext).unwrap();
        ct[3] ^= 1;
        let mut tampered = env.clone();
        tampered.ciphertext = b64::encode(&ct);
        assert_eq!(open(&b, &tampered, &a.public_key()), Err(OpenError::Decrypt));

        let mut n = b64::decode(&env.nonce).unwrap();
        n[0] ^= 1;
        let mut renonced = env.clone();
        renonced.nonce = b64::encode(&n);
        assert_eq!(open(&b, &renonced, &a.public_key()), Err(OpenError::Decrypt));

        let mut v2 = env.clone();
        v2.v = 2;
        assert_eq!(open(&b, &v2, &a.public_key()), Err(OpenError::Version));

        let mut short = env.clone();
        short.nonce = b64::encode(&[0u8; 23]);
        assert_eq!(open(&b, &short, &a.public_key()), Err(OpenError::Encoding));
    }

    #[test]
    fn reflected_frame_fails() {
        // The X25519 shared key is the same in both directions; the direction byte stops a frame a
        // sent to b from being replayed to a as if b had sent it.
        let (a, b) = (DeviceKeys::generate(), DeviceKeys::generate());
        let env = seal(&a, "a", "b", &b.public_key(), 9, &body(), false);
        let mut reflected = env.clone();
        reflected.from = "b".into();
        reflected.to = "a".into();
        assert_eq!(open(&a, &reflected, &b.public_key()), Err(OpenError::Nonce));
    }

    #[test]
    fn interop_with_plain_nacl_box() {
        // Independently encrypt with SalsaBox and our nonce layout: open must accept it, i.e. the
        // wire format is a plain NaCl box (tweetnacl `nacl.box` compatible).
        let a_secret = [7u8; 32];
        let a = DeviceKeys::from_secret_bytes(a_secret);
        let b = DeviceKeys::generate();
        let mut nonce = [0x55u8; 24];
        nonce[15] = direction(&a.public_key(), &b.public_key());
        nonce[16..].copy_from_slice(&3u64.to_be_bytes());
        let pt = serde_json::to_vec(&body()).unwrap();
        let sb = crypto_box::SalsaBox::new(
            &crypto_box::PublicKey::from(b.public_key()),
            &crypto_box::SecretKey::from(a_secret),
        );
        let ct = sb.encrypt(crypto_box::Nonce::from_slice(&nonce), pt.as_slice()).unwrap();
        assert_eq!(ct.len(), pt.len() + TAG_LEN);
        let env = PeerEnvelope {
            v: PEER_PROTOCOL_VERSION,
            from: "a".into(),
            to: "b".into(),
            seq: 3,
            nonce: b64::encode(&nonce),
            ciphertext: b64::encode(&ct),
            pk: None,
        };
        assert_eq!(open(&b, &env, &a.public_key()).unwrap(), body());
    }
}
