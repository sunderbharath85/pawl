// Typed wrappers around the Rust commands/events in src-tauri.
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { homeDir } from "@tauri-apps/api/path";
import type { PetFrame, PetSpace } from "./petLayout";

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

/** Payload of the `panel-focus` event (the pet's menu → panel). */
export interface PanelFocus {
  petId?: string;
  sessionId?: string;
  view?: "session" | "approvals" | "chat" | "pets" | "new" | "rules" | "home" | "settings" | "machines" | "setup";
  /** Settings only: the section to scroll to (e.g. "pairing"). */
  section?: string;
  requestId?: string;
}

/** True inside the Tauri webview (false in a plain browser tab during `vite dev`). */
export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

let home: Promise<string | null> | null = null;
/** This user's home folder (looked up once), or null outside Tauri or when the OS does not say. */
export function homeFolder(): Promise<string | null> {
  if (!isTauri) return Promise.resolve(null);
  home ??= homeDir().then(
    (h) => h || null,
    () => null,
  );
  return home;
}

export const native = {
  hubCredentials: () => invoke<HubCredentials>("hub_credentials"),
  hubHealth: () => invoke<NativeHubHealth>("hub_health"),
  startHub: () => invoke<StartHubResult>("start_hub"),
  locateHub: () => invoke<string | null>("locate_hub"),
  /** This machine's main network address (a suggestion for the peer listener), or null. */
  lanAddress: () => invoke<string | null>("lan_address"),
  quit: () => invoke<void>("quit_app"),
  setHitRects: (rects: HitRect[]) => invoke<void>("set_hit_rects", { rects }),
  setCaptureAll: (capture: boolean) => invoke<void>("set_capture_all", { capture }),
  /** Size the pet window to `frame` (the default pet stays where the user put it); resolves with the room around it. */
  setPetFrame: (frame: PetFrame) => invoke<PetSpace | null>("set_pet_frame", { frame }),
  /** Start moving the pet with the mouse (call while the button is down). */
  petStartDrag: () => invoke<void>("pet_start_drag"),
  /** The pet's right-click menu: its name, Open hub, Rename… (enabled by `canRename`), Reset position, Settings, Quit. */
  petContextMenu: (name: string, canRename: boolean) => invoke<void>("pet_context_menu", { name, canRename }),
  /** Put the pet back at its default spot (bottom-right of the main monitor). */
  resetPetPosition: () => invoke<void>("reset_pet_position"),
  setPetsPaused: (paused: boolean) => invoke<void>("set_pets_paused", { paused }),
  getPetsPaused: () => invoke<boolean>("get_pets_paused"),
  /** Show the panel (created again if it was closed), bring it to the front and focus it. Only on the user's request. */
  openPanel: (focus?: PanelFocus) => invoke<void>("open_panel", { focus: focus ?? null }),
  /** The view the panel was opened for before it was listening (once). */
  takePanelFocus: () => invoke<PanelFocus | null>("take_panel_focus"),
};

export interface NativeEvents {
  "hub-started": StartHubResult;
  "hub-start-failed": string;
  "pets-paused": boolean;
  "pet-hover": boolean;
  /** Room around the default pet changed (it was dragged, reset, or the monitors changed). */
  "pet-space": PetSpace;
  /** Open the pet's bubble (tray icon click, a second launch). */
  "pet-attention": null;
  /** "Rename…" in the pet's menu: open the bubble with the rename field. */
  "pet-rename": null;
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
