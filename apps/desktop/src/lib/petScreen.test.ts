import { describe, expect, test } from "bun:test";
import { initialHubState, type HubState } from "@pawl/hub-client";
import type { PetView } from "@pawl/pet-engine";
import type { Machine, PermissionRequest, Pet, Session } from "@pawl/protocol";
import { defaultPetState, MAX_SWITCHER_CHIPS, newRequestIds, RECENT_SESSION_MS, repoName, switcherSessions } from "./petScreen";

const ME = "winpc0000001";
const MAC = "macmini00002";
const OFF = "laptop000003";
const NOW = 50_000_000;
const me: Machine = { id: ME, name: "Windows PC", os: "windows", lastSeen: NOW, online: true };
const mac: Machine = { id: MAC, name: "Mac mini", os: "macos", lastSeen: NOW, online: true };
const off: Machine = { id: OFF, name: "Laptop", os: "linux", lastSeen: NOW - 1e6, online: false };

function state(over: Partial<HubState> = {}): HubState {
  return { ...initialHubState(), machine: me, machines: { [ME]: me, [MAC]: mac, [OFF]: off }, ...over };
}

let seq = 0;
function session(over: Partial<Session> = {}): Session {
  seq++;
  const machineId = over.machineId ?? ME;
  return {
    id: `${machineId}:s${seq}`,
    machineId,
    agentId: "claude-code",
    cwd: "E:/work/api",
    source: "owned",
    status: "idle",
    startedAt: NOW - 100_000 + seq,
    lastEventAt: NOW - 1000,
    ...over,
  };
}

function request(s: Session, over: Partial<PermissionRequest> = {}): PermissionRequest {
  seq++;
  return {
    requestId: `${s.machineId}:r${seq}`,
    machineId: s.machineId,
    sessionId: s.id,
    agentId: s.agentId,
    toolCall: { toolCallId: `t${seq}`, title: "Run npm test", kind: "execute", status: "pending" },
    options: [{ optionId: "a", name: "Allow", kind: "allow_once" }],
    createdAt: NOW - 500,
    ...over,
  } as PermissionRequest;
}

function bySession(list: Session[]): Record<string, Session> {
  return Object.fromEntries(list.map((s) => [s.id, s]));
}
function byRequest(list: PermissionRequest[]): Record<string, PermissionRequest> {
  return Object.fromEntries(list.map((r) => [r.requestId, r]));
}

function view(over: Partial<PetView>): PetView {
  return {
    petId: `${ME}:p1`,
    machineId: ME,
    agentId: "claude-code",
    species: "dog",
    name: "Rex",
    activity: "napping",
    since: 0,
    mood: 70,
    visitor: false,
    remoteOffline: false,
    ...over,
  };
}

describe("default pet state: needs you > working > done / snag > idle > napping", () => {
  test("any approval (this machine or an online paired one) means needs you, oldest first", () => {
    const a = session({ status: "working" });
    const b = session({ machineId: MAC, status: "waiting_permission" });
    const ra = request(a, { createdAt: NOW - 100 });
    const rb = request(b, { createdAt: NOW - 900 });
    const st = defaultPetState(state({ sessions: bySession([a, b]), pending: byRequest([ra, rb]) }), []);
    expect(st.mood).toBe("needs_you");
    expect(st.activity).toBe("alert");
    expect(st.pending.map((p) => p.requestId)).toEqual([rb.requestId, ra.requestId]);
    expect(st.line).toBe("2 approvals waiting for you");
    // b has a request to answer, so it is not "waiting in its terminal" too.
    expect(st.waiting).toEqual([]);
  });

  test("an offline machine's approval is left out", () => {
    const s = session({ machineId: OFF, status: "waiting_permission" });
    const st = defaultPetState(state({ sessions: bySession([s]), pending: byRequest([request(s)]) }), []);
    expect(st.pending).toEqual([]);
    expect(st.mood).toBe("napping");
  });

  test("a hook session waiting in its terminal needs you too (nothing to answer here)", () => {
    const s = session({ source: "cli_hook", status: "waiting_permission", agentId: "codex" });
    const st = defaultPetState(state({ sessions: bySession([s]), agents: [{ id: "codex", machineId: ME, name: "Codex", command: [], installed: true, source: "registry" }] }), []);
    expect(st.mood).toBe("needs_you");
    expect(st.pending).toEqual([]);
    expect(st.line).toBe("Codex is waiting for you in api");
  });

  test("a running session anywhere means working; the newest one's refined pose wins", () => {
    const old = session({ status: "working", lastEventAt: NOW - 5000 });
    const recent = session({ machineId: MAC, status: "thinking", lastEventAt: NOW - 100, cwd: "/Users/me/web/" });
    const views = [view({ sessionId: recent.id, activity: "planning" })];
    const st = defaultPetState(state({ sessions: bySession([old, recent]) }), views);
    expect(st.mood).toBe("working");
    expect(st.activity).toBe("planning");
    expect(st.busy.map((s) => s.id)).toEqual([recent.id, old.id]);
    expect(st.line).toBe("Watching 2 busy sessions");
    const one = defaultPetState(state({ sessions: bySession([recent]) }), []);
    expect(one.activity).toBe("thinking");
    expect(one.line).toBe("claude-code is working in web on Mac mini");
  });

  test("ended sessions and offline machines do not count as working", () => {
    const ended = session({ status: "working", endedAt: NOW - 10 });
    const gone = session({ machineId: OFF, status: "working" });
    expect(defaultPetState(state({ sessions: bySession([ended, gone]) }), []).mood).toBe("napping");
  });

  test("with nothing running: a snag, then a finished turn, then idle, then napping", () => {
    const s = state();
    expect(defaultPetState(s, [view({ activity: "happy" }), view({ activity: "sad" })]).mood).toBe("oops");
    expect(defaultPetState(s, [view({ activity: "happy" }), view({ activity: "idle" })]).activity).toBe("happy");
    expect(defaultPetState(s, [view({ activity: "idle" })]).mood).toBe("idle");
    expect(defaultPetState(s, [view({ activity: "napping" })]).mood).toBe("napping");
    expect(defaultPetState(s, []).activity).toBe("napping");
    // Pets of offline machines and pets that ran to another machine do not set the mood.
    expect(defaultPetState(s, [view({ activity: "sad", remoteOffline: true }), view({ activity: "idle", away: MAC })]).mood).toBe("napping");
  });
});

