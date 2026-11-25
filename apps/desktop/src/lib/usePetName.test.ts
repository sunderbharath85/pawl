import { describe, expect, test } from "bun:test";
import { SettingsError, type HubClient, type HubState } from "@pawl/hub-client";
import type { HubSettingsPatch, Pet } from "@pawl/protocol";
import { hubPetName, renameDefaultPet, type RenameResult } from "./usePetName";

const biscuit: Pet = { id: "m:p1", machineId: "m", agentId: "codex", species: "dog", skin: "default", name: "Biscuit", personality: "", mood: 70, xp: 0 };

function hubWith(petName: unknown): HubState {
  return { pets: { [biscuit.id]: biscuit }, settings: { bot: { petName } } } as unknown as HubState;
}

function fakeClient(answer: (patch: HubSettingsPatch) => Promise<unknown>) {
  const sent: HubSettingsPatch[] = [];
  const client = {
    updateSettings: (patch: HubSettingsPatch) => {
      sent.push(patch);
      return answer(patch);
    },
  } as unknown as HubClient;
  return { client, sent };
}

describe("renaming the default pet goes through the hub", () => {
  test("the hub's name; none from a hub without the setting", () => {
    expect(hubPetName(hubWith(" Mochi "))).toBe("Mochi");
    expect(hubPetName(hubWith(undefined))).toBeNull();
    expect(hubPetName({ pets: {} } as unknown as HubState)).toBeNull();
  });

  test("a good name is sent trimmed as bot.petName", async () => {
    const { client, sent } = fakeClient(async () => ({}));
    expect(await renameDefaultPet(client, hubWith("Mochi"), "  Rex ")).toEqual({ ok: true, name: "Rex" });
    expect(sent).toEqual([{ bot: { petName: "Rex" } }]);
  });

  test("checked here first (another pet's name, too long, unchanged): nothing is sent", async () => {
    const { client, sent } = fakeClient(async () => ({}));
    const taken = await renameDefaultPet(client, hubWith("Mochi"), "biscuit");
    expect(taken.ok).toBe(false);
    expect((await renameDefaultPet(client, hubWith("Mochi"), "a".repeat(41))).ok).toBe(false);
    expect(await renameDefaultPet(client, hubWith("Mochi"), " Mochi")).toEqual({ ok: true, name: "Mochi" });
    expect(sent).toEqual([]);
  });

  test("the hub's refusal is shown as its reason; a hub without the setting cannot rename", async () => {
    const { client } = fakeClient(async () => {
      throw new SettingsError("bot.petName", "is already another pet's name", "update_settings");
    });
    expect(await renameDefaultPet(client, hubWith("Mochi"), "Rex")).toEqual({ ok: false, error: "The name is already another pet's name." });
    const old = await renameDefaultPet(client, hubWith(undefined), "Rex");
    expect(old.ok).toBe(false);
  });
});

describe("the rename field in the bubble", () => {
  test("starts from the current name, with Save and Cancel", async () => {
    const { createElement } = await import("react");
    const { renderToStaticMarkup } = await import("react-dom/server");
    const { RenameField } = await import("../pet/Bubble");
    const html = renderToStaticMarkup(createElement(RenameField, { name: "Mochi", onRename: async (): Promise<RenameResult> => ({ ok: true, name: "Mochi" }), onDone: () => {} }));
    expect(html).toContain("New name for Mochi");
    expect(html).toContain('value="Mochi"');
    expect(html).toContain(">Save</button>");
    expect(html).toContain(">Cancel</button>");
  });
});
