// Pure: turn a session's event log into display items for the session detail view.
import {
  isTruncatedPayload,
  type ErrorPayload,
  type HookPayload,
  type HubEvent,
  type PermissionResolvedPayload,
  type PlanEntry,
  type PlanPayload,
  type StopPayload,
  type TextPayload,
  type ToolCallPayload,
  type ToolCallStatus,
  type ToolKind,
  type ToolLocation,
} from "@pawl/protocol";

export type TranscriptItem =
  | { kind: "user"; key: string; text: string; ts: number }
  | { kind: "agent"; key: string; text: string; ts: number }
  | { kind: "thought"; key: string; text: string; ts: number }
  | {
      kind: "tool";
      key: string;
      toolCallId: string;
      title: string;
      toolKind?: ToolKind;
      status?: ToolCallStatus;
      locations: ToolLocation[];
      ts: number;
    }
  | { kind: "plan"; key: string; entries: PlanEntry[]; ts: number }
  | {
      kind: "permission";
      key: string;
      text: string;
      ts: number;
      /** A decision made on another machine than the session's owner ("Approved from Windows PC"). */
      fromMachineId?: string | undefined;
    }
  | { kind: "stop"; key: string; reason: string; ts: number }
  | { kind: "error"; key: string; text: string; ts: number }
  | { kind: "note"; key: string; text: string; ts: number }
  /** A mirrored event whose payload was too large to copy over (open the session on its machine). */
  | { kind: "truncated"; key: string; bytes: number; ts: number };

function text(payload: unknown): string {
  const t = (payload as Partial<TextPayload> | null)?.text;
  return typeof t === "string" ? t : "";
}

const STOP_LABEL: Record<string, string> = {
  end_turn: "Turn complete",
  max_tokens: "Stopped: out of tokens",
  max_turn_requests: "Stopped: turn limit reached",
  refusal: "Stopped: the agent refused",
  cancelled: "Cancelled",
};

/**
 * Consecutive message/thought chunks are concatenated; tool_call_update events are folded into the
 * originating tool call row; only the latest plan is kept in place (older plans are replaced).
 */
export function buildTranscript(events: readonly HubEvent[]): TranscriptItem[] {
  const items: TranscriptItem[] = [];
  const tools = new Map<string, Extract<TranscriptItem, { kind: "tool" }>>();
  let planIndex = -1;

  for (const e of events) {
    const last = items[items.length - 1];
    if (isTruncatedPayload(e.payload)) {
      const bytes = typeof e.payload.bytes === "number" ? e.payload.bytes : 0;
      // Consecutive large chunks read as one gap.
      if (last?.kind === "truncated") items[items.length - 1] = { ...last, bytes: last.bytes + bytes };
      else items.push({ kind: "truncated", key: e.id, bytes, ts: e.ts });
      continue;
    }
    switch (e.type) {
      case "user_message": {
        const t = text(e.payload);
        if (t) items.push({ kind: "user", key: `u${e.id}`, text: t, ts: e.ts });
        break;
      }
      case "agent_message_chunk": {
        const t = text(e.payload);
        if (!t) break;
        if (last?.kind === "agent") last.text += t;
        else items.push({ kind: "agent", key: `a${e.id}`, text: t, ts: e.ts });
        break;
      }
      case "agent_thought_chunk": {
        const t = text(e.payload);
        if (!t) break;
        if (last?.kind === "thought") last.text += t;
        else items.push({ kind: "thought", key: `t${e.id}`, text: t, ts: e.ts });
        break;
      }
      case "tool_call":
      case "tool_call_update": {
        const p = (e.payload ?? {}) as Partial<ToolCallPayload>;
        if (typeof p.toolCallId !== "string") break;
        let row = tools.get(p.toolCallId);
        if (!row) {
          row = {
            kind: "tool",
            key: `c${e.id}`,
            toolCallId: p.toolCallId,
            title: p.title || p.kind || "Tool call",
            toolKind: p.kind,
            status: p.status,
            locations: p.locations ?? [],
            ts: e.ts,
          };
          tools.set(p.toolCallId, row);
          items.push(row);
        } else {
          if (p.title) row.title = p.title;
          if (p.kind) row.toolKind = p.kind;
          if (p.status) row.status = p.status;
          if (p.locations?.length) row.locations = p.locations;
        }
        break;
      }
      case "plan": {
        const entries = (e.payload as Partial<PlanPayload> | null)?.entries;
        if (!Array.isArray(entries)) break;
        const item: TranscriptItem = { kind: "plan", key: `p${e.id}`, entries, ts: e.ts };
        if (planIndex >= 0 && planIndex === items.length - 1) items[planIndex] = item;
        else {
          if (planIndex >= 0) items.splice(planIndex, 1);
          items.push(item);
          planIndex = items.length - 1;
        }
        break;
      }
      case "request_permission": {
        const p = e.payload as { toolCall?: Partial<ToolCallPayload> } | null;
        const what = p?.toolCall?.title || p?.toolCall?.kind || "a tool call";
        items.push({ kind: "permission", key: `r${e.id}`, text: `Asked to approve: ${what}`, ts: e.ts });
        break;
      }
      case "permission_resolved": {
        const p = e.payload as Partial<PermissionResolvedPayload> | null;
        const outcome = p?.outcome?.kind === "selected" ? `option “${p.outcome.optionId}”` : "cancelled";
        const from = typeof p?.byMachineId === "string" && p.byMachineId ? p.byMachineId : undefined;
        items.push({ kind: "permission", key: `rr${e.id}`, text: `Answered (${p?.by ?? "user"}): ${outcome}`, ts: e.ts, fromMachineId: from });
        break;
      }
      case "stop": {
        const reason = (e.payload as Partial<StopPayload> | null)?.stopReason ?? "end_turn";
        items.push({ kind: "stop", key: `s${e.id}`, reason: STOP_LABEL[reason] ?? `Stopped (${reason})`, ts: e.ts });
        break;
      }
      case "error": {
        const m = (e.payload as Partial<ErrorPayload> | null)?.message ?? "Unknown error";
        items.push({ kind: "error", key: `e${e.id}`, text: m, ts: e.ts });
        break;
      }
      case "hook": {
        const p = e.payload as Partial<HookPayload> | null;
        const label = [p?.hookEvent, p?.message].filter(Boolean).join(": ");
        if (label) items.push({ kind: "note", key: `h${e.id}`, text: label, ts: e.ts });
        break;
      }
      case "session_start":
        items.push({ kind: "note", key: `ss${e.id}`, text: "Session started", ts: e.ts });
        break;
      case "session_end":
        items.push({ kind: "note", key: `se${e.id}`, text: "Session ended", ts: e.ts });
        break;
      case "notification": {
        const p = e.payload as { title?: string; body?: string } | null;
        const label = [p?.title, p?.body].filter(Boolean).join(": ");
        if (label) items.push({ kind: "note", key: `n${e.id}`, text: label, ts: e.ts });
        break;
      }
      default:
        break;
    }
  }
  return items;
}
