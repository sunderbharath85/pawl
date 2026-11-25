// Voice (v0.2, docs/CONTRACTS.md "v0.2 Voice"): typed wrappers for the Rust voice engine's commands and events, the
// pure state mapping behind the UI, and the useVoice() hook. The engine runs in this app (never in the hub): wake word,
// voice activity detection and speech to text on this PC. Audio stays in memory; nothing is recorded.
// Outside the Tauri app (browser dev) voice is unavailable unless a backend is installed (the dev mock, tests).
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { useCallback, useEffect, useReducer, useRef } from "react";
import { isTauri } from "./native";

// ---------------------------------------------------------------- contract types

export type VoiceState =
  | "off"
  | "missing_models"
  | "downloading"
  | "loading"
  | "idle"
  | "wake_listening"
  | "hearing"
  | "transcribing"
  | "muted"
  | "error";

/** Model sets to download: the default set (wake word, VAD, fast speech model) or the accurate speech model. */
export type VoiceModels = "default" | "accurate";
export type AsrModel = "fast" | "accurate";
export type ListenAction = "start" | "stop" | "cancel";

export interface VoiceConfig {
  enabled: boolean;
  wakeWord: boolean;
  keyword: string;
  spokenAs: string | null;
  sensitivity: number;
  micId: string | null;
  asrModel: AsrModel;
  hotkey: string | null;
  muted: boolean;
  readReplies: boolean;
}

export interface VoiceMic {
  id: string;
  name: string;
  isDefault: boolean;
}

export interface VoiceStatus {
  state: VoiceState;
  config: VoiceConfig;
  error: string | null;
  models: { default: { installed: boolean; bytes: number }; accurate: { installed: boolean; bytes: number } };
  mics: VoiceMic[];
}

export type VoiceEvent =
  | { type: "state"; state: VoiceState }
  | { type: "wake"; keyword: string; score: number }
  | { type: "level"; rms: number }
  | { type: "transcript"; id: string; text: string }
  | { type: "download"; models: VoiceModels; received: number; total: number }
  | { type: "error"; message: string };

export const DEFAULT_VOICE_CONFIG: VoiceConfig = {
  enabled: false,
  wakeWord: true,
  keyword: "",
  spokenAs: null,
  sensitivity: 0.5,
  micId: null,
  asrModel: "fast",
  hotkey: null,
  muted: false,
  readReplies: false,
};

/** Approximate download sizes, for the Settings copy (the engine reports exact sizes once installed). */
export const MODEL_SIZE_MB: Record<VoiceModels, number> = { default: 50, accurate: 108 };
export const ASR_SIZE_MB: Record<AsrModel, number> = { fast: 30, accurate: 108 };

// ---------------------------------------------------------------- backend

/** The engine's commands and events. The Tauri one in the app; tests and the dev mock install their own. */
export interface VoiceBackend {
  status(): Promise<VoiceStatus>;
  configure(config: VoiceConfig): Promise<VoiceStatus>;
  download(models: VoiceModels): Promise<void>;
  cancelDownload(): Promise<void>;
  deleteModels(models: VoiceModels | "all"): Promise<VoiceStatus>;
  listen(action: ListenAction): Promise<void>;
  micTest(on: boolean): Promise<void>;
  subscribe(fn: (e: VoiceEvent) => void): () => void;
}

export const tauriVoice: VoiceBackend = {
  status: () => invoke<VoiceStatus>("voice_status"),
  configure: (config) => invoke<VoiceStatus>("voice_configure", { config }),
  download: (models) => invoke<void>("voice_download", { models }),
  cancelDownload: () => invoke<void>("voice_cancel_download"),
  deleteModels: (models) => invoke<VoiceStatus>("voice_delete_models", { models }),
  listen: (action) => invoke<void>("voice_listen", { action }),
  micTest: (on) => invoke<void>("voice_mic_test", { on }),
  subscribe(fn) {
    let unlisten: UnlistenFn | null = null;
    let disposed = false;
    listen<VoiceEvent>("voice", (e) => fn(e.payload))
      .then((u) => {
        if (disposed) u();
        else unlisten = u;
      })
      .catch((err) => console.warn("[desktop] listen voice failed", err));
    return () => {
      disposed = true;
      unlisten?.();
    };
  },
};

