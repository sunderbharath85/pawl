import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { HubClient, HubRequestError, pickPermissionOption, type HubClientOptions } from "../src/index";
import { event, FakeSocket, permission, session, tick, welcome } from "./fixtures";

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
    expect(ws.lastSent()).toMatchObject({ type: "get_events", sessionId: "s1", sinceId: 2 });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { events: [event(3), event(4)] } });
    expect((await p).map((e) => e.id)).toEqual([3, 4]);
    expect(c.getState().events.s1!.map((e) => e.id)).toEqual([3, 4]);
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
