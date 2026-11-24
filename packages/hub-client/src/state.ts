// Framework-free hub state + pure reducers. No DOM, no Node, no React.
import type {
  AckResults,
  Agent,
  ChatMessage,
  ClientMsg,
  HubEvent,
  HubNotification,
  Pet,
  PermissionRequest,
  Rule,
  ServerMsg,
  Session,
  Welcome,
} from "@ai-pet/protocol";

/** `Omit` that distributes over unions (keeps discriminated unions intact). */
export type DistributiveOmit<T, K extends PropertyKey> = T extends unknown ? Omit<T, K> : never;

export type HubConnectionStatus = "disconnected" | "connecting" | "connected" | "unauthorized";

export interface HubState {
  status: HubConnectionStatus;
  agents: Agent[];
  sessions: Record<string, Session>;
  pets: Record<string, Pet>;
  pending: Record<string, PermissionRequest>;
  rules: Rule[];
  /** Per session, ordered by event id ascending, capped (default last 500). */
  events: Record<string, HubEvent[]>;
  chat: ChatMessage[];
  notifications: HubNotification[];
  /** From the hello `Welcome`, once connected at least once. */
  hubVersion?: string | undefined;
  /** Last connection/handshake error, cleared on successful hello. */
  lastError?: string | undefined;
}

export interface StateLimits {
  /** Max events kept per session (newest kept). */
  eventCap: number;
  /** Max chat messages kept (newest kept). */
  chatCap: number;
  /** Max notifications kept (newest kept). */
  notificationCap: number;
}

export const DEFAULT_LIMITS: StateLimits = {
  eventCap: 500,
  chatCap: 300,
  notificationCap: 100,
};

export function initialHubState(): HubState {
  return {
    status: "disconnected",
    agents: [],
    sessions: {},
    pets: {},
    pending: {},
    rules: [],
    events: {},
    chat: [],
    notifications: [],
  };
}

function byKey<T>(items: readonly T[], key: (item: T) => string): Record<string, T> {
  const out: Record<string, T> = {};
  for (const item of items) out[key(item)] = item;
  return out;
}

/** Keep the newest `cap` items of an array (returns the same array if already short enough). */
function capTail<T>(items: T[], cap: number): T[] {
  return items.length > cap ? items.slice(items.length - cap) : items;
}

/**
 * Merge `incoming` events into an id-ascending list: dedupes by id (incoming wins),
 * keeps ascending order, caps to the newest `cap`.
 */
export function mergeEvents(existing: readonly HubEvent[] | undefined, incoming: readonly HubEvent[], cap: number): HubEvent[] {
  if (incoming.length === 0) return existing ? (existing as HubEvent[]) : [];
  const base = existing ?? [];
  // Fast path: single event newer than everything we have (the live-stream case).
  if (incoming.length === 1) {
    const ev = incoming[0]!;
    const last = base[base.length - 1];
    if (!last || ev.id > last.id) return capTail([...base, ev], cap);
  }
  const map = new Map<number, HubEvent>();
  for (const ev of base) map.set(ev.id, ev);
  for (const ev of incoming) map.set(ev.id, ev);
  const merged = [...map.values()].sort((a, b) => a.id - b.id);
  return capTail(merged, cap);
}

function upsertById<T extends { id: string }>(items: readonly T[], item: T, cap: number): T[] {
  const idx = items.findIndex((x) => x.id === item.id);
  if (idx >= 0) {
    const next = items.slice();
    next[idx] = item;
    return next;
  }
  return capTail([...items, item], cap);
}

function upsertChat(chat: readonly ChatMessage[], msg: ChatMessage, cap: number): ChatMessage[] {
  const idx = chat.findIndex((m) => m.id === msg.id);
  if (idx >= 0) {
    const next = chat.slice();
    next[idx] = msg;
    return next;
  }
  const next = [...chat, msg];
  // Keep chronological order even if a message arrives late.
  if (chat.length > 0 && chat[chat.length - 1]!.ts > msg.ts) next.sort((a, b) => a.ts - b.ts);
  return capTail(next, cap);
}

/** Apply the hello `Welcome` snapshot. Events/chat/notifications are kept (they are incremental). */
export function applyWelcome(state: HubState, welcome: Welcome): HubState {
  const pets = byKey(welcome.pets ?? [], (p) => p.id);
  return {
    ...state,
    status: "connected",
    hubVersion: welcome.hubVersion,
    lastError: undefined,
    agents: welcome.agents ?? [],
    sessions: byKey(welcome.sessions ?? [], (s) => s.id),
    pets,
    pending: byKey(welcome.pendingPermissions ?? [], (r) => r.requestId),
    rules: welcome.rules ?? [],
  };
}

