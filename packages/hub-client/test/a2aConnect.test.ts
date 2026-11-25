import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import type { A2aConnectStatus } from "@pawl/protocol";
import {
  A2A_CONNECT_STATE_LABEL,
  A2A_CONNECT_STATE_TONE,
  A2A_CONNECT_TARGETS,
  A2A_SERVER_STARTS,
  a2aConnectAction,
  a2aConnectCallerMarked,
  a2aConnectConfirm,
  a2aConnectDoneText,
  a2aConnectName,
  a2aConnectPreviewable,
  a2aConnectRemovable,
  a2aConnectRemoveConfirm,
  a2aConnectServerProblem,
  callerConnectTarget,
  connectCallerHint,
  connectFacts,
  HubClient,
  HubRequestError,
  hubLacksMessage,
  type ConnectFacts,
} from "../src/index";
import { FakeSocket, tick, welcome } from "./fixtures";

const row = (over: Partial<A2aConnectStatus> = {}): A2aConnectStatus => ({
  target: "claude",
  name: "Claude Code",
  agentInstalled: true,
  state: "missing",
  configPath: "C:\\Users\\me\\.claude.json",
  callerId: "claude-code",
  callerExists: false,
  ...over,
});

describe("connect coding agents: rows", () => {
  test("the four targets in the hub's order; labels and tones for every state", () => {
    expect(A2A_CONNECT_TARGETS).toEqual(["claude", "codex", "copilot-cli", "copilot-vscode"]);
    expect(A2A_CONNECT_STATE_LABEL).toEqual({ connected: "Connected", outdated: "Needs update", missing: "Not connected", unsupported: "Not supported" });
    expect(A2A_CONNECT_STATE_TONE.connected).toBe("done");
    expect(A2A_CONNECT_STATE_TONE.outdated).toBe("waiting");
    expect(A2A_CONNECT_STATE_TONE.missing).toBe("idle");
  });

  test("the hub's name, else the target's label", () => {
    expect(a2aConnectName(row())).toBe("Claude Code");
    expect(a2aConnectName({ target: "copilot-vscode", name: " " })).toBe("Copilot in VS Code");
    expect(a2aConnectName({ target: "codex" })).toBe("Codex");
  });

  test("what the buttons do per state", () => {
    expect(a2aConnectAction({ state: "missing" })).toBe("connect");
    expect(a2aConnectAction({ state: "outdated" })).toBe("update");
    expect(a2aConnectAction({ state: "connected" })).toBeNull();
    expect(a2aConnectAction({ state: "unsupported" })).toBeNull();
    expect(a2aConnectPreviewable({ state: "connected" })).toBe(true);
    expect(a2aConnectPreviewable({ state: "unsupported" })).toBe(false);
  });

  test("Remove is always possible when there may be something to take away", () => {
    expect(a2aConnectRemovable({ state: "connected" })).toBe(true);
    expect(a2aConnectRemovable({ state: "outdated" })).toBe(true);
    // A config Pawl cannot read: the hub still revokes the token (and warns it could not edit the file).
    expect(a2aConnectRemovable({ state: "unsupported" })).toBe(true);
    // No entry, but the caller Connect made is still there.
    expect(a2aConnectRemovable({ state: "missing", callerExists: true, callerMarked: true })).toBe(true);
    // A caller the user added by hand with that id is never offered for removal here.
    expect(a2aConnectRemovable({ state: "missing", callerExists: true, callerMarked: false })).toBe(false);
    // A hub without callerMarked: treated as not marked.
    expect(a2aConnectRemovable({ state: "missing", callerExists: true })).toBe(false);
    expect(a2aConnectRemovable({ state: "missing", callerExists: false })).toBe(false);
    expect(a2aConnectRemovable({ state: "missing" })).toBe(false);
    expect(a2aConnectCallerMarked({ callerExists: true, callerMarked: true })).toBe(true);
    expect(a2aConnectCallerMarked({ callerExists: false, callerMarked: true })).toBe(false);
  });

  test("a connected row whose A2A server failed to start shows the server's error", () => {
    expect(a2aConnectServerProblem(row({ state: "connected" }), { error: "port 47802 is in use" })).toBe(
      "Claude Code cannot reach Pawl: the A2A server is not running (port 47802 is in use).",
    );
    expect(a2aConnectServerProblem(row({ state: "connected" }), { error: null })).toBeNull();
    expect(a2aConnectServerProblem(row({ state: "missing" }), { error: "port 47802 is in use" })).toBeNull();
    expect(a2aConnectServerProblem(row({ state: "connected" }), null)).toBeNull();
  });

  test("a newer hub names the stopped server in the row's detail: that is shown, once", () => {
    const detail = "Pawl's A2A server is not running: port 47802 is in use";
    expect(a2aConnectServerProblem(row({ state: "connected", detail }), { error: "port 47802 is in use" })).toBe(detail);
    expect(a2aConnectServerProblem(row({ state: "connected", detail: "Pawl's A2A server is not running: it is not running" }), { error: null })).toBe(
      "Pawl's A2A server is not running: it is not running",
    );
    // Another state's detail is the row's ordinary hint, not a server problem.
    expect(a2aConnectServerProblem(row({ state: "outdated", detail: "Pawl's A2A server is off; Connect turns it on (127.0.0.1 only), no new token" }), { error: null })).toBeNull();
  });
});

