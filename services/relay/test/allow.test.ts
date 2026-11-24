import { describe, expect, test } from "bun:test";
import { RELAY_MAX_ALLOWED } from "@krypto/protocol";
import { ALLOW_KEY, AccessControl, SLOTS_KEY } from "../src/allow";
import { encodeUrl, randomBytes } from "../src/b64";
import { PAIRING_MAX_TTL_MS, PAIRING_SLOT_MAX } from "../src/limits";
import { MemoryKv } from "../src/storage";

const pk = () => encodeUrl(randomBytes(32));
const token = () => encodeUrl(randomBytes(16));
const T0 = 1_800_000_000_000;

describe("allowlist", () => {
  test("replace, persist and reload", () => {
    const kv = new MemoryKv();
    const acl = new AccessControl(kv);
    const [a, b, c] = [pk(), pk(), pk()];
    expect(acl.setAllowed([a, b, a])).toEqual({ ok: true, changed: true });
    expect(acl.isAllowed(a)).toBe(true);
    expect(acl.isAllowed(c)).toBe(false);
    expect(acl.hasAllowed()).toBe(true);
    expect(new AccessControl(kv).isAllowed(b)).toBe(true); // survives a restart (hibernation)
    expect(acl.setAllowed([c])).toEqual({ ok: true, changed: true }); // replace, not merge
    expect(acl.isAllowed(a)).toBe(false);
    expect(new AccessControl(kv).listAllowed()).toEqual([c]);
    acl.setAllowed([]);
    expect(kv.get(ALLOW_KEY)).toBeUndefined();
    expect(acl.hasAllowed()).toBe(false);
  });

  test("an unchanged set is not written again", () => {
    const kv = new MemoryKv();
    const acl = new AccessControl(kv);
    const [a, b] = [pk(), pk()];
    acl.setAllowed([a, b]);
    const writes = kv.writes;
    expect(acl.setAllowed([b, a, b])).toEqual({ ok: true, changed: false });
    expect(new AccessControl(kv).setAllowed([a, b])).toEqual({ ok: true, changed: false }); // after a reload too
    expect(kv.writes).toBe(writes);
    expect(new AccessControl(new MemoryKv()).setAllowed([])).toEqual({ ok: true, changed: false });
  });

  test("bounded", () => {
    const acl = new AccessControl(new MemoryKv());
    expect(acl.setAllowed(Array.from({ length: RELAY_MAX_ALLOWED }, pk)).ok).toBe(true);
    expect(acl.setAllowed(Array.from({ length: RELAY_MAX_ALLOWED + 1 }, pk))).toEqual({ ok: false });
    expect(acl.listAllowed().length).toBe(RELAY_MAX_ALLOWED); // a refused update leaves the old set
  });
});

describe("pairing slots", () => {
  test("one shot", () => {
    const kv = new MemoryKv();
    const acl = new AccessControl(kv);
    const t = token();
    expect(acl.addPairingSlot(t, T0 + 60_000, T0).ok).toBe(true);
    expect(acl.consumePairing(token(), T0 + 1)).toBe(false);
    expect(new AccessControl(kv).consumePairing(t, T0 + 1)).toBe(true);
    expect(new AccessControl(kv).consumePairing(t, T0 + 2)).toBe(false);
    expect(kv.get(SLOTS_KEY)).toBeUndefined();
  });

  test("expiry", () => {
    const acl = new AccessControl(new MemoryKv());
    const t = token();
    expect(acl.addPairingSlot(t, T0, T0).ok).toBe(false); // exp must be in the future
    expect(acl.addPairingSlot(t, T0 - 1, T0).ok).toBe(false);
    acl.addPairingSlot(t, T0 + 1000, T0);
    expect(acl.consumePairing(t, T0 + 1000)).toBe(false);
    expect(acl.liveSlots(T0 + 1000)).toBe(0);
  });

  test("exp clamped to one hour", () => {
    const acl = new AccessControl(new MemoryKv());
    const r = acl.addPairingSlot(token(), T0 + 2 * PAIRING_MAX_TTL_MS, T0);
    expect(r).toEqual({ ok: true, exp: T0 + PAIRING_MAX_TTL_MS, changed: true });
    expect(acl.nextSlotExpiry()).toBe(T0 + PAIRING_MAX_TTL_MS);
  });

  test("max live slots: oldest registration evicted", () => {
    const acl = new AccessControl(new MemoryKv());
    const tokens = Array.from({ length: PAIRING_SLOT_MAX + 1 }, token);
    tokens.forEach((t, i) => acl.addPairingSlot(t, T0 + 60_000, T0 + i));
    expect(acl.liveSlots(T0 + 10)).toBe(PAIRING_SLOT_MAX);
    expect(acl.consumePairing(tokens[0]!, T0 + 10)).toBe(false);
    expect(acl.consumePairing(tokens[PAIRING_SLOT_MAX]!, T0 + 10)).toBe(true);
  });

  test("the same token again: no write when unchanged, in-place update when its expiry moved", () => {
    const kv = new MemoryKv();
    const acl = new AccessControl(kv);
    const t = token();
    acl.addPairingSlot(t, T0 + 10_000, T0);
    const writes = kv.writes;
    expect(acl.addPairingSlot(t, T0 + 10_000, T0 + 5)).toEqual({ ok: true, exp: T0 + 10_000, changed: false });
    expect(new AccessControl(kv).addPairingSlot(t, T0 + 10_500, T0 + 6).ok).toBe(true); // < 1 s: unchanged
    expect(kv.writes).toBe(writes);
    expect(acl.addPairingSlot(t, T0 + 50_000, T0 + 7)).toEqual({ ok: true, exp: T0 + 50_000, changed: true });
    expect(acl.liveSlots(T0 + 8)).toBe(1);
    expect(acl.consumePairing(t, T0 + 20_000)).toBe(true);
  });

  test("an expired token registered again is a new slot", () => {
    const acl = new AccessControl(new MemoryKv());
    const t = token();
    acl.addPairingSlot(t, T0 + 1000, T0);
    expect(acl.addPairingSlot(t, T0 + 5000, T0 + 2000)).toEqual({ ok: true, exp: T0 + 5000, changed: true });
    expect(acl.consumePairing(t, T0 + 3000)).toBe(true);
  });

  test("prune", () => {
    const acl = new AccessControl(new MemoryKv());
    acl.addPairingSlot(token(), T0 + 10, T0);
    acl.addPairingSlot(token(), T0 + 1000, T0);
    expect(acl.pruneSlots(T0 + 10)).toBe(1);
    expect(acl.liveSlots(T0 + 10)).toBe(1);
  });
});
