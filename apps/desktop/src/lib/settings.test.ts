import { describe, expect, test } from "bun:test";
import type { Pet } from "@pawl/protocol";
import { checkDefaultPetName, DEFAULT_PET_ID } from "./petScreen";
import { DEFAULT_SETTINGS, normalizeSettings, type DesktopSettings } from "./settings";

describe("desktop settings", () => {
  test("existing users (no new keys stored) get one default pet named Pawl", () => {
    const old = { notifyPermissions: false, petSize: 96, showPetNames: true, setupOffered: true } as Partial<DesktopSettings>;
    const s = normalizeSettings(old);
    expect(s.petsOnScreen).toBe("default");
    expect(s.petAnimation).toBe("chat");
    expect(s.lastPetName).toBe("");
    expect(s.petNameMigrated).toBe(false);
    expect(s.defaultPetSpecies).toBe("dog");
    expect(s.notifyPermissions).toBe(false);
    expect(s.petSize).toBe(96);
    expect(normalizeSettings(null)).toEqual(DEFAULT_SETTINGS);
  });

  test("malformed values fall back to the defaults", () => {
    const bad = { petsOnScreen: "all", defaultPetSpecies: "dragon", petSize: 9000 } as unknown as Partial<DesktopSettings>;
    const s = normalizeSettings(bad);
    expect(s.petsOnScreen).toBe("default");
    expect(s.defaultPetSpecies).toBe("dog");
    expect(s.petSize).toBe(168);
    expect(normalizeSettings({ petsOnScreen: "sessions", defaultPetSpecies: "cat" }).petsOnScreen).toBe("sessions");
    expect(normalizeSettings({ petAnimation: "always" }).petAnimation).toBe("always");
    expect(normalizeSettings({ petAnimation: "wiggly" } as unknown as Partial<DesktopSettings>).petAnimation).toBe("chat");
    expect(normalizeSettings({ petSize: Number.NaN }).petSize).toBe(DEFAULT_SETTINGS.petSize);
  });
});

describe("every stored field is coerced to its type", () => {
  test("wrong types fall back to the default, field by field; unknown keys are dropped", () => {
    const stored = {
      notifyPermissions: "false",
      notifyTurns: 0,
      notifyHub: false,
      petSize: "120",
      showPetNames: null,
      lastCwd: 42,
      lastAgentId: "codex",
      setupOffered: "yes",
      setupDone: true,
      lastPetName: ["Rex"],
      defaultPetSpecies: 3,
      petsOnScreen: true,
      somethingOld: "x",
    } as unknown as Partial<DesktopSettings>;
    const s = normalizeSettings(stored);
    expect(s).toEqual({ ...DEFAULT_SETTINGS, notifyHub: false, lastAgentId: "codex", setupDone: true });
    expect("somethingOld" in s).toBe(false);
    for (const key of Object.keys(DEFAULT_SETTINGS) as Array<keyof DesktopSettings>) {
      expect(typeof s[key]).toBe(typeof DEFAULT_SETTINGS[key]);
    }
  });

  test("read-aloud voice and speed: system default and 1x; the speed stays between 0.5x and 2x", () => {
    expect(normalizeSettings({}).speechVoice).toBe("");
    expect(normalizeSettings({}).speechRate).toBe(1);
    expect(normalizeSettings({ speechRate: 9 }).speechRate).toBe(2);
    expect(normalizeSettings({ speechRate: 0.1 }).speechRate).toBe(0.5);
    expect(normalizeSettings({ speechRate: Number.NaN }).speechRate).toBe(1);
  });

  test("garbage instead of an object gives the defaults", () => {
    expect(normalizeSettings("nope" as unknown as Partial<DesktopSettings>)).toEqual(DEFAULT_SETTINGS);
    expect(normalizeSettings([] as unknown as Partial<DesktopSettings>)).toEqual(DEFAULT_SETTINGS);
  });
});

describe("the old local pet name moves to the hub once", () => {
  test("a stored defaultPetName is kept as legacyPetName until the move is done, then dropped", () => {
    const s = normalizeSettings({ defaultPetName: " Rex " } as unknown as Partial<DesktopSettings>);
    expect(s.legacyPetName).toBe("Rex");
    expect("defaultPetName" in s).toBe(false);
    const done = normalizeSettings({ defaultPetName: "Rex", petNameMigrated: true, legacyPetName: "" } as unknown as Partial<DesktopSettings>);
    expect(done.legacyPetName).toBe("");
    // Already captured: not overwritten.
    expect(normalizeSettings({ defaultPetName: "Old", legacyPetName: "Rex" } as unknown as Partial<DesktopSettings>).legacyPetName).toBe("Rex");
  });
});

describe("default pet name", () => {
  const pets: Pet[] = [{ id: "m:p1", machineId: "m", agentId: "codex", species: "dog", skin: "default", name: "Biscuit", personality: "", mood: 70, xp: 0 }];

  test("trimmed, 1-40 characters", () => {
    expect(checkDefaultPetName("  Rover ", pets)).toEqual({ ok: true, name: "Rover" });
    expect(checkDefaultPetName("   ", pets).ok).toBe(false);
    expect(checkDefaultPetName("a".repeat(41), pets).ok).toBe(false);
    expect(checkDefaultPetName("a".repeat(40), pets).ok).toBe(true);
  });

  test("not another pet's name (case-insensitive), and no invisible characters", () => {
    expect(checkDefaultPetName("biscuit", pets)).toMatchObject({ ok: false, code: "name_taken" });
    const zwsp = String.fromCodePoint(0x200b);
    expect(checkDefaultPetName(`Pa${zwsp}wl`, pets)).toMatchObject({ ok: false, code: "invalid" });
    // The default pet is not a hub pet: nothing can share its id.
    expect(pets.some((p) => p.id === DEFAULT_PET_ID)).toBe(false);
  });
});
