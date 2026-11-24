import { describe, expect, test } from "bun:test";
import { USER_ACTIVE_WINDOW_MS, type Agent, type Machine, type Pet } from "@krypto/protocol";
import {
  activeMachineId,
  agentFor,
  checkMachineName,
  groupByMachine,
  HubRequestError,
  isLocal,
  MACHINE_NAME_MAX,
  machineDisplayName,
  machinesLine,
  machineSummary,
  petFor,
  readOnlyReason,
  renameErrorText,
} from "../src/index";

const A = "aaaaaaaaaaaa";
const B = "bbbbbbbbbbbb";

const pet = (machineId: string, agentId: string, name: string): Pet => ({
  id: `${machineId}:pet-${agentId}`,
  machineId,
  agentId,
  species: "dog",
  skin: "default",
  name,
  personality: "",
  mood: 70,
  xp: 0,
});

const agent = (machineId: string, id: string, name: string): Agent => ({
  id,
  machineId,
  name,
  command: [id],
  installed: true,
  source: "registry",
});

describe("petFor / agentFor", () => {
  test("pets match by (machineId, agentId), never by agent id alone", () => {
    const pets = [pet(A, "claude-code", "Biscuit"), pet(B, "claude-code", "Pepper")];
    expect(petFor(pets, B, "claude-code")?.name).toBe("Pepper");
    expect(petFor(pets, A, "claude-code")?.name).toBe("Biscuit");
    expect(petFor(pets, A, "codex")).toBeUndefined();
  });

  test("agents prefer the exact machine and fall back to the id", () => {
    const agents = [agent(A, "codex", "Codex (A)"), agent(B, "codex", "Codex (B)")];
    expect(agentFor(agents, "codex", B)?.name).toBe("Codex (B)");
    expect(agentFor(agents, "codex", "cccccccccccc")?.name).toBe("Codex (A)");
    expect(agentFor(agents, "codex")?.name).toBe("Codex (A)");
    expect(agentFor(agents, "gemini", A)).toBeUndefined();
  });
});

describe("machine names", () => {
  const machine: Machine = { id: A, name: "Windows PC", os: "windows", lastSeen: 0, online: true };

  test("display name and summary", () => {
    expect(machineDisplayName(machine)).toBe("Windows PC");
    expect(machineDisplayName(undefined)).toBe("This machine");
    expect(machineDisplayName({ ...machine, name: "  " })).toBe("This machine");
    expect(machineSummary(machine)).toBe(`Windows PC · Windows · id ${A}`);
  });

  test("checkMachineName trims and enforces 1–40 characters", () => {
    expect(checkMachineName("  Mac mini ")).toEqual({ ok: true, name: "Mac mini" });
    expect(checkMachineName("   ")).toMatchObject({ ok: false, error: "Give this machine a name." });
    expect(checkMachineName("x".repeat(MACHINE_NAME_MAX))).toMatchObject({ ok: true });
    expect(checkMachineName("x".repeat(MACHINE_NAME_MAX + 1)).ok).toBe(false);
  });

  test("renameErrorText maps hub and client error codes", () => {
    expect(renameErrorText(new HubRequestError("not_owner", "update_machine"))).toBe("Only this machine's own hub can rename it.");
    expect(renameErrorText(new HubRequestError("no_machine", "update_machine"))).toContain("predates machine names");
    expect(renameErrorText(new HubRequestError("invalid", "update_machine"))).toContain(`1–${MACHINE_NAME_MAX}`);
    expect(renameErrorText(new HubRequestError("timeout", "update_machine", "timed out"))).toBe("timed out");
    expect(renameErrorText("boom")).toBe("boom");
  });
});

test("machinesLine", () => {
  expect(machinesLine(3, 2)).toBe("3 machines · 2 online");
  expect(machinesLine(1, 1)).toBe("1 machine · 1 online");
  expect(machinesLine(2)).toBe("2 machines");
});

