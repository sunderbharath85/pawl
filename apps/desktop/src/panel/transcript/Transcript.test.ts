// Render smoke test: every transcript block renders (server-side, no DOM) with the AI Elements components, and none
// of the raw hook data or injected markup reaches the page.
import { describe, expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { EventType, HubEvent } from "@pawl/protocol";
import { groupWork, toTranscript } from "@pawl/hub-client";
import { Block } from "./Transcript";

let seq = 0;
function hook(hookEvent: string, raw: Record<string, unknown>): HubEvent {
  seq++;
  const type: EventType = hookEvent === "SessionStart" ? "session_start" : "hook";
  return { id: `m1:${seq}`, seq, machineId: "m1", sessionId: "m1:s1", type, payload: { source: "claude", hookEvent, raw: { hook_event_name: hookEvent, ...raw } }, ts: 1_700_000_000_000 + seq };
}
function acp(type: EventType, payload: unknown): HubEvent {
  seq++;
  return { id: `m1:${seq}`, seq, machineId: "m1", sessionId: "m1:s2", type, payload, ts: 1_700_000_000_000 + seq };
}

function render(events: HubEvent[], source: "cli_hook" | "owned"): string {
  const blocks = groupWork(toTranscript({ source }, events));
  return blocks.map((b) => renderToStaticMarkup(createElement(Block, { block: b }))).join("\n");
}

describe("transcript blocks render", () => {
  test("a hook session: bubble, attachments, work step, markdown reply, notices", () => {
    const html = render(
      [
        hook("SessionStart", { source: "startup" }),
        hook("UserPromptSubmit", { prompt: 'why does it fail? [Image #3]\n<pasted_content id="1">FAIL x</pasted_content><system-reminder>secret plan</system-reminder>' }),
        hook("PreToolUse", { tool_name: "Bash", tool_use_id: "a", tool_input: { command: "git status" } }),
        hook("PostToolUse", { tool_name: "Bash", tool_use_id: "a", tool_input: { command: "git status" }, tool_response: { stdout: " M x.ts" } }),
        hook("PreToolUse", { tool_name: "Read", tool_use_id: "b", tool_input: { file_path: "x.ts" } }),
        hook("PostToolUse", { tool_name: "Read", tool_use_id: "b", tool_input: { file_path: "x.ts" }, tool_response: "x" }),
        hook("Stop", { last_assistant_message: "Fixed **it**.\n\n```bash\nbun test\n```\n\nSee [docs](https://example.com)." }),
        hook("UserPromptSubmit", { prompt: "<task-notification><status>completed</status><summary>build done</summary></task-notification>" }),
        hook("Notification", { notification_type: "idle_prompt" }),
      ],
      "cli_hook",
    );
    expect(html).toContain("why does it fail?");
    expect(html).toContain("Image #3");
    expect(html).toContain("Pasted text");
    expect(html).toContain("Worked: ran 1 command, read 1 file");
    expect(html).toContain(`data-streamdown="strong"`);
    expect(html).toContain("bun test");
    expect(html).toContain("Background task finished: build done");
    expect(html).toContain("Waiting for you in the terminal");
    // Links never navigate the panel.
    expect(html).not.toContain('href="https://example.com"');
    for (const raw of ["system-reminder", "secret plan", "pasted_content", "task-notification", "hook_event_name", "PreToolUse", "[Image #3]"]) {
      expect(html).not.toContain(raw);
    }
  });

  test("an owned session: reasoning, plan, tool with a diff, permission line, stop notice", () => {
    const html = render(
      [
        acp("user_message", { text: "rename it" }),
        acp("agent_thought_chunk", { text: "look first" }),
        acp("plan", { entries: [{ content: "Find uses", priority: "high", status: "completed" }] }),
        acp("tool_call", { toolCallId: "c1", title: "Edit a.ts", kind: "edit", status: "pending", content: [{ type: "diff", path: "a.ts", oldText: "a", newText: "b" }] }),
        acp("request_permission", { requestId: "m1:r1", toolCall: { toolCallId: "c1", title: "Edit a.ts", kind: "edit" }, options: [{ optionId: "y", name: "Allow", kind: "allow_once" }] }),
        acp("permission_resolved", { requestId: "m1:r1", outcome: { kind: "selected", optionId: "y" }, by: "rule" }),
        acp("tool_call_update", { toolCallId: "c1", status: "failed" }),
        acp("stop", { stopReason: "max_tokens" }),
      ],
      "owned",
    );
    expect(html).toContain("rename it");
    expect(html).toContain("Thought");
    expect(html).toContain("1 of 1 done");
    expect(html).toContain("Edit a.ts");
    expect(html).toContain("failed");
    expect(html).toContain("Asked for approval: Edit a.ts");
    expect(html).toContain("approved by a rule");
    expect(html).toContain("Stopped: it reached its token limit");
  });

  test("an approval answered by voice says so", () => {
    const html = render(
      [
        acp("request_permission", { requestId: "m1:r9", toolCall: { toolCallId: "c9", title: "Run bun test", kind: "execute" }, options: [{ optionId: "y", name: "Allow", kind: "allow_once" }, { optionId: "n", name: "Deny", kind: "reject_once" }] }),
        acp("permission_resolved", { requestId: "m1:r9", outcome: { kind: "selected", optionId: "y" }, by: "user", via: "voice" }),
        acp("request_permission", { requestId: "m1:r10", toolCall: { toolCallId: "c10", title: "Delete dist", kind: "delete" }, options: [{ optionId: "n", name: "Deny", kind: "reject_once" }] }),
        acp("permission_resolved", { requestId: "m1:r10", outcome: { kind: "selected", optionId: "n" }, by: "user", via: "voice" }),
      ],
      "owned",
    );
    expect(html).toContain("approved by voice");
    expect(html).toContain("denied by voice");
  });
});
