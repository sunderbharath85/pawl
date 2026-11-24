// Thin typed wrapper around the webview API injected by VS Code.
import type { WebviewToHost } from "../shared/messages";

export interface PersistedState {
  tab: "sessions" | "chat";
  selectedSessionId: string | null;
  chatPetId: string | null;
  showEnded: boolean;
}

interface VsCodeApi {
  postMessage(msg: unknown): void;
  getState(): unknown;
  setState(state: unknown): void;
}

declare function acquireVsCodeApi(): VsCodeApi;

const api: VsCodeApi =
  typeof acquireVsCodeApi === "function"
    ? acquireVsCodeApi()
    : { postMessage: () => undefined, getState: () => undefined, setState: () => undefined };

export function post(msg: WebviewToHost): void {
  api.postMessage(msg);
}

export const DEFAULT_PERSISTED: PersistedState = { tab: "sessions", selectedSessionId: null, chatPetId: null, showEnded: false };

export function loadPersisted(): PersistedState {
  const raw = api.getState() as Partial<PersistedState> | undefined;
  return { ...DEFAULT_PERSISTED, ...(raw && typeof raw === "object" ? raw : {}) };
}

export function savePersisted(state: PersistedState): void {
  api.setState(state);
}

/** CSP nonce of this document (for any <style> element we must create at runtime). */
export function cspNonce(): string | undefined {
  return document.querySelector<HTMLMetaElement>('meta[name="ai-pet-nonce"]')?.content || undefined;
}
