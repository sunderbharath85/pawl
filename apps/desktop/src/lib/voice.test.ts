import { describe, expect, test } from "bun:test";
import {
  canTalk,
  DEFAULT_VOICE_CONFIG,
  downloadFraction,
  formatBytes,
  hotkeyFromKey,
  hotkeyLabel,
  INITIAL_VOICE,
  levelPercent,
  listenPhase,
  micMarker,
  parseHotkey,
  syncKeyword,
  voiceReduce,
  type KeyLike,
  type VoiceAction,
  type VoiceModel,
  type VoiceStatus,
} from "./voice";

const status = (over: Partial<VoiceStatus> = {}): VoiceStatus => ({
  state: "wake_listening",
  config: { ...DEFAULT_VOICE_CONFIG, enabled: true, keyword: "Mochi" },
  error: null,
  models: { default: { installed: true, bytes: 52_000_000 }, accurate: { installed: false, bytes: 0 } },
  mics: [{ id: "m1", name: "Headset", isDefault: true }],
  ...over,
});

const run = (actions: VoiceAction[], from: VoiceModel = INITIAL_VOICE) => actions.reduce(voiceReduce, from);

describe("voice state mapping", () => {
  test("status, then events: state, level, wake, transcript", () => {
    const m = run([
      { type: "status", status: status() },
      { type: "wake", keyword: "Mochi", score: 0.8, at: 10 },
      { type: "state", state: "hearing", at: 11 },
      { type: "level", rms: 0.4, at: 12 },
    ]);
    expect(m.state).toBe("hearing");
    expect(m.status?.state).toBe("hearing");
    expect(m.level).toBe(0.4);
    expect(m.wake).toEqual({ keyword: "Mochi", score: 0.8, at: 10 });
    expect(listenPhase(m.state)).toBe("listening");
    const t = run([{ type: "state", state: "transcribing", at: 13 }, { type: "transcript", id: "u1", text: "run the tests", at: 14 }], m);
    expect(t.level).toBe(0);
    expect(listenPhase(t.state)).toBe("transcribing");
    expect(t.transcript).toEqual({ id: "u1", text: "run the tests", at: 14 });
  });

  test("levels are clamped; download progress clears once the download ends; errors clear on a good state", () => {
    expect(run([{ type: "level", rms: 7, at: 1 }]).level).toBe(1);
    expect(run([{ type: "level", rms: Number.NaN, at: 1 }]).level).toBe(0);
    const d = run([
      { type: "state", state: "downloading", at: 1 },
      { type: "download", models: "default", received: 25, total: 100, at: 2 },
    ]);
    expect(downloadFraction(d.download)).toBe(0.25);
    expect(run([{ type: "state", state: "loading", at: 3 }], d).download).toBeNull();
    expect(downloadFraction({ models: "default", received: 5, total: 0 })).toBeNull();
    const e = run([{ type: "error", message: "no microphone", at: 1 }, { type: "state", state: "error", at: 2 }]);
    expect(e.error).toBe("no microphone");
    expect(run([{ type: "state", state: "idle", at: 3 }], e).error).toBeNull();
  });

  test("the mic marker: live while listening or during a mic test, crossed when muted, else none", () => {
    expect(micMarker(run([{ type: "status", status: status() }]), 0)).toBe("live");
    expect(micMarker(run([{ type: "state", state: "hearing", at: 0 }]), 0)).toBe("live");
    const idle = run([{ type: "status", status: status({ state: "idle" }) }]);
    expect(micMarker(idle, 5000)).toBeNull();
    // A mic test sends levels without changing the state.
    const testing = run([{ type: "level", rms: 0.2, at: 5000 }], idle);
    expect(micMarker(testing, 5500)).toBe("live");
    expect(micMarker(testing, 6500)).toBeNull();
    const muted = run([{ type: "status", status: status({ state: "muted", config: { ...DEFAULT_VOICE_CONFIG, enabled: true, muted: true } }) }]);
    expect(micMarker(muted, 0)).toBe("muted");
    expect(micMarker(INITIAL_VOICE, 0)).toBeNull();
  });

  test("push-to-talk only when voice is ready and not busy or muted", () => {
    for (const s of ["idle", "wake_listening"] as const) expect(canTalk({ ...INITIAL_VOICE, state: s })).toBe(true);
    for (const s of ["off", "missing_models", "downloading", "loading", "hearing", "transcribing", "muted", "error"] as const) {
      expect(canTalk({ ...INITIAL_VOICE, state: s })).toBe(false);
    }
  });

  test("levels are on a dBFS scale already: they map straight onto a meter", () => {
    expect(levelPercent(0)).toBe(0);
    expect(levelPercent(0.62)).toBe(62);
    expect(levelPercent(1)).toBe(100);
    expect(levelPercent(1.4)).toBe(100);
    expect(levelPercent(-0.2)).toBe(0);
    expect(levelPercent(Number.NaN)).toBe(0);
  });

  test("sizes read like people write them", () => {
    expect(formatBytes(512_000)).toBe("500 KB");
    expect(formatBytes(5_000_000)).toBe("4.8 MB");
    expect(formatBytes(113_246_208)).toBe("108 MB");
  });
});

