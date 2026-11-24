import { describe, expect, test } from "bun:test";
import { RELAY_MAX_ALLOWED } from "@krypto/protocol";
import { decodeStd, decodeUrl, encodeStd, encodeUrl, randomBytes } from "../src/b64";
import {
  decodePk,
  errorFrame,
  isValidPk,
  isValidToken,
  messageBytes,
  msgFrame,
  parseClientFrame,
  parseEnvelope,
  utf8Length,
} from "../src/frames";

const pk = () => encodeUrl(randomBytes(32));
const token = () => encodeUrl(randomBytes(16));
const env = (over: Record<string, unknown> = {}) => ({
  v: 1,
  from: "abcdefghijkl",
  to: "mnopqrstuvwx",
  seq: 1_700_000_000_000_000,
  nonce: encodeStd(randomBytes(24)),
  box: encodeStd(randomBytes(64)),
  ...over,
});

describe("b64", () => {
  test("round trips both alphabets", () => {
    for (const n of [0, 1, 2, 3, 16, 24, 32, 100]) {
      const b = randomBytes(n);
      expect(decodeStd(encodeStd(b))).toEqual(b);
      expect(decodeUrl(encodeUrl(b))).toEqual(b);
    }
  });

  test("rejects non-canonical and foreign alphabets", () => {
    const p = pk();
    // The last char of a 43-char pk carries 2 unused bits; flipping them gives the same bytes, a different string.
    const last = p[42]!;
    const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    const idx = alphabet.indexOf(last);
    const alt = p.slice(0, 42) + alphabet[(idx & ~3) | ((idx + 1) & 3)];
    expect(decodeUrl(alt)).toBeNull();
    expect(decodeUrl(p.replace(/./, "+"))).toBeNull();
    expect(decodeStd("abc")).toBeNull();
    expect(decodeStd("ab-_")).toBeNull();
    expect(decodeUrl("a")).toBeNull();
  });
});

describe("pk and token", () => {
  test("valid shapes", () => {
    expect(isValidPk(pk())).toBe(true);
    expect(decodePk(pk())?.length).toBe(32);
    expect(isValidToken(token())).toBe(true);
  });

  test("invalid shapes", () => {
    expect(isValidPk(encodeStd(randomBytes(32)))).toBe(false); // padded standard
    expect(isValidPk(pk().slice(1))).toBe(false);
    expect(isValidPk(encodeUrl(randomBytes(31)))).toBe(false);
    expect(isValidPk(42)).toBe(false);
    expect(isValidToken(pk())).toBe(false);
    expect(isValidToken(token() + "A")).toBe(false);
  });
});

describe("sizes", () => {
  test("utf8Length matches TextEncoder", () => {
    for (const s of ["", "abc", "é", "日本語", "😀x", "\ud800", "a\udc00b"]) {
      expect(utf8Length(s)).toBe(new TextEncoder().encode(s).length);
    }
  });

  test("messageBytes bails out on huge strings", () => {
    expect(messageBytes("x".repeat(10), 5)).toBe(10);
    expect(messageBytes(new ArrayBuffer(7))).toBe(7);
  });
});

describe("parseEnvelope", () => {
  test("keeps only known fields", () => {
    const e = parseEnvelope({ ...env(), extra: { nested: true } });
    expect(e).not.toBeNull();
    expect(Object.keys(e!).sort()).toEqual(["box", "from", "nonce", "seq", "to", "v"]);
    expect(parseEnvelope(env({ pk: encodeStd(randomBytes(32)) }))?.pk).toBeDefined();
  });

  test("rejects bad shapes", () => {
    expect(parseEnvelope(null)).toBeNull();
    expect(parseEnvelope([])).toBeNull();
    expect(parseEnvelope(env({ v: 2 }))).toBeNull();
    expect(parseEnvelope(env({ seq: -1 }))).toBeNull();
    expect(parseEnvelope(env({ seq: 1.5 }))).toBeNull();
    expect(parseEnvelope(env({ seq: 2 ** 60 }))).toBeNull();
    expect(parseEnvelope(env({ from: "" }))).toBeNull();
    expect(parseEnvelope(env({ to: "x".repeat(129) }))).toBeNull();
    expect(parseEnvelope(env({ nonce: "not base64!" }))).toBeNull();
    expect(parseEnvelope(env({ box: "" }))).toBeNull();
    expect(parseEnvelope(env({ box: "abc" }))).toBeNull();
    expect(parseEnvelope(env({ pk: 7 }))).toBeNull();
  });
});

