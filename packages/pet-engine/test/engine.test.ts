import { describe, expect, test } from "bun:test";
import type { HubEvent, Pet, PermissionRequest, Session } from "@ai-pet/protocol";
import {
  activityForEvent,
  derivePetViews,
  HAPPY_DECAY_MS,
  lastSignificantEvents,
  NAP_AFTER_MS,
  nextPetViewChange,
  PET_ACTIVITIES,
  PET_LINES,
  petLine,
  SAD_DECAY_MS,
  SPECIES_META,
  type DerivePetViewsInput,
} from "../src/index";

let nextId = 1;
function ev(type: HubEvent["type"], payload: unknown = {}, ts = 1000, sessionId = "s1"): HubEvent {
  return { id: nextId++, sessionId, type, payload, ts };
}
function session(over: Partial<Session> = {}): Session {
  return { id: "s1", agentId: "claude-code", cwd: "/r", source: "owned", status: "idle", startedAt: 0, lastEventAt: 1000, ...over };
}
const pet: Pet = { id: "p1", agentId: "claude-code", species: "dog", skin: "default", name: "Biscuit", personality: "", mood: 70, xp: 0 };
function perm(over: Partial<PermissionRequest> = {}): PermissionRequest {
  return {
    requestId: "r1",
    sessionId: "s1",
    agentId: "claude-code",
    toolCall: { toolCallId: "t", title: "Run npm test", kind: "execute" },
    options: [{ optionId: "a", name: "Allow", kind: "allow_once" }],
    createdAt: 1500,
    ...over,
  };
}
function input(over: Partial<DerivePetViewsInput> = {}): DerivePetViewsInput {
  return { pets: [pet], sessions: [], pending: [], lastEvents: {}, now: 1000, ...over };
}

describe("activityForEvent", () => {
  test("PLAN table", () => {
    expect(activityForEvent(ev("agent_thought_chunk", { text: "hm" }))).toBe("thinking");
    expect(activityForEvent(ev("tool_call", { toolCallId: "t", kind: "edit" }))).toBe("working");
    expect(activityForEvent(ev("tool_call", { toolCallId: "t", kind: "execute" }))).toBe("working");
    expect(activityForEvent(ev("tool_call", { toolCallId: "t", kind: "read" }))).toBe("thinking");
    expect(activityForEvent(ev("tool_call", { toolCallId: "t" }))).toBe("working");
    expect(activityForEvent(ev("plan", { entries: [] }))).toBe("planning");
    expect(activityForEvent(ev("request_permission", {}))).toBe("alert");
    expect(activityForEvent(ev("stop", { stopReason: "end_turn" }))).toBe("happy");
    expect(activityForEvent(ev("stop", { stopReason: "refusal" }))).toBe("sad");
    expect(activityForEvent(ev("stop", { stopReason: "cancelled" }))).toBe("idle");
    expect(activityForEvent(ev("error", { message: "x" }))).toBe("sad");
  });
  test("tool_call_update only while in progress", () => {
    expect(activityForEvent(ev("tool_call_update", { toolCallId: "t", status: "in_progress", kind: "edit" }))).toBe("working");
    expect(activityForEvent(ev("tool_call_update", { toolCallId: "t", status: "completed" }))).toBeNull();
  });
  test("hooks", () => {
    expect(activityForEvent(ev("hook", { source: "claude", hookEvent: "Stop", raw: {} }))).toBe("happy");
    expect(activityForEvent(ev("hook", { source: "claude", hookEvent: "PreToolUse", raw: {} }))).toBe("working");
    expect(activityForEvent(ev("hook", { source: "claude", hookEvent: "UserPromptSubmit", raw: {} }))).toBe("thinking");
    expect(activityForEvent(ev("hook", { source: "claude", hookEvent: "Notification", raw: {} }))).toBe("alert");
    expect(activityForEvent(ev("hook", { source: "codex", hookEvent: "agent-turn-complete", raw: {} }))).toBe("happy");
    expect(activityForEvent(ev("hook", { source: "claude", hookEvent: "Whatever", raw: {} }))).toBeNull();
  });
  test("neutral events and junk payloads", () => {
    expect(activityForEvent(ev("permission_resolved", {}))).toBeNull();
    expect(activityForEvent(ev("session_end", {}))).toBeNull();
    expect(activityForEvent(ev("tool_call", null))).toBe("working");
    expect(activityForEvent(ev("hook", "garbage"))).toBeNull();
  });
});

