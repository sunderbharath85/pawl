import { describe, expect, test } from "bun:test";
import {
  applyAck,
  applyWelcome,
  initialHubState,
  lastEventsBySession,
  latestEventSeq,
  machineCounts,
  mergeEvents,
  reduceServerMsg,
  removeDevice,
  scopeToMachine,
  sortedMachines,
  upsertMachine,
} from "../src/state";
import { device2, event, machine, machine2, MID, MID2, peerOff, peerOn, permission, pet, session, welcome } from "./fixtures";

const small = { eventCap: 3, chatCap: 10, notificationCap: 2 };

describe("applyWelcome", () => {
  test("replaces snapshot collections and marks connected", () => {
    const s = applyWelcome({ ...initialHubState(), lastError: "x", events: { s1: [event(1)] } }, welcome);
    expect(s.status).toBe("connected");
    expect(s.hubVersion).toBe("0.1.0");
    expect(s.lastError).toBeUndefined();
    expect(s.agents).toHaveLength(1);
    expect(Object.keys(s.sessions)).toEqual(["s1"]);
    expect(s.pets.p1?.name).toBe("Biscuit");
    expect(Object.keys(s.pending)).toEqual(["r1"]);
    expect(s.rules).toHaveLength(1);
    expect(s.events.s1).toHaveLength(1); // incremental data kept
  });
});

describe("reduceServerMsg", () => {
  const base = applyWelcome(initialHubState(), welcome);

  test("ack is a no-op", () => {
    expect(reduceServerMsg(base, { type: "ack", id: "x", ok: true })).toBe(base);
  });

  test("agents replaces list", () => {
    expect(reduceServerMsg(base, { type: "agents", agents: [] }).agents).toEqual([]);
  });

  test("session upserts immutably", () => {
    const s = reduceServerMsg(base, { type: "session", session: session("s1", { status: "working" }) });
    expect(s.sessions.s1?.status).toBe("working");
    const s2 = reduceServerMsg(s, { type: "session", session: session("s2") });
    expect(Object.keys(s2.sessions).sort()).toEqual(["s1", "s2"]);
    expect(base.sessions.s1?.status).toBe("idle");
  });

  test("event appends per session, dedupes and caps", () => {
    let s = base;
    for (let i = 1; i <= 5; i++) s = reduceServerMsg(s, { type: "event", event: event(i) }, small);
    expect(s.events.s1!.map((e) => e.seq)).toEqual([3, 4, 5]);
    const again = reduceServerMsg(s, { type: "event", event: event(5, "s1", { type: "stop" }) }, small);
    expect(again.events.s1!.map((e) => e.seq)).toEqual([3, 4, 5]);
    expect(again.events.s1![2]!.type).toBe("stop");
    const other = reduceServerMsg(s, { type: "event", event: event(6, "s2") });
    expect(other.events.s2!.map((e) => e.seq)).toEqual([6]);
    expect(other.events.s1).toBe(s.events.s1);
  });

  test("default event cap is 500 per session", () => {
    let s = base;
    for (let i = 1; i <= 520; i++) s = reduceServerMsg(s, { type: "event", event: event(i) });
    expect(s.events.s1).toHaveLength(500);
    expect(s.events.s1![0]!.seq).toBe(21);
  });

  test("permission_request / permission_resolved", () => {
    const s = reduceServerMsg(base, { type: "permission_request", request: permission("r2") });
    expect(Object.keys(s.pending).sort()).toEqual(["r1", "r2"]);
    const s2 = reduceServerMsg(s, { type: "permission_resolved", requestId: "r1", sessionId: "s1", outcome: { kind: "cancelled" }, by: "user" });
    expect(Object.keys(s2.pending)).toEqual(["r2"]);
    const same = reduceServerMsg(s2, { type: "permission_resolved", requestId: "nope", sessionId: "s1", outcome: { kind: "cancelled" }, by: "rule" });
    expect(same).toBe(s2);
    // automatic cancel by the hub
    const s3 = reduceServerMsg(s2, { type: "permission_resolved", requestId: "r2", sessionId: "s1", outcome: { kind: "cancelled" }, by: "system" });
    expect(Object.keys(s3.pending)).toEqual([]);
  });

  test("pet upserts", () => {
    expect(reduceServerMsg(base, { type: "pet", pet: { ...pet, species: "cat" } }).pets.p1?.species).toBe("cat");
  });

  test("chat keeps ts order and dedupes by id", () => {
    let s = reduceServerMsg(base, { type: "chat", message: { id: "m2", machineId: MID, role: "pet", text: "woof", ts: 20 } });
    s = reduceServerMsg(s, { type: "chat", message: { id: "m1", machineId: MID, role: "user", text: "hi", ts: 10 } });
    s = reduceServerMsg(s, { type: "chat", message: { id: "m2", machineId: MID, role: "pet", text: "woof!", ts: 20 } });
    expect(s.chat.map((m) => m.id)).toEqual(["m1", "m2"]);
    expect(s.chat[1]!.text).toBe("woof!");
  });

  test("notification appends with cap", () => {
    let s = base;
    for (let i = 0; i < 5; i++) {
      s = reduceServerMsg(s, { type: "notification", notification: { id: `n${i}`, machineId: MID, level: "info", title: "t", body: "b", ts: i } }, small);
    }
    expect(s.notifications.map((n) => n.id)).toEqual(["n3", "n4"]);
  });

  test("unknown message type is ignored", () => {
    expect(reduceServerMsg(base, { type: "future_thing" } as never)).toBe(base);
  });
});

