//! Relay helpers that need no node state: URL rules, relay addresses, the auth answer, client-side
//! pacing and the TLS provider. The connection itself is `node::relay`.

use crate::envelope::RANDOM_LEN;
use crate::invite::valid_host;
use crate::{b64, DeviceKeys};
use crypto_box::aead::Aead;
use krypto_protocol::RELAY_AUTH_CONTEXT;
use std::fmt;
use std::net::IpAddr;
use std::time::Duration;
use tokio::time::Instant;

/// Longest relay URL accepted.
const MAX_URL_LEN: usize = 512;
/// Longest relay access key accepted.
const MAX_KEY_LEN: usize = 256;
/// A relay address (an X25519 public key, base64url unpadded) is always this long.
const ADDRESS_LEN: usize = 43;
/// Client-side pacing of every frame we send to the relay, below its per-socket limit (50 frames/s,
/// burst 200), so a burst (a mirror resync) is slowed down here instead of dropped there.
const PACE_PER_SEC: f64 = 40.0;
const PACE_BURST: f64 = 150.0;

/// Which relay to use (`PeerNode::set_relay`).
#[derive(Clone, PartialEq, Eq)]
pub struct RelayConfig {
    /// `wss://host[:port][/path]`, or `ws://` to a loopback host (tests, `wrangler dev`).
    pub url: String,
    /// The relay's access key (`k=` on connect, base64url). Secret: never logged. `None` only for a
    /// relay deliberately run open.
    pub access_key: Option<String>,
}

impl fmt::Debug for RelayConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelayConfig")
            .field("url", &self.url)
            .field("access_key", &self.access_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl RelayConfig {
    /// Validated, with the URL normalized (`normalize_url`).
    pub(crate) fn normalized(&self) -> Result<RelayConfig, RelayConfigError> {
        let url = normalize_url(&self.url).ok_or(RelayConfigError::BadUrl)?;
        let access_key = match self.access_key.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(k) if valid_access_key(k) => Some(k.to_string()),
            Some(_) => return Err(RelayConfigError::BadKey),
        };
        Ok(RelayConfig { url, access_key })
    }
}

/// `PeerNode::set_relay` errors. `Display` is the wire error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RelayConfigError {
    /// Not `wss://` / `https://` (or `ws://` / `http://` to a loopback host), or not a plain
    /// `scheme://host[:port][/path]`.
    #[error("bad_url")]
    BadUrl,
    /// Not 1..=256 base64url characters.
    #[error("bad_key")]
    BadKey,
}

/// A relay access key: base64url characters only, so it goes into the connect query as is.
pub(crate) fn valid_access_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= MAX_KEY_LEN && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Normalize a relay URL. `wss://` or `https://` for any host; `ws://` or `http://` only for a loopback
/// host (tests, `wrangler dev`). No user info, query, fragment or percent-escapes. Returns
/// `wss://host[:port][/path]` (or `ws://...`) with the host lowercased and the default port and
/// trailing `/` dropped, so two spellings of one relay compare equal.
pub(crate) fn normalize_url(raw: &str) -> Option<String> {
    let s = raw.trim();
    if s.is_empty() || s.len() > MAX_URL_LEN {
        return None;
    }
    let (scheme, rest) = s.split_once("://")?;
    let (secure, scheme) = match scheme.to_ascii_lowercase().as_str() {
        "wss" | "https" => (true, "wss"),
        "ws" | "http" => (false, "ws"),
        _ => return None,
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let path = path.trim_end_matches('/');
    let path_ok = path.split('/').skip(1).all(|seg| {
        !seg.is_empty()
            && seg != "."
            && seg != ".."
            && seg.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
    });
    if !path_ok {
        return None;
    }
    let (host, port) = split_authority(authority)?;
    let host = host.to_ascii_lowercase();
    if !secure && !loopback_host(&host) {
        return None;
    }
    let default_port = if secure { 443 } else { 80 };
    let port = match port {
        Some(p) if p != default_port => format!(":{p}"),
        _ => String::new(),
    };
    Some(format!("{scheme}://{host}{port}{path}"))
}

/// `host[:port]` with host a DNS name, an IPv4 address or `[IPv6]`.
fn split_authority(a: &str) -> Option<(&str, Option<u16>)> {
    if a.is_empty() || !a.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b':' | b'[' | b']')) {
        return None;
    }
    let (host, port) = if a.starts_with('[') {
        let end = a.find(']')?;
        let (host, rest) = a.split_at(end + 1);
        match rest {
            "" => (host, None),
            r => (host, Some(r.strip_prefix(':')?)),
        }
    } else {
        match a.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (a, None),
        }
    };
    let port = match port {
        None => None,
        Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => match p.parse::<u16>() {
            Ok(n) if n != 0 => Some(n),
            _ => return None,
        },
        Some(_) => return None,
    };
    valid_host(host).then_some((host, port))
}

