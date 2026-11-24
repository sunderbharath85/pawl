import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import type { PermissionRequest } from "@pawl/protocol";
import {
  applyAck,
  applyWelcome,
  approveOptionId,
  HubClient,
  HubRequestError,
  INTENT_ERROR_CODES,
  initialHubState,
  intentErrorText,
  isAllowOption,
  isAlreadyDecided,
  needsConfirmation,
  offlineReason,
  ownerName,
  recentCwds,
  REMOTE_REQUEST_TIMEOUT_MS,
  remoteOptionBlocked,
  startNeedsConfirmation,
  type HubClientOptions,
} from "../src/index";
import { agent, FakeSocket, machine, machine2, MID, MID2, permission, session, tick, welcome } from "./fixtures";

const remoteReq: PermissionRequest = permission(`${MID2}:r9`, { machineId: MID2, sessionId: `${MID2}:s9` });
const localReq: PermissionRequest = permission(`${MID}:r1`);
const remoteSession = session(`${MID2}:s9`, { machineId: MID2, status: "working" });
const paired = { ...welcome, machines: [machine, { ...machine2, online: true }], pendingPermissions: [localReq, remoteReq], sessions: [session(`${MID}:s1`), remoteSession] };

const clients: HubClient[] = [];
function connected(over: Partial<HubClientOptions> = {}) {
  const c = new HubClient({
    token: "t",
    client: "desktop",
    clientVersion: "1",
    WebSocketImpl: FakeSocket,
    minReconnectDelayMs: 5,
    maxReconnectDelayMs: 20,
    requestTimeoutMs: 200,
    ...over,
  });
  clients.push(c);
  c.connect();
  const ws = FakeSocket.last();
  ws.handshake(paired);
  return { c, ws };
}
beforeEach(() => FakeSocket.reset());
afterEach(() => {
  for (const c of clients.splice(0)) c.close();
});

describe("confirmation rules", () => {
  const state = applyWelcome(initialHubState(), paired);

  test("only another machine's allow_* answer needs a confirmation", () => {
    expect(needsConfirmation(state, remoteReq, "o-allow")).toBe(true);
    expect(needsConfirmation(state, remoteReq, "o-always")).toBe(true);
    expect(needsConfirmation(state, remoteReq, "o-reject")).toBe(false);
    expect(needsConfirmation(state, remoteReq, null)).toBe(false);
    expect(needsConfirmation(state, remoteReq, "not-an-option")).toBe(false);
    expect(needsConfirmation(state, localReq, "o-allow")).toBe(false);
    expect(isAllowOption(remoteReq, "o-allow")).toBe(true);
    expect(isAllowOption(remoteReq, null)).toBe(false);
  });

  test("remotely only allow_once and reject_* can be chosen; always-allow says where to choose it", () => {
    expect(remoteOptionBlocked(state, remoteReq, "o-always")).toBe("Choose always-allow on Mac mini");
    expect(remoteOptionBlocked(state, remoteReq, "o-allow")).toBeNull();
    expect(remoteOptionBlocked(state, remoteReq, "o-reject")).toBeNull();
    expect(remoteOptionBlocked(state, remoteReq, null)).toBeNull();
    expect(remoteOptionBlocked(state, localReq, "o-always")).toBeNull();
  });

  test("approveOptionId: allow_once; always-allow only for this machine's own request", () => {
    const always = [{ optionId: "o-always", name: "Always", kind: "allow_always" as const }];
    expect(approveOptionId(state, remoteReq)).toBe("o-allow");
    expect(approveOptionId(state, { ...remoteReq, options: always })).toBeNull();
    expect(approveOptionId(state, { ...localReq, options: always })).toBe("o-always");
  });

  test("starting on another machine needs a confirmation; this machine (or default) does not", () => {
    expect(startNeedsConfirmation(state, MID2)).toBe(true);
    expect(startNeedsConfirmation(state, MID)).toBe(false);
    expect(startNeedsConfirmation(state, undefined)).toBe(false);
  });

  test("offlineReason names an offline owner", () => {
    expect(offlineReason(state, MID)).toBeNull();
    expect(offlineReason(state, MID2)).toBeNull();
    const off = applyWelcome(initialHubState(), { ...paired, machines: [machine, machine2] });
    expect(offlineReason(off, MID2)).toBe("Mac mini is offline");
    expect(offlineReason(off, "unknown00000")).toBe("That machine is offline");
    expect(ownerName(off, MID2)).toBe("Mac mini");
    expect(ownerName(off, "unknown00000")).toBe("that machine");
  });
});

