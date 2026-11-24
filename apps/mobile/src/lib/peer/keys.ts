// The phone's long-term X25519 device key (the same key type as a hub's `device.key`). The secret lives in a
// private field with no accessor except `exportSecret()`, which only the secure-storage adapter calls; nothing
// here logs or serializes it. NaCl box = X25519 + XSalsa20-Poly1305 (tweetnacl `nacl.box`, byte-compatible with
// Rust `crypto_box::SalsaBox`).
import nacl from "tweetnacl";
import { bytesEqual, encode, encodeUrl } from "./b64";
import type { RandomBytes } from "./types";

export const KEY_BYTES = 32;
export const NONCE_BYTES = 24;
export const TAG_BYTES = 16;

/**
 * X25519 points of small order, as libsodium's blocklist lists them (bit 255 cleared; X25519 ignores it), the same
 * table as crates/peer/src/intro.rs. A box to such a key has a shared secret anyone can compute.
 */
const LOW_ORDER: readonly (readonly number[])[] = [
  new Array<number>(32).fill(0),
  [1, ...new Array<number>(31).fill(0)],
  [
    0xe0, 0xeb, 0x7a, 0x7c, 0x3b, 0x41, 0xb8, 0xae, 0x16, 0x56, 0xe3, 0xfa, 0xf1, 0x9f, 0xc4, 0x6a, 0xda, 0x09, 0x8d, 0xeb,
    0x9c, 0x32, 0xb1, 0xfd, 0x86, 0x62, 0x05, 0x16, 0x5f, 0x49, 0xb8, 0x00,
  ],
  [
    0x5f, 0x9c, 0x95, 0xbc, 0xa3, 0x50, 0x8c, 0x24, 0xb1, 0xd0, 0xb1, 0x55, 0x9c, 0x83, 0xef, 0x5b, 0x04, 0x44, 0x5c, 0xc4,
    0x58, 0x1c, 0x8e, 0x86, 0xd8, 0x22, 0x4e, 0xdd, 0xd0, 0x9f, 0x11, 0x57,
  ],
  [0xec, ...new Array<number>(30).fill(0xff), 0x7f],
  [0xed, ...new Array<number>(30).fill(0xff), 0x7f],
  [0xee, ...new Array<number>(30).fill(0xff), 0x7f],
];

/** True for a public key of small order (in any encoding X25519 accepts), or not 32 bytes. */
export function isLowOrderKey(pk: Uint8Array): boolean {
  if (pk.length !== KEY_BYTES) return true;
  return LOW_ORDER.some((p) => p.every((b, i) => b === (i === 31 ? pk[i]! & 0x7f : pk[i]!)));
}

export class DeviceKeys {
  readonly #secret: Uint8Array;
  readonly publicKey: Uint8Array;
  /** Precomputed shared keys per peer public key (base64). */
  readonly #shared = new Map<string, Uint8Array>();

  private constructor(secret: Uint8Array) {
    if (secret.length !== KEY_BYTES) throw new Error("device secret key must be 32 bytes");
    this.#secret = Uint8Array.from(secret);
    this.publicKey = nacl.box.keyPair.fromSecretKey(this.#secret).publicKey;
  }

  /** A fresh key pair from the injected CSPRNG. */
  static generate(random: RandomBytes): DeviceKeys {
    const bytes = random(KEY_BYTES);
    const keys = new DeviceKeys(bytes);
    bytes.fill(0);
    return keys;
  }

  /** Rebuild from a stored secret (32 bytes). */
  static fromSecret(secret: Uint8Array): DeviceKeys {
    return new DeviceKeys(secret);
  }

  /** Base64 (standard, padded): `Device.publicKey`, envelope `pk`. */
  get publicB64(): string {
    return encode(this.publicKey);
  }

  /** Base64url unpadded (43 chars): this device's relay address. */
  get address(): string {
    return encodeUrl(this.publicKey);
  }

  /** The secret, for the secure-storage adapter only. The caller must not keep or log it. */
  exportSecret(): Uint8Array {
    return Uint8Array.from(this.#secret);
  }

  /** True if `pk` is this device's own public key. */
  isOwn(pk: Uint8Array): boolean {
    return bytesEqual(pk, this.publicKey);
  }

  /**
   * True for a low-order (or otherwise degenerate) peer key: the X25519 shared secret is all zero, so anyone
   * could forge boxes "from" it. Such keys are never accepted.
   */
  isLowOrder(pk: Uint8Array): boolean {
    if (isLowOrderKey(pk)) return true;
    const s = nacl.scalarMult(this.#secret, pk);
    let acc = 0;
    for (const x of s) acc |= x;
    return acc === 0;
  }

  #sharedWith(pk: Uint8Array): Uint8Array {
    const k = encode(pk);
    let s = this.#shared.get(k);
    if (!s) {
      s = nacl.box.before(pk, this.#secret);
      if (this.#shared.size > 128) this.#shared.clear();
      this.#shared.set(k, s);
    }
    return s;
  }

  /** NaCl box from this device to `pk`. */
  box(plaintext: Uint8Array, nonce: Uint8Array, pk: Uint8Array): Uint8Array {
    if (nonce.length !== NONCE_BYTES || pk.length !== KEY_BYTES) throw new Error("bad nonce or key length");
    return nacl.box.after(plaintext, nonce, this.#sharedWith(pk));
  }

  /** Open a NaCl box from `pk` to this device; null when it does not authenticate. */
  open(ciphertext: Uint8Array, nonce: Uint8Array, pk: Uint8Array): Uint8Array | null {
    if (nonce.length !== NONCE_BYTES || pk.length !== KEY_BYTES || ciphertext.length < TAG_BYTES) return null;
    return nacl.box.open.after(ciphertext, nonce, this.#sharedWith(pk));
  }

  /** Never print the secret. */
  toJSON(): { publicKey: string } {
    return { publicKey: this.publicB64 };
  }

  toString(): string {
    return `DeviceKeys(${this.publicB64})`;
  }
}
