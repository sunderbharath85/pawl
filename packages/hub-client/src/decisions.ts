// Approvals history (pure): the permission requests that were answered, from the session events this client has
// (live events since it connected, plus the sessions it loaded), newest first, with how each was answered (by you,
// by voice, by a rule, from another machine).
import type { HubEvent, PermissionOutcome, PermissionOption, PermissionRequest, PermissionResolvedPayload, ResolvedBy, ToolCallPayload, ToolKind } from "@pawl/protocol";

export interface Decision {
  requestId: string;
  sessionId: string;
  title: string;
  toolKind: ToolKind;
  outcome: "approved" | "denied" | "cancelled";
  by: ResolvedBy;
  via?: "voice";
  byMachineId?: string;
  ts: number;
}

type Obj = Record<string, unknown>;
const asObj = (v: unknown): Obj => (v && typeof v === "object" && !Array.isArray(v) ? (v as Obj) : {});

/**
 * Decisions in `events` (by session), newest first, at most `limit`. Withdrawn requests are left out unless asked for.
 * `known`: requests seen while they were pending (a window that connected after a request was made has no event for
 * it, only the request itself, then its resolution).
 */
export function recentDecisions(
  events: Readonly<Record<string, readonly HubEvent[]>>,
  limit = 10,
  opts: { withdrawn?: boolean; known?: Iterable<PermissionRequest> } = {},
): Decision[] {
  const out: Decision[] = [];
  const seen = new Map<string, { title: string; toolKind: ToolKind; options: PermissionOption[] }>();
  for (const r of opts.known ?? []) {
    seen.set(r.requestId, { title: r.toolCall.title || "Permission request", toolKind: r.toolCall.kind ?? "other", options: r.options });
  }
  for (const [sessionId, list] of Object.entries(events)) {
    const asked = new Map(seen);
    for (const ev of list) {
      const p = asObj(ev.payload);
      if (ev.type === "request_permission") {
        const tool = asObj(p.toolCall) as Partial<ToolCallPayload>;
        const id = typeof p.requestId === "string" ? p.requestId : "";
        if (id) {
          asked.set(id, {
            title: (tool.title ?? (typeof p.title === "string" ? p.title : "")) || "Permission request",
            toolKind: (tool.kind ?? (p.kind as ToolKind | undefined)) || "other",
            options: Array.isArray(p.options) ? (p.options as PermissionOption[]) : [],
          });
        }
      } else if (ev.type === "permission_resolved") {
        const r = p as Partial<PermissionResolvedPayload>;
        const id = typeof r.requestId === "string" ? r.requestId : "";
        const req = asked.get(id);
        if (!id || !req) continue;
        const o = r.outcome as PermissionOutcome | undefined;
        let outcome: Decision["outcome"] = "cancelled";
        if (o?.kind === "selected") {
          const kind = req.options.find((x) => x.optionId === o.optionId)?.kind;
          outcome = kind?.startsWith("reject") ? "denied" : "approved";
        }
        if (outcome === "cancelled" && !opts.withdrawn) continue;
        const d: Decision = { requestId: id, sessionId, title: req.title, toolKind: req.toolKind, outcome, by: r.by ?? "user", ts: ev.ts };
        if (r.via === "voice") d.via = "voice";
        if (typeof r.byMachineId === "string" && r.byMachineId) d.byMachineId = r.byMachineId;
        out.push(d);
      }
    }
  }
  return out.sort((a, b) => b.ts - a.ts || a.requestId.localeCompare(b.requestId)).slice(0, Math.max(0, limit));
}
