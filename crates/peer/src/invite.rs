//! One-time pairing invites:
//! `krypto://peer?v=1&mid=<machineId>&pk=<b64url pk>&addr=<host:port>&code=<b64url 16 bytes>&exp=<unix ms>`.

use crate::b64;
use krypto_protocol::{is_valid_machine_id, MachineId, UnixMs};
use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

const PREFIX: &str = "krypto://peer?";
const MAX_URI_LEN: usize = 1024;
pub(crate) const CODE_LEN: usize = 16;

#[derive(Clone, PartialEq, Eq)]
pub struct Invite {
    pub machine_id: MachineId,
    pub public_key: [u8; 32],
    /// "host:port" of the inviter's peer listener (IPv6 as "[addr]:port").
    pub addr: String,
    /// Base64url (no padding) of 16 random bytes. Secret: never log it (nor the URI).
    pub code: String,
    pub expires_at: UnixMs,
}

/// Errors from parsing an invite URI or creating an invite. `Display` is the wire error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InviteError {
    #[error("bad_uri")]
    BadUri,
    #[error("peer_listener_off")]
    ListenerOff,
    #[error("invalid")]
    Invalid,
}

impl fmt::Debug for Invite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Invite")
            .field("machine_id", &self.machine_id)
            .field("public_key", &b64::encode(&self.public_key))
            .field("addr", &self.addr)
            .field("code", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl Invite {
    pub fn to_uri(&self) -> String {
        format!(
            "{PREFIX}v=1&mid={}&pk={}&addr={}&code={}&exp={}",
            self.machine_id,
            b64::encode_url(&self.public_key),
            escape_addr(&self.addr),
            self.code,
            self.expires_at
        )
    }

    /// Strict parse. Surrounding whitespace is ignored (pasted URIs); unknown parameters are
    /// ignored; duplicates, a missing parameter or any invalid value is `BadUri`. Expiry is not
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
        let mut fields: [Option<String>; 6] = Default::default();
        const KEYS: [&str; 6] = ["v", "mid", "pk", "addr", "code", "exp"];
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').ok_or(InviteError::BadUri)?;
            let Some(i) = KEYS.iter().position(|key| *key == k) else { continue };
            if fields[i].is_some() {
                return Err(InviteError::BadUri);
            }
            fields[i] = Some(percent_decode(v).ok_or(InviteError::BadUri)?);
        }
        let [Some(v), Some(mid), Some(pk), Some(addr), Some(code), Some(exp)] = fields else {
            return Err(InviteError::BadUri);
        };
        if v != "1" || !is_valid_machine_id(&mid) || !valid_addr(&addr) {
            return Err(InviteError::BadUri);
        }
        let public_key: [u8; 32] = b64::decode_url(&pk)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or(InviteError::BadUri)?;
        match b64::decode_url(&code) {
            Ok(c) if c.len() == CODE_LEN => {}
            _ => return Err(InviteError::BadUri),
        }
        let expires_at: UnixMs = exp.parse().map_err(|_| InviteError::BadUri)?;
        if expires_at <= 0 || !exp.bytes().all(|b| b.is_ascii_digit()) {
            return Err(InviteError::BadUri);
        }
        Ok(Invite { machine_id: mid, public_key, addr, code, expires_at })
    }
}

/// Fresh random invite code: raw bytes (kept in memory by the inviter) and the base64url string.
pub(crate) fn new_code() -> ([u8; CODE_LEN], String) {
    let mut c = [0u8; CODE_LEN];
    getrandom::fill(&mut c).expect("OS random number generator unavailable");
    let s = b64::encode_url(&c);
    (c, s)
}

/// "host:port" with a non-zero port; host is an IPv4 address, a DNS name, or "[IPv6]". Unspecified
/// addresses (0.0.0.0, ::) are rejected: nobody can dial them.
pub(crate) fn valid_addr(s: &str) -> bool {
    if s.is_empty() || s.len() > 262 {
        return false;
    }
    let (host, port) = if let Some(rest) = s.strip_prefix('[') {
        let Some((h, p)) = rest.split_once("]:") else { return false };
        match h.parse::<Ipv6Addr>() {
            Ok(ip) if !ip.is_unspecified() => {}
            _ => return false,
        }
        (None, p)
    } else {
        let Some((h, p)) = s.rsplit_once(':') else { return false };
        if h.contains(':') {
            // Bare IPv6 is ambiguous; it must be bracketed.
            return false;
        }
        (Some(h), p)
    };
    if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    match port.parse::<u16>() {
        Ok(p) if p != 0 => {}
        _ => return false,
    }
    let Some(host) = host else { return true };
    if let Ok(ip) = host.parse::<IpAddr>() {
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
pub(crate) fn peer_addr_ok(s: &str) -> bool {
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

fn escape_addr(addr: &str) -> String {
    let mut out = String::with_capacity(addr.len() + 6);
    for c in addr.chars() {
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' | ':' => out.push(c),
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
        Invite { machine_id: "abcdef012345".into(), public_key: [9u8; 32], addr: addr.into(), code, expires_at: 1_700_000_000_000 }
    }

    #[test]
    fn uri_round_trip() {
        for addr in ["192.168.1.5:47801", "mac-mini.local:47801", "[fe80::1]:47801", "[::1]:9"] {
            let inv = sample(addr);
            let uri = inv.to_uri();
            assert!(uri.starts_with("krypto://peer?v=1&mid=abcdef012345&pk="), "{uri}");
            assert!(!uri.contains('['));
            assert_eq!(Invite::parse(&uri).unwrap(), inv);
            assert_eq!(Invite::parse(&format!("  {uri}\n")).unwrap(), inv);
        }
    }

    #[test]
    fn parse_ignores_unknown_and_order() {
        let inv = sample("10.0.0.2:47801");
        let uri = format!(
            "krypto://peer?exp={}&code={}&x=y&addr=10.0.0.2:47801&pk={}&mid=abcdef012345&v=1",
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
            good.replace("krypto://peer?", "http://peer?"),
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
            good.split("&code=").next().unwrap().to_string() + "&code=AAAA&exp=1",
            format!("{}{}", good, "x".repeat(MAX_URI_LEN)),
        ];
        for b in bad {
            assert_eq!(Invite::parse(&b), Err(InviteError::BadUri), "{b}");
        }
    }

    #[test]
    fn debug_redacts_code() {
        let inv = sample("10.0.0.2:47801");
        assert!(!format!("{inv:?}").contains(&inv.code));
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
        assert_eq!(Invite::parse(&good.replace("10.0.0.2:47801", "10.0.0.2%3A47801")).unwrap().addr, "10.0.0.2:47801");
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
    }
}
