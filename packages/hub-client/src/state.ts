// Framework-free hub state + pure reducers. No DOM, no Node, no React.
import type {
  AckResults,
  Agent,
  ChatMessage,
  ClientMsg,
  Device,
  HubEvent,
  HubNotification,
  HubSettings,
  Machine,
  MachineId,
  Pet,
  PeerStatus,
  PermissionRequest,
  Rule,
  ServerMsg,
  Session,
  Welcome,
} from "@pawl/protocol";
import { isPhoneOs } from "./machine";

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
  /**
   * Per session id (global). Each list is deduped by the event's global `id` and ordered by `seq`
   * ascending (the hub's cursor), capped (default last 500).
   */
  events: Record<string, HubEvent[]>;
  chat: ChatMessage[];
  notifications: HubNotification[];
  /** The machine this hub runs on (from the hello `Welcome`; kept across reconnects). */
  machine?: Machine | undefined;
  /** Every known machine by id: this hub's own plus every paired (not revoked) hub, with its online state. */
  machines: Record<MachineId, Machine>;
  /** The pairing roster (never includes this hub). Replaced by every Welcome and `devices` message. */
  devices: Device[];
  /** This hub's peer-listener status; undefined until a protocol v3 hub said hello. */
  peer?: PeerStatus | undefined;
  /** From the hello `Welcome`, once connected at least once. */
  hubVersion?: string | undefined;
  /** Last connection/handshake error, cleared on successful hello. */
  lastError?: string | undefined;
  /** The hub's protocol version from the hello `Welcome` (settings need 7+). */
  protocolVersion?: number | undefined;
  /**
   * The hub's settings (migration step 5b), from `getSettings` / `updateSettings` and every `settings` broadcast.
   * Undefined until fetched once. Secrets are never included (only whether they are set).
   */
  settings?: HubSettings | undefined;
  /** Settings keys saved to config.toml that only take effect after a hub restart (e.g. "hub.port"). */
  restartRequired?: string[] | undefined;
  /** Where the hub keeps its config.toml (from `getSettings`). */
  settingsConfigPath?: string | undefined;
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
    machines: {},
    devices: [],
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

/** Event order: `seq` ascending (per-machine cursor), then global id for a stable total order. */
function compareEvents(a: HubEvent, b: HubEvent): number {
  if (a.seq !== b.seq) return a.seq - b.seq;
  return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
}

/**
 * Merge `incoming` events into a `seq`-ascending list: dedupes by global `id` (incoming wins),
 * keeps ascending order, caps to the newest `cap`.
 */
export function mergeEvents(existing: readonly HubEvent[] | undefined, incoming: readonly HubEvent[], cap: number): HubEvent[] {
  if (incoming.length === 0) return existing ? (existing as HubEvent[]) : [];
  const base = existing ?? [];
  // Fast path: single event newer than everything we have (the live-stream case).
  if (incoming.length === 1) {
    const ev = incoming[0]!;
    const last = base[base.length - 1];
    if (!last || compareEvents(ev, last) > 0) {
      if (!base.some((e) => e.id === ev.id)) return capTail([...base, ev], cap);
    }
  }
  const map = new Map<string, HubEvent>();
  for (const ev of base) map.set(ev.id, ev);
  for (const ev of incoming) map.set(ev.id, ev);
  const merged = [...map.values()].sort(compareEvents);
  return capTail(merged, cap);
}

/** Highest `seq` held for a session (the `sinceSeq` cursor for `getEvents`), or undefined if none. */
export function latestEventSeq(state: Pick<HubState, "events">, sessionId: string): number | undefined {
  const list = state.events[sessionId];
  return list && list.length > 0 ? list[list.length - 1]!.seq : undefined;
}

/** Insert or replace a machine; also refreshes `state.machine` when it is this hub's own. */
export function upsertMachine(state: HubState, machine: Machine): HubState {
  return {
    ...state,
    machines: { ...state.machines, [machine.id]: machine },
    machine: state.machine && state.machine.id === machine.id ? machine : state.machine,
  };
}

/**
 * Forget a paired device: drops it from `devices` and, for a hub (whose device id is its machine id),
 * from `machines`. This hub's own machine is never removed. Returns `state` itself when nothing changes.
 */
