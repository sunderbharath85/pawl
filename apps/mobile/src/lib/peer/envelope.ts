// Sealing and opening PeerEnvelopes, byte-compatible with crates/peer/src/envelope.rs.
//
//   nonce = 15 random bytes || dir (1 byte) || seq (u64 big-endian)
//   dir   = 1 if sender_pk < receiver_pk (bytewise) else 0
//
// NaCl box has no associated data, so seq and direction are bound through the authenticated nonce: `open` rejects
// an envelope whose `seq` differs from its nonce (no seq-bumped replays) or whose direction byte is wrong (no frame
// reflected back to its sender). `from` / `to` are bound by the keys: the receiver opens with its own secret and the
// roster key of `from`.
import type { PeerBody, PeerEnvelope } from "@krypto/protocol";
import { compareBytes, decode, encode, tryDecode, utf8Decode, utf8Encode } from "./b64";
import { NONCE_BYTES, TAG_BYTES, type DeviceKeys } from "./keys";
import type { RandomBytes } from "./types";

export const PEER_ENVELOPE_VERSION = 1;
/** Index of the direction byte; the relay auth nonce domain (>= 2 there) relies on it. */
export const NONCE_RANDOM_LEN = 15;

export type OpenError = "bad_version" | "bad_encoding" | "bad_nonce" | "decrypt_failed" | "bad_body";

export type OpenResult = { ok: true; body: PeerBody; plaintext: string } | { ok: false; error: OpenError };

export function direction(senderPk: Uint8Array, receiverPk: Uint8Array): number {
  return compareBytes(senderPk, receiverPk) < 0 ? 1 : 0;
}

/** u64 big-endian of a non-negative safe integer. */
export function seqBytes(seq: number): Uint8Array {
  if (!Number.isSafeInteger(seq) || seq < 0) throw new Error("seq must be a non-negative safe integer");
  const out = new Uint8Array(8);
  let hi = Math.floor(seq / 0x1_0000_0000);
  let lo = seq % 0x1_0000_0000;
  for (let i = 7; i >= 4; i--) {
    out[i] = lo & 0xff;
    lo = Math.floor(lo / 256);
  }
  for (let i = 3; i >= 0; i--) {
    out[i] = hi & 0xff;
    hi = Math.floor(hi / 256);
  }
  return out;
}

/** The 24-byte nonce for a frame. `random15` = the random prefix (tests pass a fixed one). */
export function makeNonce(senderPk: Uint8Array, receiverPk: Uint8Array, seq: number, random15: Uint8Array): Uint8Array {
  if (random15.length !== NONCE_RANDOM_LEN) throw new Error("nonce prefix must be 15 bytes");
  const n = new Uint8Array(NONCE_BYTES);
  n.set(random15, 0);
  n[NONCE_RANDOM_LEN] = direction(senderPk, receiverPk);
  n.set(seqBytes(seq), NONCE_RANDOM_LEN + 1);
  return n;
}

/** UTF-8 JSON of a body (JSON.stringify keeps insertion order; receivers parse, so key order does not matter). */
export function encodeBody(body: PeerBody): Uint8Array {
  return utf8Encode(JSON.stringify(body));
}

export interface SealOptions {
  /** Add our public key (only on `pair_request`: the receiver does not know us yet). */
  includePk?: boolean;
  /** Override the nonce's random prefix (cross-language vectors). Production leaves it to `random`. */
  noncePrefix?: Uint8Array;
}

/** Seal already-serialized plaintext (UTF-8 JSON of a PeerBody). */
export function sealPlaintext(
  keys: DeviceKeys,
  from: string,
  to: string,
  toPk: Uint8Array,
  seq: number,
  plaintext: Uint8Array,
  random: RandomBytes,
  opts: SealOptions = {},
): PeerEnvelope {
  const prefix = opts.noncePrefix ?? random(NONCE_RANDOM_LEN);
  const nonce = makeNonce(keys.publicKey, toPk, seq, prefix);
  const ciphertext = keys.box(plaintext, nonce, toPk);
  const env: PeerEnvelope = { v: PEER_ENVELOPE_VERSION, from, to, seq, nonce: encode(nonce), box: encode(ciphertext) };
  if (opts.includePk) env.pk = keys.publicB64;
  return env;
}

