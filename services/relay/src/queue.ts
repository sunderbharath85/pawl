// Frames waiting for an offline device. Bounded by count, bytes and age (RELAY_QUEUE_*): expired frames are
// pruned first, then the oldest are dropped until a new frame fits. Each frame is one storage value (the ready
// `msg` JSON); a small index value keeps order, sizes and ages so nothing has to list storage.

import { RELAY_QUEUE_MAX_BYTES, RELAY_QUEUE_MAX_FRAMES, RELAY_QUEUE_TTL_MS } from "@krypto/protocol";
import type { KvStore } from "./storage";

export const QUEUE_INDEX_KEY = "q:idx";
export const QUEUE_FRAME_PREFIX = "q:f:";

export interface QueueLimits {
  maxFrames: number;
  maxBytes: number;
  ttlMs: number;
}

export const DEFAULT_QUEUE_LIMITS: QueueLimits = {
  maxFrames: RELAY_QUEUE_MAX_FRAMES,
  maxBytes: RELAY_QUEUE_MAX_BYTES,
  ttlMs: RELAY_QUEUE_TTL_MS,
};

interface Item {
  id: number;
  at: number;
  bytes: number;
  /** Sender pk (relay address). */
  from: string;
  /** Admitted through a pairing slot rather than the allowlist. */
  p?: true;
}

/** Who a queued frame came from; used to drop frames of senders that are no longer allowed. */
export interface QueueMeta {
  from: string;
  viaPairing?: boolean;
}

interface Index {
  next: number;
  items: Item[];
}

export interface QueuedFrame {
  id: number;
  msg: string;
}

/** `dropped` = older frames removed to make room (the sender is told `queue_full`). */
export type PushResult = { queued: true; dropped: number } | { queued: false };

export class MailboxQueue {
  private idx: Index | null = null;

  constructor(
    private readonly kv: KvStore,
    private readonly limits: QueueLimits = DEFAULT_QUEUE_LIMITS,
  ) {}

  private index(): Index {
    if (!this.idx) this.idx = this.kv.get<Index>(QUEUE_INDEX_KEY) ?? { next: 1, items: [] };
    return this.idx;
  }

  private save(): void {
    const idx = this.index();
    if (idx.items.length === 0) this.kv.delete(QUEUE_INDEX_KEY);
    else this.kv.put(QUEUE_INDEX_KEY, idx);
  }

  private frameKey(id: number): string {
    return `${QUEUE_FRAME_PREFIX}${id}`;
  }

  private dropFirst(n: number): void {
    const idx = this.index();
    for (const item of idx.items.splice(0, n)) this.kv.delete(this.frameKey(item.id));
  }

  get count(): number {
    return this.index().items.length;
  }

  get bytes(): number {
    return this.index().items.reduce((sum, i) => sum + i.bytes, 0);
  }

  /** Remove expired frames. Returns how many were removed. */
  prune(now: number): number {
    const idx = this.index();
    let n = 0;
    while (n < idx.items.length && idx.items[n]!.at + this.limits.ttlMs <= now) n++;
    if (n > 0) {
      this.dropFirst(n);
      this.save();
    }
    return n;
  }

  /** Queue one ready-to-send frame of `bytes` bytes. Frames that can never fit are refused. */
  push(msg: string, bytes: number, now: number, meta: QueueMeta): PushResult {
    if (bytes > this.limits.maxBytes || this.limits.maxFrames < 1) return { queued: false };
    this.prune(now);
    const idx = this.index();
    let total = this.bytes;
    let drop = 0;
    while (
      drop < idx.items.length &&
      (idx.items.length - drop + 1 > this.limits.maxFrames || total + bytes > this.limits.maxBytes)
    ) {
      total -= idx.items[drop]!.bytes;
      drop++;
    }
    this.dropFirst(drop);
    const id = idx.next++;
    this.kv.put(this.frameKey(id), msg);
    const item: Item = { id, at: now, bytes, from: meta.from };
    if (meta.viaPairing) item.p = true;
    idx.items.push(item);
    this.save();
    return { queued: true, dropped: drop };
  }

  /** Every live frame, oldest first (expired ones are pruned). Call `ack(n)` for the ones actually sent. */
  peek(now: number): QueuedFrame[] {
    this.prune(now);
    const out: QueuedFrame[] = [];
    let missing = false;
    for (const item of this.index().items) {
      const msg = this.kv.get<string>(this.frameKey(item.id));
      if (typeof msg === "string") out.push({ id: item.id, msg });
      else missing = true;
    }
    if (missing) {
      // Index and frames disagree (should not happen): keep only frames that still exist.
      const ids = new Set(out.map((f) => f.id));
      this.index().items = this.index().items.filter((i) => ids.has(i.id));
      this.save();
    }
    return out;
  }

  /** Remove the first `n` frames (after they were sent). */
  ack(n: number): void {
    if (n <= 0) return;
    this.dropFirst(n);
    this.save();
  }

  /**
   * Keep only frames for which `keep(from, viaPairing)` is true (e.g. after an allowlist change). Returns how many
   * were dropped.
   */
  retain(keep: (from: string, viaPairing: boolean) => boolean): number {
    const idx = this.index();
    const kept: Item[] = [];
    let dropped = 0;
    for (const item of idx.items) {
      if (keep(item.from, item.p === true)) kept.push(item);
      else {
        this.kv.delete(this.frameKey(item.id));
        dropped++;
      }
    }
    if (dropped > 0) {
      idx.items = kept;
      this.save();
    }
    return dropped;
  }

  /** When the oldest frame expires, or null if empty. */
  nextExpiry(): number | null {
    const first = this.index().items[0];
    return first ? first.at + this.limits.ttlMs : null;
  }

  clear(): void {
    this.dropFirst(this.index().items.length);
    this.save();
  }
}
