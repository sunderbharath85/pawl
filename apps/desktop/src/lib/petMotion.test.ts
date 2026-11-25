import { describe, expect, test } from "bun:test";
import { CHAT_WAIT_MAX_MS, chatPhase, petPose, REACT_MS, SESSION_WAIT_MAX_MS, sessionPhase } from "./petMotion";

describe("pet pose: still by default, animated only while the person chats", () => {
  test("only while we chat (default): still idle whatever the agents do", () => {
    for (const agentActivity of ["working", "alert", "napping", "thinking", "sad"] as const) {
      expect(petPose({ mode: "chat", reducedMotion: false, agentActivity, phase: "idle" })).toEqual({ activity: "idle", animate: false });
    }
  });

  test("waiting for a reply: thinking, animated; the reply: one happy reaction", () => {
    expect(petPose({ mode: "chat", reducedMotion: false, agentActivity: "working", phase: "waiting" })).toEqual({ activity: "thinking", animate: true });
    expect(petPose({ mode: "chat", reducedMotion: false, agentActivity: "working", phase: "reacting" })).toEqual({ activity: "happy", animate: true });
  });

  test("always: the pose follows the agents, animated", () => {
    expect(petPose({ mode: "always", reducedMotion: false, agentActivity: "working", phase: "idle" })).toEqual({ activity: "working", animate: true });
    expect(petPose({ mode: "always", reducedMotion: false, agentActivity: "alert", phase: "waiting" })).toEqual({ activity: "alert", animate: true });
  });

  test("prefers-reduced-motion: always still (the pose may still say what is going on)", () => {
    expect(petPose({ mode: "chat", reducedMotion: true, agentActivity: "working", phase: "waiting" }).animate).toBe(false);
    expect(petPose({ mode: "chat", reducedMotion: true, agentActivity: "working", phase: "reacting" }).animate).toBe(false);
    expect(petPose({ mode: "always", reducedMotion: true, agentActivity: "working", phase: "idle" })).toEqual({ activity: "working", animate: false });
  });
});

describe("the default pet's chat phase", () => {
  const sent = new Set(["u2"]);
  test("waiting from the person's message until the bot's reply, then a short reaction, then idle", () => {
    const waiting = [
      { id: "u1", role: "user" as const, ts: 100 },
      { id: "p1", role: "pet" as const, ts: 200 },
      { id: "u2", role: "user" as const, ts: 1_000 },
    ];
    expect(chatPhase(waiting, sent, 2_000)).toBe("waiting");
    expect(chatPhase(waiting, sent, 1_000 + CHAT_WAIT_MAX_MS)).toBe("idle");
    const replied = [...waiting, { id: "p2", role: "pet" as const, ts: 3_000 }];
    expect(chatPhase(replied, sent, 3_500)).toBe("reacting");
    expect(chatPhase(replied, sent, 3_000 + REACT_MS)).toBe("idle");
  });

  test("messages sent elsewhere (the panel's Chat view, another machine) do not animate the pet", () => {
    expect(chatPhase([{ id: "u9", role: "user", ts: 1_000 }], sent, 1_500)).toBe("idle");
    expect(chatPhase([], sent, 0)).toBe("idle");
  });
});

describe("a session pet's phase", () => {
  test("from the prompt to the agent's first reply, then a short reaction", () => {
    const events = [
      { type: "user_message", ts: 10_000 },
      { type: "tool_call", ts: 11_000 },
    ];
    expect(sessionPhase(events, 10_000, 12_000)).toBe("waiting");
    expect(sessionPhase(events, 10_000, 10_000 + SESSION_WAIT_MAX_MS)).toBe("idle");
    const replied = [...events, { type: "agent_message_chunk", ts: 13_000 }];
    expect(sessionPhase(replied, 10_000, 13_500)).toBe("reacting");
    expect(sessionPhase(replied, 10_000, 13_000 + REACT_MS)).toBe("idle");
    // An earlier turn's reply does not count.
    expect(sessionPhase([{ type: "agent_message_chunk", ts: 5_000 }], 10_000, 10_500)).toBe("waiting");
    expect(sessionPhase(events, undefined, 12_000)).toBe("idle");
  });
});
