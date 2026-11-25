import { describe, expect, test } from "bun:test";
import type { A2aSettings, HubSettings } from "@pawl/protocol";
import {
  a2aCallerLabel,
  a2aCardUrl,
  a2aServerPatch,
  a2aServerStatus,
  addAgentPatch,
  addCallerPatch,
  agentTokenPatch,
  callerApprovalsPatch,
  checkA2aAgentUrl,
  checkA2aName,
  checkA2aToken,
  folderName,
  removeAgentPatch,
  removeCallerPatch,
  riskyChanges,
  settingsFieldLabel,
  suggestA2aId,
} from "../src/index";

const a2a: A2aSettings = {
  server: false,
  port: 47802,
  running: false,
  url: null,
  error: null,
  callers: [{ id: "claude-desktop", name: "Claude Desktop", tokenSet: true, allowApprovals: false }],
  agents: [{ id: "gemini", name: "Gemini CLI", url: "http://127.0.0.1:41242", tokenSet: true }],
};
const hub = (over: Partial<A2aSettings> = {}) =>
  ({
    a2a: { ...a2a, ...over },
    bot: { provider: "auto", understand: false, anthropicApiKeySet: false, openaiApiKeySet: false, geminiApiKeySet: false },
    peer: { listen: null, acceptIntents: false, startRoots: [] },
    relay: { url: null, accessKeySet: false },
    customAgents: [],
  }) as unknown as HubSettings;

describe("Settings > A2A: patches", () => {
  test("server on/off and port: only what changed", () => {
    expect(a2aServerPatch(a2a, { server: true })).toEqual({ a2a: { server: true } });
    expect(a2aServerPatch(a2a, { server: false, port: 47802 })).toEqual({});
    expect(a2aServerPatch(a2a, { port: 48000 })).toEqual({ a2a: { port: 48000 } });
  });

  test("callers: the list replaces; a new caller has a free id and no approvals; tokens are never in it", () => {
    const { patch, id } = addCallerPatch(a2a, "  Claude Desktop ");
    expect(id).toBe("claude-desktop-2");
    expect(patch).toEqual({
      a2a: {
        callers: [
          { id: "claude-desktop", name: "Claude Desktop", allowApprovals: false },
          { id: "claude-desktop-2", name: "Claude Desktop", allowApprovals: false },
        ],
      },
    });
    expect(callerApprovalsPatch(a2a, "claude-desktop", true)).toEqual({ a2a: { callers: [{ id: "claude-desktop", name: "Claude Desktop", allowApprovals: true }] } });
    expect(removeCallerPatch(a2a, "claude-desktop")).toEqual({ a2a: { callers: [] } });
    expect(JSON.stringify(addCallerPatch(a2a, "x").patch)).not.toContain("token");
  });

  test("outside agents: tokens absent keep, a string sets, null clears; ids from the name or the host", () => {
    const added = addAgentPatch(a2a, { url: " https://agents.example.com/a2a ", token: " s3cret " });
    expect(added.id).toBe("agents-example-com");
    expect(added.patch).toEqual({
      a2a: {
        agents: [
          { id: "gemini", name: "Gemini CLI", url: "http://127.0.0.1:41242" },
          { id: "agents-example-com", url: "https://agents.example.com/a2a", token: "s3cret" },
        ],
      },
    });
    expect(addAgentPatch(a2a, { url: "https://x.dev", name: "Review bot" }).patch.a2a?.agents?.[1]).toEqual({ id: "review-bot", name: "Review bot", url: "https://x.dev" });
    expect(agentTokenPatch(a2a, "gemini", "new")).toEqual({ a2a: { agents: [{ id: "gemini", name: "Gemini CLI", url: "http://127.0.0.1:41242", token: "new" }] } });
    expect(agentTokenPatch(a2a, "gemini", null)).toEqual({ a2a: { agents: [{ id: "gemini", name: "Gemini CLI", url: "http://127.0.0.1:41242", token: null }] } });
    expect(removeAgentPatch(a2a, "gemini")).toEqual({ a2a: { agents: [] } });
  });

  test("ids follow the hub's rule", () => {
    expect(suggestA2aId("Zed Editor!", [])).toBe("zed-editor");
    expect(suggestA2aId("", [])).toBe("caller");
    expect(suggestA2aId("é", [], "agent")).toBe("e");
    expect(suggestA2aId("a".repeat(60), [])).toMatch(/^[a-z0-9-]{1,40}$/);
  });
});

