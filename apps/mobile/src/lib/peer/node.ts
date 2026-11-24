// PhoneNode: the phone as a Krypto peer (PLAN.md step 7). Relay only. Pairs by invite URI (QR), keeps a per-hub
// E2E link (hello -> presence, presence every 15 s, offline after 45 s of silence), mirrors every hub's sessions,
// pets and pending approvals, applies group-roster introductions, and sends intents (`client: "mobile"`).
// Platform-free: storage, WebSocket, clock and randomness are injected (see ./types). Drive it headless from bun:
//
//   const phone = await PhoneNode.open({ name: "Test phone", os: "android" });
//   phone.start();
//   await phone.pair(inviteUri);
//   await phone.waitFor((s) => Object.keys(s.pending).length > 0);
//   await phone.approve(requestId, { confirmed: true });
import {
  INTENT_TIMEOUT_MS,
  MACHINE_ID_RE,
  PRESENCE_INTERVAL_MS,
  PRESENCE_OFFLINE_MS,
  REPO_INDEX_MAX,
  parseGlobalId,
  type Agent,
  type HubEvent,
  type Intent,
  type IntroDevice,
  type Machine,
  type MachineId,
  type MachineOs,
  type PeerBody,
  type PeerEnvelope,
  type PermissionRequest,
  type Pet,
  type Presence,
  type RelayErrorCode,
  type RepoRef,
  type Session,
  type UnixMs,
} from "@krypto/protocol";
import { bytesEqual, decodeKey, encode } from "./b64";
import { open, seal } from "./envelope";
import {
  approveOption,
  denyOption,
  IntentError,
  intentNeedsConfirmation,
  newIntentId,
  type IntentApplied,
} from "./intents";
import { parseInvite, type Invite } from "./invite";
import { DeviceKeys } from "./keys";
import { applyMirrorBody, emptyMirror, MIRROR_MAX_PETS, startSync, type HubMirror, type Resolution } from "./mirror";
import { parseRelayAddress, relayAddress } from "./net";
import { RelayClient, type RelayConfig, type RelayStatusInfo, type RelayTimings } from "./relay-client";
import { cleanDeviceName, deviceNameOk, evaluateIntro, helloRetryDelay, type IntroSkipReason } from "./roster";
import {
  hubKey,
  K_IDENTITY,
  K_RELAY,
  K_REVOKED,
  K_SETTINGS,
  PhoneStore,
  REVOKED_MAX,
  seqKey,
  type StoredHub,
  type StoredIdentity,
  type StoredSeq,
  type StoredSettings,
} from "./store";
import {
  globalSocketFactory,
  MemoryStore,
  silentLogger,
  systemClock,
  webRandomBytes,
  type Clock,
  type KeyValueStore,
  type Logger,
  type RandomBytes,
  type SocketFactory,
  type TimerHandle,
} from "./types";

// ---------- public types ----------

export interface PhoneTimings {
  /** Pairing: the inviter must answer within this (Rust JOIN_TIMEOUT). */
  joinTimeoutMs: number;
  /** Pairing: the relay must authenticate within this. */
  relayWaitMs: number;
  presenceIntervalMs: number;
  /** No authenticated frame from a hub for this long: its link is down. */
  offlineAfterMs: number;
  /** Wait this long for the answer to a hello before trying again (plus the retry backoff). */
  helloAnswerMs: number;
  intentTimeoutMs: number;
  /** hello / presence stamped further than this from our clock are dropped. */
  tsSkewMs: number;
  relay?: Partial<RelayTimings>;
}

export const DEFAULT_PHONE_TIMINGS: PhoneTimings = {
  joinTimeoutMs: 10_000,
  relayWaitMs: 10_000,
  presenceIntervalMs: PRESENCE_INTERVAL_MS,
  offlineAfterMs: PRESENCE_OFFLINE_MS,
  helloAnswerMs: 10_000,
  intentTimeoutMs: INTENT_TIMEOUT_MS,
  tsSkewMs: 60 * 60_000,
};

export type PhoneOs = Extract<MachineOs, "ios" | "android" | "other">;

export interface PhoneNodeOptions {
  /** Persistent storage (expo-secure-store on the phone). Default: in memory. */
  store?: KeyValueStore;
  /** WebSocket factory. Default: the global WebSocket (React Native, bun). */
  socketFactory?: SocketFactory;
  clock?: Clock;
  /** CSPRNG. Default: crypto.getRandomValues. */
  random?: RandomBytes;
  logger?: Logger;
  /** Device name shown on the hubs (used when the identity is created, and by `setName`). Default "Phone". */
  name?: string;
  /** Default "other"; the Expo adapter passes ios / android from Platform. */
  os?: PhoneOs;
  timings?: Partial<PhoneTimings>;
}

export interface HubView {
  id: MachineId;
  name: string;
  os?: MachineOs;
  hubVersion?: string;
  /** Live link (the phone reaches hubs over the relay only). */
  online: boolean;
  path: "relay" | null;
  lastSeen?: UnixMs;
  pairedAt: UnixMs;
  introducedBy?: MachineId;
  userActiveAt?: UnixMs;
  sync: HubMirror["sync"];
  truncated: boolean;
  /** The relay says this hub does not accept our frames (not paired there yet, or it removed this phone). */
  refusedSince?: UnixMs;
}

export type NoticeLevel = "info" | "success" | "warning";

/** Phone-local notices (group roster changes, unpairing). Never contain secrets. */
export interface PhoneNotice {
  id: string;
  level: NoticeLevel;
  text: string;
  ts: UnixMs;
}

export interface PhoneSnapshot {
  /** Bumped on every change. */
  version: number;
  deviceId: string;
  name: string;
  os: PhoneOs;
  publicKey: string;
  paired: boolean;
  joining: boolean;
  relay: RelayStatusInfo;
  hubs: HubView[];
  /** Hubs as Machines (online = linked). */
  machines: Record<MachineId, Machine>;
  sessions: Record<string, Session>;
  pets: Record<string, Pet>;
  pending: Record<string, PermissionRequest>;
  events: Record<string, HubEvent[]>;
  agents: Record<MachineId, Agent[]>;
  repos: Record<MachineId, RepoRef[]>;
  /** Recent decisions across hubs, newest first. */
  resolutions: Resolution[];
  notices: PhoneNotice[];
  pushEnabled: boolean;
  pushToken: string | null;
}

export type PhoneEvent =
  | { type: "paired"; hub: HubView }
  | { type: "link"; hubId: MachineId; up: boolean }
  | { type: "hub_added"; hub: HubView; introducedBy: MachineId }
  | { type: "hub_removed"; hubId: MachineId; byHub: boolean }
  | { type: "permission_request"; request: PermissionRequest }
  | { type: "permission_resolved"; resolution: Resolution }
  | { type: "relay"; status: RelayStatusInfo }
  | { type: "notice"; notice: PhoneNotice };

export type PairErrorCode =
  | "bad_uri"
  /** A LAN-only invite: the phone needs a relay invite ("Pair a phone" on the desktop). */
  | "no_relay"
  /** The invite names a different relay than the hubs already paired. */
  | "other_relay"
  | "self_invite"
  | "busy"
  | "unreachable"
  | "timeout"
  | "invite_rejected";

