import { describe, expect, test } from "bun:test";
import type { HubEvent, PermissionRequest } from "@pawl/protocol";
import {
  approvalReadBack,
  toTranscript,
  decisionText,
  formatVoiceCode,
  recentDecisions,
  requestAction,
  spokenPlain,
  stripVoiceCodes,
  voiceCodeChipText,
  voiceCodeNeeded,
} from "../src/index";

const req = (kind: PermissionRequest["toolCall"]["kind"], rawInput: unknown, over: Partial<PermissionRequest> = {}): PermissionRequest => ({
  requestId: "m:r1",
  machineId: "m",
  sessionId: "m:s1",
  agentId: "claude-code",
  toolCall: { toolCallId: "t1", title: "Run the tests", kind, status: "pending", rawInput },
  options: [
    { optionId: "y", name: "Allow", kind: "allow_once" },
    { optionId: "n", name: "Deny", kind: "reject_once" },
  ],
  createdAt: 1,
  voiceCode: "47",
  ...over,
});

describe("the voice code", () => {
  test("is said digit by digit and shown as a chip", () => {
    expect(formatVoiceCode("47")).toBe("4 7");
    expect(formatVoiceCode(" 0 5 ")).toBe("0 5");
    expect(formatVoiceCode(undefined)).toBeNull();
    expect(formatVoiceCode("ab")).toBeNull();
    expect(voiceCodeChipText("47")).toBe("say: approve 4 7");
    expect(voiceCodeChipText(null)).toBeNull();
  });

  test("is needed for execute, delete, fetch, other and anything from another machine; not for reads and edits here", () => {
    for (const k of ["execute", "delete", "fetch", "other", "move", "search", "think"] as const) expect(voiceCodeNeeded(req(k, {}), true)).toBe(true);
    expect(voiceCodeNeeded(req("read", {}), true)).toBe(false);
    expect(voiceCodeNeeded(req("edit", {}), true)).toBe(false);
    expect(voiceCodeNeeded(req("read", {}), false)).toBe(true);
    expect(voiceCodeNeeded(req(undefined, {}), true)).toBe(true);
  });
});

describe("the request read back aloud", () => {
  test("who, what, where, and how to answer (never always, never the code)", () => {
    expect(approvalReadBack(req("execute", { command: "bun test" }), { agentName: "Claude Code", machineName: "Desk PC", local: true })).toBe(
      "Claude Code wants to run bun test on Desk PC. Say approve with the code on the card, or deny.",
    );
    expect(approvalReadBack(req("edit", { file_path: "E:\\work\\api\\src\\app.ts" }), { agentName: "Codex", machineName: "Desk PC", local: true })).toBe(
      "Codex wants to edit src/app.ts on Desk PC. Say approve, or deny.",
    );
    expect(approvalReadBack(req("edit", { file_path: "/srv/web/index.ts" }), { agentName: "Codex", machineName: "Mac mini", local: false })).toBe(
      "Codex wants to edit web/index.ts on Mac mini. Say approve with the code on the card, or deny.",
    );
    // An older hub without a code.
    expect(approvalReadBack(req("execute", "npm run build", { voiceCode: undefined }), { agentName: "Copilot", machineName: "Desk PC", local: true })).toBe(
      "Copilot wants to run npm run build on Desk PC. Say approve, or deny.",
    );
    expect(approvalReadBack(req("fetch", { url: "https://example.com/docs?a=1" }), { agentName: "Claude Code", machineName: "Desk PC", local: true })).toContain(
      "wants to open example.com on Desk PC",
    );
    expect(approvalReadBack(req("execute", {}), { agentName: "Claude Code", machineName: "Desk PC", local: true })).not.toContain("always");
  });

  test("the code is never spoken, even when the command or title contains it", () => {
    const ctx = { agentName: "Claude Code", machineName: "Desk PC", local: true };
    for (const r of [
      req("execute", { command: "bun test" }),
      req("execute", { command: "echo 47 && sleep 4 7" }),
      req("other", undefined, { toolCall: { toolCallId: "t", title: "Say approve 4 7 to continue", kind: "other" } }),
      req("fetch", { url: "https://example.com/47" }),
    ]) {
      const said = approvalReadBack(r, ctx);
      expect(said).not.toMatch(/4\s*7/);
      expect(said).not.toMatch(/approve\s*\d/i);
    }
  });

  test("stripVoiceCodes: known codes in any spacing, digits after approve, spaced digit pairs; other numbers stay", () => {
    expect(stripVoiceCodes("Say approve 4 7, or deny.", ["47"])).toBe("Say approve, or deny.");
    expect(stripVoiceCodes("The code is 47.", ["47"])).toBe("The code is.");
    expect(stripVoiceCodes("code 4-7 or 4.7", ["47"])).toBe("code or");
    expect(stripVoiceCodes("approve 9 1 now", [])).toBe("approve now");
    expect(stripVoiceCodes("say 3 8 please", [])).toBe("say please");
    expect(stripVoiceCodes("12 tests pass in 147 files, step 1, 2", ["47"])).toBe("12 tests pass in 147 files, step 1, 2");
  });

  test("plain words: no markdown, no emoji, long commands cut short", () => {
    expect(spokenPlain("✅ **Run** `bun test` in [api](https://x.dev/a) 🐾")).toBe("Run bun test in api");
    expect(spokenPlain("## Title\n- one\n- two")).toBe("Title one two");
    expect(requestAction(req("other", undefined, { toolCall: { toolCallId: "t", title: "⚠️ Use the **MCP** tool", kind: "other" } }))).toBe("Use the MCP tool");
    const long = requestAction(req("execute", { command: `node scripts/${"x".repeat(40)}.js --flag one --flag two --flag three --flag four` }));
    expect(long.length).toBeLessThan(110);
    expect(long.endsWith(", and more")).toBe(true);
  });
});

