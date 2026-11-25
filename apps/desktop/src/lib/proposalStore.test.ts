import { describe, expect, test } from "bun:test";
import { MAX_LEDGER_ENTRIES, ProposalLedger } from "@pawl/hub-client";
import { PROPOSALS_KEY, STALE_RUNNING_MS, STALE_RUNNING_TEXT, storageProposalStore } from "./proposalStore";

/** One localStorage shared by two windows. */
function memoryStorage() {
  const data = new Map<string, string>();
  return {
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => void data.set(k, v),
    data,
  };
}

describe("proposals used in one window are used in the other (shared localStorage)", () => {
  test("confirmed in the bubble: the panel cannot run it again, nor can a reloaded panel", () => {
    const storage = memoryStorage();
    let now = 1_000;
    const bubble = new ProposalLedger(storageProposalStore(storage, () => now));
    const panel = new ProposalLedger(storageProposalStore(storage, () => now));
    expect(bubble.begin("msg-1", "p-a")).toBe(true);
    expect(panel.begin("msg-1", "p-b")).toBe(false);
    bubble.done("msg-1", "p-a", "Started on this machine.", "m1:s1");
    now += 1_000;
    const reloaded = new ProposalLedger(storageProposalStore(storage, () => now));
    expect(reloaded.available("msg-1")).toBe(false);
    expect(reloaded.get("msg-1")).toEqual({ status: "done", proposalId: "p-a", text: "Started on this machine.", sessionId: "m1:s1" });
    expect(storage.data.has(PROPOSALS_KEY)).toBe(true);
  });

  test("a run whose window went away shows as sent with no answer (not running forever), and stays used", () => {
    const storage = memoryStorage();
    let now = 0;
    const gone = new ProposalLedger(storageProposalStore(storage, () => now));
    gone.begin("msg-1", "p-a");
    now = STALE_RUNNING_MS - 1;
    expect(new ProposalLedger(storageProposalStore(storage, () => now)).get("msg-1")).toEqual({ status: "running", proposalId: "p-a" });
    now = STALE_RUNNING_MS + 1;
    const later = new ProposalLedger(storageProposalStore(storage, () => now));
    expect(later.get("msg-1")).toEqual({ status: "failed", proposalId: "p-a", text: STALE_RUNNING_TEXT, retryable: false });
    expect(later.available("msg-1")).toBe(false);
  });

  test("an entry keeps its time while unchanged", () => {
    const storage = memoryStorage();
    let now = 0;
    const l = new ProposalLedger(storageProposalStore(storage, () => now));
    l.begin("old", "p");
    now = STALE_RUNNING_MS + 10;
    l.begin("new", "p"); // rewrites the store; "old" keeps its first time and goes stale
    expect(new ProposalLedger(storageProposalStore(storage, () => now)).get("old")?.status).toBe("failed");
    expect(new ProposalLedger(storageProposalStore(storage, () => now)).get("new")?.status).toBe("running");
  });

  test("capped; unreadable or foreign data is ignored; no storage keeps nothing", () => {
    const storage = memoryStorage();
    const l = new ProposalLedger(storageProposalStore(storage));
    for (let i = 0; i < MAX_LEDGER_ENTRIES + 3; i++) l.begin(`m${i}`, "p");
    const stored = JSON.parse(storage.data.get(PROPOSALS_KEY)!) as { rows: unknown[] };
    expect(stored.rows).toHaveLength(MAX_LEDGER_ENTRIES);

    storage.data.set(PROPOSALS_KEY, "{not json");
    expect(storageProposalStore(storage).load()).toBeNull();
    storage.data.set(PROPOSALS_KEY, JSON.stringify({ v: 2, rows: [] }));
    expect(storageProposalStore(storage).load()).toBeNull();
    storage.data.set(PROPOSALS_KEY, JSON.stringify({ v: 1, rows: [["m", { status: "done" }, 1], ["ok", { status: "running", proposalId: "p" }, 1], "junk"] }));
    expect(storageProposalStore(storage, () => 2).load()).toEqual([["ok", { status: "running", proposalId: "p" }]]);

    const none = storageProposalStore(null);
    expect(none.load()).toBeNull();
    none.save([["m", { status: "running", proposalId: "p" }]]);
    const throwing = { getItem: () => { throw new Error("denied"); }, setItem: () => { throw new Error("full"); } };
    expect(storageProposalStore(throwing).load()).toBeNull();
    expect(() => storageProposalStore(throwing).save([])).not.toThrow();
  });
});
