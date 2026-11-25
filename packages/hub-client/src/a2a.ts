// Settings > A2A (PLAN.md "A2A orchestration"): Pawl as an A2A agent on 127.0.0.1 for programs on this PC (callers,
// each with a write-only bearer token), and outside A2A agents Pawl delegates to. Pure helpers for the desktop form:
// patches (lists replace; tokens are write-only), validation, labels and the risky changes that need a confirm.
import type { A2aAgentSetting, A2aCallerSetting, A2aSettings, A2aTaskSummary, HubSettings, HubSettingsPatch } from "@pawl/protocol";
import { A2A_AGENT_PREFIX } from "@pawl/protocol";
import type { RiskyChange } from "./settings";

/** Caller and agent ids: `[a-z0-9-]{1,40}` (the hub's rule). */
export const A2A_ID_RE = /^[a-z0-9-]{1,40}$/;
/** At most this many outside agents (the hub refuses more). */
export const A2A_AGENTS_MAX = 16;
export const A2A_NAME_MAX = 80;
export const A2A_TOKEN_MAX = 512;

/** The A2A section of a hub's settings, or null for a hub that does not speak A2A. */
export function a2aOf(s: HubSettings | undefined | null): A2aSettings | null {
  return s?.a2a ?? null;
}

/** A free id for `name`: "Claude Desktop" -> "claude-desktop" (then "-2", "-3", … when taken). */
export function suggestA2aId(name: string, taken: Iterable<string>, fallback = "caller"): string {
  const used = new Set(taken);
  const base =
    name
      .toLowerCase()
      .normalize("NFKD")
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-+|-+$/g, "")
      .slice(0, 34)
      .replace(/-+$/, "") || fallback;
  if (!used.has(base)) return base;
  for (let i = 2; ; i++) {
    const id = `${base}-${i}`;
    if (!used.has(id)) return id;
  }
}

/** A caller's or agent's display name: 1-80 characters after trimming. */
export function checkA2aName(raw: string): string | null {
  const t = raw.trim();
  if (!t) return "Give it a name (for example the program it is).";
  if (t.length > A2A_NAME_MAX) return `At most ${A2A_NAME_MAX} characters.`;
  return null;
}

const LOOPBACK_HOSTS = new Set(["127.0.0.1", "localhost", "[::1]"]);

/**
 * An outside agent's URL, as the hub accepts it: absolute https, or http only to this machine (127.0.0.1,
 * localhost, [::1]); no user name or password in it.
 */
export function checkA2aAgentUrl(raw: string): string | null {
  const t = raw.trim();
  if (!t) return "Paste the agent's URL (its A2A endpoint or its agent card).";
  let u: URL;
  try {
    u = new URL(t);
  } catch {
    return "That is not a full URL. It starts with https://";
  }
  if (u.username || u.password) return "Leave the user name and password out of the URL; add a token instead.";
  if (u.protocol === "https:") return null;
  if (u.protocol === "http:") return LOOPBACK_HOSTS.has(u.hostname) ? null : "Use https:// (plain http is only allowed to this machine).";
  return "Use an https:// URL.";
}

/** True when the agent runs on this PC (prompts stay on it). */
export function isLoopbackUrl(raw: string): boolean {
  try {
    return LOOPBACK_HOSTS.has(new URL(raw.trim()).hostname);
  } catch {
    return false;
  }
}

/** "agents.example.com" from a URL (for labels), or the text itself when it is not one. */
export function urlHost(raw: string): string {
  try {
    return new URL(raw.trim()).host || raw;
  } catch {
    return raw;
  }
}

/** A bearer token for an outside agent: 1-512 visible ASCII characters. */
export function checkA2aToken(raw: string): string | null {
  const t = raw.trim();
  if (!t) return "Paste the token first.";
  if (t.length > A2A_TOKEN_MAX) return `A token is at most ${A2A_TOKEN_MAX} characters.`;
  if (!/^[\x21-\x7e]+$/.test(t)) return "A token has no spaces or special characters.";
  return null;
}