test("lastSignificantEvents skips neutral events", () => {
  const a = ev("plan", { entries: [] }, 10);
  const b = ev("permission_resolved", {}, 20);
  const out = lastSignificantEvents({ s1: [a, b], s2: [] });
  expect(out.s1).toBe(a);
  expect(out.s2).toBeUndefined();
});

describe("derivePetViews", () => {
  test("no sessions → napping", () => {
    const [v] = derivePetViews(input());
    expect(v).toMatchObject({ petId: "p1", agentId: "claude-code", species: "dog", name: "Biscuit", activity: "napping", mood: 70 });
    expect(v!.bubble).toBeUndefined();
  });

  test("pending permission → alert with permission bubble (beats everything)", () => {
    const [v] = derivePetViews(input({ sessions: [session({ status: "working" })], pending: [perm()], now: 2000 }));
    expect(v!.activity).toBe("alert");
    expect(v!.bubble).toMatchObject({ kind: "permission", requestId: "r1" });
    expect(v!.bubble!.text).toContain("Run npm test");
    expect(v!.since).toBe(1500);
    expect(v!.sessionId).toBe("s1");
  });

  test("oldest pending first, with a count", () => {
    const [v] = derivePetViews(input({ pending: [perm({ requestId: "r2", createdAt: 9 }), perm({ requestId: "r1", createdAt: 5 })] }));
    expect(v!.bubble!.requestId).toBe("r1");
    expect(v!.bubble!.text).toContain("+1 more");
  });

  test("permission for another agent's session is ignored", () => {
    const [v] = derivePetViews(input({ pending: [perm({ agentId: "codex", sessionId: "other" })] }));
    expect(v!.activity).toBe("napping");
  });

  test("busy session: status refined by last event", () => {
    const s = session({ status: "thinking", lastEventAt: 900 });
    expect(derivePetViews(input({ sessions: [s] }))[0]!.activity).toBe("thinking");
    const plan = ev("plan", { entries: [] }, 950);
    expect(derivePetViews(input({ sessions: [s], lastEvents: { s1: plan } }))[0]!.activity).toBe("planning");
    const working = session({ status: "working" });
    const thought = ev("agent_thought_chunk", { text: "x" }, 990);
    expect(derivePetViews(input({ sessions: [working], lastEvents: { s1: thought } }))[0]!.activity).toBe("thinking");
    // a stale 'stop' event does not override a busy status
    const stop = ev("stop", { stopReason: "end_turn" }, 800);
    expect(derivePetViews(input({ sessions: [working], lastEvents: { s1: stop } }))[0]!.activity).toBe("working");
  });

  test("hook session waiting_permission → alert with say bubble naming VS Code", () => {
    const [v] = derivePetViews(input({ sessions: [session({ status: "waiting_permission", source: "vscode_hook" })] }));
    expect(v!.activity).toBe("alert");
    expect(v!.bubble!.kind).toBe("say");
    expect(v!.bubble!.text).toContain("VS Code");
  });

  test("end_turn → happy for 6 s, then idle, then napping after 60 s", () => {
    const s = session({ status: "done", lastEventAt: 1000 });
    const stop = ev("stop", { stopReason: "end_turn" }, 1000);
    const at = (now: number) => derivePetViews(input({ sessions: [s], lastEvents: { s1: stop }, now }))[0]!;
    expect(at(1000).activity).toBe("happy");
    expect(at(1000).bubble?.kind).toBe("say");
    expect(at(1000 + HAPPY_DECAY_MS - 1).activity).toBe("happy");
    expect(at(1000 + HAPPY_DECAY_MS).activity).toBe("idle");
    expect(at(1000 + HAPPY_DECAY_MS).since).toBe(1000 + HAPPY_DECAY_MS);
    expect(at(1000 + NAP_AFTER_MS - 1).activity).toBe("idle");
    expect(at(1000 + NAP_AFTER_MS).activity).toBe("napping");
    expect(at(1000 + NAP_AFTER_MS).since).toBe(1000 + NAP_AFTER_MS);
  });

  test("error → sad for 10 s with the message", () => {
    const s = session({ status: "error", lastEventAt: 1000 });
    const err = ev("error", { message: "boom: disk full" }, 1000);
    const at = (now: number) => derivePetViews(input({ sessions: [s], lastEvents: { s1: err }, now }))[0]!;
    expect(at(5000).activity).toBe("sad");
    expect(at(5000).bubble!.text).toContain("disk full");
    expect(at(1000 + SAD_DECAY_MS).activity).toBe("idle");
  });

  test("refusal on a done session is sad", () => {
    const s = session({ status: "done", lastEventAt: 1000 });
    const stop = ev("stop", { stopReason: "refusal" }, 1000);
    expect(derivePetViews(input({ sessions: [s], lastEvents: { s1: stop }, now: 2000 }))[0]!.activity).toBe("sad");
  });

  test("ended session counts its endedAt for the nap timer", () => {
    const s = session({ status: "done", lastEventAt: 1000, endedAt: 30_000 });
    const v = derivePetViews(input({ sessions: [s], now: 30_000 + NAP_AFTER_MS - 1 }))[0]!;
    expect(v.activity).toBe("idle");
    expect(v.sessionId).toBeUndefined();
    expect(derivePetViews(input({ sessions: [s], now: 30_000 + NAP_AFTER_MS }))[0]!.activity).toBe("napping");
  });

  test("most recently active busy session wins", () => {
    const a = session({ id: "a", status: "working", lastEventAt: 100 });
    const b = session({ id: "b", status: "thinking", lastEventAt: 200 });
    const [v] = derivePetViews(input({ sessions: [a, b] }));
    expect(v!.sessionId).toBe("b");
    expect(v!.activity).toBe("thinking");
  });

  test("one view per pet, own agent only, input order kept", () => {
    const cat: Pet = { ...pet, id: "p2", agentId: "codex", species: "cat", name: "Miso" };
    const views = derivePetViews(input({ pets: [cat, pet], sessions: [session({ status: "working" })] }));
    expect(views.map((v) => [v.petId, v.activity])).toEqual([
      ["p2", "napping"],
      ["p1", "working"],
    ]);
  });

  test("bubble text is stable across re-derivations", () => {
    const s = session({ status: "done", lastEventAt: 1000 });
    const stop = ev("stop", { stopReason: "end_turn" }, 1000);
    const a = derivePetViews(input({ sessions: [s], lastEvents: { s1: stop }, now: 1100 }))[0]!;
    const b = derivePetViews(input({ sessions: [s], lastEvents: { s1: stop }, now: 4000 }))[0]!;
    expect(a.bubble!.text).toBe(b.bubble!.text);
  });
});

