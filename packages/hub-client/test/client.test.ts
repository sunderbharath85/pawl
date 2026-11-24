import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import {
  HubClient,
  HubRequestError,
  JOIN_PEER_TIMEOUT_MS,
  MACHINE_NAME_MAX,
  normalizeMachineName,
  pickPermissionOption,
  type HubClientOptions,
} from "../src/index";
import { USER_ACTIVITY_REPORT_MS } from "@pawl/protocol";
import { device2, event, FakeSocket, inviteFrom2, machine, machine2, MID, MID2, peerOff, peerOn, permission, session, tick, welcome } from "./fixtures";

function makeClient(over: Partial<HubClientOptions> = {}): HubClient {
  return new HubClient({
    token: "secret",
    client: "desktop",
    clientVersion: "1.2.3",
    WebSocketImpl: FakeSocket,
    minReconnectDelayMs: 5,
    maxReconnectDelayMs: 20,
    requestTimeoutMs: 200,
    ...over,
  });
}

const clients: HubClient[] = [];
function client(over: Partial<HubClientOptions> = {}): HubClient {
  const c = makeClient(over);
  clients.push(c);
  return c;
}

beforeEach(() => FakeSocket.reset());
afterEach(() => {
  for (const c of clients.splice(0)) c.close();
});

describe("handshake", () => {
  test("sends hello first and applies welcome", () => {
    const c = client();
    const statuses: string[] = [];
    c.subscribe((s) => statuses.push(s.status));
    c.connect();
    expect(c.getState().status).toBe("connecting");
    const ws = FakeSocket.last();
    expect(ws.url).toBe("ws://127.0.0.1:47800/ws");
    ws.open();
    expect(ws.sent).toHaveLength(1);
    expect(ws.sent[0]).toMatchObject({ type: "hello", client: "desktop", clientVersion: "1.2.3", token: "secret" });
    ws.receive({ type: "ack", id: ws.sent[0].id, ok: true, result: welcome });
    const s = c.getState();
    expect(s.status).toBe("connected");
    expect(s.pets.p1?.name).toBe("Biscuit");
    expect(statuses).toContain("connecting");
    expect(statuses[statuses.length - 1]).toBe("connected");
  });

  test("normalizes http URL to ws endpoint", () => {
    const c = client({ url: "http://127.0.0.1:9999" });
    c.connect();
    expect(FakeSocket.last().url).toBe("ws://127.0.0.1:9999/ws");
  });

  test("unauthorized stops reconnecting and rejects requests", async () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    const queued = c.prompt("s1", "hi");
    ws.open();
    ws.receive({ type: "ack", id: ws.sent[0].id, ok: false, error: "unauthorized" });
    expect(c.getState().status).toBe("unauthorized");
    await expect(queued).rejects.toMatchObject({ code: "unauthorized" });
    await tick(40);
    expect(FakeSocket.instances).toHaveLength(1);
    await expect(c.listAgents()).rejects.toBeInstanceOf(HubRequestError);
  });

  test("setToken after unauthorized reconnects with the new token", () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.open();
    ws.receive({ type: "ack", id: ws.sent[0].id, ok: false, error: "unauthorized" });
    c.setToken("better");
    const ws2 = FakeSocket.last();
    expect(ws2).not.toBe(ws);
    ws2.open();
    expect(ws2.sent[0]).toMatchObject({ type: "hello", token: "better" });
  });

  test("hello timeout drops the socket and reconnects", async () => {
    const c = client({ requestTimeoutMs: 20 });
    c.connect();
    FakeSocket.last().open();
    await tick(60);
    expect(FakeSocket.instances.length).toBeGreaterThanOrEqual(2);
    expect(c.getState().lastError).toBeDefined();
  });
});

