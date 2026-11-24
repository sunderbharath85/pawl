// b64 (ported RFC 4648 / strictness cases from crates/peer/src/b64.rs), keys, envelope seal/open, nonce layout,
// replay-by-seq-bump and wrong-direction rejection, relay auth answer.
import { describe, expect, test } from "bun:test";
import nacl from "tweetnacl";
import { RELAY_AUTH_CONTEXT, type PeerBody } from "@krypto/protocol";
import {
  authAnswer,
  B64Error,
  decode,
  decodeUrl,
  DeviceKeys,
  direction,
  encode,
  encodeUrl,
  isLowOrderKey,
  makeNonce,
  open,
  seal,
  sealPlaintext,
  seqBytes,
  utf8Decode,
  utf8Encode,
} from "../src/lib/peer";
import { seededRandom } from "./helpers";

const VECTORS: [string, string][] = [
  ["", ""],
  ["f", "Zg=="],
  ["fo", "Zm8="],
  ["foo", "Zm9v"],
  ["foob", "Zm9vYg=="],
  ["fooba", "Zm9vYmE="],
  ["foobar", "Zm9vYmFy"],
];

describe("b64", () => {
  test("RFC 4648 standard and url", () => {
    for (const [plain, enc] of VECTORS) {
      expect(encode(utf8Encode(plain))).toBe(enc);
      expect(utf8Decode(decode(enc))).toBe(plain);
      const unpadded = enc.replace(/=+$/, "");
      expect(encodeUrl(utf8Encode(plain))).toBe(unpadded);
      expect(utf8Decode(decodeUrl(unpadded))).toBe(plain);
    }
  });

  test("alphabets differ in 62/63", () => {
    const b = Uint8Array.from([0xfb, 0xff, 0xbf]);
    expect(encode(b)).toBe("+/+/");
    expect(encodeUrl(b)).toBe("-_-_");
    expect(() => decode("-_-_")).toThrow(B64Error);
    expect(() => decodeUrl("+/+/")).toThrow(B64Error);
  });

  test("round trip all lengths, same as Buffer", () => {
    const data = Uint8Array.from({ length: 256 }, (_, i) => i);
    for (let n = 0; n < data.length; n++) {
      const d = data.subarray(0, n);
      expect(encode(d)).toBe(Buffer.from(d).toString("base64"));
      expect(decode(encode(d))).toEqual(d);
      expect(decodeUrl(encodeUrl(d))).toEqual(d);
    }
  });

  test("rejects malformed and non-canonical", () => {
    for (const bad of ["Zg=", "Zg", "Z===", "Zg=a", "Zh==", "Zm9=", "Zm9v\n", "Zm 9"]) expect(() => decode(bad)).toThrow(B64Error);
    for (const bad of ["Zg==", "Z", "Zh", "Zm9vY"]) expect(() => decodeUrl(bad)).toThrow(B64Error);
  });

  test("strict UTF-8", () => {
    expect(utf8Decode(utf8Encode("héllo 🐶"))).toBe("héllo 🐶");
    expect(utf8Decode(Uint8Array.from([0xc0, 0x80]))).toBeNull(); // overlong
    expect(utf8Decode(Uint8Array.from([0xed, 0xa0, 0x80]))).toBeNull(); // surrogate
    expect(utf8Decode(Uint8Array.from([0xe2, 0x82]))).toBeNull(); // truncated
    expect(Buffer.from(utf8Encode("añ€𐍈")).toString("utf8")).toBe("añ€𐍈");
  });
});

