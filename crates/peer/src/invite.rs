//! One-time pairing invites:
//! `pawl://peer?v=1&mid=<machineId>&pk=<b64url pk>&addr=<host:port>&code=<b64url 16 bytes>&exp=<unix ms>`
//! plus `&relay=<percent-encoded relay URL>&rt=<b64url 16 bytes>[&rk=<relay access key>]` when the
//! inviter is on a relay. `addr` may be left out when `relay` is present (an inviter off the LAN has
//! nothing to dial).

use crate::b64;
use crate::relay::{normalize_url, valid_access_key};
use pawl_protocol::{is_valid_machine_id, MachineId, UnixMs};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

const PREFIX: &str = "pawl://peer?";
const MAX_URI_LEN: usize = 1024;
pub(crate) const CODE_LEN: usize = 16;

#[derive(Clone, PartialEq, Eq)]
pub struct Invite {
    pub machine_id: MachineId,
    pub public_key: [u8; 32],
    /// "host:port" of the inviter's peer listener (IPv6 as "[addr]:port"). `None` only when `relay` is set.
    pub addr: Option<String>,
    /// Base64url (no padding) of 16 random bytes. Secret: never log it (nor the URI).
    pub code: String,
    pub expires_at: UnixMs,
    /// Normalized URL of the relay the inviter is connected to (see `PeerNode::set_relay`).
    pub relay: Option<String>,
    /// The inviter's one-shot relay pairing slot: base64url (no padding) of 16 random bytes. Set iff
    /// `relay` is. Never log it.
    pub relay_token: Option<String>,
    /// The relay's access key, so a joiner without a relay can pair through it. Only with `relay`.
    /// Secret: never log it.
    pub relay_key: Option<String>,
}

/// Errors from parsing an invite URI or creating an invite. `Display` is the wire error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InviteError {
    #[error("bad_uri")]
    BadUri,
    /// Neither the peer listener nor the relay is on: nobody could reach us.
    #[error("peer_listener_off")]
    ListenerOff,
    #[error("invalid")]
    Invalid,
    /// A relay-only invite needs a connected relay.
    #[error("relay_off")]
    RelayOff,
}

