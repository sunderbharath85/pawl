// Cross-language test vectors committed by sde-peer (crates/peer/tests/vectors/*.json, read-only): the phone must
// seal and open byte-identically to the Rust side.
import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import type { PeerBody, PeerEnvelope } from "@krypto/protocol";
import {
  authAnswer,
  checkIntroDevice,
  decode,
  DEVICE_NAME_MAX,
  deviceNameOk,
  decodeUrl,
  DeviceKeys,
  direction,
  encode,
  encodeBody,
  encodeUrl,
  fromHex,
  open,
  parseInvite,
  relayConnectUrl,
  relayHostHeader,
  sealPlaintext,
  toHex,
  tryParseInvite,
  utf8Decode,
  utf8Encode,
} from "../src/lib/peer";

const DIR = join(import.meta.dir, "../../../crates/peer/tests/vectors");
const load = <T>(name: string): T => JSON.parse(readFileSync(join(DIR, name), "utf8")) as T;

interface KeyVec {
  id: string | null;
  name: string;
  publicKey: string;
  publicKeyHex: string;
  relayAddress: string;
  secretKeyHex: string;
}
const keysFile = load<{ keys: KeyVec[] }>("keys.json");
const KEYS = new Map(keysFile.keys.map((k) => [k.name, k]));
const dk = (name: string) => DeviceKeys.fromSecret(fromHex(KEYS.get(name)!.secretKeyHex)!);

describe("vectors: keys", () => {
  test("public key, base64 and relay address derive from the secret", () => {
    for (const k of keysFile.keys) {
      const keys = DeviceKeys.fromSecret(fromHex(k.secretKeyHex)!);
      expect(toHex(keys.publicKey)).toBe(k.publicKeyHex);
      expect(keys.publicB64).toBe(k.publicKey);
      expect(keys.address).toBe(k.relayAddress);
    }
  });
});

describe("vectors: base64", () => {
  const v = load<{ samples: { hex: string; std: string; url: string }[]; invalidStd: string[]; invalidUrl: string[] }>("base64.json");
  test("samples encode and decode both ways", () => {
    for (const s of v.samples) {
      const bytes = fromHex(s.hex)!;
      expect(encode(bytes)).toBe(s.std);
      expect(encodeUrl(bytes)).toBe(s.url);
      expect(toHex(decode(s.std))).toBe(s.hex);
      expect(toHex(decodeUrl(s.url))).toBe(s.hex);
    }
  });
  test("invalid strings are refused", () => {
    for (const s of v.invalidStd) expect(() => decode(s)).toThrow();
    for (const s of v.invalidUrl) expect(() => decodeUrl(s)).toThrow();
  });
});

interface EnvVec {
  name: string;
  sender: string;
  receiver: string;
  seq: number;
  dir: number;
  randomHex: string;
  nonceHex: string;
  plaintext: string;
  body: PeerBody;
  envelope: PeerEnvelope;
}
interface RejectVec {
  name: string;
  sender: string;
  receiver: string;
  error: string;
  envelope: PeerEnvelope;
  lastRxSeq?: number;
}
const envFile = load<{ vectors: EnvVec[]; rejects: RejectVec[]; replay: RejectVec }>("envelopes.json");

describe("vectors: envelopes", () => {
  test(`seal reproduces all ${envFile.vectors.length} envelopes byte for byte`, () => {
    expect(envFile.vectors.length).toBeGreaterThanOrEqual(12);
    for (const v of envFile.vectors) {
      const sender = dk(v.sender);
      const receiver = dk(v.receiver);
      expect(direction(sender.publicKey, receiver.publicKey)).toBe(v.dir);
      const env = sealPlaintext(sender, v.envelope.from, v.envelope.to, receiver.publicKey, v.seq, utf8Encode(v.plaintext), () => {
        throw new Error("vectors fix the nonce");
      }, { noncePrefix: fromHex(v.randomHex)!, includePk: v.envelope.pk !== undefined });
      expect(toHex(decode(env.nonce))).toBe(v.nonceHex);
      // Every field byte-identical (the vector file lists keys sorted; order on the wire does not matter).
      expect(env).toEqual(v.envelope);
    }
  });

  test("open gives the body; re-encoding the body gives the same plaintext", () => {
    for (const v of envFile.vectors) {
      const r = open(dk(v.receiver), v.envelope, dk(v.sender).publicKey);
      expect(r.ok).toBe(true);
      if (!r.ok) continue;
      expect(r.body).toEqual(v.body);
      expect(r.plaintext).toBe(v.plaintext);
      expect(new TextDecoder().decode(encodeBody(r.body))).toBe(v.plaintext);
    }
  });

  test("rejects carry the Rust OpenError codes", () => {
    expect(envFile.rejects.length).toBeGreaterThanOrEqual(7);
    for (const r of envFile.rejects) {
      const res = open(dk(r.receiver), r.envelope, dk(r.sender).publicKey);
      expect(res.ok).toBe(false);
      if (!res.ok) expect(`${r.name}: ${res.error}`).toBe(`${r.name}: ${r.error}`);
    }
  });

  test("replay: dropped when seq <= last accepted (before opening)", () => {
    const r = envFile.replay;
    expect(r.envelope.seq <= r.lastRxSeq!).toBe(true);
    // The frame itself is authentic; only the seq rule refuses it.
    expect(open(dk(r.receiver), r.envelope, dk(r.sender).publicKey).ok).toBe(true);
  });
});

