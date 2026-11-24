import * as vscode from "vscode";
import { promises as fsp } from "node:fs";
import type { HubState } from "@ai-pet/hub-client";
import { parseSpriteManifest, type SpriteManifest } from "@ai-pet/pet-engine";
import {
  WEBVIEW_EVENTS_PER_SESSION,
  type HostToWebview,
  type SpritesInfo,
  type WebviewHubState,
  type WebviewToHost,
} from "../shared/messages";
import type { HubConnection } from "./connection";
import { errorMessage, nonce } from "./util";

export const PET_VIEW_ID = "ai-pet.petView";
export const PET_PANEL_TYPE = "ai-pet.petPanel";

/** Coalesce bursts of hub messages (streaming chunks) into one postMessage per frame-ish. */
const PUSH_THROTTLE_MS = 80;

/** Handles messages from any pet webview; resolves when done, rejects to show an inline error. */
export type WebviewMessageHandler = (msg: WebviewToHost) => Promise<void>;

interface Surface {
  webview: vscode.Webview;
  mode: "view" | "panel";
  ready: boolean;
}

export class PetWebviews implements vscode.WebviewViewProvider, vscode.WebviewPanelSerializer, vscode.Disposable {
  private view: vscode.WebviewView | undefined;
  private panel: vscode.WebviewPanel | undefined;
  private readonly surfaces = new Set<Surface>();
  private readonly disposables: vscode.Disposable[] = [];
  private pushTimer: ReturnType<typeof setTimeout> | null = null;
  private lastPushAt = 0;
  private manifest: Promise<SpriteManifest | null> | null = null;
  private pendingFocus: string | null = null;

  constructor(
    private readonly context: vscode.ExtensionContext,
    private readonly hub: HubConnection,
    private readonly onMessage: WebviewMessageHandler,
    private readonly log: vscode.LogOutputChannel,
  ) {
    this.disposables.push(
      vscode.window.registerWebviewViewProvider(PET_VIEW_ID, this, { webviewOptions: { retainContextWhenHidden: true } }),
      vscode.window.registerWebviewPanelSerializer(PET_PANEL_TYPE, this),
      hub.onState(() => this.schedulePush()),
      vscode.workspace.onDidChangeWorkspaceFolders(() => this.postInitAll()),
    );
  }

  private get distUri(): vscode.Uri {
    return vscode.Uri.joinPath(this.context.extensionUri, "dist");
  }

  // ---- WebviewViewProvider ----
  resolveWebviewView(view: vscode.WebviewView): void {
    this.view = view;
    view.title = "Pet";
    const surface = this.attach(view.webview, "view");
    view.onDidDispose(() => {
      this.surfaces.delete(surface);
      if (this.view === view) this.view = undefined;
    });
    view.onDidChangeVisibility(() => {
      if (view.visible) this.schedulePush(true);
    });
  }

  // ---- WebviewPanelSerializer (restores the editor panel after reload) ----
  async deserializeWebviewPanel(panel: vscode.WebviewPanel): Promise<void> {
    this.adoptPanel(panel);
  }

  /** Reveal the sidebar view (creating it if needed). */
  async revealView(): Promise<void> {
    await vscode.commands.executeCommand(`${PET_VIEW_ID}.focus`);
  }

  openPanel(): void {
    if (this.panel) {
      this.panel.reveal(undefined, false);
      return;
    }
    const panel = vscode.window.createWebviewPanel(
      PET_PANEL_TYPE,
      "AI Pet",
      { viewColumn: vscode.ViewColumn.Beside, preserveFocus: false },
      { enableScripts: true, retainContextWhenHidden: true, localResourceRoots: [this.distUri] },
    );
    this.adoptPanel(panel);
  }

  /** Select a session in every open pet surface (sent once a surface is ready). */
  focusSession(sessionId: string): void {
    this.pendingFocus = sessionId;
    for (const s of this.surfaces) if (s.ready) this.post(s, { type: "focusSession", sessionId });
  }

  private adoptPanel(panel: vscode.WebviewPanel): void {
    this.panel = panel;
    panel.iconPath = vscode.Uri.joinPath(this.context.extensionUri, "media", "pet.svg");
    const surface = this.attach(panel.webview, "panel");
    panel.onDidDispose(() => {
      this.surfaces.delete(surface);
      if (this.panel === panel) this.panel = undefined;
    });
    panel.onDidChangeViewState(() => {
      if (panel.visible) this.schedulePush(true);
    });
  }

