// Challenge-response proof that a device holds the secret key of the public key it connected as.
// Relay: fresh X25519 key + 24-byte nonce per connection -> challenge. Device: nacl.box(utf8(text), nonce,
// relayPk, deviceSk) where text = `${RELAY_AUTH_CONTEXT}:${nonce}:${host}`. Relay: nacl.box.open with
// (devicePk, relaySk) and compare. Binding the Host stops a relay from replaying another relay's challenge.
//
// Nonce domain separation: the device boxes the auth answer with its long-term key, the same key it uses for
// PeerEnvelopes. Envelope nonces are 15 random bytes || dir (0 or 1) || seq (u64 BE), so byte 15 of an envelope
// nonce is always 0 or 1. Challenge nonces always have byte 15 in 2..=255, so an auth answer can never reuse an
// envelope nonce, even if a malicious relay sent a peer's public key as `relayPk` (that would make the device box
// under the device<->peer shared key; a repeated nonce there would leak plaintext and allow forgeries). Devices
// refuse challenges whose byte 15 is 0 or 1 (`answerChallenge` below does).

import nacl from "tweetnacl";
import { RELAY_AUTH_CONTEXT, RELAY_PROTOCOL_VERSION } from "@krypto/protocol";
import { bytesEqual, decodeStd, encodeStd, randomBytes as defaultRandom, utf8 } from "./b64";

/** Index of the envelope nonce's direction byte (0 or 1 in envelopes; 2..=255 in challenges). */
export const NONCE_DOMAIN_BYTE = 15;
/** Smallest byte-15 value a challenge nonce may have. */
export const CHALLENGE_DOMAIN_MIN = 2;

/** True iff `nonce` (24 bytes) is in the challenge domain, i.e. can never equal an envelope nonce. */
export function isChallengeNonce(nonce: Uint8Array): boolean {
  return nonce.length === nacl.box.nonceLength && nonce[NONCE_DOMAIN_BYTE]! >= CHALLENGE_DOMAIN_MIN;
}

/** 24 random bytes with byte 15 mapped into 2..=255 (uniform enough: 254 values from one random byte). */
export function challengeNonce(random: (n: number) => Uint8Array = defaultRandom): Uint8Array {
  const nonce = Uint8Array.from(random(nacl.box.nonceLength));
  nonce[NONCE_DOMAIN_BYTE] = CHALLENGE_DOMAIN_MIN + (nonce[NONCE_DOMAIN_BYTE]! % (256 - CHALLENGE_DOMAIN_MIN));
  return nonce;
}

export interface ChallengeFrame {
  t: "challenge";
  nonce: string;
  relayPk: string;
  v: 1;
}

/** What the relay keeps (in the socket attachment) until the device answers. Never logged or sent. */
export interface PendingChallenge {
  /** Standard base64, 24 bytes. */
  nonce: string;
  /** Standard base64, 32 bytes: the per-connection relay secret key. */
  relaySk: string;
}

export function createChallenge(random: (n: number) => Uint8Array = defaultRandom): {
  frame: ChallengeFrame;
  pending: PendingChallenge;
} {
  const kp = nacl.box.keyPair.fromSecretKey(random(nacl.box.secretKeyLength));
  const nonce = encodeStd(challengeNonce(random));
  return {
    frame: { t: "challenge", nonce, relayPk: encodeStd(kp.publicKey), v: RELAY_PROTOCOL_VERSION },
    pending: { nonce, relaySk: encodeStd(kp.secretKey) },
  };
}

export function authPlaintext(nonce: string, host: string): string {
  return `${RELAY_AUTH_CONTEXT}:${nonce}:${host}`;
}

function allZero(b: Uint8Array): boolean {
  let acc = 0;
  for (const x of b) acc |= x;
  return acc === 0;
}

/**
 * True iff `box` (standard base64) opens with (devicePk, relaySk) under the challenge nonce and the plaintext is
 * exactly the expected auth text for `host`. Low-order device keys (all-zero shared secret, which anyone could
 * forge a box for) are refused.
 */
export function verifyAuth(box: string, pending: PendingChallenge, host: string, devicePk: Uint8Array): boolean {
  if (devicePk.length !== nacl.box.publicKeyLength) return false;
  const ct = decodeStd(box);
  const nonce = decodeStd(pending.nonce);
  const sk = decodeStd(pending.relaySk);
  if (!ct || !nonce || !sk || !isChallengeNonce(nonce) || sk.length !== nacl.box.secretKeyLength) {
    return false;
  }
  if (ct.length < nacl.box.overheadLength) return false;
  if (allZero(nacl.scalarMult(sk, devicePk))) return false;
  const pt = nacl.box.open(ct, nonce, devicePk, sk);
  if (!pt) return false;
  return bytesEqual(pt, utf8(authPlaintext(pending.nonce, host)));
}

/**
 * Device side (used by the smoke script and tests): the `box` of the auth answer, standard base64. Refuses a
 * challenge nonce outside the challenge domain (byte 15 < 2): boxing it with the device key could reuse an envelope
 * nonce.
 */
export function answerChallenge(
  challenge: { nonce: string; relayPk: string },
  host: string,
  deviceSecretKey: Uint8Array,
): string {
  const nonce = decodeStd(challenge.nonce);
  const relayPk = decodeStd(challenge.relayPk);
  if (!nonce || nonce.length !== nacl.box.nonceLength) throw new Error("bad challenge nonce");
  if (!isChallengeNonce(nonce)) throw new Error("challenge nonce in the envelope domain (byte 15 < 2)");
  if (!relayPk || relayPk.length !== nacl.box.publicKeyLength) throw new Error("bad relay key");
  return encodeStd(nacl.box(utf8(authPlaintext(challenge.nonce, host)), nonce, relayPk, deviceSecretKey));
}
