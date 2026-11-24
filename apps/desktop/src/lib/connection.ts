// Owns the desktop's HubClient: reads credentials from the Rust side, watches
// /health, (re)connects after the hub starts, and exposes a small "phase" model
// the UI can render ("hub is napping — wake it?").
import { HubClient, fetchHubHealth, toHubHttpUrl } from "@krypto/hub-client";
import { isTauri, native, onNative, type HubCredentials } from "./native";

export const CLIENT_VERSION = "0.1.0";

export type HubPhase =
  /** Reading credentials. */
  | "booting"
  /** /health fails (or no token yet): offer to start `krypto serve`. */
  | "hub-down"
  /** We asked Rust to spawn the hub and are waiting for it. */
  | "starting"
  /** The hub acked `restart_hub`; waiting for the replacement to come back (or RESTART_WAIT_MS to pass). */
  | "restarting"
  | "connecting"
  | "connected"
  /** Hub is up but rejected our token. */
  | "unauthorized";

export interface ConnectionMeta {
  phase: HubPhase;
  url: string;
  /** Bumps whenever `client` is replaced (re-subscribe React hooks). */
  clientId: number;
  hubVersion?: string;
  /** Last start attempt error (binary missing, early exit...). */
  startError?: string;
  /** Last health/credential problem worth showing in a tooltip. */
  detail?: string;
}

const HEALTH_EVERY_MS = 4000;
/** How long a restart may take before we stop saying "restarting" (the hub retries its port for ~10 s). */
export const RESTART_WAIT_MS = 30_000;

export class HubConnection {
  client: HubClient;
  private meta: ConnectionMeta = { phase: "booting", url: toHubHttpUrl(undefined), clientId: 0 };
  /** Working copy; published to `meta` (new identity) only when something changed. */
  private draft: ConnectionMeta = this.meta;
  private clientId = 0;
  private listeners = new Set<(m: ConnectionMeta) => void>();
  private creds: HubCredentials | null = null;
  private healthy: boolean | null = null;
  private starting = false;
  /** Set while a requested restart is in progress: until when, and whether the old connection has dropped yet. */
  private restarting: { until: number; dropped: boolean } | null = null;
  private healthTimer: ReturnType<typeof setInterval> | null = null;
  private unsubs: Array<() => void> = [];
  private started = false;

  constructor() {
    this.client = this.makeClient(undefined, "");
  }

  private makeClient(url: string | undefined, token: string): HubClient {
    const client = new HubClient({ url, token, client: "desktop", clientVersion: CLIENT_VERSION });
    client.subscribe(() => this.recompute());
    return client;
  }

  getMeta(): ConnectionMeta {
    return this.meta;
  }

  subscribeMeta(fn: (m: ConnectionMeta) => void): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** Begin: load credentials, probe health, connect. Idempotent. */
  start(): void {
    if (this.started) return;
    this.started = true;
    this.unsubs.push(
      onNative("hub-started", () => {
        this.starting = false;
        void this.refresh();
      }),
      onNative("hub-start-failed", (err) => {
        this.starting = false;
        this.patch({ startError: err });
        this.recompute();
      }),
    );
    void this.refresh();
    this.healthTimer = setInterval(() => void this.tick(), HEALTH_EVERY_MS);
  }

  stop(): void {
    if (this.healthTimer) clearInterval(this.healthTimer);
    this.healthTimer = null;
    for (const u of this.unsubs.splice(0)) u();
    this.client.close();
    this.started = false;
  }

  /** Ask Rust to spawn `krypto serve`, then reconnect. */
  async startHub(): Promise<void> {
    if (this.starting) return;
    if (!isTauri) {
      this.patch({ startError: "Starting the hub needs the desktop app (run `krypto serve` manually)." });
      return;
    }
    this.starting = true;
    this.patch({ startError: undefined });
    this.recompute();
    try {
      const res = await native.startHub();
      if (!res.healthy) {
        this.patch({ startError: "The hub was launched but has not answered /health yet." });
      }
    } catch (err) {
      this.patch({ startError: String(err) });
    } finally {
      this.starting = false;
      await this.refresh();
    }
  }

  /**
   * Ask the hub to restart itself (`restart_hub`; the caller confirmed with the user). Rejects with the hub's error.
   * After the ack the phase is "restarting" until the replacement hub says hello (it may be on a new port: the
   * credentials and health probe follow runtime.json) or RESTART_WAIT_MS pass.
   */
  async restartHub(): Promise<void> {
    if (this.restarting) return;
    await this.client.restartHub();
    this.restarting = { until: Date.now() + RESTART_WAIT_MS, dropped: false };
    this.patch({ startError: undefined });
    this.recompute();
  }