/** Pure reducer for every hub -> client message. `ack` is a no-op here (see `applyAck`). */
export function reduceServerMsg(state: HubState, msg: ServerMsg, limits: StateLimits = DEFAULT_LIMITS): HubState {
  switch (msg.type) {
    case "ack":
      return state;
    case "agents":
      return { ...state, agents: msg.agents };
    case "session":
      return { ...state, sessions: { ...state.sessions, [msg.session.id]: msg.session } };
    case "event": {
      const sid = msg.event.sessionId;
      const current = state.events[sid];
      const next = mergeEvents(current, [msg.event], limits.eventCap);
      if (next === current) return state;
      return { ...state, events: { ...state.events, [sid]: next } };
    }
    case "permission_request":
      return { ...state, pending: { ...state.pending, [msg.request.requestId]: msg.request } };
    case "permission_resolved": {
      if (!(msg.requestId in state.pending)) return state;
      const { [msg.requestId]: _gone, ...pending } = state.pending;
      return { ...state, pending };
    }
    case "pet":
      return { ...state, pets: { ...state.pets, [msg.pet.id]: msg.pet } };
    case "chat":
      return { ...state, chat: upsertChat(state.chat, msg.message, limits.chatCap) };
    case "notification":
      return { ...state, notifications: upsertById(state.notifications, msg.notification, limits.notificationCap) };
    default: {
      // Unknown message type from a newer hub: ignore.
      const _exhaustive: never = msg;
      void _exhaustive;
      return state;
    }
  }
}

/** A client request body (a `ClientMsg` without its `id`). */
export type ClientRequest = DistributiveOmit<ClientMsg, "id">;

/**
 * Fold a successful `ack` result into state, so request results are reflected even if the
 * hub does not also broadcast them. Irrelevant types return `state` unchanged.
 */
export function applyAck(
  state: HubState,
  request: ClientRequest,
  result: unknown,
  limits: StateLimits = DEFAULT_LIMITS,
): HubState {
  switch (request.type) {
    case "hello":
      return result ? applyWelcome(state, result as AckResults["hello"]) : state;
    case "list_agents": {
      const agents = (result as Partial<AckResults["list_agents"]> | undefined)?.agents;
      return agents ? { ...state, agents } : state;
    }
    case "list_sessions": {
      const sessions = (result as Partial<AckResults["list_sessions"]> | undefined)?.sessions;
      if (!sessions) return state;
      return { ...state, sessions: { ...state.sessions, ...byKey(sessions, (s) => s.id) } };
    }
    case "get_events": {
      const events = (result as Partial<AckResults["get_events"]> | undefined)?.events;
      if (!events || events.length === 0) return state;
      const grouped: Record<string, HubEvent[]> = {};
      for (const ev of events) (grouped[ev.sessionId] ??= []).push(ev);
      const nextEvents = { ...state.events };
      for (const [sid, evs] of Object.entries(grouped)) {
        nextEvents[sid] = mergeEvents(nextEvents[sid], evs, limits.eventCap);
      }
      return { ...state, events: nextEvents };
    }
    case "start_session":
    case "resume_session": {
      const session = (result as Partial<AckResults["start_session"]> | undefined)?.session;
      if (!session) return state;
      return { ...state, sessions: { ...state.sessions, [session.id]: session } };
    }
    case "resolve_permission": {
      // Optimistic: the hub also broadcasts `permission_resolved`.
      if (!(request.requestId in state.pending)) return state;
      const { [request.requestId]: _gone, ...pending } = state.pending;
      return { ...state, pending };
    }
    case "chat": {
      const message = (result as Partial<AckResults["chat"]> | undefined)?.message;
      if (!message) return state;
      return { ...state, chat: upsertChat(state.chat, message, limits.chatCap) };
    }
    case "update_pet": {
      const pet = (result as Partial<AckResults["update_pet"]> | undefined)?.pet;
      if (!pet) return state;
      return { ...state, pets: { ...state.pets, [pet.id]: pet } };
    }
    case "set_rules": {
      const rules = (result as Partial<AckResults["set_rules"]> | undefined)?.rules;
      return rules ? { ...state, rules } : state;
    }
    default:
      return state;
  }
}

/** Most recent event per session (handy input for pet-engine's `derivePetViews`). */
export function lastEventsBySession(state: Pick<HubState, "events">): Record<string, HubEvent | undefined> {
  const out: Record<string, HubEvent | undefined> = {};
  for (const [sid, evs] of Object.entries(state.events)) out[sid] = evs[evs.length - 1];
  return out;
}
