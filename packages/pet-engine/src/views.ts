// Pure derivation of what every pet is doing right now. Time-driven decays use `now` only (no timers).
import type { ErrorPayload, HubEvent, Pet, PermissionRequest, Session, SessionStatus, Species, ToolCallPayload } from "@krypto/protocol";
import { activityForEvent, activityForStatus, HAPPY_DECAY_MS, NAP_AFTER_MS, SAD_DECAY_MS, type PetActivity } from "./activity";
import { hashString, petLine } from "./lines";

export interface PetBubbleData {
  text: string;
  kind: "say" | "permission";
  requestId?: string | undefined;
}

export interface PetView {
  petId: string;
  agentId: string;
  species: Species;
  name: string;
  activity: PetActivity;
  /** When the current activity started (unix ms; 0 = unknown / never active). */
  since: number;
  bubble?: PetBubbleData | undefined;
  /** Session the pet is reacting to, if any. */
  sessionId?: string | undefined;
  mood: number;
  /** Pet skin (from `Pet.skin`), passed through for rendering. */
  skin?: string | undefined;
}

export interface DerivePetViewsInput {
  pets: Pet[];
  sessions: Session[];
  pending: PermissionRequest[];
  /** Latest event per session id (e.g. hub-client `lastEventsBySession` or `lastSignificantEvents`). */
  lastEvents: Record<string, HubEvent | undefined>;
  now: number;
}

const BUSY: ReadonlySet<SessionStatus> = new Set<SessionStatus>(["thinking", "working", "waiting_permission"]);
const REFINABLE: ReadonlySet<PetActivity> = new Set<PetActivity>(["thinking", "working", "planning"]);

const MAX_BUBBLE_DETAIL = 80;

function clip(s: string, max = MAX_BUBBLE_DETAIL): string {
  const t = s.replace(/\s+/g, " ").trim();
  return t.length > max ? `${t.slice(0, max - 1)}…` : t;
}

/** Short human description of what a permission request wants to do. */
export function describePermission(req: Pick<PermissionRequest, "toolCall">): string {
  const tc: Partial<ToolCallPayload> = req.toolCall ?? {};
  if (tc.title) return clip(tc.title);
  const path = tc.locations?.[0]?.path;
  const verb: Record<string, string> = {
    read: "Read",
    edit: "Edit",
    delete: "Delete",
    move: "Move",
    search: "Search",
    execute: "Run a command",
    think: "Think",
    fetch: "Fetch from the web",
    switch_mode: "Switch mode",
    other: "Use a tool",
  };
  const v = verb[tc.kind ?? "other"] ?? "Use a tool";
  return clip(path ? `${v} ${path}` : v);
}

function sourceWhere(s: Session): string {
  switch (s.source) {
    case "vscode_hook":
      return " in VS Code";
    case "cli_hook":
      return " in the terminal";
    default:
      return "";
  }
}

function errorDetail(ev: HubEvent | undefined): string | null {
  if (!ev || ev.type !== "error") return null;
  const msg = (ev.payload as Partial<ErrorPayload> | null)?.message;
  return typeof msg === "string" && msg.trim() ? clip(msg, 60) : null;
}

function activityTs(s: Session, ev: HubEvent | undefined): number {
  return Math.max(s.lastEventAt ?? 0, s.endedAt ?? 0, ev?.ts ?? 0);
}

interface Derived {
  view: PetView;
  /** Earliest future time at which the view changes purely because time passes (null = never). */
  nextAt: number | null;
}