fn loopback_host(host: &str) -> bool {
    let bare = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    host == "localhost" || bare.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// A device's relay address: its public key, base64url unpadded.
pub(crate) fn address(pk: &[u8; 32]) -> String {
    b64::encode_url(pk)
}

/// Strict inverse of `address` (43 canonical base64url characters).
pub(crate) fn parse_address(s: &str) -> Option<[u8; 32]> {
    if s.len() != ADDRESS_LEN {
        return None;
    }
    b64::decode_url(s).ok()?.try_into().ok()
}

/// The relay's per-connection public key from a challenge (standard base64, 32 bytes).
pub(crate) fn parse_relay_pk(s: &str) -> Option<[u8; 32]> {
    b64::decode(s).ok()?.try_into().ok()
}

/// The `auth` answer to a relay challenge: NaCl box of `{RELAY_AUTH_CONTEXT}:{nonce}:{host}` under the
/// challenge nonce, from our key to the relay's per-connection key, standard base64. `host` is the
/// Host header of our connect request.
///
/// The box uses our long-term key with a relay-chosen key and nonce, so a hostile relay could pick a
/// peer's key and the nonce of an envelope we sent that peer, and learn its keystream. Envelope nonces
/// carry the direction (0/1) at byte 15; a challenge nonce must have >= 2 there, so the two can never
/// meet. `bad_auth` for such a nonce or for our own key as `relay_pk` (the caller also refuses its
/// peers' keys); `bad_frame` for malformed fields.
pub(crate) fn auth_answer(keys: &DeviceKeys, nonce: &str, relay_pk: &str, host: &str) -> Result<String, &'static str> {
    let nonce_bytes: [u8; 24] = b64::decode(nonce).ok().and_then(|b| b.try_into().ok()).ok_or("bad_frame")?;
    let relay_pk = parse_relay_pk(relay_pk).ok_or("bad_frame")?;
    if nonce_bytes[RANDOM_LEN] < 2 || relay_pk == keys.public_key() {
        return Err("bad_auth");
    }
    let text = format!("{RELAY_AUTH_CONTEXT}:{nonce}:{host}");
    let ciphertext = keys
        .salsa_box(&relay_pk)
        .encrypt(crypto_box::Nonce::from_slice(&nonce_bytes), text.as_bytes())
        .map_err(|_| "bad_frame")?;
    Ok(b64::encode(&ciphertext))
}

/// Token bucket for the frames we send to the relay.
pub(crate) struct Pacer {
    tokens: f64,
    last: Instant,
}

impl Pacer {
    pub fn new(now: Instant) -> Self {
        Pacer { tokens: PACE_BURST, last: now }
    }