describe("applyAck", () => {
  const base = applyWelcome(initialHubState(), welcome);

  test("start_session / resume_session upsert the session", () => {
    expect(applyAck(base, { type: "start_session", agentId: "claude-code", cwd: "/x" }, { session: session("s9") }).sessions.s9).toBeDefined();
    expect(
      applyAck(base, { type: "resume_session", agentId: "claude-code", acpSessionId: "a", cwd: "/x" }, { session: session("s8") }).sessions.s8,
    ).toBeDefined();
  });

  test("get_events merges sorted into state", () => {
    const s = applyAck(base, { type: "get_events", sessionId: "s1" }, { events: [event(3), event(1), event(2)] });
    expect(s.events.s1!.map((e) => e.seq)).toEqual([1, 2, 3]);
  });

  test("resolve_permission removes pending optimistically", () => {
    expect(applyAck(base, { type: "resolve_permission", requestId: "r1", optionId: "o-allow" }, {}).pending.r1).toBeUndefined();
  });

  test("update_pet / set_rules / list_agents / list_sessions / chat", () => {
    let s = applyAck(base, { type: "update_pet", petId: "p1", patch: { name: "Rex" } }, { pet: { ...pet, name: "Rex" } });
    expect(s.pets.p1?.name).toBe("Rex");
    s = applyAck(s, { type: "set_rules", rules: [] }, { rules: [] });
    expect(s.rules).toEqual([]);
    s = applyAck(s, { type: "list_agents" }, { agents: [] });
    expect(s.agents).toEqual([]);
    s = applyAck(s, { type: "list_sessions" }, { sessions: [session("s5")] });
    expect(Object.keys(s.sessions).sort()).toEqual(["s1", "s5"]);
    s = applyAck(s, { type: "chat", text: "hi" }, { message: { id: "m1", machineId: MID, role: "user", text: "hi", ts: 1 } });
    expect(s.chat).toHaveLength(1);
  });

  test("prompt ack and missing results leave state unchanged", () => {
    expect(applyAck(base, { type: "prompt", sessionId: "s1", text: "x" }, {})).toBe(base);
    expect(applyAck(base, { type: "start_session", agentId: "a", cwd: "/" }, undefined)).toBe(base);
  });
});

test("mergeEvents keeps order and handles out-of-order input", () => {
  expect(mergeEvents([event(1), event(3)], [event(2)], 10).map((e) => e.seq)).toEqual([1, 2, 3]);
  expect(mergeEvents(undefined, [], 10)).toEqual([]);
});

