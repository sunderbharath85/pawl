// PhoneNode end to end against the in-memory relay and fake hubs: pairing (joiner side), hello / presence, sync,
// live mirror, intents, roster_intro rules, not_allowed retries, revocation, persistence across restarts.
import { describe, expect, test } from "bun:test";
import { INTENT_TIMEOUT_MS } from "@pawl/protocol";
import type { PeerEnvelope } from "@pawl/protocol";
import { IntentError, MemoryStore, PairError, PhoneNode, encode, seal } from "../src/lib/peer";
import { FakeClock, FakeHub, FakeRelay, flush, sampleEvent, sampleRequest, sampleSession, seededRandom } from "./helpers";

async function setup(opts: { seed?: number; store?: MemoryStore; clock?: FakeClock; relay?: FakeRelay } = {}) {
  const clock = opts.clock ?? new FakeClock();
  const relay = opts.relay ?? new FakeRelay();
  const store = opts.store ?? new MemoryStore();
  const phone = await PhoneNode.open({
    store,
    clock,
    random: seededRandom(opts.seed ?? 3),
    socketFactory: relay.factory,
    name: "Test phone",
    os: "android",
  });
  return { clock, relay, store, phone };
}

async function pairWith(phone: PhoneNode, hub: FakeHub, clock: FakeClock) {
  const p = phone.pair(hub.createPhoneInvite());
  await clock.advance(50);
  return p;
}

describe("PhoneNode pairing", () => {
  test("pairs over the relay with a relay-only invite and links up", async () => {
    const { clock, relay, phone, store } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    hub.sessions = [sampleSession(hub.id)];
    hub.pending = [sampleRequest(hub.id)];
    hub.events = [sampleEvent(hub.id, 5), sampleEvent(hub.id, 6)];
    const view = await pairWith(phone, hub, clock);
    expect(view.id).toBe(hub.id);
    expect(view.name).toBe("Mac mini");
    // The hub stored the phone as kind phone (os android in pair_request).
    expect(hub.roster.get(phone.deviceId)?.kind).toBe("phone");
    const req = hub.received.find((r) => r.body.type === "pair_request")!.body;
    expect(req.type === "pair_request" && req.machine.os).toBe("android");
    expect(req.type === "pair_request" && req.addr).toBeUndefined();
    // Allow list then holds the hub; the pairing slot was consumed.
    expect(relay.mailbox(addressOf(phone)).allow.has(hub.keys.address)).toBe(true);
    await clock.advance(50);
    const snap = phone.getSnapshot();
    expect(snap.paired).toBe(true);
    expect(snap.hubs[0]?.online).toBe(true);
    expect(Object.keys(snap.sessions)).toEqual([`${hub.id}:s1`]);
    expect(Object.keys(snap.pending)).toEqual([`${hub.id}:r1`]);
    expect(snap.events[`${hub.id}:s1`]?.map((e) => e.seq)).toEqual([5, 6]);
    expect(snap.hubs[0]?.sync).toBe("synced");
    expect(snap.repos[hub.id]?.[0]?.name).toBe("ai-pet");
    // Phone sent presence + sync_request after the link came up.
    expect(hub.bodiesFrom(phone.deviceId, "sync_request").length).toBe(1);
    expect(hub.bodiesFrom(phone.deviceId, "presence").length).toBeGreaterThan(0);
    // Relay config + roster persisted.
    await phone.flush();
    expect(store.data.get("pawl.hubs")).toBe(JSON.stringify([hub.id]));
    expect(JSON.parse(store.data.get("pawl.relay")!).url).toBe(relay.url);
    expect(store.data.get("pawl.sk")).toMatch(/^[0-9a-f]{64}$/);
  });

  test("a used invite is rejected (relay not_allowed) and the adopted relay is turned off", async () => {
    const { clock, relay, phone } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    const uri = hub.createPhoneInvite();
    const first = phone.pair(uri);
    await clock.advance(50);
    await first;
    const { phone: other } = await setup({ seed: 9, clock, relay });
    const second = other.pair(uri);
    second.catch(() => {});
    await clock.advance(50);
    await expect(second).rejects.toMatchObject({ code: "invite_rejected" });
    expect(other.getSnapshot().relay.url).toBeNull();
  });

  test("wrong code gets pair_reject -> invite_rejected", async () => {
    const { clock, relay, phone } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    const uri = hub.createPhoneInvite();
    hub.invites.clear();
    const p = phone.pair(uri);
    p.catch(() => {});
    await clock.advance(50);
    await expect(p).rejects.toBeInstanceOf(PairError);
    await expect(p).rejects.toMatchObject({ code: "invite_rejected" });
  });

  test("no answer -> timeout; LAN-only invites and bad URIs are refused", async () => {
    const { clock, relay, phone } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    hub.online = false;
    const p = phone.pair(hub.createPhoneInvite());
    p.catch(() => {});
    await clock.advance(11_000);
    await expect(p).rejects.toMatchObject({ code: "timeout" });
    await expect(phone.pair("pawl://peer?v=1")).rejects.toMatchObject({ code: "bad_uri" });
    const lan = `pawl://peer?v=1&mid=macmini00001&pk=${hub.keys.address}&addr=10.0.0.2:47801&code=${"A".repeat(22)}&exp=1`;
    await expect(phone.pair(lan)).rejects.toMatchObject({ code: "no_relay" });
  });

  test("wrong relay access key -> unreachable", async () => {
    const { clock, relay, phone } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    const uri = hub.createPhoneInvite().replace(/rk=[^&]+/, "rk=wrongkey");
    const p = phone.pair(uri);
    p.catch(() => {});
    await clock.advance(11_000);
    await expect(p).rejects.toMatchObject({ code: "unreachable" });
  });
});