export class PairError extends Error {
  constructor(
    readonly code: PairErrorCode,
    /** For `unreachable`: the relay's last error code (bad_key, bad_auth, timeout, ...). */
    readonly detail?: string,
  ) {
    super(detail ? `${code} (${detail})` : code);
    this.name = "PairError";
  }
}

// ---------- internals ----------

interface LinkState {
  up: boolean;
  lastAuthAt: number;
  helloPending: boolean;
  attempt: number;
  retry: TimerHandle | null;
  silence: TimerHandle | null;
  presence: TimerHandle | null;
}

interface HubRec {
  stored: StoredHub;
  pk: Uint8Array;
  address: string;
  seq: StoredSeq;
  machine: Machine;
  agents: Agent[];
  repos: RepoRef[];
  presencePets: Pet[];
  mirror: HubMirror;
  link: LinkState;
  refusedSince?: UnixMs;
}

interface JoinState {
  invite: Invite;
  reqSeq: number;
  settle(r: { ok: true; env: PeerEnvelope } | { ok: false; code: PairErrorCode; detail?: string }): void;
}

interface PendingIntent {
  hubId: MachineId;
  resolve(v: IntentApplied): void;
  reject(e: IntentError): void;
  timer: TimerHandle;
  action: Intent["kind"];
}

const NOTICES_MAX = 50;
/** Same limit as a hub's machine name. */
export const PHONE_NAME_MAX = 40;
const AGENTS_MAX = 64;

function newDeviceId(random: RandomBytes): string {
  const alphabet = "abcdefghijklmnopqrstuvwxyz0123456789";
  let id = "";
  while (id.length < 12) {
    for (const b of random(16)) {
      // 252 = 7 * 36: rejection sampling keeps every character equally likely.
      if (b < 252 && id.length < 12) id += alphabet[b % 36];
    }
  }
  return id;
}

function strOk(s: unknown, max: number): s is string {
  // eslint-disable-next-line no-control-regex
  return typeof s === "string" && s.length > 0 && s.length <= max && !/[\u0000-\u001f\u007f]/.test(s);
}

export class PhoneNode {
  readonly #clock: Clock;
  readonly #random: RandomBytes;
  readonly #log: Logger;
  readonly #t: PhoneTimings;
  readonly #store: PhoneStore;
  readonly #socketFactory: SocketFactory;
  #keys: DeviceKeys;
  #identity: StoredIdentity;
  #relayConfig: RelayConfig | null;
  #relay: RelayClient;
  #hubs = new Map<MachineId, HubRec>();
  #revoked: { ids: string[]; pks: string[] };
  #settings: StoredSettings;
  #join: JoinState | null = null;
  /** During a join on an adopted relay: allow only the inviter, send no hellos. */
  #restricted = false;
  #intents = new Map<string, PendingIntent>();
  /** Accepted frames waiting for their last_rx to be stored (see `#commitInbound`). */
  #inbound: { rec: HubRec; body: PeerBody }[] = [];
  #committing = false;
  #notices: PhoneNotice[] = [];
  #started = false;
  #version = 0;
  #snapshot: PhoneSnapshot | null = null;
  #listeners = new Set<(s: PhoneSnapshot) => void>();
  #eventListeners = new Set<(e: PhoneEvent) => void>();
  #notifyQueued = false;

  private constructor(args: {
    clock: Clock;
    random: RandomBytes;
    log: Logger;
    timings: PhoneTimings;
    store: PhoneStore;
    socketFactory: SocketFactory;
    keys: DeviceKeys;
    identity: StoredIdentity;
    relay: RelayConfig | null;
    revoked: { ids: string[]; pks: string[] };
    settings: StoredSettings;
  }) {
    this.#clock = args.clock;
    this.#random = args.random;
    this.#log = args.log;
    this.#t = args.timings;
    this.#store = args.store;
    this.#socketFactory = args.socketFactory;
    this.#keys = args.keys;
    this.#identity = args.identity;
    this.#relayConfig = args.relay;
    this.#revoked = args.revoked;
    this.#settings = args.settings;
    this.#relay = this.#makeRelay();
  }

  /** Load the phone's identity and roster from `store` (creating a fresh identity on first use). */
  static async open(opts: PhoneNodeOptions = {}): Promise<PhoneNode> {
    const clock = opts.clock ?? systemClock;
    const random = opts.random ?? webRandomBytes;
    const kv = opts.store ?? new MemoryStore();
    const store = new PhoneStore(kv, clock);
    const loaded = await store.load();
    let keys: DeviceKeys;
    let fresh = false;
    if (loaded.secret) {
      keys = DeviceKeys.fromSecret(loaded.secret);
      loaded.secret.fill(0);
    } else {
      keys = DeviceKeys.generate(random);
      const secret = keys.exportSecret();
      await store.saveSecret(secret);
      secret.fill(0);
      fresh = true;
    }
    const storedOs = loaded.identity?.os;
    const os: PhoneOs = opts.os ?? (storedOs === "ios" || storedOs === "android" ? storedOs : "other");
    let identity = loaded.identity;
    if (fresh || !identity || !MACHINE_ID_RE.test(identity.deviceId)) {
      identity = {
        deviceId: newDeviceId(random),
        name: cleanDeviceName(opts.name, PHONE_NAME_MAX) ?? "Phone",
        os,
        createdAt: clock.now(),
      };
      store.put(K_IDENTITY, identity);
    } else if (identity.os !== os) {
      identity = { ...identity, os };
      store.put(K_IDENTITY, identity);
    }
    const relay =
      !fresh && loaded.relay && typeof loaded.relay.url === "string"
        ? { url: loaded.relay.url, accessKey: loaded.relay.accessKey ?? null }
        : null;
    const node = new PhoneNode({
      clock,
      random,
      log: opts.logger ?? silentLogger,
      timings: { ...DEFAULT_PHONE_TIMINGS, ...opts.timings },
      store,
      socketFactory: opts.socketFactory ?? globalSocketFactory,
      keys,
      identity,
      relay,
      revoked: fresh ? { ids: [], pks: [] } : loaded.revoked,
      settings: loaded.settings,
    });
    if (!fresh) for (const { hub, seq } of loaded.hubs) node.#addRec(hub, seq);
    await store.flush();
    return node;
  }

  // ---------- facade: identity + lifecycle ----------

  get deviceId(): string {
    return this.#identity.deviceId;
  }

  get publicKey(): string {
    return this.#keys.publicB64;
  }