/** The facts of a fresh preview (`newToken` from the ack; the callers in the settings now). */
const facts = (over: Partial<ConnectFacts> = {}): ConnectFacts => ({ startsServer: false, newToken: true, callerExists: false, takesOver: false, callerId: "claude-code", ...over });

describe("connect coding agents: confirms", () => {
  test("Connect names the file, the agent's own token in it, and that it can never answer approvals", () => {
    const c = a2aConnectConfirm(row(), facts());
    expect(c.title).toBe("Connect Claude Code to Pawl?");
    expect(c.confirm).toBe("Connect");
    expect(c.body).toContain('Pawl adds a "pawl" entry to C:\\Users\\me\\.claude.json.');
    expect(c.body).toContain("Claude Code gets its own token, stored in that file.");
    expect(c.body).toContain("can never answer approvals");
    // A first connect: there is no old token to stop.
    expect(c.body).not.toContain("old token");
    expect(c.body).not.toContain(A2A_SERVER_STARTS);
    expect(c.body).not.toContain("Settings Sync");
  });

  test("when the preview said startsServer, the confirm says the A2A server goes on (127.0.0.1 only)", () => {
    const c = a2aConnectConfirm(row({ target: "codex", name: "Codex", configPath: "~/.codex/config.toml" }), facts({ startsServer: true, callerId: "codex" }));
    expect(c.body.endsWith("This turns on Pawl's A2A server, on 127.0.0.1 only.")).toBe(true);
    expect(c.key).toBe("a2a.connect.codex");
  });

  test("Update with a new token for the existing caller: the old token stops working", () => {
    const c = a2aConnectConfirm(row({ state: "outdated", target: "copilot-cli", name: "Copilot CLI", configPath: "~/.copilot/mcp-config.json" }), facts({ callerExists: true, newToken: true }));
    expect(c.title).toBe("Update Copilot CLI's connection to Pawl?");
    expect(c.confirm).toBe("Update");
    expect(c.body).toContain('Pawl rewrites the "pawl" entry in ~/.copilot/mcp-config.json.');
    expect(c.body).toContain("Copilot CLI gets a new token, stored in that file; its old token stops working.");
  });

  test("Update that only turns the server back on (newToken false): the entry and the token stay", () => {
    const c = a2aConnectConfirm(row({ state: "outdated" }), facts({ callerExists: true, newToken: false, startsServer: true }));
    expect(c.body).toContain('The "pawl" entry in C:\\Users\\me\\.claude.json stays as it is.');
    expect(c.body).toContain("Claude Code keeps its token, stored in that file.");
    expect(c.body).not.toContain("stops working");
    expect(c.body).not.toContain("rewrites");
    expect(c.body).not.toContain("backup");
    expect(c.body.endsWith(A2A_SERVER_STARTS)).toBe(true);
  });

  test("a hub without newToken: a token for an existing caller is assumed new", () => {
    const c = a2aConnectConfirm(row({ state: "outdated" }), facts({ callerExists: true, newToken: undefined }));
    expect(c.body).toContain("its old token stops working");
  });

  test("taking over a caller the user added: said so, by its id", () => {
    const preview = { startsServer: false, newToken: true, status: row({ callerExists: true }) };
    const handAdded = [{ id: "claude-code", name: "My Claude", tokenSet: true, allowApprovals: true }];
    const f = connectFacts(preview, handAdded);
    expect(f).toEqual({ startsServer: false, newToken: true, callerExists: true, takesOver: true, callerId: "claude-code" });
    const c = a2aConnectConfirm(row(), f);
    expect(c.body).toContain('It takes over the caller "claude-code" you added: Claude Code uses it from now on, with a new token stored in that file; its old token stops working.');
    // The same caller already marked by Connect is not a takeover; no caller at all is not either.
    expect(connectFacts(preview, [{ ...handAdded[0]!, connectTarget: "claude" }]).takesOver).toBe(false);
    expect(connectFacts({ ...preview, status: row() }, []).takesOver).toBe(false);
  });

  test("the hub's callerMarked decides a takeover when it is there", () => {
    const marked = { startsServer: false, newToken: true, status: { ...row({ callerExists: true }), callerMarked: true } };
    const unmarked = { startsServer: false, newToken: true, status: { ...row({ callerExists: true }), callerMarked: false } };
    // Even if the settings have not caught up yet (no connectTarget), the hub's word counts.
    expect(connectFacts(marked, [{ id: "claude-code", name: "Claude Code", tokenSet: true, allowApprovals: false }]).takesOver).toBe(false);
    expect(connectFacts(unmarked, [{ id: "claude-code", name: "Claude Code", tokenSet: true, allowApprovals: false, connectTarget: "claude" }]).takesOver).toBe(true);
  });

  test("Copilot in VS Code: Settings Sync can copy mcp.json to other computers", () => {
    const c = a2aConnectConfirm(row({ target: "copilot-vscode", name: "Copilot in VS Code", configPath: "C:\\Users\\me\\AppData\\Roaming\\Code\\User\\mcp.json" }), facts({ callerId: "copilot-vscode" }));
    expect(c.body).toContain("If VS Code's Settings Sync is on, it can copy mcp.json, token included, to your other computers.");
  });

  test("Remove: the entry goes and, when Connect made the caller, its token stops working at once; the server stays", () => {
    const c = a2aConnectRemoveConfirm({ ...row({ state: "connected", callerExists: true }), callerMarked: true });
    expect(c.title).toBe("Disconnect Claude Code from Pawl?");
    expect(c.confirm).toBe("Remove");
    expect(c.body).toContain('takes its "pawl" entry out of C:\\Users\\me\\.claude.json');
    expect(c.body).toContain("its token stops working at once");
    expect(c.body).toContain("A2A server stays as it is");
  });

  test("Remove without a caller, or with one the user added (never removed): nothing about a token", () => {
    const plain = 'Pawl takes its "pawl" entry out of C:\\Users\\me\\.claude.json (a backup is made first). Pawl\'s A2A server stays as it is.';
    expect(a2aConnectRemoveConfirm(row({ state: "outdated", callerExists: false })).body).toBe(plain);
    expect(a2aConnectRemoveConfirm({ ...row({ state: "connected", callerExists: true }), callerMarked: false }).body).toBe(plain);
    // A hub without callerMarked: not marked.
    expect(a2aConnectRemoveConfirm(row({ state: "connected", callerExists: true })).body).toBe(plain);
  });

  test("Remove on an unsupported row: Pawl tries to take the entry out (it may not manage); the token goes either way", () => {
    const c = a2aConnectRemoveConfirm({ ...row({ state: "unsupported", callerExists: true }), callerMarked: true });
    expect(c.body).toBe(
      'Pawl tries to take its "pawl" entry out of C:\\Users\\me\\.claude.json (a backup is made first), but it may not be able to: it cannot read that file. ' +
        "Either way it removes Claude Code's caller: its token stops working at once and anything it has open with Pawl ends. Pawl's A2A server stays as it is.",
    );
    const noCaller = a2aConnectRemoveConfirm(row({ state: "unsupported", callerExists: false }));
    expect(noCaller.body).toContain("Pawl tries to take its");
    expect(noCaller.body).not.toContain("token");
  });

  test("the toast after apply / remove; a Remove with a warning says the token was revoked", () => {
    expect(a2aConnectDoneText(row(), "apply", true)).toBe("Connected Claude Code to Pawl");
    expect(a2aConnectDoneText(row(), "apply", false)).toBe("Claude Code was already connected");
    expect(a2aConnectDoneText(row(), "remove", true)).toBe("Disconnected Claude Code");
    expect(a2aConnectDoneText(row(), "remove", false)).toBe("Claude Code was not connected");
    expect(a2aConnectDoneText(row(), "remove", true, "could not edit C:\\Users\\me\\.claude.json; take the pawl entry out by hand")).toBe("Token revoked");
  });
});

