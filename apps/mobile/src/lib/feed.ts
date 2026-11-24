// Pure: turn a session's raw HubEvents into a readable feed (chunks merged, tool updates folded in).
import type {
  ErrorPayload,
  HookPayload,
  HubEvent,
  PermissionOutcome,
  PlanEntry,
  PlanPayload,
  StopReason,
  ToolCallPayload,
  ToolCallStatus,
  ToolKind,
  ToolLocation,
} from "@krypto/protocol";

export type FeedItem =
  | { kind: "user"; key: string; ts: number; text: string }
  | { kind: "agent"; key: string; ts: number; text: string }
  | { kind: "thought"; key: string; ts: number; text: string }
  | {
      kind: "tool";
      key: string;
      ts: number;
      toolCallId: string;
      title: string;
      toolKind?: ToolKind;
      status?: ToolCallStatus;
      locations: ToolLocation[];
    }
  | { kind: "plan"; key: string; ts: number; entries: PlanEntry[] }
  | { kind: "permission"; key: string; ts: number; requestId?: string; title: string; outcome?: PermissionOutcome; by?: string }
  | { kind: "stop"; key: string; ts: number; reason: StopReason | string }
  | { kind: "error"; key: string; ts: number; message: string }
  | { kind: "system"; key: string; ts: number; text: string; tone: "info" | "alert" };

function obj(v: unknown): Record<string, unknown> {
  return typeof v === "object" && v !== null ? (v as Record<string, unknown>) : {};
}

function text(v: unknown): string {
  const t = obj(v).text;
  return typeof t === "string" ? t : "";
}

const KIND_VERB: Record<string, string> = {
  read: "Read",
  edit: "Edit",
  delete: "Delete",
  move: "Move",
  search: "Search",
  execute: "Run",
  think: "Think",
  fetch: "Fetch",
  switch_mode: "Switch mode",
  other: "Tool",
};

export function toolTitle(tc: Partial<ToolCallPayload>): string {
  if (tc.title && tc.title.trim()) return tc.title.trim();
  const verb = KIND_VERB[tc.kind ?? "other"] ?? "Tool";
  const p = tc.locations?.[0]?.path;
  return p ? `${verb} ${p}` : verb;
}

function hookText(p: Partial<HookPayload>): string {
  const src = p.source ? `${p.source[0]!.toUpperCase()}${p.source.slice(1)}` : "Hook";
  const ev = p.hookEvent ?? "event";
  return p.message ? `${src} · ${ev}: ${p.message}` : `${src} · ${ev}`;
}

/**
 * Build the feed. Events must be in ascending id order (hub-client keeps them that way).
 * Consecutive message/thought chunks merge; tool_call_update folds into its tool_call item;
 * permission_resolved marks the matching permission item.
 */
