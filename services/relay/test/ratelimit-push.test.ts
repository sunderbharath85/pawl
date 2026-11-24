import { describe, expect, test } from "bun:test";
import {
  EXPO_PUSH_URL,
  PUSH_BODY,
  PUSH_KEY,
  PUSH_MIN_INTERVAL_MS,
  PushRegistry,
  buildExpoPushRequest,
  isValidExpoToken,
  parseExpoResponse,
  sendWake,
} from "../src/push";
import { FrameLimiter, TokenBucket } from "../src/ratelimit";
import { MemoryKv } from "../src/storage";

const T0 = 1_800_000_000_000;

describe("token bucket", () => {
  test("burst then refill at the rate", () => {
    const b = new TokenBucket(50, 200, T0);
    let n = 0;
    while (b.take(T0)) n++;
    expect(n).toBe(200);
    expect(b.take(T0 + 10)).toBe(false); // 0.5 token
    expect(b.take(T0 + 20)).toBe(true); // 1 token after 20 ms
    expect(b.available(T0 + 1020)).toBe(50);
    expect(b.available(T0 + 100_000)).toBe(200); // capped at burst
  });
});

describe("frame limiter", () => {
  const opts = { ratePerSec: 10, burst: 2, noticeIntervalMs: 1000, abuseDrops: 5, abuseWindowMs: 10_000 };

  test("ok, then one notice per interval, then drops", () => {
    const l = new FrameLimiter(T0, opts);
    expect(l.check(T0)).toBe("ok");
    expect(l.check(T0)).toBe("ok");
    expect(l.check(T0)).toBe("notify");
    expect(l.check(T0)).toBe("drop");
    expect(l.check(T0 + 50)).toBe("drop");
    expect(l.check(T0 + 100)).toBe("ok"); // refilled one token
  });

  test("sustained flood closes the socket", () => {
    const l = new FrameLimiter(T0, opts);
    const verdicts = Array.from({ length: 10 }, () => l.check(T0));
    expect(verdicts).toContain("close");
  });

  test("drop window resets", () => {
    const l = new FrameLimiter(T0, { ...opts, ratePerSec: 0.001 });
    for (let i = 0; i < 2; i++) l.check(T0);
    for (let i = 0; i < 4; i++) expect(l.check(T0)).not.toBe("close");
    expect(l.check(T0 + 10_000)).not.toBe("close"); // new window
  });
});