describe("requests", () => {
  test("correlates acks by id and resolves results", async () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    const p1 = c.startSession("claude-code", "/repo", "go");
    const p2 = c.listAgents(true);
    const [m1, m2] = ws.sent.slice(1);
    expect(m1).toMatchObject({ type: "start_session", agentId: "claude-code", cwd: "/repo", prompt: "go" });
    expect(m2).toMatchObject({ type: "list_agents", refresh: true });
    expect(m1.id).not.toBe(m2.id);
    // answer out of order
    ws.receive({ type: "ack", id: m2.id, ok: true, result: { agents: [] } });
    ws.receive({ type: "ack", id: m1.id, ok: true, result: { session: session("s2") } });
    expect((await p1).id).toBe("s2");
    expect(await p2).toEqual([]);
    expect(c.getState().sessions.s2).toBeDefined();
    expect(c.getState().agents).toEqual([]);
  });

  test("omits undefined optional fields", () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    void c.startSession("claude-code", "/repo").catch(() => {});
    void c.chat("hello").catch(() => {});
    expect("prompt" in ws.sent[1]).toBe(false);
    expect("petId" in ws.sent[2]).toBe(false);
  });

  test("ok:false rejects with the hub error code", async () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    const p = c.prompt("nope", "x");
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: false, error: "unknown_session" });
    await expect(p).rejects.toMatchObject({ code: "unknown_session", requestType: "prompt" });
  });

  test("times out", async () => {
    const c = client({ requestTimeoutMs: 15 });
    c.connect();
    FakeSocket.last().handshake();
    await expect(c.cancel("s1")).rejects.toMatchObject({ code: "timeout" });
  });

  test("queues requests until the hello completes", async () => {
    const c = client();
    c.connect();
    const p = c.chat("hi", "p1");
    const ws = FakeSocket.last();
    ws.open();
    expect(ws.sent.map((m) => m.type)).toEqual(["hello"]);
    ws.receive({ type: "ack", id: ws.sent[0].id, ok: true, result: welcome });
    expect(ws.sent.map((m) => m.type)).toEqual(["hello", "chat"]);
    ws.receive({ type: "ack", id: ws.sent[1].id, ok: true, result: { message: { id: "m1", role: "user", text: "hi", ts: 1 } } });
    expect((await p).id).toBe("m1");
    expect(c.getState().chat).toHaveLength(1);
  });

  test("rejects immediately when never connected", async () => {
    const c = client();
    await expect(c.prompt("s1", "x")).rejects.toMatchObject({ code: "closed" });
  });

  test("in-flight requests reject on disconnect; queued ones survive reconnect", async () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    const inflight = c.prompt("s1", "x");
    ws.serverClose();
    await expect(inflight).rejects.toMatchObject({ code: "disconnected" });
    expect(c.getState().status).toBe("disconnected");
    const queued = c.cancel("s1");
    await tick(40);
    const ws2 = FakeSocket.last();
    expect(ws2).not.toBe(ws);
    ws2.handshake();
    const cancelMsg = ws2.sent.find((m) => m.type === "cancel");
    expect(cancelMsg).toBeDefined();
    ws2.receive({ type: "ack", id: cancelMsg.id, ok: true, result: {} });
    await expect(queued).resolves.toBeUndefined();
  });

  test("close() rejects everything and stops reconnecting", async () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    const p = c.prompt("s1", "x");
    c.close();
    await expect(p).rejects.toMatchObject({ code: "closed" });
    expect(c.getState().status).toBe("disconnected");
    await tick(40);
    expect(FakeSocket.instances).toHaveLength(1);
  });

  test("getEvents merges into state", async () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    const p = c.getEvents("s1", 2);
    expect(ws.lastSent()).toMatchObject({ type: "get_events", sessionId: "s1", sinceSeq: 2 });
    expect(ws.lastSent()).not.toHaveProperty("sinceId");
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { events: [event(3), event(4)] } });
    expect((await p).map((e) => e.seq)).toEqual([3, 4]);
    expect(c.getState().events.s1!.map((e) => e.id)).toEqual([`${MID}:3`, `${MID}:4`]);
  });
});

