//! Web origins for browser tasks (v0.2 item 5): `[browser] allow_sites` entries, the origin of a URL a
//! browser step opens, and host names shown in their ASCII (punycode, RFC 3492) form so a look-alike
//! Unicode host cannot pass for another on an approval card.
//!
//! An entry is an origin: `github.com` means `https://github.com` (port 443); `github.com:8443` means
//! `https://github.com:8443`; `http://localhost:3000` names plain HTTP explicitly. A URL matches an entry
//! only when its scheme, host and port are all the same.

use std::fmt;

/// One web origin: scheme (`http` / `https`), ASCII host (lowercase), port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub scheme: String,
    pub host: String,
    pub port: u16,
}

fn default_port(scheme: &str) -> u16 {
    if scheme == "http" {
        80
    } else {
        443
    }
}

impl fmt::Display for Origin {
    /// `https://github.com`, `https://github.com:8443`, `http://localhost:3000`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.port == default_port(&self.scheme) {
            write!(f, "{}://{}", self.scheme, self.host)
        } else {
            write!(f, "{}://{}:{}", self.scheme, self.host, self.port)
        }
    }
}

/// Encode one label with Punycode (RFC 3492). None on overflow.
pub fn punycode(input: &str) -> Option<String> {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    const SKEW: u32 = 38;
    const DAMP: u32 = 700;
    let digit = |d: u32| -> char {
        if d < 26 {
            char::from(b'a' + d as u8)
        } else {
            char::from(b'0' + (d - 26) as u8)
        }
    };
    let adapt = |delta: u32, points: u32, first: bool| -> u32 {
        let mut delta = if first { delta / DAMP } else { delta / 2 };
        delta += delta / points;
        let mut k = 0;
        while delta > ((BASE - TMIN) * TMAX) / 2 {
            delta /= BASE - TMIN;
            k += BASE;
        }
        k + (((BASE - TMIN + 1) * delta) / (delta + SKEW))
    };
    let cps: Vec<u32> = input.chars().map(u32::from).collect();
    let mut out: String = input.chars().filter(char::is_ascii).collect();
    let basic = out.chars().count() as u32;
    let mut h = basic;
    if basic > 0 {
        out.push('-');
    }
    let (mut n, mut delta, mut bias) = (128u32, 0u32, 72u32);
    while (h as usize) < cps.len() {
        let m = cps.iter().copied().filter(|&c| c >= n).min()?;
        delta = delta.checked_add((m - n).checked_mul(h + 1)?)?;
        n = m;
        for &c in &cps {
            if c < n {
                delta = delta.checked_add(1)?;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        TMIN
                    } else if k >= bias + TMAX {
                        TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    out.push(digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(digit(q));
                bias = adapt(delta, h + 1, h == basic);
                delta = 0;
                h += 1;
            }
        }
        delta = delta.checked_add(1)?;
        n = n.checked_add(1)?;
    }
    Some(out)
}

/// A host name in its ASCII form: lowercase, one trailing dot dropped, every non-ASCII label as
/// `xn--<punycode>`. None when it is not a host name (empty label, a label over 63 characters, a
/// character other than letters, digits and `-`, a leading or trailing `-`, over 253 characters).
/// Unicode normalisation (NFC) is not applied: a name typed in another form shows a different `xn--`.
pub fn host_to_ascii(host: &str) -> Option<String> {
    let h = host.trim().to_lowercase();
    let h = h.strip_suffix('.').unwrap_or(&h);
    if h.is_empty() {
        return None;
    }
    let mut labels = Vec::new();
    for label in h.split('.') {
        if label.is_empty() || label.starts_with('-') || label.ends_with('-') || !label.chars().all(|c| c.is_alphanumeric() || c == '-') {
            return None;
        }
        let ascii = if label.is_ascii() { label.to_string() } else { format!("xn--{}", punycode(label)?) };
        if ascii.len() > 63 {
            return None;
        }
        labels.push(ascii);
    }
    let out = labels.join(".");
    (out.len() <= 253).then_some(out)
}

/// `host[:port]` -> (ASCII host, port). None for user info, an IPv6 literal, `%`, a bad port.
fn split_authority(authority: &str) -> Option<(String, Option<u16>)> {
    if authority.contains('@') || authority.starts_with('[') || authority.contains('%') || authority.contains(char::is_whitespace) {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, Some(p.parse::<u16>().ok().filter(|p| *p > 0)?)),
        None => (authority, None),
    };
    Some((host_to_ascii(host)?, port))
}

/// The scheme of a URL (`https`, `chrome`, `javascript`), lowercase, if it has one.
pub fn scheme_of(url: &str) -> Option<String> {
    let (scheme, _) = url.trim().split_once(':')?;
    let ok = scheme.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c));
    ok.then(|| scheme.to_ascii_lowercase())
}

/// The origin of an `http(s)` URL. None for any other scheme, user info, an IPv6 literal or a host
/// that is not a host name.
pub fn url_origin(url: &str) -> Option<Origin> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority = rest.split(['/', '?', '#', '\\']).next().unwrap_or_default();
    let (host, port) = split_authority(authority)?;
    let port = port.unwrap_or_else(|| default_port(&scheme));
    Some(Origin { scheme, host, port })
}

