// Admission of a new unauthenticated socket into a mailbox that already has pending ones.
// We cannot see whether a pending socket's `auth` is in flight, only how long ago it got its challenge. A real
// device answers within one round trip, so a pending socket older than the grace period has not sent auth and is
// the one to evict; a younger one may be mid-auth and is never evicted in favour of a fresh connection.

import { AUTH_GRACE_MS, MAX_PENDING_SOCKETS } from "./limits";

export interface PendingSocket<T> {
  socket: T;
  /** When its challenge was sent (ms). */
  at: number;
}

export interface Admission<T> {
  admit: boolean;
  /** Pending sockets to close (bad_auth) before admitting. Empty when not admitting. */
  evict: T[];
}

export function planAdmission<T>(
  pending: readonly PendingSocket<T>[],
  now: number,
  max: number = MAX_PENDING_SOCKETS,
  graceMs: number = AUTH_GRACE_MS,
): Admission<T> {
  const need = pending.length - max + 1;
  if (need <= 0) return { admit: true, evict: [] };
  const stale = pending.filter((p) => now - p.at >= graceMs).sort((a, b) => a.at - b.at);
  if (stale.length < need) return { admit: false, evict: [] };
  return { admit: true, evict: stale.slice(0, need).map((p) => p.socket) };
}