export function buildFeed(events: readonly HubEvent[]): FeedItem[] {
  const out: FeedItem[] = [];
  const tools = new Map<string, Extract<FeedItem, { kind: "tool" }>>();
  const perms = new Map<string, Extract<FeedItem, { kind: "permission" }>>();
  // Mutable copies so merges never touch earlier output objects held by React.
  for (const ev of events) {
    const last = out[out.length - 1];
    const p = obj(ev.payload);
    switch (ev.type) {
      case "agent_message_chunk":
      case "agent_thought_chunk": {
        const kind = ev.type === "agent_message_chunk" ? "agent" : "thought";
        const t = text(ev.payload);
        if (!t) break;
        if (last && last.kind === kind) {
          out[out.length - 1] = { ...last, text: last.text + t };
        } else out.push({ kind, key: `e${ev.id}`, ts: ev.ts, text: t });
        break;
      }
      case "user_message": {
        const t = text(ev.payload);
        if (t) out.push({ kind: "user", key: `e${ev.id}`, ts: ev.ts, text: t });
        break;
      }
      case "tool_call":
      case "tool_call_update": {
        const tc = p as Partial<ToolCallPayload>;
        const id = typeof tc.toolCallId === "string" ? tc.toolCallId : `anon-${ev.id}`;
        const existing = tools.get(id);
        if (existing) {
          const idx = out.indexOf(existing);
          const merged: Extract<FeedItem, { kind: "tool" }> = {
            ...existing,
            title: tc.title?.trim() ? tc.title.trim() : existing.title,
            ...(tc.kind ? { toolKind: tc.kind } : {}),
            ...(tc.status ? { status: tc.status } : {}),
            locations: tc.locations?.length ? tc.locations : existing.locations,
          };
          tools.set(id, merged);
          if (idx >= 0) out[idx] = merged;
        } else {
          const item: Extract<FeedItem, { kind: "tool" }> = {
            kind: "tool",
            key: `e${ev.id}`,
            ts: ev.ts,
            toolCallId: id,
            title: toolTitle(tc),
            ...(tc.kind ? { toolKind: tc.kind } : {}),
            ...(tc.status ? { status: tc.status } : {}),
            locations: tc.locations ?? [],
          };
          tools.set(id, item);
          out.push(item);
        }
        break;
      }
      case "plan": {
        const rawEntries = (p as Partial<PlanPayload>).entries;
        const entries: PlanEntry[] = Array.isArray(rawEntries) ? rawEntries : [];
        if (last && last.kind === "plan") out[out.length - 1] = { ...last, entries, ts: ev.ts };
        else out.push({ kind: "plan", key: `e${ev.id}`, ts: ev.ts, entries });
        break;
      }
      case "request_permission": {
        const requestId = typeof p.requestId === "string" ? p.requestId : undefined;
        const tc = obj(p.toolCall ?? p) as Partial<ToolCallPayload>;
        const item: Extract<FeedItem, { kind: "permission" }> = {
          kind: "permission",
          key: `e${ev.id}`,
          ts: ev.ts,
          title: toolTitle(tc),
          ...(requestId ? { requestId } : {}),
        };
        if (requestId) perms.set(requestId, item);
        out.push(item);
        break;
      }
      case "permission_resolved": {
        const requestId = typeof p.requestId === "string" ? p.requestId : undefined;
        const outcome = p.outcome as PermissionOutcome | undefined;
        const by = typeof p.by === "string" ? p.by : undefined;
        const existing = requestId ? perms.get(requestId) : undefined;
        if (existing) {
          const idx = out.indexOf(existing);
          const merged = { ...existing, ...(outcome ? { outcome } : {}), ...(by ? { by } : {}) };
          if (requestId) perms.set(requestId, merged);
          if (idx >= 0) out[idx] = merged;
        }
        break;
      }
      case "stop": {
        const reason = typeof p.stopReason === "string" ? p.stopReason : "end_turn";
        out.push({ kind: "stop", key: `e${ev.id}`, ts: ev.ts, reason });
        break;
      }
      case "error": {
        const message = (p as Partial<ErrorPayload>).message;
        out.push({ kind: "error", key: `e${ev.id}`, ts: ev.ts, message: typeof message === "string" && message ? message : "Something went wrong" });
        break;
      }
      case "session_start":
        out.push({ kind: "system", key: `e${ev.id}`, ts: ev.ts, text: "Session started", tone: "info" });
        break;
      case "session_end":
        out.push({ kind: "system", key: `e${ev.id}`, ts: ev.ts, text: "Session ended", tone: "info" });
        break;
      case "notification": {
        const title = typeof p.title === "string" ? p.title : "";
        const body = typeof p.body === "string" ? p.body : typeof p.message === "string" ? p.message : "";
        out.push({ kind: "system", key: `e${ev.id}`, ts: ev.ts, text: [title, body].filter(Boolean).join(" — ") || "Notification", tone: "alert" });
        break;
      }
      case "hook":
        out.push({ kind: "system", key: `e${ev.id}`, ts: ev.ts, text: hookText(p as Partial<HookPayload>), tone: "info" });
        break;
      default:
        break;
    }
  }
  return out;
}

export function stopReasonLabel(reason: string): string {
  switch (reason) {
    case "end_turn":
      return "Turn finished";
    case "cancelled":
      return "Cancelled";
    case "refusal":
      return "The agent refused";
    case "max_tokens":
      return "Ran out of tokens";
    case "max_turn_requests":
      return "Hit the turn limit";
    default:
      return `Stopped (${reason})`;
  }
}
