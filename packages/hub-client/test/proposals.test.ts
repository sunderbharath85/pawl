import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import type { ChatProposal } from "@krypto/protocol";
import {
  applyWelcome,
  HubClient,
  HubRequestError,
  initialHubState,
  PROPOSAL_TTL_MS,
  ProposalLedger,
  proposalConfirmText,
  proposalDoneText,
  proposalExpired,
  proposalProblem,
  proposalRetryable,
  repoIndexHas,
} from "../src/index";
import { FakeSocket, machine, machine2, MID, MID2, session, tick, welcome } from "./fixtures";

const macRepos = [
  { name: "ai-pet", path: "/Users/me/ai-pet", lastUsedAt: 9 },
  { name: "web", path: "/Users/me/web", lastUsedAt: 3 },
];

/** Answer the list_repos request executeProposal makes before a start. */
async function answerRepos(ws: FakeSocket, repos: unknown[] = macRepos) {
  await tick();
  const req = ws.lastSent();
  expect(req).toEqual({ type: "list_repos", id: req.id, machineId: MID2 });
  ws.receive({ type: "ack", id: req.id, ok: true, result: { repos } });
  await tick();
}

const startP: ChatProposal = {
  id: "p-1",
  kind: "start",
  machineId: MID2,
  agentId: "claude-code",
  cwd: "/Users/me/ai-pet",
  repoName: "ai-pet",
  text: "run the tests",
  summary: "Start Claude Code on Mac mini in ai-pet: run the tests",
};
const promptP: ChatProposal = {
  id: "p-2",
  kind: "prompt",
  machineId: MID2,
  agentId: "claude-code",
  sessionId: `${MID2}:s7`,
  text: "run the tests",
  summary: "Send to Claude Code on Mac mini (ai-pet): run the tests",
};
const paired = { ...welcome, machines: [machine, { ...machine2, online: true }] };

const clients: HubClient[] = [];
function connected() {
  const c = new HubClient({ token: "t", client: "desktop", clientVersion: "1", WebSocketImpl: FakeSocket, requestTimeoutMs: 200 });
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

describe("listRepos", () => {
  test("this machine by default; another machine by id", async () => {
    const { c, ws } = connected();
    const mine = c.listRepos();
    expect(ws.lastSent()).toEqual({ type: "list_repos", id: ws.lastSent().id });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { repos: [{ name: "krypto", path: "C:/code/krypto" }] } });
    expect(await mine).toEqual([{ name: "krypto", path: "C:/code/krypto" }]);

    const own = c.listRepos({ machineId: MID });
    expect(ws.lastSent()).toEqual({ type: "list_repos", id: ws.lastSent().id });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { repos: [] } });
    await own;

    const theirs = c.listRepos({ machineId: MID2 });
    expect(ws.lastSent()).toEqual({ type: "list_repos", id: ws.lastSent().id, machineId: MID2 });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { repos: [{ name: "ai-pet", path: "/Users/me/ai-pet", lastUsedAt: 5 }] } });
    expect((await theirs)[0]?.name).toBe("ai-pet");
  });
});

