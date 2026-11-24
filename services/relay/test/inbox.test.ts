import { describe, expect, test } from "bun:test";
import type { PeerEnvelope } from "@krypto/protocol";
import { encodeStd, encodeUrl, randomBytes } from "../src/b64";
import { DEFAULT_INBOX, Inbox, SEEN_KEY, type DeliverRequest } from "../src/inbox";
import { PUSH_KEY, PUSH_MIN_INTERVAL_MS } from "../src/push";
import { MemoryKv } from "../src/storage";

const T0 = 1_800_000_000_000;
const pk = () => encodeUrl(randomBytes(32));
const token = () => encodeUrl(randomBytes(16));
let seq = 1;
const env = (boxBytes = 32): PeerEnvelope => ({
  v: 1,
  from: "dev-from",
  to: "dev-to",
  seq: seq++,
  nonce: encodeStd(randomBytes(24)),
  box: encodeStd(randomBytes(boxBytes)),
});
const offline = () => false;
const PUSH = "ExponentPushToken[abc]";

function inbox(over: Partial<typeof DEFAULT_INBOX> = {}) {
  const kv = new MemoryKv();
  return { kv, inbox: new Inbox(kv, { ...DEFAULT_INBOX, ...over }) };
}

describe("deliver basics", () => {
  test("allowed sender: live if possible, else queued in order", () => {
    const { inbox: ib } = inbox();
    const b = pk();
    ib.setAllowed([b]);
    const sent: string[] = [];
    const e1 = env();
    expect(ib.deliver({ from: b, env: e1 }, T0, (m) => (sent.push(m), true)).result).toBe("delivered");
    expect(JSON.parse(sent[0]!)).toEqual({ t: "msg", from: b, env: e1 });
    expect(ib.deliver({ from: b, env: env() }, T0, offline).result).toBe("queued");
    expect(ib.deliver({ from: b, env: env() }, T0, offline).result).toBe("queued");
    expect(ib.onAuthenticated(T0).length).toBe(2);
  });

  test("unknown sender without a slot is refused", () => {
    const { inbox: ib } = inbox();
    expect(ib.deliver({ from: pk(), env: env() }, T0, () => true).result).toBe("not_allowed");
    expect(ib.deliver({ from: "nope", env: env() }, T0, () => true).result).toBe("not_allowed");
  });

  test("pairing slot admits exactly one frame", () => {
    const { inbox: ib } = inbox();
    const t = token();
    ib.addPairingSlot(t, T0 + 60_000, T0);
    const c = pk();
    const first = ib.deliver({ from: c, env: env(), pairing: t }, T0, offline);
    expect(first).toMatchObject({ result: "queued", viaPairing: true });
    expect(ib.deliver({ from: c, env: env(), pairing: t }, T0, offline).result).toBe("not_allowed");
  });
});