describe("activeMachineId", () => {
  const NOW = 10_000_000;
  const me: Machine = { id: A, name: "Windows PC", os: "windows", lastSeen: NOW, online: true };
  const mac: Machine = { id: B, name: "Mac mini", os: "macos", lastSeen: NOW, online: true };
  const C = "cccccccccccc";
  const box: Machine = { id: C, name: "Attic box", os: "linux", lastSeen: NOW, online: true };
  const state = (...machines: Machine[]) => ({ machine: me, machines: Object.fromEntries(machines.map((m) => [m.id, m])) });

  test("the online machine with the latest activity wins", () => {
    const s = state({ ...me, userActiveAt: NOW - 60_000 }, { ...mac, userActiveAt: NOW - 1_000 }, { ...box, userActiveAt: NOW - 5_000 });
    expect(activeMachineId(s, NOW)).toBe(B);
  });

  test("offline machines and stale activity do not count", () => {
    expect(activeMachineId(state({ ...me, userActiveAt: NOW - 60_000 }, { ...mac, online: false, userActiveAt: NOW }), NOW)).toBe(A);
    expect(activeMachineId(state(me, { ...mac, userActiveAt: NOW - USER_ACTIVE_WINDOW_MS - 1 }), NOW)).toBe(A);
    expect(activeMachineId(state(me, { ...mac, userActiveAt: NOW - USER_ACTIVE_WINDOW_MS }), NOW)).toBe(B);
  });

  test("nobody active: this machine; this machine counts as online even if flagged otherwise", () => {
    expect(activeMachineId(state(me, mac), NOW)).toBe(A);
    expect(activeMachineId(state({ ...me, online: false, userActiveAt: NOW }, { ...mac, userActiveAt: NOW - 10 }), NOW)).toBe(A);
  });

  test("ties go to this machine, then to the lower id", () => {
    expect(activeMachineId(state({ ...me, userActiveAt: NOW }, { ...mac, userActiveAt: NOW }), NOW)).toBe(A);
    expect(activeMachineId(state(me, { ...box, userActiveAt: NOW }, { ...mac, userActiveAt: NOW }), NOW)).toBe(B);
  });

  test("before the hub introduced this machine", () => {
    expect(activeMachineId({ machine: undefined, machines: {} }, NOW)).toBe("");
    expect(activeMachineId({ machine: undefined, machines: { [B]: { ...mac, userActiveAt: NOW } } }, NOW)).toBe(B);
  });
});

describe("isLocal", () => {
  const state = { machine: { id: A, name: "Windows PC", os: "windows" as const, lastSeen: 0, online: true } };

  test("machine ids and global ids of this machine are local", () => {
    expect(isLocal(state, A)).toBe(true);
    expect(isLocal(state, `${A}:session-1`)).toBe(true);
    expect(isLocal(state, `${A}:42`)).toBe(true);
    expect(isLocal(state, B)).toBe(false);
    expect(isLocal(state, `${B}:session-1`)).toBe(false);
  });

  test("unknown shapes are not local; everything is local before the hub said hello", () => {
    expect(isLocal(state, "local-dog")).toBe(false);
    expect(isLocal({ machine: undefined }, `${B}:x`)).toBe(true);
  });
});

describe("groupByMachine / readOnlyReason", () => {
  const C = "cccccccccccc";
  const me: Machine = { id: A, name: "Windows PC", os: "windows", lastSeen: 0, online: true };
  const zed: Machine = { id: B, name: "Zed box", os: "linux", lastSeen: 0, online: true };
  const attic: Machine = { id: C, name: "Attic", os: "linux", lastSeen: 0, online: false };
  const state = { machine: me, machines: { [A]: me, [B]: zed, [C]: attic } };
  const item = (machineId: string, n: number) => ({ machineId, n });

  test("this machine first, then online, then by name; unknown machines last; order kept inside", () => {
    const groups = groupByMachine(state, [item(C, 1), item("unknown00000", 2), item(B, 3), item(A, 4), item(B, 5)]);
    expect(groups.map((g) => g.machineId)).toEqual([A, B, C, "unknown00000"]);
    expect(groups[0]).toMatchObject({ local: true, online: true, machine: me });
    expect(groups[1]!.items.map((i) => i.n)).toEqual([3, 5]);
    expect(groups[2]).toMatchObject({ local: false, online: false });
    expect(groups[3]).toMatchObject({ machine: undefined, online: false });
  });

  test("before the hub said hello everything is one local group", () => {
    const groups = groupByMachine({ machine: undefined, machines: {} }, [item(A, 1)]);
    expect(groups).toHaveLength(1);
    expect(groups[0]).toMatchObject({ local: true, online: true });
  });

  test("readOnlyReason names the owning machine", () => {
    expect(readOnlyReason(state, A)).toBeNull();
    expect(readOnlyReason(state, B)).toBe("Change it on Zed box: pets belong to their machine.");
    expect(readOnlyReason(state, "unknown00000")).toContain("Change it on its own machine");
  });
});
