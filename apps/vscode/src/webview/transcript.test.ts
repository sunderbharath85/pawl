import { describe, expect, test } from "bun:test";
import type { HubEvent } from "@krypto/protocol";
import { buildTranscript } from "./transcript";

const MID = "abc123def456";
let nextSeq = 1;
const ev = (type: HubEvent["type"], payload: unknown): HubEvent => {
  const seq = nextSeq++;
  return { id: `${MID}:${seq}`, seq, machineId: MID, sessionId: `${MID}:s1`, type, payload, ts: 1000 + seq };
};

describe("buildTranscript", () => {
  test("concatenates consecutive chunks and splits on other events", () => {
    const items = buildTranscript([
      ev("user_message", { text: "fix it" }),
      ev("agent_thought_chunk", { text: "hmm " }),
      ev("agent_thought_chunk", { text: "ok" }),
      ev("agent_message_chunk", { text: "On " }),
      ev("agent_message_chunk", { text: "it." }),
      ev("stop", { stopReason: "end_turn" }),
      ev("agent_message_chunk", { text: "Again" }),
    ]);
    expect(items.map((i) => i.kind)).toEqual(["user", "thought", "agent", "stop", "agent"]);
    expect(items[1]).toMatchObject({ text: "hmm ok" });
    expect(items[2]).toMatchObject({ text: "On it." });
    expect(items[3]).toMatchObject({ reason: "Turn complete" });
  });

  test("folds tool_call_update into the original tool row", () => {
    const items = buildTranscript([
      ev("tool_call", { toolCallId: "t1", title: "Edit a.ts", kind: "edit", status: "pending" }),
      ev("agent_message_chunk", { text: "writing" }),
      ev("tool_call_update", { toolCallId: "t1", status: "completed", locations: [{ path: "/w/a.ts", line: 3 }] }),
    ]);
    expect(items).toHaveLength(2);
    expect(items[0]).toMatchObject({ kind: "tool", title: "Edit a.ts", toolKind: "edit", status: "completed" });
    expect(items[0].kind === "tool" && items[0].locations[0]?.line).toBe(3);
  });

  test("keeps only the latest plan, moved to its newest position", () => {
    const entry = (content: string, status: "pending" | "completed") => ({ content, priority: "medium", status });
    const items = buildTranscript([
      ev("plan", { entries: [entry("a", "pending")] }),
      ev("agent_message_chunk", { text: "x" }),
      ev("plan", { entries: [entry("a", "completed")] }),
      ev("plan", { entries: [entry("a", "completed"), entry("b", "pending")] }),
    ]);
    expect(items.map((i) => i.kind)).toEqual(["agent", "plan"]);
    expect(items[1].kind === "plan" && items[1].entries).toHaveLength(2);
  });

  test("row keys derive from global event ids and stay unique", () => {
    const first = ev("user_message", { text: "a" });
    const items = buildTranscript([first, ev("agent_message_chunk", { text: "b" }), ev("stop", { stopReason: "end_turn" })]);
    expect(items[0]?.key).toBe(`u${first.id}`);
    expect(items[0]?.key).toContain(`${MID}:`);
    expect(new Set(items.map((i) => i.key)).size).toBe(items.length);
  });

  test("ignores malformed payloads", () => {
    const items = buildTranscript([ev("agent_message_chunk", null), ev("tool_call", { nope: 1 }), ev("plan", { entries: "x" })]);
    expect(items).toEqual([]);
  });
});

describe("mirrored payloads that were too large", () => {
  test("show as one 'large output' row per run", () => {
    const items = buildTranscript([
      ev("agent_message_chunk", { text: "Reading the log" }),
      ev("tool_call", { truncated: true, bytes: 70_000 }),
      ev("tool_call_update", { truncated: true, bytes: 10_000 }),
      ev("agent_message_chunk", { text: "Done" }),
    ]);
    expect(items.map((i) => i.kind)).toEqual(["agent", "truncated", "agent"]);
    expect(items[1]).toMatchObject({ kind: "truncated", bytes: 80_000 });
  });
});

test("a decision from another machine keeps where it came from", () => {
  const items = buildTranscript([
    ev("permission_resolved", { requestId: "r1", outcome: { kind: "selected", optionId: "allow" }, by: "user", byMachineId: "winpc0000001" }),
    ev("permission_resolved", { requestId: "r2", outcome: { kind: "cancelled" }, by: "system" }),
  ]);
  expect(items[0]).toMatchObject({ kind: "permission", fromMachineId: "winpc0000001" });
  expect(items[1]).toMatchObject({ kind: "permission", fromMachineId: undefined });
});
