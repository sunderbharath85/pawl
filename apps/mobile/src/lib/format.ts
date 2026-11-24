import type { Agent, Session } from "@krypto/protocol";

/** "now", "42s", "5m", "3h", "2d", or a short date. */
export function timeAgo(ts: number | undefined, now: number = Date.now()): string {
  if (!ts) return "";
  const s = Math.max(0, Math.round((now - ts) / 1000));
  if (s < 10) return "now";
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h`;
  const d = Math.floor(h / 24);
  if (d < 7) return `${d}d`;
  return new Date(ts).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

export function clockTime(ts: number): string {
  return new Date(ts).toLocaleTimeString(undefined, { hour: "numeric", minute: "2-digit" });
}

/** Last path segment of a cwd (Windows or POSIX). */
export function baseName(path: string): string {
  const parts = path.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

/** Shorten a long path for display: keeps the last two segments. */
export function shortPath(path: string): string {
  const parts = path.replace(/[\\/]+$/, "").split(/[\\/]/).filter(Boolean);
  if (parts.length <= 2) return path;
  return `…/${parts.slice(-2).join("/")}`;
}

export function agentName(agents: readonly Agent[], agentId: string): string {
  return agents.find((a) => a.id === agentId)?.name ?? prettyAgentId(agentId);
}

export function prettyAgentId(id: string): string {
  const known: Record<string, string> = { "claude-code": "Claude Code", codex: "Codex", copilot: "Copilot", gemini: "Gemini CLI" };
  return known[id] ?? id.replace(/[-_]+/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
}

export function sessionTitle(s: Session, agents: readonly Agent[]): string {
  return s.title?.trim() || `${agentName(agents, s.agentId)} · ${baseName(s.cwd)}`;
}

export function sourceLabel(source: Session["source"]): string {
  switch (source) {
    case "owned":
      return "Started by your pet";
    case "vscode_hook":
      return "VS Code";
    case "cli_hook":
      return "Terminal";
    case "wrapper":
      return "Editor";
  }
}

export function isActiveSession(s: Session): boolean {
  return s.endedAt == null;
}

export function isBusy(s: Session): boolean {
  return s.endedAt == null && (s.status === "thinking" || s.status === "working" || s.status === "waiting_permission");
}

/** Unique cwds of known sessions, most recently used first. */
export function knownCwds(sessions: readonly Session[]): string[] {
  const seen = new Map<string, number>();
  for (const s of sessions) {
    if (!s.cwd) continue;
    const t = Math.max(s.lastEventAt ?? 0, s.startedAt ?? 0);
    if ((seen.get(s.cwd) ?? -1) < t) seen.set(s.cwd, t);
  }
  return [...seen.entries()].sort((a, b) => b[1] - a[1]).map(([cwd]) => cwd);
}

export function truncate(s: string, max: number): string {
  const t = s.replace(/\s+/g, " ").trim();
  return t.length > max ? `${t.slice(0, max - 1)}…` : t;
}