describe("parseClientFrame", () => {
  const ok = (v: unknown) => parseClientFrame(JSON.stringify(v));

  test("accepts every frame type", () => {
    expect(ok({ t: "auth", box: encodeStd(randomBytes(80)) }).ok).toBe(true);
    expect(ok({ t: "allow", pks: [pk(), pk()] }).ok).toBe(true);
    expect(ok({ t: "allow", pks: [] }).ok).toBe(true);
    expect(ok({ t: "allow_pairing", token: token(), exp: Date.now() + 1000 }).ok).toBe(true);
    expect(ok({ t: "send", to: pk(), env: env() }).ok).toBe(true);
    expect(ok({ t: "send", to: pk(), env: env(), pairing: token() }).ok).toBe(true);
    expect(ok({ t: "register_push", expoToken: "ExponentPushToken[abc]" }).ok).toBe(true);
    expect(ok({ t: "ping", ts: 1 }).ok).toBe(true);
  });

  test("dedupes allow and caps it", () => {
    const p = pk();
    const r = ok({ t: "allow", pks: [p, p] });
    expect(r.ok && r.frame.t === "allow" && r.frame.pks).toEqual([p]);
    expect(ok({ t: "allow", pks: Array.from({ length: RELAY_MAX_ALLOWED + 1 }, pk) }).ok).toBe(false);
    expect(ok({ t: "allow", pks: [pk(), "nope"] }).ok).toBe(false);
  });

  test("rejects malformed frames with bad_frame", () => {
    for (const bad of [
      "not json",
      "[]",
      "null",
      JSON.stringify({ t: "nope" }),
      JSON.stringify({ t: 1 }),
      JSON.stringify({ t: "auth" }),
      JSON.stringify({ t: "auth", box: "x".repeat(4096) }),
      JSON.stringify({ t: "allow_pairing", token: token(), exp: "soon" }),
      JSON.stringify({ t: "allow_pairing", token: "short", exp: 1 }),
      JSON.stringify({ t: "send", to: "bad", env: env() }),
      JSON.stringify({ t: "send", to: pk(), env: env({ v: 0 }) }),
      JSON.stringify({ t: "send", to: pk(), env: env(), pairing: "bad" }),
      JSON.stringify({ t: "register_push", expoToken: 5 }),
      JSON.stringify({ t: "ping" }),
    ]) {
      expect(parseClientFrame(bad)).toEqual({ ok: false, code: "bad_frame" });
    }
  });

  test("send rebuilds the envelope", () => {
    const r = ok({ t: "send", to: pk(), env: { ...env(), junk: 1 } });
    expect(r.ok && r.frame.t === "send" && "junk" in r.frame.env).toBe(false);
  });
});

describe("server frames", () => {
  test("error with and without to", () => {
    expect(JSON.parse(errorFrame("bad_auth"))).toEqual({ t: "error", code: "bad_auth" });
    const p = pk();
    expect(JSON.parse(errorFrame("not_allowed", p))).toEqual({ t: "error", code: "not_allowed", to: p });
  });

  test("msg carries from and env", () => {
    const p = pk();
    const e = parseEnvelope(env())!;
    expect(JSON.parse(msgFrame(p, e))).toEqual({ t: "msg", from: p, env: e });
  });
});
