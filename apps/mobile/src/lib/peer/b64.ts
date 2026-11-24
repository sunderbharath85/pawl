// Strict RFC 4648 base64, byte-for-byte the same as crates/peer/src/b64.rs: standard alphabet with padding
// (`encode` / `decode`) and the URL-safe alphabet without padding (`encodeUrl` / `decodeUrl`). Decoding is
// canonical: no whitespace, no mixed alphabets, exact padding, and the unused low bits of the last symbol must be
// zero, so every byte string has exactly one accepted encoding. Also UTF-8 helpers (Hermes has no reliable
// TextDecoder). Pure: no React Native or Node imports.

const STD = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

export class B64Error extends Error {
  constructor(readonly kind: "invalid_byte" | "invalid_length" | "invalid_padding" | "trailing_bits") {
    super(`invalid base64 (${kind})`);
    this.name = "B64Error";
  }
}

function encodeWith(b: Uint8Array, alphabet: string, pad: boolean): string {
  let out = "";
  let i = 0;
  for (; i + 3 <= b.length; i += 3) {
    const n = (b[i]! << 16) | (b[i + 1]! << 8) | b[i + 2]!;
    out += alphabet[(n >> 18) & 63]! + alphabet[(n >> 12) & 63]! + alphabet[(n >> 6) & 63]! + alphabet[n & 63]!;
  }
  const rest = b.length - i;
  if (rest === 1) {
    const n = b[i]! << 16;
    out += alphabet[(n >> 18) & 63]! + alphabet[(n >> 12) & 63]!;
    if (pad) out += "==";
  } else if (rest === 2) {
    const n = (b[i]! << 16) | (b[i + 1]! << 8);
    out += alphabet[(n >> 18) & 63]! + alphabet[(n >> 12) & 63]! + alphabet[(n >> 6) & 63]!;
    if (pad) out += "=";
  }
  return out;
}

function sextet(alphabet: string, c: number): number {
  if (c >= 65 && c <= 90) return c - 65; // A-Z
  if (c >= 97 && c <= 122) return c - 97 + 26; // a-z
  if (c >= 48 && c <= 57) return c - 48 + 52; // 0-9
  if (c === alphabet.charCodeAt(62)) return 62;
  if (c === alphabet.charCodeAt(63)) return 63;
  return -1;
}

function decodeWith(s: string, alphabet: string, padded: boolean): Uint8Array {
  let body = s;
  if (padded) {
    if (s.length % 4 !== 0) throw new B64Error("invalid_length");
    let pad = 0;
    while (pad < s.length && s.charCodeAt(s.length - 1 - pad) === 61) pad++;
    if (pad > 2) throw new B64Error("invalid_padding");
    body = s.slice(0, s.length - pad);
  }
  if (body.includes("=")) throw new B64Error("invalid_byte");
  if (body.length % 4 === 1) throw new B64Error("invalid_length");
  const out = new Uint8Array(Math.floor(body.length / 4) * 3 + (body.length % 4 === 0 ? 0 : (body.length % 4) - 1));
  const val = (i: number): number => {
    const v = sextet(alphabet, body.charCodeAt(i));
    if (v < 0) throw new B64Error("invalid_byte");
    return v;
  };
  let o = 0;
  let i = 0;
  for (; i + 4 <= body.length; i += 4) {
    const n = (val(i) << 18) | (val(i + 1) << 12) | (val(i + 2) << 6) | val(i + 3);
    out[o++] = (n >> 16) & 0xff;
    out[o++] = (n >> 8) & 0xff;
    out[o++] = n & 0xff;
  }
  const rest = body.length - i;
  if (rest === 2) {
    const a = val(i);
    const b = val(i + 1);
    if ((b & 0x0f) !== 0) throw new B64Error("trailing_bits");
    out[o++] = ((a << 2) | (b >> 4)) & 0xff;
  } else if (rest === 3) {
    const a = val(i);
    const b = val(i + 1);
    const c = val(i + 2);
    if ((c & 0x03) !== 0) throw new B64Error("trailing_bits");
    out[o++] = ((a << 2) | (b >> 4)) & 0xff;
    out[o++] = ((b << 4) | (c >> 2)) & 0xff;
  }
  return out;
}

/** Standard alphabet, `=` padded. */
export function encode(b: Uint8Array): string {
  return encodeWith(b, STD, true);
}

/** Standard alphabet, padding required. Throws `B64Error`. */
export function decode(s: string): Uint8Array {
  return decodeWith(s, STD, true);
}

