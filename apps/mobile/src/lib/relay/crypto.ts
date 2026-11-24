// NaCl box (X25519 + XSalsa20-Poly1305, 24-byte nonce) via tweetnacl, per CONTRACTS.md "Relay".
// Rust side: crypto_box::SalsaBox — byte-compatible with nacl.box.
// The PRNG must be installed before generating keys/nonces on React Native (see ./prng.ts).
import nacl from "tweetnacl";
import { decodeBase64, decodeUtf8, encodeBase64, encodeUtf8 } from "./encoding";

export const PUBLIC_KEY_BYTES = nacl.box.publicKeyLength; // 32
export const SECRET_KEY_BYTES = nacl.box.secretKeyLength; // 32
export const NONCE_BYTES = nacl.box.nonceLength; // 24

export interface BoxKeyPair {
  publicKey: Uint8Array;
  secretKey: Uint8Array;
}

/** Sealed payload as it travels in relay frames (standard base64). */
export interface SealedBox {
  nonce: string;
  box: string;
}

export function generateKeyPair(): BoxKeyPair {
  const kp = nacl.box.keyPair();
  return { publicKey: kp.publicKey, secretKey: kp.secretKey };
}

/** Re-derive a key pair from a stored secret key. */
export function keyPairFromSecretKey(secretKey: Uint8Array): BoxKeyPair {
  if (secretKey.length !== SECRET_KEY_BYTES) throw new Error("bad secret key length");
  const kp = nacl.box.keyPair.fromSecretKey(secretKey);
  return { publicKey: kp.publicKey, secretKey: kp.secretKey };
}

/** Decode and length-check a base64 public key (throws with a readable message). */
export function decodePublicKey(b64: string): Uint8Array {
  let pk: Uint8Array;
  try {
    pk = decodeBase64(b64);
  } catch {
    throw new Error("public key is not valid base64");
  }
  if (pk.length !== PUBLIC_KEY_BYTES) throw new Error(`public key must be ${PUBLIC_KEY_BYTES} bytes, got ${pk.length}`);
  return pk;
}

/**
 * A NaCl box channel with one peer. Uses the precomputed shared key (`nacl.box.before`) so each frame
 * only pays for XSalsa20-Poly1305.
 */
export class BoxChannel {
  private readonly shared: Uint8Array;

  constructor(theirPublicKey: Uint8Array, mySecretKey: Uint8Array) {
    if (theirPublicKey.length !== PUBLIC_KEY_BYTES) throw new Error("bad peer public key length");
    if (mySecretKey.length !== SECRET_KEY_BYTES) throw new Error("bad secret key length");
    this.shared = nacl.box.before(theirPublicKey, mySecretKey);
  }

  /** Encrypt UTF-8 text with a fresh random nonce. */
  sealText(text: string): SealedBox {
    const nonce = nacl.randomBytes(NONCE_BYTES);
    const box = nacl.box.after(encodeUtf8(text), nonce, this.shared);
    return { nonce: encodeBase64(nonce), box: encodeBase64(box) };
  }

  /** Decrypt to UTF-8 text; null if the box fails authentication or is malformed. */
  openText(sealed: SealedBox): string | null {
    let nonce: Uint8Array;
    let box: Uint8Array;
    try {
      nonce = decodeBase64(sealed.nonce);
      box = decodeBase64(sealed.box);
    } catch {
      return null;
    }
    if (nonce.length !== NONCE_BYTES) return null;
    const plain = nacl.box.open.after(box, nonce, this.shared);
    return plain ? decodeUtf8(plain) : null;
  }

  sealJson(value: unknown): SealedBox {
    return this.sealText(JSON.stringify(value));
  }

  /** Decrypt + JSON.parse; null on auth failure or invalid JSON. */
  openJson<T = unknown>(sealed: SealedBox): T | null {
    const text = this.openText(sealed);
    if (text == null) return null;
    try {
      return JSON.parse(text) as T;
    } catch {
      return null;
    }
  }
}

/** Short human fingerprint of a public key, e.g. "3F9A·0C1D·77E2" (for the pairing confirm screen). */
export function keyFingerprint(publicKey: Uint8Array): string {
  const h = nacl.hash(publicKey);
  const hex = Array.from(h.slice(0, 6), (b) => b.toString(16).padStart(2, "0")).join("").toUpperCase();
  return `${hex.slice(0, 4)}·${hex.slice(4, 8)}·${hex.slice(8, 12)}`;
}

export { encodeBase64, decodeBase64 };
