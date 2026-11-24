// Messages between the extension host and the pet webview (view or editor panel).
// The webview opens no socket: the host owns the HubClient and relays state here.
import type { HubState } from "@ai-pet/hub-client";
import type { SpriteManifest } from "@ai-pet/pet-engine";

/** Host-side view of the hub connection. */
export interface ConnInfo {
  status: HubState["status"];
  /** True once we have been disconnected long enough to call the hub "offline" (quiet reconnect before that). */
  offline: boolean;
  /** No token could be found (hub never started on this machine, or env/settings empty). */
  missingToken: boolean;
  url: string;
  hubVersion?: string;
}

/** HubState minus what the webview never needs. Events are trimmed per session. */
export type WebviewHubState = Omit<HubState, "rules">;

export interface SpritesInfo {
  manifest: SpriteManifest;
  baseUrl: string;
}

export type HostToWebview =
  | {
      type: "init";
      mode: "view" | "panel";
      sprites: SpritesInfo | null;
      workspaceFolders: string[];
      extensionVersion: string;
    }
  | { type: "state"; conn: ConnInfo; state: WebviewHubState }
  /** Ask the webview to select (and scroll to) a session, e.g. after Start Session. */
  | { type: "focusSession"; sessionId: string }
  /** Result of a webview-initiated action that failed; shown inline. */
  | { type: "error"; message: string };

export type WebviewCommand =
  | "startSession"
  | "startHub"
  | "installHooks"
  | "changeSpecies"
  | "reconnect"
  | "openPanel";

export type WebviewToHost =
  | { type: "ready" }
  | { type: "approve"; requestId: string }
  | { type: "deny"; requestId: string }
  | { type: "resolve"; requestId: string; optionId: string | null }
  | { type: "prompt"; sessionId: string; text: string }
  | { type: "cancel"; sessionId: string }
  | { type: "chat"; text: string; petId?: string }
  | { type: "loadEvents"; sessionId: string }
  | { type: "openFile"; path: string; line?: number; cwd?: string }
  | { type: "command"; command: WebviewCommand; args?: unknown[] };

/** Max events per session relayed to the webview on each state push. */
export const WEBVIEW_EVENTS_PER_SESSION = 200;