describe("nextPetViewChange", () => {
  test("reports the next decay / nap time", () => {
    const s = session({ status: "done", lastEventAt: 1000 });
    const stop = ev("stop", { stopReason: "end_turn" }, 1000);
    const base = { sessions: [s], lastEvents: { s1: stop } };
    expect(nextPetViewChange(input({ ...base, now: 1500 }))).toBe(1000 + HAPPY_DECAY_MS);
    expect(nextPetViewChange(input({ ...base, now: 1000 + HAPPY_DECAY_MS }))).toBe(1000 + NAP_AFTER_MS);
    expect(nextPetViewChange(input({ ...base, now: 1000 + NAP_AFTER_MS }))).toBeNull();
    expect(nextPetViewChange(input({ sessions: [session({ status: "working" })] }))).toBeNull();
  });
});

describe("petLine / SPECIES_META", () => {
  test("every species × activity has lines", () => {
    for (const sp of Object.keys(SPECIES_META) as (keyof typeof SPECIES_META)[]) {
      for (const act of PET_ACTIVITIES) {
        expect(PET_LINES[sp][act].length).toBeGreaterThan(0);
        expect(petLine(sp, act).length).toBeGreaterThan(0);
      }
    }
  });
  test("deterministic by seed, wraps, tolerates junk", () => {
    expect(petLine("dog", "alert")).toBe("Woof! Need you!");
    expect(petLine("dog", "alert", 3)).toBe(petLine("dog", "alert", 0));
    expect(petLine("dog", "alert", -1)).toBe(petLine("dog", "alert", 1));
    expect(petLine("dog", "alert", Number.NaN)).toBe("Woof! Need you!");
  });
  test("meta", () => {
    expect(SPECIES_META.dog.sound).toBe("Woof!");
    expect(SPECIES_META.robot.label).toBe("Robot");
  });
});