/** URL-safe alphabet (`-`, `_`), no padding. */
export function encodeUrl(b: Uint8Array): string {
  return encodeWith(b, URL, false);
}

/** URL-safe alphabet, padding rejected. Throws `B64Error`. */
export function decodeUrl(s: string): Uint8Array {
  return decodeWith(s, URL, false);
}

/** `decode` that returns null instead of throwing. */
export function tryDecode(s: unknown): Uint8Array | null {
  if (typeof s !== "string") return null;
  try {
    return decode(s);
  } catch {
    return null;
  }
}

/** `decodeUrl` that returns null instead of throwing. */
export function tryDecodeUrl(s: unknown): Uint8Array | null {
  if (typeof s !== "string") return null;
  try {
    return decodeUrl(s);
  } catch {
    return null;
  }
}

/** A 32-byte key from standard base64, or null. */
export function decodeKey(s: unknown): Uint8Array | null {
  const b = tryDecode(s);
  return b && b.length === 32 ? b : null;
}

// ---------- UTF-8 ----------

/** UTF-8 bytes of a string (lone surrogates become U+FFFD, as TextEncoder does). */
export function utf8Encode(s: string): Uint8Array {
  const out: number[] = [];
  for (let i = 0; i < s.length; i++) {
    let c = s.charCodeAt(i);
    if (c >= 0xd800 && c <= 0xdbff) {
      const d = i + 1 < s.length ? s.charCodeAt(i + 1) : 0;
      if (d >= 0xdc00 && d <= 0xdfff) {
        c = 0x10000 + ((c - 0xd800) << 10) + (d - 0xdc00);
        i++;
      } else c = 0xfffd;
    } else if (c >= 0xdc00 && c <= 0xdfff) c = 0xfffd;
    if (c < 0x80) out.push(c);
    else if (c < 0x800) out.push(0xc0 | (c >> 6), 0x80 | (c & 63));
    else if (c < 0x10000) out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
    else out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 63), 0x80 | ((c >> 6) & 63), 0x80 | (c & 63));
  }
  return Uint8Array.from(out);
}

/** Strict UTF-8 decode (null on any invalid sequence, overlong form or surrogate), like Rust's `from_utf8`. */
export function utf8Decode(b: Uint8Array): string | null {
  let out = "";
  let chunk: number[] = [];
  const flush = () => {
    if (chunk.length) {
      out += String.fromCharCode(...chunk);
      chunk = [];
    }
  };
  for (let i = 0; i < b.length; ) {
    const c = b[i]!;
    let cp: number;
    let need: number;
    let min: number;
    if (c < 0x80) {
      cp = c;
      need = 0;
      min = 0;
    } else if (c >= 0xc2 && c <= 0xdf) {
      cp = c & 0x1f;
      need = 1;
      min = 0x80;
    } else if (c >= 0xe0 && c <= 0xef) {
      cp = c & 0x0f;
      need = 2;
      min = 0x800;
    } else if (c >= 0xf0 && c <= 0xf4) {
      cp = c & 0x07;
      need = 3;
      min = 0x10000;
    } else return null;
    for (let k = 1; k <= need; k++) {
      const x = b[i + k];
      if (x === undefined || (x & 0xc0) !== 0x80) return null;
      cp = (cp << 6) | (x & 0x3f);
    }
    if (cp < min || cp > 0x10ffff || (cp >= 0xd800 && cp <= 0xdfff)) return null;
    if (cp >= 0x10000) {
      const v = cp - 0x10000;
      chunk.push(0xd800 + (v >> 10), 0xdc00 + (v & 0x3ff));
    } else chunk.push(cp);
    if (chunk.length >= 4096) flush();
    i += need + 1;
  }
  flush();
  return out;
}

/** Bytewise lexicographic compare (like Rust's `[u8; 32] < [u8; 32]`). */
export function compareBytes(a: Uint8Array, b: Uint8Array): number {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    if (a[i]! !== b[i]!) return a[i]! < b[i]! ? -1 : 1;
  }
  return a.length - b.length;
}

/** Constant-time equality for equal-length byte strings; different lengths return false. */
export function bytesEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i]! ^ b[i]!;
  return diff === 0;
}

/** Lowercase hex. */
export function toHex(b: Uint8Array): string {
  let s = "";
  for (const x of b) s += x.toString(16).padStart(2, "0");
  return s;
}

/** Hex (either case) to bytes, or null. */
export function fromHex(s: string): Uint8Array | null {
  if (s.length % 2 !== 0 || !/^[0-9a-fA-F]*$/.test(s)) return null;
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.slice(i * 2, i * 2 + 2), 16);
  return out;
}