describe("the wake word follows the pet's name", () => {
  const cfg = { ...DEFAULT_VOICE_CONFIG, keyword: "Mochi", spokenAs: "mo chee" };
  test("on load and on rename; a rename drops the old spelling", () => {
    expect(syncKeyword(cfg, "Mochi")).toBeNull();
    expect(syncKeyword(cfg, null)).toBeNull();
    expect(syncKeyword(cfg, "  ")).toBeNull();
    expect(syncKeyword(cfg, " Biscuit ")).toEqual({ ...cfg, keyword: "Biscuit", spokenAs: null });
    // First sync (no keyword yet) keeps a spelling; a change of case only keeps it too.
    expect(syncKeyword({ ...cfg, keyword: "" }, "Mochi")).toEqual({ ...cfg, keyword: "Mochi" });
    expect(syncKeyword(cfg, "MOCHI")).toEqual({ ...cfg, keyword: "MOCHI" });
  });
});

describe("push-to-talk hotkey", () => {
  test("typed combinations are normalized", () => {
    expect(parseHotkey("ctrl + alt + space")).toEqual({ ok: true, hotkey: "Ctrl+Alt+Space" });
    expect(parseHotkey("Shift+Win+k")).toEqual({ ok: true, hotkey: "Shift+Super+K" });
    expect(parseHotkey("cmd+option+1")).toEqual({ ok: true, hotkey: "Alt+Super+1" });
    expect(parseHotkey("F8")).toEqual({ ok: true, hotkey: "F8" });
    expect(parseHotkey("Ctrl+up")).toEqual({ ok: true, hotkey: "Ctrl+Up" });
  });

  test("refused: plain keys, Shift-only, two keys, unknown keys, no key", () => {
    expect(parseHotkey("k").ok).toBe(false);
    expect(parseHotkey("Shift+K").ok).toBe(false);
    expect(parseHotkey("Ctrl+A+B").ok).toBe(false);
    expect(parseHotkey("Ctrl+§").ok).toBe(false);
    expect(parseHotkey("Ctrl+Alt").ok).toBe(false);
    expect(parseHotkey("").ok).toBe(false);
    expect(parseHotkey("F25").ok).toBe(false);
  });

  test("recorded from key presses by physical key; modifiers alone wait for a key", () => {
    const k = (over: Partial<KeyLike>): KeyLike => ({ key: "", code: "", ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, ...over });
    expect(hotkeyFromKey(k({ key: "Control", code: "ControlLeft", ctrlKey: true }))).toBeNull();
    expect(hotkeyFromKey(k({ key: " ", code: "Space", ctrlKey: true, altKey: true }))).toEqual({ ok: true, hotkey: "Ctrl+Alt+Space" });
    // Shift changes the character, not the physical key.
    expect(hotkeyFromKey(k({ key: "!", code: "Digit1", ctrlKey: true, shiftKey: true }))).toEqual({ ok: true, hotkey: "Ctrl+Shift+1" });
    expect(hotkeyFromKey(k({ key: "ArrowUp", code: "ArrowUp", altKey: true }))).toEqual({ ok: true, hotkey: "Alt+Up" });
    expect(hotkeyFromKey(k({ key: "a", code: "KeyA" }))?.ok).toBe(false);
  });

  test("shown with the platform's names", () => {
    expect(hotkeyLabel("Ctrl+Super+Space", "Win32")).toBe("Ctrl + Win + Space");
    expect(hotkeyLabel("Alt+Super+K", "MacIntel")).toBe("Option + Cmd + K");
  });
});

describe("real payloads from the app's voice commands (0.1.1, captured from the Tauri app)", () => {
  // voice_status on a PC that never downloaded the models: no microphones listed yet, everything off.
  const real: VoiceStatus = {
    state: "off",
    config: { enabled: false, wakeWord: true, keyword: "Pickles", spokenAs: null, sensitivity: 0.5, micId: null, asrModel: "fast", hotkey: null, muted: false, readReplies: false },
    error: null,
    models: { default: { installed: false, bytes: 0 }, accurate: { installed: false, bytes: 0 } },
    mics: [],
  };

  test("voice_status folds in as is: off, no marker, no talking", () => {
    const m = voiceReduce(INITIAL_VOICE, { type: "status", status: real });
    expect(m.state).toBe("off");
    expect(m.status?.mics).toEqual([]);
    expect(micMarker(m, 1000)).toBeNull();
    expect(canTalk(m)).toBe(false);
  });

  test("voice_configure answers with the whole status (the config as saved)", () => {
    const configured: VoiceStatus = { ...real, config: { ...real.config, sensitivity: 0.55 } };
    const m = voiceReduce(voiceReduce(INITIAL_VOICE, { type: "status", status: real }), { type: "status", status: configured });
    expect(m.status?.config.sensitivity).toBe(0.55);
    expect(m.status?.models.default.installed).toBe(false);
  });

  test("state events after turning it on keep the rest of the status", () => {
    let m = voiceReduce(INITIAL_VOICE, { type: "status", status: { ...real, config: { ...real.config, enabled: true }, state: "loading" } });
    m = voiceReduce(m, { type: "state", state: "wake_listening", at: 1 });
    expect(m.status?.config.enabled).toBe(true);
    expect(m.status?.mics).toEqual([]);
    expect(micMarker(m, 2)).toBe("live");
    m = voiceReduce(m, { type: "state", state: "hearing", trigger: "ui", at: 3 });
    expect(m.state).toBe("hearing");
  });
});
