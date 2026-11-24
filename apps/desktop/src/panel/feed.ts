// Folds a session's raw HubEvents into readable feed items: streamed text chunks
// become paragraphs, tool_call + tool_call_update merge into one card, etc.
import type {
  HookPayload,
  HubEvent,
  PermissionOutcome,
  PermissionRequest,
  PermissionResolvedPayload,
  PlanEntry,
  StopReason,
  ToolCallPayload,
  ToolCallStatus,
  ToolKind,
  ToolLocation,
} from "@ai-pet/protocol";

export type FeedItem =
  | { kind: "text"; key: string; role: "agent" | "thought" | "user"; text: string; ts: number }
  | {
      kind: "tool";
      key: string;
      toolCallId: string;
      title: string;
      toolKind: ToolKind;
      status: ToolCallStatus;
      locations: ToolLocation[];
      ts: number;
    }
  | { kind: "plan"; key: string; entries: PlanEntry[]; ts: number }
  | { kind: "permission"; key: string; requestId?: string; title: string; toolKind: ToolKind; ts: number }
  | { kind: "resolved"; key: string; requestId: string; outcome: PermissionOutcome | null; by: string; ts: number }
  | { kind: "stop"; key: string; reason: StopReason | string; ts: number }
  | { kind: "error"; key: string; message: string; ts: number }
  | { kind: "notice"; key: string; icon: string; text: string; ts: number };

function obj(v: unknown): Record<string, unknown> {
  return v && typeof v === "object" ? (v as Record<string, unknown>) : {};
}

function str(v: unknown): string | undefined {
  return typeof v === "string" ? v : undefined;
}

function textOf(payload: unknown): string {
  const p = obj(payload);
  if (typeof p.text === "string") return p.text;
  // Some agents send ACP content blocks: { content: { type: "text", text } }
  const content = obj(p.content);
  if (typeof content.text === "string") return content.text;
  return "";
}

export function buildFeed(events: readonly HubEvent[]): FeedItem[] {
  const items: FeedItem[] = [];
  const toolIndex = new Map<string, number>();

  const appendText = (role: "agent" | "thought" | "user", ev: HubEvent) => {
    const text = textOf(ev.payload);
    if (!text) return;
    const last = items[items.length - 1];
    if (last && last.kind === "text" && last.role === role && role !== "user") {
      items[items.length - 1] = { ...last, text: last.text + text };
      return;
    }
    items.push({ kind: "text", key: `e${ev.id}`, role, text, ts: ev.ts });
  };

  for (const ev of events) {
    switch (ev.type) {
      case "agent_message_chunk":
        appendText("agent", ev);
        break;
      case "agent_thought_chunk":
        appendText("thought", ev);
        break;
      case "user_message":
        appendText("user", ev);
        break;
      case "tool_call":
      case "tool_call_update": {
        const p = obj(ev.payload) as Partial<ToolCallPayload>;
        const id = p.toolCallId ?? `anon-${ev.id}`;
        const idx = toolIndex.get(id);
        if (idx !== undefined) {
          const prev = items[idx];
          if (prev && prev.kind === "tool") {
            items[idx] = {
              ...prev,
              title: p.title ?? prev.title,
              toolKind: p.kind ?? prev.toolKind,
              status: p.status ?? prev.status,
              locations: p.locations ?? prev.locations,
            };
          }
        } else {
          toolIndex.set(id, items.length);
          items.push({
            kind: "tool",
            key: `t${id}`,
            toolCallId: id,
            title: p.title ?? "Tool call",
            toolKind: p.kind ?? "other",
            status: p.status ?? "pending",
            locations: p.locations ?? [],
            ts: ev.ts,
          });
        }
        break;
      }
      case "plan": {
        const entries = (obj(ev.payload).entries as PlanEntry[] | undefined) ?? [];
        const last = items[items.length - 1];
        if (last && last.kind === "plan") items[items.length - 1] = { ...last, entries, ts: ev.ts };
        else items.push({ kind: "plan", key: `e${ev.id}`, entries, ts: ev.ts });
        break;
      }
      case "request_permission": {
        const p = obj(ev.payload) as Partial<PermissionRequest> & Partial<ToolCallPayload>;
        const tool = obj(p.toolCall) as Partial<ToolCallPayload>;
        items.push({
          kind: "permission",
          key: `e${ev.id}`,
          requestId: str(p.requestId),
          title: tool.title ?? p.title ?? "Permission requested",
          toolKind: tool.kind ?? p.kind ?? "other",
          ts: ev.ts,
        });
        break;
      }
      case "permission_resolved": {
        const p = obj(ev.payload) as Partial<PermissionResolvedPayload>;
        items.push({
          kind: "resolved",
          key: `e${ev.id}`,
          requestId: p.requestId ?? "",
          outcome: p.outcome ?? null,
          by: p.by ?? "user",
          ts: ev.ts,
        });
        break;
      }
      case "stop":
        items.push({ kind: "stop", key: `e${ev.id}`, reason: str(obj(ev.payload).stopReason) ?? "end_turn", ts: ev.ts });
        break;
      case "error":
        items.push({ kind: "error", key: `e${ev.id}`, message: str(obj(ev.payload).message) ?? "Error", ts: ev.ts });
        break;
      case "session_start":
        items.push({ kind: "notice", key: `e${ev.id}`, icon: "🐾", text: "Session started", ts: ev.ts });
        break;
      case "session_end":
        items.push({ kind: "notice", key: `e${ev.id}`, icon: "🛏️", text: "Session ended", ts: ev.ts });
        break;
      case "notification": {
        const p = obj(ev.payload);
        const text = [str(p.title), str(p.body) ?? str(p.message)].filter(Boolean).join(": ") || "Notification";
        items.push({ kind: "notice", key: `e${ev.id}`, icon: "🔔", text, ts: ev.ts });
        break;
      }
      case "hook": {
        const p = obj(ev.payload) as Partial<HookPayload>;
        const text = p.message ? `${p.hookEvent ?? "hook"}: ${p.message}` : (p.hookEvent ?? "hook event");
        items.push({ kind: "notice", key: `e${ev.id}`, icon: "🪝", text: `${p.source ?? "agent"} · ${text}`, ts: ev.ts });
        break;
      }
    }
  }
  return items;
}

export const STOP_LABEL: Record<string, string> = {
  end_turn: "Turn finished",
  max_tokens: "Stopped: token limit",
  max_turn_requests: "Stopped: too many requests",
  refusal: "The agent refused",
  cancelled: "Cancelled",
};
