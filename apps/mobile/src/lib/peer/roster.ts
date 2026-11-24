// Group-roster introductions as the phone applies them (CONTRACTS.md "Step 7 clarifications"), plus the peer name
// rule. Pure. A `roster_intro` is accepted only from a paired, linked hub; at most 32 devices; each device passes the
// structural checks of Rust `check_intro_device` (bad_id, bad_name, bad_kind, bad_key, bad_addr, in that order);
// phones are never introduced to phones; a device whose id or key already exists with a different key / id is a
// conflict (skipped, never a replacement); a revoked id or key is never re-added.
import { MACHINE_ID_RE, type IntroDevice, type MachineOs } from "@pawl/protocol";
import { bytesEqual, decodeKey, encode } from "./b64";
import { isLowOrderKey } from "./keys";
import { peerAddrOk } from "./net";

export const INTRO_MAX_DEVICES = 32;
/** Longest device name a peer may give, in characters after trimming (Rust MAX_INTRO_NAME_CHARS). */
export const DEVICE_NAME_MAX = 64;

export type IntroSkipReason =
  | "too_many"
  | "bad_id"
  | "bad_name"
  | "bad_kind"
  | "bad_key"
  | "bad_addr"
  | "not_a_hub"
  | "duplicate"
  | "self"
  | "sender"
  | "known"
  | "id_conflict"
  | "key_conflict"
  | "revoked";

export interface IntroContext {
  myId: string;
  myPk: Uint8Array;
  senderId: string;
  senderPk: Uint8Array;
  /** Paired hubs: id -> public key. */
  known: ReadonlyMap<string, Uint8Array>;
  revokedIds: ReadonlySet<string>;
  /** Base64 (standard) public keys of revoked devices. */
  revokedPks: ReadonlySet<string>;
  /** Low-order / degenerate key check (needs our secret key; see DeviceKeys.isLowOrder). */
  isLowOrder(pk: Uint8Array): boolean;
}

export interface AcceptedIntro {
  id: string;
  name: string;
  publicKey: Uint8Array;
  addr?: string;
  os?: MachineOs;
}

export interface IntroDecision {
  accepted: AcceptedIntro[];
  skipped: { id: string; name: string; reason: IntroSkipReason }[];
}

const OS_VALUES: readonly MachineOs[] = ["windows", "macos", "linux", "ios", "android", "other"];

/**
 * Characters a peer-given name may not contain anywhere (Rust `intro::device_name_ok`, names.json vectors): control
 * characters (Unicode Cc) and invisible / direction-changing format characters: the Arabic letter mark, zero-width
 * spaces / joiners / marks, line and paragraph separators, bidi embeddings / overrides / isolates, word joiner and
 * invisible operators, the BOM.
 */
const NAME_FORBIDDEN: readonly (readonly [number, number])[] = [
  [0x0000, 0x001f],
  [0x007f, 0x009f],
  [0x061c, 0x061c],
  [0x200b, 0x200f],
  [0x2028, 0x202e],
  [0x2060, 0x2064],
  [0x2066, 0x2069],
  [0xfeff, 0xfeff],
];

/** Unicode White_Space, what Rust `str::trim` removes. */
const WHITE_SPACE: readonly (readonly [number, number])[] = [
  [0x0009, 0x000d],
  [0x0020, 0x0020],
  [0x0085, 0x0085],
  [0x00a0, 0x00a0],
  [0x1680, 0x1680],
  [0x2000, 0x200a],
  [0x2028, 0x2029],
  [0x202f, 0x202f],
  [0x205f, 0x205f],
  [0x3000, 0x3000],
];

function inRanges(cp: number, ranges: readonly (readonly [number, number])[]): boolean {
  for (const [lo, hi] of ranges) if (cp >= lo && cp <= hi) return true;
  return false;
}

/** Code points of `s` with Unicode White_Space trimmed from both ends (Rust `str::trim`). */
function trimmedCodePoints(s: string): string[] {
  const cps = [...s];
  let a = 0;
  let b = cps.length;
  while (a < b && inRanges(cps[a]!.codePointAt(0)!, WHITE_SPACE)) a++;
  while (b > a && inRanges(cps[b - 1]!.codePointAt(0)!, WHITE_SPACE)) b--;
  return cps.slice(a, b);
}

/** No forbidden character anywhere in the (untrimmed) name. */
function noForbidden(s: string): boolean {
  for (const c of s) if (inRanges(c.codePointAt(0)!, NAME_FORBIDDEN)) return false;
  return true;
}

/**
 * Rust `device_name_ok`: no forbidden character anywhere in the untrimmed name, then 1..=max code points once Unicode
 * White_Space is trimmed. Every name a peer gives us (pair_accept, hello, presence, roster_intro) must pass: names end
 * up in the biometric prompt ("Approve on <name>"), the approval cards and the notices.
 */
export function deviceNameOk(raw: unknown, max = DEVICE_NAME_MAX): raw is string {
  if (typeof raw !== "string" || !noForbidden(raw)) return false;
  const len = trimmedCodePoints(raw).length;
  return len >= 1 && len <= max;
}

/** The trimmed name if it passes `deviceNameOk`, else null. */
export function cleanDeviceName(raw: unknown, max = DEVICE_NAME_MAX): string | null {
  return deviceNameOk(raw, max) ? trimmedCodePoints(raw).join("") : null;
}

/** Remove forbidden characters (for a name this phone derives itself, e.g. the OS device name). */
export function stripNameChars(raw: string): string {
  let out = "";
  for (const c of raw) if (!inRanges(c.codePointAt(0)!, NAME_FORBIDDEN)) out += c;
  return out;
}