describe("expo push", () => {
  test("token shape", () => {
    expect(isValidExpoToken("ExponentPushToken[xxxxxxxxxxxxxxxxxxxxxx]")).toBe(true);
    expect(isValidExpoToken("ExpoPushToken[abc_DEF-123]")).toBe(true);
    expect(isValidExpoToken("ExponentPushToken[]")).toBe(false);
    expect(isValidExpoToken("ExponentPushToken[a b]")).toBe(false);
    expect(isValidExpoToken("ExponentPushToken[abc]x")).toBe(false);
    expect(isValidExpoToken(`ExponentPushToken[${"a".repeat(300)}]`)).toBe(false);
    expect(isValidExpoToken("https://evil/")).toBe(false);
    expect(isValidExpoToken(1)).toBe(false);
  });

  test("request is content-free", () => {
    const { url, init } = buildExpoPushRequest("ExponentPushToken[abc]");
    expect(url).toBe(EXPO_PUSH_URL);
    expect(init.method).toBe("POST");
    const headers = init.headers as Record<string, string>;
    expect(headers["content-type"]).toBe("application/json");
    expect(headers.authorization).toBeUndefined();
    const body = JSON.parse(init.body as string);
    expect(body).toEqual([{ to: "ExponentPushToken[abc]", title: "Pawl", body: PUSH_BODY, sound: "default", priority: "high" }]);
    expect(body[0].data).toBeUndefined();
    const withAuth = buildExpoPushRequest("ExponentPushToken[abc]", "secret");
    expect((withAuth.init.headers as Record<string, string>).authorization).toBe("Bearer secret");
  });

  test("response parsing", () => {
    expect(parseExpoResponse(200, { data: [{ status: "ok", id: "x" }] })).toBe("ok");
    expect(parseExpoResponse(200, { data: { status: "ok", id: "x" } })).toBe("ok");
    expect(parseExpoResponse(200, { data: [{ status: "error", details: { error: "DeviceNotRegistered" } }] })).toBe(
      "device_not_registered",
    );
    expect(parseExpoResponse(200, { data: [{ status: "error", details: { error: "MessageRateExceeded" } }] })).toBe("error");
    expect(parseExpoResponse(500, { data: [{ status: "ok" }] })).toBe("error");
    expect(parseExpoResponse(200, null)).toBe("error");
    expect(parseExpoResponse(200, { data: [] })).toBe("error");
  });

  test("registry throttles to one push per minute", () => {
    const kv = new MemoryKv();
    const reg = new PushRegistry(kv);
    expect(reg.claim(T0)).toBeNull(); // nothing registered
    reg.register("ExponentPushToken[a]");
    expect(reg.claim(T0)).toBe("ExponentPushToken[a]");
    expect(reg.claim(T0 + PUSH_MIN_INTERVAL_MS - 1)).toBeNull();
    reg.register("ExponentPushToken[b]"); // re-register keeps the throttle
    expect(new PushRegistry(kv).claim(T0 + 1)).toBeNull();
    expect(reg.claim(T0 + PUSH_MIN_INTERVAL_MS)).toBe("ExponentPushToken[b]");
    reg.unregister("ExponentPushToken[a]"); // stale token: no-op
    expect(kv.get(PUSH_KEY)).toBeDefined();
    reg.unregister("ExponentPushToken[b]");
    expect(kv.get(PUSH_KEY)).toBeUndefined();
  });

  test("clear (unregister_push) forgets the token and keeps an open throttle window", () => {
    const kv = new MemoryKv();
    const reg = new PushRegistry(kv);
    expect(reg.clear(T0)).toBe(false); // nothing stored: no write
    expect(kv.writes).toBe(0);
    reg.register("ExponentPushToken[a]");
    expect(reg.claim(T0)).toBe("ExponentPushToken[a]");
    expect(reg.clear(T0 + 1)).toBe(true);
    expect(kv.get(PUSH_KEY)).toEqual({ lastAt: T0 });
    expect(reg.claim(T0 + PUSH_MIN_INTERVAL_MS)).toBeNull(); // no token
    expect(reg.clear(T0 + 2)).toBe(false);
    reg.unregister("ExponentPushToken[a]"); // a late DeviceNotRegistered for the old token: no-op
    expect(kv.get(PUSH_KEY)).toEqual({ lastAt: T0 });
    reg.register("ExponentPushToken[b]");
    expect(reg.claim(T0 + 3)).toBeNull(); // throttle survived the off/on
    expect(reg.clear(T0 + PUSH_MIN_INTERVAL_MS)).toBe(true);
    expect(kv.get(PUSH_KEY)).toBeUndefined(); // window over: key removed
  });

  test("sendWake never throws", async () => {
    const failing = (async () => {
      throw new Error("network");
    }) as unknown as typeof fetch;
    expect(await sendWake("ExponentPushToken[a]", undefined, failing)).toBe("error");
    let seen: RequestInit | undefined;
    const okFetch = (async (_url: string, init: RequestInit) => {
      seen = init;
      return new Response(JSON.stringify({ data: [{ status: "ok", id: "1" }] }), { status: 200 });
    }) as unknown as typeof fetch;
    expect(await sendWake("ExponentPushToken[a]", "tok", okFetch)).toBe("ok");
    expect((seen?.headers as Record<string, string>).authorization).toBe("Bearer tok");
    const notJson = (async () => new Response("oops", { status: 200 })) as unknown as typeof fetch;
    expect(await sendWake("ExponentPushToken[a]", undefined, notJson)).toBe("error");
  });
});