let backend: VoiceBackend | null = isTauri ? tauriVoice : null;

/** The engine, or null where there is none (a browser tab without the dev mock). */
export function voiceBackend(): VoiceBackend | null {
  return backend;
}

/** Install a backend (the dev mock in `vite dev`, a fake in tests). */
export function setVoiceBackend(b: VoiceBackend | null): void {
  backend = b;
}

// ---------------------------------------------------------------- state mapping (pure)

export interface VoiceModel {
  /** The last full status (null until the first answer). */
  status: VoiceStatus | null;
  state: VoiceState;
  /** Microphone level 0..1 (while hearing or during a mic test). */
  level: number;
  /** When the last level arrived (ms; 0 = never): a level means the mic is open. */
  levelAt: number;
  transcript: { id: string; text: string; at: number } | null;
  wake: { keyword: string; score: number; at: number } | null;
  download: { models: VoiceModels; received: number; total: number } | null;
  error: string | null;
}

export const INITIAL_VOICE: VoiceModel = {
  status: null,
  state: "off",
  level: 0,
  levelAt: 0,
  transcript: null,
  wake: null,
  download: null,
  error: null,
};

export type VoiceAction = { type: "status"; status: VoiceStatus } | ({ at: number } & VoiceEvent);

function clamp01(n: number): number {
  return Number.isFinite(n) ? Math.min(1, Math.max(0, n)) : 0;
}

export function voiceReduce(m: VoiceModel, a: VoiceAction): VoiceModel {
  switch (a.type) {
    case "status":
      return {
        ...m,
        status: a.status,
        state: a.status.state,
        error: a.status.error,
        download: a.status.state === "downloading" ? m.download : null,
        level: a.status.state === "hearing" ? m.level : 0,
      };
    case "state": {
      const status = m.status ? { ...m.status, state: a.state } : null;
      return {
        ...m,
        status,
        state: a.state,
        error: a.state === "error" ? m.error : null,
        download: a.state === "downloading" ? m.download : null,
        level: a.state === "hearing" ? m.level : 0,
      };
    }
    case "level":
      return { ...m, level: clamp01(a.rms), levelAt: a.at };
    case "wake":
      return { ...m, wake: { keyword: a.keyword, score: a.score, at: a.at } };
    case "transcript":
      return { ...m, transcript: { id: a.id, text: a.text, at: a.at } };
    case "download":
      return { ...m, download: { models: a.models, received: Math.max(0, a.received), total: Math.max(0, a.total) } };
    case "error":
      return { ...m, error: a.message };
  }
}

/** A level arrived this recently: the mic is open (a mic test does not change the state). */
export const LEVEL_FRESH_MS = 1000;

/** What the pet and its bubble show: "live" while the mic is open, "muted" when the user muted it, else nothing. */
export type MicMarker = "live" | "muted" | null;

export function micMarker(m: VoiceModel, now: number): MicMarker {
  if (m.state === "wake_listening" || m.state === "hearing") return "live";
  if (m.levelAt > 0 && now - m.levelAt < LEVEL_FRESH_MS) return "live";
  if (m.state === "muted" || (m.status?.config.enabled && m.status.config.muted)) return "muted";
  return null;
}

/** Push-to-talk can start now (voice on, models loaded, mic not muted, nothing in progress). */
export function canTalk(m: VoiceModel): boolean {
  return m.state === "idle" || m.state === "wake_listening";
}

