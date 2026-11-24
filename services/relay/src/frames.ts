// Client frame validation and server frame builders. Every check here is a shape check: the relay never opens
// envelopes, it only bounds their size and makes sure they are the PeerEnvelope JSON shape.

import {
  RELAY_MAX_ALLOWED,
  type PeerEnvelope,
  type RelayClientFrame,
  type RelayErrorCode,
  type RelayServerFrame,
} from "@krypto/protocol";
import { decodeUrl, looksStd } from "./b64";
import { AUTH_FRAME_MAX_BYTES, ENV_ID_MAX_CHARS, ENV_NONCE_MAX_CHARS, ENV_PK_MAX_CHARS } from "./limits";

/** base64url, unpadded, 43 chars, canonical -> 32 bytes. A device's relay address. */
export const PK_CHARS = 43;
/** base64url, unpadded, 22 chars, canonical -> 16 bytes. */
export const TOKEN_CHARS = 22;

const PK_RE = /^[A-Za-z0-9_-]{43}$/;
const TOKEN_RE = /^[A-Za-z0-9_-]{22}$/;

export function isValidPk(s: unknown): s is string {
  return typeof s === "string" && PK_RE.test(s) && decodeUrl(s)?.length === 32;
}

export function decodePk(s: string): Uint8Array | null {
  if (!PK_RE.test(s)) return null;
  const b = decodeUrl(s);
  return b && b.length === 32 ? b : null;
}

export function isValidToken(s: unknown): s is string {
  return typeof s === "string" && TOKEN_RE.test(s) && decodeUrl(s)?.length === 16;
}

/** UTF-8 byte length without allocating when the answer is obvious. */
export function utf8Length(s: string): number {
  // Every UTF-16 code unit encodes to 1..3 bytes (a surrogate pair, 2 units, to 4).
  let bytes = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    if (c < 0x80) bytes += 1;
    else if (c < 0x800) bytes += 2;
    else if (c >= 0xd800 && c <= 0xdbff && i + 1 < s.length) {
      const d = s.charCodeAt(i + 1);
      if (d >= 0xdc00 && d <= 0xdfff) {
        bytes += 4;
        i++;
      } else bytes += 3;
    } else bytes += 3;
  }
  return bytes;
}

/** Size of a WebSocket message as received, in bytes. `max` lets huge strings bail out early. */
export function messageBytes(message: string | ArrayBuffer, max = Infinity): number {
  if (typeof message !== "string") return message.byteLength;
  if (message.length > max) return message.length; // bytes >= code units, already over
  return utf8Length(message);
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

function boundedString(v: unknown, max: number): v is string {
  return typeof v === "string" && v.length > 0 && v.length <= max;
}

/**
 * Validate a PeerEnvelope's JSON shape and rebuild it with only the known fields (so nothing but an envelope is
 * ever forwarded). Returns null on any mismatch. `seq` must be a safe integer: a JSON number above 2^53 would be
 * re-serialized differently and break the receiver's nonce check.
 */
export function parseEnvelope(v: unknown): PeerEnvelope | null {
  if (!isObject(v)) return null;
  if (v.v !== 1) return null;
  if (!boundedString(v.from, ENV_ID_MAX_CHARS) || !boundedString(v.to, ENV_ID_MAX_CHARS)) return null;
  if (typeof v.seq !== "number" || !Number.isSafeInteger(v.seq) || v.seq < 0) return null;
  if (!boundedString(v.nonce, ENV_NONCE_MAX_CHARS) || !looksStd(v.nonce)) return null;
  if (typeof v.box !== "string" || v.box.length === 0 || !looksStd(v.box)) return null;
  const env: PeerEnvelope = { v: 1, from: v.from, to: v.to, seq: v.seq, nonce: v.nonce, box: v.box };
  if (v.pk !== undefined) {
    if (!boundedString(v.pk, ENV_PK_MAX_CHARS) || !looksStd(v.pk)) return null;
    env.pk = v.pk;
  }
  return env;
}

export type ParseResult = { ok: true; frame: RelayClientFrame } | { ok: false; code: RelayErrorCode };

const BAD: ParseResult = { ok: false, code: "bad_frame" };

/**
 * Parse and validate one client frame (JSON text). Size limits are the caller's job (it knows the socket state);
 * this only checks shape. Semantic checks that need state or the clock (pairing exp window, own pk) are done by
 * the modules that own that state.
 */
export function parseClientFrame(text: string): ParseResult {
  let raw: unknown;
  try {
    raw = JSON.parse(text);
  } catch {
    return BAD;
  }
  if (!isObject(raw) || typeof raw.t !== "string") return BAD;
  switch (raw.t) {
    case "auth": {
      if (!boundedString(raw.box, AUTH_FRAME_MAX_BYTES) || !looksStd(raw.box)) return BAD;
      return { ok: true, frame: { t: "auth", box: raw.box } };
    }
    case "allow": {
      const pks = raw.pks;
      if (!Array.isArray(pks) || pks.length > RELAY_MAX_ALLOWED) return BAD;
      if (!pks.every(isValidPk)) return BAD;
      return { ok: true, frame: { t: "allow", pks: [...new Set(pks as string[])] } };
    }
    case "allow_pairing": {
      if (!isValidToken(raw.token)) return BAD;
      if (typeof raw.exp !== "number" || !Number.isSafeInteger(raw.exp)) return BAD;
      return { ok: true, frame: { t: "allow_pairing", token: raw.token, exp: raw.exp } };
    }
    case "send": {
      if (!isValidPk(raw.to)) return BAD;
      const env = parseEnvelope(raw.env);
      if (!env) return BAD;
      const pairing = raw.pairing;
      if (pairing === undefined) return { ok: true, frame: { t: "send", to: raw.to, env } };
      if (!isValidToken(pairing)) return BAD;
      return { ok: true, frame: { t: "send", to: raw.to, env, pairing } };
    }
    case "register_push": {
      if (typeof raw.expoToken !== "string") return BAD;
      return { ok: true, frame: { t: "register_push", expoToken: raw.expoToken } };
    }
    case "unregister_push":
      return { ok: true, frame: { t: "unregister_push" } };
    case "ping": {
      if (typeof raw.ts !== "number" || !Number.isFinite(raw.ts)) return BAD;
      return { ok: true, frame: { t: "ping", ts: raw.ts } };
    }
    default:
      return BAD;
  }
}

/** Frames that change mailbox config; they share the per-mailbox config budget (`Inbox.takeConfig`). */
export function isConfigFrame(f: RelayClientFrame): boolean {
  return f.t === "allow" || f.t === "allow_pairing" || f.t === "register_push" || f.t === "unregister_push";
}

// ---------- server frames ----------

export function encodeServerFrame(frame: RelayServerFrame): string {
  return JSON.stringify(frame);
}

export function errorFrame(code: RelayErrorCode, to?: string): string {
  return encodeServerFrame(to === undefined ? { t: "error", code } : { t: "error", code, to });
}

export function okFrame(queued: number): string {
  return encodeServerFrame({ t: "ok", queued });
}

export function pongFrame(ts: number): string {
  return encodeServerFrame({ t: "pong", ts });
}

/** The frame a recipient gets. `from` is always the sender's authenticated public key. */
export function msgFrame(from: string, env: PeerEnvelope): string {
  return encodeServerFrame({ t: "msg", from, env });
}