describe("permissions", () => {
  test("approve picks allow_once, deny picks reject_once", async () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    const a = c.approve("r1");
    expect(ws.lastSent()).toMatchObject({ type: "resolve_permission", requestId: "r1", optionId: "o-allow" });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: {} });
    await a;
    expect(c.getState().pending.r1).toBeUndefined();

    ws.receive({ type: "permission_request", request: permission("r2") });
    const d = c.deny("r2");
    expect(ws.lastSent()).toMatchObject({ type: "resolve_permission", requestId: "r2", optionId: "o-reject" });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: {} });
    await d;
  });

  test("deny without reject option sends null; approve unknown rejects", async () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    ws.receive({ type: "permission_request", request: permission("r3", { options: [{ optionId: "x", name: "ok", kind: "allow_always" }] }) });
    const d = c.deny("r3");
    expect(ws.lastSent()).toMatchObject({ type: "resolve_permission", requestId: "r3", optionId: null });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: {} });
    await d;
    await expect(c.approve("missing")).rejects.toMatchObject({ code: "unknown_request" });
  });

  test("pickPermissionOption falls back to allow_always", () => {
    expect(pickPermissionOption({ options: [{ optionId: "x", name: "ok", kind: "allow_always" }] }, "allow")).toBe("x");
    expect(pickPermissionOption({ options: [] }, "reject")).toBeNull();
  });
});

describe("listeners", () => {
  test("onMessage fires per type after state is reduced; unsubscribe works", () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    const seen: string[] = [];
    const off = c.onMessage("session", (m) => {
      seen.push(`${m.session.id}:${c.getState().sessions[m.session.id]?.status}`);
    });
    ws.receive({ type: "session", session: session("s1", { status: "working" }) });
    off();
    ws.receive({ type: "session", session: session("s1", { status: "done" }) });
    expect(seen).toEqual(["s1:working"]);
  });

  test("subscribe gets every message; throwing listener does not break others", () => {
    const c = client();
    const orig = console.error;
    console.error = () => {};
    const off = c.subscribe(() => {
      throw new Error("bad listener");
    });
    try {
      const msgs: string[] = [];
      c.subscribe((_s, m) => m && msgs.push(m.type));
      c.connect();
      const ws = FakeSocket.last();
      ws.handshake();
      ws.receive({ type: "event", event: event(1) });
      ws.receiveBytes({ type: "notification", notification: { id: "n1", level: "info", title: "t", body: "b", ts: 1 } });
      ws.receive("not json");
      expect(msgs).toEqual(["ack", "event", "notification"]);
      expect(c.getState().notifications).toHaveLength(1);
    } finally {
      off();
      console.error = orig;
    }
  });

  test("state identity changes only on change", () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    const before = c.getState();
    ws.receive({ type: "permission_resolved", requestId: "zzz", sessionId: "s1", outcome: { kind: "cancelled" }, by: "rule" });
    expect(c.getState()).toBe(before);
    ws.receive({ type: "event", event: event(1) });
    expect(c.getState()).not.toBe(before);
  });
});

describe("reconnect", () => {
  test("reconnects with backoff after server close, resets after welcome", async () => {
    const c = client();
    c.connect();
    FakeSocket.last().handshake();
    FakeSocket.last().serverClose();
    await tick(40);
    expect(FakeSocket.instances.length).toBe(2);
    FakeSocket.last().handshake();
    expect(c.getState().status).toBe("connected");
  });

  test("keeps retrying when the hub is down", async () => {
    const c = client();
    c.connect();
    FakeSocket.last().serverClose();
    await tick(15);
    FakeSocket.last().serverClose();
    await tick(40);
    FakeSocket.last().serverClose();
    await tick(60);
    expect(FakeSocket.instances.length).toBeGreaterThanOrEqual(3);
  });

  test("constructor throw is treated as a failed attempt", async () => {
    FakeSocket.failConstruct = true;
    const c = client();
    c.connect();
    expect(c.getState().status).toBe("disconnected");
    expect(c.getState().lastError).toBe("boom");
    FakeSocket.failConstruct = false;
    await tick(40);
    expect(FakeSocket.instances.length).toBe(1);
  });

  test("reconnect:false stays down", async () => {
    const c = client({ reconnect: false });
    c.connect();
    FakeSocket.last().handshake();
    FakeSocket.last().serverClose();
    await tick(40);
    expect(FakeSocket.instances).toHaveLength(1);
    expect(c.getState().status).toBe("disconnected");
  });

  test("missing WebSocket implementation reports an error", () => {
    const g = globalThis as { WebSocket?: unknown };
    const saved = g.WebSocket;
    g.WebSocket = undefined;
    try {
      const c = client({ WebSocketImpl: undefined });
      c.connect();
      expect(c.getState().status).toBe("disconnected");
      expect(c.getState().lastError).toContain("WebSocket");
    } finally {
      g.WebSocket = saved;
    }
  });
});

