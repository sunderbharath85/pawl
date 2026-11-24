// HubClient: framework-free WebSocket client for the Krypto hub.
// Works with the browser/webview `WebSocket`, Node's global `WebSocket` (Node 22+) and the `ws` package
// (pass it as `WebSocketImpl`). No DOM, Node or React imports here.
import type {
  AckResults,
  Agent,
  ChatMessage,
  ClientKind,
  ClientMsg,
  HistorySession,
  HubEvent,
  Machine,
  Pet,
  PermissionRequest,
  Rule,
  ServerMsg,
  Session,
  Welcome,
} from "@krypto/protocol";
import { USER_ACTIVITY_REPORT_MS } from "@krypto/protocol";
import {
  applyAck,
  applyWelcome,
  DEFAULT_LIMITS,
  initialHubState,
  reduceServerMsg,
  type ClientRequest,
  type DistributiveOmit,
  type HubConnectionStatus,
  type HubState,
  type StateLimits,
} from "./state";
import { MACHINE_NAME_MAX, normalizeMachineName } from "./machine";
import { INVITE_TTL_MAX_SECS, INVITE_TTL_MIN_SECS, parseInviteUri } from "./peer";
import { toHubWsUrl } from "./url";

export interface HubClientOptions {
  /** Hub WebSocket URL. Default `ws://127.0.0.1:47800/ws`. `http(s)://host:port` is accepted and normalized. */
  url?: string | undefined;
  token: string;
  client: ClientKind;
  clientVersion: string;
  /** WebSocket constructor. Defaults to `globalThis.WebSocket`; pass the `ws` package's default export in older Node. */
  WebSocketImpl?: any | undefined;
  /** Reconnect automatically with exponential backoff (0.5 s → 10 s). Default true. */
  reconnect?: boolean | undefined;
  /** Per-request ack timeout. Default 15 000 ms. */
  requestTimeoutMs?: number | undefined;
  /** Backoff bounds. Defaults 500 / 10 000 ms. */
  minReconnectDelayMs?: number | undefined;
  maxReconnectDelayMs?: number | undefined;
  /** Max events kept per session in state. Default 500. */
  eventCap?: number | undefined;
  /** Max chat messages / notifications kept in state. Defaults 300 / 100. */
  chatCap?: number | undefined;
  notificationCap?: number | undefined;
}

/** Error thrown by `HubClient.request` and helpers. `code` is the hub's `ack.error`, or a client code. */
export class HubRequestError extends Error {
  readonly code: string;
  readonly requestType: string;
  constructor(code: string, requestType: string, message?: string) {
    super(message ?? `hub request "${requestType}" failed: ${code}`);
    this.name = "HubRequestError";
    this.code = code;
    this.requestType = requestType;
  }
}

