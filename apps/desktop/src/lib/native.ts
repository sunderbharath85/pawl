// Typed wrappers around the Rust commands/events in src-tauri.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface HubCredentials {
  /** HTTP base, e.g. http://127.0.0.1:47800 */
  url: string;
  /** WebSocket endpoint, e.g. ws://127.0.0.1:47800/ws */
  wsUrl: string;
  /** Empty when the hub never ran (no token file yet). */
  token: string;
  tokenSource: "env" | "file" | "missing";
  dataDir: string | null;
}

export interface NativeHubHealth {
  ok: boolean;
  version: string | null;
  error: string | null;
}

export interface StartHubResult {
  path: string | null;
  pid: number | null;
  alreadyRunning: boolean;
  healthy: boolean;
  version: string | null;
}

export interface HitRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

/** Payload of the `panel-focus` event (pet click → panel). */
export interface PanelFocus {
  petId?: string;
  sessionId?: string;
  view?: "session" | "approvals" | "chat" | "pets" | "new" | "rules" | "home";
  requestId?: string;
}

/** True inside the Tauri webview (false in a plain browser tab during `vite dev`). */
export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export const native = {
  hubCredentials: () => invoke<HubCredentials>("hub_credentials"),
  hubHealth: () => invoke<NativeHubHealth>("hub_health"),
  startHub: () => invoke<StartHubResult>("start_hub"),
  locateHub: () => invoke<string | null>("locate_hub"),
  quit: () => invoke<void>("quit_app"),
  setHitRects: (rects: HitRect[]) => invoke<void>("set_hit_rects", { rects }),
  setCaptureAll: (capture: boolean) => invoke<void>("set_capture_all", { capture }),
  /** While the strip shows an approval it follows the monitor under the cursor. */
  setFollowCursor: (follow: boolean) => invoke<void>("set_follow_cursor", { follow }),
  setPetsPaused: (paused: boolean) => invoke<void>("set_pets_paused", { paused }),
  getPetsPaused: () => invoke<boolean>("get_pets_paused"),
  openPanel: (focus?: PanelFocus) => invoke<void>("open_panel", { focus: focus ?? null }),
};

export interface NativeEvents {
  "hub-started": StartHubResult;
  "hub-start-failed": string;
  "pets-paused": boolean;
  "pet-hover": boolean;
  "panel-focus": PanelFocus;
  /** The global cursor moved (throttled by Rust): the user is on this machine. */
  "user-activity": null;
}

export function onNative<K extends keyof NativeEvents>(name: K, fn: (payload: NativeEvents[K]) => void): () => void {
  if (!isTauri) return () => {};
  let unlisten: UnlistenFn | null = null;
  let disposed = false;
  listen<NativeEvents[K]>(name, (e) => fn(e.payload))
    .then((u) => {
      if (disposed) u();
      else unlisten = u;
    })
    .catch((err) => console.warn(`[desktop] listen ${name} failed`, err));
  return () => {
    disposed = true;
    unlisten?.();
  };
}
