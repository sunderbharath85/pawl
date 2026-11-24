// Pure ACP/hook event -> pet activity mapping (PLAN.md "Pet engine" table).
import type { HookPayload, HubEvent, SessionStatus, StopPayload, ToolCallPayload, ToolKind } from "@pawl/protocol";

export type PetActivity = "napping" | "idle" | "thinking" | "working" | "planning" | "alert" | "happy" | "sad";

export const PET_ACTIVITIES: readonly PetActivity[] = ["napping", "idle", "thinking", "working", "planning", "alert", "happy", "sad"];

/** How long `happy` (after end_turn) lasts before decaying to idle. */
export const HAPPY_DECAY_MS = 6_000;
/** How long `sad` (after error/refusal) lasts before decaying to idle. */
export const SAD_DECAY_MS = 10_000;
/** With no active session for this long, the pet naps. */
export const NAP_AFTER_MS = 60_000;

/** Tool kinds where the pet "sniffs around" (thinking) instead of digging (working). */
const LOOKING_KINDS: ReadonlySet<ToolKind> = new Set<ToolKind>(["read", "search", "think", "switch_mode"]);

function obj(v: unknown): Record<string, unknown> | null {
  return typeof v === "object" && v !== null ? (v as Record<string, unknown>) : null;
}

function toolActivity(payload: unknown): PetActivity {
  const kind = obj(payload)?.["kind"] as ToolCallPayload["kind"];
  return kind && LOOKING_KINDS.has(kind) ? "thinking" : "working";
}

function stopActivity(payload: unknown): PetActivity | null {
  const reason = obj(payload)?.["stopReason"] as StopPayload["stopReason"] | undefined;
  switch (reason) {
    case "end_turn":
      return "happy";
    case "refusal":
    case "max_tokens":
    case "max_turn_requests":
      return "sad";
    case "cancelled":
      return "idle";
    default:
      return "happy";
  }
}

/** Claude Code / Codex / Copilot hook names -> activity. */
function hookActivity(payload: unknown): PetActivity | null {
  const p = obj(payload) as Partial<HookPayload> | null;
  const name = String(p?.hookEvent ?? "").toLowerCase().replace(/[^a-z]/g, "");
  switch (name) {
    case "sessionstart":
      return "idle";
    case "userpromptsubmit":
    case "userpromptsubmitted":
      return "thinking";
    case "pretooluse":
    case "posttooluse":
      return "working";
    case "notification":
    case "permissionrequest":
      return "alert";
    case "stop":
    case "subagentstop":
    case "agentturncomplete":
      return "happy";
    case "error":
    case "posttoolusefailure":
      return "sad";
    default:
      return null;
  }
}

/**
 * Pure. thought → thinking; tool_call edit/execute/... → working (read/search → thinking);
 * plan → planning; request_permission → alert; stop end_turn → happy; error/refusal → sad.
 * Returns null for events that should not change what the pet is doing.
 */
export function activityForEvent(e: HubEvent): PetActivity | null {
  switch (e.type) {
    case "user_message":
    case "agent_thought_chunk":
    case "agent_message_chunk":
      return "thinking";
    case "tool_call":
      return toolActivity(e.payload);
    case "tool_call_update": {
      const status = obj(e.payload)?.["status"] as ToolCallPayload["status"];
      if (status === "in_progress" || status === "pending") return toolActivity(e.payload);
      return null;
    }
    case "plan":
      return "planning";
    case "request_permission":
      return "alert";
    case "stop":
      return stopActivity(e.payload);
    case "error":
      return "sad";
    case "session_start":
      return "idle";
    case "notification":
      return "alert";
    case "hook":
      return hookActivity(e.payload);
    case "session_end":
    case "permission_resolved":
      return null;
    default:
      return null;
  }
}

/** Coarse activity for a session status (used when there is no informative last event). */
export function activityForStatus(status: SessionStatus): PetActivity {
  switch (status) {
    case "thinking":
      return "thinking";
    case "working":
      return "working";
    case "waiting_permission":
      return "alert";
    case "done":
      return "happy";
    case "error":
      return "sad";
    case "idle":
    default:
      return "idle";
  }
}

/** Newest event per session whose activity is non-null (skips noise like permission_resolved). */
export function lastSignificantEvents(events: Record<string, readonly HubEvent[] | undefined>): Record<string, HubEvent | undefined> {
  const out: Record<string, HubEvent | undefined> = {};
  for (const [sid, list] of Object.entries(events)) {
    if (!list) continue;
    for (let i = list.length - 1; i >= 0; i--) {
      const ev = list[i]!;
      if (activityForEvent(ev) !== null) {
        out[sid] = ev;
        break;
      }
    }
  }
  return out;
}
