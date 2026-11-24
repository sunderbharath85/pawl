import { describe, expect, test } from "bun:test";
import { HubRequestError } from "@krypto/hub-client";
import type { Pet } from "@krypto/protocol";
import { checkPetName, PET_NAME_MAX, petRenameErrorText } from "./petNames";

const pet = (id: string, name: string, machineId = "aaaaaaaaaaaa"): Pet => ({
  id: `${machineId}:${id}`,
  machineId,
  agentId: id,
  species: "dog",
  skin: "default",
  name,
  personality: "",
  mood: 70,
  xp: 0,
});

const pets = [pet("pet-claude-code", "Biscuit"), pet("pet-codex", "Pepper", "bbbbbbbbbbbb")];
const self = pets[0]!.id;

describe("checkPetName", () => {
  test("trims and accepts 1–40 characters", () => {
    expect(checkPetName("  Waffles ", self, pets)).toEqual({ ok: true, name: "Waffles" });
    expect(checkPetName("x".repeat(PET_NAME_MAX), self, pets).ok).toBe(true);
    expect(checkPetName("🐕".repeat(PET_NAME_MAX), self, pets).ok).toBe(true); // code points, not UTF-16 units
  });

  test("empty or too long is invalid", () => {
    expect(checkPetName("   ", self, pets)).toMatchObject({ ok: false, code: "invalid" });
    expect(checkPetName("x".repeat(PET_NAME_MAX + 1), self, pets)).toMatchObject({ ok: false, code: "invalid" });
  });

  test("a name used by another pet on any machine is taken (case-insensitive)", () => {
    const res = checkPetName(" pepper ", self, pets);
    expect(res).toMatchObject({ ok: false, code: "name_taken" });
    expect(!res.ok && res.error).toContain("Pepper");
  });

  test("a pet may keep (or re-case) its own name", () => {
    expect(checkPetName("BISCUIT", self, pets)).toEqual({ ok: true, name: "BISCUIT" });
  });
});

describe("petRenameErrorText", () => {
  test("maps the hub's name_taken and invalid codes", () => {
    expect(petRenameErrorText(new HubRequestError("name_taken", "update_pet"), "Pepper")).toContain("“Pepper”");
    expect(petRenameErrorText(new HubRequestError("name_taken", "update_pet"))).toContain("Another pet");
    expect(petRenameErrorText(new HubRequestError("invalid", "update_pet"))).toContain(`1–${PET_NAME_MAX}`);
  });

  test("leaves other failures to the caller", () => {
    expect(petRenameErrorText(new HubRequestError("timeout", "update_pet"))).toBeUndefined();
    expect(petRenameErrorText(new Error("boom"))).toBeUndefined();
  });
});