describe("connect coding agents: their callers", () => {
  test("a caller Connect made carries its target; a caller added by hand has none", () => {
    expect(callerConnectTarget({ id: "claude-code", connectTarget: "claude" })).toBe("claude");
    expect(callerConnectTarget({ id: "copilot-vscode", connectTarget: "copilot-vscode" })).toBe("copilot-vscode");
    expect(callerConnectTarget({ id: "claude-desktop" })).toBeNull();
    expect(callerConnectTarget({ id: "x", connectTarget: null })).toBeNull();
    // A target from a newer hub this client does not know is treated as an ordinary caller.
    expect(callerConnectTarget({ id: "x", connectTarget: "cursor" })).toBeNull();
  });

  test("its hint in Callers, in place of Can answer approvals", () => {
    expect(connectCallerHint("claude")).toBe("Connected by Pawl for Claude Code. It can never answer approvals.");
    expect(connectCallerHint("copilot-cli", "Copilot CLI")).toBe("Connected by Pawl for Copilot CLI. It can never answer approvals.");
  });

  test("Remove from Callers before the list loaded: the same confirm, naming the agent's config file", () => {
    // A Callers row is a caller Connect made (connectTarget), so it is marked and its token goes.
    const c = a2aConnectRemoveConfirm({ target: "codex", callerExists: true, callerMarked: true });
    expect(c.title).toBe("Disconnect Codex from Pawl?");
    expect(c.body).toContain('takes its "pawl" entry out of Codex\'s config file');
    expect(c.body).toContain("its token stops working at once");
    expect(c.confirm).toBe("Remove");
  });
});

