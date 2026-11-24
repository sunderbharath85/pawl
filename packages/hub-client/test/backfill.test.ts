import { describe, expect, test } from "bun:test";
import type { HubEvent } from "@krypto/protocol";
import { backfillSession, SessionBackfill, type EventSource } from "../src/index";

const MID = "abc123def456";
const SID = `${MID}:s1`;

function ev(seq: number): HubEvent {
  return { id: `${MID}:${seq}`, seq, machineId: MID, sessionId: SID, type: "agent_message_chunk", payload: { text: "x" }, ts: seq };
}

function range(from: number, to: number): HubEvent[] {
  const out: HubEvent[] = [];
  for (let s = from; s <= to; s++) out.push(ev(s));
  return out;
}

/** Fake hub: holds `log` server-side; the client keeps whatever fetches returned. */
class FakeClient implements EventSource {
  calls: Array<{ sinceSeq?: number; limit?: number }> = [];
  held: HubEvent[] = [];
  gate: Promise<void> | null = null;
  constructor(public log: HubEvent[]) {}
  getState() {
    return { events: { [SID]: this.held } };
  }
  async getEvents(_sid: string, sinceSeq?: number, limit = 500): Promise<HubEvent[]> {
    this.calls.push({ sinceSeq, limit });
    if (this.gate) await this.gate;
    const page = sinceSeq === undefined ? this.log.slice(-limit) : this.log.filter((e) => e.seq > sinceSeq).slice(0, limit);
    const byId = new Map([...this.held, ...page].map((e) => [e.id, e]));
    this.held = [...byId.values()].sort((a, b) => a.seq - b.seq);
    return page;
  }
}

describe("backfillSession", () => {
  test("no cursor loads the latest window", async () => {
    const calls: Array<number | undefined> = [];
    const res = await backfillSession(async (since) => (calls.push(since), []), undefined, 10);
    expect(res).toBe("window");
    expect(calls).toEqual([undefined]);
  });

  test("catches up after the cursor when the gap is smaller than a page", async () => {
    const calls: Array<number | undefined> = [];
    const res = await backfillSession(async (since) => (calls.push(since), range(6, 8)), 5, 10);
    expect(res).toBe("catch-up");
    expect(calls).toEqual([5]);
  });

  test("a full catch-up page falls back to the latest window", async () => {
    const calls: Array<number | undefined> = [];
    const res = await backfillSession(async (since) => (calls.push(since), since === undefined ? [] : range(6, 15)), 5, 10);
    expect(res).toBe("catch-up+window");
    expect(calls).toEqual([5, undefined]);
  });
});

describe("SessionBackfill", () => {
  test("first sync per client ignores live-tail events and loads the window", async () => {
    const client = new FakeClient(range(1, 30));
    client.held = [ev(30)]; // a live event arrived before the panel opened
    const tracker = new SessionBackfill(10);
    expect(await tracker.sync(client, SID)).toBe("window");
    expect(client.calls).toEqual([{ sinceSeq: undefined, limit: 10 }]);
    expect(client.held.map((e) => e.seq)).toEqual(range(21, 30).map((e) => e.seq));
  });

  test("later syncs use the newest held seq as the cursor", async () => {
    const client = new FakeClient(range(1, 12));
    const tracker = new SessionBackfill(10);
    await tracker.sync(client, SID);
    client.log = range(1, 15); // events 13..15 happened while disconnected
    expect(await tracker.sync(client, SID)).toBe("catch-up");
    expect(client.calls[1]).toEqual({ sinceSeq: 12, limit: 10 });
    expect(client.held.at(-1)?.seq).toBe(15);
  });

  test("concurrent syncs of one session share a single request chain", async () => {
    const client = new FakeClient(range(1, 3));
    let open!: () => void;
    client.gate = new Promise<void>((r) => (open = r));
    const tracker = new SessionBackfill(10);
    const a = tracker.sync(client, SID);
    const b = tracker.sync(client, SID);
    expect(b).toBe(a);
    open();
    await a;
    expect(client.calls).toHaveLength(1);
  });

  test("a failed sync is retried as a first sync", async () => {
    const client = new FakeClient(range(1, 3));
    const fetch = client.getEvents.bind(client);
    let fail = true;
    client.getEvents = async (sid, sinceSeq, limit) => {
      if (fail) {
        fail = false;
        throw new Error("offline");
      }
      return fetch(sid, sinceSeq, limit);
    };
    client.held = [ev(3)];
    const tracker = new SessionBackfill(10);
    await expect(tracker.sync(client, SID)).rejects.toThrow("offline");
    expect(await tracker.sync(client, SID)).toBe("window");
    expect(client.calls.at(-1)).toEqual({ sinceSeq: undefined, limit: 10 });
  });

  test("tracks clients independently (a replaced HubClient starts over)", async () => {
    const tracker = new SessionBackfill(10);
    const first = new FakeClient(range(1, 3));
    await tracker.sync(first, SID);
    const second = new FakeClient(range(1, 3));
    second.held = first.held;
    expect(await tracker.sync(second, SID)).toBe("window");
  });
});