  /** Connect to the relay (if paired) and say hello to every hub. Idempotent. */
  start(): void {
    this.#started = true;
    if (this.#relayConfig) this.#relay.setConfig(this.#relayConfig);
  }

  /** Disconnect (timers stop, state stays). */
  async stop(): Promise<void> {
    this.#started = false;
    this.#relay.stop();
    for (const rec of this.#hubs.values()) this.#linkDown(rec, false);
    await this.#store.flush();
  }

  /** The app came to the foreground: reconnect now and retry every hub that is not linked. */
  onForeground(): void {
    if (!this.#started) return;
    this.#relay.reconnectNow();
    if (this.#relay.connected) {
      for (const rec of this.#hubs.values()) {
        if (!rec.link.up) {
          rec.link.attempt = 0;
          this.#hello(rec);
        }
      }
    }
  }

  /** The app goes to the background: persist now (the OS may suspend us). */
  onBackground(): Promise<void> {
    return this.#store.flush();
  }

  /** Wait for queued storage writes. */
  flush(): Promise<void> {
    return this.#store.flush();
  }

  /** Rename this phone (1..=40 chars); hubs see it with the next presence. */
  setName(name: string): void {
    const clean = cleanDeviceName(name, PHONE_NAME_MAX);
    if (!clean) throw new Error("invalid_name");
    this.#identity = { ...this.#identity, name: clean };
    this.#store.put(K_IDENTITY, this.#identity);
    for (const rec of this.#hubs.values()) if (rec.link.up) this.#sendPresence(rec);
    this.#changed();
  }

  // ---------- facade: state ----------

  getSnapshot(): PhoneSnapshot {
    if (!this.#snapshot) this.#snapshot = this.#buildSnapshot();
    return this.#snapshot;
  }

  /** Called (coalesced, async) after every state change. */
  subscribe(fn: (s: PhoneSnapshot) => void): () => void {
    this.#listeners.add(fn);
    return () => this.#listeners.delete(fn);
  }

  /** Discrete events (pairing, links, new approvals, roster changes), synchronously as they happen. */
  onEvent(fn: (e: PhoneEvent) => void): () => void {
    this.#eventListeners.add(fn);
    return () => this.#eventListeners.delete(fn);
  }

  /** Resolve once `pred(snapshot)` holds (checked now and after every change); reject after `timeoutMs`. */
  waitFor(pred: (s: PhoneSnapshot) => boolean, timeoutMs = 30_000): Promise<PhoneSnapshot> {
    return new Promise((resolve, reject) => {
      const now = this.getSnapshot();
      if (pred(now)) return resolve(now);
      const timer = this.#clock.setTimeout(() => {
        off();
        reject(new Error("timeout"));
      }, timeoutMs);
      const off = this.subscribe((s) => {
        if (!pred(s)) return;
        off();
        this.#clock.clearTimeout(timer);
        resolve(s);
      });
    });
  }

  hubs(): HubView[] {
    return this.getSnapshot().hubs;
  }

  relayStatus(): RelayStatusInfo {
    return this.#relay.status;
  }

  sessions(machineId?: MachineId): Session[] {
    const all = Object.values(this.getSnapshot().sessions);
    return (machineId ? all.filter((s) => s.machineId === machineId) : all).sort((a, b) => b.lastEventAt - a.lastEventAt);
  }

  pendingPermissions(): PermissionRequest[] {
    return Object.values(this.getSnapshot().pending).sort((a, b) => a.createdAt - b.createdAt);
  }

  events(sessionId: string): HubEvent[] {
    return this.getSnapshot().events[sessionId] ?? [];
  }

  /** How a request was decided (for "Approved from <device>"), if this phone saw it. */
  resolution(requestId: string): Resolution | undefined {
    return this.getSnapshot().resolutions.find((r) => r.requestId === requestId);
  }

  /** Display name for a device id that decided something: this phone, a paired hub, or null if unknown. */
  deviceName(id: string | undefined): string | null {
    if (!id) return null;
    if (id === this.deviceId) return "this phone";
    return this.#hubs.get(id)?.machine.name ?? null;
  }

  // ---------- facade: push ----------

  /** The Expo push token (null = none). Registered with the relay on every session while push is enabled. */
  setPushToken(token: string | null): void {
    const s = this.#settings;
    // Dropping a token the relay may hold: it must be told to forget it. A new token while push is on replaces it.
    const pushClear = token === null ? s.pushClear || s.pushToken !== null : s.pushEnabled ? false : s.pushClear;
    this.#settings = { ...s, pushToken: token, pushClear };
    this.#store.put(K_SETTINGS, this.#settings);
    this.#relay.syncPush();
    this.#changed();
  }

  /**
   * Push on: the token goes to the relay (`register_push`). Off: the relay is told `unregister_push` now, or on the next
   * relay session if it is not connected (the pending flag is persisted).
   */
  setPushEnabled(on: boolean): void {
    const s = this.#settings;
    // Only turning it off (not repeating "off") asks the relay to forget the token.
    this.#settings = { ...s, pushEnabled: on, pushClear: on ? false : s.pushEnabled || s.pushClear };
    this.#store.put(K_SETTINGS, this.#settings);
    this.#relay.syncPush();
    this.#changed();
  }

  // ---------- facade: pairing ----------

  /**
   * Pair with the hub that showed `uri` (a phone invite: relay-only). Connects to the invite's relay (with its
   * access key) when no relay is set yet, allows only the inviter until pairing succeeds, sends `pair_request`
   * with the relay pairing token and waits for `pair_accept`. Throws PairError.
   */
  async pair(uri: string): Promise<HubView> {
    let invite: Invite;
    try {
      invite = parseInvite(uri);
    } catch {
      throw new PairError("bad_uri");
    }
    if (!invite.relay || !invite.relayToken) throw new PairError("no_relay");
    if (invite.machineId === this.deviceId || this.#keys.isOwn(invite.publicKey) || this.#keys.isLowOrder(invite.publicKey)) {
      throw new PairError("self_invite");
    }
    if (this.#join) throw new PairError("busy");
    const prev = this.#relayConfig;
    if (prev && prev.url !== invite.relay && this.#hubs.size > 0) throw new PairError("other_relay");
    const target: RelayConfig = {
      url: invite.relay,
      accessKey: invite.relayKey ?? (prev && prev.url === invite.relay ? prev.accessKey : null),
    };
    const adopt = !prev || prev.url !== target.url || prev.accessKey !== target.accessKey;
    const existing = this.#hubs.get(invite.machineId);
    const reqSeq = Math.max((existing?.seq.tx ?? 0) + 1, this.#timeSeq());
    let resolveJoin!: (v: HubView) => void;
    let rejectJoin!: (e: PairError) => void;
    const answered = new Promise<HubView>((res, rej) => {
      resolveJoin = res;
      rejectJoin = rej;
    });
    // Unhandled until awaited below; keep runtimes from flagging an early rejection.
    answered.catch(() => {});
    const join: JoinState = {
      invite,
      reqSeq,
      settle: (r) => {
        if (this.#join !== join) return;
        if (!r.ok) {
          rejectJoin(new PairError(r.code, r.detail));
          return;
        }
        const machine = this.#openPairReply(join, r.env);
        if (machine === "reject") {
          rejectJoin(new PairError("invite_rejected"));
        } else if (machine) {
          // Finish now, synchronously: the inviter's next frames (presence, intro) must find the paired hub.
          this.#join = null;
          resolveJoin(this.#finishJoin(join, machine, r.env.seq));
        }
        // Anything else from the inviter is not the answer to this request: keep waiting.
      },
    };
    this.#join = join;
    this.#changed();
    let timer: TimerHandle | null = null;
    try {
      this.#started = true;
      if (adopt) {
        // Until pairing succeeds, this relay learns only the inviter (no allow list of other hubs, no hellos).
        this.#restricted = this.#hubs.size === 0;
        this.#relayConfig = target;
      }
      this.#relay.setConfig(this.#relayConfig);
      await this.#waitRelay(this.#t.relayWaitMs);
      this.#relay.syncAllow();
      const env = seal(
        this.#keys,
        this.deviceId,
        invite.machineId,
        invite.publicKey,
        reqSeq,
        { type: "pair_request", code: invite.code, machine: this.#me(), ts: this.#clock.now() },
        this.#random,
        { includePk: true },
      );
      timer = this.#clock.setTimeout(() => join.settle({ ok: false, code: "timeout" }), this.#t.joinTimeoutMs);
      if (!this.#relay.send({ t: "send", to: relayAddress(invite.publicKey), env, pairing: invite.relayToken })) {
        throw new PairError("unreachable", "closed");
      }
      const view = await answered;
      if (adopt) this.#store.put(K_RELAY, { url: target.url, accessKey: target.accessKey });
      await this.#store.flush();
      return view;
    } catch (e) {
      if (adopt) {
        // A relay this join turned on goes back to what it was (off, for a first pairing).
        this.#relayConfig = prev;
        this.#restricted = false;
        this.#relay.setConfig(prev);
      }
      throw e instanceof PairError ? e : new PairError("unreachable", e instanceof Error ? e.message : undefined);
    } finally {
      if (timer !== null) this.#clock.clearTimeout(timer);
      if (this.#join === join) this.#join = null;
      this.#restricted = false;
      this.#relay.syncAllow();
      this.#changed();
    }
  }

  /** Remove one hub from this phone: tell it (bye revoked), forget it, never re-add it from an intro. */
  async removeHub(hubId: MachineId): Promise<void> {
    const rec = this.#hubs.get(hubId);
    if (!rec) return;
    this.#sendBody(rec, { type: "bye", reason: "revoked", ts: this.#clock.now() });
    this.#removeRec(rec, false);
    await this.#store.flush();
  }

  /**
   * Unpair this phone everywhere: bye{revoked} to every hub, then wipe the key, roster and settings and start over
   * with a fresh identity (a new key and device id).
   */
  async unpairPhone(): Promise<void> {
    const ids = [...this.#hubs.keys()];
    // The relay forgets this phone's push token first: nothing should wake a phone that left.
    this.#relay.send({ t: "unregister_push" });
    for (const rec of this.#hubs.values()) this.#sendBody(rec, { type: "bye", reason: "revoked", ts: this.#clock.now() });
    // Let the socket drain the byes before it closes.
    await new Promise<void>((r) => this.#clock.setTimeout(r, 300));
    this.#relay.stop();
    for (const rec of [...this.#hubs.values()]) this.#clearLinkTimers(rec);
    for (const p of this.#intents.values()) this.#clock.clearTimeout(p.timer);
    for (const [, p] of this.#intents) p.reject(new IntentError("closed", p.hubId, p.action));
    this.#intents.clear();
    this.#hubs.clear();
    await this.#store.wipe(ids);
    this.#keys = DeviceKeys.generate(this.#random);
    const secret = this.#keys.exportSecret();
    await this.#store.saveSecret(secret);
    secret.fill(0);
    this.#identity = { ...this.#identity, deviceId: newDeviceId(this.#random), createdAt: this.#clock.now() };
    this.#store.put(K_IDENTITY, this.#identity);
    this.#relayConfig = null;
    this.#revoked = { ids: [], pks: [] };
    // The next identity has its own mailbox; the token is registered there after the next pairing.
    this.#settings = { pushEnabled: this.#settings.pushEnabled, pushToken: this.#settings.pushToken, pushClear: false };
    this.#store.put(K_SETTINGS, this.#settings);
    this.#notices = [];
    this.#relay = this.#makeRelay();
    await this.#store.flush();
    this.#changed();
  }

  // ---------- facade: intents ----------

  /** Send an intent to the hub that owns its target (start: `machineId`). Resolves on `applied`, else IntentError. */
  sendIntent(machineId: MachineId, intent: Intent, opts: { confirmed?: boolean } = {}): Promise<IntentApplied> {
    const action = intent.kind;
    const rec = this.#hubs.get(machineId);
    if (!rec) return Promise.reject(new IntentError("not_found", machineId, action));
    if (intentNeedsConfirmation(intent) && opts.confirmed !== true) {
      return Promise.reject(new IntentError("unconfirmed", machineId, action));
    }
    if (!this.#relay.connected) return Promise.reject(new IntentError("closed", machineId, action));
    if (!rec.link.up) return Promise.reject(new IntentError("offline", machineId, action));
    const intentId = newIntentId(this.#random);
    const body: PeerBody = {
      type: "intent",
      intentId,
      client: "mobile",
      confirmed: opts.confirmed === true,
      intent,
      ts: this.#clock.now(),
    };
    return new Promise<IntentApplied>((resolve, reject) => {
      const timer = this.#clock.setTimeout(() => {
        this.#intents.delete(intentId);
        reject(new IntentError("timeout", machineId, action));
      }, this.#t.intentTimeoutMs);
      this.#intents.set(intentId, { hubId: machineId, resolve, reject, timer, action });
      if (!this.#sendBody(rec, body)) {
        this.#clock.clearTimeout(timer);
        this.#intents.delete(intentId);
        reject(new IntentError("offline", machineId, action));
      }
    });
  }

  /** Approve once (allow_once). `confirmed` must come from a successful biometric / passcode check. */
  approve(requestId: string, opts: { confirmed: boolean }): Promise<IntentApplied> {
    const { machineId, request, problem } = this.#findRequest(requestId, "approve");
    if (problem) return Promise.reject(problem);
    const optionId = approveOption(request!);
    if (!optionId) return Promise.reject(new IntentError("no_allow_once", machineId, "approve"));
    return this.sendIntent(machineId, { kind: "approve", requestId, optionId }, opts);
  }

  /** Deny (reject_once, else no option). Needs no confirmation. */
  deny(requestId: string): Promise<IntentApplied> {
    const { machineId, request, problem } = this.#findRequest(requestId, "deny");
    if (problem) return Promise.reject(problem);
    return this.sendIntent(machineId, { kind: "deny", requestId, optionId: denyOption(request!) });
  }

  prompt(sessionId: string, text: string): Promise<IntentApplied> {
    const machineId = parseGlobalId(sessionId)?.machineId ?? "";
    const t = text.trim();
    if (!t) return Promise.reject(new IntentError("invalid", machineId, "prompt"));
    return this.sendIntent(machineId, { kind: "prompt", sessionId, text: t });
  }

  cancel(sessionId: string): Promise<IntentApplied> {
    const machineId = parseGlobalId(sessionId)?.machineId ?? "";
    return this.sendIntent(machineId, { kind: "cancel", sessionId });
  }

  /** Start an agent on `machineId` in `cwd` (one of its repos). `confirmed` must come from the biometric gate. */
  startSession(args: { machineId: MachineId; agentId: string; cwd: string; prompt?: string; confirmed: boolean }): Promise<IntentApplied> {
    const intent: Intent = { kind: "start", agentId: args.agentId, cwd: args.cwd };
    const p = args.prompt?.trim();
    if (p) intent.prompt = p;
    return this.sendIntent(args.machineId, intent, { confirmed: args.confirmed });
  }

  // ---------- internals: relay ----------

  #makeRelay(): RelayClient {
    return new RelayClient({
      keys: this.#keys,
      socketFactory: this.#socketFactory,
      clock: this.#clock,
      random: this.#random,
      logger: this.#log,
      timings: this.#t.relay,
      refusedRelayKeys: () => {
        const keys = [...this.#hubs.values()].map((r) => r.pk);
        if (this.#join) keys.push(this.#join.invite.publicKey);
        return keys;
      },
      allowList: () => {
        const list: string[] = [];
        if (this.#join) list.push(relayAddress(this.#join.invite.publicKey));
        if (!this.#restricted) for (const r of this.#hubs.values()) list.push(r.address);
        return list;
      },
      pushToken: () => (this.#settings.pushEnabled ? this.#settings.pushToken : null),
      pushClear: () => this.#settings.pushClear,
      onPushCleared: () => {
        this.#settings = { ...this.#settings, pushClear: false };
        this.#store.put(K_SETTINGS, this.#settings);
      },
      onMessage: (from, env) => this.#onMessage(from, env),
      onError: (code, to) => this.#onRelayError(code, to),
      onConnected: () => this.#onRelayConnected(),
      onDisconnected: (reason) => this.#onRelayDisconnected(reason),
      onStatus: (status) => {
        this.#emit({ type: "relay", status });
        this.#changed();
      },
    });
  }

  #waitRelay(ms: number): Promise<void> {
    if (this.#relay.connected) return Promise.resolve();
    return new Promise((resolve, reject) => {
      let done = false;
      const finish = (err?: PairError) => {
        if (done) return;
        done = true;
        this.#clock.clearTimeout(timer);
        off();
        if (err) reject(err);
        else resolve();
      };
      const timer = this.#clock.setTimeout(
        () => finish(new PairError("unreachable", this.#relay.status.lastError ?? "timeout")),
        ms,
      );
      const off = this.onEvent((e) => {
        if (e.type !== "relay") return;
        if (e.status.connected) finish();
        // Credentials the relay refused will not get better by waiting.
        else if (e.status.lastError === "bad_auth" || e.status.lastError === "unsupported_version") {
          finish(new PairError("unreachable", e.status.lastError));
        }
      });
    });
  }

  #onRelayConnected(): void {
    if (this.#restricted) return;
    for (const rec of this.#hubs.values()) {
      if (!rec.link.up) {
        rec.link.attempt = 0;
        this.#hello(rec);
      }
    }
  }

  #onRelayDisconnected(reason: string): void {
    this.#join?.settle({ ok: false, code: "unreachable", detail: reason });
    // Every link is on the relay path: they all go down; hellos go out again on the next session.
    for (const rec of this.#hubs.values()) this.#linkDown(rec, false);
  }

  #onRelayError(code: RelayErrorCode, to?: string): void {
    const pk = parseRelayAddress(to);
    if (code !== "not_allowed" || !pk) {
      this.#log("debug", "relay error", { code });
      return;
    }
    if (this.#join && bytesEqual(this.#join.invite.publicKey, pk)) {
      // The relay refused our pair_request: the pairing slot was used, expired or never existed.
      this.#join.settle({ ok: false, code: "invite_rejected" });
      return;
    }
    const rec = [...this.#hubs.values()].find((r) => bytesEqual(r.pk, pk));
    if (!rec) return;
    // That hub does not (yet) accept our frames: it has not processed its intro, or it removed this phone.
    rec.refusedSince ??= this.#clock.now();
    if (rec.link.up) this.#linkDown(rec, false);
    // 1 s, 2 s, 5 s, then every 30 s (attempt = hellos sent since the link was last up).
    this.#scheduleHello(rec, helloRetryDelay(rec.link.attempt - 1));
    this.#changed();
  }

  // ---------- internals: frames ----------

  #timeSeq(): number {
    return Math.max(0, Math.floor(this.#clock.now())) * 1000;
  }

  /** `max(last_tx + 1, now_ms * 1000)`, persisted. */
  #nextTx(rec: HubRec): number {
    const seq = Math.max(rec.seq.tx + 1, this.#timeSeq());
    rec.seq = { ...rec.seq, tx: seq };
    this.#store.put(seqKey(rec.stored.id), rec.seq);
    return seq;
  }

  #sendBody(rec: HubRec, body: PeerBody): boolean {
    if (!this.#relay.connected) return false;
    const env = seal(this.#keys, this.deviceId, rec.stored.id, rec.pk, this.#nextTx(rec), body, this.#random);
    return this.#relay.send({ t: "send", to: rec.address, env });
  }

  #me(): Machine {
    return { id: this.deviceId, name: this.#identity.name, os: this.#identity.os, lastSeen: this.#clock.now(), online: true };
  }

  #fresh(ts: unknown): boolean {
    return typeof ts === "number" && Number.isFinite(ts) && Math.abs(ts - this.#clock.now()) <= this.#t.tsSkewMs;
  }

  #onMessage(from: string, env: PeerEnvelope): void {
    const fromPk = parseRelayAddress(from);
    if (!fromPk) return;
    if (env.to !== this.deviceId || env.from === this.deviceId || !MACHINE_ID_RE.test(env.from) || this.#keys.isOwn(fromPk)) {
      this.#log("debug", "relay: dropped a misaddressed frame");
      return;
    }
    // Phones never accept pairing requests.
    if (env.pk !== undefined) return;
    const join = this.#join;
    if (join && join.invite.machineId === env.from && bytesEqual(join.invite.publicKey, fromPk)) {
      join.settle({ ok: true, env });
      return;
    }
    const rec = this.#hubs.get(env.from);
    if (!rec) {
      this.#log("debug", "relay: dropped a frame from an unknown device");
      return;
    }
    if (!bytesEqual(rec.pk, fromPk)) {
      this.#log("warn", "relay: dropped a frame whose relay sender is not the hub's roster key", { device: env.from });
      return;
    }
    if (env.seq <= rec.seq.rx) {
      this.#log("debug", "dropped a replayed frame", { device: env.from });
      return;
    }
    const opened = open(this.#keys, env, rec.pk);
    if (!opened.ok) {
      this.#log("debug", "dropped a frame that did not open", { device: env.from, error: opened.error });
      return;
    }
    const body = opened.body;
    if ((body.type === "hello" && !this.#fresh(body.ts)) || (body.type === "presence" && !this.#fresh(body.presence?.ts))) {
      this.#log("debug", "dropped a hello/presence stamped too far from our clock", { device: env.from });
      return;
    }
    rec.seq = { ...rec.seq, rx: env.seq };
    this.#store.put(seqKey(rec.stored.id), rec.seq);
    rec.link.lastAuthAt = this.#clock.now();
    rec.refusedSince = undefined;
    // Write-ahead: the body is acted on only after the new last_rx is stored, so a crash can never leave an
    // acted-on frame whose seq a replay could reuse.
    this.#inbound.push({ rec, body });
    this.#commitInbound();
  }

  /**
   * Group commit of accepted frames: store every queued last_rx (one flush for the whole burst), then act on the
   * bodies in arrival order. Frames that arrive meanwhile wait for the next commit, whose flush includes their seq.
   */
  #commitInbound(): void {
    if (this.#committing || this.#inbound.length === 0) return;
    this.#committing = true;
    const batch = this.#inbound.splice(0);
    void this.#store.flush().then(() => {
      for (const { rec, body } of batch) {
        // Removed or re-paired since: drop.
        if (this.#hubs.get(rec.stored.id) === rec) this.#onBody(rec, body);
      }
      this.#committing = false;
      this.#commitInbound();
    });
  }

  #onBody(rec: HubRec, body: PeerBody): void {
    if (body.type === "bye") {
      if (body.reason === "revoked") {
        this.#removeRec(rec, true);
      } else {
        this.#linkDown(rec, true);
      }
      return;
    }
    if (body.type === "pair_request" || body.type === "pair_accept" || body.type === "pair_reject") return;
    // A hello must name its sender with an allowed name (Rust drops it); the frame is not acted on at all.
    if (body.type === "hello" && !this.#machineOk(rec, body.machine)) {
      this.#log("debug", "dropped a hello for another machine or with a refused name", { device: rec.stored.id });
      return;
    }
    if (!rec.link.up) {
      const answered = rec.link.helloPending;
      if (body.type === "hello") this.#updateMachine(rec, body.machine, body.addr);
      if (body.type === "presence") this.#applyPresence(rec, body.presence);
      this.#linkUp(rec);
      // The hub thinks it is linked but we were not: say hello so it resyncs (unless this answers ours).
      if (!answered && body.type !== "hello") this.#sendHello(rec);
      if (body.type === "hello" || body.type === "presence") return;
    }
    switch (body.type) {
      case "hello":
        // The hub lost its link state (restart, new relay session): answer and resync.
        this.#updateMachine(rec, body.machine, body.addr);
        this.#sendPresence(rec);
        this.#requestSync(rec);
        return;
      case "presence":
        this.#applyPresence(rec, body.presence);
        return;
      case "mirror":
      case "mirror_snapshot":
      case "mirror_events":
      case "sync_done":
        this.#applyMirror(rec, body);
        return;
      case "intent_ack":
        this.#onIntentAck(rec, body);
        return;
      case "roster_intro":
        this.#onRosterIntro(rec, body.devices);
        return;
      case "sync_request":
      case "intent":
        // A phone owns no sessions and accepts no intents.
        return;
      default:
        return;
    }
  }

  #applyMirror(rec: HubRec, body: PeerBody): void {
    const before = rec.mirror;
    const after = applyMirrorBody(before, body, this.#clock.now());
    if (after === before) return;
    rec.mirror = after;
    if (body.type === "mirror") {
      const u = body.update;
      if (u.type === "permission_request" && after.state.pending[u.request.requestId] && !before.state.pending[u.request.requestId]) {
        this.#emit({ type: "permission_request", request: u.request });
      } else if (u.type === "permission_resolved") {
        const res = after.resolutions[0];
        if (res && res.requestId === u.requestId) this.#emit({ type: "permission_resolved", resolution: res });
      }
    }
    this.#changed();
  }

  #onIntentAck(rec: HubRec, body: Extract<PeerBody, { type: "intent_ack" }>): void {
    const p = this.#intents.get(body.intentId);
    if (!p || p.hubId !== rec.stored.id) return;
    this.#intents.delete(body.intentId);
    this.#clock.clearTimeout(p.timer);
    if (body.status === "applied") {
      p.resolve(body.result === undefined ? { intentId: body.intentId } : { intentId: body.intentId, result: body.result });
    } else {
      p.reject(new IntentError(body.reason ?? "invalid", p.hubId, p.action));
    }
  }

  /** The machine a hello / presence names is the sender and its name passes `deviceNameOk`. */
  #machineOk(rec: HubRec, m: Machine | undefined): m is Machine {
    return typeof m === "object" && m !== null && m.id === rec.stored.id && deviceNameOk(m.name);
  }

  #updateMachine(rec: HubRec, m: Machine | undefined, addr?: string): void {
    if (!this.#machineOk(rec, m)) return;
    const name = m.name.trim();
    const os = typeof m.os === "string" ? m.os : rec.machine.os;
    const now = this.#clock.now();
    rec.machine = {
      ...rec.machine,
      name,
      os,
      lastSeen: now,
      ...(typeof m.hubVersion === "string" && m.hubVersion.length <= 40 ? { hubVersion: m.hubVersion } : {}),
      ...(typeof m.userActiveAt === "number" ? { userActiveAt: m.userActiveAt } : {}),
    };
    const stored: StoredHub = { ...rec.stored, name, os, lastSeen: now };
    if (rec.machine.hubVersion) stored.hubVersion = rec.machine.hubVersion;
    void addr;
    const changed = stored.name !== rec.stored.name || stored.os !== rec.stored.os || stored.hubVersion !== rec.stored.hubVersion;
    rec.stored = stored;
    // lastSeen alone is not worth a secure-store write on every heartbeat.
    if (changed) this.#store.put(hubKey(stored.id), stored);
  }

  #applyPresence(rec: HubRec, p: Presence | undefined): void {
    // Presence for another machine or with a refused name is ignored whole (Rust `on_body`).
    if (!p || typeof p !== "object" || !this.#machineOk(rec, p.machine)) return;
    this.#updateMachine(rec, p.machine);
    const id = rec.stored.id;
    rec.agents = (Array.isArray(p.agents) ? p.agents : [])
      .filter((a) => a && a.machineId === id && strOk(a.id, 80) && typeof a.name === "string")
      .slice(0, AGENTS_MAX);
    rec.repos = (Array.isArray(p.repos) ? p.repos : [])
      .filter((r) => r && strOk(r.name, 80) && strOk(r.path, 1024))
      .slice(0, REPO_INDEX_MAX);
    rec.presencePets = (Array.isArray(p.pets) ? p.pets : [])
      .filter((pet) => pet && pet.machineId === id && parseGlobalId(pet.id)?.machineId === id && typeof pet.name === "string")
      .slice(0, MIRROR_MAX_PETS);
    if (typeof p.userActiveAt === "number") rec.machine = { ...rec.machine, userActiveAt: p.userActiveAt };
    this.#changed();
  }

  // ---------- internals: links ----------

  #sendHello(rec: HubRec): boolean {
    rec.link.helloPending = true;
    return this.#sendBody(rec, { type: "hello", machine: this.#me(), ts: this.#clock.now() });
  }

  #hello(rec: HubRec): void {
    if (this.#restricted || !this.#relay.connected) return;
    this.#sendHello(rec);
    rec.link.attempt++;
    // No answer in time (hub offline): try again after the backoff.
    this.#scheduleHello(rec, this.#t.helloAnswerMs + helloRetryDelay(rec.link.attempt - 1));
  }

  #scheduleHello(rec: HubRec, delay: number): void {
    if (rec.link.retry !== null) this.#clock.clearTimeout(rec.link.retry);
    rec.link.retry = this.#clock.setTimeout(() => {
      rec.link.retry = null;
      if (!rec.link.up && this.#hubs.get(rec.stored.id) === rec) this.#hello(rec);
    }, delay);
  }

  #sendPresence(rec: HubRec): void {
    const presence: Presence = { machine: this.#me(), agents: [], repos: [], pets: [], ts: this.#clock.now() };
    this.#sendBody(rec, { type: "presence", presence });
  }

  #requestSync(rec: HubRec): void {
    if (this.#sendBody(rec, { type: "sync_request", sinceSeq: rec.mirror.maxSeq })) {
      rec.mirror = startSync(rec.mirror);
    }
  }

  #linkUp(rec: HubRec): void {
    if (rec.link.up) return;
    const l = rec.link;
    l.up = true;
    l.attempt = 0;
    l.helloPending = false;
    l.lastAuthAt = this.#clock.now();
    if (l.retry !== null) this.#clock.clearTimeout(l.retry);
    l.retry = null;
    rec.machine = { ...rec.machine, online: true, lastSeen: this.#clock.now() };
    this.#armSilence(rec);
    this.#armPresence(rec);
    this.#sendPresence(rec);
    this.#requestSync(rec);
    this.#emit({ type: "link", hubId: rec.stored.id, up: true });
    this.#changed();
  }

  #linkDown(rec: HubRec, retry: boolean): void {
    if (rec.link.up) {
      rec.link.up = false;
      rec.machine = { ...rec.machine, online: false };
      if (rec.mirror.sync === "syncing") rec.mirror = { ...rec.mirror, sync: "idle", version: rec.mirror.version + 1 };
      this.#emit({ type: "link", hubId: rec.stored.id, up: false });
      this.#changed();
    }
    rec.link.helloPending = false;
    if (rec.link.silence !== null) this.#clock.clearTimeout(rec.link.silence);
    if (rec.link.presence !== null) this.#clock.clearTimeout(rec.link.presence);
    rec.link.silence = rec.link.presence = null;
    if (retry && this.#relay.connected) this.#scheduleHello(rec, helloRetryDelay(rec.link.attempt));
  }

  #armSilence(rec: HubRec): void {
    if (rec.link.silence !== null) this.#clock.clearTimeout(rec.link.silence);
    const due = rec.link.lastAuthAt + this.#t.offlineAfterMs - this.#clock.now();
    rec.link.silence = this.#clock.setTimeout(() => {
      rec.link.silence = null;
      if (!rec.link.up) return;
      if (this.#clock.now() - rec.link.lastAuthAt >= this.#t.offlineAfterMs) {
        this.#log("info", "hub offline: no authenticated frame in time", { device: rec.stored.id });
        this.#linkDown(rec, true);
      } else this.#armSilence(rec);
    }, Math.max(50, due));
  }

  #armPresence(rec: HubRec): void {
    if (rec.link.presence !== null) this.#clock.clearTimeout(rec.link.presence);
    rec.link.presence = this.#clock.setTimeout(() => {
      rec.link.presence = null;
      if (!rec.link.up) return;
      this.#sendPresence(rec);
      this.#armPresence(rec);
    }, this.#t.presenceIntervalMs);
  }

  #clearLinkTimers(rec: HubRec): void {
    const l = rec.link;
    for (const h of [l.retry, l.silence, l.presence]) if (h !== null) this.#clock.clearTimeout(h);
    l.retry = l.silence = l.presence = null;
  }

  // ---------- internals: roster ----------

  #addRec(stored: StoredHub, seq: StoredSeq): HubRec | null {
    const pk = decodeKey(stored.publicKey);
    if (!pk || !MACHINE_ID_RE.test(stored.id)) return null;
    const rec: HubRec = {
      stored,
      pk,
      address: relayAddress(pk),
      seq,
      machine: {
        id: stored.id,
        name: stored.name,
        os: stored.os ?? "other",
        lastSeen: stored.lastSeen ?? 0,
        online: false,
        ...(stored.hubVersion ? { hubVersion: stored.hubVersion } : {}),
      },
      agents: [],
      repos: [],
      presencePets: [],
      mirror: emptyMirror(stored.id),
      link: { up: false, lastAuthAt: 0, helloPending: false, attempt: 0, retry: null, silence: null, presence: null },
    };
    this.#hubs.set(stored.id, rec);
    return rec;
  }

  #persistRoster(): void {
    this.#store.putHubs([...this.#hubs.keys()]);
  }

  #removeRec(rec: HubRec, byHub: boolean): void {
    const id = rec.stored.id;
    this.#linkDown(rec, false);
    this.#clearLinkTimers(rec);
    this.#hubs.delete(id);
    this.#store.put(hubKey(id), null);
    this.#store.put(seqKey(id), null);
    this.#persistRoster();
    this.#revoked = {
      ids: [id, ...this.#revoked.ids.filter((x) => x !== id)].slice(0, REVOKED_MAX),
      pks: [rec.stored.publicKey, ...this.#revoked.pks.filter((x) => x !== rec.stored.publicKey)].slice(0, REVOKED_MAX),
    };
    this.#store.put(K_REVOKED, this.#revoked);
    for (const [iid, p] of this.#intents) {
      if (p.hubId !== id) continue;
      this.#clock.clearTimeout(p.timer);
      this.#intents.delete(iid);
      p.reject(new IntentError("not_found", id, p.action));
    }
    this.#relay.syncAllow();
    if (this.#hubs.size === 0 && !this.#join) {
      // Nothing left to reach: the relay can go (the next pairing brings its own).
      this.#relay.stop();
      this.#relayConfig = null;
      this.#store.put(K_RELAY, null);
    }
    this.#notice(byHub ? "warning" : "info", byHub ? `${rec.machine.name} unpaired this phone` : `Removed ${rec.machine.name}`);
    this.#emit({ type: "hub_removed", hubId: id, byHub });
    this.#changed();
  }

  #onRosterIntro(sender: HubRec, devices: IntroDevice[]): void {
    if (!sender.link.up) return;
    const decision = evaluateIntro(
      {
        myId: this.deviceId,
        myPk: this.#keys.publicKey,
        senderId: sender.stored.id,
        senderPk: sender.pk,
        known: new Map([...this.#hubs.values()].map((r) => [r.stored.id, r.pk])),
        revokedIds: new Set(this.#revoked.ids),
        revokedPks: new Set(this.#revoked.pks),
        isLowOrder: (pk) => this.#keys.isLowOrder(pk),
      },
      devices,
    );
    const now = this.#clock.now();
    const added: HubRec[] = [];
    for (const d of decision.accepted) {
      const stored: StoredHub = {
        id: d.id,
        name: d.name,
        publicKey: encode(d.publicKey),
        pairedAt: now,
        introducedBy: sender.stored.id,
        ...(d.os ? { os: d.os } : {}),
        ...(d.addr ? { addr: d.addr } : {}),
      };
      const rec = this.#addRec(stored, { rx: 0, tx: 0 });
      if (!rec) continue;
      this.#store.put(hubKey(d.id), stored);
      this.#store.put(seqKey(d.id), rec.seq);
      added.push(rec);
      this.#notice("success", `${sender.machine.name} added ${d.name}`);
    }
    const conflictText: Partial<Record<IntroSkipReason, string>> = {
      id_conflict: "a different key than the one this phone knows",
      key_conflict: "a key another paired machine already uses",
      revoked: "a machine that was removed from this phone",
    };
    for (const s of decision.skipped) {
      const why = conflictText[s.reason];
      if (why) this.#notice("warning", `${sender.machine.name} introduced ${s.name || s.id} with ${why}; not added`);
    }
    if (added.length === 0) {
      this.#changed();
      return;
    }
    this.#persistRoster();
    this.#relay.syncAllow();
    for (const rec of added) {
      this.#emit({ type: "hub_added", hub: this.#hubView(rec), introducedBy: sender.stored.id });
      this.#hello(rec);
    }
    this.#changed();
  }

  #openPairReply(join: JoinState, env: PeerEnvelope): Machine | "reject" | null {
    const mid = join.invite.machineId;
    if (env.from !== mid || env.to !== this.deviceId || env.pk !== undefined) return null;
    const lastRx = this.#hubs.get(mid)?.seq.rx ?? 0;
    if (env.seq <= lastRx) return null;
    const r = open(this.#keys, env, join.invite.publicKey);
    if (!r.ok) return null;
    if (r.body.type === "pair_reject") return "reject";
    if (r.body.type !== "pair_accept") return null;
    if (r.body.machine?.id !== mid || r.body.reqSeq !== join.reqSeq) return null;
    // An inviter whose name breaks the rule is refused (Rust: PairReply::Reject).
    if (!deviceNameOk(r.body.machine.name)) return "reject";
    return r.body.machine;
  }

  #finishJoin(join: JoinState, machine: Machine, replySeq: number): HubView {
    const inv = join.invite;
    const now = this.#clock.now();
    const old = this.#hubs.get(inv.machineId);
    if (old) {
      this.#clearLinkTimers(old);
      this.#hubs.delete(inv.machineId);
    }
    const pkB64 = encode(inv.publicKey);
    // Pairing by hand is the trust decision: it clears an earlier removal.
    this.#revoked = {
      ids: this.#revoked.ids.filter((x) => x !== inv.machineId),
      pks: this.#revoked.pks.filter((x) => x !== pkB64),
    };
    this.#store.put(K_REVOKED, this.#revoked);
    const stored: StoredHub = {
      id: inv.machineId,
      name: machine.name.trim(),
      publicKey: pkB64,
      pairedAt: now,
      lastSeen: now,
      ...(typeof machine.os === "string" ? { os: machine.os } : {}),
      ...(typeof machine.hubVersion === "string" ? { hubVersion: machine.hubVersion } : {}),
      ...(inv.addr ? { addr: inv.addr } : {}),
    };
    const rec = this.#addRec(stored, { rx: replySeq, tx: join.reqSeq })!;
    if (old) rec.mirror = old.mirror;
    this.#store.put(hubKey(stored.id), stored);
    this.#store.put(seqKey(stored.id), rec.seq);
    this.#persistRoster();
    this.#restricted = false;
    this.#relay.syncAllow();
    // The inviter registered the link when it accepted; its presence follows. Ours goes out now.
    this.#linkUp(rec);
    this.#relay.syncPush();
    const view = this.#hubView(rec);
    this.#notice("success", `Paired with ${stored.name}`);
    this.#emit({ type: "paired", hub: view });
    // Other hubs this phone already knew (a second pairing): say hello if not linked.
    this.#onRelayConnected();
    return view;
  }

  #findRequest(
    requestId: string,
    action: "approve" | "deny",
  ): { machineId: MachineId; request?: PermissionRequest; problem?: IntentError } {
    const machineId = parseGlobalId(requestId)?.machineId ?? "";
    const rec = this.#hubs.get(machineId);
    if (!rec) return { machineId, problem: new IntentError("not_found", machineId, action) };
    const request = rec.mirror.state.pending[requestId];
    if (!request) {
      const decided = rec.mirror.resolutions.some((r) => r.requestId === requestId);
      return { machineId, problem: new IntentError(decided ? "already_decided" : "not_found", machineId, action) };
    }
    return { machineId, request };
  }

  // ---------- internals: snapshot + events ----------

  #notice(level: NoticeLevel, text: string): void {
    const notice: PhoneNotice = { id: newIntentId(this.#random), level, text, ts: this.#clock.now() };
    this.#notices = [notice, ...this.#notices].slice(0, NOTICES_MAX);
    this.#emit({ type: "notice", notice });
  }

  #emit(e: PhoneEvent): void {
    for (const fn of [...this.#eventListeners]) {
      try {
        fn(e);
      } catch {
        // A listener's bug must not break the node.
      }
    }
  }

  #changed(): void {
    this.#version++;
    this.#snapshot = null;
    if (this.#notifyQueued) return;
    this.#notifyQueued = true;
    void Promise.resolve().then(() => {
      this.#notifyQueued = false;
      if (this.#listeners.size === 0) return;
      const snap = this.getSnapshot();
      for (const fn of [...this.#listeners]) {
        try {
          fn(snap);
        } catch {
          // ignore listener errors
        }
      }
    });
  }

  #hubView(rec: HubRec): HubView {
    const v: HubView = {
      id: rec.stored.id,
      name: rec.machine.name,
      online: rec.link.up,
      path: rec.link.up ? "relay" : null,
      pairedAt: rec.stored.pairedAt,
      sync: rec.mirror.sync,
      truncated: rec.mirror.truncated,
    };
    if (rec.machine.os) v.os = rec.machine.os;
    if (rec.machine.hubVersion) v.hubVersion = rec.machine.hubVersion;
    if (rec.machine.lastSeen) v.lastSeen = rec.machine.lastSeen;
    if (rec.stored.introducedBy) v.introducedBy = rec.stored.introducedBy;
    if (rec.machine.userActiveAt) v.userActiveAt = rec.machine.userActiveAt;
    if (rec.refusedSince !== undefined) v.refusedSince = rec.refusedSince;
    return v;
  }

  #buildSnapshot(): PhoneSnapshot {
    const hubs: HubView[] = [];
    const machines: Record<MachineId, Machine> = {};
    const sessions: Record<string, Session> = {};
    const pets: Record<string, Pet> = {};
    const pending: Record<string, PermissionRequest> = {};
    const events: Record<string, HubEvent[]> = {};
    const agents: Record<MachineId, Agent[]> = {};
    const repos: Record<MachineId, RepoRef[]> = {};
    let resolutions: Resolution[] = [];
    for (const rec of this.#hubs.values()) {
      hubs.push(this.#hubView(rec));
      machines[rec.stored.id] = { ...rec.machine, online: rec.link.up };
      const st = rec.mirror.state;
      Object.assign(sessions, st.sessions);
      for (const p of rec.presencePets) pets[p.id] = p;
      Object.assign(pets, st.pets);
      Object.assign(pending, st.pending);
      Object.assign(events, st.events);
      agents[rec.stored.id] = rec.agents;
      repos[rec.stored.id] = rec.repos;
      resolutions = resolutions.concat(rec.mirror.resolutions);
    }
    hubs.sort((a, b) => Number(b.online) - Number(a.online) || a.name.localeCompare(b.name) || (a.id < b.id ? -1 : 1));
    resolutions.sort((a, b) => b.at - a.at);
    return {
      version: this.#version,
      deviceId: this.deviceId,
      name: this.#identity.name,
      os: this.#identity.os as PhoneOs,
      publicKey: this.#keys.publicB64,
      paired: this.#hubs.size > 0,
      joining: this.#join !== null,
      relay: this.#relay.status,
      hubs,
      machines,
      sessions,
      pets,
      pending,
      events,
      agents,
      repos,
      resolutions,
      notices: this.#notices,
      pushEnabled: this.#settings.pushEnabled,
      pushToken: this.#settings.pushToken,
    };
  }
}
