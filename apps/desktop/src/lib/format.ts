import type { Agent, PermissionRequest, Session, SessionStatus, ToolKind } from "@krypto/protocol";
import { agentFor } from "./machine";

export const STATUS_META: Record<SessionStatus, { label: string; tone: string }> = {
  idle: { label: "Idle", tone: "idle" },
  thinking: { label: "Thinking", tone: "thinking" },
  working: { label: "Working", tone: "working" },
  waiting_permission: { label: "Needs you", tone: "waiting" },
  done: { label: "Done", tone: "done" },
  error: { label: "Error", tone: "error" },
};

export const SOURCE_LABEL: Record<Session["source"], string> = {
  owned: "Pet-run",
  vscode_hook: "VS Code",
  cli_hook: "Terminal",
  wrapper: "Editor",
};

export const TOOL_KIND_ICON: Record<ToolKind, string> = {
  read: "📖",
  edit: "✏️",
  delete: "🗑️",
  move: "📦",
  search: "🔎",
  execute: "⚡",
  think: "💭",
  fetch: "🌐",
  switch_mode: "🔀",
  other: "🦴",
};

export const TOOL_KINDS: ToolKind[] = ["read", "edit", "delete", "move", "search", "execute", "think", "fetch", "switch_mode", "other"];

export function isActive(s: Session): boolean {
  return !s.endedAt && (s.status === "thinking" || s.status === "working" || s.status === "waiting_permission");
}

export function timeAgo(ts: number, now = Date.now()): string {
  const d = Math.max(0, now - ts);
  if (d < 10_000) return "just now";
  if (d < 60_000) return `${Math.floor(d / 1000)}s ago`;
  if (d < 3_600_000) return `${Math.floor(d / 60_000)}m ago`;
  if (d < 86_400_000) return `${Math.floor(d / 3_600_000)}h ago`;
  return `${Math.floor(d / 86_400_000)}d ago`;
}

export function clockTime(ts: number): string {
  return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

/** Last 2 path segments: `C:\code\krypto\apps` → `krypto/apps`. */
export function shortPath(p: string, segments = 2): string {
  const parts = p.split(/[\/]+/).filter(Boolean);
  if (parts.length <= segments) return p;
  return "…/" + parts.slice(-segments).join("/");
}

/** Display name of agent `id` on `machineId` (agents are per machine; ids repeat across machines). */
export function agentName(agents: Agent[], id: string, machineId?: string): string {
  return agentFor(agents, id, machineId)?.name ?? id;
}

export function sessionTitle(s: Session, agents: Agent[]): string {
  return s.title?.trim() || `${agentName(agents, s.agentId, s.machineId)} · ${shortPath(s.cwd || "~")}`;
}

export function permissionSummary(req: PermissionRequest): string {
  const t = req.toolCall;
  if (t.title) return t.title;
  const loc = t.locations?.[0]?.path;
  return `${t.kind ?? "tool"}${loc ? ` ${shortPath(loc)}` : ""}`;
}

export function errorText(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}
