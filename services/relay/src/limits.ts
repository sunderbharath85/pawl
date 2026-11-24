// Relay-local limits and close codes. Wire-level caps (RELAY_MAX_ALLOWED, RELAY_QUEUE_*, RELAY_FRAME_MAX_BYTES)
// come from @krypto/protocol; everything here is an implementation bound of this relay.

/** A device must answer the challenge within this long. */
export const AUTH_TIMEOUT_MS = 10_000;
/** Largest `auth` frame accepted (the real one is ~500 bytes). */
export const AUTH_FRAME_MAX_BYTES = 2048;
/**
 * Unauthenticated sockets per mailbox. To admit a new one when full, the oldest pending socket that has had its
 * challenge for at least AUTH_GRACE_MS (a real device answers within one round trip) is closed. Sockets younger than
 * that may be mid-auth and are never evicted for a fresh connection; if all are, the new connection gets 429.
 */
export const MAX_PENDING_SOCKETS = 8;
export const AUTH_GRACE_MS = 3000;

/** Frames per second (and burst) a mailbox accepts from all senders together, checked before the allowlist. */
export const INBOUND_PER_SEC = 100;
export const INBOUND_BURST = 400;
/** Config frames (allow, allow_pairing, register_push) per second (and burst) per mailbox. */
export const CONFIG_PER_SEC = 5;
export const CONFIG_BURST = 10;

/** Per-socket token bucket. */
export const RATE_PER_SEC = 50;
export const RATE_BURST = 200;
/** A socket that has this many frames dropped by the rate limiter inside one window is closed. */
export const RATE_ABUSE_DROPS = 5000;
export const RATE_ABUSE_WINDOW_MS = 10_000;
/** At most one `rate_limited` error per socket per this long (the rest are dropped silently). */
export const RATE_NOTICE_INTERVAL_MS = 1000;

/** Frames a mailbox is still forwarding to other mailboxes (memory bound). Beyond -> `rate_limited`. */
export const MAX_INFLIGHT_FRAMES = 256;
export const MAX_INFLIGHT_BYTES = 8 * 1024 * 1024;
/** Recipients a mailbox forwards to concurrently (one ordered chain each). */
export const MAX_INFLIGHT_RECIPIENTS = 128;

/**
 * A delivered `msg{from, env}` may be this much larger than the `send{to, env}` it came from (`from` vs `to`, and
 * numbers re-serialized without exponents). Anything bigger is refused with `too_large`.
 */
export const MSG_HEADROOM_BYTES = 64;

/** Pairing slots: at most this many live, each at most this far in the future. */
export const PAIRING_SLOT_MAX = 4;
export const PAIRING_MAX_TTL_MS = 60 * 60 * 1000;

/** Envelope field bounds (shape checks only; the relay never opens envelopes). */
export const ENV_ID_MAX_CHARS = 128;
export const ENV_NONCE_MAX_CHARS = 64;
export const ENV_PK_MAX_CHARS = 64;

/** `Host` header bound for the auth plaintext. */
export const HOST_MAX_CHARS = 255;

/** Alarms closer together than this are merged (avoids alarm thrash). */
export const ALARM_SLACK_MS = 50;

/** WebSocket close codes. */
export const CLOSE_NORMAL = 1000;
export const CLOSE_POLICY = 1008; // bad_auth, auth timeout, abuse
export const CLOSE_INTERNAL = 1011;
export const CLOSE_REPLACED = 4000; // a newer authenticated socket took over this mailbox