describe("connect coding agents: an older hub", () => {
  // Exactly what a 0.1.1 hub answers (captured): serde's unknown variant, as the ack's error.
  const OLD =
    "invalid message: unknown variant `a2a_connect_status`, expected one of `hello`, `list_agents`, `list_sessions`, `get_events`, " +
    "`start_session`, `prompt`, `start_browser_task`, `cancel`, `resolve_permission`, `list_history`, `resume_session`, `chat`, " +
    "`update_pet`, `set_rules`, `update_machine`, `create_invite`, `join_peer`, `revoke_device`, `user_activity`, `list_repos`, " +
    "`get_settings`, `update_settings`, `hook_status`, `install_hooks`, `open_panel`, `restart_hub`, `a2a_rotate_token`, `list_tasks` at line 1 column 28";

  test("its unknown-variant error means the hub lacks the message", () => {
    expect(hubLacksMessage(new HubRequestError(OLD, "a2a_connect_status"), "a2a_connect_status")).toBe(true);
    expect(hubLacksMessage(new HubRequestError("unknown_type", "a2a_connect_status"), "a2a_connect_status")).toBe(true);
  });

  test("other errors, and another message's unknown variant, are not that", () => {
    expect(hubLacksMessage(new HubRequestError(OLD, "a2a_connect_status"), "a2a_connect")).toBe(false);
    expect(hubLacksMessage(new HubRequestError("invalid message: missing field `target` at line 1 column 40", "a2a_connect"), "a2a_connect")).toBe(false);
    expect(hubLacksMessage(new HubRequestError("forbidden", "a2a_connect_status"), "a2a_connect_status")).toBe(false);
    expect(hubLacksMessage(new Error("boom"), "a2a_connect_status")).toBe(false);
    expect(hubLacksMessage(null, "a2a_connect_status")).toBe(false);
  });
});

