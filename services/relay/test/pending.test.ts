import { describe, expect, test } from "bun:test";
import { AUTH_GRACE_MS, MAX_PENDING_SOCKETS } from "../src/limits";
import { planAdmission } from "../src/pending";

const T0 = 1_800_000_000_000;
const sockets = (ats: number[]) => ats.map((at, i) => ({ socket: `s${i}`, at }));

describe("(3) pending-socket admission", () => {
  test("room left: admit, evict nothing", () => {
    expect(planAdmission(sockets([T0, T0]), T0)).toEqual({ admit: true, evict: [] });
    expect(planAdmission([], T0)).toEqual({ admit: true, evict: [] });
  });

  test("full: evict the oldest socket that never answered (older than the grace period)", () => {
    const ats = Array.from({ length: MAX_PENDING_SOCKETS }, (_, i) => T0 + i * 100);
    const now = T0 + AUTH_GRACE_MS + 50; // s0 is stale (3050 ms), the rest are within the grace period
    expect(planAdmission(sockets(ats), now)).toEqual({ admit: true, evict: ["s0"] });
  });

  test("full of fresh sockets (maybe mid-auth): refuse the newcomer, evict nobody", () => {
    const ats = Array.from({ length: MAX_PENDING_SOCKETS }, () => T0);
    expect(planAdmission(sockets(ats), T0 + AUTH_GRACE_MS - 1)).toEqual({ admit: false, evict: [] });
  });

  test("a fresh socket is never evicted even when it is the oldest of the fresh ones", () => {
    // 7 fresh + 1 stale at the end of the list: the stale one goes, the fresh ones stay.
    const ats = [...Array.from({ length: MAX_PENDING_SOCKETS - 1 }, () => T0 + 5000), T0];
    expect(planAdmission(sockets(ats), T0 + 5000 + 10)).toEqual({ admit: true, evict: [`s${MAX_PENDING_SOCKETS - 1}`] });
  });

  test("over the cap (e.g. after a config change): evict enough stale ones, oldest first", () => {
    const plan = planAdmission(sockets([T0 + 2, T0, T0 + 1, T0 + 3]), T0 + 10_000, 2, 1000);
    expect(plan).toEqual({ admit: true, evict: ["s1", "s2", "s0"] });
  });
});