describe("intentErrorText", () => {
  const err = (code: string) => new HubRequestError(code, "resolve_permission");

  test("the contract's wording for the headline codes", () => {
    expect(intentErrorText(err("already_decided"), "Mac mini", "approve")).toBe("Already answered.");
    expect(intentErrorText(err("offline"), "Mac mini", "approve")).toBe("Mac mini is offline.");
    expect(intentErrorText(err("timeout"), "Mac mini", "approve")).toBe("No answer from Mac mini yet — check that machine.");
  });

  test("every code has clear text naming the machine (no raw codes)", () => {
    for (const code of INTENT_ERROR_CODES) {
      for (const action of ["approve", "deny", "prompt", "start", "cancel"] as const) {
        const text = intentErrorText(err(code), "Mac mini", action);
        expect(text.length).toBeGreaterThan(10);
        if (code.includes("_")) expect(text).not.toContain(code);
        if (code !== "already_decided") expect(text).toContain("Mac mini");
      }
    }
  });

  test("forbidden explains the owner's policy per action", () => {
    expect(intentErrorText(err("forbidden"), "Mac mini", "start")).toContain("only lets other machines start agents in folders it already uses");
    expect(intentErrorText(err("forbidden"), "Mac mini", "approve")).toContain("Always-allow must be chosen on Mac mini itself");
    expect(intentErrorText(err("forbidden"), "Mac mini", "resolve")).toContain("Always-allow must be chosen on Mac mini itself");
    for (const action of ["start", "approve", "deny", "prompt", "cancel"] as const) {
      expect(intentErrorText(err("forbidden"), "Mac mini", action)).toContain("accept_intents is off");
    }
  });

  test("invalid and not_found depend on the action", () => {
    expect(intentErrorText(err("invalid"), "Mac mini", "start")).toContain("folder must be one of its repos");
    expect(intentErrorText(err("invalid"), "Mac mini", "approve")).toContain("option");
    expect(intentErrorText(err("not_found"), "Mac mini", "prompt")).toContain("session");
    expect(intentErrorText(err("not_found"), "Mac mini", "approve")).toContain("request");
  });

  test("unknown codes and plain errors", () => {
    expect(intentErrorText(err("brand_new"), "Mac mini", "cancel")).toBe("Mac mini couldn't cancel that (brand_new).");
    expect(intentErrorText(new Error("boom"), "Mac mini", "cancel")).toBe("boom");
    expect(isAlreadyDecided(err("already_decided"))).toBe(true);
    expect(isAlreadyDecided(err("timeout"))).toBe(false);
  });
});

test("recentCwds: one machine's distinct folders, newest first", () => {
  const sessions = {
    a: session("a", { machineId: MID2, cwd: "/repo/a", lastEventAt: 3 }),
    b: session("b", { machineId: MID2, cwd: "/repo/b/", lastEventAt: 5 }),
    c: session("c", { machineId: MID2, cwd: "/repo/a", lastEventAt: 9 }),
    d: session("d", { machineId: MID, cwd: "/local", lastEventAt: 10 }),
    e: session("e", { machineId: MID2, cwd: "/repo/b", lastEventAt: 1 }),
  };
  expect(recentCwds({ sessions }, MID2)).toEqual(["/repo/a", "/repo/b/"]);
  expect(recentCwds({ sessions }, MID2, 1)).toEqual(["/repo/a"]);
  expect(recentCwds({ sessions }, MID)).toEqual(["/local"]);
});

