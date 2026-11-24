// Per-hub read-only mirror (PLAN.md step 3, as a phone sees it). Pure. Every item must carry the sending hub's
// machine id (and ids with its prefix); anything else is dropped, so a hub can never write another machine's rows.
// Live updates reuse @pawl/hub-client's reducer: MirrorUpdate variants are ServerMsg variants.
import {
  DEFAULT_LIMITS,
  initialHubState,
  mergeEvents,
  reduceServerMsg,
  type HubState,
} from "@pawl/hub-client";
import {
  parseGlobalId,
  type HubEvent,
  type HubNotification,
  type MachineId,
  type MirrorUpdate,
  type PeerBody,
  type PermissionOutcome,
  type PermissionRequest,
  type Pet,
  type ResolvedBy,
  type ServerMsg,
  type Session,
  type UnixMs,
} from "@pawl/protocol";

/** Mirror caps per hub (the phone keeps less than a hub does). */
export const MIRROR_MAX_SESSIONS = 500;
export const MIRROR_EVENTS_PER_SESSION = DEFAULT_LIMITS.eventCap;
export const MIRROR_MAX_RESOLUTIONS = 100;
/** Distinct sessions with events kept per hub (the ones with the oldest last event go first). */
export const MIRROR_MAX_EVENT_SESSIONS = 500;
/** Pending requests kept per hub (oldest first out), same as the hubs' own mirror cap. */
export const MIRROR_MAX_PENDING = 500;
/** Pets kept per hub (earliest received first out), same as the hubs' own mirror cap. */
export const MIRROR_MAX_PETS = 200;

/** How a request was decided, for "Approved from <device>". */
export interface Resolution {
  requestId: string;
  sessionId: string;
  machineId: MachineId;
  outcome: PermissionOutcome;
  by: ResolvedBy;
  /** The device the decision came from when it was not the owner (another hub or a phone). */
  byMachineId?: string;
  /** "approve" / "deny" when the chosen option's kind is known, else "cancel" or "answer". */
  verdict: "approve" | "deny" | "cancel" | "answer";
  /** The request as it was pending (if this mirror held it). */
  request?: PermissionRequest;
  at: UnixMs;
}

export interface HubMirror {
  machineId: MachineId;
  /** hub-client state: sessions, pets, pending, events, notifications (other fields unused). */
  state: HubState;
  resolutions: Resolution[];
  /** Highest event seq held from this hub (the next sync_request cursor). */
  maxSeq: number;
  sync: "idle" | "syncing" | "synced";
  /** The last sync skipped older events. */
  truncated: boolean;
  /** Bumped on every change (cheap change detection for snapshots). */
  version: number;
}

export function emptyMirror(machineId: MachineId): HubMirror {
  return { machineId, state: initialHubState(), resolutions: [], maxSeq: 0, sync: "idle", truncated: false, version: 0 };
}

function ownedBy(id: unknown, machineId: MachineId): boolean {
  return typeof id === "string" && parseGlobalId(id)?.machineId === machineId;
}

export function sessionOk(s: Session, m: MachineId): boolean {
  return typeof s === "object" && s !== null && s.machineId === m && ownedBy(s.id, m) && typeof s.status === "string";
}

export function petOk(p: Pet, m: MachineId): boolean {
  return typeof p === "object" && p !== null && p.machineId === m && ownedBy(p.id, m) && typeof p.name === "string";
}

export function requestOk(r: PermissionRequest, m: MachineId): boolean {
  return (
    typeof r === "object" &&
    r !== null &&
    r.machineId === m &&
    ownedBy(r.requestId, m) &&
    ownedBy(r.sessionId, m) &&
    Array.isArray(r.options)
  );
}

export function eventOk(e: HubEvent, m: MachineId): boolean {
  return (
    typeof e === "object" &&
    e !== null &&
    e.machineId === m &&
    Number.isSafeInteger(e.seq) &&
    e.seq > 0 &&
    e.id === `${m}:${e.seq}` &&
    ownedBy(e.sessionId, m)
  );
}