describe("the bubble's session switcher: one pet, sessions one at a time", () => {
  const pets: Record<string, Pet> = {
    [`${ME}:p1`]: { id: `${ME}:p1`, machineId: ME, agentId: "claude-code", species: "cat", skin: "default", name: "Mochi", personality: "", mood: 70, xp: 0 },
  };

  test("needs-you first, then running, then recent; labelled with pet name and repo (+ machine); ended, stale and offline left out", () => {
    const stale = session({ status: "idle", lastEventAt: NOW - RECENT_SESSION_MS - 1 });
    const recent = session({ status: "idle", lastEventAt: NOW - 60_000, cwd: "E:/work/docs" });
    const ended = session({ status: "working", endedAt: NOW });
    const offline = session({ status: "working", machineId: OFF });
    const running = session({ status: "working", lastEventAt: NOW - 5000 });
    const asks = session({ status: "idle", machineId: MAC, agentId: "codex", cwd: "/srv/web", lastEventAt: NOW - 90_000 });
    const waiting = session({ status: "waiting_permission", lastEventAt: NOW - 70_000, cwd: "E:/work/cli" });
    const rq = request(asks);
    const { chips, more } = switcherSessions(
      state({ sessions: bySession([stale, recent, ended, offline, running, asks, waiting]), pending: byRequest([rq]), pets }),
      NOW,
    );
    expect(chips.map((c) => c.sessionId)).toEqual([waiting.id, asks.id, running.id, recent.id]);
    expect(more).toEqual([]);
    expect(chips[1]).toMatchObject({ name: "codex", label: "codex · web · Mac mini", needsYou: true, remote: true });
    expect(chips[1]!.pending.map((p) => p.requestId)).toEqual([rq.requestId]);
    expect(chips[0]).toMatchObject({ name: "Mochi", repo: "cli", needsYou: true, pending: [] });
    expect(chips[2]).toMatchObject({ label: "Mochi · api", running: true, needsYou: false });
  });

  test("at most 5 chips, the rest under more; the focused session always gets a chip", () => {
    const list = Array.from({ length: 8 }, (_, i) => session({ status: "working", lastEventAt: NOW - 1000 * (i + 1) }));
    const s = state({ sessions: bySession(list) });
    const plain = switcherSessions(s, NOW);
    expect(plain.chips).toHaveLength(MAX_SWITCHER_CHIPS);
    expect(plain.more.map((c) => c.sessionId)).toEqual(list.slice(5).map((x) => x.id));
    const focused = switcherSessions(s, NOW, { focus: list[7]!.id });
    expect(focused.chips.map((c) => c.sessionId)).toEqual([...list.slice(0, 4), list[7]!].map((x) => x.id));
    expect(focused.more.map((c) => c.sessionId)).toEqual([list[4]!, list[5]!, list[6]!].map((x) => x.id));
    // A focused session that went quiet stays in the switcher while it is focused.
    const quiet = session({ status: "idle", lastEventAt: NOW - RECENT_SESSION_MS * 2 });
    expect(switcherSessions(state({ sessions: bySession([quiet]) }), NOW, { focus: quiet.id }).chips.map((c) => c.sessionId)).toEqual([quiet.id]);
    expect(switcherSessions(state({ sessions: bySession([quiet]) }), NOW).chips).toEqual([]);
  });
});

describe("helpers", () => {
  test("repoName", () => {
    expect(repoName("E:\\work\\api\\")).toBe("api");
    expect(repoName("/home/me/web")).toBe("web");
    expect(repoName("")).toBe("~");
  });
  test("newRequestIds", () => {
    const s = session();
    const r1 = request(s);
    const r2 = request(s);
    expect(newRequestIds(new Set([r1.requestId]), [r1, r2])).toEqual([r2.requestId]);
    expect(newRequestIds(new Set([r1.requestId, r2.requestId]), [r1])).toEqual([]);
  });
});