test("lastEventsBySession picks the newest event per session", () => {
  const last = lastEventsBySession({ events: { s1: [event(1), event(2)], s2: [event(7, "s2")] } });
  expect(last.s1?.seq).toBe(2);
  expect(last.s2?.seq).toBe(7);
});

describe("machines (protocol v2)", () => {
  test("initial state knows no machine", () => {
    const s = initialHubState();
    expect(s.machine).toBeUndefined();
    expect(s.machines).toEqual({});
  });

  test("applyWelcome stores this hub's machine and the machines snapshot", () => {
    const s = applyWelcome(initialHubState(), { ...welcome, machines: [machine, machine2] });
    expect(s.machine).toEqual(machine);
    expect(Object.keys(s.machines).sort()).toEqual([MID, MID2].sort());
    expect(s.machines[MID2]?.name).toBe("Mac mini");
  });

  test("applyWelcome always includes this hub's machine, even if `machines` omits it", () => {
    const s = applyWelcome(initialHubState(), { ...welcome, machines: [machine2] });
    expect(s.machines[MID]).toEqual(machine);
    expect(s.machines[MID2]).toEqual(machine2);
  });

  test("applyWelcome replaces stale machines but keeps our machine if a (pre-v2) hub omits it", () => {
    const first = applyWelcome(initialHubState(), { ...welcome, machines: [machine, machine2] });
    const again = applyWelcome(first, { ...welcome, machines: [machine] });
    expect(Object.keys(again.machines)).toEqual([MID]);
    const legacy = applyWelcome(first, { ...welcome, machine: undefined, machines: undefined } as never);
    expect(legacy.machine).toEqual(machine);
    expect(legacy.machines).toEqual({});
  });

  test("machine message renames this hub's machine", () => {
    const base = applyWelcome(initialHubState(), welcome);
    const renamed = { ...machine, name: "Gaming PC" };
    const s = reduceServerMsg(base, { type: "machine", machine: renamed });
    expect(s.machine?.name).toBe("Gaming PC");
    expect(s.machines[MID]?.name).toBe("Gaming PC");
    expect(base.machine?.name).toBe("Windows PC"); // immutable
  });

  test("machine message for a peer adds it without touching this hub's machine", () => {
    const base = applyWelcome(initialHubState(), welcome);
    const s = reduceServerMsg(base, { type: "machine", machine: machine2 });
    expect(s.machine).toBe(base.machine);
    expect(s.machines[MID2]).toEqual(machine2);
    expect(s.machines[MID]).toEqual(machine);
  });

  test("upsertMachine before any Welcome does not invent a local machine", () => {
    const s = upsertMachine(initialHubState(), machine2);
    expect(s.machine).toBeUndefined();
    expect(s.machines[MID2]).toEqual(machine2);
  });

  test("update_machine ack folds the result into state", () => {
    const base = applyWelcome(initialHubState(), welcome);
    const s = applyAck(base, { type: "update_machine", machineId: MID, name: "Desk" }, { machine: { ...machine, name: "Desk" } });
    expect(s.machine?.name).toBe("Desk");
    expect(s.machines[MID]?.name).toBe("Desk");
    expect(applyAck(base, { type: "update_machine", machineId: MID, name: "Desk" }, undefined)).toBe(base);
  });
});