describe("keys", () => {
  test("generate / fromSecret / no secret in JSON", () => {
    const r = seededRandom(5);
    const a = DeviceKeys.generate(r);
    const b = DeviceKeys.fromSecret(a.exportSecret());
    expect(b.publicB64).toBe(a.publicB64);
    expect(a.address.length).toBe(43);
    expect(JSON.stringify(a)).toBe(JSON.stringify({ publicKey: a.publicB64 }));
    expect(String(a)).not.toContain(Buffer.from(a.exportSecret()).toString("hex"));
  });

  test("low-order keys", () => {
    expect(isLowOrderKey(new Uint8Array(32))).toBe(true);
    const one = new Uint8Array(32);
    one[0] = 1;
    expect(isLowOrderKey(one)).toBe(true);
    const pMinus1 = new Uint8Array(32).fill(0xff);
    pMinus1[0] = 0xec;
    pMinus1[31] = 0xff; // bit 255 set: still low order
    expect(isLowOrderKey(pMinus1)).toBe(true);
    const k = DeviceKeys.generate(seededRandom(8));
    expect(isLowOrderKey(k.publicKey)).toBe(false);
    expect(k.isLowOrder(new Uint8Array(32))).toBe(true);
    expect(k.isLowOrder(DeviceKeys.generate(seededRandom(9)).publicKey)).toBe(false);
  });
});

const body: PeerBody = { type: "bye", reason: "shutdown", ts: 42 };

describe("envelope", () => {
  const random = seededRandom(17);
  const a = DeviceKeys.generate(random);
  const b = DeviceKeys.generate(random);

  test("nonce layout: 15 random || dir || seq u64 BE", () => {
    const prefix = Uint8Array.from({ length: 15 }, (_, i) => i + 1);
    const n = makeNonce(a.publicKey, b.publicKey, 0x0102030405, prefix);
    expect([...n.subarray(0, 15)]).toEqual([...prefix]);
    expect(n[15]).toBe(direction(a.publicKey, b.publicKey));
    expect([...n.subarray(16)]).toEqual([0, 0, 0, 1, 2, 3, 4, 5]);
    expect([...seqBytes(1_800_000_000_000_000)]).toEqual([...Buffer.from((1_800_000_000_000_000n).toString(16).padStart(16, "0"), "hex")]);
    expect(direction(a.publicKey, b.publicKey) + direction(b.publicKey, a.publicKey)).toBe(1);
  });

  test("seal / open round trip; pk only when asked; random prefix differs", () => {
    const env = seal(a, "aaaaaaaaaaaa", "bbbbbbbbbbbb", b.publicKey, 7, body, random);
    expect(env.v).toBe(1);
    expect(env.seq).toBe(7);
    expect(env.pk).toBeUndefined();
    const r = open(b, env, a.publicKey);
    expect(r.ok && r.body).toEqual(body);
    const env2 = seal(a, "aaaaaaaaaaaa", "bbbbbbbbbbbb", b.publicKey, 8, body, random, { includePk: true });
    expect(env2.pk).toBe(a.publicB64);
    const env3 = seal(a, "aaaaaaaaaaaa", "bbbbbbbbbbbb", b.publicKey, 8, body, random, { includePk: true });
    expect(env3.nonce).not.toBe(env2.nonce);
  });

  test("is a plain NaCl box (opens with tweetnacl directly, and the other way round)", () => {
    const env = seal(a, "a", "b", b.publicKey, 3, body, random);
    const pt = nacl.box.open(decode(env.box), decode(env.nonce), a.publicKey, b.exportSecret());
    expect(utf8Decode(pt!)).toBe(JSON.stringify(body));
    // Independently built with tweetnacl and the layout: open accepts it.
    const nonce = new Uint8Array(24).fill(0x55);
    nonce[15] = direction(a.publicKey, b.publicKey);
    nonce.set(seqBytes(3), 16);
    const ct = nacl.box(utf8Encode(JSON.stringify(body)), nonce, b.publicKey, a.exportSecret());
    const r = open(b, { v: 1, from: "a", to: "b", seq: 3, nonce: encode(nonce), box: encode(ct) }, a.publicKey);
    expect(r.ok && r.body).toEqual(body);
  });

  test("seq-bumped replay, tamper, renonce, version and short nonce fail", () => {
    const env = seal(a, "a", "b", b.publicKey, 5, body, random);
    expect(open(b, { ...env, seq: 6 }, a.publicKey)).toEqual({ ok: false, error: "bad_nonce" });
    const ct = decode(env.box);
    ct[3]! ^= 1;
    expect(open(b, { ...env, box: encode(ct) }, a.publicKey)).toEqual({ ok: false, error: "decrypt_failed" });
    const n = decode(env.nonce);
    n[0]! ^= 1;
    expect(open(b, { ...env, nonce: encode(n) }, a.publicKey)).toEqual({ ok: false, error: "decrypt_failed" });
    expect(open(b, { ...env, v: 2 as 1 }, a.publicKey)).toEqual({ ok: false, error: "bad_version" });
    expect(open(b, { ...env, nonce: encode(new Uint8Array(23)) }, a.publicKey)).toEqual({ ok: false, error: "bad_encoding" });
  });

  test("a frame reflected back to its sender fails (direction byte)", () => {
    const env = seal(a, "a", "b", b.publicKey, 9, body, random);
    expect(open(a, { ...env, from: "b", to: "a" }, b.publicKey)).toEqual({ ok: false, error: "bad_nonce" });
  });

  test("wrong key: third party cannot open", () => {
    const c = DeviceKeys.generate(random);
    const env = seal(a, "a", "b", b.publicKey, 1, body, random);
    expect(open(c, env, a.publicKey).ok).toBe(false);
    const wrong = open(b, env, c.publicKey);
    expect(wrong.ok).toBe(false);
  });

  test("non-JSON / unknown body type is bad_body", () => {
    const env = sealPlaintext(a, "a", "b", b.publicKey, 2, utf8Encode('{"type":"nope"}'), random);
    expect(open(b, env, a.publicKey)).toEqual({ ok: false, error: "bad_body" });
  });
});

