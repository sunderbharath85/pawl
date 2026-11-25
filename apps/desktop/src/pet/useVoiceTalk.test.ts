import { describe, expect, test } from "bun:test";
import { markSpeechEnd, markSpeechStart, PAUSE_RENEW_MS, setSpeechPause, SPEECH_QUIET_MS, speechBusy } from "../lib/speech";
import { DIDNT_CATCH, voiceReaction } from "./useVoiceTalk";

describe("what the pet window does with voice events", () => {
  test("a wake word or any utterance starting opens the listening bubble", () => {
    expect(voiceReaction({ type: "wake", keyword: "Mochi", score: 0.7 }, new Set(), false)).toEqual({ kind: "open" });
    expect(voiceReaction({ type: "state", state: "hearing" }, new Set(), false)).toEqual({ kind: "open" });
    expect(voiceReaction({ type: "state", state: "transcribing" }, new Set(), false)).toEqual({ kind: "none" });
    expect(voiceReaction({ type: "level", rms: 0.3 }, new Set(), false)).toEqual({ kind: "none" });
  });

  test("a transcript goes to the chat once per id; an empty one says it did not catch that", () => {
    const seen = new Set<string>();
    expect(voiceReaction({ type: "transcript", id: "u1", text: "  run the tests in api " }, seen, false)).toEqual({ kind: "send", text: "run the tests in api" });
    expect(voiceReaction({ type: "transcript", id: "u1", text: "run the tests in api" }, seen, false)).toEqual({ kind: "none" });
    expect(voiceReaction({ type: "transcript", id: "u2", text: "   " }, seen, false)).toEqual({ kind: "say", text: DIDNT_CATCH });
  });

  test("while Settings tests the wake word, nothing happens (no bubble, nothing sent)", () => {
    const seen = new Set<string>();
    expect(voiceReaction({ type: "wake", keyword: "Mochi", score: 0.9 }, seen, true)).toEqual({ kind: "none" });
    expect(voiceReaction({ type: "transcript", id: "u3", text: "hello" }, seen, true)).toEqual({ kind: "none" });
    expect(seen.size).toBe(0);
  });
});

describe("the pet never answers itself", () => {
  const talking = { busy: true };
  test("while the pet speaks: a wake word is ignored; an utterance the wake word started is cancelled and its transcript dropped", () => {
    const seen = new Set<string>();
    expect(voiceReaction({ type: "wake", keyword: "Mochi", score: 1 }, seen, false, talking)).toEqual({ kind: "none" });
    expect(voiceReaction({ type: "state", state: "hearing", trigger: "wake" }, seen, false, talking)).toEqual({ kind: "cancel" });
    expect(voiceReaction({ type: "transcript", id: "u5", text: "approve" }, seen, false, { busy: true, utterance: "wake" })).toEqual({ kind: "none" });
    // Dropped for good: it is not sent later either.
    expect(voiceReaction({ type: "transcript", id: "u5", text: "approve" }, seen, false)).toEqual({ kind: "none" });
  });

  test("push-to-talk from Pawl's UI or the hotkey during speech is the user: kept (the pet stops talking instead)", () => {
    for (const trigger of ["ui", "hotkey"] as const) {
      expect(voiceReaction({ type: "state", state: "hearing", trigger }, new Set(), false, talking)).toEqual({ kind: "open" });
      expect(voiceReaction({ type: "transcript", id: `u-${trigger}`, text: "what's up" }, new Set(), false, { busy: true, utterance: trigger })).toEqual({
        kind: "send",
        text: "what's up",
      });
    }
  });

  test("when the pet is quiet, a wake-started utterance is normal", () => {
    expect(voiceReaction({ type: "state", state: "hearing", trigger: "wake" }, new Set(), false)).toEqual({ kind: "open" });
    expect(voiceReaction({ type: "transcript", id: "u7", text: "hi" }, new Set(), false, { busy: false, utterance: "wake" })).toEqual({ kind: "send", text: "hi" });
  });
});

describe("the voice engine is paused while the pet speaks", () => {
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

  test("paused at the start, resumed 500 ms after the end; speaking again before that keeps it paused", async () => {
    const calls: boolean[] = [];
    setSpeechPause((on) => calls.push(on));
    markSpeechStart();
    expect(calls).toEqual([true]);
    expect(speechBusy()).toBe(true);
    markSpeechEnd();
    await sleep(200);
    markSpeechStart();
    markSpeechEnd();
    await sleep(SPEECH_QUIET_MS - 100);
    expect(calls).toEqual([true, true]);
    await sleep(250);
    expect(calls).toEqual([true, true, false]);
    expect(speechBusy()).toBe(false);
    setSpeechPause(null);
  });

  test("voice turned off while paused: the engine is resumed; the pause is renewed before the engine's 30 s safety net", () => {
    const calls: boolean[] = [];
    setSpeechPause((on) => calls.push(on));
    markSpeechStart();
    setSpeechPause(null);
    expect(calls).toEqual([true, false]);
    markSpeechEnd();
    expect(PAUSE_RENEW_MS).toBeLessThan(30_000);
  });
});
