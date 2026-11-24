// A scripted ACP agent for the end-to-end tests (JSON-RPC over stdio, one JSON message per line).
// Every prompt runs one tool call that needs permission, waits for the client's answer, reports it as
// "permission answer: <optionId>" and ends the turn:
//   prompt contains "read" -> read <cwd>/README.md (kind read: the default rules allow it inside the repo)
//   prompt contains "edit" -> edit <cwd>/notes.md  (kind edit: asks)
//   anything else          -> run `bun test`        (kind execute: always asks)
import { join } from "node:path";
import { createInterface } from "node:readline";

type Json = Record<string, any>;
const send = (m: Json) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", ...m }) + "\n");
const update = (sessionId: string, u: Json) => send({ method: "session/update", params: { sessionId, update: u } });

let sessions = 0;
let nextId = 1000;
const cwds = new Map<string, string>();
const waiting = new Map<number, { promptId: unknown; sessionId: string; call: string }>();

createInterface({ input: process.stdin }).on("line", (line) => {
  let m: Json;
  try {
    m = JSON.parse(line);
  } catch {
    return;
  }
  if (m.method === "initialize") {
    send({ id: m.id, result: { protocolVersion: 1, agentCapabilities: {} } });
  } else if (m.method === "session/new") {
    const sessionId = `e2e-${++sessions}`;
    cwds.set(sessionId, m.params.cwd);
    send({ id: m.id, result: { sessionId } });
  } else if (m.method === "session/prompt") {
    const sessionId: string = m.params.sessionId;
    const text: string = m.params.prompt?.[0]?.text ?? "";
    const cwd = cwds.get(sessionId) ?? ".";
    const call = `call-${nextId}`;
    const tool = text.includes("read")
      ? { toolCallId: call, title: "read README.md", kind: "read", locations: [{ path: join(cwd, "README.md") }] }
      : text.includes("edit")
        ? { toolCallId: call, title: "edit notes.md", kind: "edit", locations: [{ path: join(cwd, "notes.md") }] }
        : { toolCallId: call, title: "bun test", kind: "execute" };
    update(sessionId, { sessionUpdate: "tool_call", status: "pending", ...tool });
    const id = nextId++;
    waiting.set(id, { promptId: m.id, sessionId, call });
    send({
      id,
      method: "session/request_permission",
      params: {
        sessionId,
        toolCall: tool,
        options: [
          { optionId: "allow", name: "Allow", kind: "allow_once" },
          { optionId: "always", name: "Always allow", kind: "allow_always" },
          { optionId: "reject", name: "Reject", kind: "reject_once" },
        ],
      },
    });
  } else if (m.method === "session/cancel") {
    // a notification: the client answers the open permission request with `cancelled`
  } else if (m.id !== undefined && !m.method && waiting.has(m.id)) {
    const w = waiting.get(m.id)!;
    waiting.delete(m.id);
    const outcome = m.result?.outcome ?? {};
    const chosen: string = outcome.optionId ?? outcome.outcome ?? "none";
    const ok = chosen === "allow" || chosen === "always";
    update(w.sessionId, { sessionUpdate: "tool_call_update", toolCallId: w.call, status: ok ? "completed" : "failed" });
    update(w.sessionId, { sessionUpdate: "agent_message_chunk", content: { type: "text", text: `permission answer: ${chosen}` } });
    send({ id: w.promptId, result: { stopReason: outcome.outcome === "cancelled" ? "cancelled" : "end_turn" } });
  } else if (m.id !== undefined && m.method) {
    send({ id: m.id, error: { code: -32601, message: "Method not found" } });
  }
}).on("close", () => process.exit(0));