function addressOf(phone: PhoneNode): string {
  const pk = Buffer.from(phone.publicKey, "base64");
  return Buffer.from(pk).toString("base64").replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

describe("PhoneNode links", () => {
  test("hello on start is answered by presence; silence takes the link down; retries bring it back", async () => {
    const { clock, relay, phone, store } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    await pairWith(phone, hub, clock);
    await phone.stop();
    // Restart the phone from the same store.
    const { phone: again } = await setup({ store, clock, relay });
    expect(again.deviceId).toBe(phone.deviceId);
    again.start();
    await clock.advance(100);
    expect(hub.bodiesFrom(again.deviceId, "hello").length).toBe(1);
    expect(again.getSnapshot().hubs[0]?.online).toBe(true);
    // The hub stops answering: offline after 45 s.
    hub.online = false;
    await clock.advance(46_000);
    expect(again.getSnapshot().hubs[0]?.online).toBe(false);
    hub.online = true;
    await clock.advance(15_000);
    expect(again.getSnapshot().hubs[0]?.online).toBe(true);
  });

  test("presence goes out every 15 s while linked", async () => {
    const { clock, relay, phone } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    await pairWith(phone, hub, clock);
    const before = hub.bodiesFrom(phone.deviceId, "presence").length;
    // Keep the hub talking so the link stays up.
    for (let i = 0; i < 4; i++) {
      await clock.advance(15_000);
      hub.send(phone.deviceId, hub.presence());
    }
    expect(hub.bodiesFrom(phone.deviceId, "presence").length - before).toBeGreaterThanOrEqual(4);
  });

  test("a replayed envelope is dropped (seq <= last rx)", async () => {
    const { clock, relay, phone } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    await pairWith(phone, hub, clock);
    await clock.advance(10);
    const req = sampleRequest(hub.id, "r9");
    const captured: { from: string; to: string; env: PeerEnvelope }[] = [];
    const orig = relay.route.bind(relay);
    relay.route = (from, to, env, pairing) => {
      captured.push({ from, to, env });
      return orig(from, to, env, pairing);
    };
    hub.mirror({ type: "permission_request", request: req });
    await clock.advance(10);
    relay.route = orig;
    expect(phone.getSnapshot().pending[req.requestId]).toBeDefined();
    hub.mirror({ type: "permission_resolved", requestId: req.requestId, sessionId: req.sessionId, outcome: { kind: "cancelled" }, by: "system" });
    await clock.advance(10);
    expect(phone.getSnapshot().pending[req.requestId]).toBeUndefined();
    // The relay (or anyone on the path) replays the older permission_request frame: dropped.
    const old = captured[0]!;
    expect(relay.route(old.from, old.to, old.env)).toBe(true);
    await clock.advance(10);
    expect(phone.getSnapshot().pending[req.requestId]).toBeUndefined();
    // Same frame with a bumped seq: the nonce no longer matches, dropped too.
    relay.route(old.from, old.to, { ...old.env, seq: old.env.seq + 10_000_000 });
    await clock.advance(10);
    expect(phone.getSnapshot().pending[req.requestId]).toBeUndefined();
  });

  test("a frame whose relay sender is not the hub's key is dropped", async () => {
    const { clock, relay, phone } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    const other = new FakeHub({ id: "otherhub0001", name: "Other", relay, clock, seed: 5 });
    await pairWith(phone, hub, clock);
    await clock.advance(10);
    // `other` claims to be the Mac (env.from) but the relay authenticated its own key.
    other.addDevice(phone.deviceId, Buffer.from(phone.publicKey, "base64"), "phone", "x");
    relay.mailbox(addressOf(phone)).allow.add(other.keys.address);
    const fake = sampleRequest(hub.id, "forged");
    const d = other.roster.get(phone.deviceId)!;
    d.tx = clock.now() * 1000 + 5;
    const env = seal(other.keys, hub.id, phone.deviceId, d.pk, d.tx, { type: "mirror", update: { type: "permission_request", request: fake } }, other.random);
    relay.route(other.keys.address, addressOf(phone), env);
    await clock.advance(10);
    expect(phone.getSnapshot().pending[fake.requestId]).toBeUndefined();
  });
});

describe("PhoneNode intents", () => {
  test("approve needs confirmed; applied ack resolves; already_decided rejects", async () => {
    const { clock, relay, phone } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    const req = sampleRequest(hub.id);
    hub.pending = [req];
    await pairWith(phone, hub, clock);
    await clock.advance(10);
    await expect(phone.approve(req.requestId, { confirmed: false })).rejects.toMatchObject({ code: "unconfirmed" });
    const p = phone.approve(req.requestId, { confirmed: true });
    await clock.advance(10);
    await expect(p).resolves.toMatchObject({ result: {} });
    const sent = hub.bodiesFrom(phone.deviceId, "intent")[0]!;
    expect(sent.type === "intent" && sent.client).toBe("mobile");
    expect(sent.type === "intent" && sent.confirmed).toBe(true);
    expect(sent.type === "intent" && sent.intent).toEqual({ kind: "approve", requestId: req.requestId, optionId: "allow" });
    // Still pending in the phone's mirror until permission_resolved arrives: a second approve races and loses.
    const again = phone.approve(req.requestId, { confirmed: true });
    again.catch(() => {});
    await clock.advance(10);
    await expect(again).rejects.toMatchObject({ code: "already_decided" });
    hub.mirror({ type: "permission_resolved", requestId: req.requestId, sessionId: req.sessionId, outcome: { kind: "selected", optionId: "allow" }, by: "user", byMachineId: phone.deviceId });
    await clock.advance(10);
    const res = phone.resolution(req.requestId)!;
    expect(res.verdict).toBe("approve");
    expect(phone.deviceName(res.byMachineId)).toBe("this phone");
    // Now decided locally too.
    await expect(phone.approve(req.requestId, { confirmed: true })).rejects.toMatchObject({ code: "already_decided" });
  });

  test("deny picks reject_once; prompt / cancel / start; timeout and offline", async () => {
    const { clock, relay, phone } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    const req = sampleRequest(hub.id);
    hub.pending = [req];
    await pairWith(phone, hub, clock);
    await clock.advance(10);
    const d = phone.deny(req.requestId);
    await clock.advance(10);
    await d;
    const sent = hub.bodiesFrom(phone.deviceId, "intent").map((b) => (b.type === "intent" ? b.intent : null));
    expect(sent[0]).toEqual({ kind: "deny", requestId: req.requestId, optionId: "reject" });
    await expect(phone.startSession({ machineId: hub.id, agentId: "claude-code", cwd: "/code/ai-pet", confirmed: false })).rejects.toMatchObject({ code: "unconfirmed" });
    const s = phone.startSession({ machineId: hub.id, agentId: "claude-code", cwd: "/code/ai-pet", prompt: " run tests ", confirmed: true });
    await clock.advance(10);
    const started = await s;
    expect((started.result as { session: { id: string } }).session.id).toBe(`${hub.id}:s-new`);
    const p = phone.prompt(`${hub.id}:s1`, "hello");
    const c = phone.cancel(`${hub.id}:s1`);
    await clock.advance(10);
    await Promise.all([p, c]);
    const kinds = hub.bodiesFrom(phone.deviceId, "intent").map((b) => (b.type === "intent" ? b.intent.kind : ""));
    expect(kinds).toEqual(["deny", "start", "prompt", "cancel"]);
    const startIntent = hub.bodiesFrom(phone.deviceId, "intent")[1]!;
    expect(startIntent.type === "intent" && startIntent.intent).toEqual({ kind: "start", agentId: "claude-code", cwd: "/code/ai-pet", prompt: "run tests" });
    // No ack -> timeout after INTENT_TIMEOUT_MS.
    hub.online = false;
    const t = phone.prompt(`${hub.id}:s1`, "anyone?");
    t.catch(() => {});
    await clock.advance(INTENT_TIMEOUT_MS + 10);
    await expect(t).rejects.toMatchObject({ code: "timeout" });
    // Link down -> offline without sending.
    await clock.advance(50_000);
    await expect(phone.cancel(`${hub.id}:s1`)).rejects.toMatchObject({ code: "offline" });
    await expect(phone.cancel("zzzzzzzzzzzz:s1")).rejects.toBeInstanceOf(IntentError);
  });

  test("a request without allow_once cannot be approved from the phone", async () => {
    const { clock, relay, phone } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    const req = sampleRequest(hub.id);
    req.options = req.options.filter((o) => o.kind !== "allow_once");
    hub.pending = [req];
    await pairWith(phone, hub, clock);
    await clock.advance(10);
    await expect(phone.approve(req.requestId, { confirmed: true })).rejects.toMatchObject({ code: "no_allow_once" });
  });
});

describe("PhoneNode roster_intro", () => {
  test("adds introduced hubs (introducedBy), hellos them, retries on not_allowed 1 s, 2 s, 5 s, 30 s", async () => {
    const { clock, relay, phone } = await setup();
    const mac = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock, seed: 11 });
    const pc = new FakeHub({ id: "windowspc001", name: "Windows PC", relay, clock, seed: 12 });
    await pairWith(phone, mac, clock);
    await clock.advance(10);
    // The Mac introduces the PC before the PC knows the phone: the relay refuses our hellos at first.
    mac.intro(phone.deviceId, [{ id: pc.id, kind: "hub", name: "Windows PC", publicKey: encode(pc.keys.publicKey), os: "windows" }]);
    await clock.advance(10);
    const hubs = phone.getSnapshot().hubs;
    const pcView = hubs.find((h) => h.id === pc.id)!;
    expect(pcView.introducedBy).toBe(mac.id);
    expect(pcView.os).toBe("windows");
    expect(phone.getSnapshot().notices[0]?.text).toBe("Mac mini added Windows PC");
    const refusals = () => relay.errors.filter((e) => e.to === pc.keys.address).length;
    expect(refusals()).toBe(1);
    await clock.advance(1_000);
    expect(refusals()).toBe(2);
    await clock.advance(2_000);
    expect(refusals()).toBe(3);
    await clock.advance(5_000);
    expect(refusals()).toBe(4);
    await clock.advance(29_000);
    expect(refusals()).toBe(4);
    // The PC learns the phone (its own intro); the next retry gets through.
    pc.addDevice(phone.deviceId, Buffer.from(phone.publicKey, "base64"), "phone", "Test phone");
    await clock.advance(1_500);
    expect(phone.getSnapshot().hubs.find((h) => h.id === pc.id)?.online).toBe(true);
  });

  test("never replaces a key, ignores phones, revoked devices and intros from unknown senders", async () => {
    const { clock, relay, phone } = await setup();
    const mac = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock, seed: 11 });
    const pc = new FakeHub({ id: "windowspc001", name: "Windows PC", relay, clock, seed: 12 });
    await pairWith(phone, mac, clock);
    await clock.advance(10);
    const evil = new FakeHub({ id: "evilhub00001", name: "Evil", relay, clock, seed: 13 });
    mac.intro(phone.deviceId, [
      // A different key for the Mac itself (the sender) -> refused.
      { id: mac.id, kind: "hub", name: "Mac mini", publicKey: encode(evil.keys.publicKey) },
      // Phones are never introduced to phones.
      { id: "otherphone01", kind: "phone", name: "iPad", publicKey: encode(evil.keys.publicKey) },
      // Our own id.
      { id: phone.deviceId, kind: "hub", name: "Me", publicKey: encode(evil.keys.publicKey) },
      // Low-order key.
      { id: "loworder0001", kind: "hub", name: "Zero", publicKey: encode(new Uint8Array(32)) },
      { id: pc.id, kind: "hub", name: "Windows PC", publicKey: encode(pc.keys.publicKey) },
    ]);
    await clock.advance(10);
    expect(phone.getSnapshot().hubs.map((h) => h.id).sort()).toEqual([mac.id, pc.id].sort());
    // A second intro naming the PC with another key: conflict, key kept.
    mac.intro(phone.deviceId, [{ id: pc.id, kind: "hub", name: "Windows PC", publicKey: encode(evil.keys.publicKey) }]);
    await clock.advance(10);
    expect(phone.getSnapshot().notices[0]?.text).toContain("different key");
    // An unpaired device cannot introduce anything (its frames do not even open).
    evil.addDevice(phone.deviceId, Buffer.from(phone.publicKey, "base64"), "phone", "x");
    evil.intro(phone.deviceId, [{ id: "sneakyhub001", kind: "hub", name: "Sneaky", publicKey: encode(evil.keys.publicKey) }]);
    await clock.advance(10);
    expect(phone.getSnapshot().hubs.some((h) => h.id === "sneakyhub001")).toBe(false);
    // Removing the PC means a later intro never re-adds it.
    await phone.removeHub(pc.id);
    mac.intro(phone.deviceId, [{ id: pc.id, kind: "hub", name: "Windows PC", publicKey: encode(pc.keys.publicKey) }]);
    await clock.advance(10);
    expect(phone.getSnapshot().hubs.some((h) => h.id === pc.id)).toBe(false);
  });

  test("bye{revoked} from a hub removes it; unpairPhone says bye everywhere and resets the identity", async () => {
    const { clock, relay, phone, store } = await setup();
    const mac = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock, seed: 11 });
    await pairWith(phone, mac, clock);
    await clock.advance(10);
    mac.send(phone.deviceId, { type: "bye", reason: "revoked", ts: clock.now() });
    await clock.advance(10);
    expect(phone.getSnapshot().paired).toBe(false);
    expect(phone.getSnapshot().notices[0]?.text).toBe("Mac mini unpaired this phone");
    const pc = new FakeHub({ id: "windowspc001", name: "Windows PC", relay, clock, seed: 12 });
    await pairWith(phone, pc, clock);
    await clock.advance(10);
    const oldId = phone.deviceId;
    const oldPk = phone.publicKey;
    const done = phone.unpairPhone();
    await clock.advance(500);
    await done;
    expect(pc.roster.has(oldId)).toBe(false);
    expect(phone.deviceId).not.toBe(oldId);
    expect(phone.publicKey).not.toBe(oldPk);
    expect(phone.getSnapshot().hubs).toEqual([]);
    expect(store.data.has("pawl.hubs")).toBe(false);
    void flush;
  });
});

