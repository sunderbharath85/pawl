// The decisions a mailbox makes, without the Workers runtime: who may write to it, what to do with an inbound
// frame (deliver live, queue, refuse), config frames from its owner, and when a push wake is allowed. The Durable
// Object (mailbox.ts) only adds sockets, RPC and alarms around this.

import { RELAY_FRAME_MAX_BYTES, type PeerEnvelope } from "@pawl/protocol";
import { AccessControl, type SlotResult } from "./allow";
import { isValidPk, isValidToken, msgFrame, utf8Length } from "./frames";
import {
  CONFIG_BURST,
  CONFIG_PER_SEC,
  INBOUND_BURST,
  INBOUND_PER_SEC,
  MSG_HEADROOM_BYTES,
} from "./limits";
import { PushRegistry } from "./push";
import { DEFAULT_QUEUE_LIMITS, MailboxQueue, type QueueLimits, type QueuedFrame } from "./queue";
import { TokenBucket } from "./ratelimit";
import type { KvStore } from "./storage";

/** Set once, the first time the owner authenticates (push wakes need it). */
export const SEEN_KEY = "seen";

export interface DeliverRequest {
  /** The sender's authenticated public key (set by the sending mailbox, never by a client). */
  from: string;
  env: PeerEnvelope;
  pairing?: string;
}

export type DeliverResult = "delivered" | "queued" | "queue_full" | "not_allowed" | "too_large" | "rate_limited";

export interface DeliverOutcome {
  result: DeliverResult;
  /** Older queued frames dropped to make room. */
  dropped: number;
  /** A pairing slot admitted this frame. */
  viaPairing: boolean;
  /** Push token to wake, when the frame was queued and a wake is due. */
  wake: string | null;
}

export interface InboxOptions {
  inboundPerSec: number;
  inboundBurst: number;
  configPerSec: number;
  configBurst: number;
  queue: QueueLimits;
}

export const DEFAULT_INBOX: InboxOptions = {
  inboundPerSec: INBOUND_PER_SEC,
  inboundBurst: INBOUND_BURST,
  configPerSec: CONFIG_PER_SEC,
  configBurst: CONFIG_BURST,
  queue: DEFAULT_QUEUE_LIMITS,
};

export type AllowOutcome = { ok: true; changed: boolean; droppedQueued: number } | { ok: false };

export class Inbox {
  readonly acl: AccessControl;
  readonly queue: MailboxQueue;
  readonly push: PushRegistry;
  private inbound: TokenBucket | null = null;
  private config: TokenBucket | null = null;

  constructor(
    private readonly kv: KvStore,
    private readonly opts: InboxOptions = DEFAULT_INBOX,
  ) {
    this.acl = new AccessControl(kv);
    this.queue = new MailboxQueue(kv, opts.queue);
    this.push = new PushRegistry(kv);
  }

  // ---------- owner ----------

  /** The owner authenticated: remember that it exists (once) and return the queued frames to flush. */
  onAuthenticated(now: number): QueuedFrame[] {
    if (this.kv.get(SEEN_KEY) === undefined) this.kv.put(SEEN_KEY, now);
    return this.queue.peek(now);
  }

  /** Budget for config frames (allow, allow_pairing, register_push, unregister_push). False = `rate_limited`. */
  takeConfig(now: number): boolean {
    if (!this.config) this.config = new TokenBucket(this.opts.configPerSec, this.opts.configBurst, now);
    return this.config.take(now);
  }

  /** Replace the allowed senders; queued frames from senders no longer allowed are dropped. */
  setAllowed(pks: readonly string[]): AllowOutcome {
    const r = this.acl.setAllowed(pks);
    if (!r.ok) return { ok: false };
    if (!r.changed) return { ok: true, changed: false, droppedQueued: 0 };
    const droppedQueued = this.queue.retain((from, viaPairing) => viaPairing || this.acl.isAllowed(from));
    return { ok: true, changed: true, droppedQueued };
  }

  addPairingSlot(token: string, exp: number, now: number): SlotResult {
    return this.acl.addPairingSlot(token, exp, now);
  }

  /** Returns true when the stored token changed. */
  registerPush(token: string): boolean {
    return this.push.register(token);
  }

  /** `unregister_push`: no wake after this until a new `register_push`. Returns true when a token was removed. */
  unregisterPush(now: number): boolean {
    return this.push.clear(now);
  }

  /** Push wakes only for a mailbox whose owner has authenticated at least once and that has paired senders. */
  mayPush(): boolean {
    return this.kv.get(SEEN_KEY) !== undefined && this.acl.hasAllowed();
  }

  // ---------- inbound ----------

  /**
   * A frame from another mailbox. Order of checks, cheapest first and before anything is consumed: the mailbox's
   * inbound budget, shapes, size, then the (in-memory) allowlist, and only then a pairing slot. `sendLive` tries
   * the owner's live socket; if it cannot, the frame is queued.
   */
  deliver(req: DeliverRequest, now: number, sendLive: (msg: string) => boolean): DeliverOutcome {
    const out = (result: DeliverResult, extra: Partial<DeliverOutcome> = {}): DeliverOutcome => ({
      result,
      dropped: 0,
      viaPairing: false,
      wake: null,
      ...extra,
    });
    if (!this.inbound) this.inbound = new TokenBucket(this.opts.inboundPerSec, this.opts.inboundBurst, now);
    if (!this.inbound.take(now)) return out("rate_limited");
    if (!isValidPk(req.from) || (req.pairing !== undefined && !isValidToken(req.pairing))) return out("not_allowed");

    const msg = msgFrame(req.from, req.env);
    const bytes = utf8Length(msg);
    if (bytes > RELAY_FRAME_MAX_BYTES + MSG_HEADROOM_BYTES) return out("too_large");

    let viaPairing = false;
    if (!this.acl.isAllowed(req.from)) {
      if (req.pairing === undefined || !this.acl.consumePairing(req.pairing, now)) return out("not_allowed");
      viaPairing = true;
    }

    if (sendLive(msg)) return out("delivered", { viaPairing });

    const r = this.queue.push(msg, bytes, now, { from: req.from, viaPairing });
    if (!r.queued) return out("queue_full", { viaPairing });
    const wake = this.mayPush() ? this.push.claim(now) : null;
    return out(r.dropped > 0 ? "queue_full" : "queued", { dropped: r.dropped, viaPairing, wake });
  }
}
