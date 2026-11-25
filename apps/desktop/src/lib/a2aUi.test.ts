import { describe, expect, test } from "bun:test";
import type { A2aSettings, A2aTaskSummary } from "@pawl/protocol";
import { a2aTaskChip, isOutsideAgent, outsideAgentUrl, startCwd } from "./a2aUi";
import { repoName } from "./petScreen";

const a2a: A2aSettings = {
  server: true,
  port: 47802,
  running: true,
  url: "http://127.0.0.1:47802",
  error: null,
  callers: [{ id: "zed", name: "Zed", tokenSet: true, allowApprovals: false }],
  agents: [{ id: "gemini", name: "Gemini CLI", url: "http://127.0.0.1:41242", tokenSet: false }],
};
const task = (over: Partial<A2aTaskSummary>): A2aTaskSummary => ({ taskId: "t", sessionId: "m:s1", state: "working", caller: "local", ...over });

describe("outside A2A agents in New session", () => {
  test("they work where they run: no folder needed, the hub gets the agent's URL", () => {
    expect(isOutsideAgent({ id: "a2a:gemini", source: "a2a" })).toBe(true);
    expect(isOutsideAgent({ id: "claude-code", source: "registry" })).toBe(false);
    expect(outsideAgentUrl("a2a:gemini", a2a)).toBe("http://127.0.0.1:41242");
    expect(outsideAgentUrl("a2a:other", a2a)).toBeNull();
    expect(startCwd({ id: "a2a:gemini", source: "a2a" }, "", a2a)).toBe("http://127.0.0.1:41242");
    expect(startCwd({ id: "a2a:peer-agent", source: "a2a" }, "", null)).toBe("a2a:peer-agent");
    expect(startCwd({ id: "codex", source: "registry" }, "  C:\\code\\api ", a2a)).toBe("C:\\code\\api");
  });

  test("their sessions show the agent's host where a repo would be (pet switcher, bubble)", () => {
    expect(repoName("https://agents.example.com/a2a")).toBe("agents.example.com");
    expect(repoName("http://127.0.0.1:41242")).toBe("127.0.0.1:41242");
    expect(repoName("E:/work/api/")).toBe("api");
  });
});

describe("the A2A task chip in the session view", () => {
  test("an outside agent's session: its latest task state", () => {
    const chip = a2aTaskChip([task({ state: "completed", updatedAt: 1 }), task({ state: "input_required", updatedAt: 5 })], { id: "m:s1", agentId: "a2a:gemini" }, a2a);
    expect(chip).toEqual({ label: "A2A task · Needs input", tone: "ask", title: "Latest task: Needs input." });
  });

  test("a session an outside caller or a paired machine started: who, and the state", () => {
    expect(a2aTaskChip([task({ caller: "a2a:zed", state: "working" })], { id: "m:s1", agentId: "claude-code" }, a2a)?.label).toBe("A2A · Zed · Working");
    expect(a2aTaskChip([task({ caller: "peer:mac1", state: "failed" })], { id: "m:s1", agentId: "codex" }, a2a, () => "Mac mini")).toMatchObject({
      label: "A2A · Mac mini · Failed",
      tone: "bad",
    });
  });

  test("your own ACP session, or no tasks: no chip", () => {
    expect(a2aTaskChip([task({})], { id: "m:s1", agentId: "claude-code" }, a2a)).toBeNull();
    expect(a2aTaskChip([], { id: "m:s1", agentId: "a2a:gemini" }, a2a)).toBeNull();
    expect(a2aTaskChip([task({ sessionId: "m:other" })], { id: "m:s1", agentId: "a2a:gemini" }, a2a)).toBeNull();
  });
});
