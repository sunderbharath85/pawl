// The relay client: one outbound WebSocket to `{url}/v1/connect?pk=<our address>&k=<access key>`, authenticated
// with the device key (challenge -> auth -> ok), then `allow` (who may send to our mailbox), `register_push`,
// `send` and `ping`. Mirrors crates/peer/src/node/relay.rs and services/relay. Reconnects with backoff
// (1 s -> 60 s, jitter); a socket replaced by a newer one with our key (close 4000) waits the maximum.
import {
  RELAY_FRAME_MAX_BYTES,
  RELAY_PROTOCOL_VERSION,
  type PeerEnvelope,
  type RelayClientFrame,
  type RelayErrorCode,
} from "@pawl/protocol";
import { RELAY_AUTH_CONTEXT } from "@pawl/protocol";
import { bytesEqual, encode, tryDecode, utf8Encode } from "./b64";
import { parseEnvelope, NONCE_RANDOM_LEN } from "./envelope";
import { NONCE_BYTES, type DeviceKeys } from "./keys";
import { relayConnectUrl, relayHostHeader } from "./net";
import { SOCKET_OPEN, silentLogger, type Clock, type Logger, type RandomBytes, type SocketFactory, type SocketLike, type TimerHandle } from "./types";

export interface RelayConfig {
  /** Normalized (`normalizeRelayUrl`). */
  url: string;
  /** Secret: never logged. */
  accessKey: string | null;
}

/** `lastError`: unreachable | timeout | bad_auth | bad_frame | unsupported_version | replaced | closed. */
export interface RelayStatusInfo {
  url: string | null;
  connected: boolean;
  lastError?: string;
}

export interface RelayTimings {
  /** Challenge and ok must each arrive within this. */
  authWaitMs: number;
  pingIntervalMs: number;
  /** No frame from the relay for this long: the socket is dead. */
  silentAfterMs: number;
  backoffMinMs: number;
  backoffMaxMs: number;
}

export const DEFAULT_RELAY_TIMINGS: RelayTimings = {
  authWaitMs: 10_000,
  pingIntervalMs: 15_000,
  silentAfterMs: 45_000,
  backoffMinMs: 1_000,
  backoffMaxMs: 60_000,
};

/** Close code of the relay when a newer authenticated socket took our mailbox. */
export const CLOSE_REPLACED = 4000;
/** Close code of the relay for a failed or late auth. */
export const CLOSE_POLICY = 1008;
const INBOUND_MAX_CHARS = RELAY_FRAME_MAX_BYTES + 4096;

export type AuthAnswer = { ok: true; box: string } | { ok: false; error: "bad_frame" | "bad_auth" };

/**
 * The `auth` answer to a relay challenge: NaCl box of `${RELAY_AUTH_CONTEXT}:${nonce}:${host}` under the challenge
 * nonce, from our key to the relay's per-connection key (standard base64). The box uses our long-term key with a
 * relay-chosen key and nonce, so a hostile relay could pick a peer's key and an envelope's nonce to learn its
 * keystream. Envelope nonces carry 0/1 at byte 15; a challenge must have >= 2 there, and its key may be neither ours
 * nor any peer's (`refused`), else `bad_auth`. Malformed fields: `bad_frame`.
 */
export function authAnswer(keys: DeviceKeys, nonce: string, relayPk: string, host: string, refused: readonly Uint8Array[] = []): AuthAnswer {
  const n = tryDecode(nonce);
  const pk = tryDecode(relayPk);
  if (!n || n.length !== NONCE_BYTES || !pk || pk.length !== 32) return { ok: false, error: "bad_frame" };
  if (n[NONCE_RANDOM_LEN]! < 2 || keys.isOwn(pk) || refused.some((r) => bytesEqual(r, pk))) {
    return { ok: false, error: "bad_auth" };
  }
  const text = `${RELAY_AUTH_CONTEXT}:${nonce}:${host}`;
  return { ok: true, box: encode(keys.box(utf8Encode(text), n, pk)) };
}