describe("relay auth", () => {
  const random = seededRandom(23);
  const device = DeviceKeys.generate(random);
  const relay = DeviceKeys.generate(random);

  test("answer opens on the relay with the bound host", () => {
    const nonce = new Uint8Array(24).fill(7);
    const n = encode(nonce);
    const ans = authAnswer(device, n, relay.publicB64, "127.0.0.1:8787");
    expect(ans.ok).toBe(true);
    if (!ans.ok) return;
    const pt = nacl.box.open(decode(ans.box), nonce, device.publicKey, relay.exportSecret());
    expect(utf8Decode(pt!)).toBe(`${RELAY_AUTH_CONTEXT}:${n}:127.0.0.1:8787`);
    expect(authAnswer(device, "short", relay.publicB64, "h")).toEqual({ ok: false, error: "bad_frame" });
    expect(authAnswer(device, n, encode(new Uint8Array(31)), "h")).toEqual({ ok: false, error: "bad_frame" });
  });

  test("refuses nonces in the envelope domain (byte 15 < 2)", () => {
    for (const dir of [0, 1]) {
      const n = new Uint8Array(24).fill(9);
      n[15] = dir;
      expect(authAnswer(device, encode(n), relay.publicB64, "h")).toEqual({ ok: false, error: "bad_auth" });
    }
    for (const dir of [2, 255]) {
      const n = new Uint8Array(24).fill(9);
      n[15] = dir;
      expect(authAnswer(device, encode(n), relay.publicB64, "h").ok).toBe(true);
    }
    // A real envelope nonce sits in the refused domain.
    const env = seal(device, "aaaaaaaaaaaa", "bbbbbbbbbbbb", relay.publicKey, 5, { type: "sync_request", sinceSeq: 0 }, random);
    expect(decode(env.nonce)[15]!).toBeLessThan(2);
    expect(authAnswer(device, env.nonce, relay.publicB64, "h")).toEqual({ ok: false, error: "bad_auth" });
  });

  test("refuses our own key and any roster key as relayPk", () => {
    const n = encode(new Uint8Array(24).fill(9));
    expect(authAnswer(device, n, device.publicB64, "h")).toEqual({ ok: false, error: "bad_auth" });
    const peer = DeviceKeys.generate(random);
    expect(authAnswer(device, n, peer.publicB64, "h", [peer.publicKey])).toEqual({ ok: false, error: "bad_auth" });
  });
});
