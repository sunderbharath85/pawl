// A2A in the desktop UI (pure): outside A2A agents in New session (they work where they run, not in a folder), and
// the A2A task chip in the session view (useful for outside agents' sessions and for sessions an outside caller or a
// paired machine started; for your own ACP sessions it would only repeat the status).
import { A2A_TASK_STATE_LABEL, a2aCallerLabel, a2aTaskTone, isA2aAgentId, type A2aTaskTone } from "@pawl/hub-client";
import type { A2aSettings, A2aTaskSummary, Agent, Session } from "@pawl/protocol";
import { A2A_AGENT_PREFIX } from "@pawl/protocol";

/** An outside A2A agent (no folder, no command: it runs at its URL). */
export function isOutsideAgent(agent: Pick<Agent, "id" | "source"> | undefined | null): boolean {
  return !!agent && (agent.source === "a2a" || isA2aAgentId(agent.id));
}

/** The URL this machine's outside agent was added by (null when unknown, e.g. another machine's agent). */
export function outsideAgentUrl(agentId: string, a2a: A2aSettings | null | undefined): string | null {
  if (!agentId.startsWith(A2A_AGENT_PREFIX)) return null;
  const slug = agentId.slice(A2A_AGENT_PREFIX.length);
  return a2a?.agents?.find((a) => a.id === slug)?.url ?? null;
}

/**
 * The "folder" New session sends: the folder typed for an ACP agent; for an outside agent (which works where it runs,
 * so the hub ignores the folder) its URL, else its id, so the request is never empty.
 */
export function startCwd(agent: Pick<Agent, "id" | "source">, typed: string, a2a: A2aSettings | null | undefined): string {
  if (!isOutsideAgent(agent)) return typed.trim();
  return outsideAgentUrl(agent.id, a2a) ?? agent.id;
}

export interface A2aTaskChip {
  label: string;
  tone: A2aTaskTone;
  title: string;
}

/**
 * The chip for a session's latest A2A task, or null when it would add nothing (your own session of an ACP agent:
 * the status pill says it already).
 */
export function a2aTaskChip(
  tasks: readonly A2aTaskSummary[],
  session: Pick<Session, "id" | "agentId">,
  a2a: A2aSettings | null | undefined,
  machineName?: (id: string) => string | undefined,
): A2aTaskChip | null {
  const own = tasks.filter((t) => t.sessionId === session.id);
  if (own.length === 0) return null;
  const latest = own.reduce((a, b) => ((b.updatedAt ?? 0) > (a.updatedAt ?? 0) ? b : a));
  const outside = isA2aAgentId(session.agentId);
  if (!outside && latest.caller === "local") return null;
  const state = A2A_TASK_STATE_LABEL[latest.state];
  const by = latest.caller === "local" ? null : a2aCallerLabel(latest.caller, a2a ?? null, machineName);
  return {
    label: by ? `A2A · ${by} · ${state}` : `A2A task · ${state}`,
    tone: a2aTaskTone(latest.state),
    title: `${by ? `Started by ${by} over A2A. ` : ""}Latest task: ${state}${latest.text ? ` (${latest.text})` : ""}.`,
  };
}
