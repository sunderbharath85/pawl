import { describe, expect, test } from "bun:test";
import { initialHubState, type HubState } from "@krypto/hub-client";
import type { Machine, Pet, PermissionRequest } from "@krypto/protocol";
import { petViewsOf, primaryView } from "./petViews";

const ME = "winpc0000001";
const MAC = "macmini00002";
const NOW = 80_000_000;
const me: Machine = { id: ME, name: "Windows PC", os: "windows", lastSeen: NOW, online: true };
const mac: Machine = { id: MAC, name: "Mac mini", os: "macos", lastSeen: NOW, online: true };
const pet = (machineId: string, name: string): Pet => ({
  id: `${machineId}:pet-claude-code`,
  machineId,
  agentId: "claude-code",
  species: "dog",
  skin: "default",
  name,
  personality: "",
  mood: 70,
  xp: 0,
});
const req: PermissionRequest = {
  requestId: `${ME}:r1`,
  machineId: ME,
  sessionId: `${ME}:s1`,
  agentId: "claude-code",
  toolCall: { toolCallId: "t", title: "Run tests", kind: "execute" },
  options: [{ optionId: "a", name: "Allow", kind: "allow_once" }],
  createdAt: NOW - 1000,
};

function state(over: Partial<HubState> = {}): HubState {
  const pets = [pet(ME, "Biscuit"), pet(MAC, "Pepper")];
  return {
    ...initialHubState(),
    status: "connected",
    machine: me,
    machines: { [ME]: me, [MAC]: mac },
    pets: Object.fromEntries(pets.map((p) => [p.id, p])),
    ...over,
  };
}

describe("petViewsOf", () => {
  test("another machine's pet is a visitor; home pets lead the status bar", () => {
    const views = petViewsOf(state(), NOW);
    expect(views.map((v) => [v.name, v.visitor])).toEqual([
      ["Biscuit", false],
      ["Pepper", true],
    ]);
    expect(primaryView(views.filter((v) => !v.visitor))?.name).toBe("Biscuit");
  });

  test("a home pet with an approval runs to the machine the user is active on", () => {
    const active = state({
      pending: { [req.requestId]: req },
      machines: { [ME]: { ...me, userActiveAt: NOW - 120_000 }, [MAC]: { ...mac, userActiveAt: NOW - 5_000 } },
    });
    const home = petViewsOf(active, NOW).find((v) => v.machineId === ME);
    expect(home?.away).toBe(MAC);
    const here = petViewsOf(state({ pending: { [req.requestId]: req } }), NOW).find((v) => v.machineId === ME);
    expect(here?.away).toBeUndefined();
    expect(here?.bubble?.requestId).toBe(req.requestId);
  });

  test("pets of an offline machine are remoteOffline, not visitors", () => {
    const views = petViewsOf(state({ machines: { [ME]: me, [MAC]: { ...mac, online: false } } }), NOW);
    expect(views.find((v) => v.machineId === MAC)).toMatchObject({ visitor: false, remoteOffline: true });
  });
});
