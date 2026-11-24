import { describe, expect, test } from "bun:test";
import { encodeUrl, randomBytes } from "../src/b64";
import { accessKeyMatches, handleRequest, isValidAccessKey, type WorkerDeps } from "../src/gate";

const KEY = encodeUrl(randomBytes(32));
const PK = encodeUrl(randomBytes(32));

function deps(over: Partial<WorkerDeps> = {}, allow = true) {
  const calls = { limit: [] as string[], forward: [] as Array<[string, string]> };
  const d: WorkerDeps = {
    accessKey: KEY,
    open: undefined,
    limiter: {
      async limit({ key }) {
        calls.limit.push(key);
        return { success: allow };
      },
    },
    async forward(pk, host) {
      calls.forward.push([pk, host]);
      return new Response("forwarded", { status: 200 });
    },
    ...over,
  };
  return { d, calls };
}

function connect(query: string, headers: Record<string, string> = {}): Request {
  return new Request(`https://relay.test/v1/connect?${query}`, {
    headers: { upgrade: "websocket", "cf-connecting-ip": "203.0.113.7", ...headers },
  });
}

describe("routes", () => {
  test("health, 404, 426", async () => {
    const { d, calls } = deps();
    const h = await handleRequest(new Request("https://relay.test/health"), d);
    expect(h.status).toBe(200);
    expect(await h.json()).toEqual({ ok: true });
    expect((await handleRequest(new Request("https://relay.test/health", { method: "POST" }), d)).status).toBe(405);
    expect((await handleRequest(new Request("https://relay.test/other"), d)).status).toBe(404);
    expect((await handleRequest(new Request(`https://relay.test/v1/connect?pk=${PK}&k=${KEY}`), d)).status).toBe(426);
    expect(calls.limit.length + calls.forward.length).toBe(0);
  });

  test("valid connect is forwarded with pk and host only", async () => {
    const { d, calls } = deps();
    const r = await handleRequest(connect(`pk=${PK}&k=${KEY}`), d);
    expect(r.status).toBe(200);
    expect(calls.forward).toEqual([[PK, "relay.test"]]);
    expect(calls.limit).toEqual(["connect:203.0.113.7"]);
  });
});

describe("(A) access key", () => {
  test("missing, wrong, repeated or empty key -> 401, no mailbox touched", async () => {
    const { d, calls } = deps();
    const other = encodeUrl(randomBytes(32));
    for (const q of [`pk=${PK}`, `pk=${PK}&k=${other}`, `pk=${PK}&k=${KEY}&k=${KEY}`, `pk=${PK}&k=`, `pk=${PK}&k=${KEY}x`]) {
      expect((await handleRequest(connect(q), d)).status).toBe(401);
    }
    expect(calls.forward).toEqual([]);
  });

  test("the key is checked before the pk (a wrong key never reaches idFromName)", async () => {
    const { d, calls } = deps();
    expect((await handleRequest(connect(`pk=bad&k=nope`), d)).status).toBe(401);
    expect((await handleRequest(connect(`pk=bad&k=${KEY}`), d)).status).toBe(400);
    expect(calls.forward).toEqual([]);
  });

  test("secret unset -> 503 unless RELAY_OPEN = 1", async () => {
    const unset = deps({ accessKey: undefined });
    expect((await handleRequest(connect(`pk=${PK}`), unset.d)).status).toBe(503);
    const empty = deps({ accessKey: "" });
    expect((await handleRequest(connect(`pk=${PK}`), empty.d)).status).toBe(503);
    const notOne = deps({ accessKey: undefined, open: "true" });
    expect((await handleRequest(connect(`pk=${PK}`), notOne.d)).status).toBe(503);
    expect(unset.calls.forward.length + unset.calls.limit.length).toBe(0);

    const open = deps({ accessKey: undefined, open: "1" });
    expect((await handleRequest(connect(`pk=${PK}`), open.d)).status).toBe(200);
    expect(open.calls.forward.length).toBe(1);
  });

  test("a key that is set still applies when RELAY_OPEN = 1", async () => {
    const { d } = deps({ open: "1" });
    expect((await handleRequest(connect(`pk=${PK}`), d)).status).toBe(401);
  });

  test("a malformed configured key fails closed", async () => {
    const { d, calls } = deps({ accessKey: "short" });
    expect((await handleRequest(connect(`pk=${PK}&k=short`), d)).status).toBe(503);
    expect(calls.forward).toEqual([]);
  });

  test("key helpers", () => {
    expect(isValidAccessKey(KEY)).toBe(true);
    expect(isValidAccessKey(KEY.slice(1))).toBe(false);
    expect(isValidAccessKey(encodeUrl(randomBytes(16)))).toBe(false);
    expect(accessKeyMatches(KEY, KEY)).toBe(true);
    expect(accessKeyMatches(undefined, KEY)).toBe(false);
    expect(accessKeyMatches(KEY.slice(0, 42) + (KEY[42] === "A" ? "Q" : "A"), KEY)).toBe(false);
  });
});

describe("(1) per-IP connect limit", () => {
  test("over budget -> 429 before the key, the pk or any mailbox", async () => {
    const { d, calls } = deps({}, false);
    const r = await handleRequest(connect(`pk=${PK}&k=${KEY}`), d);
    expect(r.status).toBe(429);
    expect(r.headers.get("retry-after")).toBe("60");
    expect((await handleRequest(connect(`pk=bad&k=wrong`), d)).status).toBe(429);
    expect(calls.forward).toEqual([]);
  });

  test("keyed by client IP", async () => {
    const { d, calls } = deps();
    await handleRequest(connect(`pk=${PK}&k=${KEY}`, { "cf-connecting-ip": "2001:db8::1" }), d);
    await handleRequest(new Request(`https://relay.test/v1/connect?pk=${PK}&k=${KEY}`, { headers: { upgrade: "websocket" } }), d);
    expect(calls.limit).toEqual(["connect:2001:db8::1", "connect:unknown"]);
  });

  test("limiter failure fails closed", async () => {
    const { d, calls } = deps({
      limiter: {
        async limit() {
          throw new Error("binding down");
        },
      },
    });
    expect((await handleRequest(connect(`pk=${PK}&k=${KEY}`), d)).status).toBe(503);
    expect(calls.forward).toEqual([]);
  });
});

describe("pk and host", () => {
  test("bad or repeated pk -> 400", async () => {
    const { d, calls } = deps();
    for (const q of [`k=${KEY}`, `pk=short&k=${KEY}`, `pk=${PK}&pk=${PK}&k=${KEY}`]) {
      expect((await handleRequest(connect(q), d)).status).toBe(400);
    }
    expect(calls.forward).toEqual([]);
  });
});
