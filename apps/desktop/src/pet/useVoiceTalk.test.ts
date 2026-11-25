import { describe, expect, test } from "bun:test";
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
