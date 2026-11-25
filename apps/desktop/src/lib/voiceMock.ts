// A stand-in voice engine for `vite dev` in a browser tab (no Tauri, no microphone): it follows the contract in
// docs/CONTRACTS.md "v0.2 Voice" closely enough to build and screenshot the UI. Loaded only in dev, and only when the
// URL asks for it: ?voice=mock (a fresh install: voice off, no models) or ?voice=ready (on, models installed,
// listening for the wake word). The page can drive it through `window.__pawlVoiceMock` (emit, listen, …).
import { DEFAULT_VOICE_CONFIG, setVoiceBackend, type ListenAction, type VoiceBackend, type VoiceConfig, type VoiceEvent, type VoiceModels, type VoiceState, type VoiceStatus } from "./voice";

const MB = 1024 * 1024;
const SIZES: Record<VoiceModels, number> = { default: 52 * MB, accurate: 108 * MB };
const SAMPLE_TRANSCRIPTS = ["What's everyone doing?", "Run the tests in api", ""];

export interface VoiceMock extends VoiceBackend {
  emit(e: VoiceEvent): void;
  current(): VoiceStatus;
}

export function createVoiceMock(start: "fresh" | "ready" = "fresh"): VoiceMock {
  const ready = start === "ready";
  const st: VoiceStatus = {
    state: "off",
    config: { ...DEFAULT_VOICE_CONFIG, enabled: ready, keyword: "" },
    error: null,
    models: { default: { installed: ready, bytes: ready ? SIZES.default : 0 }, accurate: { installed: false, bytes: 0 } },
    mics: [
      { id: "mic-default", name: "Microphone (Realtek Audio)", isDefault: true },
      { id: "mic-headset", name: "Headset Microphone (Jabra Evolve 65)", isDefault: false },
    ],
  };
  const listeners = new Set<(e: VoiceEvent) => void>();
  let levelTimer: ReturnType<typeof setInterval> | null = null;
  let downloadTimer: ReturnType<typeof setInterval> | null = null;
  let utterance = 0;

  const emit = (e: VoiceEvent) => listeners.forEach((fn) => fn(e));
  const derived = (): VoiceState => {
    if (!st.config.enabled) return "off";
    if (!st.models.default.installed || (st.config.asrModel === "accurate" && !st.models.accurate.installed)) return "missing_models";
    if (st.config.muted) return "muted";
    return st.config.wakeWord ? "wake_listening" : "idle";
  };
  const setState = (s: VoiceState) => {
    st.state = s;
    emit({ type: "state", state: s });
  };
  const levels = (on: boolean) => {
    if (levelTimer) clearInterval(levelTimer);
    // dBFS-scaled like the engine: speech around 0.5-0.85.
    levelTimer = on ? setInterval(() => emit({ type: "level", rms: 0.5 + Math.random() * 0.35 }), 60) : null;
  };
  st.state = derived();

  const mock: VoiceMock = {
    emit,
    current: () => structuredClone(st),
    status: async () => structuredClone(st),
    async configure(config: VoiceConfig) {
      st.config = { ...config };
      if (st.state !== "downloading" && st.state !== "hearing" && st.state !== "transcribing") setState(derived());
      return structuredClone(st);
    },
    async download(models: VoiceModels) {
      if (downloadTimer || st.models[models].installed) return;
      const total = SIZES[models];
      let received = 0;
      setState("downloading");
      downloadTimer = setInterval(() => {
        received = Math.min(total, received + total / 30);
        emit({ type: "download", models, received, total });
        if (received >= total) {
          clearInterval(downloadTimer!);
          downloadTimer = null;
          st.models[models] = { installed: true, bytes: total };
          setState("loading");
          setTimeout(() => setState(derived()), 400);
        }
      }, 120);
    },
    async cancelDownload() {
      if (downloadTimer) clearInterval(downloadTimer);
      downloadTimer = null;
      setState(derived());
    },
    async deleteModels(which) {
      for (const m of which === "all" ? (["default", "accurate"] as const) : [which]) st.models[m] = { installed: false, bytes: 0 };
      setState(derived());
      return structuredClone(st);
    },
    async listen(action: ListenAction) {
      if (action === "start") {
        if (st.state !== "idle" && st.state !== "wake_listening") return;
        setState("hearing");
        levels(true);
      } else if (action === "stop") {
        if (st.state !== "hearing") return;
        levels(false);
        setState("transcribing");
        const text = SAMPLE_TRANSCRIPTS[utterance % SAMPLE_TRANSCRIPTS.length]!;
        const id = `mock-${++utterance}`;
        setTimeout(() => {
          emit({ type: "transcript", id, text });
          setState(derived());
        }, 900);
      } else {
        levels(false);
        setState(derived());
      }
    },
    async micTest(on: boolean) {
      levels(on);
    },
    subscribe(fn) {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
  };
  return mock;
}

/** Install the mock when the URL asks for it (dev only; see the top of this file). */
export function installVoiceMockFromUrl(): void {
  const mode = new URLSearchParams(window.location.search).get("voice");
  if (mode !== "mock" && mode !== "ready") return;
  const mock = createVoiceMock(mode === "ready" ? "ready" : "fresh");
  setVoiceBackend(mock);
  (window as unknown as { __pawlVoiceMock: VoiceMock }).__pawlVoiceMock = mock;
}