/** Minimal structural WebSocket surface used by the client (browser WebSocket and `ws` both satisfy it). */
interface SocketLike {
  readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: ((ev: { code?: number; reason?: string }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
}

type SocketCtor = new (url: string) => SocketLike;

const WS_OPEN = 1;

interface PendingRequest {
  id: string;
  request: ClientRequest;
  frame: string;
  sent: boolean;
  timer: ReturnType<typeof setTimeout>;
  resolve(result: unknown): void;
  reject(err: Error): void;
}

/** Per-call request options. */
export interface RequestOptions {
  /** Ack timeout for this request; defaults to the client's `requestTimeoutMs`. */
  timeoutMs?: number | undefined;
}

/**
 * Client-side wait for a `join_peer` ack. The hub gives up dialing after 10 s and then answers
 * `unreachable` / `timeout` itself; this only has to outlast that.
 */
export const JOIN_PEER_TIMEOUT_MS = 30_000;

type StateListener = (state: HubState, msg?: ServerMsg) => void;
type AnyMsgListener = (msg: ServerMsg) => void;

const textDecoder = typeof TextDecoder !== "undefined" ? new TextDecoder() : null;

function frameToString(data: unknown): string | null {
  if (typeof data === "string") return data;
  if (!textDecoder) return null;
  if (data instanceof ArrayBuffer) return textDecoder.decode(new Uint8Array(data));
  if (ArrayBuffer.isView(data)) {
    return textDecoder.decode(new Uint8Array(data.buffer, data.byteOffset, data.byteLength));
  }
  if (Array.isArray(data) && data.every((d) => ArrayBuffer.isView(d))) {
    // `ws` fragmented binary messages (Buffer[]).
    return data.map((d) => frameToString(d) ?? "").join("");
  }
  return null;
}

function randomPrefix(): string {
  return Math.random().toString(36).slice(2, 8);
}

function safeCall(fn: () => void): void {
  try {
    fn();
  } catch (err) {
    // A throwing listener must never break the message loop.
    console.error("[hub-client] listener threw", err);
  }
}

export class HubClient {
  private opts: HubClientOptions & {
    reconnect: boolean;
    requestTimeoutMs: number;
    minReconnectDelayMs: number;
    maxReconnectDelayMs: number;
  };
  private readonly url: string;
  private readonly limits: StateLimits;
  private state: HubState = initialHubState();
  private ws: SocketLike | null = null;
  /** True between connect() and close(): the client wants to be connected. */
  private active = false;
  private handshakeDone = false;
  private helloId: string | null = null;
  private helloTimer: ReturnType<typeof setTimeout> | null = null;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private attempts = 0;
  private seq = 0;
  private readonly idPrefix = randomPrefix();
  private readonly pending = new Map<string, PendingRequest>();
  private readonly stateListeners = new Set<StateListener>();
  private readonly typeListeners = new Map<string, Set<AnyMsgListener>>();
  /** When `user_activity` was last sent on the current connection (reset by every hello). */
  private lastActivityReport = -Infinity;

  constructor(opts: HubClientOptions) {
    // `??` (not spread) so an explicit `undefined` still gets the default.
    this.opts = {
      ...opts,
      reconnect: opts.reconnect ?? true,
      requestTimeoutMs: opts.requestTimeoutMs ?? 15_000,
      minReconnectDelayMs: opts.minReconnectDelayMs ?? 500,
      maxReconnectDelayMs: opts.maxReconnectDelayMs ?? 10_000,
    };
    this.url = toHubWsUrl(opts.url);
    this.limits = {
      eventCap: opts.eventCap ?? DEFAULT_LIMITS.eventCap,
      chatCap: opts.chatCap ?? DEFAULT_LIMITS.chatCap,
      notificationCap: opts.notificationCap ?? DEFAULT_LIMITS.notificationCap,
    };
  }

  // ---------------------------------------------------------------- lifecycle

  /** Open the connection (idempotent). Also resumes after `close()` or an `unauthorized` stop. */
  connect(): void {
    this.active = true;
    if (this.ws) return;
    this.clearReconnectTimer();
    this.openSocket();
  }

  /** Close and stop reconnecting. Rejects every outstanding request. */
  close(): void {
    this.active = false;
    this.clearReconnectTimer();
    const ws = this.ws;
    this.detachSocket();
    if (ws) {
      try {
        ws.close(1000, "client closed");
      } catch {
        /* already closing */
      }
    }
    this.rejectPending(() => true, "closed", "hub client closed");
    this.setStatus("disconnected");
  }

  /** Replace the auth token (e.g. after the hub regenerated it). Reconnects if currently active. */
  setToken(token: string): void {
    this.opts = { ...this.opts, token };
    if (this.active || this.state.status === "unauthorized") {
      this.close();
      this.connect();
    }
  }

  /** The resolved WebSocket URL this client dials. */
  get wsUrl(): string {
    return this.url;
  }

  // ---------------------------------------------------------------- state

  /** Immutable snapshot; a new object identity after every change (safe for useSyncExternalStore). */
  getState(): HubState {
    return this.state;
  }

  /** Called after every state change and every server message. Returns an unsubscribe function. */
  subscribe(fn: (state: HubState, msg?: ServerMsg) => void): () => void {
    this.stateListeners.add(fn);
    return () => {
      this.stateListeners.delete(fn);
    };
  }

  /** Typed listener for one server message type (after the state reducer ran). */
  onMessage<T extends ServerMsg["type"]>(type: T, fn: (msg: Extract<ServerMsg, { type: T }>) => void): () => void {
    let set = this.typeListeners.get(type);
    if (!set) {
      set = new Set();
      this.typeListeners.set(type, set);
    }
    const listener = fn as AnyMsgListener;
    set.add(listener);
    return () => {
      set!.delete(listener);
    };
  }

  // ---------------------------------------------------------------- requests

  /**
   * Send a request and resolve with its `ack.result`. Rejects on `ok:false` (code = hub error),
   * on timeout (`timeout`, default 15 s), on disconnect after send (`disconnected`), when closed (`closed`)
   * or unauthorized (`unauthorized`). Requests made while (re)connecting are queued until the hello completes.
   */
  request<T extends keyof AckResults>(
    msg: DistributiveOmit<Extract<ClientMsg, { type: T }>, "id">,
    options?: RequestOptions,
  ): Promise<AckResults[T]> {
    const request = msg as unknown as ClientRequest;
    return new Promise<AckResults[T]>((resolve, reject) => {
      if (this.state.status === "unauthorized") {
        reject(new HubRequestError("unauthorized", request.type, "hub rejected the token (unauthorized)"));
        return;
      }
      if (!this.active) {
        reject(new HubRequestError("closed", request.type, "hub client is not connected; call connect() first"));
        return;
      }
      const id = `${this.idPrefix}-${++this.seq}`;
      const frame = JSON.stringify({ ...request, id });
      const entry: PendingRequest = {
        id,
        request,
        frame,
        sent: false,
        timer: setTimeout(() => {
          if (this.pending.delete(id)) {
            reject(new HubRequestError("timeout", request.type, `hub request "${request.type}" timed out`));
          }
        }, options?.timeoutMs ?? this.opts.requestTimeoutMs),
        resolve: resolve as (r: unknown) => void,
        reject,
      };
      this.pending.set(id, entry);
      if (this.isReady()) this.sendEntry(entry);
    });
  }

  async startSession(agentId: string, cwd: string, prompt?: string): Promise<Session> {
    const res = await this.request<"start_session">(
      prompt === undefined ? { type: "start_session", agentId, cwd } : { type: "start_session", agentId, cwd, prompt },
    );
    return res.session;
  }

  async prompt(sessionId: string, text: string): Promise<void> {
    await this.request<"prompt">({ type: "prompt", sessionId, text });
  }

  async cancel(sessionId: string): Promise<void> {
    await this.request<"cancel">({ type: "cancel", sessionId });
  }

  async resolvePermission(requestId: string, optionId: string | null): Promise<void> {
    await this.request<"resolve_permission">({ type: "resolve_permission", requestId, optionId });
  }

  /** Approve with the request's `allow_once` option (falls back to `allow_always`). */
  async approve(requestId: string): Promise<void> {
    const req = this.requirePending(requestId, "resolve_permission");
    const option = pickPermissionOption(req, "allow");
    if (!option) throw new HubRequestError("no_allow_option", "resolve_permission", `permission request ${requestId} has no allow option`);
    await this.resolvePermission(requestId, option);
  }

  /** Deny with the request's `reject_once` option, else cancel (optionId null). */
  async deny(requestId: string): Promise<void> {
    const req = this.state.pending[requestId];
    const option = req ? pickPermissionOption(req, "reject") : null;
    await this.resolvePermission(requestId, option);
  }

  /** Send a chat line; resolves with the stored user message (the pet's reply arrives as a `chat` message). */
  async chat(text: string, petId?: string): Promise<ChatMessage> {
    const res = await this.request<"chat">(petId === undefined ? { type: "chat", text } : { type: "chat", text, petId });
    return res.message;
  }

  async updatePet(petId: string, patch: Partial<Pick<Pet, "species" | "name" | "skin" | "personality">>): Promise<Pet> {
    const res = await this.request<"update_pet">({ type: "update_pet", petId, patch });
    return res.pet;
  }

  async listAgents(refresh?: boolean): Promise<Agent[]> {
    const res = await this.request<"list_agents">(refresh === undefined ? { type: "list_agents" } : { type: "list_agents", refresh });
    return res.agents;
  }

  async listSessions(includeEnded?: boolean): Promise<Session[]> {
    const res = await this.request<"list_sessions">(
      includeEnded === undefined ? { type: "list_sessions" } : { type: "list_sessions", includeEnded },
    );
    return res.sessions;
  }

  /**
   * Fetch (older/missed) events of a session; they are also merged into `state.events` (deduped by id).
   * `sinceSeq` is the `seq` cursor: only events with a greater `seq` are returned (see `latestEventSeq`).
   */
  async getEvents(sessionId: string, sinceSeq?: number, limit?: number): Promise<HubEvent[]> {
    const msg: DistributiveOmit<Extract<ClientMsg, { type: "get_events" }>, "id"> = { type: "get_events", sessionId };
    if (sinceSeq !== undefined) msg.sinceSeq = sinceSeq;
    if (limit !== undefined) msg.limit = limit;
    const res = await this.request<"get_events">(msg);
    return res.events;
  }

  async listHistory(agentId: string, cwd?: string): Promise<HistorySession[]> {
    const res = await this.request<"list_history">(cwd === undefined ? { type: "list_history", agentId } : { type: "list_history", agentId, cwd });
    return res.sessions;
  }

  async resumeSession(agentId: string, acpSessionId: string, cwd: string): Promise<Session> {
    const res = await this.request<"resume_session">({ type: "resume_session", agentId, acpSessionId, cwd });
    return res.session;
  }

  async setRules(rules: Rule[]): Promise<Rule[]> {
    const res = await this.request<"set_rules">({ type: "set_rules", rules });
    return res.rules;
  }

  /**
   * Rename this hub's machine (the local one from the Welcome). The name is trimmed and must be
   * 1–40 characters (else rejects with code `invalid` without contacting the hub). If the client has
   * not completed a hello yet, waits for it (same timeout as a request). Resolves with the updated machine,
   * which is also folded into `state.machine` / `state.machines`.
   */
  async updateMachine(name: string): Promise<Machine> {
    const trimmed = normalizeMachineName(name);
    if (trimmed === null) {
      throw new HubRequestError("invalid", "update_machine", `machine name must be 1–${MACHINE_NAME_MAX} characters`);
    }
    const machineId = await this.localMachineId("update_machine");
    const res = await this.request<"update_machine">({ type: "update_machine", machineId, name: trimmed });
    return res.machine;
  }

  /**
   * Create a one-time pairing invite on this hub (needs its peer listener on). `ttlSecs` must be an
   * integer in 60..3600 (default 600 on the hub) and `addr` overrides the advertised "host:port";
   * both are checked locally first (code `invalid`). Hub errors: `peer_listener_off`, `invalid`.
   * The URI holds a one-time secret: show it, never log or store it.
   */
  async createInvite(opts: { ttlSecs?: number | undefined; addr?: string | undefined } = {}): Promise<AckResults["create_invite"]> {
    const msg: DistributiveOmit<Extract<ClientMsg, { type: "create_invite" }>, "id"> = { type: "create_invite" };
    if (opts.ttlSecs !== undefined) {
      const ttl = opts.ttlSecs;
      if (!Number.isInteger(ttl) || ttl < INVITE_TTL_MIN_SECS || ttl > INVITE_TTL_MAX_SECS) {
        throw new HubRequestError("invalid", "create_invite", `invite lifetime must be ${INVITE_TTL_MIN_SECS}–${INVITE_TTL_MAX_SECS} seconds`);
      }
      msg.ttlSecs = ttl;
    }
    const addr = opts.addr?.trim();
    if (addr) {
      if (/\s/.test(addr)) throw new HubRequestError("invalid", "create_invite", "invite address must be host:port");
      msg.addr = addr;
    }
    return this.request<"create_invite">(msg);
  }

  /**
   * Pair this hub with another one from its invite URI. Rejects locally with `bad_uri` (not an invite)
   * or `self_invite` (made by this hub) without contacting the hub. Hub errors: `bad_uri`, `self_invite`,
   * `unreachable`, `invite_rejected`, `timeout`. Resolves with the new device and its machine, which are
   * also folded into `state.devices` / `state.machines`.
   */
  async joinPeer(uri: string): Promise<AckResults["join_peer"]> {
    const trimmed = uri.trim();
    const info = parseInviteUri(trimmed);
    if (!info) throw new HubRequestError("bad_uri", "join_peer", "not a Krypto pairing invite");
    if (info.machineId === this.state.machine?.id) {
      throw new HubRequestError("self_invite", "join_peer", "the invite was created by this hub");
    }
    return this.request<"join_peer">({ type: "join_peer", uri: trimmed }, { timeoutMs: Math.max(JOIN_PEER_TIMEOUT_MS, this.opts.requestTimeoutMs) });
  }

  /** Unpair a device (its links close and it can no longer connect; the peer is told). Hub error: `not_found`. */
  async revokeDevice(deviceId: string): Promise<void> {
    await this.request<"revoke_device">({ type: "revoke_device", deviceId });
  }

  /**
   * Tell the hub the user is active on this machine (cursor, keys, editor focus…). Call it on every
   * input; it sends `user_activity` at most once per USER_ACTIVITY_REPORT_MS, and only while connected
   * (nothing is queued for later). Returns true if a report was sent.
   */
  reportUserActivity(now: number = Date.now()): boolean {
    if (!this.isReady() || now - this.lastActivityReport < USER_ACTIVITY_REPORT_MS) return false;
    this.lastActivityReport = now;
    this.request<"user_activity">({ type: "user_activity" }).catch(() => {
      // Older hubs answer with an error; activity is best effort.
    });
    return true;
  }

  // ---------------------------------------------------------------- internals

  /** This hub's machine id, waiting for the hello `Welcome` if it has not arrived yet. */
  private localMachineId(type: string): Promise<string> {
    const known = this.state.machine?.id;
    if (known) return Promise.resolve(known);
    return new Promise<string>((resolve, reject) => {
      const fail = (code: string, message: string) => {
        cleanup();
        reject(new HubRequestError(code, type, message));
      };
      const check = (state: HubState) => {
        if (state.machine?.id) {
          cleanup();
          resolve(state.machine.id);
        } else if (state.status === "unauthorized") {
          fail("unauthorized", "hub rejected the token (unauthorized)");
        } else if (!this.active) {
          fail("closed", "hub client is not connected; call connect() first");
        } else if (state.status === "connected") {
          // Connected to a hub whose Welcome carries no machine (pre-v2 hub).
          fail("no_machine", "hub did not report its machine (protocol v2 required)");
        }
      };
      const timer = setTimeout(() => fail("timeout", `hub request "${type}" timed out waiting for the hub's machine`), this.opts.requestTimeoutMs);
      const unsubscribe = this.subscribe((state) => check(state));
      function cleanup(): void {
        clearTimeout(timer);
        unsubscribe();
      }
      check(this.state);
    });
  }

  private requirePending(requestId: string, type: string): PermissionRequest {
    const req = this.state.pending[requestId];
    if (!req) throw new HubRequestError("unknown_request", type, `no pending permission request ${requestId}`);
    return req;
  }

  private isReady(): boolean {
    return this.handshakeDone && this.ws !== null && this.ws.readyState === WS_OPEN;
  }

  private sendEntry(entry: PendingRequest): void {
    try {
      this.ws!.send(entry.frame);
      entry.sent = true;
    } catch (err) {
      // Socket died between the readiness check and send: leave it queued for the next connection.
      this.state = { ...this.state, lastError: errorText(err) };
    }
  }

  private resolveCtor(): SocketCtor | null {
    const impl = this.opts.WebSocketImpl ?? (globalThis as { WebSocket?: unknown }).WebSocket;
    return typeof impl === "function" ? (impl as SocketCtor) : null;
  }

  private openSocket(): void {
    const Ctor = this.resolveCtor();
    if (!Ctor) {
      this.active = false;
      this.state = { ...this.state, lastError: "no WebSocket implementation available (pass WebSocketImpl)" };
      this.setStatus("disconnected", true);
      this.rejectPending(() => true, "no_websocket", "no WebSocket implementation available");
      return;
    }
    this.setStatus("connecting");
    let ws: SocketLike;
    try {
      ws = new Ctor(this.url);
    } catch (err) {
      this.state = { ...this.state, lastError: errorText(err) };
      this.onDisconnected();
      return;
    }
    this.ws = ws;
    this.handshakeDone = false;
    ws.onopen = () => {
      if (this.ws !== ws) return;
      this.sendHello(ws);
    };
    ws.onmessage = (ev) => {
      if (this.ws !== ws) return;
      this.handleFrame(ev.data);
    };
    ws.onerror = (ev) => {
      if (this.ws !== ws) return;
      const message = (ev as { message?: unknown } | null)?.message;
      this.state = { ...this.state, lastError: typeof message === "string" && message ? message : "websocket error" };
    };
    ws.onclose = (ev) => {
      if (this.ws !== ws) return;
      this.detachSocket();
      if (!this.handshakeDone && ev && typeof ev.code === "number" && ev.code !== 1000 && ev.code !== 1005) {
        this.state = { ...this.state, lastError: this.state.lastError ?? `closed (${ev.code}${ev.reason ? `: ${ev.reason}` : ""})` };
      }
      this.onDisconnected();
    };
  }

  private sendHello(ws: SocketLike): void {
    const id = `${this.idPrefix}-hello-${++this.seq}`;
    this.helloId = id;
    const hello: Extract<ClientMsg, { type: "hello" }> = {
      type: "hello",
      id,
      client: this.opts.client,
      clientVersion: this.opts.clientVersion,
      token: this.opts.token,
    };
    try {
      ws.send(JSON.stringify(hello));
    } catch (err) {
      this.state = { ...this.state, lastError: errorText(err) };
      this.dropSocket();
      return;
    }
    this.clearHelloTimer();
    this.helloTimer = setTimeout(() => {
      if (this.ws !== ws || this.handshakeDone) return;
      this.state = { ...this.state, lastError: "hello handshake timed out" };
      this.dropSocket();
    }, this.opts.requestTimeoutMs);
  }

  private handleFrame(data: unknown): void {
    const text = frameToString(data);
    if (text == null) return;
    let msg: ServerMsg;
    try {
      msg = JSON.parse(text) as ServerMsg;
    } catch {
      return; // not JSON: ignore
    }
    if (!msg || typeof msg !== "object" || typeof (msg as { type?: unknown }).type !== "string") return;

    if (msg.type === "ack") {
      if (msg.id === this.helloId) {
        this.handleHelloAck(msg);
        return;
      }
      const entry = this.pending.get(msg.id);
      if (entry) {
        this.pending.delete(msg.id);
        clearTimeout(entry.timer);
        if (msg.ok) {
          const next = applyAck(this.state, entry.request, msg.result, this.limits);
          this.commit(next, msg);
          entry.resolve(msg.result ?? {});
        } else {
          this.commit(this.state, msg);
          entry.reject(new HubRequestError(msg.error ?? "error", entry.request.type));
        }
        return;
      }
      this.commit(this.state, msg);
      return;
    }

    this.commit(reduceServerMsg(this.state, msg, this.limits), msg);
  }

  private handleHelloAck(msg: Extract<ServerMsg, { type: "ack" }>): void {
    this.helloId = null;
    this.clearHelloTimer();
    if (msg.ok) {
      this.handshakeDone = true;
      this.attempts = 0;
      // A fresh connection reports the next input at once.
      this.lastActivityReport = -Infinity;
      const next = msg.result
        ? applyWelcome(this.state, msg.result as Welcome)
        : { ...this.state, status: "connected" as const, lastError: undefined };
      this.commit(next, msg);
      this.flushQueue();
      return;
    }
    const error = msg.error ?? "handshake failed";
    if (error === "unauthorized") {
      // Bad token: stop reconnecting until connect()/setToken() is called again.
      this.active = false;
      this.state = { ...this.state, lastError: "unauthorized" };
      const ws = this.ws;
      this.detachSocket();
      try {
        ws?.close(1000, "unauthorized");
      } catch {
        /* ignore */
      }
      this.clearReconnectTimer();
      this.rejectPending(() => true, "unauthorized", "hub rejected the token (unauthorized)");
      this.setStatus("unauthorized", true, msg);
      return;
    }
    this.state = { ...this.state, lastError: error };
    this.commit(this.state, msg);
    this.dropSocket();
  }

  private flushQueue(): void {
    for (const entry of this.pending.values()) {
      if (!entry.sent && this.isReady()) this.sendEntry(entry);
    }
  }

  /** Close the current socket ourselves and run disconnect handling. */
  private dropSocket(): void {
    const ws = this.ws;
    this.detachSocket();
    if (ws) {
      try {
        ws.close();
      } catch {
        /* ignore */
      }
    }
    this.onDisconnected();
  }

  private detachSocket(): void {
    const ws = this.ws;
    this.ws = null;
    this.handshakeDone = false;
    this.helloId = null;
    this.clearHelloTimer();
    if (ws) {
      ws.onopen = null;
      ws.onmessage = null;
      ws.onclose = null;
      // Keep a no-op error handler: `ws` (Node) throws on unhandled 'error' during close.
      ws.onerror = () => {};
    }
  }

  private onDisconnected(): void {
    // In-flight requests may or may not have been processed; fail them. Unsent ones stay queued.
    this.rejectPending((e) => e.sent, "disconnected", "hub connection lost");
    if (this.state.status === "unauthorized") return;
    this.setStatus("disconnected");
    if (this.active && this.opts.reconnect) {
      this.scheduleReconnect();
    } else {
      this.active = false;
      this.rejectPending(() => true, "disconnected", "hub connection lost");
    }
  }

  private scheduleReconnect(): void {
    this.clearReconnectTimer();
    const base = Math.min(this.opts.maxReconnectDelayMs, this.opts.minReconnectDelayMs * 2 ** this.attempts);
    const jitter = base * 0.2 * (Math.random() * 2 - 1);
    const delay = Math.max(0, Math.round(Math.min(this.opts.maxReconnectDelayMs, base + jitter)));
    this.attempts = Math.min(this.attempts + 1, 30);
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      if (this.active && !this.ws) this.openSocket();
    }, delay);
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
  }