describe("machine identity", () => {
  test("welcome exposes this hub's machine; `machine` messages update state and fire listeners", () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    expect(c.getState().machine).toEqual(machine);
    expect(c.getState().machines[MID]).toEqual(machine);
    const seen: string[] = [];
    c.onMessage("machine", (m) => seen.push(m.machine.name));
    ws.receive({ type: "machine", machine: machine2 });
    ws.receive({ type: "machine", machine: { ...machine, name: "Gaming PC" } });
    expect(seen).toEqual(["Mac mini", "Gaming PC"]);
    expect(c.getState().machine?.name).toBe("Gaming PC");
    expect(Object.keys(c.getState().machines).sort()).toEqual([MID, MID2].sort());
  });

  test("updateMachine sends the local machine id with a trimmed name and folds the ack", async () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    const p = c.updateMachine("  Windows Desk  ");
    await tick();
    const sent = ws.lastSent();
    expect(sent).toMatchObject({ type: "update_machine", machineId: MID, name: "Windows Desk" });
    ws.receive({ type: "ack", id: sent.id, ok: true, result: { machine: { ...machine, name: "Windows Desk" } } });
    expect((await p).name).toBe("Windows Desk");
    expect(c.getState().machine?.name).toBe("Windows Desk");
  });

  test("updateMachine called while connecting waits for the welcome", async () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    const p = c.updateMachine("Desk");
    await tick();
    expect(ws.sent.filter((m) => m.type === "update_machine")).toHaveLength(0);
    ws.handshake();
    await tick();
    const sent = ws.lastSent();
    expect(sent).toMatchObject({ type: "update_machine", machineId: MID, name: "Desk" });
    ws.receive({ type: "ack", id: sent.id, ok: true, result: { machine: { ...machine, name: "Desk" } } });
    expect((await p).name).toBe("Desk");
  });

  test("updateMachine surfaces hub errors (e.g. not_owner)", async () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    const p = c.updateMachine("Desk");
    await tick();
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: false, error: "not_owner" });
    await expect(p).rejects.toMatchObject({ code: "not_owner", requestType: "update_machine" });
  });

  test("updateMachine rejects invalid names locally without sending", async () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    const before = ws.sent.length;
    await expect(c.updateMachine("   ")).rejects.toMatchObject({ code: "invalid" });
    await expect(c.updateMachine("x".repeat(MACHINE_NAME_MAX + 1))).rejects.toBeInstanceOf(HubRequestError);
    expect(ws.sent.length).toBe(before);
  });

  test("updateMachine rejects when not connected, on a pre-v2 hub, or when closed while waiting", async () => {
    const idle = client();
    await expect(idle.updateMachine("Desk")).rejects.toMatchObject({ code: "closed" });

    const legacy = client();
    legacy.connect();
    FakeSocket.last().handshake({ ...welcome, machine: undefined, machines: undefined } as never);
    await expect(legacy.updateMachine("Desk")).rejects.toMatchObject({ code: "no_machine" });

    const waiting = client();
    waiting.connect();
    const p = waiting.updateMachine("Desk");
    waiting.close();
    await expect(p).rejects.toMatchObject({ code: "closed" });
  });

  test("updateMachine times out if the welcome never arrives", async () => {
    const c = client({ requestTimeoutMs: 30, reconnect: false });
    c.connect();
    await expect(c.updateMachine("Desk")).rejects.toMatchObject({ code: "timeout" });
  });

  test("normalizeMachineName trims and bounds by code points", () => {
    expect(normalizeMachineName("  Mac mini ")).toBe("Mac mini");
    expect(normalizeMachineName("")).toBeNull();
    expect(normalizeMachineName("🐶".repeat(MACHINE_NAME_MAX))).toBe("🐶".repeat(MACHINE_NAME_MAX));
    expect(normalizeMachineName("a".repeat(MACHINE_NAME_MAX + 1))).toBeNull();
  });
});

