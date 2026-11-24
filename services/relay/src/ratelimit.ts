// Per-socket frame limiter: a token bucket, a throttle on `rate_limited` notices and an abuse cut-off.

import {
  RATE_ABUSE_DROPS,
  RATE_ABUSE_WINDOW_MS,
  RATE_BURST,
  RATE_NOTICE_INTERVAL_MS,
  RATE_PER_SEC,
} from "./limits";

export class TokenBucket {
  private tokens: number;
  private last: number;

  constructor(
    private readonly ratePerSec: number,
    private readonly burst: number,
    now: number,
  ) {
    this.tokens = burst;
    this.last = now;
  }

  take(now: number, n = 1): boolean {
    if (now > this.last) {
      this.tokens = Math.min(this.burst, this.tokens + ((now - this.last) * this.ratePerSec) / 1000);
      this.last = now;
    }
    if (this.tokens >= n) {
      this.tokens -= n;
      return true;
    }
    return false;
  }

  available(now: number): number {
    const t = now > this.last ? Math.min(this.burst, this.tokens + ((now - this.last) * this.ratePerSec) / 1000) : this.tokens;
    return Math.floor(t);
  }
}

/** ok: handle the frame. drop: ignore it. notify: ignore it and send `rate_limited`. close: close the socket. */
export type LimitVerdict = "ok" | "drop" | "notify" | "close";

export interface FrameLimiterOptions {
  ratePerSec: number;
  burst: number;
  noticeIntervalMs: number;
  abuseDrops: number;
  abuseWindowMs: number;
}

export const DEFAULT_LIMITER: FrameLimiterOptions = {
  ratePerSec: RATE_PER_SEC,
  burst: RATE_BURST,
  noticeIntervalMs: RATE_NOTICE_INTERVAL_MS,
  abuseDrops: RATE_ABUSE_DROPS,
  abuseWindowMs: RATE_ABUSE_WINDOW_MS,
};

export class FrameLimiter {
  private readonly bucket: TokenBucket;
  private lastNotice = -Infinity;
  private windowStart: number;
  private windowDrops = 0;

  constructor(
    now: number,
    private readonly opts: FrameLimiterOptions = DEFAULT_LIMITER,
  ) {
    this.bucket = new TokenBucket(opts.ratePerSec, opts.burst, now);
    this.windowStart = now;
  }

  check(now: number): LimitVerdict {
    if (this.bucket.take(now)) return "ok";
    if (now - this.windowStart >= this.opts.abuseWindowMs) {
      this.windowStart = now;
      this.windowDrops = 0;
    }
    this.windowDrops++;
    if (this.windowDrops >= this.opts.abuseDrops) return "close";
    if (now - this.lastNotice >= this.opts.noticeIntervalMs) {
      this.lastNotice = now;
      return "notify";
    }
    return "drop";
  }
}