impl fmt::Debug for Invite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Invite")
            .field("machine_id", &self.machine_id)
            .field("public_key", &b64::encode(&self.public_key))
            .field("addr", &self.addr)
            .field("code", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("relay", &self.relay)
            .field("relay_token", &self.relay_token.as_ref().map(|_| "<redacted>"))
            .field("relay_key", &self.relay_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl Invite {
    pub fn to_uri(&self) -> String {
        let mut uri = format!("{PREFIX}v=1&mid={}&pk={}", self.machine_id, b64::encode_url(&self.public_key));
        if let Some(addr) = &self.addr {
            uri.push_str(&format!("&addr={}", escape(addr, true)));
        }
        uri.push_str(&format!("&code={}&exp={}", self.code, self.expires_at));
        if let (Some(relay), Some(token)) = (&self.relay, &self.relay_token) {
            uri.push_str(&format!("&relay={}&rt={token}", escape(relay, false)));
            if let Some(key) = &self.relay_key {
                uri.push_str(&format!("&rk={key}"));
            }
        }
        uri
    }

    /// Strict parse. Surrounding whitespace is ignored (pasted URIs); unknown parameters are
    /// ignored; duplicates, a missing parameter or any invalid value is `BadUri`. `addr` may be
    /// missing only when `relay` is present, `relay` and `rt` come together, and `rk` needs `relay`. Expiry is not
    /// checked here: the inviter decides.
    pub fn parse(uri: &str) -> Result<Invite, InviteError> {
        let uri = uri.trim();
        if uri.len() > MAX_URI_LEN || uri.len() < PREFIX.len() {
            return Err(InviteError::BadUri);
        }
        let (scheme, query) = uri.split_at(PREFIX.len());
        if !scheme.eq_ignore_ascii_case(PREFIX) || query.contains('#') {
            return Err(InviteError::BadUri);
        }
        let mut fields: [Option<String>; 9] = Default::default();
        const KEYS: [&str; 9] = ["v", "mid", "pk", "addr", "code", "exp", "relay", "rt", "rk"];
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').ok_or(InviteError::BadUri)?;
            let Some(i) = KEYS.iter().position(|key| *key == k) else { continue };
            if fields[i].is_some() {
                return Err(InviteError::BadUri);
            }
            fields[i] = Some(percent_decode(v).ok_or(InviteError::BadUri)?);
        }
        let [Some(v), Some(mid), Some(pk), addr, Some(code), Some(exp), relay, rt, rk] = fields else {
            return Err(InviteError::BadUri);
        };
        if v != "1" || !is_valid_machine_id(&mid) || addr.as_deref().is_some_and(|a| !valid_addr(a)) {
            return Err(InviteError::BadUri);
        }
        if rk.as_deref().is_some_and(|k| relay.is_none() || !valid_access_key(k)) {
            return Err(InviteError::BadUri);
        }
        let (relay, relay_token) = match (relay, rt) {
            (None, None) => (None, None),
            (Some(relay), Some(rt)) => {
                let relay = normalize_url(&relay).ok_or(InviteError::BadUri)?;
                match b64::decode_url(&rt) {
                    Ok(t) if t.len() == CODE_LEN => {}
                    _ => return Err(InviteError::BadUri),
                }
                (Some(relay), Some(rt))
            }
            _ => return Err(InviteError::BadUri),
        };
        if addr.is_none() && relay.is_none() {
            return Err(InviteError::BadUri);
        }
        let public_key: [u8; 32] = b64::decode_url(&pk)
            .ok()
            .and_then(|b| b.try_into().ok())
            .filter(|pk| !crate::intro::is_low_order(pk))
            .ok_or(InviteError::BadUri)?;
        match b64::decode_url(&code) {
            Ok(c) if c.len() == CODE_LEN => {}
            _ => return Err(InviteError::BadUri),
        }
        let expires_at: UnixMs = exp.parse().map_err(|_| InviteError::BadUri)?;
        if expires_at <= 0 || !exp.bytes().all(|b| b.is_ascii_digit()) {
            return Err(InviteError::BadUri);
        }
        Ok(Invite { machine_id: mid, public_key, addr, code, expires_at, relay, relay_token, relay_key: rk })
    }
}

/// Fresh random invite code (or relay pairing token): raw bytes (kept in memory by the inviter) and
/// the base64url string.
pub(crate) fn new_code() -> ([u8; CODE_LEN], String) {
    let mut c = [0u8; CODE_LEN];
    getrandom::fill(&mut c).expect("OS random number generator unavailable");
    let s = b64::encode_url(&c);
    (c, s)
}

/// "host:port" with a non-zero port; host as in `valid_host`.
pub(crate) fn valid_addr(s: &str) -> bool {
    if s.is_empty() || s.len() > 262 {
        return false;
    }
    let (host, port) = if s.starts_with('[') {
        let Some(end) = s.find("]:") else { return false };
        (&s[..=end], &s[end + 2..])
    } else {
        let Some((h, p)) = s.rsplit_once(':') else { return false };
        (h, p)
    };
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    match port.parse::<u16>() {
        Ok(p) if p != 0 => {}
        _ => return false,
    }
    valid_host(host)
}

/// A host we could dial: an IPv4 address, a DNS name, or "[IPv6]" (bare IPv6 is ambiguous next to a
/// port and is refused). Unspecified addresses (0.0.0.0, ::) are rejected: nobody can dial them.
pub(crate) fn valid_host(host: &str) -> bool {
    if let Some(inner) = host.strip_prefix('[') {
        return inner.strip_suffix(']').and_then(|h| h.parse::<Ipv6Addr>().ok()).is_some_and(|ip| !ip.is_unspecified());
    }
    if host.contains(':') {
        return false;
    }
    if let Ok(ip) = host.parse::<Ipv4Addr>() {
        return !ip.is_unspecified();
    }
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// May we store (and later dial) an addr a *peer* reported in hello / pair_request / pair_accept?
/// Only IP literals in private (10/8, 172.16/12, 192.168/16), Tailscale CGNAT (100.64/10),
/// link-local (169.254/16, fe80::/10), loopback and IPv6 ULA (fc00::/7) ranges, so a paired peer cannot
/// point our dialer at arbitrary hosts. The addr in an invite URI (typed by the user) is not restricted.
pub fn peer_addr_ok(s: &str) -> bool {
    if !valid_addr(s) {
        return false;
    }
    let Ok(sa) = s.parse::<SocketAddr>() else { return false };
    match sa.ip() {
        IpAddr::V4(v4) => local_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => local_v4(v4),
            None => {
                let first = v6.segments()[0];
                v6.is_loopback() || (first & 0xffc0) == 0xfe80 || (first & 0xfe00) == 0xfc00
            }
        },
    }
}

fn local_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_private() || ip.is_loopback() || ip.is_link_local() || (a == 100 && (b & 0xc0) == 64)
}

