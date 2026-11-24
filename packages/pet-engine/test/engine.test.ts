import { describe, expect, test } from "bun:test";
import { globalId, type HubEvent, type Machine, type Pet, type PermissionRequest, type Session } from "@krypto/protocol";
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
  stageShowsApproval,
  approvalConfirmText,
  type DerivePetViewsInput,
} from "../src/index";

/** This machine and a paired one (12 chars [a-z0-9]). */
const MID = "winpc0000001";
const MID2 = "macmini00002";

let nextSeq = 1;
function ev(type: HubEvent["type"], payload: unknown = {}, ts = 1000, sessionId = "s1", machineId = MID): HubEvent {
  const seq = nextSeq++;
  return { id: globalId(machineId, seq), seq, machineId, sessionId, type, payload, ts };
}
function session(over: Partial<Session> = {}): Session {
  return { id: "s1", machineId: MID, agentId: "claude-code", cwd: "/r", source: "owned", status: "idle", startedAt: 0, lastEventAt: 1000, ...over };
}
const pet: Pet = { id: "p1", machineId: MID, agentId: "claude-code", species: "dog", skin: "default", name: "Biscuit", personality: "", mood: 70, xp: 0 };
function perm(over: Partial<PermissionRequest> = {}): PermissionRequest {
  return {
    requestId: "r1",
    machineId: MID,
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

describe("derivePetViews across machines", () => {
  // Same agent id ("claude-code") on two machines: global ids differ, pets must never mix.
  const here: Pet = { ...pet, id: globalId(MID, "pet-claude-code"), name: "Biscuit" };
  const there: Pet = { ...pet, id: globalId(MID2, "pet-claude-code"), machineId: MID2, name: "Pepper" };
  const hereSession = session({ id: globalId(MID, "s1"), status: "working", lastEventAt: 900 });
  const thereSession = session({ id: globalId(MID2, "s1"), machineId: MID2, status: "thinking", lastEventAt: 950 });
  const machines: Record<string, Machine> = {
    [MID]: { id: MID, name: "Windows PC", os: "windows", lastSeen: 1, online: true },
    [MID2]: { id: MID2, name: "Mac mini", os: "macos", lastSeen: 1, online: true },
  };
  const both = (over: Partial<DerivePetViewsInput> = {}) => input({ pets: [here, there], machines, ...over });

  test("views carry machineId and machineName", () => {
    const [a, b] = derivePetViews(both());
    expect(a).toMatchObject({ petId: here.id, machineId: MID, machineName: "Windows PC", name: "Biscuit" });
    expect(b).toMatchObject({ petId: there.id, machineId: MID2, machineName: "Mac mini", name: "Pepper" });
  });

  test("machineName is undefined without (or outside) the machines map", () => {
    const [a] = derivePetViews(input({ pets: [here] }));
    expect(a!.machineId).toBe(MID);
    expect(a!.machineName).toBeUndefined();
    const [b] = derivePetViews(input({ pets: [there], machines: { [MID]: machines[MID]! } }));
    expect(b!.machineName).toBeUndefined();
  });

  test("each pet follows only its own machine's session", () => {
    const [a, b] = derivePetViews(both({ sessions: [hereSession, thereSession] }));
    expect(a).toMatchObject({ activity: "working", sessionId: hereSession.id });
    expect(b).toMatchObject({ activity: "thinking", sessionId: thereSession.id });
  });

  test("a session on the other machine does not wake this machine's pet", () => {
    const [a, b] = derivePetViews(both({ sessions: [thereSession] }));
    expect(a!.activity).toBe("napping");
    expect(a!.sessionId).toBeUndefined();
    expect(b!.activity).toBe("thinking");
  });

  test("a pending request goes to the pet on the request's machine only", () => {
    const req = perm({ requestId: globalId(MID2, "r1"), machineId: MID2, sessionId: thereSession.id });
    const [a, b] = derivePetViews(both({ sessions: [hereSession, thereSession], pending: [req] }));
    expect(a!.activity).toBe("working");
    expect(a!.bubble).toBeUndefined();
    expect(b).toMatchObject({ activity: "alert", sessionId: thereSession.id });
    expect(b!.bubble).toMatchObject({ kind: "permission", requestId: req.requestId });
  });

  test("a pending request matches by (machineId, agentId) even before its session is known", () => {
    const req = perm({ requestId: globalId(MID2, "r1"), machineId: MID2, sessionId: globalId(MID2, "unknown") });
    const [a, b] = derivePetViews(both({ pending: [req] }));
    expect(a!.activity).toBe("napping");
    expect(b!.activity).toBe("alert");
  });

  test("pending requests on both machines are counted per pet", () => {
    const mine = perm({ requestId: globalId(MID, "r1"), sessionId: hereSession.id, createdAt: 10 });
    const theirs1 = perm({ requestId: globalId(MID2, "r1"), machineId: MID2, sessionId: thereSession.id, createdAt: 5 });
    const theirs2 = perm({ requestId: globalId(MID2, "r2"), machineId: MID2, sessionId: thereSession.id, createdAt: 6 });
    const [a, b] = derivePetViews(both({ sessions: [hereSession, thereSession], pending: [theirs2, mine, theirs1] }));
    expect(a!.bubble!.requestId).toBe(mine.requestId);
    expect(a!.bubble!.text).not.toContain("more");
    expect(b!.bubble!.requestId).toBe(theirs1.requestId);
    expect(b!.bubble!.text).toContain("+1 more");
  });

  test("last events are looked up by global session id", () => {
    const plan = ev("plan", { entries: [] }, 960, thereSession.id, MID2);
    const [a, b] = derivePetViews(both({ sessions: [hereSession, thereSession], lastEvents: { [thereSession.id]: plan } }));
    expect(a!.activity).toBe("working");
    expect(b!.activity).toBe("planning");
  });

  test("decay timers are tracked per machine", () => {
    const done = session({ id: globalId(MID2, "s1"), machineId: MID2, status: "done", lastEventAt: 1000 });
    const stop = ev("stop", { stopReason: "end_turn" }, 1000, done.id, MID2);
    const at = both({ sessions: [done], lastEvents: { [done.id]: stop }, now: 1500 });
    const [a, b] = derivePetViews(at);
    expect(a!.activity).toBe("napping");
    expect(b!.activity).toBe("happy");
    expect(nextPetViewChange(at)).toBe(1000 + HAPPY_DECAY_MS);
  });
});

describe("visitors and the active machine (L/A rules)", () => {
  const L = MID;
  const M = MID2;
  const C = "atticbox0003";
  const machines: Record<string, Machine> = {
    [L]: { id: L, name: "Windows PC", os: "windows", lastSeen: 1000, online: true },
    [M]: { id: M, name: "Mac mini", os: "macos", lastSeen: 1000, online: true },
    [C]: { id: C, name: "Attic box", os: "linux", lastSeen: 900, online: false },
  };
  const home = pet;
  const visitor: Pet = { ...pet, id: `${M}:p1`, machineId: M, name: "Pepper" };
  const sleeper: Pet = { ...pet, id: `${C}:p1`, machineId: C, name: "Rex" };
  const homeReq = perm();
  const visitorReq = perm({ requestId: `${M}:r1`, machineId: M, sessionId: `${M}:s1` });
  const sleeperReq = perm({ requestId: `${C}:r1`, machineId: C, sessionId: `${C}:s1` });
  const place = (over: Partial<DerivePetViewsInput> = {}) =>
    derivePetViews(input({ pets: [home, visitor, sleeper], machines, localMachineId: L, now: 2000, ...over }));

  test("without localMachineId every pet is a home pet (compatibility)", () => {
    const views = derivePetViews(input({ pets: [home, visitor], machines }));
    expect(views.map((v) => [v.visitor, v.remoteOffline, v.away])).toEqual([
      [false, false, undefined],
      [false, false, undefined],
    ]);
  });

  test("other machines' pets are visitors when online, remoteOffline otherwise", () => {
    const [h, v, o] = place();
    expect([h!.visitor, h!.remoteOffline]).toEqual([false, false]);
    expect([v!.visitor, v!.remoteOffline, v!.machineName]).toEqual([true, false, "Mac mini"]);
    expect([o!.visitor, o!.remoteOffline]).toEqual([false, true]);
  });

  test("a machine missing from `machines` counts as offline", () => {
    const [, v] = derivePetViews(input({ pets: [home, visitor], localMachineId: L, now: 2000 }));
    expect(v!.remoteOffline).toBe(true);
    expect(v!.visitor).toBe(false);
  });

  test("A == L: home pet keeps its Approve/Deny bubble and stays home", () => {
    const [h] = place({ pending: [homeReq], activeMachineId: L });
    expect(h!.away).toBeUndefined();
    expect(h!.activity).toBe("alert");
    expect(h!.bubble).toMatchObject({ kind: "permission", requestId: "r1" });
    expect(h!.bubble?.readOnly).toBeUndefined();
    expect(h!.bubble?.confirm).toBeUndefined();
  });

  test("A != L: home pet with a pending approval runs to A", () => {
    const [h] = place({ pending: [homeReq], activeMachineId: M });
    expect(h!.away).toBe(M);
    expect(h!.activity).toBe("alert");
  });

  test("activeMachineId defaults to localMachineId", () => {
    const [h] = place({ pending: [homeReq] });
    expect(h!.away).toBeUndefined();
  });

  test("a home pet without an approval never goes away", () => {
    const [h] = place({ activeMachineId: M, sessions: [session({ status: "working" })] });
    expect(h!.away).toBeUndefined();
    expect(h!.activity).toBe("working");
  });

  test("A == L: a visitor with a pending approval ran here; approving needs a confirm step", () => {
    const [, v] = place({ pending: [visitorReq], activeMachineId: L });
    expect(v!.activity).toBe("alert");
    expect(v!.bubble).toMatchObject({ kind: "permission", requestId: `${M}:r1`, confirm: { machineName: "Mac mini", action: "Run npm test" } });
    expect(v!.bubble!.readOnly).toBeUndefined();
    expect(v!.bubble!.text).toBe("Run npm test? (on Mac mini)");
    expect(approvalConfirmText(v!.bubble!)).toBe("Approve on Mac mini: Run npm test?");
    expect(v!.away).toBeUndefined();
  });

  test("a visitor's request without allow_once cannot be approved from here (always-allow is chosen there)", () => {
    const alwaysOnly = { ...visitorReq, options: [{ optionId: "always", name: "Always", kind: "allow_always" as const }] };
    const [, v] = place({ pending: [alwaysOnly], activeMachineId: L });
    expect(v!.bubble?.approveBlocked).toBe("Choose always-allow on Mac mini");
    const [, ok] = place({ pending: [visitorReq], activeMachineId: L });
    expect(ok!.bubble?.approveBlocked).toBeUndefined();
  });

  test("a home pet's own request needs no confirm step", () => {
    const [h] = place({ pending: [homeReq], activeMachineId: L });
    expect(h!.bubble?.confirm).toBeUndefined();
    expect(approvalConfirmText(h!.bubble!)).toBeNull();
  });

  test("A != L: a visitor with a pending approval waits without a bubble", () => {
    for (const a of [M, "thirdbox0004"]) {
      const [, v] = place({ pending: [visitorReq], activeMachineId: a });
      expect(v!.activity).toBe("alert");
      expect(v!.bubble).toBeUndefined();
    }
  });

  test("visitors keep their activity but drop chatter bubbles", () => {
    const vs: Session = session({ id: `${M}:s1`, machineId: M, status: "done", lastEventAt: 1900 });
    const stop = ev("stop", { stopReason: "end_turn" }, 1900, vs.id, M);
    const [, v] = place({ sessions: [vs], lastEvents: { [vs.id]: stop } });
    expect(v!.activity).toBe("happy");
    expect(v!.bubble).toBeUndefined();
  });

  test("pets of an offline machine nap without bubbles, even with a stale approval", () => {
    const [, , o] = place({ pending: [sleeperReq], activeMachineId: L });
    expect(o!.activity).toBe("napping");
    expect(o!.bubble).toBeUndefined();
    expect(o!.sessionId).toBeUndefined();
    expect(o!.since).toBe(900);
  });

  test("stageShowsApproval: only a home pet's own request (not away) or a visitor that ran here", () => {
    // A == L: the home pet shows its request here.
    expect(stageShowsApproval(place({ pending: [homeReq], activeMachineId: L }))).toBe(true);
    // A != L: it ran away; nothing is shown here.
    expect(stageShowsApproval(place({ pending: [homeReq], activeMachineId: M }))).toBe(false);
    // A visitor's request shows only when it ran here (A == L).
    expect(stageShowsApproval(place({ pending: [visitorReq], activeMachineId: L }))).toBe(true);
    expect(stageShowsApproval(place({ pending: [visitorReq], activeMachineId: M }))).toBe(false);
    // An offline machine's stale request, or no request at all.
    expect(stageShowsApproval(place({ pending: [sleeperReq], activeMachineId: L }))).toBe(false);
    expect(stageShowsApproval(place())).toBe(false);
    // Say-bubbles never count.
    expect(stageShowsApproval(place({ sessions: [session({ status: "waiting_permission" })] }))).toBe(false);
  });

  test("an offline machine's approval does not send home pets away", () => {
    const [h] = place({ pending: [sleeperReq], activeMachineId: L });
    expect(h!.away).toBeUndefined();
    expect(h!.activity).toBe("napping");
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
