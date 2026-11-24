// Pure logic: mirror reducers, roster_intro rules, intent helpers, the biometric gate (mocked) and the relay client's
// auth / reconnect behaviour against the in-memory relay.
import { describe, expect, test } from "bun:test";
import type { IntroDevice } from "@krypto/protocol";
import {
  applyMirrorBody,
  cleanDeviceName,
  deviceNameOk,
  MIRROR_MAX_EVENT_SESSIONS,
  MIRROR_MAX_PENDING,
  MIRROR_MAX_PETS,
  stripNameChars,
  approveOption,
  ApprovalGate,
  denyOption,
  DeviceKeys,
  emptyMirror,
  encode,
  evaluateIntro,
  gateAvailability,
  helloRetryDelay,
  IntentError,
  phoneIntentErrorText,
  RelayClient,
  type Authenticator,
  type AuthCapability,
  type IntroContext,
  type RelayStatusInfo,
} from "../src/lib/peer";
import { FakeClock, FakeRelay, sampleEvent, sampleRequest, sampleSession, seededRandom } from "./helpers";

const M = "macmini00001";
const OTHER = "windowspc001";

describe("mirror reducers", () => {
  test("snapshot replaces, drops foreign items; events merge and set the cursor", () => {
    let m = emptyMirror(M);
    m = applyMirrorBody(
      m,
      {
        type: "mirror_snapshot",
        sessions: [sampleSession(M), sampleSession(OTHER), { ...sampleSession(M, "x"), id: `${OTHER}:x` }],
        pets: [],
        pendingPermissions: [sampleRequest(M), sampleRequest(OTHER)],
      },
      1,
    );
    expect(Object.keys(m.state.sessions)).toEqual([`${M}:s1`]);
    expect(Object.keys(m.state.pending)).toEqual([`${M}:r1`]);
    expect(m.sync).toBe("syncing");
    m = applyMirrorBody(m, { type: "mirror_events", events: [sampleEvent(M, 3), sampleEvent(M, 2), sampleEvent(OTHER, 9), { ...sampleEvent(M, 4), id: `${M}:5` }] }, 1);
    expect(m.state.events[`${M}:s1`]?.map((e) => e.seq)).toEqual([2, 3]);
    expect(m.maxSeq).toBe(3);
    m = applyMirrorBody(m, { type: "sync_done", seq: 3, truncated: true }, 1);
    expect(m.sync).toBe("synced");
    expect(m.truncated).toBe(true);
    // A later snapshot replaces sessions but keeps events.
    m = applyMirrorBody(m, { type: "mirror_snapshot", sessions: [], pets: [], pendingPermissions: [] }, 2);
    expect(m.state.sessions).toEqual({});
    expect(m.state.events[`${M}:s1`]?.length).toBe(2);
  });

  test("live updates (hub-client reducer) with validation; resolutions record who decided", () => {
    let m = emptyMirror(M);
    const req = sampleRequest(M);
    m = applyMirrorBody(m, { type: "mirror", update: { type: "permission_request", request: req } }, 1);
    m = applyMirrorBody(m, { type: "mirror", update: { type: "permission_request", request: sampleRequest(OTHER, "evil") } }, 1);
    expect(Object.keys(m.state.pending)).toEqual([req.requestId]);
    m = applyMirrorBody(m, { type: "mirror", update: { type: "event", event: sampleEvent(M, 10) } }, 1);
    expect(m.maxSeq).toBe(10);
    const before = m;
    m = applyMirrorBody(m, { type: "mirror", update: { type: "event", event: sampleEvent(M, 10) } }, 1);
    expect(m).toBe(before); // duplicate event: no change
    m = applyMirrorBody(
      m,
      {
        type: "mirror",
        update: { type: "permission_resolved", requestId: req.requestId, sessionId: req.sessionId, outcome: { kind: "selected", optionId: "reject" }, by: "user", byMachineId: OTHER },
      },
      50,
    );
    expect(m.state.pending).toEqual({});
    expect(m.resolutions[0]).toMatchObject({ requestId: req.requestId, verdict: "deny", byMachineId: OTHER, at: 50 });
    expect(m.resolutions[0]?.request?.toolCall.title).toBe("Run bun test");
    // A resolution for another machine's request is dropped.
    const r2 = applyMirrorBody(m, { type: "mirror", update: { type: "permission_resolved", requestId: `${OTHER}:r1`, sessionId: `${OTHER}:s1`, outcome: { kind: "cancelled" }, by: "system" } }, 60);
    expect(r2).toBe(m);
  });
});