  private clearHelloTimer(): void {
    if (this.helloTimer) {
      clearTimeout(this.helloTimer);
      this.helloTimer = null;
    }
  }

  private rejectPending(filter: (e: PendingRequest) => boolean, code: string, message: string): void {
    for (const [id, entry] of [...this.pending]) {
      if (!filter(entry)) continue;
      this.pending.delete(id);
      clearTimeout(entry.timer);
      entry.reject(new HubRequestError(code, entry.request.type, message));
    }
  }

  private setStatus(status: HubConnectionStatus, force = false, msg?: ServerMsg): void {
    if (!force && this.state.status === status) return;
    this.commit({ ...this.state, status }, msg, true);
  }

  /** Store the next state and notify. Subscribers fire on every message, typed listeners per type. */
  private commit(next: HubState, msg?: ServerMsg, changed = next !== this.state): void {
    this.state = next;
    if (!msg && !changed) return;
    for (const fn of [...this.stateListeners]) safeCall(() => fn(this.state, msg));
    if (msg) {
      const set = this.typeListeners.get(msg.type);
      if (set) for (const fn of [...set]) safeCall(() => fn(msg));
    }
  }
}

/** Option id for an approve/deny decision: allow → allow_once ?? allow_always; reject → reject_once ?? null. */
export function pickPermissionOption(req: Pick<PermissionRequest, "options">, decision: "allow" | "reject"): string | null {
  const opts = req.options ?? [];
  if (decision === "allow") {
    return (opts.find((o) => o.kind === "allow_once") ?? opts.find((o) => o.kind === "allow_always"))?.optionId ?? null;
  }
  return opts.find((o) => o.kind === "reject_once")?.optionId ?? null;
}

function errorText(err: unknown): string {
  if (err instanceof Error) return err.message;
  return String(err);
}
