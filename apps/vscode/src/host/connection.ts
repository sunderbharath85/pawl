import * as vscode from "vscode";
import WebSocket from "ws";
import { DEFAULT_HUB_WS_URL, HubClient, scopeToMachine, toHubWsUrl, type HubState } from "@krypto/hub-client";
import { readHubCredentials } from "@krypto/hub-client/node";
import type { ServerMsg } from "@krypto/protocol";
import type { ConnInfo } from "../shared/messages";
import { errorMessage } from "./util";

/** How long the client may be disconnected before we show "hub offline" (quiet reconnect until then). */
const OFFLINE_AFTER_MS = 3000;
/** Poll interval for a token file that does not exist yet / changed after an `unauthorized`. */
const CREDENTIAL_POLL_MS = 5000;

interface Credentials {
  url: string;
  token: string | null;
}

export function emptyHubState(status: HubState["status"] = "disconnected"): HubState {
  return {
    status,
    machines: {},
    devices: [],
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

export class HubOfflineError extends Error {
  constructor() {
    super("The Krypto hub is offline. Run “Krypto: Start Hub” and try again.");
    this.name = "HubOfflineError";
  }
}

/**
 * Owns the HubClient for this window. Resolves credentials (settings > env > hub data dir),
 * reconnects quietly, and reports an "offline" state only after a grace period.
 */
export class HubConnection implements vscode.Disposable {
  private client: HubClient | null = null;
  private unsubscribe: (() => void) | null = null;
  private token: string | null = null;
  private url = DEFAULT_HUB_WS_URL;
  private disconnectedSince = Date.now();
  private offlineTimer: ReturnType<typeof setTimeout> | null = null;
  private pollTimer: ReturnType<typeof setInterval> | null = null;
  private lastStatus: HubState["status"] | null = null;
  private readonly stateListeners = new Set<(state: HubState, conn: ConnInfo) => void>();
  private readonly msgListeners = new Set<(msg: ServerMsg, state: HubState) => void>();
  private readonly disposables: vscode.Disposable[] = [];
  private conn: ConnInfo = { status: "disconnected", offline: false, missingToken: false, url: DEFAULT_HUB_WS_URL };
  private scopedFrom: HubState | null = null;
  private scoped: HubState | null = null;

  constructor(
    private readonly clientVersion: string,
    private readonly log: vscode.LogOutputChannel,
  ) {
    this.disposables.push(
      vscode.workspace.onDidChangeConfiguration((e) => {
        if (e.affectsConfiguration("krypto.hubUrl") || e.affectsConfiguration("krypto.hubToken")) {
          this.log.info("hub settings changed; reconnecting");
          this.restart();
        }
      }),
    );
  }

  /**
   * Current hub state (empty when no client exists yet), limited to this machine's pets, sessions and
   * approvals: paired machines are listed in `machines` / `devices`, their work arrives in migration step 3.
   */
  get state(): HubState {
    return this.client ? this.local(this.client.getState()) : emptyHubState(this.conn.status);
  }

  /** `scopeToMachine`, memoized on the client's state identity. */
  private local(state: HubState): HubState {
    if (state !== this.scopedFrom || !this.scoped) {
      this.scopedFrom = state;
      this.scoped = scopeToMachine(state, state.machine?.id);
    }
    return this.scoped;
  }

  get info(): ConnInfo {
    return this.conn;
  }

  get connected(): boolean {
    return this.client !== null && this.client.getState().status === "connected";
  }

  /** The client, only while connected. Throws HubOfflineError otherwise. */
  get api(): HubClient {
    if (!this.client || this.client.getState().status !== "connected") throw new HubOfflineError();
    return this.client;
  }

  onState(fn: (state: HubState, conn: ConnInfo) => void): vscode.Disposable {
    this.stateListeners.add(fn);
    return new vscode.Disposable(() => this.stateListeners.delete(fn));
  }

  onMessage(fn: (msg: ServerMsg, state: HubState) => void): vscode.Disposable {
    this.msgListeners.add(fn);
    return new vscode.Disposable(() => this.msgListeners.delete(fn));
  }

  start(): void {
    if (this.client) return;
    const creds = this.resolveCredentials();
    this.url = creds.url;
    this.token = creds.token;
    this.disconnectedSince = Date.now();
    if (!creds.token) {
      this.log.warn(`no hub token found; waiting for the hub to create one (polling every ${CREDENTIAL_POLL_MS / 1000}s)`);
      this.setConn({ status: "disconnected", offline: true, missingToken: true, url: creds.url });
      this.startPolling();
      this.emitState(this.state);
      return;
    }
    this.log.info(`connecting to ${creds.url}`);
    const client = new HubClient({
      url: creds.url,
      token: creds.token,
      client: "vscode",
      clientVersion: this.clientVersion,
      WebSocketImpl: WebSocket,
      reconnect: true,
    });
    this.client = client;
    this.unsubscribe = client.subscribe((state, msg) => this.handleUpdate(client, state, msg));
    this.handleUpdate(client, client.getState(), undefined);
    client.connect();
  }

  /** Drop the current client and connect again immediately (settings changed, hub just started, user asked). */
  restart(): void {
    this.stop();
    this.start();
  }

  private stop(): void {
    this.unsubscribe?.();
    this.unsubscribe = null;
    const client = this.client;
    this.client = null;
    try {
      client?.close();
    } catch (err) {
      this.log.debug(`close failed: ${errorMessage(err)}`);
    }
    this.stopPolling();
    this.clearOfflineTimer();
    this.lastStatus = null;
  }

  private resolveCredentials(): Credentials {
    const cfg = vscode.workspace.getConfiguration("krypto");
    const urlSetting = cfg.get<string>("hubUrl")?.trim() || "";
    const tokenSetting = cfg.get<string>("hubToken")?.trim() || "";
    let url = urlSetting || process.env.KRYPTO_HUB_URL?.trim() || DEFAULT_HUB_WS_URL;
    let token: string | null = tokenSetting || null;
    if (!token || !urlSetting) {
      try {
        const creds = readHubCredentials();
        if (!urlSetting && creds.url) url = creds.url;
        if (!token && creds.token) token = creds.token.trim() || null;
      } catch (err) {
        this.log.debug(`readHubCredentials: ${errorMessage(err)}`);
      }
    }
    return { url: toHubWsUrl(url), token };
  }

  private handleUpdate(client: HubClient, raw: HubState, msg: ServerMsg | undefined): void {
    if (client !== this.client) return; // stale client after restart
    const state = this.local(raw);
    if (state.status !== this.lastStatus) this.onStatusChange(state);
    if (msg) {
      for (const fn of this.msgListeners) {
        try {
          fn(msg, state);
        } catch (err) {
          this.log.error(`message listener failed: ${errorMessage(err)}`);
        }
      }
    }
    this.emitState(state);
  }

  private onStatusChange(state: HubState): void {
    const prev = this.lastStatus;
    this.lastStatus = state.status;
    const line = `hub ${state.status}${state.lastError ? ` (${state.lastError})` : ""}`;
    // Quiet reconnect: only transitions out of / into "connected" are worth an info line.
    if (state.status === "connected" || state.status === "unauthorized" || prev === "connected") this.log.info(line);
    else this.log.debug(line);
    switch (state.status) {
      case "connected":
        this.clearOfflineTimer();
        this.stopPolling();
        this.setConn({ status: "connected", offline: false, missingToken: false, url: this.url, hubVersion: state.hubVersion });
        return;
      case "unauthorized":
        // The hub may have rotated its token: watch the token file and reconnect when it changes.
        this.clearOfflineTimer();
        this.setConn({ status: "unauthorized", offline: true, missingToken: false, url: this.url });
        this.startPolling();
        return;
      default: {
        if (prev === "connected" || prev === null) this.disconnectedSince = Date.now();
        const offline = Date.now() - this.disconnectedSince >= OFFLINE_AFTER_MS;
        this.setConn({ status: state.status, offline: this.conn.offline || offline, missingToken: false, url: this.url });
        if (!offline) this.armOfflineTimer();
      }
    }
  }

  private armOfflineTimer(): void {
    if (this.offlineTimer) return;
    const wait = Math.max(0, OFFLINE_AFTER_MS - (Date.now() - this.disconnectedSince));
    this.offlineTimer = setTimeout(() => {
      this.offlineTimer = null;
      if (!this.connected) {
        this.setConn({ ...this.conn, offline: true });
        this.emitState(this.state);
      }
    }, wait);
  }

  private clearOfflineTimer(): void {
    if (this.offlineTimer) clearTimeout(this.offlineTimer);
    this.offlineTimer = null;
  }

  private startPolling(): void {
    if (this.pollTimer) return;
    this.pollTimer = setInterval(() => {
      const creds = this.resolveCredentials();
      if (creds.token && (creds.token !== this.token || creds.url !== this.url || !this.client)) {
        this.log.info("hub credentials changed; reconnecting");
        this.restart();
      }
    }, CREDENTIAL_POLL_MS);
  }

  private stopPolling(): void {
    if (this.pollTimer) clearInterval(this.pollTimer);
    this.pollTimer = null;
  }

  private setConn(next: ConnInfo): void {
    this.conn = next;
  }

  private emitState(state: HubState): void {
    for (const fn of this.stateListeners) {
      try {
        fn(state, this.conn);
      } catch (err) {
        this.log.error(`state listener failed: ${errorMessage(err)}`);
      }
    }
  }

  dispose(): void {
    this.stop();
    this.stateListeners.clear();
    this.msgListeners.clear();
    for (const d of this.disposables) d.dispose();
  }
}

/** Port the hub should listen on for this URL (for `krypto serve --port`), or null for the default. */
export function portOf(url: string): number | null {
  try {
    const u = new URL(url);
    return u.port ? Number(u.port) : null;
  } catch {
    return null;
  }
}
