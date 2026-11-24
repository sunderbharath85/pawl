// Who may drop frames into a mailbox: the allowed-sender set (paired devices' public keys, replaced wholesale by
// `allow`) and one-shot pairing slots (`allow_pairing`). Both persist in the mailbox's storage and are bounded.

import { RELAY_MAX_ALLOWED } from "@pawl/protocol";
import { bytesEqual, utf8 } from "./b64";
import { PAIRING_MAX_TTL_MS, PAIRING_SLOT_MAX } from "./limits";
import type { KvStore } from "./storage";

export const ALLOW_KEY = "allow";
export const SLOTS_KEY = "slots";

export interface PairingSlot {
  token: string;
  exp: number;
  /** When the slot was registered (eviction order). */
  at: number;
}

export type SlotResult = { ok: true; exp: number; changed: boolean } | { ok: false };
export type AllowResult = { ok: true; changed: boolean } | { ok: false };

/** Re-registering a token whose expiry moved less than this is "unchanged". */
const SLOT_EXP_TOLERANCE_MS = 1000;

export class AccessControl {
  private allowed: Set<string> | null = null;
  private slots: PairingSlot[] | null = null;

  constructor(private readonly kv: KvStore) {}

  private allowSet(): Set<string> {
    if (!this.allowed) this.allowed = new Set(this.kv.get<string[]>(ALLOW_KEY) ?? []);
    return this.allowed;
  }

  private slotList(): PairingSlot[] {
    if (!this.slots) this.slots = this.kv.get<PairingSlot[]>(SLOTS_KEY) ?? [];
    return this.slots;
  }

  private saveSlots(slots: PairingSlot[]): void {
    this.slots = slots;
    if (slots.length === 0) this.kv.delete(SLOTS_KEY);
    else this.kv.put(SLOTS_KEY, slots);
  }

  /**
   * Replace the allowed senders. `pks` must already be valid pks (frames.ts); duplicates are folded. An unchanged
   * set is not written again (devices re-send `allow` on every reconnect).
   */
  setAllowed(pks: readonly string[]): AllowResult {
    const set = new Set(pks);
    if (set.size > RELAY_MAX_ALLOWED) return { ok: false };
    const prev = this.allowSet();
    if (prev.size === set.size && [...set].every((pk) => prev.has(pk))) return { ok: true, changed: false };
    this.allowed = set;
    if (set.size === 0) this.kv.delete(ALLOW_KEY);
    else this.kv.put(ALLOW_KEY, [...set]);
    return { ok: true, changed: true };
  }

  listAllowed(): string[] {
    return [...this.allowSet()];
  }

  isAllowed(pk: string): boolean {
    return this.allowSet().has(pk);
  }

  /** True when at least one sender is allowed. */
  hasAllowed(): boolean {
    return this.allowSet().size > 0;
  }

  /**
   * Register a one-shot slot. `exp` must be in the future; it is clamped to now + 1 h so a device clock a little
   * ahead of ours does not break a max-TTL invite. The same live token again with the same expiry (within 1 s) is
   * a no-op (no write); with another expiry it is updated in place. When 4 slots are live the oldest registration
   * is evicted (the newest invite is the one the user is about to use).
   */
  addPairingSlot(token: string, exp: number, now: number): SlotResult {
    if (!Number.isFinite(exp) || exp <= now) return { ok: false };
    const clamped = Math.min(exp, now + PAIRING_MAX_TTL_MS);
    const current = this.slotList();
    const same = current.find((s) => s.token === token && s.exp > now);
    if (same && Math.abs(same.exp - clamped) < SLOT_EXP_TOLERANCE_MS) return { ok: true, exp: same.exp, changed: false };
    const slots = current.filter((s) => s.exp > now && s.token !== token);
    slots.push({ token, exp: clamped, at: same?.at ?? now });
    slots.sort((a, b) => a.at - b.at);
    while (slots.length > PAIRING_SLOT_MAX) slots.shift();
    this.saveSlots(slots);
    return { ok: true, exp: clamped, changed: true };
  }

  /** Consume the live slot matching `token` (constant-time compare). True iff one was consumed. */
  consumePairing(token: string, now: number): boolean {
    const slots = this.slotList();
    const want = utf8(token);
    let hit = -1;
    for (let i = 0; i < slots.length; i++) {
      const s = slots[i]!;
      if (bytesEqual(utf8(s.token), want) && s.exp > now && hit === -1) hit = i;
    }
    const live = slots.filter((s, i) => i !== hit && s.exp > now);
    if (hit !== -1 || live.length !== slots.length) this.saveSlots(live);
    return hit !== -1;
  }

  /** Drop expired slots; returns how many were removed. */
  pruneSlots(now: number): number {
    const slots = this.slotList();
    const live = slots.filter((s) => s.exp > now);
    if (live.length !== slots.length) this.saveSlots(live);
    return slots.length - live.length;
  }

  liveSlots(now: number): number {
    return this.slotList().filter((s) => s.exp > now).length;
  }

  nextSlotExpiry(): number | null {
    const slots = this.slotList();
    return slots.length === 0 ? null : Math.min(...slots.map((s) => s.exp));
  }
}