/** The bubble's listening line: "Listening…" while hearing, "Transcribing…" after, else none. */
export type ListenPhase = "listening" | "transcribing" | null;

export function listenPhase(state: VoiceState): ListenPhase {
  return state === "hearing" ? "listening" : state === "transcribing" ? "transcribing" : null;
}

/** Download progress 0..1, or null when the size is not known yet. */
export function downloadFraction(d: VoiceModel["download"]): number | null {
  if (!d || d.total <= 0) return null;
  return Math.min(1, d.received / d.total);
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024 * 1024) return `${Math.max(0, Math.round(bytes / 1024))} KB`;
  const mb = bytes / (1024 * 1024);
  return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)} MB`;
}

/**
 * The wake word follows the default pet's name (the hub's `bot.petName`): the config to send when it differs, or
 * null. A "say it like" spelling belongs to the old name, so a rename clears it.
 */
export function syncKeyword(config: VoiceConfig, hubName: string | null | undefined): VoiceConfig | null {
  const name = (hubName ?? "").trim();
  if (!name || config.keyword === name) return null;
  const renamed = config.keyword.trim() !== "" && config.keyword.trim().toLowerCase() !== name.toLowerCase();
  return { ...config, keyword: name, spokenAs: renamed ? null : config.spokenAs };
}

// ---------------------------------------------------------------- push-to-talk hotkey

const MOD_ORDER = ["Ctrl", "Alt", "Shift", "Super"] as const;
type Mod = (typeof MOD_ORDER)[number];
const MOD_ALIASES: Record<string, Mod> = {
  ctrl: "Ctrl",
  control: "Ctrl",
  alt: "Alt",
  option: "Alt",
  shift: "Shift",
  super: "Super",
  meta: "Super",
  win: "Super",
  cmd: "Super",
  command: "Super",
};
const NAMED_KEYS: Record<string, string> = {
  space: "Space",
  enter: "Enter",
  tab: "Tab",
  backspace: "Backspace",
  up: "Up",
  down: "Down",
  left: "Left",
  right: "Right",
  home: "Home",
  end: "End",
  pageup: "PageUp",
  pagedown: "PageDown",
  insert: "Insert",
  delete: "Delete",
};

function normalizeKey(raw: string): string | null {
  const k = raw.trim().toLowerCase();
  if (/^[a-z]$/.test(k) || /^[0-9]$/.test(k)) return k.toUpperCase();
  if (/^f([1-9]|1[0-9]|2[0-4])$/.test(k)) return k.toUpperCase();
  return NAMED_KEYS[k] ?? null;
}

export type HotkeyParse = { ok: true; hotkey: string } | { ok: false; error: string };

const NEEDS_MOD = "Add Ctrl, Alt or Win: a key on its own (or with only Shift) would go off while you type.";

/**
 * A push-to-talk combination such as "ctrl + alt + space" in the form Rust registers ("Ctrl+Alt+Space"): modifiers in
 * a fixed order, then one key. A plain key (or Shift + key) is refused, except F1-F24.
 */
export function parseHotkey(text: string): HotkeyParse {
  const parts = text
    .split("+")
    .map((p) => p.trim())
    .filter(Boolean);
  if (parts.length === 0) return { ok: false, error: "Press a key combination." };
  const mods = new Set<Mod>();
  let key: string | null = null;
  for (const p of parts) {
    const mod = MOD_ALIASES[p.toLowerCase()];
    if (mod) {
      mods.add(mod);
      continue;
    }
    if (key) return { ok: false, error: "Use one key with Ctrl, Alt, Shift or Win." };
    key = normalizeKey(p);
    if (!key) return { ok: false, error: `“${p}” can't be part of a shortcut. Use a letter, a digit, F1-F24, Space or an arrow key.` };
  }
  if (!key) return { ok: false, error: "Add a key after the modifiers (for example Ctrl+Alt+Space)." };
  const isF = /^F\d+$/.test(key);
  if (!isF && !mods.has("Ctrl") && !mods.has("Alt") && !mods.has("Super")) return { ok: false, error: NEEDS_MOD };
  return { ok: true, hotkey: [...MOD_ORDER.filter((m) => mods.has(m)), key].join("+") };
}

