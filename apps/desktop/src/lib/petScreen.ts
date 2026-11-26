// What the desktop pet window shows (pure, no DOM): ONE pet, the default pet, that orchestrates every agent and
// mirrors the most important state across all sessions and machines. Sessions (this machine's and paired machines')
// never get pets of their own on screen: the default pet's bubble switches between them, one at a time.
import { checkPetName, deviceNameProblem, isLocal, machineDisplayName, PET_NAME_MAX, petFor, type HubState, type PetNameCheck } from "@pawl/hub-client";
import { activityForStatus, type PetActivity, type PetView } from "@pawl/pet-engine";
import type { PermissionRequest, Pet, Session, SessionStatus } from "@pawl/protocol";
import { agentName } from "./format";

/** Most important first: needs you > working > just finished / hit a snag > idle > napping. */
export type DefaultPetMood = "needs_you" | "working" | "done" | "oops" | "idle" | "napping";

export interface DefaultPetState {
  mood: DefaultPetMood;
  activity: PetActivity;
  /** Approvals that can be answered from here (this machine's, or an online paired machine's), oldest first. */
  pending: PermissionRequest[];
  /** Hook-tracked sessions waiting for the user in their terminal / editor (no request to answer here). */
  waiting: Session[];
  /** Running sessions (thinking / working), most recently active first. */
  busy: Session[];
  /** One short line for the bubble header and the tooltip. */
  line: string;
}

type ScreenState = Pick<HubState, "sessions" | "pending" | "machines" | "machine" | "agents" | "pets">;

const RUNNING: ReadonlySet<SessionStatus> = new Set<SessionStatus>(["thinking", "working"]);
const REFINED: ReadonlySet<PetActivity> = new Set<PetActivity>(["thinking", "working", "planning"]);

/** This machine, or a paired machine that is online (an offline machine's items cannot be acted on). */
export function reachable(state: Pick<HubState, "machine" | "machines">, machineId: string): boolean {
  return isLocal(state, machineId) || state.machines[machineId]?.online === true;
}

function live(state: ScreenState, s: Session): boolean {
  return s.endedAt == null && reachable(state, s.machineId);
}