describe("roster_intro rules", () => {
  const random = seededRandom(41);
  const me = DeviceKeys.generate(random);
  const sender = DeviceKeys.generate(random);
  const known = DeviceKeys.generate(random);
  const fresh = DeviceKeys.generate(random);
  const revoked = DeviceKeys.generate(random);
  const ctx: IntroContext = {
    myId: "myphone00001",
    myPk: me.publicKey,
    senderId: M,
    senderPk: sender.publicKey,
    known: new Map([
      [M, sender.publicKey],
      ["knownhub0001", known.publicKey],
    ]),
    revokedIds: new Set(["gonehub00001"]),
    revokedPks: new Set([revoked.publicB64]),
    isLowOrder: (pk) => me.isLowOrder(pk),
  };
  const dev = (id: string, pk: string, over: Partial<IntroDevice> = {}): IntroDevice => ({ id, kind: "hub", name: "Hub", publicKey: pk, ...over });

  test("accepts new hubs, skips everything else with a reason", () => {
    const cp = (n: number) => String.fromCodePoint(n);
    const k = () => DeviceKeys.generate(random).publicB64;
    const cases: [IntroDevice, string][] = [
      [dev("newhub000001", fresh.publicB64, { name: "  Windows PC ", os: "windows", addr: "192.168.1.9:47801" }), "accepted"],
      [dev("knownhub0001", known.publicB64), "known"],
      [dev("knownhub0001", k()), "id_conflict"],
      [dev("otherid00001", known.publicB64), "key_conflict"],
      [dev("gonehub00001", k()), "revoked"],
      [dev("newhub000002", revoked.publicB64), "revoked"],
      [dev(ctx.myId, k()), "self"],
      [dev("mykey0000001", me.publicB64), "self"],
      [dev(M, k()), "sender"],
      [dev("newhub000001", k()), "duplicate"],
      [dev("phone0000001", k(), { kind: "phone" }), "bad_kind"],
      [dev("phone0000002", k(), { kind: "phone", os: "android" }), "not_a_hub"],
      [dev("iphonehub001", k(), { os: "ios" }), "bad_kind"],
      [dev("weirdos00001", k(), { os: "plan9" as never }), "bad_kind"],
      [dev("weirdkind001", k(), { kind: "tablet" as never }), "bad_kind"],
      [dev("phoneaddr001", k(), { kind: "phone", os: "ios", addr: "192.168.1.9:47801" }), "bad_addr"],
      [dev("zwsp00000001", k(), { name: `Mac${cp(0x200b)}mini` }), "bad_name"],
      [dev("bidi00000001", k(), { name: `Mac ${cp(0x202e)}gnp.exe` }), "bad_name"],
      [dev("bom000000001", k(), { name: `${cp(0xfeff)}Mac` }), "bad_name"],
      [dev("alm000000001", k(), { name: `Mac${cp(0x061c)}` }), "bad_name"],
      [dev("wj0000000001", k(), { name: `Mac${cp(0x2060)}mini` }), "bad_name"],
      [dev("toolong00001", k(), { name: "x".repeat(65) }), "bad_name"],
      [dev("BADID", k()), "bad_id"],
      [dev("noname000001", k(), { name: "   " }), "bad_name"],
      [dev("ctrl00000001", k(), { name: `a${cp(7)}b` }), "bad_name"],
      [dev("lowkey000001", encode(new Uint8Array(32))), "bad_key"],
      [dev("badaddr00001", k(), { addr: "8.8.8.8:53" }), "bad_addr"],
    ];
    const d = evaluateIntro(ctx, cases.map((c) => c[0]));
    expect(d.accepted.map((a) => [a.id, a.name, a.os, a.addr])).toEqual([["newhub000001", "Windows PC", "windows", "192.168.1.9:47801"]]);
    const got = d.skipped.map((x) => x.reason);
    expect(got).toEqual(cases.filter((c) => c[1] !== "accepted").map((c) => c[1]));
    // A refused name is never echoed back (it would reach a notice).
    expect(d.skipped.find((x) => x.id === "bidi00000001")?.name).toBe("");
  });

  test("more than 32 devices is dropped whole", () => {
    const many = Array.from({ length: 33 }, (_, i) => dev(`hub${String(i).padStart(9, "0")}`, DeviceKeys.generate(random).publicB64));
    const d = evaluateIntro(ctx, many);
    expect(d.accepted).toEqual([]);
    expect(d.skipped[0]?.reason).toBe("too_many");
  });

  test("hello retry schedule", () => {
    expect([0, 1, 2, 3, 4, 10].map(helloRetryDelay)).toEqual([1000, 2000, 5000, 30000, 30000, 30000]);
  });
});