describe("events keyed by global id, ordered by seq", () => {
  test("ids are global strings and seq is the cursor", () => {
    const s = reduceServerMsg(initialHubState(), { type: "event", event: event(7) });
    expect(s.events.s1![0]!.id).toBe(`${MID}:7`);
    expect(latestEventSeq(s, "s1")).toBe(7);
    expect(latestEventSeq(s, "nope")).toBeUndefined();
  });

  test("live event and get_events result with the same id are deduped", () => {
    let s = reduceServerMsg(initialHubState(), { type: "event", event: event(2) });
    s = applyAck(s, { type: "get_events", sessionId: "s1", sinceSeq: 0 }, { events: [event(1), event(2), event(3)] });
    expect(s.events.s1!.map((e) => e.id)).toEqual([`${MID}:1`, `${MID}:2`, `${MID}:3`]);
    s = reduceServerMsg(s, { type: "event", event: event(3, "s1", { type: "stop" }) });
    expect(s.events.s1).toHaveLength(3);
    expect(s.events.s1![2]!.type).toBe("stop");
  });

  test("same seq on two machines are distinct events (dedupe is by id, not seq)", () => {
    const merged = mergeEvents([event(5)], [event(5, "s1", { machineId: MID2 })], 10);
    expect(merged.map((e) => e.id).sort()).toEqual([`${MID2}:5`, `${MID}:5`].sort());
    // Re-delivering either one does not duplicate it.
    const again = mergeEvents(merged, [event(5, "s1", { machineId: MID2 })], 10);
    expect(again).toHaveLength(2);
  });

  test("late (lower seq) live event is inserted in order", () => {
    let s = initialHubState();
    for (const seq of [1, 3, 2]) s = reduceServerMsg(s, { type: "event", event: event(seq) });
    expect(s.events.s1!.map((e) => e.seq)).toEqual([1, 2, 3]);
  });

  test("cap keeps the newest by seq", () => {
    const merged = mergeEvents(undefined, [event(9), event(2), event(5), event(7)], 2);
    expect(merged.map((e) => e.seq)).toEqual([7, 9]);
  });
});

describe("pairing roster + peer link (protocol v3)", () => {
  const paired = applyWelcome(initialHubState(), { ...welcome, machines: [machine, machine2], devices: [device2], peer: peerOn });

  test("initial state has an empty roster and no peer status", () => {
    const s = initialHubState();
    expect(s.devices).toEqual([]);
    expect(s.peer).toBeUndefined();
  });

  test("applyWelcome stores devices and peer status, replacing the previous snapshot", () => {
    expect(paired.devices).toEqual([device2]);
    expect(paired.peer).toEqual(peerOn);
    const again = applyWelcome(paired, welcome);
    expect(again.devices).toEqual([]);
    expect(again.peer).toEqual(peerOff);
  });

  test("a pre-v3 Welcome (no devices / peer) leaves an empty roster and no peer status", () => {
    const legacy = applyWelcome(paired, { ...welcome, devices: undefined, peer: undefined } as never);
    expect(legacy.devices).toEqual([]);
    expect(legacy.peer).toBeUndefined();
  });

  test("devices replaces the roster", () => {
    const s = reduceServerMsg(applyWelcome(initialHubState(), welcome), { type: "devices", devices: [device2] });
    expect(s.devices).toEqual([device2]);
    expect(reduceServerMsg(s, { type: "devices", devices: [] }).devices).toEqual([]);
  });

  test("peer_status replaces the listener status", () => {
    const s = reduceServerMsg(paired, { type: "peer_status", peer: peerOff });
    expect(s.peer).toEqual(peerOff);
    expect(paired.peer).toEqual(peerOn); // immutable
  });

  test("machine_removed drops the machine and its roster entry", () => {
    const s = reduceServerMsg(paired, { type: "machine_removed", machineId: MID2 });
    expect(Object.keys(s.machines)).toEqual([MID]);
    expect(s.devices).toEqual([]);
    expect(paired.machines[MID2]).toEqual(machine2); // immutable
  });

  test("machine_removed never drops this hub's own machine; unknown ids are a no-op", () => {
    expect(reduceServerMsg(paired, { type: "machine_removed", machineId: MID })).toBe(paired);
    expect(reduceServerMsg(paired, { type: "machine_removed", machineId: "nobody000000" })).toBe(paired);
  });

  test("a machine going offline / online flips in place", () => {
    const online = reduceServerMsg(paired, { type: "machine", machine: { ...machine2, online: true, lastSeen: 50 } });
    expect(online.machines[MID2]?.online).toBe(true);
    const offline = reduceServerMsg(online, { type: "machine", machine: { ...machine2, online: false, lastSeen: 60 } });
    expect(offline.machines[MID2]).toMatchObject({ online: false, lastSeen: 60 });
    expect(offline.machine).toBe(paired.machine);
  });

  test("join_peer ack adds the device and machine; replaying it does not duplicate", () => {
    const base = applyWelcome(initialHubState(), welcome);
    const req = { type: "join_peer" as const, uri: "krypto://peer?x" };
    const s = applyAck(base, req, { device: device2, machine: { ...machine2, online: true } });
    expect(s.devices).toEqual([device2]);
    expect(s.machines[MID2]?.online).toBe(true);
    const again = applyAck(s, req, { device: { ...device2, addr: "10.0.0.2:47801" }, machine: machine2 });
    expect(again.devices).toHaveLength(1);
    expect(again.devices[0]?.addr).toBe("10.0.0.2:47801");
    expect(applyAck(base, req, undefined)).toBe(base);
  });

  test("revoke_device ack removes the device (and its machine) optimistically", () => {
    const s = applyAck(paired, { type: "revoke_device", deviceId: MID2 }, {});
    expect(s.devices).toEqual([]);
    expect(s.machines[MID2]).toBeUndefined();
    expect(s.machines[MID]).toEqual(machine);
  });

  test("removeDevice also forgets a roster-only device (e.g. a phone, which has no machine)", () => {
    const phone = { ...device2, id: "phone-1", kind: "phone" as const, name: "Pixel" };
    const s = removeDevice({ ...paired, devices: [device2, phone] }, "phone-1");
    expect(s.devices).toEqual([device2]);
    expect(s.machines).toBe(paired.machines);
  });

  test("create_invite ack leaves state unchanged (the URI is never stored)", () => {
    const s = applyAck(paired, { type: "create_invite" }, { uri: "krypto://peer?secret", expiresAt: 5 });
    expect(s).toBe(paired);
    expect(JSON.stringify(s)).not.toContain("secret");
  });
});

