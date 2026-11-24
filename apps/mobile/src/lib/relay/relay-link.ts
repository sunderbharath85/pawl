// RelayLink: carries the normal hub protocol (ClientMsg/ServerMsg JSON) through the E2E-encrypted relay.
//
// `HubClient` from @krypto/hub-client only needs a WebSocket-shaped constructor (`WebSocketImpl`). RelayLink
// hands it `SocketImpl`, a class whose instances look like a WebSocket to HubClient but actually:
//   - dial `wss://<relay>/v1/connect?role=device&hubId=..&deviceId=..`
//   - wrap every outgoing protocol frame in `{t:"msg", from: deviceId, to: hubId, nonce, box}` (nacl.box)
//   - open incoming `msg` frames from the hub and deliver the plaintext JSON as `onmessage({data})`
// so all state handling (hello/Welcome, acks, reducers, reconnect/backoff, queueing) is hub-client's own.
import { BoxChannel, decodeBase64 } from "./crypto";
import { parseRelayFrame, relayConnectUrl, type DeviceOutFrame, type MsgFrame } from "./frames";

export interface RelayLinkConfig {
  relay: string;
  hubId: string;
  /** Hub public key (base64). */
  hubPk: string;
  deviceId: string;
  /** Device secret key (base64). */
  deviceSk: string;
  /** WebSocket constructor; defaults to the global one (React Native provides it). */
  WebSocketImpl?: new (url: string) => WebSocket;
}

export interface RelayLinkStatus {
  /** The relay WebSocket is open. */
  relayConnected: boolean;
  /** Hub presence as reported by the relay (undefined = unknown / relay does not report it). */
  hubOnline?: boolean;
  /** Last relay-level problem (error frame, undecryptable box, socket error). */
  lastError?: string;
  /** Last time an authenticated frame from the hub arrived (unix ms). */
  lastHubFrameAt?: number;
  /** The relay/hub says this device is not (or no longer) paired. Re-pairing is needed. */
  deviceRevoked?: boolean;
}

/** Relay error codes meaning "this device is not paired with that hub". */
const REVOKED_CODES: ReadonlySet<string> = new Set(["unknown_device", "unpaired", "device_revoked", "unauthorized"]);

