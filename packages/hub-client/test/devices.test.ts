import { describe, expect, test } from "bun:test";
import type { Device, Machine } from "@krypto/protocol";
import {
  groupDevices,
  introducedByRevoked,
  introducedByText,
  inviteState,
  inviteUsedBy,
  isPhoneInvite,
  isPhoneOs,
  machineCounts,
  OS_LABEL,
  pairedPhones,
  phoneCounts,
  phoneOs,
  phonePairingBlock,
  phonesLine,
  PHONE_QR_WARNING,
  revokeIntroducedPrompt,
  revokeManyText,
  sortedMachines,
  type PendingInvite,
} from "../src/index";
import { device2, inviteFrom2, machine, machine2, MID, MID2, peerOff } from "./fixtures";

const iphone: Device = { id: "iphone000001", kind: "phone", name: "iPhone", os: "ios", publicKey: "cGhvbmU=", pairedAt: 200, path: "relay" };
const pixel: Device = { id: "pixel0000001", kind: "phone", name: "Pixel", os: "android", publicKey: "cGl4ZWw=", pairedAt: 210 };
const ipad: Device = { id: "ipad00000001", kind: "phone", name: "Bob's iPad", publicKey: "aXBhZA==", pairedAt: 220, introducedBy: MID2 };

const PHONE_URI = `krypto://peer?v=1&mid=${MID}&pk=cGstd2lucGM&relay=wss://relay.example.dev&rt=AAAA&rk=k3y_SECRET&code=AAAAAAAAAAAAAAAAAAAAAA&exp=1900000000000`;

describe("devices grouping", () => {
  test("phones and hubs are split and sorted (online first, then name)", () => {
    const g = groupDevices([pixel, device2, ipad, iphone]);
    expect(g.hubs.map((d) => d.id)).toEqual([MID2]);
    expect(g.phones.map((d) => d.name)).toEqual(["iPhone", "Bob's iPad", "Pixel"]);
    expect(g.other).toEqual([]);
    // A kind from a newer hub is kept apart instead of being shown as a hub or a phone.
    const tablet = { ...pixel, id: "tab000000001", kind: "tablet" } as unknown as Device;
    expect(groupDevices([tablet]).other).toEqual([tablet]);
  });

  test("phone counts, line and OS", () => {
    const state = { devices: [device2, iphone, pixel, ipad] };
    expect(pairedPhones(state).length).toBe(3);
    expect(phoneCounts(state)).toEqual({ total: 3, online: 1 });
    expect(phoneCounts({ devices: [device2] })).toEqual({ total: 0, online: 0 });
    expect(phonesLine(0)).toBe("No phones paired");
    expect(phonesLine(1)).toBe("1 phone");
    expect(phonesLine(3, 1)).toBe("3 phones · 1 online");
    expect(phoneOs(iphone)).toBe("ios");
    expect(phoneOs(pixel)).toBe("android");
    expect(phoneOs(ipad)).toBeNull(); // an older hub does not say
    expect(phoneOs({ kind: "hub", os: "ios" })).toBeNull();
    expect(OS_LABEL.ios).toBe("iOS");
    expect(OS_LABEL.android).toBe("Android");
    expect(isPhoneOs("android")).toBe(true);
    expect(isPhoneOs("macos")).toBe(false);
  });

  test("phones never count or sort as machines", () => {
    const phoneMachine: Machine = { id: iphone.id, name: "iPhone", os: "ios", lastSeen: 1, online: true };
    const state = { machine, machines: { [MID]: machine, [MID2]: machine2, [iphone.id]: phoneMachine } };
    expect(sortedMachines(state).map((m) => m.id)).toEqual([MID, MID2]);
    expect(machineCounts(state)).toEqual({ total: 2, online: 1 });
  });
});

describe("introducedBy text", () => {
  const state = { machine, machines: { [MID]: machine, [MID2]: machine2 }, devices: [device2, ipad] };

  test("names the introducing machine", () => {
    expect(introducedByText(ipad, state)).toBe("Added by Mac mini");
    expect(introducedByText(iphone, state)).toBeNull();
  });

  test("falls back to the roster name, then the id", () => {
    expect(introducedByText(ipad, { machine, machines: {}, devices: [device2] })).toBe("Added by Mac mini");
    expect(introducedByText(ipad, { machine, machines: {}, devices: [] })).toBe(`Added by machine ${MID2} (no longer paired)`);
    // Never "added by" this machine itself.
    expect(introducedByText({ introducedBy: MID }, state)).toBeNull();
  });
});

describe("invite state (QR and machine invites)", () => {
  const base: PendingInvite = { uri: PHONE_URI, expiresAt: 10_000, createdAt: 1_000, knownDeviceIds: [MID2], ignoredDeviceIds: [], forPhone: true };

  test("waiting, then expired", () => {
    expect(inviteState(base, [device2], 4_000)).toEqual({ state: "waiting", msLeft: 6_000 });
    expect(inviteState(base, [device2], 10_000)).toEqual({ state: "expired" });
  });

  test("a phone invite is used only by a new, directly paired phone", () => {
    const fresh = { ...iphone, pairedAt: 2_000 };
    expect(inviteState(base, [device2, fresh], 4_000)).toEqual({ state: "used", by: fresh });
    // Used wins over expired: the card says who paired.
    expect(inviteState(base, [fresh], 20_000)).toMatchObject({ state: "used" });
    // A new hub, or a phone another hub introduced, did not use it.
    const newHub = { ...device2, id: "linuxbox0003", pairedAt: 2_000 };
    const introduced = { ...ipad, pairedAt: 2_000 };
    expect(inviteUsedBy(base, [newHub, introduced])).toBeUndefined();
  });

  test("a machine invite ignores phones, intros and machines this hub joined", () => {
    const inv: PendingInvite = { ...base, uri: inviteFrom2, forPhone: false, knownDeviceIds: [], ignoredDeviceIds: ["joined000004"] };
    const phone = { ...iphone, pairedAt: 2_000 };
    const joined = { ...device2, id: "joined000004", pairedAt: 2_000 };
    const introducedHub = { ...device2, id: "introhub0005", pairedAt: 2_000, introducedBy: MID2 };
    expect(inviteUsedBy(inv, [phone, joined, introducedHub])).toBeUndefined();
    const used = { ...device2, pairedAt: 2_000 };
    expect(inviteUsedBy(inv, [phone, used])).toEqual(used);
  });

  test("a re-paired device counts once it pairs after the invite", () => {
    const repaired = { ...iphone, pairedAt: 1_500 };
    expect(inviteUsedBy({ ...base, knownDeviceIds: [iphone.id] }, [repaired])).toEqual(repaired);
    expect(inviteUsedBy({ ...base, knownDeviceIds: [iphone.id] }, [{ ...iphone, pairedAt: 500 }])).toBeUndefined();
  });
});