/** Seal `body` from this device (`from`) to `to` (public key `toPk`). */
export function seal(
  keys: DeviceKeys,
  from: string,
  to: string,
  toPk: Uint8Array,
  seq: number,
  body: PeerBody,
  random: RandomBytes,
  opts: SealOptions = {},
): PeerEnvelope {
  return sealPlaintext(keys, from, to, toPk, seq, encodeBody(body), random, opts);
}

function isObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * Shape check of an envelope from the wire (the relay already checked it, but the core does not trust the relay).
 * Rebuilds it with only the known fields.
 */
export function parseEnvelope(v: unknown): PeerEnvelope | null {
  if (!isObject(v)) return null;
  if (typeof v.from !== "string" || typeof v.to !== "string") return null;
  if (typeof v.seq !== "number" || !Number.isSafeInteger(v.seq) || v.seq < 0) return null;
  if (typeof v.nonce !== "string" || typeof v.box !== "string") return null;
  if (typeof v.v !== "number") return null;
  const env: PeerEnvelope = { v: v.v as 1, from: v.from, to: v.to, seq: v.seq, nonce: v.nonce, box: v.box };
  if (v.pk !== undefined) {
    if (typeof v.pk !== "string") return null;
    env.pk = v.pk;
  }
  return env;
}

/** Minimal structural check that decrypted JSON is a PeerBody (a known `type` string). */
const BODY_TYPES = new Set([
  "pair_request",
  "pair_accept",
  "pair_reject",
  "hello",
  "presence",
  "bye",
  "mirror",
  "sync_request",
  "mirror_snapshot",
  "mirror_events",
  "sync_done",
  "intent",
  "intent_ack",
  "roster_intro",
]);

export function parseBody(text: string): PeerBody | null {
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    return null;
  }
  if (!isObject(v) || typeof v.type !== "string" || !BODY_TYPES.has(v.type)) return null;
  return v as unknown as PeerBody;
}

/**
 * Open an envelope sent to this device by the holder of `senderPk`: version, encoding, then the nonce binding
 * (direction + seq), then authentication. The caller applies from/to/seq policy.
 */
export function open(keys: DeviceKeys, env: PeerEnvelope, senderPk: Uint8Array): OpenResult {
  if (env.v !== PEER_ENVELOPE_VERSION) return { ok: false, error: "bad_version" };
  const nonce = tryDecode(env.nonce);
  if (!nonce || nonce.length !== NONCE_BYTES) return { ok: false, error: "bad_encoding" };
  const ciphertext = tryDecode(env.box);
  if (!ciphertext || ciphertext.length < TAG_BYTES) return { ok: false, error: "bad_encoding" };
  if (!Number.isSafeInteger(env.seq) || env.seq < 0) return { ok: false, error: "bad_nonce" };
  if (nonce[NONCE_RANDOM_LEN] !== direction(senderPk, keys.publicKey)) return { ok: false, error: "bad_nonce" };
  const want = seqBytes(env.seq);
  for (let i = 0; i < 8; i++) {
    if (nonce[NONCE_RANDOM_LEN + 1 + i] !== want[i]) return { ok: false, error: "bad_nonce" };
  }
  const plain = keys.open(ciphertext, nonce, senderPk);
  if (!plain) return { ok: false, error: "decrypt_failed" };
  const text = utf8Decode(plain);
  if (text === null) return { ok: false, error: "bad_body" };
  const body = parseBody(text);
  if (!body) return { ok: false, error: "bad_body" };
  return { ok: true, body, plaintext: text };
}

/** Decode the nonce of an envelope (for tests / diagnostics). */
export function envelopeNonce(env: PeerEnvelope): Uint8Array {
  return decode(env.nonce);
}