/** What hub-client needs from a socket (structural; mirrors its internal SocketLike). */
export interface HubSocketLike {
  readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: ((ev: { code?: number; reason?: string }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
}

export type HubSocketCtor = new (url: string) => HubSocketLike;

const CONNECTING = 0;
const OPEN = 1;
const CLOSING = 2;
const CLOSED = 3;

type Listener = () => void;

export class RelayLink {
  readonly SocketImpl: HubSocketCtor;
  private readonly cfg: RelayLinkConfig;
  private readonly channel: BoxChannel;
  private status: RelayLinkStatus = { relayConnected: false };
  private readonly listeners = new Set<Listener>();
  private current: RelaySocketInternals | null = null;
  /** Relay-level frames to (re)send whenever a socket opens (e.g. register_push). */
  private readonly sticky = new Map<string, DeviceOutFrame>();
  private disposed = false;

  constructor(cfg: RelayLinkConfig) {
    this.cfg = cfg;
    this.channel = new BoxChannel(decodeBase64(cfg.hubPk), decodeBase64(cfg.deviceSk));
    // eslint-disable-next-line @typescript-eslint/no-this-alias
    const link = this;
    this.SocketImpl = class RelaySocket implements HubSocketLike {
      readyState = CONNECTING;
      onopen: HubSocketLike["onopen"] = null;
      onmessage: HubSocketLike["onmessage"] = null;
      onclose: HubSocketLike["onclose"] = null;
      onerror: HubSocketLike["onerror"] = null;
      private readonly internals: RelaySocketInternals;
      constructor(_url: string) {
        this.internals = link.open(this);
      }
      send(data: string): void {
        if (this.readyState !== OPEN) throw new Error("relay socket is not open");
        this.internals.sendProtocol(data);
      }
      close(code?: number, reason?: string): void {
        if (this.readyState === CLOSED || this.readyState === CLOSING) return;
        this.readyState = CLOSING;
        this.internals.close(code, reason);
      }
    };
  }

  getStatus(): RelayLinkStatus {
    return this.status;
  }

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  }

  /** Register (or refresh) this device's Expo push token with the relay. Re-sent on every reconnect. */
  registerPush(expoPushToken: string): void {
    const frame: DeviceOutFrame = { t: "register_push", deviceId: this.cfg.deviceId, expoPushToken };
    this.sticky.set("register_push", frame);
    this.current?.sendRelay(frame);
  }

  /** Stop re-sending the push registration (the relay forgets it when the device unpairs). */
  clearPush(): void {
    this.sticky.delete("register_push");
  }

  dispose(): void {
    this.disposed = true;
    this.current?.close(1000, "disposed");
    this.current = null;
    this.listeners.clear();
  }

  // ------------------------------------------------------------------ internals

  private setStatus(patch: Partial<RelayLinkStatus>): void {
    const next = { ...this.status, ...patch };
    const keys = Object.keys(patch) as (keyof RelayLinkStatus)[];
    if (keys.every((k) => this.status[k] === next[k])) return;
    this.status = next;
    for (const fn of [...this.listeners]) {
      try {
        fn();
      } catch (err) {
        console.error("[relay-link] listener threw", err);
      }
    }
  }

  private open(facade: HubSocketLike): RelaySocketInternals {
    const Ctor = this.cfg.WebSocketImpl ?? (globalThis as { WebSocket?: new (url: string) => WebSocket }).WebSocket;
    const { cfg, channel } = this;
    let ws: WebSocket | null = null;
    let closed = false;

    const finish = (code?: number, reason?: string) => {
      if (closed) return;
      closed = true;
      facade.readyState = CLOSED;
      if (this.current === internals) {
        this.current = null;
        this.setStatus({ relayConnected: false });
      }
      facade.onclose?.({ code, reason });
    };

    const internals: RelaySocketInternals = {
      sendProtocol: (data: string) => {
        if (!ws || ws.readyState !== OPEN) throw new Error("relay socket is not open");
        const sealed = channel.sealText(data);
        const frame: MsgFrame = { t: "msg", from: cfg.deviceId, to: cfg.hubId, nonce: sealed.nonce, box: sealed.box };
        ws.send(JSON.stringify(frame));
      },
      sendRelay: (frame: DeviceOutFrame) => {
        if (ws && ws.readyState === OPEN) {
          try {
            ws.send(JSON.stringify(frame));
          } catch {
            /* re-sent on next open via sticky */
          }
        }
      },
      close: (code?: number, reason?: string) => {
        try {
          ws?.close(code ?? 1000, reason);
        } catch {
          /* ignore */
        }
        // RN fires onclose asynchronously; report closure right away so hub-client can move on.
        finish(code ?? 1000, reason);
      },
    };

    if (this.disposed || !Ctor) {
      setTimeout(() => {
        facade.onerror?.({ message: this.disposed ? "relay link disposed" : "no WebSocket implementation" });
        finish(1006, "unavailable");
      }, 0);
      return internals;
    }

    let url: string;
    try {
      url = relayConnectUrl(cfg.relay, { role: "device", hubId: cfg.hubId, deviceId: cfg.deviceId });
      ws = new Ctor(url);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.setStatus({ lastError: message });
      setTimeout(() => {
        facade.onerror?.({ message });
        finish(1006, message);
      }, 0);
      return internals;
    }

    // Replace any previous socket (hub-client only keeps one at a time).
    if (this.current && this.current !== internals) this.current.close(1000, "replaced");
    this.current = internals;
    const sock = ws;

    sock.onopen = () => {
      if (closed) return;
      facade.readyState = OPEN;
      this.setStatus({ relayConnected: true, lastError: undefined });
      for (const frame of this.sticky.values()) internals.sendRelay(frame);
      facade.onopen?.({});
    };
    sock.onmessage = (ev: { data: unknown }) => {
      if (closed || typeof ev.data !== "string") return;
      const frame = parseRelayFrame(ev.data);
      if (!frame) return;
      switch (frame.t) {
        case "msg": {
          if (frame.from !== cfg.hubId) return; // only the paired hub may talk to us
          const text = channel.openText(frame);
          if (text == null) {
            this.setStatus({ lastError: "dropped a frame that failed decryption" });
            return;
          }
          this.setStatus({ lastHubFrameAt: Date.now(), hubOnline: true });
          facade.onmessage?.({ data: text });
          return;
        }
        case "error":
          this.setStatus({
            lastError: frame.message ?? frame.code ?? "relay error",
            ...(frame.code && REVOKED_CODES.has(frame.code) ? { deviceRevoked: true } : {}),
          });
          return;
        case "presence": {
          const online = typeof frame.hubOnline === "boolean" ? frame.hubOnline : typeof frame.online === "boolean" ? frame.online : undefined;
          if (online !== undefined) this.setStatus({ hubOnline: online });
          return;
        }
        default:
          return;
      }
    };
    sock.onerror = (ev: unknown) => {
      if (closed) return;
      const message = (ev as { message?: unknown } | null)?.message;
      const text = typeof message === "string" && message ? message : "relay connection error";
      this.setStatus({ lastError: text });
      facade.onerror?.({ message: text });
    };
    sock.onclose = (ev: { code?: number; reason?: string }) => {
      finish(ev?.code, ev?.reason);
    };
    return internals;
  }
}

interface RelaySocketInternals {
  sendProtocol(data: string): void;
  sendRelay(frame: DeviceOutFrame): void;
  close(code?: number, reason?: string): void;
}
