// Look at the screen (v0.2 item 4): typed wrappers for the desktop's screen commands (docs/CONTRACTS.md "v0.2 Screen
// and Browser"). Nothing is captured until the user picks a source; the picture lives in memory only until it is
// sent or the flow is closed. Outside the Tauri app there is no screen access (browser dev: the mock, ?screen=mock).
import { invoke } from "@tauri-apps/api/core";
import type { ChatImage } from "@pawl/protocol";
import { isTauri } from "./native";

/** A screen or window the user can pick (Pawl's own windows are left out by the desktop). */
export interface ScreenSource {
  id: string;
  kind: "screen" | "window";
  title: string;
  app: string;
  /** Small base64 PNG. */
  thumb: string;
}

/** A capture, scaled to at most 2048 px on the long side. */
export interface ScreenCapture {
  mediaType: ChatImage["mediaType"];
  /** base64 */
  data: string;
  width: number;
  height: number;
}

/**
 * The desktop's screen gate (src-tauri/src/screen/gate.rs): a picker session starts with `begin` when the picker
 * opens (a nonce, valid 2 min, bound to this window, which must be visible; a new begin replaces the old) and ends
 * with `end` on Send, Cancel or close. Within it: `sources` at most every 2 s and 5 times, `capture` at most every
 * 1 s and 3 times, of an id from the latest list.
 */
export interface ScreenBackend {
  begin(): Promise<string>;
  sources(nonce: string): Promise<ScreenSource[]>;
  capture(nonce: string, id: string): Promise<ScreenCapture>;
  end(nonce: string): Promise<void>;
}

export const tauriScreen: ScreenBackend = {
  begin: () => invoke<string>("screen_begin"),
  sources: (nonce) => invoke<ScreenSource[]>("screen_sources", { nonce }),
  capture: (nonce, id) => invoke<ScreenCapture>("screen_capture", { nonce, id }),
  end: (nonce) => invoke<void>("screen_end", { nonce }),
};

/** The gate's own messages (shown as they are). */
export const SCREEN_OPEN_AGAIN = "Open the screen picker again.";
export const SCREEN_PICK_AGAIN = "Pick the screen or window again.";
export const SCREEN_SLOW_DOWN = "Too many screen pictures in a short time. Wait a moment, then try again.";

/**
 * What "Try again" does after a gate error: start a new picker session (expired or replaced), load the list again
 * (the source is gone), or the same after a short wait (too many in a short time).
 */
export function screenRetry(message: string): "restart" | "reload" | "wait" {
  if (message.includes(SCREEN_OPEN_AGAIN)) return "restart";
  if (message.includes(SCREEN_SLOW_DOWN)) return "wait";
  return "reload";
}

let backend: ScreenBackend | null = isTauri ? tauriScreen : null;

export function screenBackend(): ScreenBackend | null {
  return backend;
}

/** Install a backend (the dev mock, tests). */
export function setScreenBackend(b: ScreenBackend | null): void {
  backend = b;
}

export function pngDataUrl(base64: string): string {
  return `data:image/png;base64,${base64}`;
}

export function captureDataUrl(c: Pick<ScreenCapture, "mediaType" | "data">): string {
  return `data:${c.mediaType};base64,${c.data}`;
}

/** The capture as the one picture of a chat message or prompt. */
export function toChatImage(c: ScreenCapture): ChatImage {
  return { mediaType: c.mediaType, data: c.data };
}

/** Screens first, then windows by app and title. */
export function sortSources(list: readonly ScreenSource[]): ScreenSource[] {
  return [...list].sort((a, b) => (a.kind === b.kind ? 0 : a.kind === "screen" ? -1 : 1) || a.app.localeCompare(b.app) || a.title.localeCompare(b.title));
}