/** Phones (and only phones) run iOS or Android (Rust `phone_os`). */
export function isPhoneOs(os: unknown): boolean {
  return os === "ios" || os === "android";
}

export type IntroCheckCode = "bad_id" | "bad_name" | "bad_kind" | "bad_key" | "bad_addr";

export type IntroCheck =
  | { ok: true; id: string; name: string; kind: "hub" | "phone"; publicKey: Uint8Array; addr?: string; os?: MachineOs }
  | { ok: false; code: IntroCheckCode };

/**
 * Structural check of one introduced device, exactly Rust `check_intro_device` (codes in its order): bad_id (not a
 * 12-char [a-z0-9] id), bad_name (`deviceNameOk`), bad_kind (kind phone iff os is ios / android; a hub may have no
 * os; an unknown kind or os), bad_key (not canonical standard base64 of 32 bytes, or of small order), bad_addr (an
 * addr `peerAddrOk` refuses, or any addr on a phone).
 */
export function checkIntroDevice(raw: unknown): IntroCheck {
  const d = (typeof raw === "object" && raw !== null ? raw : {}) as Partial<Record<keyof IntroDevice, unknown>>;
  if (typeof d.id !== "string" || !MACHINE_ID_RE.test(d.id)) return { ok: false, code: "bad_id" };
  const name = cleanDeviceName(d.name);
  if (name === null) return { ok: false, code: "bad_name" };
  const os = d.os === undefined || d.os === null ? undefined : d.os;
  if ((d.kind !== "hub" && d.kind !== "phone") || (os !== undefined && !OS_VALUES.includes(os as MachineOs))) {
    return { ok: false, code: "bad_kind" };
  }
  const isPhone = d.kind === "phone";
  if (isPhone !== isPhoneOs(os)) return { ok: false, code: "bad_kind" };
  const pk = decodeKey(d.publicKey);
  if (!pk || isLowOrderKey(pk)) return { ok: false, code: "bad_key" };
  const addr = d.addr === undefined || d.addr === null ? undefined : d.addr;
  if (addr !== undefined && (isPhone || typeof addr !== "string" || !peerAddrOk(addr))) return { ok: false, code: "bad_addr" };
  const out: IntroCheck = { ok: true, id: d.id, name, kind: d.kind, publicKey: pk };
  if (typeof addr === "string") out.addr = addr;
  if (os !== undefined) out.os = os as MachineOs;
  return out;
}

/** What the phone does with a `roster_intro`: structural checks, then the receiver's own (context) rules. */
export function evaluateIntro(ctx: IntroContext, devices: unknown): IntroDecision {
  const out: IntroDecision = { accepted: [], skipped: [] };
  if (!Array.isArray(devices)) return out;
  if (devices.length > INTRO_MAX_DEVICES) {
    out.skipped.push({ id: "", name: "", reason: "too_many" });
    return out;
  }
  const seenIds = new Set<string>();
  const seenPks: Uint8Array[] = [];
  for (const raw of devices as unknown[]) {
    const c = checkIntroDevice(raw);
    if (!c.ok) {
      const r = (typeof raw === "object" && raw !== null ? raw : {}) as { id?: unknown; name?: unknown };
      // Only an id / name that passed the rules is ever shown (in a notice).
      out.skipped.push({
        id: typeof r.id === "string" && MACHINE_ID_RE.test(r.id) ? r.id : "",
        name: cleanDeviceName(r.name) ?? "",
        reason: c.code,
      });
      continue;
    }
    const { id, name, publicKey: pk } = c;
    const skip = (reason: IntroSkipReason) => out.skipped.push({ id, name, reason });
    // Structurally fine, but phones are never introduced to phones.
    if (c.kind === "phone") {
      skip("not_a_hub");
      continue;
    }
    if (seenIds.has(id) || seenPks.some((p) => bytesEqual(p, pk))) {
      skip("duplicate");
      continue;
    }
    // A degenerate shared secret with our own key (beyond the small-order table).
    if (ctx.isLowOrder(pk)) {
      skip("bad_key");
      continue;
    }
    if (id === ctx.myId || bytesEqual(pk, ctx.myPk)) {
      skip("self");
      continue;
    }
    if (id === ctx.senderId || bytesEqual(pk, ctx.senderPk)) {
      skip("sender");
      continue;
    }
    const pkB64 = encode(pk);
    if (ctx.revokedIds.has(id) || ctx.revokedPks.has(pkB64)) {
      skip("revoked");
      continue;
    }
    const existing = ctx.known.get(id);
    if (existing) {
      skip(bytesEqual(existing, pk) ? "known" : "id_conflict");
      continue;
    }
    let keyOwner: string | null = null;
    for (const [kid, kpk] of ctx.known) if (bytesEqual(kpk, pk)) keyOwner = kid;
    if (keyOwner !== null) {
      skip("key_conflict");
      continue;
    }
    seenIds.add(id);
    seenPks.push(pk);
    const accepted: AcceptedIntro = { id, name, publicKey: pk };
    if (c.addr !== undefined) accepted.addr = c.addr;
    if (c.os !== undefined) accepted.os = c.os;
    out.accepted.push(accepted);
  }
  return out;
}

/** Retry schedule for hello to a hub that refused us or did not answer: 1 s, 2 s, 5 s, then every 30 s. */
export const HELLO_RETRY_MS: readonly number[] = [1_000, 2_000, 5_000, 30_000];

export function helloRetryDelay(attempt: number): number {
  return HELLO_RETRY_MS[Math.min(Math.max(0, attempt), HELLO_RETRY_MS.length - 1)]!;
}