    fn refill(&mut self, now: Instant) {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + dt * PACE_PER_SEC).min(PACE_BURST);
        self.last = now;
    }

    /// When the next frame may go (`now` if it may go now).
    pub fn ready_at(&mut self, now: Instant) -> Instant {
        self.refill(now);
        if self.tokens >= 1.0 {
            now
        } else {
            now + Duration::from_secs_f64((1.0 - self.tokens) / PACE_PER_SEC)
        }
    }

    /// Spend one token (after `ready_at(now) <= now`).
    pub fn take(&mut self, now: Instant) {
        self.refill(now);
        self.tokens -= 1.0;
    }
}

/// tokio-tungstenite builds its rustls config from the process-wide crypto provider; install
/// aws-lc-rs once (a no-op if the process already installed one).
pub(crate) fn ensure_tls_provider() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_rules() {
        let ok = [
            ("wss://relay.krypto.dev", "wss://relay.krypto.dev"),
            ("https://Relay.Krypto.dev/", "wss://relay.krypto.dev"),
            (" wss://relay.krypto.dev:443 ", "wss://relay.krypto.dev"),
            ("wss://relay.krypto.dev:8443/r/v-1", "wss://relay.krypto.dev:8443/r/v-1"),
            ("WSS://10.0.0.5", "wss://10.0.0.5"),
            ("wss://[2001:db8::1]:9", "wss://[2001:db8::1]:9"),
            ("ws://127.0.0.1:8787", "ws://127.0.0.1:8787"),
            ("http://localhost:8787//", "ws://localhost:8787"),
            ("ws://[::1]:80", "ws://[::1]"),
        ];
        for (raw, want) in ok {
            assert_eq!(normalize_url(raw).as_deref(), Some(want), "{raw}");
        }
        let bad = [
            "",
            "relay.krypto.dev",
            "ftp://relay.krypto.dev",
            "ws://relay.krypto.dev",
            "http://10.0.0.5:8787",
            "wss://user@relay.krypto.dev",
            "wss://relay.krypto.dev/?x=1",
            "wss://relay.krypto.dev/#x",
            "wss://relay.krypto.dev/a%20b",
            "wss://relay.krypto.dev/a//b",
            "wss://relay.krypto.dev/../x",
            "wss://relay.krypto.dev:0",
            "wss://relay.krypto.dev:65536",
            "wss://relay.krypto.dev:",
            "wss://:443",
            "wss://a..b",
            "wss://-a.dev",
            "wss://[::]:443",
            "wss://::1",
            "wss://relay .dev",
        ];
        for raw in bad {
            assert_eq!(normalize_url(raw), None, "{raw}");
        }
        assert_eq!(normalize_url(&format!("wss://{}.dev", "a".repeat(600))), None);
    }

    #[test]
    fn config_rules() {
        let cfg = |url: &str, key: Option<&str>| RelayConfig { url: url.into(), access_key: key.map(String::from) };
        let n = cfg("https://Relay.dev/", Some(" a-B_9 ")).normalized().unwrap();
        assert_eq!(n, cfg("wss://relay.dev", Some("a-B_9")));
        assert_eq!(cfg("wss://relay.dev", Some("")).normalized().unwrap().access_key, None);
        assert_eq!(cfg("wss://relay.dev", None).normalized().unwrap().access_key, None);
        for bad in ["a+b", "a/b", "a=", "a b", "k&x=1"] {
            assert_eq!(cfg("wss://relay.dev", Some(bad)).normalized(), Err(RelayConfigError::BadKey), "{bad}");
        }
        assert_eq!(cfg("wss://relay.dev", Some(&"k".repeat(257))).normalized(), Err(RelayConfigError::BadKey));
        assert_eq!(cfg("ws://relay.dev", Some("k")).normalized(), Err(RelayConfigError::BadUrl));
        let dbg = format!("{:?}", cfg("wss://relay.dev", Some("s3cr3t")));
        assert!(dbg.contains("relay.dev") && !dbg.contains("s3cr3t"), "{dbg}");
        assert_eq!(RelayConfigError::BadUrl.to_string(), "bad_url");
        assert_eq!(RelayConfigError::BadKey.to_string(), "bad_key");
    }

    #[test]
    fn addresses_are_strict() {
        let pk = [0xfbu8; 32];
        let a = address(&pk);
        assert_eq!(a.len(), 43);
        assert!(!a.contains('=') && !a.contains('+') && !a.contains('/'));
        assert_eq!(parse_address(&a), Some(pk));
        assert_eq!(parse_address(&b64::encode(&pk)), None, "standard base64 is not an address");
        assert_eq!(parse_address(&a[..42]), None);
        assert_eq!(parse_address(&format!("{a}A")), None);
    }

    #[test]
    fn auth_answer_opens_on_the_relay() {
        let device = DeviceKeys::generate();
        let relay = DeviceKeys::generate();
        let nonce = b64::encode(&[7u8; 24]);
        let answer = auth_answer(&device, &nonce, &b64::encode(&relay.public_key()), "127.0.0.1:8787").unwrap();
        let ciphertext = b64::decode(&answer).unwrap();
        let plain = relay
            .salsa_box(&device.public_key())
            .decrypt(crypto_box::Nonce::from_slice(&[7u8; 24]), ciphertext.as_slice())
            .unwrap();
        assert_eq!(plain, format!("{RELAY_AUTH_CONTEXT}:{nonce}:127.0.0.1:8787").as_bytes());
        assert_eq!(auth_answer(&device, "short", &b64::encode(&relay.public_key()), "h"), Err("bad_frame"));
        assert_eq!(auth_answer(&device, &nonce, &b64::encode(&[1u8; 31]), "h"), Err("bad_frame"));
    }

    #[test]
    fn auth_refuses_challenges_that_could_reuse_an_envelope_nonce() {
        let device = DeviceKeys::generate();
        let relay = b64::encode(&DeviceKeys::generate().public_key());
        // Byte 15 is the envelope direction byte (0/1): such a nonce is refused, >= 2 is fine.
        for dir in [0u8, 1] {
            let mut n = [9u8; 24];
            n[RANDOM_LEN] = dir;
            assert_eq!(auth_answer(&device, &b64::encode(&n), &relay, "h"), Err("bad_auth"), "dir {dir}");
        }
        for dir in [2u8, 255] {
            let mut n = [9u8; 24];
            n[RANDOM_LEN] = dir;
            assert!(auth_answer(&device, &b64::encode(&n), &relay, "h").is_ok(), "dir {dir}");
        }
        // Our own key as the relay key: refused.
        let own = b64::encode(&device.public_key());
        assert_eq!(auth_answer(&device, &b64::encode(&[9u8; 24]), &own, "h"), Err("bad_auth"));
        // The real envelope nonces sit exactly in the refused domain.
        let peer = DeviceKeys::generate();
        let env = crate::seal(&device, "aaaaaaaaaaaa", "bbbbbbbbbbbb", &peer.public_key(), 5, &krypto_protocol::PeerBody::SyncRequest { since_seq: 0 }, false);
        let n = b64::decode(&env.nonce).unwrap();
        assert!(n[RANDOM_LEN] < 2);
        assert_eq!(auth_answer(&device, &env.nonce, &b64::encode(&peer.public_key()), "h"), Err("bad_auth"));
    }

    #[test]
    fn pacer_allows_a_burst_then_paces() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0);
        for _ in 0..PACE_BURST as usize {
            assert_eq!(p.ready_at(t0), t0);
            p.take(t0);
        }
        let next = p.ready_at(t0);
        assert!(next > t0 && next <= t0 + Duration::from_millis(30), "{:?}", next - t0);
        // One second later 40 more may go.
        let t1 = t0 + Duration::from_secs(1);
        for _ in 0..PACE_PER_SEC as usize {
            assert_eq!(p.ready_at(t1), t1);
            p.take(t1);
        }
        assert!(p.ready_at(t1) > t1);
    }

    #[test]
    fn tls_provider_install_is_idempotent() {
        ensure_tls_provider();
        ensure_tls_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
    }
}
