import { describe, expect, test } from "bun:test";
import type { Device, Machine } from "@krypto/protocol";
import { machinesLine } from "@krypto/hub-client";
import { presenceLabel } from "../shared/machine";
import { machinePickItems } from "./machineList";

const ME = "winpc0000001";
const MAC = "macmini00002";
const BOX = "linuxbox0003";

const m = (id: string, name: string, over: Partial<Machine> = {}): Machine => ({ id, name, os: "linux", lastSeen: 0, online: false, ...over });
const me = m(ME, "Windows PC", { os: "windows", online: true, hubVersion: "0.3.0" });
const mac = m(MAC, "Mac mini", { os: "macos", online: true });
const box = m(BOX, "Attic box", { lastSeen: 1_000 });
const dev = (id: string, over: Partial<Device> = {}): Device => ({ id, kind: "hub", name: id, publicKey: "pk", pairedAt: 1, ...over });

const state = {
  machine: me,
  machines: { [ME]: me, [MAC]: mac, [BOX]: box },
  devices: [dev(MAC, { addr: "192.168.1.20:47801" }), dev(BOX)],
  peer: { listening: "192.168.1.10:47801", publicKey: "pk", relay: { url: null, connected: false } },
};

describe("machinePickItems", () => {
  test("this machine first, then online peers, each with OS, presence and address", () => {
    const { title, items } = machinePickItems(state, true, 5_000);
    expect(title).toBe("Krypto: 3 machines · 2 online");
    expect(items.map((i) => i.label)).toEqual(["$(circle-filled) Windows PC", "$(circle-filled) Mac mini", "$(circle-outline) Attic box"]);
    expect(items[0]).toMatchObject({ description: "this machine · Windows", detail: "online · listening on 192.168.1.10:47801 · krypto 0.3.0" });
    expect(items[1]).toMatchObject({ description: "macOS", detail: "online · 192.168.1.20:47801" });
    expect(items[2]!.detail).toStartWith("offline · last seen ");
  });

  test("hub offline: nothing is shown as online", () => {
    const { title, items } = machinePickItems(state, false);
    expect(title).toBe("Krypto: 3 machines · hub offline");
    expect(items.every((i) => i.label.startsWith("$(circle-outline)"))).toBe(true);
    expect(items[1]!.detail).toStartWith("hub offline");
  });

  test("listener off, no peers yet, and a roster-only device", () => {
    const alone = { machine: me, machines: { [ME]: me }, devices: [], peer: { listening: null, publicKey: "pk", relay: { url: null, connected: false } } };
    const { items } = machinePickItems(alone, true);
    expect(items[0]!.detail).toContain("peer listener off");
    expect(items[1]!.label).toContain("No other machines paired yet");

    const phone = machinePickItems({ ...alone, devices: [dev("phone-1", { kind: "phone", name: "Pixel" })] }, true);
    expect(phone.items[1]).toMatchObject({ label: "$(circle-outline) Pixel", description: "phone" });
  });

  test("online paired machines show how they are linked (LAN / Relay)", () => {
    const linked = { ...state, devices: [dev(MAC, { addr: "192.168.1.20:47801", path: "relay" as const }), dev(BOX, { path: "lan" as const })] };
    const { items } = machinePickItems(linked, true, 5_000);
    expect(items[1]!.description).toBe("macOS · Relay");
    // Offline machines have no live link.
    expect(items[2]!.description).toBe("Linux");
    expect(machinePickItems(linked, false).items[1]!.description).toBe("macOS");
  });

  test("before the hub said hello", () => {
    const { title, items } = machinePickItems({ machine: undefined, machines: {}, devices: [], peer: undefined }, false);
    expect(title).toBe("Krypto: Machines");
    expect(items).toHaveLength(1);
  });
});

test("machinesLine", () => {
  expect(machinesLine(3, 2)).toBe("3 machines · 2 online");
  expect(machinesLine(1, 1)).toBe("1 machine · 1 online");
});

test("presenceLabel", () => {
  expect(presenceLabel({ online: true, lastSeen: 0 })).toBe("online");
  expect(presenceLabel({ online: false, lastSeen: 0 })).toBe("offline · not seen yet");
  const now = new Date(2026, 10, 23, 15, 0).getTime();
  const today = presenceLabel({ online: false, lastSeen: new Date(2026, 10, 23, 14, 5).getTime() }, now);
  const older = presenceLabel({ online: false, lastSeen: new Date(2026, 10, 1, 9, 0).getTime() }, now);
  expect(today).toStartWith("offline · last seen ");
  expect(today).toMatch(/\d/);
  expect(older).not.toBe(today);
});