describe("HubClient remote actions", () => {
  test("a remote approve without confirmation is refused locally; nothing is sent", async () => {
    const { c, ws } = connected();
    const before = ws.sent.length;
    await expect(c.approve(remoteReq.requestId)).rejects.toMatchObject({ code: "unconfirmed", requestType: "resolve_permission" });
    await expect(c.resolvePermission(remoteReq.requestId, "o-allow", { confirmed: false })).rejects.toMatchObject({ code: "unconfirmed" });
    expect(ws.sent.length).toBe(before);
  });

  test("always-allow on another machine's request is refused locally (forbidden), even confirmed", async () => {
    const { c, ws } = connected();
    const before = ws.sent.length;
    await expect(c.resolvePermission(remoteReq.requestId, "o-always")).rejects.toMatchObject({ code: "forbidden" });
    await expect(c.resolvePermission(remoteReq.requestId, "o-always", { confirmed: true })).rejects.toMatchObject({ code: "forbidden" });
    expect(ws.sent.length).toBe(before);
  });

  test("a remote approve uses allow_once only; a request offering only always-allow is forbidden here", async () => {
    const onlyAlways = permission(`${MID2}:r10`, {
      machineId: MID2,
      options: [
        { optionId: "o-always", name: "Always", kind: "allow_always" },
        { optionId: "o-reject", name: "Reject", kind: "reject_once" },
      ],
    });
    const { c, ws } = connected();
    ws.receive({ type: "permission_request", request: onlyAlways });
    const before = ws.sent.length;
    await expect(c.approve(onlyAlways.requestId, { confirmed: true })).rejects.toMatchObject({ code: "forbidden" });
    expect(ws.sent.length).toBe(before);
    // This machine's own request still falls back to always-allow.
    const localAlways = permission(`${MID}:r11`, { options: onlyAlways.options });
    ws.receive({ type: "permission_request", request: localAlways });
    const p = c.approve(localAlways.requestId);
    expect(ws.lastSent()).toMatchObject({ type: "resolve_permission", requestId: localAlways.requestId, optionId: "o-always" });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: {} });
    await p;
  });

  test("a confirmed remote approve sends confirmed:true and waits past the hub's intent timeout", async () => {
    const { c, ws } = connected({ requestTimeoutMs: 20 });
    let settled = false;
    const p = c.approve(remoteReq.requestId, { confirmed: true }).finally(() => (settled = true));
    const sent = ws.lastSent();
    expect(sent).toEqual({ type: "resolve_permission", id: sent.id, requestId: remoteReq.requestId, optionId: "o-allow", confirmed: true });
    await tick(60);
    expect(settled).toBe(false);
    expect(REMOTE_REQUEST_TIMEOUT_MS).toBeGreaterThan(15_000);
    ws.receive({ type: "ack", id: sent.id, ok: false, error: "already_decided" });
    await expect(p).rejects.toMatchObject({ code: "already_decided" });
  });

  test("a remote deny needs no confirmation; local answers send no confirmed field", async () => {
    const { c, ws } = connected();
    const d = c.deny(remoteReq.requestId);
    expect(ws.lastSent()).toEqual({ type: "resolve_permission", id: ws.lastSent().id, requestId: remoteReq.requestId, optionId: "o-reject" });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: {} });
    await d;
    const a = c.approve(localReq.requestId);
    expect(ws.lastSent()).toEqual({ type: "resolve_permission", id: ws.lastSent().id, requestId: localReq.requestId, optionId: "o-allow" });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: {} });
    await a;
  });

  test("hub errors for forwarded requests keep their code", async () => {
    const { c, ws } = connected();
    for (const code of ["offline", "timeout", "forbidden", "rate_limited", "invalid", "not_found"]) {
      const p = c.resolvePermission(remoteReq.requestId, null);
      ws.receive({ type: "ack", id: ws.lastSent().id, ok: false, error: code });
      await expect(p).rejects.toMatchObject({ code });
    }
  });

  test("the mirror's permission_resolved (with byMachineId) removes the request", () => {
    const { c, ws } = connected();
    ws.receive({
      type: "permission_resolved",
      requestId: remoteReq.requestId,
      sessionId: remoteReq.sessionId,
      outcome: { kind: "selected", optionId: "o-allow" },
      by: "user",
      byMachineId: MID,
    });
    expect(c.getState().pending[remoteReq.requestId]).toBeUndefined();
  });

  test("remote prompt and cancel need no confirmation but wait for the owner", async () => {
    const { c, ws } = connected({ requestTimeoutMs: 20 });
    let settled = false;
    const p = c.prompt(remoteSession.id, "run the tests").finally(() => (settled = true));
    expect(ws.lastSent()).toEqual({ type: "prompt", id: ws.lastSent().id, sessionId: remoteSession.id, text: "run the tests" });
    await tick(60);
    expect(settled).toBe(false);
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: {} });
    await p;
    const k = c.cancel(remoteSession.id);
    expect(ws.lastSent()).toMatchObject({ type: "cancel", sessionId: remoteSession.id });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: {} });
    await k;
  });

  test("startSession: this machine by default; another machine needs a confirmation", async () => {
    const { c, ws } = connected();
    const local = c.startSession({ agentId: "claude-code", cwd: "/repo", machineId: MID });
    expect(ws.lastSent()).toEqual({ type: "start_session", id: ws.lastSent().id, agentId: "claude-code", cwd: "/repo" });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { session: session(`${MID}:new`) } });
    expect((await local).id).toBe(`${MID}:new`);

    const before = ws.sent.length;
    await expect(c.startSession({ agentId: "claude-code", cwd: "/r", machineId: MID2 })).rejects.toMatchObject({ code: "unconfirmed" });
    expect(ws.sent.length).toBe(before);

    const remote = c.startSession({ agentId: "claude-code", cwd: "/r", prompt: "hi", machineId: MID2, confirmed: true });
    expect(ws.lastSent()).toEqual({
      type: "start_session",
      id: ws.lastSent().id,
      agentId: "claude-code",
      cwd: "/r",
      prompt: "hi",
      machineId: MID2,
      confirmed: true,
    });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { session: session(`${MID2}:new`, { machineId: MID2 }) } });
    expect((await remote).machineId).toBe(MID2);
  });

  test("startSession positional form still starts here", async () => {
    const { c, ws } = connected();
    const p = c.startSession("claude-code", "/repo", "go");
    expect(ws.lastSent()).toEqual({ type: "start_session", id: ws.lastSent().id, agentId: "claude-code", cwd: "/repo", prompt: "go" });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { session: session("x") } });
    await p;
  });

  test("listAgents({machineId}) returns that machine's agents without replacing this machine's", async () => {
    const { c, ws } = connected();
    const theirs = { ...agent, machineId: MID2, name: "Claude on the Mac" };
    const p = c.listAgents({ machineId: MID2 });
    expect(ws.lastSent()).toEqual({ type: "list_agents", id: ws.lastSent().id, machineId: MID2 });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { agents: [theirs] } });
    expect(await p).toEqual([theirs]);
    expect(c.getState().agents).toEqual([agent]);

    const own = c.listAgents({ machineId: MID, refresh: true });
    expect(ws.lastSent()).toEqual({ type: "list_agents", id: ws.lastSent().id, refresh: true });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { agents: [] } });
    await own;
    expect(c.getState().agents).toEqual([]);
  });

  test("applyAck: list_agents for another machine leaves state.agents alone", () => {
    const s = applyWelcome(initialHubState(), paired);
    expect(applyAck(s, { type: "list_agents", machineId: MID2 }, { agents: [] })).toBe(s);
    expect(applyAck(s, { type: "list_agents" }, { agents: [] }).agents).toEqual([]);
  });
});
