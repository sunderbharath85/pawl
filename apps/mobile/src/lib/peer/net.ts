// Address and URL rules, the same as crates/peer/src/invite.rs (valid_addr / valid_host / peer_addr_ok) and
// crates/peer/src/relay.rs (normalize_url / valid_access_key / address helpers).
import { encodeUrl, tryDecodeUrl } from "./b64";

const MAX_URL_LEN = 512;
const MAX_KEY_LEN = 256;
/** A relay address (an X25519 public key, base64url unpadded) is always 43 characters. */
export const RELAY_ADDRESS_LEN = 43;

// ---------- IP literals ----------

/** Rust `Ipv4Addr::from_str`: four decimal octets 0..=255, no leading zeros, nothing else. */
export function parseIpv4(s: string): number[] | null {
  const parts = s.split(".");
  if (parts.length !== 4) return null;
  const out: number[] = [];
  for (const p of parts) {
    if (!/^\d{1,3}$/.test(p) || (p.length > 1 && p[0] === "0")) return null;
    const n = Number(p);
    if (n > 255) return null;
    out.push(n);
  }
  return out;
}

/** Rust `Ipv6Addr::from_str` (eight 16-bit groups, one `::`, optional trailing IPv4). Returns the 8 segments. */
export function parseIpv6(s: string): number[] | null {
  if (s.length === 0 || s.length > 45 || !/^[0-9a-fA-F:.]+$/.test(s)) return null;
  const dbl = s.indexOf("::");
  if (dbl >= 0 && s.indexOf("::", dbl + 1) >= 0) return null;
  const parseGroups = (part: string, allowV4Tail: boolean): number[] | null => {
    if (part === "") return [];
    const groups = part.split(":");
    const out: number[] = [];
    for (let i = 0; i < groups.length; i++) {
      const g = groups[i]!;
      if (allowV4Tail && i === groups.length - 1 && g.includes(".")) {
        const v4 = parseIpv4(g);
        if (!v4) return null;
        out.push((v4[0]! << 8) | v4[1]!, (v4[2]! << 8) | v4[3]!);
        continue;
      }
      if (!/^[0-9a-fA-F]{1,4}$/.test(g)) return null;
      out.push(parseInt(g, 16));
    }
    return out;
  };
  if (dbl < 0) {
    const g = parseGroups(s, true);
    return g && g.length === 8 ? g : null;
  }
  const head = parseGroups(s.slice(0, dbl), false);
  const tail = parseGroups(s.slice(dbl + 2), true);
  if (!head || !tail || head.length + tail.length > 7) return null;
  return [...head, ...new Array<number>(8 - head.length - tail.length).fill(0), ...tail];
}

/**
 * A host we could dial: an IPv4 address, a DNS name, or "[IPv6]" (bare IPv6 is refused). Unspecified addresses
 * (0.0.0.0, ::) are rejected.
 */
export function validHost(host: string): boolean {
  if (host.startsWith("[")) {
    if (!host.endsWith("]")) return false;
    const seg = parseIpv6(host.slice(1, -1));
    return seg !== null && seg.some((x) => x !== 0);
  }
  if (host.includes(":")) return false;
  const v4 = parseIpv4(host);
  if (v4) return v4.some((x) => x !== 0);
  return (
    host.length > 0 &&
    host.length <= 253 &&
    host
      .split(".")
      .every((l) => l.length > 0 && l.length <= 63 && !l.startsWith("-") && !l.endsWith("-") && /^[A-Za-z0-9-]+$/.test(l))
  );
}

/** "host:port" with a non-zero port; host as in `validHost`. */
export function validAddr(s: string): boolean {
  if (s.length === 0 || s.length > 262) return false;
  let host: string;
  let port: string;
  if (s.startsWith("[")) {
    const end = s.indexOf("]:");
    if (end < 0) return false;
    host = s.slice(0, end + 1);
    port = s.slice(end + 2);
  } else {
    const i = s.lastIndexOf(":");
    if (i < 0) return false;
    host = s.slice(0, i);
    port = s.slice(i + 1);
  }
  if (!/^\d+$/.test(port)) return false;
  const p = Number(port);
  if (!Number.isInteger(p) || p < 1 || p > 65535) return false;
  return validHost(host);
}

function localV4(o: number[]): boolean {
  const [a, b] = [o[0]!, o[1]!];
  return (
    a === 10 ||
    (a === 172 && b >= 16 && b <= 31) ||
    (a === 192 && b === 168) ||
    a === 127 ||
    (a === 169 && b === 254) ||
    (a === 100 && (b & 0xc0) === 64)
  );
}

/**
 * May a peer-reported addr be kept? Only IP literals in private, Tailscale CGNAT, link-local, loopback and IPv6 ULA
 * ranges (a paired peer must not point a dialer at arbitrary hosts). The phone never dials addrs; it only keeps
 * them for display under the same rule.
 */