describe("pairing (protocol v3)", () => {
  function connected(over: Partial<HubClientOptions> = {}) {
    const c = client(over);
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    return { c, ws };
  }

  test("welcome, devices, peer_status and machine_removed update state", () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake({ ...welcome, machines: [machine, machine2], devices: [device2], peer: peerOff });
    expect(c.getState().devices).toEqual([device2]);
    expect(c.getState().peer).toEqual(peerOff);
    ws.receive({ type: "peer_status", peer: peerOn });
    expect(c.getState().peer?.listening).toBe("192.168.1.10:47801");
    ws.receive({ type: "machine_removed", machineId: MID2 });
    expect(c.getState().machines[MID2]).toBeUndefined();
    expect(c.getState().devices).toEqual([]);
    ws.receive({ type: "devices", devices: [device2] });
    expect(c.getState().devices).toEqual([device2]);
  });

  test("createInvite sends only the given options and resolves with the URI", async () => {
    const { c, ws } = connected();
    const p = c.createInvite();
    const sent = ws.lastSent();
    expect(sent).toEqual({ type: "create_invite", id: sent.id });
    ws.receive({ type: "ack", id: sent.id, ok: true, result: { uri: inviteFrom2, expiresAt: 123 } });
    expect(await p).toEqual({ uri: inviteFrom2, expiresAt: 123 });

    const p2 = c.createInvite({ ttlSecs: 120, addr: " 192.168.1.10:47801 " });
    expect(ws.lastSent()).toMatchObject({ type: "create_invite", ttlSecs: 120, addr: "192.168.1.10:47801" });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { uri: inviteFrom2, expiresAt: 1 } });
    await p2;
  });

  test("createInvite checks ttl and addr locally; hub errors keep their code", async () => {
    const { c, ws } = connected();
    const before = ws.sent.length;
    await expect(c.createInvite({ ttlSecs: 59 })).rejects.toMatchObject({ code: "invalid", requestType: "create_invite" });
    await expect(c.createInvite({ ttlSecs: 3601 })).rejects.toMatchObject({ code: "invalid" });
    await expect(c.createInvite({ ttlSecs: 90.5 })).rejects.toMatchObject({ code: "invalid" });
    await expect(c.createInvite({ addr: "my host:1" })).rejects.toMatchObject({ code: "invalid" });
    expect(ws.sent.length).toBe(before);
    const p = c.createInvite({ addr: "  " }); // a blank addr is omitted, not an error
    expect(ws.lastSent()).toEqual({ type: "create_invite", id: ws.lastSent().id });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: false, error: "peer_listener_off" });
    await expect(p).rejects.toMatchObject({ code: "peer_listener_off" });
  });

  test("createInvite forPhone: relay-only, v8 hubs only, never with an address", async () => {
    const phoneUri = `pawl://peer?v=1&mid=${MID}&pk=cGstd2lucGM&relay=wss://relay.example.dev&rt=AAAA&rk=k3y_SECRET&code=AAAAAAAAAAAAAAAAAAAAAA&exp=1900000000000`;
    // The welcome fixture is protocol v6: refused before anything is sent.
    const old = connected();
    const before = old.ws.sent.length;
    await expect(old.c.createInvite({ forPhone: true })).rejects.toMatchObject({ code: "unsupported", requestType: "create_invite" });
    expect(old.ws.sent.length).toBe(before);

    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake({ ...welcome, protocolVersion: 8 });
    await expect(c.createInvite({ forPhone: true, addr: "192.168.1.10:47801" })).rejects.toMatchObject({ code: "invalid" });
    const p = c.createInvite({ forPhone: true, ttlSecs: 300 });
    expect(ws.lastSent()).toEqual({ type: "create_invite", id: ws.lastSent().id, ttlSecs: 300, forPhone: true });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { uri: phoneUri, expiresAt: 9 } });
    expect(await p).toEqual({ uri: phoneUri, expiresAt: 9 });

    // A LAN invite cannot be scanned by a phone: refused rather than shown as a QR code.
    const lan = c.createInvite({ forPhone: true });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { uri: inviteFrom2, expiresAt: 9 } });
    await expect(lan).rejects.toMatchObject({ code: "unsupported" });

    const off = c.createInvite({ forPhone: true });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: false, error: "relay_off" });
    await expect(off).rejects.toMatchObject({ code: "relay_off" });
  });

  test("joinPeer sends the trimmed URI and folds the new peer into state", async () => {
    const { c, ws } = connected();
    const p = c.joinPeer(`  ${inviteFrom2}\n`);
    const sent = ws.lastSent();
    expect(sent).toEqual({ type: "join_peer", id: sent.id, uri: inviteFrom2 });
    ws.receive({ type: "ack", id: sent.id, ok: true, result: { device: device2, machine: { ...machine2, online: true } } });
    const res = await p;
    expect(res.device.id).toBe(MID2);
    expect(c.getState().devices).toEqual([device2]);
    expect(c.getState().machines[MID2]?.online).toBe(true);
  });

  test("joinPeer rejects bad and self invites locally; hub errors keep their code", async () => {
    const { c, ws } = connected();
    const before = ws.sent.length;
    await expect(c.joinPeer("hello")).rejects.toMatchObject({ code: "bad_uri", requestType: "join_peer" });
    await expect(c.joinPeer(inviteFrom2.replace(MID2, MID))).rejects.toMatchObject({ code: "self_invite" });
    expect(ws.sent.length).toBe(before);
    for (const code of ["unreachable", "invite_rejected", "timeout", "bad_uri"]) {
      const p = c.joinPeer(inviteFrom2);
      ws.receive({ type: "ack", id: ws.lastSent().id, ok: false, error: code });
      await expect(p).rejects.toMatchObject({ code });
    }
  });

  test("joinPeer waits longer than an ordinary request (the hub dials for up to 10 s)", async () => {
    const { c, ws } = connected({ requestTimeoutMs: 20 });
    let settled = false;
    const p = c.joinPeer(inviteFrom2).finally(() => (settled = true));
    await tick(60);
    expect(settled).toBe(false);
    expect(JOIN_PEER_TIMEOUT_MS).toBeGreaterThanOrEqual(15_000);
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: false, error: "unreachable" });
    await expect(p).rejects.toMatchObject({ code: "unreachable" });
  });

  test("revokeDevice sends the device id and drops it on ok", async () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake({ ...welcome, machines: [machine, machine2], devices: [device2] });
    const p = c.revokeDevice(MID2);
    const sent = ws.lastSent();
    expect(sent).toEqual({ type: "revoke_device", id: sent.id, deviceId: MID2 });
    ws.receive({ type: "ack", id: sent.id, ok: true, result: {} });
    await p;
    expect(c.getState().devices).toEqual([]);
    expect(c.getState().machines[MID2]).toBeUndefined();

    const missing = c.revokeDevice("gone00000000");
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: false, error: "not_found" });
    await expect(missing).rejects.toMatchObject({ code: "not_found", requestType: "revoke_device" });
  });

  test("revokeDevice returns the devices the hub introduced; revokeDevices goes through them all", async () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake({ ...welcome, protocolVersion: 8, machines: [machine, machine2], devices: [device2] });
    const introduced = [
      { id: "iphone000001", name: "iPhone", kind: "phone" as const },
      { id: "pixel0000001", name: "Pixel", kind: "phone" as const },
      { id: "linuxbox0003", name: "Linux box", kind: "hub" as const },
    ];
    const p = c.revokeDevice(MID2);
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { introduced } });
    expect(await p).toEqual({ introduced });
    // An older hub acks without a result.
    const old = c.revokeDevice("other0000001");
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true });
    expect(await old).toEqual({});

    const all = c.revokeDevices(introduced);
    const answer = async (error?: string) => {
      await tick();
      const sent = ws.lastSent();
      expect(sent.type).toBe("revoke_device");
      ws.receive(error ? { type: "ack", id: sent.id, ok: false, error } : { type: "ack", id: sent.id, ok: true, result: {} });
      return sent.deviceId as string;
    };
    // One after another: ok, already gone, refused.
    expect(await answer()).toBe("iphone000001");
    expect(await answer("not_found")).toBe("pixel0000001");
    expect(await answer("forbidden")).toBe("linuxbox0003");
    expect(await all).toEqual({ removed: introduced.slice(0, 2), failed: [{ ...introduced[2]!, code: "forbidden" }] });
  });

  test("errors never echo the invite URI", async () => {
    const { c, ws } = connected();
    const p = c.joinPeer(inviteFrom2);
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: false, error: "invite_rejected" });
    const err = (await p.catch((e: unknown) => e)) as Error;
    expect(err.message).not.toContain("pawl://");
    expect(err.message).not.toContain("AAAAAAAAAAAAAAAAAAAAAA");
    const local = (await c.joinPeer(inviteFrom2.replace("v=1", "v=0")).catch((e: unknown) => e)) as Error;
    expect(local).toMatchObject({ code: "bad_uri" });
    expect(local.message).not.toContain("AAAAAAAAAAAAAAAAAAAAAA");
    expect(local.message).not.toContain("pawl://");
    expect(c.getState().lastError ?? "").not.toContain("pawl://");
  });
});