export function notificationOk(n: HubNotification, m: MachineId): boolean {
  return typeof n === "object" && n !== null && n.machineId === m && ownedBy(n.id, m);
}

/** Is a live update entirely about machine `m`? */
export function updateOk(u: MirrorUpdate, m: MachineId): boolean {
  if (typeof u !== "object" || u === null) return false;
  switch (u.type) {
    case "session":
      return sessionOk(u.session, m);
    case "event":
      return eventOk(u.event, m);
    case "pet":
      return petOk(u.pet, m);
    case "permission_request":
      return requestOk(u.request, m);
    case "permission_resolved":
      return ownedBy(u.requestId, m) && ownedBy(u.sessionId, m) && typeof u.outcome === "object" && u.outcome !== null;
    case "notification":
      return notificationOk(u.notification, m);
    default:
      return false;
  }
}

function verdictOf(request: PermissionRequest | undefined, outcome: PermissionOutcome): Resolution["verdict"] {
  if (outcome.kind === "cancelled") return "cancel";
  const kind = request?.options.find((o) => o.optionId === outcome.optionId)?.kind;
  if (kind === "allow_once" || kind === "allow_always") return "approve";
  if (kind === "reject_once" || kind === "reject_always") return "deny";
  return "answer";
}

function capSessions(sessions: Record<string, Session>): Record<string, Session> {
  const all = Object.values(sessions);
  if (all.length <= MIRROR_MAX_SESSIONS) return sessions;
  const keep = all.sort((a, b) => b.lastEventAt - a.lastEventAt).slice(0, MIRROR_MAX_SESSIONS);
  const out: Record<string, Session> = {};
  for (const s of keep) out[s.id] = s;
  return out;
}

function capEventSessions(events: Record<string, HubEvent[]>): Record<string, HubEvent[]> {
  const keys = Object.keys(events);
  if (keys.length <= MIRROR_MAX_EVENT_SESSIONS) return events;
  const lastSeq = (k: string) => events[k]?.[events[k]!.length - 1]?.seq ?? 0;
  const keep = keys.sort((a, b) => lastSeq(b) - lastSeq(a)).slice(0, MIRROR_MAX_EVENT_SESSIONS);
  const out: Record<string, HubEvent[]> = {};
  for (const k of keep) out[k] = events[k]!;
  return out;
}

function capPending(pending: Record<string, PermissionRequest>): Record<string, PermissionRequest> {
  const all = Object.values(pending);
  if (all.length <= MIRROR_MAX_PENDING) return pending;
  const out: Record<string, PermissionRequest> = {};
  for (const r of all.sort((a, b) => b.createdAt - a.createdAt).slice(0, MIRROR_MAX_PENDING)) out[r.requestId] = r;
  return out;
}

function capPets(pets: Record<string, Pet>): Record<string, Pet> {
  const entries = Object.entries(pets);
  // Insertion order: a pet updated again keeps its place; the earliest received go first.
  return entries.length <= MIRROR_MAX_PETS ? pets : Object.fromEntries(entries.slice(entries.length - MIRROR_MAX_PETS));
}

/** Every per-hub cap, so a hub (paired, but not trusted with the phone's memory) cannot grow the mirror without bound. */
export function capState(state: HubState): HubState {
  const sessions = capSessions(state.sessions);
  const events = capEventSessions(state.events);
  const pending = capPending(state.pending);
  const pets = capPets(state.pets);
  if (sessions === state.sessions && events === state.events && pending === state.pending && pets === state.pets) return state;
  return { ...state, sessions, events, pending, pets };
}

function withState(m: HubMirror, state: HubState, extra: Partial<HubMirror> = {}): HubMirror {
  return { ...m, ...extra, state: capState(state), version: m.version + 1 };
}