export function peerAddrOk(s: string): boolean {
  if (!validAddr(s)) return false;
  if (s.startsWith("[")) {
    const seg = parseIpv6(s.slice(1, s.indexOf("]")));
    if (!seg) return false;
    // IPv4-mapped ::ffff:a.b.c.d
    if (seg.slice(0, 5).every((x) => x === 0) && seg[5] === 0xffff) {
      return localV4([seg[6]! >> 8, seg[6]! & 0xff, seg[7]! >> 8, seg[7]! & 0xff]);
    }
    const loopback = seg.slice(0, 7).every((x) => x === 0) && seg[7] === 1;
    return loopback || (seg[0]! & 0xffc0) === 0xfe80 || (seg[0]! & 0xfe00) === 0xfc00;
  }
  const host = s.slice(0, s.lastIndexOf(":"));
  const v4 = parseIpv4(host);
  return v4 !== null && localV4(v4);
}

// ---------- relay URL + keys ----------

/** A relay access key: 1..=256 base64url characters. */
export function validAccessKey(k: string): boolean {
  return k.length > 0 && k.length <= MAX_KEY_LEN && /^[A-Za-z0-9_-]+$/.test(k);
}

function loopbackHost(host: string): boolean {
  if (host === "localhost") return true;
  const bare = host.startsWith("[") && host.endsWith("]") ? host.slice(1, -1) : host;
  const v4 = parseIpv4(bare);
  if (v4) return v4[0] === 127;
  const v6 = parseIpv6(bare);
  return v6 !== null && v6.slice(0, 7).every((x) => x === 0) && v6[7] === 1;
}

function splitAuthority(a: string): { host: string; port: number | null } | null {
  if (a.length === 0 || !/^[A-Za-z0-9.\-:[\]]+$/.test(a)) return null;
  let host: string;
  let port: string | null;
  if (a.startsWith("[")) {
    const end = a.indexOf("]");
    if (end < 0) return null;
    host = a.slice(0, end + 1);
    const rest = a.slice(end + 1);
    if (rest === "") port = null;
    else if (rest.startsWith(":")) port = rest.slice(1);
    else return null;
  } else {
    const i = a.lastIndexOf(":");
    if (i >= 0) {
      host = a.slice(0, i);
      port = a.slice(i + 1);
    } else {
      host = a;
      port = null;
    }
  }
  let portNum: number | null = null;
  if (port !== null) {
    if (!/^\d+$/.test(port)) return null;
    portNum = Number(port);
    if (!Number.isInteger(portNum) || portNum < 1 || portNum > 65535) return null;
  }
  return validHost(host) ? { host, port: portNum } : null;
}

/**
 * Normalize a relay URL like the Rust side: `wss://` or `https://` for any host; `ws://` or `http://` only for a
 * loopback host. No user info, query, fragment or percent-escapes. Returns `wss://host[:port][/path]` (or `ws://`)
 * with the host lowercased, the default port and trailing `/` dropped. Null if invalid.
 */
export function normalizeRelayUrl(raw: string): string | null {
  const s = raw.trim();
  if (s.length === 0 || s.length > MAX_URL_LEN) return null;
  const sep = s.indexOf("://");
  if (sep < 0) return null;
  const schemeRaw = s.slice(0, sep).toLowerCase();
  const rest = s.slice(sep + 3);
  let secure: boolean;
  if (schemeRaw === "wss" || schemeRaw === "https") secure = true;
  else if (schemeRaw === "ws" || schemeRaw === "http") secure = false;
  else return null;
  const slash = rest.indexOf("/");
  const authority = slash >= 0 ? rest.slice(0, slash) : rest;
  let path = slash >= 0 ? rest.slice(slash) : "";
  path = path.replace(/\/+$/, "");
  const segs = path.split("/").slice(1);
  if (!segs.every((seg) => seg.length > 0 && seg !== "." && seg !== ".." && /^[A-Za-z0-9\-._~]+$/.test(seg))) return null;
  const auth = splitAuthority(authority);
  if (!auth) return null;
  const host = auth.host.toLowerCase();
  if (!secure && !loopbackHost(host)) return null;
  const defaultPort = secure ? 443 : 80;
  const port = auth.port !== null && auth.port !== defaultPort ? `:${auth.port}` : "";
  return `${secure ? "wss" : "ws"}://${host}${port}${path}`;
}

/** The Host header a WebSocket client sends for a normalized relay URL (bound into the auth text). */
export function relayHostHeader(normalizedUrl: string): string {
  const rest = normalizedUrl.slice(normalizedUrl.indexOf("://") + 3);
  const slash = rest.indexOf("/");
  return slash >= 0 ? rest.slice(0, slash) : rest;
}

/** `{url}/v1/connect?pk=<address>[&k=<key>]`. The result holds the access key: never log it. */
export function relayConnectUrl(normalizedUrl: string, address: string, accessKey: string | null): string {
  let u = `${normalizedUrl}/v1/connect?pk=${address}`;
  if (accessKey) u += `&k=${accessKey}`;
  return u;
}

/** A device's relay address: its public key, base64url unpadded. */
export function relayAddress(pk: Uint8Array): string {
  return encodeUrl(pk);
}

/** Strict inverse of `relayAddress` (43 canonical base64url characters -> 32 bytes). */
export function parseRelayAddress(s: unknown): Uint8Array | null {
  if (typeof s !== "string" || s.length !== RELAY_ADDRESS_LEN) return null;
  const b = tryDecodeUrl(s);
  return b && b.length === 32 ? b : null;
}
