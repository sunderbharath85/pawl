// One-time pairing invites, parsed exactly like crates/peer/src/invite.rs (`Invite::parse`):
//   krypto://peer?v=1&mid=<machineId>&pk=<b64url pk>[&addr=<host:port>]&code=<b64url 16 bytes>&exp=<unix ms>
//   [&relay=<percent-encoded relay URL>&rt=<b64url 16 bytes>[&rk=<relay access key>]]
// The code, relay token and access key are secrets: nothing here logs or returns them except in the Invite itself.
import { MACHINE_ID_RE, type MachineId, type UnixMs } from "@krypto/protocol";
import { encodeUrl, tryDecodeUrl, utf8Decode, utf8Encode } from "./b64";
import nacl from "tweetnacl";
import { isLowOrderKey } from "./keys";
import { normalizeRelayUrl, relayHostHeader, validAccessKey, validAddr } from "./net";

export const INVITE_PREFIX = "krypto://peer?";
const MAX_URI_LEN = 1024;
export const INVITE_CODE_BYTES = 16;

export interface Invite {
  machineId: MachineId;
  publicKey: Uint8Array;
  /** "host:port" of the inviter's peer listener; null only when `relay` is set (relay-only / phone invites). */
  addr: string | null;
  /** Base64url of 16 random bytes. Secret. */
  code: string;
  expiresAt: UnixMs;
  /** Normalized relay URL. */
  relay: string | null;
  /** The inviter's one-shot relay pairing slot (set iff `relay` is). Secret. */
  relayToken: string | null;
  /** The relay access key (only with `relay`). Secret. */
  relayKey: string | null;
}

export class InviteError extends Error {
  readonly code = "bad_uri";
  constructor() {
    super("bad_uri");
    this.name = "InviteError";
  }
}

function isHex(c: number): boolean {
  return (c >= 48 && c <= 57) || (c >= 65 && c <= 70) || (c >= 97 && c <= 102);
}

/** Strict percent-decoding: `%` needs exactly two hex digits, `+` is refused, the result must be UTF-8. */
export function percentDecode(s: string): string | null {
  const raw = utf8Encode(s);
  const bytes: number[] = [];
  for (let i = 0; i < raw.length; ) {
    const c = raw[i]!;
    if (c === 37 /* % */) {
      const a = raw[i + 1];
      const b = raw[i + 2];
      if (a === undefined || b === undefined || !isHex(a) || !isHex(b)) return null;
      bytes.push(parseInt(String.fromCharCode(a, b), 16));
      i += 3;
    } else if (c === 43 /* + */) {
      return null;
    } else {
      bytes.push(c);
      i++;
    }
  }
  return utf8Decode(Uint8Array.from(bytes));
}

const KEYS = ["v", "mid", "pk", "addr", "code", "exp", "relay", "rt", "rk"] as const;
type Key = (typeof KEYS)[number];

/**
 * Strict parse. Surrounding whitespace is ignored (pasted URIs); unknown parameters are ignored; duplicates, a
 * missing parameter or any invalid value throw `InviteError` (`bad_uri`). `addr` may be missing only when `relay`
 * is present, `relay` and `rt` come together, and `rk` needs `relay`. Expiry is not checked here (the inviter decides).
 */