describe("PhoneNode push", () => {
  const TOKEN = "ExponentPushToken[abc123]";

  test("register_push after pairing; off -> unregister_push; on again -> register_push", async () => {
    const { clock, relay, phone } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    await pairWith(phone, hub, clock);
    await clock.advance(10);
    const box = () => relay.mailbox(addressOf(phone));
    phone.setPushToken(TOKEN);
    await clock.advance(10);
    expect(box().pushToken).toBe(TOKEN);
    phone.setPushEnabled(false);
    await clock.advance(10);
    expect(box().pushToken).toBeNull();
    expect(relay.framesOf("unregister_push").length).toBe(1);
    // Not repeated on the same session.
    phone.setPushEnabled(false);
    await clock.advance(10);
    expect(relay.framesOf("unregister_push").length).toBe(1);
    phone.setPushEnabled(true);
    await clock.advance(10);
    expect(box().pushToken).toBe(TOKEN);
  });

  test("push turned off while the relay is down is cleared on the next session (and survives a restart)", async () => {
    const { clock, relay, phone, store } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    await pairWith(phone, hub, clock);
    phone.setPushToken(TOKEN);
    await clock.advance(10);
    relay.refuseConnections = true;
    relay.restart();
    await clock.advance(10);
    phone.setPushEnabled(false);
    await phone.stop();
    expect(relay.mailbox(addressOf(phone)).pushToken).toBe(TOKEN);
    relay.refuseConnections = false;
    const { phone: again } = await setup({ store, clock, relay });
    again.start();
    await clock.advance(100);
    expect(relay.mailbox(addressOf(again)).pushToken).toBeNull();
    expect(relay.framesOf("unregister_push").length).toBe(1);
    // Cleared for good: the next session sends nothing.
    relay.restart();
    await clock.advance(2_000);
    expect(relay.framesOf("unregister_push").length).toBe(1);
  });

  test("unpairPhone sends unregister_push before the byes", async () => {
    const { clock, relay, phone } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    await pairWith(phone, hub, clock);
    phone.setPushToken(TOKEN);
    await clock.advance(10);
    const addr = addressOf(phone);
    const done = phone.unpairPhone();
    await clock.advance(500);
    await done;
    expect(relay.mailbox(addr).pushToken).toBeNull();
    const mine = relay.log.filter((l) => l.from === addr).map((l) => l.frame.t);
    const unreg = mine.lastIndexOf("unregister_push");
    const lastSend = mine.lastIndexOf("send");
    expect(unreg).toBeGreaterThan(-1);
    expect(unreg).toBeLessThan(lastSend);
    expect(hub.roster.has(phone.deviceId)).toBe(false);
  });
});