  /** Re-read credentials + health and (re)connect as needed. `force` redials even after `unauthorized`. */
  async refresh(force = false): Promise<void> {
    const creds = await this.loadCredentials();
    const health = await this.probe(creds?.url);
    this.healthy = health;

    let tokenChanged = false;
    if (creds) {
      const prev = this.creds;
      tokenChanged = !!prev && prev.token !== creds.token;
      this.creds = creds;
      const moved = toHubHttpUrl(this.client.wsUrl) !== toHubHttpUrl(creds.wsUrl);
      // A saved Hub port (config.toml) only takes effect when the hub restarts: keep talking to the running hub
      // on its current address until that connection drops, then dial the new one.
      const keep = moved && this.client.getState().status === "connected";
      if (moved && !keep) {
        this.client.close();
        this.client = this.makeClient(creds.wsUrl, creds.token);
        this.clientId += 1;
      } else if (!prev || prev.token !== creds.token) {
        this.client.setToken(creds.token);
      }
      this.patch({ url: keep ? toHubHttpUrl(this.client.wsUrl) : creds.url });
    }

    const token = this.creds?.token ?? "";
    // After an `unauthorized` stop, only redial once the token actually changed.
    const rejected = !force && this.client.getState().status === "unauthorized" && !tokenChanged;
    if (health && token && !rejected) {
      this.client.connect();
    }
    this.recompute();
  }

  private async tick(): Promise<void> {
    const status = this.client.getState().status;
    if (status === "connected") {
      if (this.healthy !== true) {
        this.healthy = true;
        this.recompute();
      }
      return;
    }
    const health = await this.probe(this.creds?.url);
    const wasHealthy = this.healthy;
    this.healthy = health;
    // Hub came back (or token may have rotated): re-read credentials and connect.
    if (health && (!wasHealthy || status === "unauthorized" || status === "disconnected")) {
      await this.refresh();
      return;
    }
    this.recompute();
  }

  private async loadCredentials(): Promise<HubCredentials | null> {
    if (!isTauri) {
      // Plain browser during `vite dev`: allow ?token=… for UI work against a running hub.
      const params = new URLSearchParams(location.search);
      const token = params.get("token") ?? localStorage.getItem("krypto.devToken") ?? "";
      if (params.get("token")) localStorage.setItem("krypto.devToken", token);
      const url = toHubHttpUrl(params.get("hub") ?? undefined);
      return { url, wsUrl: url.replace(/^http/, "ws") + "/ws", token, tokenSource: token ? "env" : "missing", dataDir: null };
    }
    try {
      return await native.hubCredentials();
    } catch (err) {
      this.patch({ detail: `Could not read hub credentials: ${String(err)}` });
      return null;
    }
  }

  private async probe(url: string | undefined): Promise<boolean> {
    if (isTauri) {
      try {
        const h = await native.hubHealth();
        this.patch({ detail: h.ok ? undefined : (h.error ?? undefined), hubVersion: h.version ?? this.draft.hubVersion });
        return h.ok;
      } catch (err) {
        this.patch({ detail: String(err) });
        return false;
      }
    }
    const h = await fetchHubHealth(url);
    if (h?.version) this.patch({ hubVersion: h.version });
    // Browser fetch may be CORS-blocked even when the hub is up; let the socket decide.
    return h?.ok ?? true;
  }

  private patch(p: Partial<ConnectionMeta>): void {
    this.draft = { ...this.draft, ...p };
  }

  private recompute(): void {
    const state = this.client.getState();
    // A restart is over once the old connection dropped and a new hello arrived, or after RESTART_WAIT_MS.
    const r = this.restarting;
    if (r) {
      if (state.status !== "connected") r.dropped = true;
      if (r.dropped && state.status === "connected") this.restarting = null;
      else if (Date.now() > r.until) {
        this.restarting = null;
        if (state.status !== "connected") {
          this.patch({ startError: "The hub has not come back after the restart. Start it again, or check desktop-hub.log in its data folder." });
        }
      }
    }
    let phase: HubPhase;
    if (this.starting) phase = "starting";
    else if (this.restarting) phase = "restarting";
    else if (state.status === "connected") phase = "connected";
    else if (state.status === "unauthorized") phase = "unauthorized";
    else if (this.healthy === null && !this.creds) phase = "booting";
    else if (this.healthy === false || !this.creds?.token) phase = "hub-down";
    else phase = "connecting";

    const next: ConnectionMeta = {
      ...this.draft,
      phase,
      clientId: this.clientId,
      hubVersion: state.hubVersion ?? this.draft.hubVersion,
      ...(phase === "connected" ? { startError: undefined, detail: undefined } : {}),
    };
    this.draft = next;
    const prev = this.meta;
    const keys = new Set([...Object.keys(prev), ...Object.keys(next)]) as Set<keyof ConnectionMeta>;
    let changed = false;
    for (const k of keys) if (prev[k] !== next[k]) changed = true;
    if (!changed) return;
    this.meta = next; // new identity only on change (useSyncExternalStore-friendly)
    for (const fn of this.listeners) fn(this.meta);
  }
}

let shared: HubConnection | null = null;
/** One connection per window. */
export function getConnection(): HubConnection {
  if (!shared) {
    shared = new HubConnection();
    shared.start();
  }
  return shared;
}