describe("device name rule (Rust device_name_ok)", () => {
  const ch = (cp: number) => String.fromCodePoint(cp);
  test("accepts ordinary names, refuses control / invisible format characters and bad lengths", () => {
    for (const ok of ["Mac mini", "  Windows PC  ", "Sam's iPhone \u{1F436}", "Ünïcødé", "x".repeat(64), "\u{1F436}".repeat(64)]) {
      expect(deviceNameOk(ok)).toBe(true);
    }
    const forbidden = [0x00, 0x0a, 0x1f, 0x7f, 0x85, 0x9f, 0x061c, 0x200b, 0x200c, 0x200d, 0x200e, 0x200f, 0x2028, 0x2029, 0x202a, 0x202e, 0x2060, 0x2064, 0x2066, 0x2069, 0xfeff];
    for (const cp of forbidden) expect(`${cp.toString(16)}: ${deviceNameOk(`a${ch(cp)}b`)}`).toBe(`${cp.toString(16)}: false`);
    for (const bad of ["", "   ", "x".repeat(65), "\u{1F436}".repeat(65), 42, null]) expect(deviceNameOk(bad)).toBe(false);
    expect(cleanDeviceName("  Mac  ")).toBe("Mac");
    expect(cleanDeviceName("　Mac ")).toBe("Mac");
    expect(deviceNameOk("x".repeat(41), 40)).toBe(false);
    expect(stripNameChars(`Pi${ch(0x200b)}xel${ch(0x202e)} 9${ch(0x0a)}`)).toBe("Pixel 9");
  });
});

describe("mirror caps", () => {
  test("distinct event sessions, pending and pets are bounded per hub (oldest out)", () => {
    let m = emptyMirror(M);
    const events = Array.from({ length: MIRROR_MAX_EVENT_SESSIONS + 20 }, (_, i) => sampleEvent(M, i + 1, `s${i}`));
    m = applyMirrorBody(m, { type: "mirror_events", events }, 1);
    expect(Object.keys(m.state.events).length).toBe(MIRROR_MAX_EVENT_SESSIONS);
    expect(m.state.events[`${M}:s0`]).toBeUndefined();
    expect(m.state.events[`${M}:s${MIRROR_MAX_EVENT_SESSIONS + 19}`]).toBeDefined();
    // Live events for brand-new sessions push the oldest out too.
    m = applyMirrorBody(m, { type: "mirror", update: { type: "event", event: sampleEvent(M, 10_000, "fresh") } }, 1);
    expect(Object.keys(m.state.events).length).toBe(MIRROR_MAX_EVENT_SESSIONS);
    expect(m.state.events[`${M}:fresh`]).toBeDefined();

    const reqs = Array.from({ length: MIRROR_MAX_PENDING + 5 }, (_, i) => ({ ...sampleRequest(M, `r${i}`), createdAt: i }));
    m = applyMirrorBody(m, { type: "mirror_snapshot", sessions: [], pets: [], pendingPermissions: reqs }, 2);
    expect(Object.keys(m.state.pending).length).toBe(MIRROR_MAX_PENDING);
    expect(m.state.pending[`${M}:r0`]).toBeUndefined();
    m = applyMirrorBody(m, { type: "mirror", update: { type: "permission_request", request: { ...sampleRequest(M, "newest"), createdAt: 1e9 } } }, 3);
    expect(Object.keys(m.state.pending).length).toBe(MIRROR_MAX_PENDING);
    expect(m.state.pending[`${M}:newest`]).toBeDefined();

    const pet = (i: number) => ({ id: `${M}:pet-${i}`, machineId: M, agentId: `a${i}`, species: "dog" as const, skin: "default", name: `P${i}`, personality: "", mood: 50, xp: 0 });
    for (let i = 0; i < MIRROR_MAX_PETS + 3; i++) m = applyMirrorBody(m, { type: "mirror", update: { type: "pet", pet: pet(i) } }, 4);
    expect(Object.keys(m.state.pets).length).toBe(MIRROR_MAX_PETS);
    expect(m.state.pets[`${M}:pet-0`]).toBeUndefined();
    expect(m.state.pets[`${M}:pet-${MIRROR_MAX_PETS + 2}`]).toBeDefined();
  });
});