// ---------------------------------------------------------------- patches

export function a2aServerPatch(base: A2aSettings, next: { server?: boolean; port?: number }): HubSettingsPatch {
  const a2a: NonNullable<HubSettingsPatch["a2a"]> = {};
  if (next.server !== undefined && next.server !== base.server) a2a.server = next.server;
  if (next.port !== undefined && next.port !== base.port) a2a.port = next.port;
  return Object.keys(a2a).length ? { a2a } : {};
}

function callerList(list: readonly A2aCallerSetting[]): NonNullable<NonNullable<HubSettingsPatch["a2a"]>["callers"]> {
  return list.map((c) => ({ id: c.id, name: c.name, allowApprovals: c.allowApprovals }));
}

/** Add a caller (no token yet: the hub makes one with `a2a_rotate_token`). The list replaces the hub's. */
export function addCallerPatch(base: A2aSettings, name: string): { patch: HubSettingsPatch; id: string } {
  const id = suggestA2aId(name, base.callers.map((c) => c.id));
  return { patch: { a2a: { callers: [...callerList(base.callers), { id, name: name.trim(), allowApprovals: false }] } }, id };
}

export function removeCallerPatch(base: A2aSettings, id: string): HubSettingsPatch {
  return { a2a: { callers: callerList(base.callers.filter((c) => c.id !== id)) } };
}

export function callerApprovalsPatch(base: A2aSettings, id: string, allow: boolean): HubSettingsPatch {
  return { a2a: { callers: callerList(base.callers).map((c) => (c.id === id ? { ...c, allowApprovals: allow } : c)) } };
}

type AgentEntry = NonNullable<NonNullable<HubSettingsPatch["a2a"]>["agents"]>[number];

/** The agents as a patch list: tokens left out (absent keeps each agent's token while its id stays). */
function agentList(list: readonly A2aAgentSetting[]): AgentEntry[] {
  return list.map((a) => ({ id: a.id, name: a.name, url: a.url }));
}

export function addAgentPatch(base: A2aSettings, input: { url: string; name?: string; token?: string }): { patch: HubSettingsPatch; id: string } {
  const agents = base.agents ?? [];
  const url = input.url.trim();
  const name = (input.name ?? "").trim();
  const id = suggestA2aId(name || urlHost(url).replace(/:\d+$/, ""), agents.map((a) => a.id), "agent");
  const entry: AgentEntry = { id, url };
  if (name) entry.name = name;
  const token = (input.token ?? "").trim();
  if (token) entry.token = token;
  return { patch: { a2a: { agents: [...agentList(agents), entry] } }, id };
}

export function removeAgentPatch(base: A2aSettings, id: string): HubSettingsPatch {
  return { a2a: { agents: agentList((base.agents ?? []).filter((a) => a.id !== id)) } };
}

/** Set (a string) or clear (null) one agent's token; the others keep theirs. */
export function agentTokenPatch(base: A2aSettings, id: string, token: string | null): HubSettingsPatch {
  return { a2a: { agents: agentList(base.agents ?? []).map((a) => (a.id === id ? { ...a, token: token === null ? null : token.trim() } : a)) } };
}

// ---------------------------------------------------------------- labels

/** The server's live state for people. */
export function a2aServerStatus(a: A2aSettings): { tone: "on" | "off" | "error"; text: string } {
  if (a.error) return { tone: "error", text: a.server ? `Not running: ${a.error}` : a.error };
  if (a.running && a.url) return { tone: "on", text: `Running at ${a.url} (this PC only)` };
  if (a.server) return { tone: "off", text: "Starting…" };
  return { tone: "off", text: "Off" };
}

/** The public agent card of the running server (what callers point at). */
export function a2aCardUrl(serverUrl: string): string {
  return `${serverUrl.replace(/\/+$/, "")}/.well-known/agent-card.json`;
}