export interface RelayClientOptions {
  keys: DeviceKeys;
  socketFactory: SocketFactory;
  clock: Clock;
  random: RandomBytes;
  logger?: Logger;
  timings?: Partial<RelayTimings>;
  /** Keys the relay may never use as its per-connection key (paired devices, pending joins). */
  refusedRelayKeys(): Uint8Array[];
  /** Current allow list (relay addresses), sent on every new session and by `syncAllow`. */
  allowList(): string[];
  /** Expo push token to register on every new session, if any. */
  pushToken(): string | null;
  /** With no token: must the relay forget the one it has (push turned off)? Sent once, then `onPushCleared`. */
  pushClear?(): boolean;
  onPushCleared?(): void;
  onMessage(from: string, env: PeerEnvelope): void;
  onError(code: RelayErrorCode, to?: string): void;
  /** The session is authenticated and the allow list went out. */
  onConnected(queued: number): void;
  onDisconnected(reason: string): void;
  onStatus(status: RelayStatusInfo): void;
}

type Phase = "idle" | "connecting" | "authenticating" | "connected" | "backoff";

export class RelayClient {
  readonly #o: RelayClientOptions;
  readonly #t: RelayTimings;
  readonly #log: Logger;
  #config: RelayConfig | null = null;
  #phase: Phase = "idle";
  #socket: SocketLike | null = null;
  #gen = 0;
  #status: RelayStatusInfo = { url: null, connected: false };
  #backoff: number;
  #startedAt = 0;
  #heardAt = 0;
  #authTimer: TimerHandle | null = null;
  #pingTimer: TimerHandle | null = null;
  #retryTimer: TimerHandle | null = null;
  #sentAllow: string | null = null;
  #sentPush: string | null = null;
  #challenge: { nonce: string; relayPk: string } | null = null;

  constructor(opts: RelayClientOptions) {
    this.#o = opts;
    this.#t = { ...DEFAULT_RELAY_TIMINGS, ...opts.timings };
    this.#log = opts.logger ?? silentLogger;
    this.#backoff = this.#t.backoffMinMs;
  }

  get config(): RelayConfig | null {
    return this.#config;
  }

  get connected(): boolean {
    return this.#phase === "connected";
  }

  get status(): RelayStatusInfo {
    return this.#status;
  }