function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** "api" for "E:/work/api/" (the repo or folder a session runs in). */
export function repoName(cwd: string): string {
  // An outside A2A agent's session works where the agent runs: its "folder" is the agent's URL; show its host.
  const url = /^[a-z][a-z0-9+.-]*:\/\/([^/?#]+)/i.exec(cwd);
  if (url) return url[1]!;
  const parts = cwd.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || cwd || "~";
}

function where(state: ScreenState, s: Session): string {
  return `${repoName(s.cwd)}${isLocal(state, s.machineId) ? "" : ` on ${machineDisplayName(state.machines[s.machineId])}`}`;
}

/** The activity a running session's pet shows: its pet view's refined pose (planning…) when it has one. */
function runningActivity(s: Session, views: readonly PetView[]): PetActivity {
  const v = views.find((x) => x.sessionId === s.id);
  return v && REFINED.has(v.activity) ? v.activity : activityForStatus(s.status);
}

/**
 * The default pet's state, across all sessions and machines: needs you (any approval, local or remote, or a
 * session waiting in its terminal) > working (any running session) > just finished / hit a snag (a pet's recent
 * happy / sad) > idle > napping. Offline machines are left out.
 */
export function defaultPetState(state: ScreenState, views: readonly PetView[]): DefaultPetState {
  const pending = Object.values(state.pending)
    .filter((p) => reachable(state, p.machineId))
    .sort((a, b) => a.createdAt - b.createdAt || a.requestId.localeCompare(b.requestId));
  const asked = new Set(pending.map((p) => p.sessionId));
  const sessions = Object.values(state.sessions).filter((s) => live(state, s));
  const waiting = sessions.filter((s) => s.status === "waiting_permission" && !asked.has(s.id)).sort((a, b) => a.lastEventAt - b.lastEventAt);
  const busy = sessions.filter((s) => RUNNING.has(s.status)).sort((a, b) => b.lastEventAt - a.lastEventAt);
  const base = { pending, waiting, busy };

  if (pending.length > 0 || waiting.length > 0) {
    let line: string;
    if (pending.length > 0) line = `${plural(pending.length, "approval")} waiting for you`;
    else {
      const s = waiting[0]!;
      const more = waiting.length > 1 ? ` (+${waiting.length - 1} more)` : "";
      line = `${agentName(state.agents, s.agentId, s.machineId)} is waiting for you in ${where(state, s)}${more}`;
    }
    return { ...base, mood: "needs_you", activity: "alert", line };
  }
  if (busy.length > 0) {
    const s = busy[0]!;
    const line =
      busy.length === 1
        ? `${agentName(state.agents, s.agentId, s.machineId)} is working in ${where(state, s)}`
        : `Watching ${plural(busy.length, "busy session")}`;
    return { ...base, mood: "working", activity: runningActivity(s, views), line };
  }
  const shown = views.filter((v) => !v.remoteOffline && !v.away);
  if (shown.some((v) => v.activity === "sad")) return { ...base, mood: "oops", activity: "sad", line: "Something hit a snag. Ask me what happened." };
  if (shown.some((v) => v.activity === "happy")) return { ...base, mood: "done", activity: "happy", line: "All done!" };
  if (shown.some((v) => v.activity === "idle")) return { ...base, mood: "idle", activity: "idle", line: "All quiet. Tell me what to do next." };
  return { ...base, mood: "napping", activity: "napping", line: "Napping. Click me to chat." };
}

/** One session the default pet's bubble can switch to (its chip in the switcher). */
export interface SessionChip {
  sessionId: string;
  machineId: string;
  /** The session's agent's pet name (else the agent's name): sessions are individuals. */
  name: string;
  repo: string;
  /** "Rex · api" (+ " · Mac mini" for another machine's session). */
  label: string;
  /** An approval to answer here, or the session waits for the user in its terminal. */
  needsYou: boolean;
  /** This session's answerable approvals, oldest first. */
  pending: PermissionRequest[];
  running: boolean;
  remote: boolean;
  lastEventAt: number;
  /** A browser task (the pet drives the user's Chrome). */
  browser: boolean;
  /** The default pet's own agent session (Assistant mode): named after the pet. */
  assistant: boolean;
}

/** At most this many session chips in the bubble's switcher; the rest go under "more". */
export const MAX_SWITCHER_CHIPS = 5;
/** A session that is not running and needs nothing still shows this long after its last event. */
export const RECENT_SESSION_MS = 30 * 60_000;

function chipOrder(a: SessionChip, b: SessionChip): number {
  return Number(b.needsYou) - Number(a.needsYou) || Number(b.running) - Number(a.running) || b.lastEventAt - a.lastEventAt || a.sessionId.localeCompare(b.sessionId);
}

/**
 * The sessions the default pet's bubble can talk to, one at a time: every live session (not ended, its machine
 * reachable; this machine's and paired machines') that needs you, is running, or was active in the last
 * `RECENT_SESSION_MS`. Needs-you first, then running, then most recent. The first `max` are chips, the rest "more";
 * the focused session is always a chip (it takes the last place when it would be under "more").
 */
export function switcherSessions(
  state: ScreenState,
  now: number,
  opts: { focus?: string | null; max?: number; petName?: string | undefined } = {},
): { chips: SessionChip[]; more: SessionChip[] } {
  const max = Math.max(1, opts.max ?? MAX_SWITCHER_CHIPS);
  const pendingBy = new Map<string, PermissionRequest[]>();
  for (const p of Object.values(state.pending)) {
    if (!reachable(state, p.machineId)) continue;
    const list = pendingBy.get(p.sessionId) ?? [];
    list.push(p);
    pendingBy.set(p.sessionId, list);
  }
  const all = Object.values(state.sessions)
    .filter((s) => live(state, s))
    .filter((s) => RUNNING.has(s.status) || s.status === "waiting_permission" || pendingBy.has(s.id) || now - s.lastEventAt < RECENT_SESSION_MS || s.id === opts.focus)
    .map((s): SessionChip => {
      const pet = petFor(Object.values(state.pets), s.machineId, s.agentId);
      // The assistant's session is the default pet itself at work: its name, not the agent's pet's.
      const assistant = s.purpose === "assistant";
      const name = (assistant ? opts.petName : undefined) ?? pet?.name ?? agentName(state.agents, s.agentId, s.machineId);
      const remote = !isLocal(state, s.machineId);
      const repo = repoName(s.cwd);
      const pending = (pendingBy.get(s.id) ?? []).sort((a, b) => a.createdAt - b.createdAt);
      return {
        sessionId: s.id,
        machineId: s.machineId,
        name,
        repo,
        label: `${name} · ${repo}${remote ? ` · ${machineDisplayName(state.machines[s.machineId])}` : ""}`,
        needsYou: pending.length > 0 || s.status === "waiting_permission",
        pending,
        running: RUNNING.has(s.status),
        remote,
        lastEventAt: s.lastEventAt,
        browser: s.purpose === "browser",
        assistant,
      };
    })
    .sort(chipOrder);
  const chips = all.slice(0, max);
  const more = all.slice(max);
  const i = opts.focus ? more.findIndex((c) => c.sessionId === opts.focus) : -1;
  if (i >= 0) {
    const [focused] = more.splice(i, 1);
    const bumped = chips.pop()!;
    chips.push(focused!);
    more.unshift(bumped);
    more.sort(chipOrder);
  }
  return { chips, more };
}

/** Request ids that just appeared (for opening the bubble by itself when an approval arrives). */
export function newRequestIds(before: ReadonlySet<string>, now: readonly PermissionRequest[]): string[] {
  return now.filter((p) => !before.has(p.requestId)).map((p) => p.requestId);
}

/** Stands in for the default pet in name checks (it is local to this app, not a hub pet). */
export const DEFAULT_PET_ID = "pawl-desktop:default-pet";

/** The default pet's name: like every pet name (1–40 characters, not another pet's), and no invisible characters. */
export function checkDefaultPetName(raw: string, pets: Iterable<Pet>): PetNameCheck {
  if (deviceNameProblem(raw, PET_NAME_MAX) === "bad_char") {
    return { ok: false, code: "invalid", error: "Remove control or invisible characters (such as tabs or zero-width spaces)." };
  }
  return checkPetName(raw, DEFAULT_PET_ID, pets);
}