describe("machine list helpers", () => {
  const third = { ...machine2, id: "linuxbox0003", name: "Attic box", os: "linux" as const, online: true };
  const state = applyWelcome(initialHubState(), { ...welcome, machines: [machine2, third, machine] });

  test("sortedMachines: this machine first, then online, then by name", () => {
    expect(sortedMachines(state).map((m) => m.id)).toEqual([MID, "linuxbox0003", MID2]);
  });

  test("machineCounts counts every machine, this one included", () => {
    expect(machineCounts(state)).toEqual({ total: 3, online: 2 });
    expect(machineCounts(initialHubState())).toEqual({ total: 0, online: 0 });
  });
});

describe("scopeToMachine", () => {
  const remotePet = { ...pet, id: `${MID2}:p1`, machineId: MID2 };
  const remoteSession = session(`${MID2}:s1`, { machineId: MID2 });
  const remoteReq = permission(`${MID2}:r1`, { machineId: MID2, sessionId: `${MID2}:s1` });
  const base = applyWelcome(initialHubState(), welcome);

  test("returns the same state when everything is local (or no machine is known)", () => {
    expect(scopeToMachine(base, MID)).toBe(base);
    expect(scopeToMachine(base, undefined)).toBe(base);
  });

  test("hides another machine's pets, sessions and permission requests", () => {
    const mixed = {
      ...base,
      pets: { ...base.pets, [remotePet.id]: remotePet },
      sessions: { ...base.sessions, [remoteSession.id]: remoteSession },
      pending: { ...base.pending, [remoteReq.requestId]: remoteReq },
    };
    const s = scopeToMachine(mixed, MID);
    expect(Object.keys(s.pets)).toEqual(["p1"]);
    expect(Object.keys(s.sessions)).toEqual(["s1"]);
    expect(Object.keys(s.pending)).toEqual(["r1"]);
    expect(s.machines).toBe(mixed.machines);
  });

  test("keeps untouched collections by identity", () => {
    const s = scopeToMachine({ ...base, pets: { ...base.pets, [remotePet.id]: remotePet } }, MID);
    expect(s.sessions).toBe(base.sessions);
    expect(s.pending).toBe(base.pending);
  });
});