/// `url` with its host in ASCII form (what an approval card shows). Unchanged when it is not an
/// `http(s)` URL with a host name.
pub fn display_url(url: &str) -> String {
    let u = url.trim();
    let Some((scheme, rest)) = u.split_once("://") else { return u.to_string() };
    let end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    match split_authority(authority) {
        Some((host, port)) if !authority.is_ascii() => {
            let port = port.map(|p| format!(":{p}")).unwrap_or_default();
            format!("{scheme}://{host}{port}{tail}")
        }
        _ => u.to_string(),
    }
}

/// One `allow_sites` entry as an origin string (see the module docs): `github.com`,
/// `GitHub.com:8443`, `https://github.com/`, `http://localhost:3000`, `bücher.de`. No path, query,
/// wildcard or user info; the scheme is `http` or `https`.
pub fn normalize_site(entry: &str) -> Result<String, String> {
    let e = entry.trim();
    if e.is_empty() {
        return Err("is empty".into());
    }
    let bad = || "must be a site like github.com, github.com:8443 or http://localhost:3000 (no path, wildcard or user name)".to_string();
    let (scheme, rest) = match e.split_once("://") {
        Some((s, r)) => (s.to_ascii_lowercase(), r),
        None => ("https".to_string(), e),
    };
    if scheme != "http" && scheme != "https" {
        return Err("the scheme must be https (or http)".into());
    }
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.contains(['/', '?', '#', '\\', '*']) {
        return Err(bad());
    }
    let (host, port) = split_authority(rest).ok_or_else(bad)?;
    let port = port.unwrap_or_else(|| default_port(&scheme));
    Ok(Origin { scheme, host, port }.to_string())
}

/// The origins of `entries` (entries that are not valid sites are skipped).
pub fn origins(entries: &[String]) -> Vec<Origin> {
    entries.iter().filter_map(|e| normalize_site(e).ok()).filter_map(|o| url_origin(&o)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punycode_follows_rfc_3492() {
        assert_eq!(punycode("bücher").as_deref(), Some("bcher-kva"));
        assert_eq!(punycode("münchen").as_deref(), Some("mnchen-3ya"));
        assert_eq!(punycode("пример").as_deref(), Some("e1afmkfd"));
        assert_eq!(punycode("例え").as_deref(), Some("r8jz45g"));
        assert_eq!(host_to_ascii("Bücher.DE.").as_deref(), Some("xn--bcher-kva.de"));
        // A Cyrillic "а" in "аpple.com" is not "apple.com".
        assert_eq!(host_to_ascii("\u{0430}pple.com").as_deref(), Some("xn--pple-43d.com"));
        for bad in ["", "a..b", "-x.com", "x-.com", "a b.com", "x\u{202e}.com", &"a".repeat(64)] {
            assert_eq!(host_to_ascii(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn origins_need_scheme_host_and_port_to_match() {
        let o = url_origin("https://GitHub.com/acme/app/pull/1?x#y").unwrap();
        assert_eq!((o.scheme.as_str(), o.host.as_str(), o.port), ("https", "github.com", 443));
        assert_eq!(url_origin("http://localhost:3000/x").unwrap().to_string(), "http://localhost:3000");
        assert_eq!(url_origin("https://github.com:443/").unwrap().to_string(), "https://github.com");
        for bad in ["javascript:alert(1)", "file:///etc/passwd", "chrome://settings", "https://user@github.com/", "https://[::1]/", "github.com", "https:///x", "https://github.com:0/", "https://git%68ub.com/"] {
            assert_eq!(url_origin(bad), None, "{bad}");
        }
        assert_eq!(normalize_site(" GitHub.com ").unwrap(), "https://github.com");
        assert_eq!(normalize_site("github.com:443").unwrap(), "https://github.com");
        assert_eq!(normalize_site("github.com:8443").unwrap(), "https://github.com:8443");
        assert_eq!(normalize_site("https://github.com/").unwrap(), "https://github.com");
        assert_eq!(normalize_site("http://localhost:3000").unwrap(), "http://localhost:3000");
        assert_eq!(normalize_site("bücher.de").unwrap(), "https://xn--bcher-kva.de");
        for bad in ["", "ftp://x.com", "github.com/acme", "*.github.com", "user@github.com", "git hub.com", "x.com:0", "x.com:99999", "https://x.com/?a"] {
            assert!(normalize_site(bad).is_err(), "{bad:?}");
        }
        let allow = origins(&["github.com".into(), "http://localhost:3000".into(), "not a site".into()]);
        assert_eq!(allow.len(), 2);
        assert!(allow.contains(&url_origin("https://github.com/x").unwrap()));
        assert!(!allow.contains(&url_origin("http://github.com/x").unwrap()), "http is another origin");
        assert!(!allow.contains(&url_origin("https://github.com:8443/x").unwrap()), "another port is another origin");
        assert_eq!(scheme_of("View-Source:https://x").as_deref(), Some("view-source"));
        assert_eq!(scheme_of("relative/path"), None);
        assert_eq!(display_url("https://\u{0430}pple.com/login"), "https://xn--pple-43d.com/login");
        assert_eq!(display_url("https://github.com/x"), "https://github.com/x");
    }
}