describe("connect coding agents: wire format", () => {
  const clients: HubClient[] = [];
  function connected() {
    const c = new HubClient({ token: "t", client: "desktop", clientVersion: "1", WebSocketImpl: FakeSocket, requestTimeoutMs: 200 });
    clients.push(c);
    c.connect();
    const ws = FakeSocket.last();
    ws.handshake(welcome);
    return { c, ws };
  }
  beforeEach(() => FakeSocket.reset());
  afterEach(() => {
    for (const c of clients.splice(0)) c.close();
  });

  test("a2aConnectStatus / a2aConnect", async () => {
    const { c, ws } = connected();
    const st = c.a2aConnectStatus();
    await tick();
    const r1 = ws.lastSent();
    expect(r1).toEqual({ type: "a2a_connect_status", id: r1.id });
    ws.receive({ type: "ack", id: r1.id, ok: true, result: { targets: [row()] } });
    expect(await st).toEqual([row()]);

    const pv = c.a2aConnect("codex", "preview");
    await tick();
    const r2 = ws.lastSent();
    expect(r2).toEqual({ type: "a2a_connect", id: r2.id, target: "codex", action: "preview" });
    ws.receive({ type: "ack", id: r2.id, ok: true, result: { plan: "+ [mcp_servers.pawl]", applied: false, status: row({ target: "codex" }), startsServer: true, newToken: true } });
    expect(await pv).toMatchObject({ plan: "+ [mcp_servers.pawl]", applied: false, startsServer: true, newToken: true });

    // Remove that revoked the token but could not edit the file: the warning comes through.
    const rw = c.a2aConnect("copilot-vscode", "remove");
    await tick();
    const warning = "Revoked the token, but could not edit mcp.json: take the pawl entry out by hand.";
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: true, result: { plan: "", applied: true, status: row({ target: "copilot-vscode", state: "unsupported" }), startsServer: false, warning } });
    expect((await rw).warning).toBe(warning);

    const rm = c.a2aConnect("claude", "remove");
    await tick();
    const r3 = ws.lastSent();
    expect(r3).toEqual({ type: "a2a_connect", id: r3.id, target: "claude", action: "remove" });
    ws.receive({ type: "ack", id: r3.id, ok: false, error: "~/.claude.json is not valid JSON; fix it or add the entry by hand" });
    await expect(rm).rejects.toMatchObject({ code: "~/.claude.json is not valid JSON; fix it or add the entry by hand" });
  });

  test("an older hub's refusal comes back as a HubRequestError hubLacksMessage recognises", async () => {
    const { c, ws } = connected();
    const st = c.a2aConnectStatus();
    await tick();
    ws.receive({ type: "ack", id: ws.lastSent().id, ok: false, error: "invalid message: unknown variant `a2a_connect_status`, expected one of `hello`" });
    const err = await st.catch((e: unknown) => e);
    expect(err).toBeInstanceOf(HubRequestError);
    expect(hubLacksMessage(err, "a2a_connect_status")).toBe(true);
  });
});
