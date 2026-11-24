// Pure JS standard base64 + UTF-8 codecs. No reliance on atob/btoa/TextEncoder so the code behaves the
// same on Hermes, JSC and bun (unit tests).

const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const LOOKUP = (() => {
  const t = new Int16Array(128).fill(-1);
  for (let i = 0; i < ALPHABET.length; i++) t[ALPHABET.charCodeAt(i)] = i;
  // Accept URL-safe input too (some QR generators emit it).
  t["-".charCodeAt(0)] = 62;
  t["_".charCodeAt(0)] = 63;
  return t;
})();

/** Standard base64 (RFC 4648 §4) with `=` padding. */
export function encodeBase64(bytes: Uint8Array): string {
  let out = "";
  let i = 0;
  for (; i + 2 < bytes.length; i += 3) {
    const n = (bytes[i]! << 16) | (bytes[i + 1]! << 8) | bytes[i + 2]!;
    out += ALPHABET[(n >> 18) & 63]! + ALPHABET[(n >> 12) & 63]! + ALPHABET[(n >> 6) & 63]! + ALPHABET[n & 63]!;
  }
  const rest = bytes.length - i;
  if (rest === 1) {
    const n = bytes[i]! << 16;
    out += ALPHABET[(n >> 18) & 63]! + ALPHABET[(n >> 12) & 63]! + "==";
  } else if (rest === 2) {
    const n = (bytes[i]! << 16) | (bytes[i + 1]! << 8);
    out += ALPHABET[(n >> 18) & 63]! + ALPHABET[(n >> 12) & 63]! + ALPHABET[(n >> 6) & 63]! + "=";
  }
  return out;
}

/** Decode standard (or URL-safe) base64; whitespace ignored, padding optional. Throws on bad input. */
export function decodeBase64(input: string): Uint8Array {
  const s = input.replace(/[\s=]+/g, "");
  if (s.length % 4 === 1) throw new Error("invalid base64 length");
  const out = new Uint8Array(Math.floor((s.length * 3) / 4));
  let buf = 0;
  let bits = 0;
  let o = 0;
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    const v = c < 128 ? LOOKUP[c]! : -1;
    if (v < 0) throw new Error("invalid base64 character");
    buf = (buf << 6) | v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out[o++] = (buf >> bits) & 0xff;
    }
  }
  return o === out.length ? out : out.slice(0, o);
}

/** UTF-8 encode (handles surrogate pairs; lone surrogates become U+FFFD). */
export function encodeUtf8(text: string): Uint8Array {
  const out: number[] = [];
  for (let i = 0; i < text.length; i++) {
    let cp = text.charCodeAt(i);
    if (cp >= 0xd800 && cp <= 0xdbff) {
      const next = i + 1 < text.length ? text.charCodeAt(i + 1) : 0;
      if (next >= 0xdc00 && next <= 0xdfff) {
        cp = 0x10000 + ((cp - 0xd800) << 10) + (next - 0xdc00);
        i++;
      } else cp = 0xfffd;
    } else if (cp >= 0xdc00 && cp <= 0xdfff) cp = 0xfffd;
    if (cp < 0x80) out.push(cp);
    else if (cp < 0x800) out.push(0xc0 | (cp >> 6), 0x80 | (cp & 63));
    else if (cp < 0x10000) out.push(0xe0 | (cp >> 12), 0x80 | ((cp >> 6) & 63), 0x80 | (cp & 63));
    else out.push(0xf0 | (cp >> 18), 0x80 | ((cp >> 12) & 63), 0x80 | ((cp >> 6) & 63), 0x80 | (cp & 63));
  }
  return Uint8Array.from(out);
}

/** UTF-8 decode; malformed sequences become U+FFFD. */
export function decodeUtf8(bytes: Uint8Array): string {
  let out = "";
  let chunk: number[] = [];
  const flush = () => {
    out += String.fromCharCode(...chunk);
    chunk = [];
  };
  const push = (cp: number) => {
    if (cp > 0xffff) {
      cp -= 0x10000;
      chunk.push(0xd800 + (cp >> 10), 0xdc00 + (cp & 0x3ff));
    } else chunk.push(cp);
    if (chunk.length > 4096) flush();
  };
  for (let i = 0; i < bytes.length; ) {
    const b = bytes[i]!;
    let need = 0;
    let cp = 0;
    let min = 0;
    if (b < 0x80) {
      push(b);
      i++;
      continue;
    } else if ((b & 0xe0) === 0xc0) {
      need = 1;
      cp = b & 0x1f;
      min = 0x80;
    } else if ((b & 0xf0) === 0xe0) {
      need = 2;
      cp = b & 0x0f;
      min = 0x800;
    } else if ((b & 0xf8) === 0xf0) {
      need = 3;
      cp = b & 0x07;
      min = 0x10000;
    } else {
      push(0xfffd);
      i++;
      continue;
    }
    if (i + need >= bytes.length) {
      // Truncated sequence at the end.
      push(0xfffd);
      break;
    }
    let ok = true;
    for (let k = 1; k <= need; k++) {
      const c = bytes[i + k];
      if (c === undefined || (c & 0xc0) !== 0x80) {
        ok = false;
        break;
      }
      cp = (cp << 6) | (c & 0x3f);
    }
    if (!ok || cp < min || cp > 0x10ffff || (cp >= 0xd800 && cp <= 0xdfff)) {
      push(0xfffd);
      i++;
      continue;
    }
    push(cp);
    i += need + 1;
  }
  flush();
  return out;
}

/** Lowercase hex of bytes. */
export function toHex(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += b.toString(16).padStart(2, "0");
  return s;
}