  /** Use `config` (null = off). The same config is a no-op; anything else drops the session and reconnects. */
  setConfig(config: RelayConfig | null): void {
    const same =
      (config === null && this.#config === null) ||
      (config !== null && this.#config !== null && config.url === this.#config.url && config.accessKey === this.#config.accessKey);
    if (same) return;
    this.#teardown();
    this.#config = config;
    this.#backoff = this.#t.backoffMinMs;
    this.#setStatus({ url: config?.url ?? null, connected: false });
    if (config) this.#connect();
  }

  /** Stop for good (until the next setConfig). */
  stop(): void {
    this.#teardown();
    this.#config = null;
    this.#setStatus({ url: null, connected: false });
  }

  /** Skip the backoff wait and reconnect now if not connected (app came to the foreground, network changed). */
  reconnectNow(): void {
    if (!this.#config) return;
    if (this.#phase === "backoff" || this.#phase === "idle") {
      this.#clearRetry();
      this.#backoff = this.#t.backoffMinMs;
      this.#connect();
    } else if (this.#phase === "connected") {
      // A socket the OS froze in the background may be dead without a close event: probe it.
      this.#send({ t: "ping", ts: this.#o.clock.now() });
    }
  }

  /** Send a frame on the authenticated session. False when not connected (nothing is queued here). */
  send(frame: RelayClientFrame): boolean {
    if (this.#phase !== "connected") return false;
    return this.#send(frame);
  }

  /** Resend the allow list if it changed since the last one on this session. */
  syncAllow(): void {
    if (this.#phase !== "connected") return;
    const pks = [...new Set(this.#o.allowList())].sort();
    const key = pks.join(",");
    if (key === this.#sentAllow) return;
    if (this.#send({ t: "allow", pks })) this.#sentAllow = key;
  }

  /**
   * Register (or re-register) the push token on this session if it changed; with no token, tell the relay to forget
   * the stored one when `pushClear()` says so (push turned off).
   */
  syncPush(): void {
    if (this.#phase !== "connected") return;
    const token = this.#o.pushToken();
    if (!token) {
      if (this.#o.pushClear?.() && this.#send({ t: "unregister_push" })) {
        this.#sentPush = null;
        this.#o.onPushCleared?.();
      }
      return;
    }
    if (token === this.#sentPush) return;
    if (this.#send({ t: "register_push", expoToken: token })) this.#sentPush = token;
  }

  // ---------- internals ----------

  #setStatus(s: RelayStatusInfo): void {
    const prev = this.#status;
    if (prev.url === s.url && prev.connected === s.connected && prev.lastError === s.lastError) return;
    this.#status = s;
    this.#o.onStatus(s);
  }

  #send(frame: RelayClientFrame): boolean {
    const ws = this.#socket;
    if (!ws || ws.readyState !== SOCKET_OPEN) return false;
    try {
      ws.send(JSON.stringify(frame));
      return true;
    } catch {
      return false;
    }
  }

  #clearRetry(): void {
    if (this.#retryTimer !== null) this.#o.clock.clearTimeout(this.#retryTimer);
    this.#retryTimer = null;
  }

  #clearSessionTimers(): void {
    const c = this.#o.clock;
    if (this.#authTimer !== null) c.clearTimeout(this.#authTimer);
    if (this.#pingTimer !== null) c.clearTimeout(this.#pingTimer);
    this.#authTimer = null;
    this.#pingTimer = null;
  }

  #teardown(): void {
    this.#gen++;
    this.#clearRetry();
    this.#clearSessionTimers();
    const ws = this.#socket;
    this.#socket = null;
    const wasConnected = this.#phase === "connected";
    this.#phase = "idle";
    this.#challenge = null;
    if (ws) {
      ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
      try {
        ws.close(1000, "bye");
      } catch {
        // already closed
      }
    }
    if (wasConnected) this.#o.onDisconnected("stopped");
  }

  #connect(): void {
    const config = this.#config;
    if (!config) return;
    const gen = ++this.#gen;
    this.#phase = "connecting";
    this.#startedAt = this.#o.clock.now();
    this.#sentAllow = null;
    this.#sentPush = null;
    this.#challenge = null;
    let ws: SocketLike;
    try {
      // The URL holds the access key: never logged.
      ws = this.#o.socketFactory(relayConnectUrl(config.url, this.#o.keys.address, config.accessKey));
    } catch {
      this.#fail(gen, "unreachable");
      return;
    }
    this.#socket = ws;
    this.#armAuthTimer(gen);
    ws.onopen = () => {
      if (gen !== this.#gen) return;
      this.#phase = "authenticating";
    };
    ws.onmessage = (ev) => {
      if (gen !== this.#gen) return;
      this.#onFrame(gen, ev.data);
    };
    ws.onerror = () => {
      // A close event follows; it decides.
    };
    ws.onclose = (ev) => {
      if (gen !== this.#gen) return;
      const authenticating = this.#phase !== "connected";
      let reason: string;
      if (ev.code === CLOSE_REPLACED) reason = "replaced";
      else if (ev.code === CLOSE_POLICY && authenticating) reason = "bad_auth";
      else if (this.#phase === "connecting") reason = "unreachable";
      else reason = "closed";
      this.#fail(gen, reason);
    };
  }

  #armAuthTimer(gen: number): void {
    const c = this.#o.clock;
    if (this.#authTimer !== null) c.clearTimeout(this.#authTimer);
    this.#authTimer = c.setTimeout(() => {
      if (gen === this.#gen && this.#phase !== "connected") this.#fail(gen, "timeout");
    }, this.#t.authWaitMs);
  }

  #armPing(gen: number): void {
    const c = this.#o.clock;
    if (this.#pingTimer !== null) c.clearTimeout(this.#pingTimer);
    this.#pingTimer = c.setTimeout(() => {
      if (gen !== this.#gen || this.#phase !== "connected") return;
      if (c.now() - this.#heardAt >= this.#t.silentAfterMs) {
        this.#fail(gen, "timeout");
        return;
      }
      this.#send({ t: "ping", ts: c.now() });
      // Also retries an allow list / push token a failed send could not deliver.
      this.syncAllow();
      this.syncPush();
      this.#armPing(gen);
    }, this.#t.pingIntervalMs);
  }

  #onFrame(gen: number, data: unknown): void {
    if (typeof data !== "string" || data.length > INBOUND_MAX_CHARS) return;
    let f: Record<string, unknown>;
    try {
      const v: unknown = JSON.parse(data);
      if (typeof v !== "object" || v === null || Array.isArray(v)) return;
      f = v as Record<string, unknown>;
    } catch {
      return;
    }
    this.#heardAt = this.#o.clock.now();
    if (this.#phase !== "connected") {
      this.#onAuthFrame(gen, f);
      return;
    }
    switch (f.t) {
      case "msg": {
        const env = parseEnvelope(f.env);
        if (typeof f.from !== "string" || !env) return;
        this.#o.onMessage(f.from, env);
        return;
      }
      case "error": {
        if (typeof f.code !== "string") return;
        this.#o.onError(f.code as RelayErrorCode, typeof f.to === "string" ? f.to : undefined);
        return;
      }
      default:
        // pong, ok, a stray challenge: nothing to do (the frame already counted as "heard").
        return;
    }
  }

  #onAuthFrame(gen: number, f: Record<string, unknown>): void {
    const config = this.#config;
    if (!config) return;
    if (this.#challenge === null) {
      if (f.t !== "challenge" || typeof f.nonce !== "string" || typeof f.relayPk !== "string") {
        this.#fail(gen, "bad_frame");
        return;
      }
      if (f.v !== RELAY_PROTOCOL_VERSION) {
        this.#fail(gen, "unsupported_version");
        return;
      }
      const answer = authAnswer(this.#o.keys, f.nonce, f.relayPk, relayHostHeader(config.url), this.#o.refusedRelayKeys());
      if (!answer.ok) {
        if (answer.error === "bad_auth") this.#log("warn", "relay challenge refused: its nonce or key could reuse an envelope's");
        this.#fail(gen, answer.error);
        return;
      }
      this.#challenge = { nonce: f.nonce, relayPk: f.relayPk };
      this.#phase = "authenticating";
      if (!this.#send({ t: "auth", box: answer.box })) {
        this.#fail(gen, "closed");
        return;
      }
      this.#armAuthTimer(gen);
      return;
    }
    if (f.t === "ok") {
      const queued = typeof f.queued === "number" ? f.queued : 0;
      this.#clearSessionTimers();
      this.#phase = "connected";
      this.#heardAt = this.#o.clock.now();
      this.#setStatus({ url: config.url, connected: true });
      // The allow list goes first, before any frame of ours can need an answer.
      this.syncAllow();
      this.syncPush();
      this.#armPing(gen);
      this.#log("info", "relay connected", { queued });
      this.#o.onConnected(queued);
      return;
    }
    if (f.t === "error" && f.code === "bad_auth") {
      this.#fail(gen, "bad_auth");
      return;
    }
    this.#fail(gen, "bad_frame");
  }

  #fail(gen: number, reason: string): void {
    if (gen !== this.#gen) return;
    const wasConnected = this.#phase === "connected";
    const lived = this.#o.clock.now() - this.#startedAt;
    this.#gen++;
    this.#clearSessionTimers();
    const ws = this.#socket;
    this.#socket = null;
    this.#challenge = null;
    if (ws) {
      ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
      try {
        ws.close();
      } catch {
        // already closed
      }
    }
    this.#phase = "backoff";
    this.#setStatus({ url: this.#config?.url ?? null, connected: false, lastError: reason });
    if (wasConnected) this.#o.onDisconnected(reason);
    this.#log("info", "relay connection ended; reconnecting with backoff", { reason });
    if (!this.#config) {
      this.#phase = "idle";
      return;
    }
    if (lived >= this.#t.silentAfterMs) this.#backoff = this.#t.backoffMinMs;
    // Another socket with our key took the mailbox: do not fight it every second.
    const base = reason === "replaced" ? this.#t.backoffMaxMs : this.#backoff;
    const wait = jitter(base, this.#o.random);
    this.#backoff = Math.min(this.#backoff * 2, this.#t.backoffMaxMs);
    this.#clearRetry();
    this.#retryTimer = this.#o.clock.setTimeout(() => {
      this.#retryTimer = null;
      if (this.#phase === "backoff" && this.#config) this.#connect();
    }, wait);
  }
}

/** `d/2 + random(0..d/2)`, like the Rust side. */
export function jitter(d: number, random: RandomBytes): number {
  const half = d / 2;
  const r = random(4);
  const u = ((r[0]! << 24) >>> 0) + (r[1]! << 16) + (r[2]! << 8) + r[3]!;
  return Math.round(half + (u / 0x1_0000_0000) * half);
}