describe("intent helpers", () => {
  test("approve = allow_once only; deny = reject_once else null", () => {
    const req = sampleRequest(M);
    expect(approveOption(req)).toBe("allow");
    expect(denyOption(req)).toBe("reject");
    expect(approveOption({ options: req.options.filter((o) => o.kind !== "allow_once") })).toBeNull();
    expect(denyOption({ options: [] })).toBeNull();
  });

  test("error text", () => {
    expect(phoneIntentErrorText(new IntentError("offline", M, "approve"), "Mac mini", "approve")).toContain("Mac mini is offline");
    expect(phoneIntentErrorText(new IntentError("already_decided", M, "approve"), "Mac mini", "approve")).toBe("Already answered.");
    expect(phoneIntentErrorText(new IntentError("no_allow_once", M, "approve"), "Mac mini", "approve")).toContain("Mac mini");
  });
});

function mockAuth(cap: Partial<AuthCapability>, results: ({ success: true } | { success: false; error: string })[] = []): Authenticator & { calls: { reason: string; allowPasscode: boolean }[] } {
  const calls: { reason: string; allowPasscode: boolean }[] = [];
  return {
    calls,
    capability: async () => ({ hasHardware: false, isEnrolled: false, securityLevel: 0, kinds: [], ...cap }),
    authenticate: async (o) => {
      calls.push(o);
      return results.shift() ?? { success: false, error: "unknown" };
    },
  };
}

describe("biometric gate", () => {
  test("availability: biometrics, passcode, neither", () => {
    expect(gateAvailability({ hasHardware: true, isEnrolled: true, securityLevel: 3, kinds: ["face"] }, "ios")).toEqual({ available: true, method: "biometric", label: "Face ID" });
    expect(gateAvailability({ hasHardware: true, isEnrolled: true, securityLevel: 2, kinds: ["fingerprint"] }, "android")).toMatchObject({ method: "biometric", label: "fingerprint" });
    expect(gateAvailability({ hasHardware: true, isEnrolled: false, securityLevel: 1, kinds: ["fingerprint"] })).toMatchObject({ available: true, method: "passcode" });
    const none = gateAvailability({ hasHardware: false, isEnrolled: false, securityLevel: 0, kinds: [] });
    expect(none.available).toBe(false);
    expect(!none.available && none.explanation).toContain("screen lock");
  });

  test("confirm only on success === true", async () => {
    const ok = mockAuth({ hasHardware: true, isEnrolled: true, securityLevel: 3, kinds: ["face"] }, [{ success: true }]);
    expect(await new ApprovalGate(ok, "ios").confirm("Approve on Mac mini")).toEqual({ confirmed: true, method: "biometric" });
    expect(ok.calls[0]).toEqual({ reason: "Approve on Mac mini", allowPasscode: true });
    const cancel = mockAuth({ securityLevel: 1 }, [{ success: false, error: "user_cancel" }]);
    expect(await new ApprovalGate(cancel).confirm("x")).toMatchObject({ confirmed: false, code: "cancelled" });
    const lock = mockAuth({ hasHardware: true, isEnrolled: true, securityLevel: 3 }, [{ success: false, error: "lockout" }]);
    expect(await new ApprovalGate(lock).confirm("x")).toMatchObject({ confirmed: false, code: "lockout" });
    const fail = mockAuth({ securityLevel: 1 }, [{ success: false, error: "authentication_failed" }]);
    expect(await new ApprovalGate(fail).confirm("x")).toMatchObject({ confirmed: false, code: "failed" });
    const weird = mockAuth({ securityLevel: 1 }, [{ success: "yes" } as unknown as { success: true }]);
    expect((await new ApprovalGate(weird).confirm("x")).confirmed).toBe(false);
    const none = mockAuth({});
    expect(await new ApprovalGate(none).confirm("x")).toMatchObject({ confirmed: false, code: "unavailable" });
    expect(none.calls.length).toBe(0);
    const throws: Authenticator = { capability: async () => { throw new Error("boom"); }, authenticate: async () => ({ success: true }) };
    expect((await new ApprovalGate(throws).confirm("x")).confirmed).toBe(false);
  });

  test("one prompt at a time: a second call is refused (busy), never handed the first one's success", async () => {
    let release!: () => void;
    let prompts = 0;
    const auth: Authenticator = {
      capability: async () => ({ hasHardware: true, isEnrolled: true, securityLevel: 3, kinds: [] }),
      authenticate: () => {
        prompts++;
        return new Promise((r) => (release = () => r({ success: true })));
      },
    };
    const gate = new ApprovalGate(auth);
    const a = gate.confirm("Approve on Mac mini: run tests");
    const b = gate.confirm("Approve on Mac mini: rm -rf");
    expect(await b).toMatchObject({ confirmed: false, code: "busy" });
    await new Promise((r) => setTimeout(r, 1));
    release();
    expect((await a).confirmed).toBe(true);
    expect(prompts).toBe(1);
    // Free again afterwards.
    const c = gate.confirm("next");
    await new Promise((r) => setTimeout(r, 1));
    release();
    expect((await c).confirmed).toBe(true);
  });
});