describe("PhoneNode name rules + write-ahead rx", () => {
  const cp = (n: number) => String.fromCodePoint(n);

  test("an inviter whose name breaks the rule is refused (invite_rejected)", async () => {
    const { clock, relay, phone } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: `Mac mini${cp(0x202e)}`, relay, clock });
    const p = phone.pair(hub.createPhoneInvite());
    p.catch(() => {});
    await clock.advance(50);
    await expect(p).rejects.toMatchObject({ code: "invite_rejected" });
    expect(phone.getSnapshot().paired).toBe(false);
  });

  test("hello with a refused name is dropped; presence with one is ignored (name never changes)", async () => {
    const { clock, relay, phone, store } = await setup();
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    await pairWith(phone, hub, clock);
    await clock.advance(10);
    hub.name = `Mac${cp(0x200b)}mini`;
    hub.send(phone.deviceId, hub.presence());
    await clock.advance(10);
    expect(phone.getSnapshot().hubs[0]?.name).toBe("Mac mini");
    // After a restart (link down), a hello with the bad name does not bring the link up.
    await phone.stop();
    const { phone: again } = await setup({ store, clock, relay });
    hub.online = false; // the fake hub would answer our hello with a (bad-named) presence anyway
    again.start();
    await clock.advance(50);
    hub.online = true;
    hub.send(again.deviceId, { type: "hello", machine: hub.machine(), ts: clock.now() });
    await clock.advance(10);
    expect(again.getSnapshot().hubs[0]?.online).toBe(false);
    hub.name = "Mac mini 2";
    hub.send(again.deviceId, { type: "hello", machine: hub.machine(), ts: clock.now() });
    await clock.advance(10);
    expect(again.getSnapshot().hubs[0]?.online).toBe(true);
    expect(again.getSnapshot().hubs[0]?.name).toBe("Mac mini 2");
  });

  test("last_rx is stored before a frame is acted on", async () => {
    const clock = new FakeClock();
    const relay = new FakeRelay();
    // A store whose writes can be held back, to see what the node does before they land.
    let hold = false;
    const held: (() => void)[] = [];
    const inner = new MemoryStore();
    const store: MemoryStore = Object.assign(Object.create(MemoryStore.prototype), {
      data: inner.data,
      get: (k: string) => inner.get(k),
      delete: (k: string) => inner.delete(k),
      set: (k: string, v: string) => (hold ? new Promise<void>((r) => held.push(() => void inner.set(k, v).then(r))) : inner.set(k, v)),
    });
    const { phone } = await setup({ store, clock, relay });
    const hub = new FakeHub({ id: "macmini00001", name: "Mac mini", relay, clock });
    await pairWith(phone, hub, clock);
    await clock.advance(10);
    const seqs: number[] = [];
    const orig = relay.route.bind(relay);
    relay.route = (from, to, env, pairing) => {
      if (from === hub.keys.address) seqs.push(env.seq);
      return orig(from, to, env, pairing);
    };
    let storedAtEvent = -1;
    phone.onEvent((e) => {
      if (e.type === "permission_request") storedAtEvent = JSON.parse(inner.data.get(`pawl.seq.${hub.id}`)!).rx;
    });
    hold = true;
    hub.mirror({ type: "permission_request", request: sampleRequest(hub.id, "wa1") });
    await clock.advance(100);
    // Accepted (in memory) but not acted on while its seq write is pending.
    expect(phone.getSnapshot().pending[`${hub.id}:wa1`]).toBeUndefined();
    hold = false;
    for (const release of held.splice(0)) release();
    await clock.advance(100);
    expect(phone.getSnapshot().pending[`${hub.id}:wa1`]).toBeDefined();
    expect(storedAtEvent).toBeGreaterThanOrEqual(seqs[seqs.length - 1]!);
  });
});
