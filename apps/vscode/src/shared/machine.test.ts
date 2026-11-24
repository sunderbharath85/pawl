import { describe, expect, test } from "bun:test";
import { HubRequestError, MACHINE_NAME_MAX } from "@krypto/hub-client";
import type { Agent, Machine, Pet } from "@krypto/protocol";
import { agentFor, checkMachineName, machineDisplayName, machineSummary, petFor, renameErrorText } from "./machine";

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