describe("phone invites", () => {
  test("only relay-only invites are phone invites", () => {
    expect(isPhoneInvite(PHONE_URI)).toBe(true);
    expect(isPhoneInvite(inviteFrom2)).toBe(false); // LAN
    expect(isPhoneInvite(`${inviteFrom2}&relay=wss://relay.example.dev&rt=AAAA`)).toBe(false); // LAN + relay
    expect(isPhoneInvite("krypto://peer?v=1")).toBe(false);
  });

  test("the QR warning names the access key and rotation", () => {
    expect(PHONE_QR_WARNING).toContain("relay access key");
    expect(PHONE_QR_WARNING).toContain("rotate the key");
  });

  test("phonePairingBlock explains what is missing", () => {
    const relayOn = { ...peerOff, relay: { url: "wss://relay.example.dev", connected: true } };
    expect(phonePairingBlock({ protocolVersion: 8, peer: relayOn }, true)).toBeNull();
    expect(phonePairingBlock({ protocolVersion: 8, peer: relayOn }, false)?.code).toBe("disconnected");
    expect(phonePairingBlock({ protocolVersion: 7, peer: relayOn }, true)?.code).toBe("old_hub");
    expect(phonePairingBlock({ protocolVersion: 8, peer: undefined }, true)?.code).toBe("old_hub");
    const off = phonePairingBlock({ protocolVersion: 8, peer: peerOff }, true);
    expect(off?.code).toBe("relay_off");
    expect(off?.text).toContain("Settings");
    const down = phonePairingBlock({ protocolVersion: 8, peer: { ...peerOff, relay: { url: "wss://relay.example.dev", connected: false, lastError: "bad_key" } } }, true);
    expect(down?.code).toBe("relay_down");
    expect(down?.text).toContain("relay.example.dev");
    expect(down?.text).toContain("access key");
    expect(down?.text).not.toContain("k3y_SECRET");
  });
});

describe("revoking a hub that introduced devices", () => {
  test("introducedByRevoked cleans up the ack list", () => {
    const list = introducedByRevoked(
      {
        introduced: [
          { id: "iphone000001", name: " iPhone ", kind: "phone" },
          { id: "iphone000001", name: "dup", kind: "phone" },
          { id: MID2, name: "itself", kind: "hub" },
          { id: "", name: "no id", kind: "phone" },
          { id: "linuxbox0003", name: "", kind: "hub" },
        ],
      },
      MID2,
    );
    expect(list).toEqual([
      { id: "iphone000001", name: "iPhone", kind: "phone" },
      { id: "linuxbox0003", name: "linuxbox0003", kind: "hub" },
    ]);
    // An older hub acks with {} (or nothing).
    expect(introducedByRevoked({}, MID2)).toEqual([]);
    expect(introducedByRevoked(undefined, MID2)).toEqual([]);
  });

  test("the follow-up prompt names the hub and every device", () => {
    const one = revokeIntroducedPrompt("Mac mini", [{ id: "iphone000001", name: "iPhone", kind: "phone" }]);
    expect(one.title).toBe("Also remove the devices Mac mini added?");
    expect(one.body).toContain("this device");
    expect(one.body).toContain("unpairing Mac mini did not remove it");
    expect(one.items).toEqual([{ title: "iPhone", body: "Phone" }]);
    expect([one.confirm, one.cancel]).toEqual(["Remove it", "Keep it"]);
    const two = revokeIntroducedPrompt("Mac mini", [
      { id: "iphone000001", name: "iPhone", kind: "phone" },
      { id: "linuxbox0003", name: "Linux box", kind: "hub" },
    ]);
    expect(two.body).toContain("these 2 devices");
    expect(two.items.map((i) => i.body)).toEqual(["Phone", "Machine"]);
    expect([two.confirm, two.cancel]).toEqual(["Remove all 2", "Keep them"]);
  });

  test("revokeManyText says what was removed and what is left", () => {
    const a = { id: "a00000000001", name: "iPhone", kind: "phone" as const };
    const b = { id: "b00000000002", name: "Pixel", kind: "phone" as const };
    expect(revokeManyText({ removed: [a], failed: [] }, "Mac mini")).toEqual({ text: "Also removed iPhone", tone: "info" });
    expect(revokeManyText({ removed: [a, b], failed: [] }, "Mac mini").text).toBe("Also removed the 2 devices Mac mini added");
    expect(revokeManyText({ removed: [a], failed: [{ ...b, code: "disconnected" }] }, "Mac mini")).toEqual({
      text: "Removed iPhone. Couldn't remove Pixel; revoke it in Machines.",
      tone: "error",
    });
  });
});