function derivePet(pet: Pet, input: DerivePetViewsInput, seedBase: number): Derived {
  const { now } = input;
  const base = {
    petId: pet.id,
    agentId: pet.agentId,
    species: pet.species,
    name: pet.name,
    mood: pet.mood,
    skin: pet.skin,
  };
  const seed = (t: number) => seedBase + Math.floor(t / 1000);

  const agentSessions = input.sessions.filter((s) => s.agentId === pet.agentId);
  const sessionIds = new Set(agentSessions.map((s) => s.id));

  // 1. Pending permission → alert with an Approve/Deny bubble (oldest first).
  const myPending = input.pending
    .filter((p) => p.agentId === pet.agentId || sessionIds.has(p.sessionId))
    .sort((a, b) => a.createdAt - b.createdAt);
  const req = myPending[0];
  if (req) {
    const more = myPending.length > 1 ? ` (+${myPending.length - 1} more)` : "";
    return {
      view: {
        ...base,
        activity: "alert",
        since: req.createdAt,
        sessionId: req.sessionId,
        bubble: {
          kind: "permission",
          requestId: req.requestId,
          text: `${petLine(pet.species, "alert", seed(req.createdAt))} ${describePermission(req)}?${more}`,
        },
      },
      nextAt: null,
    };
  }

  // 2. A busy session → thinking / working / planning / alert (hook-tracked waits).
  const busy = agentSessions
    .filter((s) => s.endedAt == null && BUSY.has(s.status))
    .sort((a, b) => activityTs(b, input.lastEvents[b.id]) - activityTs(a, input.lastEvents[a.id]));
  const focus = busy[0];
  if (focus) {
    const ev = input.lastEvents[focus.id];
    const evAct = ev ? activityForEvent(ev) : null;
    const since = ev?.ts ?? focus.lastEventAt;
    if (focus.status === "waiting_permission") {
      return {
        view: {
          ...base,
          activity: "alert",
          since: focus.lastEventAt,
          sessionId: focus.id,
          bubble: { kind: "say", text: `${petLine(pet.species, "alert", seed(focus.lastEventAt))} Waiting for you${sourceWhere(focus)}.` },
        },
        nextAt: null,
      };
    }
    const activity = evAct && REFINABLE.has(evAct) ? evAct : activityForStatus(focus.status);
    return { view: { ...base, activity, since, sessionId: focus.id }, nextAt: null };
  }

  // 3. Nothing busy → recent end-of-turn happy / error sad, decaying to idle, then napping.
  let latest: Session | undefined;
  let latestTs = -Infinity;
  for (const s of agentSessions) {
    const t = activityTs(s, input.lastEvents[s.id]);
    if (t > latestTs) {
      latest = s;
      latestTs = t;
    }
  }
  if (!latest) return { view: { ...base, activity: "napping", since: 0 }, nextAt: null };

  const ev = input.lastEvents[latest.id];
  const evAct = ev ? activityForEvent(ev) : null;
  let terminal: "happy" | "sad" | null;
  if (latest.status === "error") terminal = "sad";
  else if (latest.status === "done") terminal = evAct === "sad" ? "sad" : "happy";
  else terminal = evAct === "happy" || evAct === "sad" ? evAct : null;

  const t0 = ev && evAct === terminal ? ev.ts : latest.lastEventAt;
  const napAt = latestTs + NAP_AFTER_MS;
  const sessionId = latest.endedAt == null ? latest.id : undefined;

  if (terminal) {
    const decayAt = t0 + (terminal === "happy" ? HAPPY_DECAY_MS : SAD_DECAY_MS);
    if (now < decayAt) {
      const detail = terminal === "sad" ? errorDetail(ev) : null;
      const line = petLine(pet.species, terminal, seed(t0));
      return {
        view: {
          ...base,
          activity: terminal,
          since: t0,
          sessionId: latest.id,
          bubble: { kind: "say", text: detail ? `${line} ${detail}` : line },
        },
        nextAt: decayAt,
      };
    }
    if (now < napAt) return { view: { ...base, activity: "idle", since: decayAt, sessionId }, nextAt: napAt };
    return { view: { ...base, activity: "napping", since: Math.max(napAt, decayAt) }, nextAt: null };
  }

  if (now < napAt) return { view: { ...base, activity: "idle", since: latestTs, sessionId }, nextAt: napAt };
  return { view: { ...base, activity: "napping", since: napAt }, nextAt: null };
}

/**
 * Pure: what each pet is doing at `now`. One view per pet, in input order.
 * Priority: pending permission (alert + Approve/Deny) > busy session (thinking/working/planning)
 * > recent end_turn (happy, 6 s) / error (sad, 10 s) > idle > napping (60 s without activity).
 */
export function derivePetViews(input: DerivePetViewsInput): PetView[] {
  return input.pets.map((pet) => derivePet(pet, input, hashString(pet.id)).view);
}

/**
 * Earliest time > `now` at which `derivePetViews` would return something different without any new
 * hub data (a decay or nap timer firing), or null. Use it to schedule a single re-render timer.
 */
export function nextPetViewChange(input: DerivePetViewsInput): number | null {
  let next: number | null = null;
  for (const pet of input.pets) {
    const { nextAt } = derivePet(pet, input, 0);
    if (nextAt != null && nextAt > input.now && (next == null || nextAt < next)) next = nextAt;
  }
  return next;
}
