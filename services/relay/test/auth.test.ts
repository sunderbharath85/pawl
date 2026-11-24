import { describe, expect, test } from "bun:test";
import nacl from "tweetnacl";
import { RELAY_AUTH_CONTEXT } from "@krypto/protocol";
import { decodeStd, encodeStd, randomBytes, utf8 } from "../src/b64";
import {
  NONCE_DOMAIN_BYTE,
  answerChallenge,
  authPlaintext,
  challengeNonce,
  createChallenge,
  isChallengeNonce,
  verifyAuth,
} from "../src/auth";

const HOST = "relay.example.com";

function device() {
  return nacl.box.keyPair.fromSecretKey(randomBytes(32));
}

describe("challenge", () => {
  test("fresh key and nonce per connection", () => {
    const a = createChallenge();
    const b = createChallenge();
    expect(a.frame.t).toBe("challenge");
    expect(a.frame.v).toBe(1);
    expect(decodeStd(a.frame.nonce)?.length).toBe(24);
    expect(decodeStd(a.frame.relayPk)?.length).toBe(32);
    expect(a.frame.nonce).not.toBe(b.frame.nonce);
    expect(a.frame.relayPk).not.toBe(b.frame.relayPk);
    // The frame never carries the secret.
    expect(JSON.stringify(a.frame)).not.toContain(a.pending.relaySk);
  });

  test("plaintext layout", () => {
    expect(authPlaintext("NONCE", "h:1")).toBe(`${RELAY_AUTH_CONTEXT}:NONCE:h:1`);
  });
});

describe("verifyAuth", () => {
  test("accepts the right answer", () => {
    const d = device();
    const c = createChallenge();
    const box = answerChallenge(c.frame, HOST, d.secretKey);
    expect(verifyAuth(box, c.pending, HOST, d.publicKey)).toBe(true);
  });

  test("rejects another key, host, nonce or relay key", () => {
    const d = device();
    const other = device();
    const c = createChallenge();
    const c2 = createChallenge();
    // Box made with a different device key, checked against d's public key.
    expect(verifyAuth(answerChallenge(c.frame, HOST, other.secretKey), c.pending, HOST, d.publicKey)).toBe(false);
    // Wrong host (a relay replaying another relay's challenge).
    expect(verifyAuth(answerChallenge(c.frame, "evil.example", d.secretKey), c.pending, HOST, d.publicKey)).toBe(false);
    // Answer to a different challenge.
    expect(verifyAuth(answerChallenge(c2.frame, HOST, d.secretKey), c.pending, HOST, d.publicKey)).toBe(false);
  });

  test("rejects a right box over the wrong text", () => {
    const d = device();
    const c = createChallenge();
    const nonce = decodeStd(c.frame.nonce)!;
    const relayPk = decodeStd(c.frame.relayPk)!;
    const box = encodeStd(nacl.box(utf8(`${RELAY_AUTH_CONTEXT}:${c.frame.nonce}:${HOST}x`), nonce, relayPk, d.secretKey));
    expect(verifyAuth(box, c.pending, HOST, d.publicKey)).toBe(false);
  });

  test("rejects garbage and tampering", () => {
    const d = device();
    const c = createChallenge();
    const box = answerChallenge(c.frame, HOST, d.secretKey);
    const raw = decodeStd(box)!;
    raw[0] = raw[0]! ^ 1;
    expect(verifyAuth(encodeStd(raw), c.pending, HOST, d.publicKey)).toBe(false);
    expect(verifyAuth("", c.pending, HOST, d.publicKey)).toBe(false);
    expect(verifyAuth("@@@@", c.pending, HOST, d.publicKey)).toBe(false);
    expect(verifyAuth(encodeStd(randomBytes(8)), c.pending, HOST, d.publicKey)).toBe(false);
    expect(verifyAuth(box, c.pending, HOST, d.publicKey.subarray(0, 31))).toBe(false);
  });

  test("rejects low-order device keys (forgeable shared secret)", () => {
    const zero = new Uint8Array(32);
    const c = createChallenge();
    // Anyone can compute the box key for an all-zero shared secret: forge with the zero point.
    const nonce = decodeStd(c.frame.nonce)!;
    const k = nacl.box.before(zero, randomBytes(32)); // scalarmult(any, 0) = 0 -> same key for everyone
    const forged = encodeStd(nacl.box.after(utf8(authPlaintext(c.frame.nonce, HOST)), nonce, k));
    expect(verifyAuth(forged, c.pending, HOST, zero)).toBe(false);
  });
});

describe("nonce domain separation (challenge vs envelope nonces)", () => {
  // Envelope nonce layout: 15 random || dir (0/1) || seq u64 BE. Byte 15 of a challenge nonce is always >= 2.
  const envelopeNonce = (dir: 0 | 1, seq: bigint) => {
    const n = randomBytes(24);
    n[15] = dir;
    new DataView(n.buffer).setBigUint64(16, seq);
    return n;
  };

  test("byte 15 of every challenge nonce is in 2..=255", () => {
    const seen = new Set<number>();
    for (let i = 0; i < 2000; i++) {
      const b = decodeStd(createChallenge().frame.nonce)![NONCE_DOMAIN_BYTE]!;
      expect(b).toBeGreaterThanOrEqual(2);
      seen.add(b);
    }
    expect(seen.size).toBeGreaterThan(200); // still spread over the range
  });

  test("the mapping covers every random byte value and keeps the other 23 bytes", () => {
    for (let v = 0; v < 256; v++) {
      const src = new Uint8Array(24).fill(v);
      const n = challengeNonce(() => Uint8Array.from(src));
      expect(n[NONCE_DOMAIN_BYTE]).toBe(2 + (v % 254));
      for (let i = 0; i < 24; i++) if (i !== NONCE_DOMAIN_BYTE) expect(n[i]).toBe(v);
    }
  });

  test("a challenge nonce never equals an envelope nonce", () => {
    expect(isChallengeNonce(envelopeNonce(0, 1n))).toBe(false);
    expect(isChallengeNonce(envelopeNonce(1, 1_700_000_000_000_000n))).toBe(false);
    expect(isChallengeNonce(challengeNonce())).toBe(true);
    expect(isChallengeNonce(new Uint8Array(23).fill(9))).toBe(false);
  });

  test("devices refuse a challenge in the envelope domain (malicious relay)", () => {
    const d = device();
    // A malicious relay could send a peer's public key as relayPk to make the device box under that shared key.
    const peer = device();
    for (const dir of [0, 1] as const) {
      const evil = { nonce: encodeStd(envelopeNonce(dir, 42n)), relayPk: encodeStd(peer.publicKey) };
      expect(() => answerChallenge(evil, HOST, d.secretKey)).toThrow(/envelope domain/);
    }
  });

  test("the relay never verifies against an envelope-domain nonce", () => {
    const d = device();
    const relay = device();
    const nonce = envelopeNonce(1, 7n);
    const n64 = encodeStd(nonce);
    const box = encodeStd(nacl.box(utf8(authPlaintext(n64, HOST)), nonce, relay.publicKey, d.secretKey));
    expect(verifyAuth(box, { nonce: n64, relaySk: encodeStd(relay.secretKey) }, HOST, d.publicKey)).toBe(false);
  });
});