export const A2A_TASK_STATE_LABEL: Record<A2aTaskSummary["state"], string> = {
  submitted: "Queued",
  working: "Working",
  input_required: "Needs input",
  completed: "Done",
  canceled: "Cancelled",
  rejected: "Refused",
  failed: "Failed",
  auth_required: "Needs sign-in",
  unspecified: "Unknown",
};

export type A2aTaskTone = "work" | "ask" | "done" | "bad" | "idle";

export function a2aTaskTone(state: A2aTaskSummary["state"]): A2aTaskTone {
  switch (state) {
    case "submitted":
    case "working":
      return "work";
    case "input_required":
    case "auth_required":
      return "ask";
    case "completed":
      return "done";
    case "failed":
    case "rejected":
      return "bad";
    default:
      return "idle";
  }
}

/** True for an outside A2A agent's id (`a2a:<slug>`). */
export function isA2aAgentId(agentId: string): boolean {
  return agentId.startsWith(A2A_AGENT_PREFIX);
}

/** Who started a task, for people: "you", "Mac mini" (a paired machine), "Claude Desktop" (an A2A caller). */
export function a2aCallerLabel(caller: string, a: A2aSettings | null, machineName?: (id: string) => string | undefined): string {
  if (caller === "local") return "you";
  if (caller.startsWith("peer:")) return machineName?.(caller.slice(5)) ?? "a paired machine";
  if (caller.startsWith("a2a:")) {
    const id = caller.slice(4);
    return a?.callers.find((c) => c.id === id)?.name ?? id;
  }
  return caller;
}

// ---------------------------------------------------------------- risky changes

/**
 * A2A changes that need an in-app confirm: turning the localhost server on, letting a caller answer approvals, and
 * adding an outside agent that is not on this PC (prompts then leave it).
 */
export function a2aRiskyChanges(base: HubSettings, patch: HubSettingsPatch): RiskyChange[] {
  const p = patch.a2a;
  const cur = base.a2a;
  if (!p || !cur) return [];
  const out: RiskyChange[] = [];
  if (p.server === true && !cur.server) {
    const port = p.port ?? cur.port;
    out.push({
      key: "a2a.server",
      title: "Let other tools talk to Pawl?",
      body:
        `Programs on this PC that have a token can ask Pawl what your agents are doing and ask it to start work in folders Pawl already knows. ` +
        `They cannot approve anything unless you allow it for that caller. The A2A server listens on 127.0.0.1:${port} only: nothing on your network can reach it. ` +
        "A caller needs a token you create here, and you can turn the server off at any time.",
      confirm: "Turn on A2A server",
    });
  }
  for (const c of p.callers ?? []) {
    if (!c.allowApprovals) continue;
    const before = cur.callers.find((x) => x.id === c.id);
    if (before?.allowApprovals) continue;
    out.push({
      key: `a2a.callers.${c.id}.allowApprovals`,
      title: `Let “${c.name}” answer approvals?`,
      body:
        `“${c.name}” could allow once or deny the permission requests of the sessions it started over A2A, such as reading or editing files. ` +
        "Never “always allow”, and never running commands, deleting files or network access: those always wait for you in a Pawl app. Every answer is in the audit log.",
      confirm: "Allow approvals",
    });
  }
  for (const a of p.agents ?? []) {
    if ((cur.agents ?? []).some((x) => x.id === a.id && x.url === a.url)) continue;
    if (isLoopbackUrl(a.url)) continue;
    out.push({
      key: `a2a.agents.${a.id}`,
      title: `Add the outside agent at ${urlHost(a.url)}?`,
      body:
        `The prompts you give it go to ${urlHost(a.url)}, and its answers come back as a session here. ` +
        "It does not get your files: it works where it runs. Its questions show up as approvals and are never answered for you.",
      confirm: "Add agent",
    });
  }
  return out;
}
