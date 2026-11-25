import { describe, expect, test } from "bun:test";
import { markSpeechEnd, markSpeechStart, SPEECH_QUIET_MS, speechBusy } from "../lib/speech";
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
  const talking = { busy: true, manual: false };
  test("while the pet speaks: wake words are ignored, an utterance that starts is cancelled, a transcript is dropped", () => {
    const seen = new Set<string>();
    expect(voiceReaction({ type: "wake", keyword: "Mochi", score: 1 }, seen, false, talking)).toEqual({ kind: "none" });
    expect(voiceReaction({ type: "state", state: "hearing" }, seen, false, talking)).toEqual({ kind: "cancel" });
    expect(voiceReaction({ type: "transcript", id: "u5", text: "approve" }, seen, false, talking)).toEqual({ kind: "none" });
    // Dropped for good: it is not sent later either.
    expect(voiceReaction({ type: "transcript", id: "u5", text: "approve" }, seen, false)).toEqual({ kind: "none" });
  });

  test("the user talking from Pawl's UI (holding the pet, the mic button) still works right after the pet spoke", () => {
    const user = { busy: true, manual: true };
    expect(voiceReaction({ type: "state", state: "hearing" }, new Set(), false, user)).toEqual({ kind: "open" });
    expect(voiceReaction({ type: "transcript", id: "u6", text: "what's up" }, new Set(), false, user)).toEqual({ kind: "send", text: "what's up" });
  });

  test("busy while speaking and for 500 ms after it ends", () => {
    markSpeechStart();
    expect(speechBusy(Date.now() + 10_000)).toBe(true);
    const end = Date.now();
    markSpeechEnd(end);
    expect(speechBusy(end + SPEECH_QUIET_MS - 1)).toBe(true);
    expect(speechBusy(end + SPEECH_QUIET_MS + 1)).toBe(false);
  });
});