export function removeDevice(state: HubState, deviceId: string): HubState {
  const own = state.machine?.id === deviceId;
  const inMachines = !own && deviceId in state.machines;
  const devices = state.devices.filter((d) => d.id !== deviceId);
  if (!inMachines && devices.length === state.devices.length) return state;
  let machines = state.machines;
  if (inMachines) {
    const { [deviceId]: _gone, ...rest } = state.machines;
    machines = rest;
  }
  return { ...state, machines, devices: devices.length === state.devices.length ? state.devices : devices };
}

/** Insert or replace a device in the roster (by id). */
export function upsertDevice(state: HubState, device: Device): HubState {
  const idx = state.devices.findIndex((d) => d.id === device.id);
  const devices = state.devices.slice();
  if (idx >= 0) devices[idx] = device;
  else devices.push(device);
  return { ...state, devices };
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
  // `machines` is the hub's full snapshot; make sure our own machine is in it either way.
  const machines = byKey(welcome.machines ?? [], (m) => m.id);
  const machine = welcome.machine ?? state.machine;
  if (welcome.machine) machines[welcome.machine.id] = welcome.machine;
  return {
    ...state,
    status: "connected",
    hubVersion: welcome.hubVersion,
    protocolVersion: typeof welcome.protocolVersion === "number" ? welcome.protocolVersion : state.protocolVersion,
    machine,
    machines,
    devices: welcome.devices ?? [],
    // A pre-v3 hub has no peer link at all.
    peer: welcome.peer ?? undefined,
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
    case "machine":
      return upsertMachine(state, msg.machine);
    case "machine_removed":
      return removeDevice(state, msg.machineId);
    case "devices":
      return { ...state, devices: msg.devices ?? [] };
    case "peer_status":
      return { ...state, peer: msg.peer };
    case "settings":
      return { ...state, settings: msg.settings, restartRequired: msg.restartRequired ?? [] };
    case "open_panel":
      // A request for the desktop app to show a view: an event for `onMessage`, no state.
      return state;
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
      // Another machine's agents (from its presence) are returned to the caller, not stored.
      if (request.machineId !== undefined && request.machineId !== state.machine?.id) return state;
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
    case "update_machine": {
      const machine = (result as Partial<AckResults["update_machine"]> | undefined)?.machine;
      return machine ? upsertMachine(state, machine) : state;
    }
    case "join_peer": {
      // The hub also broadcasts `devices` + `machine`; folding the ack shows the new peer at once.
      const res = result as Partial<AckResults["join_peer"]> | undefined;
      let next = state;
      if (res?.device) next = upsertDevice(next, res.device);
      if (res?.machine) next = upsertMachine(next, res.machine);
      return next;
    }
    case "revoke_device":
      // ok:true means the device is gone; the hub follows up with `machine_removed` + `devices`.
      return removeDevice(state, request.deviceId);
    case "get_settings": {
      const res = result as Partial<AckResults["get_settings"]> | undefined;
      if (!res?.settings) return state;
      return {
        ...state,
        settings: res.settings,
        restartRequired: res.restartRequired ?? [],
        settingsConfigPath: res.configPath ?? state.settingsConfigPath,
      };
    }
    case "update_settings": {
      // The hub also broadcasts `settings`; folding the ack shows the saved values at once.
      const res = result as Partial<AckResults["update_settings"]> | undefined;
      if (!res?.settings) return state;
      return { ...state, settings: res.settings, restartRequired: res.restartRequired ?? state.restartRequired ?? [] };
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

/**
 * Machines for display: this hub's own first, then online before offline, then by name.
 * Includes only what `state.machines` holds (this hub plus paired hubs).
 */
export function sortedMachines(state: Pick<HubState, "machine" | "machines">): Machine[] {
  const own = state.machine?.id;
  // Phones are devices, never machines (a hub should not list them; skip one if it does).
  return Object.values(state.machines).filter((m) => !isPhoneOs(m.os)).sort(
    (a, b) =>
      Number(b.id === own) - Number(a.id === own) ||
      Number(b.online) - Number(a.online) ||
      a.name.localeCompare(b.name) ||
      (a.id < b.id ? -1 : a.id > b.id ? 1 : 0),
  );
}

/** "N machines · M online" numbers: every known machine (this one included) and how many are online. */
export function machineCounts(state: Pick<HubState, "machines">): { total: number; online: number } {
  const all = Object.values(state.machines).filter((m) => !isPhoneOs(m.os));
  return { total: all.length, online: all.filter((m) => m.online).length };
}
