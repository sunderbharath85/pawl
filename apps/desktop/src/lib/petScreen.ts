// What the desktop pet window shows (pure, no DOM): the default pet that orchestrates every agent and mirrors the
// most important state across all sessions and machines, plus (in "sessions" mode) one pet per active session.
import { checkPetName, deviceNameProblem, isLocal, machineDisplayName, PET_NAME_MAX, petFor, type HubState, type PetNameCheck } from "@pawl/hub-client";
import { activityForStatus, type PetActivity, type PetView } from "@pawl/pet-engine";
import type { PermissionRequest, Pet, Session, SessionStatus, Species } from "@pawl/protocol";
import { agentName } from "./format";

/** "Pets on screen": the default pet only, or the default pet plus one pet per active session. */
export type PetsOnScreen = "default" | "sessions";

export const PETS_ON_SCREEN_LABEL: Record<PetsOnScreen, string> = {
  default: "Default pet only",
  sessions: "Default pet + one per active session",
};

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

export interface SessionPet {
  sessionId: string;
  machineId: string;
  species: Species;
  skin?: string | undefined;
  /** The session's agent's pet name (else the agent's name). */
  name: string;
  /** Name tag: "Rex · api" (+ " · Mac mini" for another machine's session). */
  label: string;
  activity: PetActivity;
  /** This session's answerable approvals, oldest first. */
  pending: PermissionRequest[];
  remote: boolean;
}

/** At most this many session pets line up beside the default pet; the rest become a "+N" chip. */
export const MAX_SESSION_PETS = 6;

/**
 * One pet per active session (running, or needing you), never for idle agents with no session. Oldest session
 * first so pets keep their places; offline machines are left out.
 */
export function activeSessionPets(
  state: ScreenState,
  views: readonly PetView[],
  max = MAX_SESSION_PETS,
): { pets: SessionPet[]; hidden: number } {
  const pendingBy = new Map<string, PermissionRequest[]>();
  for (const p of Object.values(state.pending)) {
    if (!reachable(state, p.machineId)) continue;
    const list = pendingBy.get(p.sessionId) ?? [];
    list.push(p);
    pendingBy.set(p.sessionId, list);
  }
  const active = Object.values(state.sessions)
    .filter((s) => live(state, s) && (RUNNING.has(s.status) || s.status === "waiting_permission" || pendingBy.has(s.id)))
    .sort((a, b) => a.startedAt - b.startedAt || a.id.localeCompare(b.id));
  const pets = active.slice(0, Math.max(0, max)).map((s): SessionPet => {
    const pet = petFor(Object.values(state.pets), s.machineId, s.agentId);
    const name = pet?.name ?? agentName(state.agents, s.agentId, s.machineId);
    const remote = !isLocal(state, s.machineId);
    const pending = (pendingBy.get(s.id) ?? []).sort((a, b) => a.createdAt - b.createdAt);
    const label = `${name} · ${repoName(s.cwd)}${remote ? ` · ${machineDisplayName(state.machines[s.machineId])}` : ""}`;
    const activity = pending.length > 0 || s.status === "waiting_permission" ? "alert" : runningActivity(s, views);
    return { sessionId: s.id, machineId: s.machineId, species: pet?.species ?? "dog", skin: pet?.skin, name, label, activity, pending, remote };
  });
  return { pets, hidden: Math.max(0, active.length - pets.length) };
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
