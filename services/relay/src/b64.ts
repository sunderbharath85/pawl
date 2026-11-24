// Strict base64 helpers. Standard (padded) for nonces, boxes and the relay key; base64url (unpadded) for relay
// addresses (device public keys) and pairing tokens. Decoders accept only the canonical encoding, so one key has
// exactly one address and one mailbox.

const STD_RE = /^[A-Za-z0-9+/]*={0,2}$/;
const URL_RE = /^[A-Za-z0-9_-]*$/;
const CHUNK = 0x8000;

export function encodeStd(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += CHUNK) {
    bin += String.fromCharCode(...bytes.subarray(i, i + CHUNK));
  }
  return btoa(bin);
}

export function decodeStd(s: string): Uint8Array | null {
  if (s.length % 4 !== 0 || !STD_RE.test(s)) return null;
  let bin: string;
  try {
    bin = atob(s);
  } catch {
    return null;
  }
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return encodeStd(out) === s ? out : null;
}

export function encodeUrl(bytes: Uint8Array): string {
  return encodeStd(bytes).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function decodeUrl(s: string): Uint8Array | null {
  if (!URL_RE.test(s) || s.length % 4 === 1) return null;
  const std = s.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (s.length % 4)) % 4);
  const out = decodeStd(std);
  if (!out) return null;
  return encodeUrl(out) === s ? out : null;
}

/** True when `s` only uses the standard base64 alphabet with valid padding (no decode; for large boxes). */
export function looksStd(s: string): boolean {
  return s.length % 4 === 0 && STD_RE.test(s);
}

export function utf8(s: string): Uint8Array {
  return new TextEncoder().encode(s);
}

/** Constant-time equality for equal-length secrets; different lengths return false. */
export function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i]! ^ b[i]!;
  return diff === 0;
}

export function randomBytes(n: number): Uint8Array {
  const out = new Uint8Array(n);
  crypto.getRandomValues(out);
  return out;
}