describe("Settings > A2A: checks and labels", () => {
  test("agent URLs: https, or http only to this machine; no user info", () => {
    expect(checkA2aAgentUrl("https://agents.example.com")).toBeNull();
    expect(checkA2aAgentUrl("http://127.0.0.1:41242")).toBeNull();
    expect(checkA2aAgentUrl("http://localhost:9000/card.json")).toBeNull();
    expect(checkA2aAgentUrl("http://agents.example.com")).toMatch(/https/);
    expect(checkA2aAgentUrl("https://me:pw@agents.example.com")).toMatch(/token/);
    expect(checkA2aAgentUrl("ftp://x")).toMatch(/https/);
    expect(checkA2aAgentUrl("agents.example.com")).toMatch(/full URL/);
    expect(checkA2aAgentUrl("")).not.toBeNull();
  });

  test("names and tokens", () => {
    expect(checkA2aName("  ")).not.toBeNull();
    expect(checkA2aName("x".repeat(81))).not.toBeNull();
    expect(checkA2aName("Zed")).toBeNull();
    expect(checkA2aToken("abc DEF")).not.toBeNull();
    expect(checkA2aToken("x".repeat(513))).not.toBeNull();
    expect(checkA2aToken("tok_123.abc-XYZ")).toBeNull();
  });

  test("the server's live state, its card URL, who started a task, field labels", () => {
    expect(a2aServerStatus(a2a)).toEqual({ tone: "off", text: "Off" });
    expect(a2aServerStatus({ ...a2a, server: true, running: true, url: "http://127.0.0.1:47802" }).text).toBe("Running at http://127.0.0.1:47802 (this PC only)");
    expect(a2aServerStatus({ ...a2a, server: true, error: "port 47802 is in use" })).toEqual({ tone: "error", text: "Not running: port 47802 is in use" });
    expect(a2aCardUrl("http://127.0.0.1:47802/")).toBe("http://127.0.0.1:47802/.well-known/agent-card.json");
    expect(a2aCallerLabel("a2a:claude-desktop", a2a)).toBe("Claude Desktop");
    expect(a2aCallerLabel("local", a2a)).toBe("you");
    expect(a2aCallerLabel("peer:mac1", a2a, () => "Mac mini")).toBe("Mac mini");
    expect(settingsFieldLabel("a2a.port")).toBe("A2A port");
  });

  test("an outside agent's session shows the agent's host where a folder would be", () => {
    expect(folderName("https://agents.example.com/a2a")).toBe("agents.example.com");
    expect(folderName("http://127.0.0.1:41242")).toBe("127.0.0.1:41242");
    expect(folderName("C:\\code\\api")).toBe("api");
  });
});

describe("Settings > A2A: what needs a confirm", () => {
  test("turning the server on says what callers can and cannot do", () => {
    const r = riskyChanges(hub(), { a2a: { server: true } });
    expect(r).toHaveLength(1);
    expect(r[0]!.key).toBe("a2a.server");
    expect(r[0]!.body).toContain("can ask Pawl what your agents are doing");
    expect(r[0]!.body).toContain("cannot approve anything unless you allow it for that caller");
    expect(r[0]!.body).toContain("127.0.0.1:47802 only");
    expect(riskyChanges(hub({ server: true }), { a2a: { server: true } })).toEqual([]);
    expect(riskyChanges(hub(), { a2a: { server: false } })).toEqual([]);
  });

  test("letting a caller answer approvals (never always, never run/delete/network); turning it off needs no confirm", () => {
    const r = riskyChanges(hub(), callerApprovalsPatch(a2a, "claude-desktop", true));
    expect(r.map((x) => x.key)).toEqual(["a2a.callers.claude-desktop.allowApprovals"]);
    expect(r[0]!.body).toContain("Never “always allow”");
    expect(r[0]!.body).toContain("never running commands, deleting files or network access");
    const on = { ...a2a, callers: [{ ...a2a.callers[0]!, allowApprovals: true }] };
    expect(riskyChanges(hub(on), callerApprovalsPatch(on, "claude-desktop", false))).toEqual([]);
    // Adding a caller (without approvals) or removing one needs no confirm.
    expect(riskyChanges(hub(), addCallerPatch(a2a, "Zed").patch)).toEqual([]);
  });

  test("an outside agent off this PC: prompts leave it; one on this PC needs no confirm", () => {
    const r = riskyChanges(hub(), addAgentPatch(a2a, { url: "https://agents.example.com/a2a" }).patch);
    expect(r).toHaveLength(1);
    expect(r[0]!.body).toContain("go to agents.example.com");
    expect(riskyChanges(hub(), addAgentPatch(a2a, { url: "http://localhost:9999" }).patch)).toEqual([]);
    // Changing an existing agent's token is not a new destination.
    expect(riskyChanges(hub(), agentTokenPatch(a2a, "gemini", "t"))).toEqual([]);
    // A hub without A2A: nothing.
    expect(riskyChanges({ ...hub(), a2a: undefined } as unknown as HubSettings, { a2a: { server: true } })).toEqual([]);
  });
});