export function parseInvite(raw: string): Invite {
  const uri = raw.trim();
  if (uri.length > MAX_URI_LEN || uri.length < INVITE_PREFIX.length) throw new InviteError();
  const scheme = uri.slice(0, INVITE_PREFIX.length);
  const query = uri.slice(INVITE_PREFIX.length);
  if (scheme.toLowerCase() !== INVITE_PREFIX || query.includes("#")) throw new InviteError();
  const fields: Partial<Record<Key, string>> = {};
  for (const pair of query.split("&")) {
    const eq = pair.indexOf("=");
    if (eq < 0) throw new InviteError();
    const k = pair.slice(0, eq);
    const v = pair.slice(eq + 1);
    if (!(KEYS as readonly string[]).includes(k)) continue;
    if (fields[k as Key] !== undefined) throw new InviteError();
    const decoded = percentDecode(v);
    if (decoded === null) throw new InviteError();
    fields[k as Key] = decoded;
  }
  const { v, mid, pk, addr, code, exp, relay, rt, rk } = fields;
  if (v === undefined || mid === undefined || pk === undefined || code === undefined || exp === undefined) {
    throw new InviteError();
  }
  if (v !== "1" || !MACHINE_ID_RE.test(mid) || (addr !== undefined && !validAddr(addr))) throw new InviteError();
  if (rk !== undefined && (relay === undefined || !validAccessKey(rk))) throw new InviteError();
  let relayUrl: string | null = null;
  let relayToken: string | null = null;
  if (relay !== undefined || rt !== undefined) {
    if (relay === undefined || rt === undefined) throw new InviteError();
    relayUrl = normalizeRelayUrl(relay);
    if (!relayUrl) throw new InviteError();
    const t = tryDecodeUrl(rt);
    if (!t || t.length !== INVITE_CODE_BYTES) throw new InviteError();
    relayToken = rt;
  }
  if (addr === undefined && relayUrl === null) throw new InviteError();
  const publicKey = tryDecodeUrl(pk);
  if (!publicKey || publicKey.length !== 32 || isLowOrderKey(publicKey)) throw new InviteError();
  const c = tryDecodeUrl(code);
  if (!c || c.length !== INVITE_CODE_BYTES) throw new InviteError();
  if (!/^\d+$/.test(exp)) throw new InviteError();
  const expiresAt = Number(exp);
  if (!Number.isSafeInteger(expiresAt) || expiresAt <= 0) throw new InviteError();
  return {
    machineId: mid,
    publicKey,
    addr: addr ?? null,
    code,
    expiresAt,
    relay: relayUrl,
    relayToken,
    relayKey: rk ?? null,
  };
}

/** `parseInvite` that returns null instead of throwing. */
export function tryParseInvite(raw: string): Invite | null {
  try {
    return parseInvite(raw);
  } catch {
    return null;
  }
}

function escape(s: string, keepColon: boolean): string {
  let out = "";
  for (const b of utf8Encode(s)) {
    const ch = String.fromCharCode(b);
    if (/[A-Za-z0-9\-._~]/.test(ch) || (keepColon && ch === ":")) out += ch;
    else out += `%${b.toString(16).toUpperCase().padStart(2, "0")}`;
  }
  return out;
}

/** The URI form (same as Rust `Invite::to_uri`). Holds secrets: never log the result. */
export function inviteToUri(inv: Invite): string {
  let uri = `${INVITE_PREFIX}v=1&mid=${inv.machineId}&pk=${encodeUrl(inv.publicKey)}`;
  if (inv.addr !== null) uri += `&addr=${escape(inv.addr, true)}`;
  uri += `&code=${inv.code}&exp=${inv.expiresAt}`;
  if (inv.relay !== null && inv.relayToken !== null) {
    uri += `&relay=${escape(inv.relay, false)}&rt=${inv.relayToken}`;
    if (inv.relayKey !== null) uri += `&rk=${inv.relayKey}`;
  }
  return uri;
}

/** The non-secret parts, for the confirm screen. */
export interface InviteSummary {
  machineId: MachineId;
  relayHost: string | null;
  lanAddr: string | null;
  expiresAt: UnixMs;
  /** Short fingerprint of the inviter's key ("3F9A·0C1D·77E2"), to compare with the desktop if wanted. */
  fingerprint: string;
}

/** Short human fingerprint of a public key, e.g. "3F9A·0C1D·77E2" (SHA-512 prefix). */
export function keyFingerprint(pk: Uint8Array): string {
  const h = nacl.hash(pk);
  let hex = "";
  for (const b of h.subarray(0, 6)) hex += b.toString(16).padStart(2, "0");
  hex = hex.toUpperCase();
  return `${hex.slice(0, 4)}·${hex.slice(4, 8)}·${hex.slice(8, 12)}`;
}

export function inviteSummary(inv: Invite): InviteSummary {
  return {
    machineId: inv.machineId,
    relayHost: inv.relay ? relayHostHeader(inv.relay) : null,
    lanAddr: inv.addr,
    expiresAt: inv.expiresAt,
    fingerprint: keyFingerprint(inv.publicKey),
  };
}

/** Find an invite URI in pasted / scanned text (bare, quoted, or a `krypto peer join "<uri>"` line). */
export function extractInviteUri(text: string): string | null {
  const lower = text.toLowerCase();
  const start = lower.indexOf(INVITE_PREFIX);
  if (start < 0) return null;
  const rest = text.slice(start);
  const end = rest.search(/[\s"'<>`]/);
  const uri = end < 0 ? rest : rest.slice(0, end);
  return uri.length > INVITE_PREFIX.length ? uri : null;
}