/** Apply one live update (already validated or not: invalid ones are dropped). */
export function applyUpdate(m: HubMirror, u: MirrorUpdate, now: UnixMs): HubMirror {
  if (!updateOk(u, m.machineId)) return m;
  if (u.type === "permission_resolved") {
    const request = m.state.pending[u.requestId];
    const res: Resolution = {
      requestId: u.requestId,
      sessionId: u.sessionId,
      machineId: m.machineId,
      outcome: u.outcome,
      by: u.by,
      verdict: verdictOf(request, u.outcome),
      at: now,
      ...(u.byMachineId !== undefined ? { byMachineId: u.byMachineId } : {}),
      ...(request ? { request } : {}),
    };
    const resolutions = [res, ...m.resolutions.filter((r) => r.requestId !== u.requestId)].slice(0, MIRROR_MAX_RESOLUTIONS);
    return withState(m, reduceServerMsg(m.state, u as ServerMsg), { resolutions });
  }
  if (u.type === "event" && m.state.events[u.event.sessionId]?.some((e) => e.id === u.event.id && e.seq === u.event.seq)) return m;
  const state = reduceServerMsg(m.state, u as ServerMsg, { ...DEFAULT_LIMITS, eventCap: MIRROR_EVENTS_PER_SESSION });
  if (state === m.state) return m;
  const maxSeq = u.type === "event" ? Math.max(m.maxSeq, u.event.seq) : m.maxSeq;
  return withState(m, state, { maxSeq });
}

/** `mirror_snapshot`: replaces this hub's sessions, pets and pending set (events are kept). */
export function applySnapshot(m: HubMirror, sessions: Session[], pets: Pet[], pending: PermissionRequest[]): HubMirror {
  const id = m.machineId;
  const s: Record<string, Session> = {};
  for (const x of Array.isArray(sessions) ? sessions : []) if (sessionOk(x, id)) s[x.id] = x;
  const p: Record<string, Pet> = {};
  for (const x of Array.isArray(pets) ? pets : []) if (petOk(x, id)) p[x.id] = x;
  const r: Record<string, PermissionRequest> = {};
  for (const x of Array.isArray(pending) ? pending : []) if (requestOk(x, id)) r[x.requestId] = x;
  return withState(m, { ...m.state, sessions: s, pets: p, pending: r }, { sync: "syncing" });
}

/** `mirror_events`: a batch, oldest first. */
export function applyEvents(m: HubMirror, events: HubEvent[]): HubMirror {
  const id = m.machineId;
  const grouped = new Map<string, HubEvent[]>();
  let maxSeq = m.maxSeq;
  for (const e of Array.isArray(events) ? events : []) {
    if (!eventOk(e, id)) continue;
    const list = grouped.get(e.sessionId);
    if (list) list.push(e);
    else grouped.set(e.sessionId, [e]);
    if (e.seq > maxSeq) maxSeq = e.seq;
  }
  if (grouped.size === 0) return m;
  const next = { ...m.state.events };
  for (const [sid, evs] of grouped) next[sid] = mergeEvents(next[sid], evs, MIRROR_EVENTS_PER_SESSION);
  return withState(m, { ...m.state, events: next }, { maxSeq });
}

/** Apply a mirror-related body from hub `m.machineId`; other bodies return `m` unchanged. */
export function applyMirrorBody(m: HubMirror, body: PeerBody, now: UnixMs): HubMirror {
  switch (body.type) {
    case "mirror":
      return applyUpdate(m, body.update, now);
    case "mirror_snapshot":
      return applySnapshot(m, body.sessions, body.pets, body.pendingPermissions);
    case "mirror_events":
      return applyEvents(m, body.events);
    case "sync_done": {
      const seq = Number.isSafeInteger(body.seq) && body.seq > 0 ? body.seq : 0;
      return { ...m, sync: "synced", truncated: body.truncated === true, maxSeq: Math.max(m.maxSeq, seq), version: m.version + 1 };
    }
    default:
      return m;
  }
}

/** Mark a new sync as started (after sending sync_request). */
export function startSync(m: HubMirror): HubMirror {
  return { ...m, sync: "syncing", version: m.version + 1 };
}