/// Percent-encode everything but RFC 3986 unreserved characters (and ':' when `keep_colon`, which
/// keeps "host:port" readable).
fn escape(s: &str, keep_colon: bool) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' => out.push(c),
            ':' if keep_colon => out.push(c),
            _ => {
                let mut buf = [0u8; 4];
                for b in c.encode_utf8(&mut buf).bytes() {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
        }
    }
    out
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                // Exactly two ASCII hex digits (`from_str_radix` alone would accept "+f").
                let hex = b.get(i + 1..i + 3)?;
                if !hex.iter().all(u8::is_ascii_hexdigit) {
                    return None;
                }
                out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
                i += 3;
            }
            b'+' => return None,
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(addr: &str) -> Invite {
        let (_, code) = new_code();
        Invite {
            machine_id: "abcdef012345".into(),
            public_key: [9u8; 32],
            addr: Some(addr.into()),
            code,
            expires_at: 1_700_000_000_000,
            relay: None,
            relay_token: None,
            relay_key: None,
        }
    }

    fn with_relay(addr: Option<&str>) -> Invite {
        Invite {
            addr: addr.map(String::from),
            relay: Some("wss://relay.pawl.dev/r".into()),
            relay_token: Some(new_code().1),
            relay_key: Some("k3y_-AZ".into()),
            ..sample("10.0.0.2:47801")
        }
    }

    #[test]
    fn uri_round_trip() {
        for addr in ["192.168.1.5:47801", "mac-mini.local:47801", "[fe80::1]:47801", "[::1]:9"] {
            let inv = sample(addr);
            let uri = inv.to_uri();
            assert!(uri.starts_with("pawl://peer?v=1&mid=abcdef012345&pk="), "{uri}");
            assert!(!uri.contains('['));
            assert!(!uri.contains("relay=") && !uri.contains("rt="));
            assert_eq!(Invite::parse(&uri).unwrap(), inv);
            assert_eq!(Invite::parse(&format!("  {uri}\n")).unwrap(), inv);
        }
    }

    #[test]
    fn relay_params_round_trip() {
        let open_relay = Invite { relay_key: None, ..with_relay(None) };
        for inv in [with_relay(Some("10.0.0.2:47801")), with_relay(None), open_relay] {
            let uri = inv.to_uri();
            assert!(uri.contains("&relay=wss%3A%2F%2Frelay.pawl.dev%2Fr&rt="), "{uri}");
            assert_eq!(uri.contains("&rk=k3y_-AZ"), inv.relay_key.is_some());
            assert_eq!(uri.contains("addr="), inv.addr.is_some());
            assert_eq!(Invite::parse(&uri).unwrap(), inv);
        }
        // The relay URL is normalized on parse.
        let inv = with_relay(None);
        let uri = inv.to_uri().replace("wss%3A%2F%2Frelay.pawl.dev%2Fr", "https%3A%2F%2FRELAY.pawl.dev%3A443%2Fr%2F");
        assert_eq!(Invite::parse(&uri).unwrap().relay.as_deref(), Some("wss://relay.pawl.dev/r"));
    }

    #[test]
    fn relay_params_are_strict() {
        let good = with_relay(None).to_uri();
        let rt = with_relay(None).relay_token.unwrap();
        let no_rt = good.split("&rt=").next().unwrap().to_string();
        let bad = [
            // relay without rt, rt without relay, and no way to reach the inviter at all
            no_rt.clone(),
            sample("10.0.0.2:47801").to_uri() + &format!("&rt={rt}"),
            sample("10.0.0.2:47801").to_uri().replace("&addr=10.0.0.2:47801", ""),
            good.replace("wss%3A%2F%2Frelay.pawl.dev%2Fr", "ws%3A%2F%2Frelay.pawl.dev"),
            good.replace("wss%3A%2F%2Frelay.pawl.dev%2Fr", "relay.pawl.dev"),
            good.replace("wss%3A%2F%2Frelay.pawl.dev%2Fr", "wss%3A%2F%2Frelay.pawl.dev%3Fx%3D1"),
            format!("{no_rt}&rt=AAAA"),
            format!("{no_rt}&rt={rt}A"),
            format!("{good}&rt={rt}"),
            format!("{good}&relay=wss%3A%2F%2Fother.dev"),
            // rk without relay, a bad key, a duplicate key
            sample("10.0.0.2:47801").to_uri() + "&rk=k3y",
            good.replace("&rk=k3y_-AZ", "&rk=k%2By"),
            format!("{good}&rk=k3y"),
        ];
        for b in bad {
            assert_eq!(Invite::parse(&b), Err(InviteError::BadUri), "{b}");
        }
    }

    #[test]
    fn parse_ignores_unknown_and_order() {
        let inv = sample("10.0.0.2:47801");
        let uri = format!(
            "pawl://peer?exp={}&code={}&x=y&addr=10.0.0.2:47801&pk={}&mid=abcdef012345&v=1",
            inv.expires_at,
            inv.code,
            b64::encode_url(&inv.public_key)
        );
        assert_eq!(Invite::parse(&uri).unwrap(), inv);
    }

    #[test]
    fn parse_rejects_bad() {
        let good = sample("10.0.0.2:47801").to_uri();
        let bad = [
            "".to_string(),
            "garbage".to_string(),
            good.replace("pawl://peer?", "http://peer?"),
            good.replace("v=1", "v=2"),
            good.replace("mid=abcdef012345", "mid=ABCDEF012345"),
            good.replace("10.0.0.2:47801", "10.0.0.2"),
            good.replace("10.0.0.2:47801", "10.0.0.2:0"),
            good.replace("10.0.0.2:47801", "0.0.0.0:47801"),
            good.replace("10.0.0.2:47801", "%5B%3A%3A%5D:47801"),
            good.replace("10.0.0.2:47801", "ev il:1"),
            good.replace("&exp=", "&exp=-"),
            good.replace("&exp=", "&exp=x"),
            format!("{good}&v=1"),
            format!("{good}#frag"),
            good.replace("&pk=", "&pk=AAAA"),
            // A key of small order (0).
            good.replace(&format!("pk={}", b64::encode_url(&[9u8; 32])), &format!("pk={}", b64::encode_url(&[0u8; 32]))),
            good.split("&code=").next().unwrap().to_string() + "&code=AAAA&exp=1",
            format!("{}{}", good, "x".repeat(MAX_URI_LEN)),
        ];
        for b in bad {
            assert_eq!(Invite::parse(&b), Err(InviteError::BadUri), "{b}");
        }
    }

    #[test]
    fn debug_redacts_secrets() {
        let inv = with_relay(Some("10.0.0.2:47801"));
        let dbg = format!("{inv:?}");
        assert!(!dbg.contains(&inv.code));
        assert!(!dbg.contains(inv.relay_token.as_deref().unwrap()));
        assert!(!dbg.contains("k3y_-AZ"));
        assert!(dbg.contains("relay.pawl.dev"));
    }

    #[test]
    fn error_codes() {
        assert_eq!(InviteError::BadUri.to_string(), "bad_uri");
        assert_eq!(InviteError::ListenerOff.to_string(), "peer_listener_off");
        assert_eq!(InviteError::Invalid.to_string(), "invalid");
    }

    #[test]
    fn percent_decode_needs_two_hex_digits() {
        assert_eq!(percent_decode("%41%3a").as_deref(), Some("A:"));
        assert_eq!(percent_decode("%+f"), None);
        assert_eq!(percent_decode("%-1"), None);
        assert_eq!(percent_decode("% f"), None);
        assert_eq!(percent_decode("%4"), None);
        assert_eq!(percent_decode("%"), None);
        let good = sample("10.0.0.2:47801").to_uri();
        assert_eq!(Invite::parse(&good.replace("10.0.0.2:47801", "10.0.0.2%+f:47801")), Err(InviteError::BadUri));
        assert_eq!(
            Invite::parse(&good.replace("10.0.0.2:47801", "10.0.0.2%3A47801")).unwrap().addr.as_deref(),
            Some("10.0.0.2:47801")
        );
    }

    #[test]
    fn peer_reported_addr_allowlist() {
        for ok in [
            "10.1.2.3:47801",
            "172.16.0.1:1",
            "172.31.255.255:1",
            "192.168.1.5:47801",
            "100.64.0.1:47801",
            "100.127.255.255:47801",
            "169.254.10.10:47801",
            "127.0.0.1:47801",
            "[::1]:47801",
            "[fe80::1]:47801",
            "[fd7a:115c:a1e0::1]:47801",
            "[::ffff:192.168.1.5]:47801",
        ] {
            assert!(peer_addr_ok(ok), "{ok}");
        }
        for bad in [
            "8.8.8.8:53",
            "172.32.0.1:1",
            "100.128.0.1:1",
            "100.63.255.255:1",
            "1.1.1.1:443",
            "[2001:db8::1]:47801",
            "[::ffff:8.8.8.8]:53",
            "localhost:47801",
            "mac-mini.local:47801",
            "example.com:443",
            "0.0.0.0:47801",
            "[::]:47801",
            "10.0.0.1:0",
            "10.0.0.1",
            "",
        ] {
            assert!(!peer_addr_ok(bad), "{bad}");
        }
    }

    #[test]
    fn addr_rules() {
        assert!(valid_addr("127.0.0.1:47801"));
        assert!(valid_addr("host:1"));
        assert!(valid_addr("[2001:db8::1]:65535"));
        assert!(!valid_addr("[::]:47801"));
        assert!(!valid_addr("::1:47801"));
        assert!(!valid_addr("host:65536"));
        assert!(!valid_addr("host:+1"));
        assert!(!valid_addr(":47801"));
        assert!(!valid_addr("a..b:1"));
        assert!(!valid_addr("host/x:1"));
        assert!(!valid_addr("[::1]47801"));
        assert!(valid_host("[::1]") && valid_host("relay.pawl.dev") && valid_host("10.0.0.1"));
        assert!(!valid_host("::1") && !valid_host("[::]") && !valid_host("0.0.0.0") && !valid_host("[::1"));
    }
}
