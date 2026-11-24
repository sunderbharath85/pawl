// Event backfill for one session, using HubEvent.seq as the cursor.
//
// The hub answers `get_events` with the latest `limit` events when no cursor is
// given, or with the events strictly after `sinceSeq` (oldest first). The client
// keeps only the newest window per session (HubState.events, deduped by global id),
// so:
//   - the first sync of a session (per HubClient) loads the latest window;
//   - later syncs (reconnects) catch up after the newest seq we hold; when the gap
//     is a full page or more, the latest window replaces the catch-up (anything
//     older would be trimmed by the client's cap anyway).
import { latestEventSeq, type HubState } from "./state";
import type { HubEvent } from "@pawl/protocol";

/** Events requested per page; matches hub-client's per-session cap. */
export const BACKFILL_WINDOW = 500;

/** Fetches (and merges into client state) events of one session. */
export type FetchEvents = (sinceSeq: number | undefined, limit: number) => Promise<readonly HubEvent[]>;

export type BackfillResult = "window" | "catch-up" | "catch-up+window";

/**
 * Run one backfill pass. `cursor` = newest `seq` already held (undefined on the first
 * sync, or when nothing is held): loads the latest window instead of catching up.
 */
export async function backfillSession(
  fetch: FetchEvents,
  cursor: number | undefined,
  window = BACKFILL_WINDOW,
): Promise<BackfillResult> {
  if (cursor === undefined) {
    await fetch(undefined, window);
    return "window";
  }
  const page = await fetch(cursor, window);
  if (page.length < window) return "catch-up";
  await fetch(undefined, window);
  return "catch-up+window";
}

/** The slice of HubClient used here (kept structural so tests can fake it). */
export interface EventSource {
  getState(): Pick<HubState, "events">;
  getEvents(sessionId: string, sinceSeq?: number, limit?: number): Promise<readonly HubEvent[]>;
}

/**
 * Tracks which sessions were already backfilled per client and collapses concurrent
 * syncs of the same session into one request chain.
 */
export class SessionBackfill {
  private readonly synced = new WeakMap<EventSource, Set<string>>();
  private readonly inflight = new WeakMap<EventSource, Map<string, Promise<BackfillResult>>>();

  constructor(private readonly window = BACKFILL_WINDOW) {}

  sync(client: EventSource, sessionId: string): Promise<BackfillResult> {
    let running = this.inflight.get(client);
    if (!running) this.inflight.set(client, (running = new Map()));
    const existing = running.get(sessionId);
    if (existing) return existing;

    let done = this.synced.get(client);
    if (!done) this.synced.set(client, (done = new Set()));
    const synced = done;
    const cursor = synced.has(sessionId) ? latestEventSeq(client.getState(), sessionId) : undefined;
    const task = backfillSession((sinceSeq, limit) => client.getEvents(sessionId, sinceSeq, limit), cursor, this.window)
      .then((result) => {
        synced.add(sessionId);
        return result;
      })
      .finally(() => running.delete(sessionId));
    running.set(sessionId, task);
    return task;
  }
}

/** A shared tracker (each window / extension host loads its own module instance). */
export const sessionBackfill = new SessionBackfill();
