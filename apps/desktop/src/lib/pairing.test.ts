import { describe, expect, test } from "bun:test";
import { HubRequestError } from "@krypto/hub-client";
import type { Device } from "@krypto/protocol";
import {
  InviteHolder,
  formatCountdown,
  inviteUsedBy,
  joinCommand,
  pairingErrorText,
  presenceText,
  type PairingAction,
  type ShownInvite,
} from "./pairing";

/** Manual clock + timers for InviteHolder. */
function fakeTimers(start = 1_000) {
  let now = start;
  let seq = 0;
  const timers = new Map<number, { at: number; fn: () => void }>();
  return {
    now: () => now,
    setTimeout: (fn: () => void, ms: number) => {
      const id = ++seq;
      timers.set(id, { at: now + ms, fn });
      return id;
    },
    clearTimeout: (h: unknown) => void timers.delete(h as number),
    advance(ms: number) {
      now += ms;
      for (const [id, t] of [...timers]) {
        if (t.at <= now) {
          timers.delete(id);
          t.fn();
        }
      }
    },
    pending: () => timers.size,
  };
}

const invite = (over: Partial<ShownInvite> = {}): ShownInvite => ({
  uri: "krypto://peer?v=1&code=secret",
  expiresAt: 61_000,
  createdAt: 1_000,
  knownDeviceIds: [],
  ignoredDeviceIds: [],
  ...over,
});

const device = (id: string, pairedAt = 500): Device => ({ id, kind: "hub", name: `Box ${id}`, publicKey: "pk", pairedAt });

describe("InviteHolder", () => {
  test("shows an invite and clears it when it expires", () => {
    const t = fakeTimers();
    const h = new InviteHolder(t);
    let changes = 0;
    h.subscribe(() => changes++);
    h.show(invite());
    expect(h.get()?.expiresAt).toBe(61_000);
    t.advance(59_999);
    expect(h.get()).not.toBeNull();
    t.advance(1);
    expect(h.get()).toBeNull();
    expect(changes).toBe(2);
    expect(t.pending()).toBe(0);
  });

  test("an already-expired invite is never shown", () => {
    const t = fakeTimers(100_000);
    const h = new InviteHolder(t);
    h.show(invite());
    expect(h.get()).toBeNull();
  });

  test("clear() and a newer invite cancel the previous timer", () => {
    const t = fakeTimers();
    const h = new InviteHolder(t);
    h.show(invite());
    h.show(invite({ uri: "krypto://peer?second", expiresAt: 121_000 }));
    expect(t.pending()).toBe(1);
    t.advance(60_000);
    expect(h.get()?.uri).toBe("krypto://peer?second");
    h.clear();
    expect(h.get()).toBeNull();
    expect(t.pending()).toBe(0);
  });

  test("ignoreDevice records machines this hub joined itself", () => {
    const h = new InviteHolder(fakeTimers());
    h.ignoreDevice("x"); // no invite: no-op
    expect(h.get()).toBeNull();
    h.show(invite());
    h.ignoreDevice("macmini00002");
    h.ignoreDevice("macmini00002");
    expect(h.get()?.ignoredDeviceIds).toEqual(["macmini00002"]);
  });

  test("unsubscribe stops notifications", () => {
    const h = new InviteHolder(fakeTimers());
    let changes = 0;
    const off = h.subscribe(() => changes++);
    off();
    h.show(invite());
    expect(changes).toBe(0);
  });
});

describe("inviteUsedBy", () => {
  test("a device that is new since the invite means it was used", () => {
    expect(inviteUsedBy(invite({ knownDeviceIds: ["a"] }), [device("a"), device("b", 2_000)])?.id).toBe("b");
  });

  test("a known device re-paired after the invite counts too", () => {
    expect(inviteUsedBy(invite({ knownDeviceIds: ["a"] }), [device("a", 1_500)])?.id).toBe("a");
  });

  test("old pairings and machines this hub joined do not", () => {
    expect(inviteUsedBy(invite({ knownDeviceIds: ["a"] }), [device("a", 500)])).toBeUndefined();
    expect(inviteUsedBy(invite({ ignoredDeviceIds: ["b"] }), [device("b", 2_000)])).toBeUndefined();
  });
});

describe("pairingErrorText", () => {
  const err = (code: string) => new HubRequestError(code, "x");

  test("every contract error code has its own friendly text", () => {
    const cases: Array<[string, PairingAction]> = [
      ["peer_listener_off", "create_invite"],
      ["invalid", "create_invite"],
      ["bad_uri", "join_peer"],
      ["self_invite", "join_peer"],
      ["unreachable", "join_peer"],
      ["invite_rejected", "join_peer"],
      ["timeout", "join_peer"],
      ["not_found", "revoke_device"],
    ];
    const texts = cases.map(([code, action]) => pairingErrorText(err(code), action));
    expect(new Set(texts).size).toBe(cases.length);
    for (const t of texts) {
      expect(t.length).toBeGreaterThan(20);
      expect(t).not.toContain("_"); // no raw codes
    }
  });

  test("invalid is explained per request", () => {
    expect(pairingErrorText(err("invalid"), "create_invite")).toContain("LAN address");
    expect(pairingErrorText(err("invalid"), "join_peer")).not.toBe(pairingErrorText(err("invalid"), "create_invite"));
  });

  test("connection loss, unknown codes and plain errors", () => {
    expect(pairingErrorText(err("disconnected"), "join_peer")).toContain("isn't connected");
    expect(pairingErrorText(err("closed"), "create_invite")).toContain("isn't connected");
    expect(pairingErrorText(err("brand_new"), "join_peer")).toContain("brand_new");
    expect(pairingErrorText(new Error("boom"), "join_peer")).toBe("boom");
    expect(pairingErrorText(null, "join_peer")).toBe("Something went wrong.");
  });
});

test("formatCountdown", () => {
  expect(formatCountdown(10_000 + 600_000, 10_000)).toBe("10:00");
  expect(formatCountdown(10_000 + 61_500, 10_000)).toBe("1:02");
  expect(formatCountdown(3_723_000, 0)).toBe("1:02:03");
  expect(formatCountdown(5, 10)).toBe("0:00");
});

test("presenceText", () => {
  expect(presenceText({ online: true, lastSeen: 0 }, 1)).toBe("Online");
  expect(presenceText({ online: false, lastSeen: 0 }, 1)).toBe("Not seen yet");
  expect(presenceText({ online: false, lastSeen: 1_000 }, 1_000 + 5 * 60_000)).toBe("Last seen 5m ago");
});

test("joinCommand quotes the URI for the shell", () => {
  expect(joinCommand("krypto://peer?v=1&mid=a")).toBe('krypto peer join "krypto://peer?v=1&mid=a"');
});