interface InviteVec {
  name: string;
  uri: string;
  machineId: string;
  publicKey: string;
  addr: string | null;
  code: string;
  expiresAt: number;
  relay: string | null;
  relayToken: string | null;
  relayKey: string | null;
}

describe("vectors: invites", () => {
  const v = load<{ valid: InviteVec[]; invalid: { name: string; uri: string }[] }>("invites.json");
  test("valid invites parse to the given fields", () => {
    expect(v.valid.length).toBeGreaterThanOrEqual(5);
    for (const i of v.valid) {
      const inv = parseInvite(i.uri);
      expect({ ...inv, publicKey: encode(inv.publicKey) }).toEqual({
        machineId: i.machineId,
        publicKey: i.publicKey,
        addr: i.addr,
        code: i.code,
        expiresAt: i.expiresAt,
        relay: i.relay,
        relayToken: i.relayToken,
        relayKey: i.relayKey,
      });
    }
  });
  test("invalid invites are refused", () => {
    expect(v.invalid.length).toBeGreaterThanOrEqual(14);
    for (const i of v.invalid) expect(`${i.name}: ${tryParseInvite(i.uri) === null}`).toBe(`${i.name}: true`);
  });
});

describe("vectors: relay auth", () => {
  const v = load<{
    valid: {
      name: string;
      device: string;
      host: string;
      relayUrl: string;
      accessKey: string;
      connectUrl: string;
      plaintext: string;
      challenge: { nonce: string; relayPk: string };
      auth: { box: string };
    }[];
    refused: { name: string; device: string; host: string; nonce: string; relayPk: string; error: string }[];
  }>("relay_auth.json");
  test("auth answers and connect URLs match", () => {
    for (const a of v.valid) {
      const keys = dk(a.device);
      expect(relayHostHeader(a.relayUrl)).toBe(a.host);
      expect(relayConnectUrl(a.relayUrl, keys.address, a.accessKey)).toBe(a.connectUrl);
      const ans = authAnswer(keys, a.challenge.nonce, a.challenge.relayPk, a.host);
      expect(ans).toEqual({ ok: true, box: a.auth.box });
    }
  });
  test("refused challenges", () => {
    for (const r of v.refused) {
      expect(authAnswer(dk(r.device), r.nonce, r.relayPk, r.host)).toEqual({ ok: false, error: r.error as "bad_auth" });
    }
  });
});

describe("vectors: names + introduced devices", () => {
  const v = load<{
    maxChars: number;
    valid: { name: string; utf8Hex: string; note: string }[];
    invalid: { name: string; utf8Hex: string; note: string }[];
    introDevices: { device: unknown; note: string; result: string }[];
  }>("names.json");
  const nameOf = (x: { name: string; utf8Hex: string }) => {
    const fromHex = utf8Decode(fromHex_(x.utf8Hex))!;
    expect(fromHex).toBe(x.name);
    return fromHex;
  };
  const fromHex_ = (h: string) => fromHex(h)!;

  test("same max length", () => {
    expect(DEVICE_NAME_MAX).toBe(v.maxChars);
  });
  test("valid names pass", () => {
    expect(v.valid.length).toBeGreaterThanOrEqual(11);
    for (const x of v.valid) expect(`${x.note}: ${deviceNameOk(nameOf(x))}`).toBe(`${x.note}: true`);
  });
  test("invalid names fail", () => {
    expect(v.invalid.length).toBeGreaterThanOrEqual(35);
    for (const x of v.invalid) expect(`${x.note}: ${deviceNameOk(nameOf(x))}`).toBe(`${x.note}: false`);
  });
  test("check_intro_device results (codes in Rust order)", () => {
    expect(v.introDevices.length).toBeGreaterThanOrEqual(21);
    for (const x of v.introDevices) {
      const r = checkIntroDevice(x.device);
      expect(`${x.note}: ${r.ok ? "ok" : r.code}`).toBe(`${x.note}: ${x.result}`);
    }
  });
});