describe("security fixes", () => {
  test("(2) per-recipient inbound budget sheds before the allowlist is read", () => {
    const { inbox: ib } = inbox({ inboundPerSec: 1, inboundBurst: 2 });
    const b = pk();
    ib.setAllowed([b]);
    let reads = 0;
    const isAllowed = ib.acl.isAllowed.bind(ib.acl);
    ib.acl.isAllowed = (p: string) => (reads++, isAllowed(p));
    const t = token();
    ib.addPairingSlot(t, T0 + 60_000, T0);
    expect(ib.deliver({ from: b, env: env() }, T0, () => true).result).toBe("delivered");
    expect(ib.deliver({ from: b, env: env() }, T0, () => true).result).toBe("delivered");
    expect(reads).toBe(2);
    // Over budget: refused without touching the allowlist or the pairing slot.
    expect(ib.deliver({ from: pk(), env: env(), pairing: t }, T0, () => true).result).toBe("rate_limited");
    expect(ib.deliver({ from: b, env: env() }, T0 + 10, () => true).result).toBe("rate_limited");
    expect(reads).toBe(2);
    expect(ib.acl.liveSlots(T0)).toBe(1);
    expect(ib.deliver({ from: b, env: env() }, T0 + 1000, () => true).result).toBe("delivered"); // refilled
  });

  test("(7) size is checked before a pairing slot is consumed", () => {
    const { inbox: ib } = inbox();
    const t = token();
    ib.addPairingSlot(t, T0 + 60_000, T0);
    const c = pk();
    const huge = env(330 * 1024);
    expect(ib.deliver({ from: c, env: huge, pairing: t }, T0, () => true).result).toBe("too_large");
    expect(ib.acl.liveSlots(T0)).toBe(1); // slot still there
    expect(ib.deliver({ from: c, env: env(), pairing: "bad-token" }, T0, () => true).result).toBe("not_allowed");
    expect(ib.acl.liveSlots(T0)).toBe(1);
    expect(ib.deliver({ from: c, env: env(), pairing: t }, T0, () => true).result).toBe("delivered");
    expect(ib.acl.liveSlots(T0)).toBe(0);
  });

  test("(4) unchanged allow / register_push / pairing token do not write; config budget 5/s", () => {
    const { kv, inbox: ib } = inbox();
    const [a, b] = [pk(), pk()];
    const t = token();
    expect(ib.setAllowed([a, b])).toMatchObject({ ok: true, changed: true });
    expect(ib.registerPush(PUSH)).toBe(true);
    expect(ib.addPairingSlot(t, T0 + 60_000, T0)).toMatchObject({ ok: true, changed: true });
    const writes = kv.writes;
    expect(ib.setAllowed([b, a])).toEqual({ ok: true, changed: false, droppedQueued: 0 });
    expect(ib.registerPush(PUSH)).toBe(false);
    expect(ib.addPairingSlot(t, T0 + 60_000, T0 + 1)).toMatchObject({ ok: true, changed: false });
    expect(kv.writes).toBe(writes);

    let ok = 0;
    for (let i = 0; i < 50; i++) if (ib.takeConfig(T0)) ok++;
    expect(ok).toBe(DEFAULT_INBOX.configBurst);
    expect(ib.takeConfig(T0 + 100)).toBe(false);
    expect(ib.takeConfig(T0 + 200)).toBe(true); // 5/s -> one token per 200 ms
  });

  test("(5) push only after the owner authenticated once and with a non-empty allowlist", () => {
    const { kv, inbox: ib } = inbox();
    const t = token();
    ib.registerPush(PUSH);
    ib.addPairingSlot(t, T0 + 60_000, T0);
    // Never authenticated, nobody allowed: queued (via a pairing slot) but no wake.
    expect(ib.deliver({ from: pk(), env: env(), pairing: t }, T0, offline)).toMatchObject({ result: "queued", wake: null });
    ib.onAuthenticated(T0);
    expect(kv.get(SEEN_KEY)).toBe(T0);
    expect(ib.mayPush()).toBe(false); // still no allowed sender
    const b = pk();
    ib.setAllowed([b]);
    expect(ib.mayPush()).toBe(true);
    expect(ib.deliver({ from: b, env: env() }, T0 + 1, offline).wake).toBe(PUSH);
    expect(ib.deliver({ from: b, env: env() }, T0 + 2, offline).wake).toBeNull(); // once a minute
    expect(ib.deliver({ from: b, env: env() }, T0 + 1 + PUSH_MIN_INTERVAL_MS, offline).wake).toBe(PUSH);
    // Delivered live: never a wake.
    expect(ib.deliver({ from: b, env: env() }, T0 + 10 * PUSH_MIN_INTERVAL_MS, () => true).wake).toBeNull();
  });

  test("(5) seen is written once", () => {
    const { kv, inbox: ib } = inbox();
    ib.onAuthenticated(T0);
    const writes = kv.writes;
    ib.onAuthenticated(T0 + 1000);
    expect(kv.writes).toBe(writes);
    expect(kv.get(SEEN_KEY)).toBe(T0);
  });

  test("(6) an allow change drops queued frames of senders no longer allowed", () => {
    const { inbox: ib } = inbox();
    const [b, c, joiner] = [pk(), pk(), pk()];
    ib.setAllowed([b, c]);
    const t = token();
    ib.addPairingSlot(t, T0 + 60_000, T0);
    const req = (from: string, extra: Partial<DeliverRequest> = {}) => ({ from, env: env(), ...extra });
    ib.deliver(req(b), T0, offline);
    ib.deliver(req(c), T0, offline);
    ib.deliver(req(joiner, { pairing: t }), T0, offline);
    ib.deliver(req(b), T0, offline);
    expect(ib.setAllowed([c])).toEqual({ ok: true, changed: true, droppedQueued: 2 });
    const left = ib.onAuthenticated(T0).map((f) => JSON.parse(f.msg).from);
    expect(left).toEqual([c, joiner]); // the pairing frame is kept (its sender is not in any allowlist yet)
  });
});

describe("unregister_push (step 7)", () => {
  test("no wake after unregister_push; nothing written when no token is stored", () => {
    const { kv, inbox: ib } = inbox();
    const b = pk();
    ib.onAuthenticated(T0);
    ib.setAllowed([b]);
    let writes = kv.writes;
    expect(ib.unregisterPush(T0)).toBe(false); // nothing registered: no-op
    expect(kv.writes).toBe(writes);

    ib.registerPush(PUSH);
    expect(ib.unregisterPush(T0)).toBe(true);
    expect(kv.get(PUSH_KEY)).toBeUndefined(); // never pushed: no throttle state to keep
    expect(ib.deliver({ from: b, env: env() }, T0 + 1, offline)).toMatchObject({ result: "queued", wake: null });
    writes = kv.writes;
    expect(ib.unregisterPush(T0 + 2)).toBe(false); // already off: no write
    expect(kv.writes).toBe(writes);
    // Turned back on: wakes again.
    ib.registerPush(PUSH);
    expect(ib.deliver({ from: b, env: env() }, T0 + 3, offline).wake).toBe(PUSH);
  });

  test("off/on within a minute keeps the once-per-minute throttle", () => {
    const { kv, inbox: ib } = inbox();
    const b = pk();
    const I = PUSH_MIN_INTERVAL_MS;
    ib.onAuthenticated(T0);
    ib.setAllowed([b]);
    ib.registerPush(PUSH);
    expect(ib.deliver({ from: b, env: env() }, T0, offline).wake).toBe(PUSH);
    expect(ib.unregisterPush(T0 + 1)).toBe(true);
    expect(kv.get(PUSH_KEY)).toEqual({ lastAt: T0 }); // token gone, throttle kept
    ib.registerPush(PUSH);
    expect(ib.deliver({ from: b, env: env() }, T0 + 2, offline).wake).toBeNull(); // toggling does not reset it
    expect(ib.deliver({ from: b, env: env() }, T0 + I, offline).wake).toBe(PUSH);
    expect(ib.unregisterPush(T0 + I + 1)).toBe(true);
    expect(ib.deliver({ from: b, env: env() }, T0 + 3 * I, offline).wake).toBeNull(); // window over, but no token
    // Past the window the key is removed entirely.
    ib.registerPush(PUSH);
    expect(ib.unregisterPush(T0 + 3 * I + 1)).toBe(true);
    expect(kv.get(PUSH_KEY)).toBeUndefined();
  });
});
