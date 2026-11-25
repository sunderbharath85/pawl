import { describe, expect, test } from "bun:test";
import type { Pet } from "@pawl/protocol";
import { FALLBACK_PET_NAME, legacyNameToSend, shownPetName } from "./petName";

const pets: Pet[] = [{ id: "m:p1", machineId: "m", agentId: "codex", species: "dog", skin: "default", name: "Biscuit", personality: "", mood: 70, xp: 0 }];

describe("the default pet's name", () => {
  test("the hub's name wins; while the hub is down the last one seen; never the product's name", () => {
    expect(shownPetName({ hubName: "Mochi", cached: "Old", legacy: "Rex" })).toBe("Mochi");
    expect(shownPetName({ hubName: null, cached: "Mochi" })).toBe("Mochi");
    expect(shownPetName({ hubName: "", cached: "", legacy: "Rex" })).toBe("Rex");
    // An older version's name that is still on its way to the hub comes before the last one seen.
    expect(shownPetName({ cached: "Mochi", legacy: "Rex" })).toBe("Rex");
    expect(shownPetName({ cached: "Pawl", legacy: "Pawl" })).toBe(FALLBACK_PET_NAME);
    expect(shownPetName({})).toBe("Your pet");
  });

  test("a name chosen in an older version is sent to the hub once, when it is valid and new", () => {
    expect(legacyNameToSend({ legacy: " Rex ", hubName: "Mochi", pets })).toBe("Rex");
    // Never chosen (the old default), already the hub's, or not a valid pet name: nothing to send.
    expect(legacyNameToSend({ legacy: "Pawl", hubName: "Mochi", pets })).toBeNull();
    expect(legacyNameToSend({ legacy: "pawl", hubName: "Mochi", pets })).toBeNull();
    expect(legacyNameToSend({ legacy: "", hubName: "Mochi", pets })).toBeNull();
    expect(legacyNameToSend({ legacy: "Mochi", hubName: "Mochi", pets })).toBeNull();
    expect(legacyNameToSend({ legacy: "biscuit", hubName: "Mochi", pets })).toBeNull();
    expect(legacyNameToSend({ legacy: "a".repeat(41), hubName: "Mochi", pets })).toBeNull();
    expect(legacyNameToSend({ legacy: `Re${String.fromCodePoint(0x200b)}x`, hubName: "Mochi", pets })).toBeNull();
  });
});
