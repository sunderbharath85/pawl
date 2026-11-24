import { describe, expect, test } from "bun:test";
import { RELAY_QUEUE_MAX_BYTES, RELAY_QUEUE_MAX_FRAMES, RELAY_QUEUE_TTL_MS } from "@krypto/protocol";
import { MailboxQueue, QUEUE_FRAME_PREFIX, QUEUE_INDEX_KEY } from "../src/queue";
import { MemoryKv } from "../src/storage";

const T0 = 1_800_000_000_000;
const msg = (i: number) => `{"t":"msg","n":${i}}`;
const M = { from: "sender-a" };

describe("queue", () => {
  test("FIFO, persisted, ack removes sent frames", () => {
    const kv = new MemoryKv();
    const q = new MailboxQueue(kv);
    for (let i = 0; i < 3; i++) expect(q.push(msg(i), 10, T0 + i, M)).toEqual({ queued: true, dropped: 0 });
    const again = new MailboxQueue(kv); // survives hibernation / restart
    expect(again.peek(T0 + 5).map((f) => f.msg)).toEqual([msg(0), msg(1), msg(2)]);
    again.ack(2);
    expect(new MailboxQueue(kv).peek(T0 + 5).map((f) => f.msg)).toEqual([msg(2)]);
    new MailboxQueue(kv).ack(1);
    expect(kv.keys()).toEqual([]); // nothing left behind
  });

  test("frame cap drops the oldest", () => {
    const q = new MailboxQueue(new MemoryKv(), { maxFrames: 3, maxBytes: 1000, ttlMs: 60_000 });
    for (let i = 0; i < 3; i++) q.push(msg(i), 1, T0, M);
    expect(q.push(msg(3), 1, T0, M)).toEqual({ queued: true, dropped: 1 });
    expect(q.peek(T0).map((f) => f.msg)).toEqual([msg(1), msg(2), msg(3)]);
  });

  test("byte cap drops as many as needed", () => {
    const q = new MailboxQueue(new MemoryKv(), { maxFrames: 100, maxBytes: 100, ttlMs: 60_000 });
    for (let i = 0; i < 5; i++) q.push(msg(i), 20, T0, M);
    expect(q.bytes).toBe(100);
    expect(q.push(msg(9), 50, T0, M)).toEqual({ queued: true, dropped: 3 });
    expect(q.bytes).toBe(90);
    expect(q.count).toBe(3);
  });

  test("a frame bigger than the whole queue is refused", () => {
    const q = new MailboxQueue(new MemoryKv(), { maxFrames: 10, maxBytes: 100, ttlMs: 60_000 });
    q.push(msg(0), 10, T0, M);
    expect(q.push(msg(1), 101, T0, M)).toEqual({ queued: false });
    expect(q.count).toBe(1);
  });

  test("TTL: prune, peek and nextExpiry", () => {
    const q = new MailboxQueue(new MemoryKv(), { maxFrames: 10, maxBytes: 1000, ttlMs: 1000 });
    q.push(msg(0), 1, T0, M);
    q.push(msg(1), 1, T0 + 500, M);
    expect(q.nextExpiry()).toBe(T0 + 1000);
    expect(q.prune(T0 + 999)).toBe(0);
    expect(q.prune(T0 + 1000)).toBe(1);
    expect(q.nextExpiry()).toBe(T0 + 1500);
    expect(q.peek(T0 + 1500)).toEqual([]);
    expect(q.nextExpiry()).toBeNull();
  });

  test("expired frames are pruned before dropping live ones", () => {
    const q = new MailboxQueue(new MemoryKv(), { maxFrames: 2, maxBytes: 1000, ttlMs: 1000 });
    q.push(msg(0), 1, T0, M);
    q.push(msg(1), 1, T0 + 900, M);
    expect(q.push(msg(2), 1, T0 + 1000, M)).toEqual({ queued: true, dropped: 0 });
    expect(q.peek(T0 + 1000).map((f) => f.msg)).toEqual([msg(1), msg(2)]);
  });

  test("retain drops frames of senders that are no longer wanted, keeps order", () => {
    const kv = new MemoryKv();
    const q = new MailboxQueue(kv);
    q.push(msg(0), 1, T0, { from: "a" });
    q.push(msg(1), 1, T0, { from: "b" });
    q.push(msg(2), 1, T0, { from: "c", viaPairing: true });
    q.push(msg(3), 1, T0, { from: "a" });
    const writes = kv.writes;
    expect(q.retain(() => true)).toBe(0);
    expect(kv.writes).toBe(writes); // nothing dropped -> nothing written
    expect(q.retain((from, viaPairing) => viaPairing || from === "b")).toBe(2);
    const reloaded = new MailboxQueue(kv);
    expect(reloaded.peek(T0).map((f) => f.msg)).toEqual([msg(1), msg(2)]);
    expect(kv.keys().filter((k) => k.startsWith(QUEUE_FRAME_PREFIX)).length).toBe(2);
  });

  test("default limits come from the protocol", () => {
    const kv = new MemoryKv();
    const q = new MailboxQueue(kv);
    for (let i = 0; i < RELAY_QUEUE_MAX_FRAMES + 10; i++) q.push(msg(i), 1, T0, M);
    expect(q.count).toBe(RELAY_QUEUE_MAX_FRAMES);
    // Storage stays bounded too: one value per frame plus the index.
    expect(kv.keys().filter((k) => k.startsWith(QUEUE_FRAME_PREFIX)).length).toBe(RELAY_QUEUE_MAX_FRAMES);
    expect(kv.keys()).toContain(QUEUE_INDEX_KEY);
    q.clear();
    expect(kv.size).toBe(0);
    const big = new MailboxQueue(new MemoryKv());
    for (let i = 0; i < 10; i++) big.push(msg(i), 320 * 1024, T0, M);
    expect(big.bytes).toBeLessThanOrEqual(RELAY_QUEUE_MAX_BYTES);
    expect(big.nextExpiry()).toBe(T0 + RELAY_QUEUE_TTL_MS);
  });

  test("index without its frame is repaired", () => {
    const kv = new MemoryKv();
    const q = new MailboxQueue(kv);
    q.push(msg(0), 1, T0, M);
    q.push(msg(1), 1, T0, M);
    kv.delete(`${QUEUE_FRAME_PREFIX}1`);
    expect(new MailboxQueue(kv).peek(T0).map((f) => f.msg)).toEqual([msg(1)]);
    expect(new MailboxQueue(kv).count).toBe(1);
  });
});
