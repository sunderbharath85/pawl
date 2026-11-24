import { describe, expect, test } from "bun:test";
import type { Device, Machine } from "@krypto/protocol";
import { machinesLine } from "@krypto/hub-client";
import { presenceLabel } from "../shared/machine";
import { machinePickItems, phonesTooltipLine } from "./machineList";

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

    // A phone is not a machine: "no other machines" still shows, the phone comes after it.
    const phone = machinePickItems({ ...alone, devices: [dev("phone-1", { kind: "phone", name: "Pixel" })] }, true);
    expect(phone.items[1]!.label).toContain("No other machines paired yet");
    expect(phone.items[2]).toMatchObject({ label: "$(circle-outline) $(device-mobile) Pixel", description: "phone" });
  });

  test("phones: OS, LAN / Relay link and who added them", () => {
    const iphone = dev("iphone000001", { kind: "phone", name: "iPhone", os: "ios", path: "relay", lastSeen: 5 });
    const pixel = dev("pixel0000001", { kind: "phone", name: "Pixel", os: "android", introducedBy: MAC });
    const { items } = machinePickItems({ ...state, devices: [...state.devices, pixel, iphone] }, true);
    const labels = items.map((i) => i.label);
    expect(labels.slice(-2)).toEqual(["$(circle-filled) $(device-mobile) iPhone", "$(circle-outline) $(device-mobile) Pixel"]);
    expect(items.at(-2)).toMatchObject({ description: "iOS phone · Relay", detail: "online" });
    expect(items.at(-1)!.description).toBe("Android phone");
    expect(items.at(-1)!.detail).toContain("Added by Mac mini");
    // Phones never show among the machines.
    expect(labels.filter((l) => l.includes("$(device-mobile)"))).toHaveLength(2);
  });

  test("phonesTooltipLine counts paired phones", () => {
    expect(phonesTooltipLine({ devices: [dev(MAC)] }, true)).toBeNull();
    const devices = [dev("p1", { kind: "phone", path: "relay" }), dev("p2", { kind: "phone" })];
    expect(phonesTooltipLine({ devices }, true)).toBe("$(device-mobile) 2 phones paired · 1 online");
    expect(phonesTooltipLine({ devices: devices.slice(1) }, false)).toBe("$(device-mobile) 1 phone paired");
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