export interface KeyLike {
  key: string;
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

/**
 * While recording a shortcut: the combination this key press makes, or null while only modifiers are held. Uses the
 * physical key (`code`), so Shift or the keyboard layout does not change it.
 */
export function hotkeyFromKey(e: KeyLike): HotkeyParse | null {
  if (["Control", "Alt", "Shift", "Meta", "OS", "AltGraph"].includes(e.key)) return null;
  let key: string;
  if (/^Key[A-Z]$/.test(e.code)) key = e.code.slice(3);
  else if (/^Digit[0-9]$/.test(e.code)) key = e.code.slice(5);
  else if (/^Arrow(Up|Down|Left|Right)$/.test(e.code)) key = e.code.slice(5);
  else key = e.code || e.key;
  const mods = [e.ctrlKey && "Ctrl", e.altKey && "Alt", e.shiftKey && "Shift", e.metaKey && "Super"].filter(Boolean);
  return parseHotkey([...mods, key].join("+"));
}

/** "Ctrl+Alt+Space" for people: "Win" on Windows, "Cmd"/"Option" on macOS. */
export function hotkeyLabel(hotkey: string, platform: string = typeof navigator !== "undefined" ? navigator.platform : ""): string {
  const mac = /mac/i.test(platform);
  return hotkey
    .split("+")
    .map((p) => (p === "Super" ? (mac ? "Cmd" : "Win") : p === "Alt" && mac ? "Option" : p))
    .join(" + ");
}

// ---------------------------------------------------------------- the hook

export interface VoiceView extends VoiceModel {
  /** There is a voice engine here (false in a browser tab). */
  available: boolean;
  /** Ask the engine for its status again. */
  refresh(): Promise<void>;
  /** Change the config (merged into the current one) and fold the answer in. */
  configure(patch: Partial<VoiceConfig>): Promise<VoiceStatus | null>;
  /** The engine itself (null when unavailable): download, listen, mic test, delete. */
  api: VoiceBackend | null;
}

/**
 * The voice engine's status, state, level and last transcript for a window. `onEvent` sees every event as it
 * arrives (the pet window sends transcripts to the chat from there).
 */
export function useVoice(onEvent?: (e: VoiceEvent, model: VoiceModel) => void): VoiceView {
  const api = voiceBackend();
  const [model, dispatch] = useReducer(voiceReduce, INITIAL_VOICE);
  const latest = useRef(model);
  latest.current = model;
  const handler = useRef(onEvent);
  handler.current = onEvent;

  const refresh = useCallback(async () => {
    if (!api) return;
    try {
      dispatch({ type: "status", status: await api.status() });
    } catch (err) {
      dispatch({ type: "error", message: String((err as Error)?.message ?? err), at: Date.now() });
    }
  }, [api]);

  useEffect(() => {
    if (!api) return;
    const off = api.subscribe((e) => {
      dispatch({ ...e, at: Date.now() });
      handler.current?.(e, latest.current);
      // A state change can come with new models or mics: keep the full status fresh at the points that matter.
      if (e.type === "state" && ["idle", "missing_models", "off", "wake_listening", "muted", "error"].includes(e.state)) void refresh();
    });
    void refresh();
    return off;
  }, [api, refresh]);

  const configure = useCallback(
    async (patch: Partial<VoiceConfig>) => {
      if (!api) return null;
      const base = latest.current.status?.config ?? DEFAULT_VOICE_CONFIG;
      const status = await api.configure({ ...base, ...patch });
      dispatch({ type: "status", status });
      return status;
    },
    [api],
  );

  return { ...model, available: !!api, refresh, configure, api };
}