describe("executeProposal", () => {
  test("refuses without confirmation, for both kinds, and sends nothing", async () => {
    const { c, ws } = connected();
    const before = ws.sent.length;
    await expect(c.executeProposal(startP)).rejects.toMatchObject({ code: "unconfirmed", requestType: "start_session" });
    await expect(c.executeProposal(startP, { confirmed: false })).rejects.toMatchObject({ code: "unconfirmed" });
    await expect(c.executeProposal(promptP)).rejects.toMatchObject({ code: "unconfirmed", requestType: "prompt" });
    expect(ws.sent.length).toBe(before);
  });

  test("refuses malformed proposals locally (invalid) and never echoes the task text", async () => {
    const { c, ws } = connected();
    const before = ws.sent.length;
    const bad: ChatProposal[] = [
      { ...startP, cwd: "" },
      { ...startP, machineId: "nope" },
      { ...promptP, sessionId: undefined },
      { ...promptP, sessionId: `${MID}:s1` }, // a session of a different machine than the proposal names
      { ...promptP, text: "  " },
      { ...startP, agentId: "" },
    ];
    for (const p of bad) {
      const err = (await c.executeProposal(p, { confirmed: true }).catch((e: unknown) => e)) as HubRequestError;
      expect(err).toMatchObject({ code: "invalid" });
      expect(err.message).not.toContain("run the tests");
    }
    expect(ws.sent.length).toBe(before);
  });

  test("start -> checks that machine's repo index, then start_session there with confirmed:true", async () => {
    const { c, ws } = connected();
    const p = c.executeProposal(startP, { confirmed: true });
    await answerRepos(ws);
    expect(ws.lastSent()).toEqual({
      type: "start_session",
      id: ws.lastSent().id,
      agentId: "claude-code",
      cwd: "/Users/me/ai-pet",
      prompt: "run the tests",
      machineId: MID2,
      confirmed: true,
    });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { session: session(`${MID2}:new`, { machineId: MID2 }) } });
    expect(await p).toMatchObject({ kind: "started", session: { id: `${MID2}:new` } });
  });

  test("start with an empty task starts without a prompt", async () => {
    const { c, ws } = connected();
    const p = c.executeProposal({ ...startP, text: "" }, { confirmed: true });
    await answerRepos(ws);
    expect(ws.lastSent().type).toBe("start_session");
    expect(ws.lastSent().prompt).toBeUndefined();
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { session: session(`${MID2}:x`, { machineId: MID2 }) } });
    await p;
  });

  test("prompt -> prompt to that session; hub errors keep their code", async () => {
    const { c, ws } = connected();
    const p = c.executeProposal(promptP, { confirmed: true });
    expect(ws.lastSent()).toEqual({ type: "prompt", id: ws.lastSent().id, sessionId: `${MID2}:s7`, text: "run the tests" });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: {} });
    expect(await p).toEqual({ kind: "sent", sessionId: `${MID2}:s7` });

    const q = c.executeProposal(startP, { confirmed: true });
    await answerRepos(ws);
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: false, error: "forbidden" });
    await expect(q).rejects.toMatchObject({ code: "forbidden" });
  });

  test("a start proposal for this machine is invalid; nothing is sent (not even list_repos)", async () => {
    const { c, ws } = connected();
    const before = ws.sent.length;
    const err = (await c.executeProposal({ ...startP, machineId: MID }, { confirmed: true }).catch((e: unknown) => e)) as HubRequestError;
    expect(err).toMatchObject({ code: "invalid", requestType: "start_session" });
    expect(err.message).toContain("this machine");
    expect(ws.sent.length).toBe(before);
    expect(proposalProblem({ ...startP, machineId: MID }, MID)).toContain("local starts are not proposals");
    expect(proposalProblem(startP, MID)).toBeNull();
  });

  test("a start whose folder is not in that machine's repo index is invalid; nothing is started", async () => {
    const { c, ws } = connected();
    const p = c.executeProposal({ ...startP, cwd: "/etc" }, { confirmed: true }).catch((e: unknown) => e);
    await answerRepos(ws);
    const err = (await p) as HubRequestError;
    expect(err).toMatchObject({ code: "invalid", requestType: "start_session" });
    expect(err.message).toContain("not one of that machine's repos");
    expect(ws.sent.filter((m) => m.type === "start_session")).toHaveLength(0);
  });

  test("the repo index is cached; a folder missing from the cache is fetched again", async () => {
    const { c, ws } = connected();
    // listRepos fills the cache: the next start in a known folder goes straight to start_session.
    const l = c.listRepos({ machineId: MID2 });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { repos: macRepos } });
    await l;
    const p = c.executeProposal(startP, { confirmed: true });
    await tick();
    expect(ws.lastSent().type).toBe("start_session");
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { session: session(`${MID2}:a`, { machineId: MID2 }) } });
    await p;
    // A folder the cache does not know (the index grew since) is checked against a fresh list_repos.
    const q = c.executeProposal({ ...startP, cwd: "/Users/me/new" }, { confirmed: true });
    await answerRepos(ws, [...macRepos, { name: "new", path: "/Users/me/new" }]);
    expect(ws.lastSent()).toMatchObject({ type: "start_session", cwd: "/Users/me/new" });
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { session: session(`${MID2}:b`, { machineId: MID2 }) } });
    await q;
  });

  test("a failing list_repos (owner offline) starts nothing and keeps its code", async () => {
    const { c, ws } = connected();
    const p = c.executeProposal(startP, { confirmed: true });
    await tick();
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: false, error: "offline" });
    await expect(p).rejects.toMatchObject({ code: "offline" });
    expect(ws.sent.filter((m) => m.type === "start_session")).toHaveLength(0);
  });
});