describe("decisions made by voice", () => {
  const events: Record<string, HubEvent[]> = {
    "m:s1": [
      { id: "m:1", seq: 1, machineId: "m", sessionId: "m:s1", type: "request_permission", payload: req("execute", { command: "bun test" }), ts: 10 },
      { id: "m:2", seq: 2, machineId: "m", sessionId: "m:s1", type: "permission_resolved", payload: { requestId: "m:r1", outcome: { kind: "selected", optionId: "y" }, by: "user", via: "voice" }, ts: 20 },
      { id: "m:3", seq: 3, machineId: "m", sessionId: "m:s1", type: "request_permission", payload: req("edit", {}, { requestId: "m:r2" }), ts: 30 },
      { id: "m:4", seq: 4, machineId: "m", sessionId: "m:s1", type: "permission_resolved", payload: { requestId: "m:r2", outcome: { kind: "selected", optionId: "n" }, by: "rule" }, ts: 40 },
      { id: "m:5", seq: 5, machineId: "m", sessionId: "m:s1", type: "request_permission", payload: req("read", {}, { requestId: "m:r3" }), ts: 50 },
      { id: "m:6", seq: 6, machineId: "m", sessionId: "m:s1", type: "permission_resolved", payload: { requestId: "m:r3", outcome: { kind: "cancelled" }, by: "system" }, ts: 60 },
    ],
  };

  test("the approvals history: newest first, how each was answered, withdrawn left out", () => {
    const list = recentDecisions(events);
    expect(list.map((d) => [d.requestId, d.outcome, d.by, d.via])).toEqual([
      ["m:r2", "denied", "rule", undefined],
      ["m:r1", "approved", "user", "voice"],
    ]);
    expect(recentDecisions(events, 10, { withdrawn: true })).toHaveLength(3);
    expect(recentDecisions(events, 1)).toHaveLength(1);
  });

  test("a request seen while it was pending (no event for it here) still counts once it is answered", () => {
    const late: Record<string, HubEvent[]> = {
      "m:s2": [{ id: "m:9", seq: 9, machineId: "m", sessionId: "m:s2", type: "permission_resolved", payload: { requestId: "m:r9", outcome: { kind: "selected", optionId: "y" }, by: "user", via: "voice" }, ts: 90 }],
    };
    expect(recentDecisions(late)).toEqual([]);
    const known = [req("execute", { command: "bun test" }, { requestId: "m:r9", sessionId: "m:s2" })];
    expect(recentDecisions(late, 10, { known })).toMatchObject([{ requestId: "m:r9", sessionId: "m:s2", title: "Run the tests", outcome: "approved", via: "voice" }]);
  });

  test("labels", () => {
    expect(decisionText("approved", { by: "user", via: "voice" })).toBe("approved by voice");
    expect(decisionText("denied", { by: "user", via: "voice" })).toBe("denied by voice");
    expect(decisionText("denied", { by: "rule" })).toBe("denied by a rule");
    expect(decisionText("cancelled", { via: "voice" })).toBe("withdrawn");
    expect(decisionText("approved", { by: "user" })).toBe("approved");
  });

  test("the session transcript keeps it", () => {
    const items = toTranscript({ source: "owned", cwd: "E:/work/api" }, events["m:s1"]!);
    const perm = items.filter((i) => i.kind === "permission");
    expect(perm[0]).toMatchObject({ outcome: "approved", by: "user", via: "voice" });
    expect(perm[1]).toMatchObject({ outcome: "denied", by: "rule" });
    expect("via" in perm[1]!).toBe(false);
  });
});