  private attach(webview: vscode.Webview, mode: Surface["mode"]): Surface {
    webview.options = { enableScripts: true, enableCommandUris: false, localResourceRoots: [this.distUri] };
    const surface: Surface = { webview, mode, ready: false };
    this.surfaces.add(surface);
    webview.html = this.html(webview);
    webview.onDidReceiveMessage(async (raw: unknown) => {
      const msg = raw as WebviewToHost;
      if (!msg || typeof msg !== "object" || typeof msg.type !== "string") return;
      if (msg.type === "ready") {
        surface.ready = true;
        await this.postInit(surface);
        this.post(surface, this.stateMessage(this.hub.state));
        if (this.pendingFocus) this.post(surface, { type: "focusSession", sessionId: this.pendingFocus });
        return;
      }
      try {
        await this.onMessage(msg);
      } catch (err) {
        this.log.warn(`webview action ${msg.type} failed: ${errorMessage(err)}`);
        this.post(surface, { type: "error", message: errorMessage(err) });
      }
    });
    return surface;
  }

  private html(webview: vscode.Webview): string {
    const n = nonce();
    const script = webview.asWebviewUri(vscode.Uri.joinPath(this.distUri, "webview.js"));
    const style = webview.asWebviewUri(vscode.Uri.joinPath(this.distUri, "webview.css"));
    const csp = [
      "default-src 'none'",
      `img-src ${webview.cspSource} data: blob:`,
      `style-src ${webview.cspSource} 'nonce-${n}'`,
      `font-src ${webview.cspSource}`,
      `script-src 'nonce-${n}'`,
      `connect-src ${webview.cspSource}`,
      "base-uri 'none'",
      "form-action 'none'",
    ].join("; ");
    return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta http-equiv="Content-Security-Policy" content="${csp}">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<meta name="ai-pet-nonce" content="${n}">
<link rel="stylesheet" href="${style}" nonce="${n}">
<title>AI Pet</title>
</head>
<body>
<div id="root"></div>
<script nonce="${n}" src="${script}"></script>
</body>
</html>`;
  }

  private loadManifest(): Promise<SpriteManifest | null> {
    if (!this.manifest) {
      const file = vscode.Uri.joinPath(this.distUri, "media", "pets", "manifest.json");
      this.manifest = fsp
        .readFile(file.fsPath, "utf8")
        .then((text) => {
          const parsed = parseSpriteManifest(JSON.parse(text));
          if (!parsed) this.log.warn("sprite manifest is invalid; using built-in pets");
          return parsed;
        })
        .catch((err: NodeJS.ErrnoException) => {
          if (err.code !== "ENOENT") this.log.warn(`sprite manifest unreadable: ${errorMessage(err)}`);
          return null;
        });
    }
    return this.manifest;
  }

  private async spritesFor(webview: vscode.Webview): Promise<SpritesInfo | null> {
    const manifest = await this.loadManifest();
    if (!manifest) return null;
    const baseUrl = webview.asWebviewUri(vscode.Uri.joinPath(this.distUri, "media", "pets")).toString();
    return { manifest, baseUrl };
  }

  private async postInit(surface: Surface): Promise<void> {
    this.post(surface, {
      type: "init",
      mode: surface.mode,
      sprites: await this.spritesFor(surface.webview),
      workspaceFolders: (vscode.workspace.workspaceFolders ?? []).map((f) => f.uri.fsPath),
      extensionVersion: String(this.context.extension.packageJSON.version ?? "0.0.0"),
    });
  }

  private postInitAll(): void {
    for (const s of this.surfaces) if (s.ready) void this.postInit(s);
  }

  private stateMessage(state: HubState): HostToWebview {
    const events: WebviewHubState["events"] = {};
    for (const [sid, list] of Object.entries(state.events)) {
      events[sid] = list.length > WEBVIEW_EVENTS_PER_SESSION ? list.slice(-WEBVIEW_EVENTS_PER_SESSION) : list;
    }
    const { rules: _rules, ...rest } = state;
    return { type: "state", conn: this.hub.info, state: { ...rest, events } };
  }

  private schedulePush(immediate = false): void {
    if (this.surfaces.size === 0) return;
    if (this.pushTimer) {
      if (!immediate) return;
      clearTimeout(this.pushTimer);
      this.pushTimer = null;
    }
    const wait = immediate ? 0 : Math.max(0, PUSH_THROTTLE_MS - (Date.now() - this.lastPushAt));
    this.pushTimer = setTimeout(() => {
      this.pushTimer = null;
      this.lastPushAt = Date.now();
      const msg = this.stateMessage(this.hub.state);
      for (const s of this.surfaces) if (s.ready) this.post(s, msg);
    }, wait);
  }

  private post(surface: Surface, msg: HostToWebview): void {
    surface.webview.postMessage(msg).then(undefined, (err: unknown) => {
      this.log.debug(`postMessage failed: ${errorMessage(err)}`);
    });
  }

  dispose(): void {
    if (this.pushTimer) clearTimeout(this.pushTimer);
    this.panel?.dispose();
    for (const d of this.disposables) d.dispose();
    this.surfaces.clear();
  }
}