describe("proposal helpers", () => {
  const state = applyWelcome(initialHubState(), paired);

  test("expiry after 10 minutes", () => {
    expect(proposalExpired({ ts: 0 }, PROPOSAL_TTL_MS)).toBe(false);
    expect(proposalExpired({ ts: 0 }, PROPOSAL_TTL_MS + 1)).toBe(true);
  });

  test("confirm and result wording names the machine", () => {
    expect(proposalConfirmText(state, startP)).toEqual({ title: "Run on Mac mini?", detail: startP.summary, button: "Run on Mac mini" });
    expect(proposalConfirmText({ machine: undefined, machines: {} }, startP).button).toBe("Run on that machine");
    expect(proposalDoneText(state, startP)).toBe("Started on Mac mini.");
    expect(proposalDoneText(state, promptP)).toBe("Sent to Mac mini.");
    expect(proposalProblem(startP)).toBeNull();
    expect(proposalProblem(promptP)).toBeNull();
  });

  test("repoIndexHas: exact folder, trailing separators ignored, case-insensitive on Windows and macOS only", () => {
    expect(repoIndexHas(macRepos, "/Users/me/ai-pet")).toBe(true);
    expect(repoIndexHas(macRepos, "/Users/me/ai-pet/")).toBe(true);
    expect(repoIndexHas(macRepos, "/users/me/AI-PET", "macos")).toBe(true);
    expect(repoIndexHas(macRepos, "/users/me/AI-PET", "linux")).toBe(false);
    expect(repoIndexHas([{ name: "k", path: "C:\\code\\krypto" }], "c:\\Code\\Krypto\\", "windows")).toBe(true);
    expect(repoIndexHas(macRepos, "/Users/me")).toBe(false);
    expect(repoIndexHas(macRepos, "/Users/me/ai-pet/sub")).toBe(false);
    expect(repoIndexHas(macRepos, "  ")).toBe(false);
  });

  test("only failures that ran nothing are retryable", () => {
    for (const code of ["offline", "rate_limited", "forbidden", "invalid", "not_found", "unconfirmed"]) {
      expect(proposalRetryable(new HubRequestError(code, "start_session"))).toBe(true);
    }
    for (const code of ["timeout", "disconnected", "already_decided", "weird"]) {
      expect(proposalRetryable(new HubRequestError(code, "start_session"))).toBe(false);
    }
  });
});

describe("ProposalLedger", () => {
  test("one proposal per message runs; the others are spent too", () => {
    const l = new ProposalLedger();
    let changes = 0;
    const off = l.subscribe(() => changes++);
    expect(l.available("m1")).toBe(true);
    expect(l.begin("m1", "a")).toBe(true);
    expect(l.begin("m1", "b")).toBe(false);
    l.done("m1", "a", "Started on Mac mini.", "s1");
    expect(l.get("m1")).toEqual({ status: "done", proposalId: "a", text: "Started on Mac mini.", sessionId: "s1" });
    expect(l.available("m1")).toBe(false);
    expect(l.available("m2")).toBe(true);
    expect(changes).toBe(2);
    off();
    l.begin("m2", "x");
    expect(changes).toBe(2);
    expect(l.snapshot()).toBe(3);
  });

  test("a failure that ran nothing frees the message; an unknown outcome does not", () => {
    const l = new ProposalLedger();
    l.begin("m1", "a");
    l.failed("m1", "a", "Mac mini is offline.", true);
    expect(l.available("m1")).toBe(true);
    expect(l.begin("m1", "b")).toBe(true);
    l.failed("m1", "b", "No answer from Mac mini yet — check that machine.", false);
    expect(l.available("m1")).toBe(false);
  });
});
