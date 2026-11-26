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

  test("a streaming reply (Assistant mode): thinking while it streams, the reaction when it is finished", () => {
    const base = [{ id: "u2", role: "user" as const, ts: 1_000 }];
    const streaming = [...base, { id: "p2", role: "pet" as const, ts: 1_200, streaming: true }];
    expect(chatPhase(streaming, sent, 1_500)).toBe("waiting");
    // An agent can work longer than the bot's minute.
    expect(chatPhase(streaming, sent, 1_000 + CHAT_WAIT_MAX_MS + 1)).toBe("waiting");
    expect(chatPhase(streaming, sent, 1_000 + SESSION_WAIT_MAX_MS)).toBe("idle");
    const done = [...base, { id: "p2", role: "pet" as const, ts: 1_200, streaming: false }];
    const doneAt = new Map([["p2", 90_000]]);
    expect(chatPhase(done, sent, 90_500, doneAt)).toBe("reacting");
    expect(chatPhase(done, sent, 90_000 + REACT_MS, doneAt)).toBe("idle");
    // Not seen finishing here: the message's own time (long ago, so still).
    expect(chatPhase(done, sent, 90_500)).toBe("idle");
  });

  test("a second message during a turn gets a quick answer; the pet keeps thinking until the first reply is done", () => {
    const both = new Set(["u1", "u2"]);
    const msgs = [
      { id: "u1", role: "user" as const, ts: 1_000 },
      { id: "p1", role: "pet" as const, ts: 1_100, streaming: true },
      { id: "u2", role: "user" as const, ts: 5_000 },
      { id: "p2", role: "pet" as const, ts: 5_100 },
    ];
    expect(chatPhase(msgs, both, 6_000)).toBe("waiting");
    // The first reply finished: back to the usual (the quick answer was long ago).
    const done = msgs.map((m) => (m.id === "p1" ? { ...m, streaming: false } : m));
    expect(chatPhase(done, both, 60_000)).toBe("idle");
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