describe("relay client", () => {
  function client(relay: FakeRelay, clock: FakeClock, keys = DeviceKeys.generate(seededRandom(51)), refused: Uint8Array[] = []) {
    const statuses: RelayStatusInfo[] = [];
    let connected = 0;
    const c = new RelayClient({
      keys,
      socketFactory: relay.factory,
      clock,
      random: seededRandom(52),
      refusedRelayKeys: () => refused,
      allowList: () => ["A".repeat(43)],
      pushToken: () => "ExponentPushToken[abc]",
      onMessage: () => {},
      onError: () => {},
      onConnected: () => connected++,
      onDisconnected: () => {},
      onStatus: (s) => statuses.push(s),
    });
    return { c, statuses, connected: () => connected, keys };
  }

  test("auth, allow and register_push on every session; reconnects with backoff after a relay restart", async () => {
    const clock = new FakeClock();
    const relay = new FakeRelay();
    const { c, connected } = client(relay, clock);
    c.setConfig({ url: relay.url, accessKey: relay.accessKey });
    await clock.advance(10);
    expect(c.connected).toBe(true);
    expect(relay.framesOf("allow").length).toBe(1);
    expect(relay.framesOf("register_push")[0]).toEqual({ t: "register_push", expoToken: "ExponentPushToken[abc]" });
    relay.restart();
    await clock.advance(10);
    expect(c.connected).toBe(false);
    expect(c.status.lastError).toBe("closed");
    await clock.advance(1_100);
    expect(c.connected).toBe(true);
    expect(connected()).toBe(2);
    expect(relay.framesOf("allow").length).toBe(2);
  });

  test("refuses a challenge nonce with byte 15 < 2, and its own or a roster key as relayPk", async () => {
    const clock = new FakeClock();
    for (const setup of ["nonce0", "nonce1", "own", "roster"] as const) {
      const relay = new FakeRelay();
      const peer = DeviceKeys.generate(seededRandom(61));
      const { c, keys } = client(relay, clock, DeviceKeys.generate(seededRandom(62)), [peer.publicKey]);
      if (setup === "nonce0") relay.nonceByte15 = 0;
      if (setup === "nonce1") relay.nonceByte15 = 1;
      if (setup === "own") relay.relayPkOverride = keys.publicKey;
      if (setup === "roster") relay.relayPkOverride = peer.publicKey;
      c.setConfig({ url: relay.url, accessKey: relay.accessKey });
      await clock.advance(10);
      expect(c.connected).toBe(false);
      expect(c.status.lastError).toBe("bad_auth");
      expect(relay.framesOf("auth").length).toBe(0);
      c.stop();
    }
  });

  test("wrong access key: unreachable, retried with growing backoff", async () => {
    const clock = new FakeClock();
    const relay = new FakeRelay();
    const { c } = client(relay, clock);
    c.setConfig({ url: relay.url, accessKey: "nope" });
    await clock.advance(10);
    expect(c.status.lastError).toBe("unreachable");
    relay.refuseConnections = true;
    await clock.advance(120_000);
    expect(c.connected).toBe(false);
    relay.refuseConnections = false;
    c.setConfig({ url: relay.url, accessKey: relay.accessKey });
    await clock.advance(10);
    expect(c.connected).toBe(true);
    c.stop();
    expect(c.status).toEqual({ url: null, connected: false });
  });

  test("silent socket is dropped after 45 s", async () => {
    const clock = new FakeClock();
    const relay = new FakeRelay();
    const { c, statuses } = client(relay, clock);
    c.setConfig({ url: relay.url, accessKey: relay.accessKey });
    await clock.advance(10);
    // The relay stops answering pings (half-open socket).
    for (const s of relay.sockets.keys()) s.deliver = () => {};
    await clock.advance(61_000);
    expect(statuses.some((st) => st.lastError === "timeout")).toBe(true);
  });
});