describe("user activity (protocol v4)", () => {
  test("reportUserActivity sends user_activity at most once per USER_ACTIVITY_REPORT_MS", () => {
    const c = client();
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake();
    const sent = () => ws.sent.filter((m) => m.type === "user_activity");
    expect(c.reportUserActivity(1_000_000)).toBe(true);
    expect(sent()).toHaveLength(1);
    expect(sent()[0]).toEqual({ type: "user_activity", id: sent()[0].id });
    expect(c.reportUserActivity(1_000_000 + USER_ACTIVITY_REPORT_MS - 1)).toBe(false);
    expect(c.reportUserActivity(1_000_000 + USER_ACTIVITY_REPORT_MS)).toBe(true);
    expect(sent()).toHaveLength(2);
    // A rejected report (older hub) is swallowed.
    ws.receive({ type: "ack", id: sent()[1].id, ok: false, error: "unknown_type" });
  });

  test("nothing is sent or queued while disconnected; a new connection reports at once", async () => {
    const c = client();
    expect(c.reportUserActivity(5)).toBe(false);
    c.connect();
    const ws = FakeSocket.last();
    expect(c.reportUserActivity(5)).toBe(false);
    ws.handshake();
    expect(c.reportUserActivity(10)).toBe(true);
    ws.serverClose();
    await tick(30);
    const ws2 = FakeSocket.last();
    expect(ws2).not.toBe(ws);
    ws2.handshake();
    expect(c.reportUserActivity(20)).toBe(true);
    expect(ws.sent.filter((m) => m.type === "user_activity")).toHaveLength(1);
    expect(ws2.sent.filter((m) => m.type === "user_activity")).toHaveLength(1);
  });
});
