// Keeps which chat proposals were used in localStorage, which the panel and the pet bubble share (and which outlives
// a reload), so a proposal confirmed in one window cannot be confirmed again in the other. Pure apart from the
// storage it is given.
import { MAX_LEDGER_ENTRIES, type ProposalEntry, type ProposalLedgerStore } from "@pawl/hub-client";

export const PROPOSALS_KEY = "pawl.desktop.proposals.v1";

/** A "running" entry older than this lost its window mid-run (closed, reloaded): it shows as sent, unanswered. */
export const STALE_RUNNING_MS = 2 * 60_000;

export const STALE_RUNNING_TEXT = "Sent, but no answer came back here. Check that machine before trying again.";

type Row = [messageId: string, entry: ProposalEntry, at: number];

function isEntry(v: unknown): v is ProposalEntry {
  if (!v || typeof v !== "object") return false;
  const e = v as Record<string, unknown>;
  if (typeof e.proposalId !== "string") return false;
  switch (e.status) {
    case "running":
      return true;
    case "done":
      return typeof e.text === "string" && (e.sessionId === undefined || typeof e.sessionId === "string");
    case "failed":
      return typeof e.text === "string" && typeof e.retryable === "boolean";
    default:
      return false;
  }
}

function readRows(storage: Pick<Storage, "getItem">): Row[] | null {
  let raw: string | null;
  try {
    raw = storage.getItem(PROPOSALS_KEY);
  } catch {
    return null;
  }
  if (!raw) return null;
  try {
    const parsed = JSON.parse(raw) as { v?: unknown; rows?: unknown };
    if (parsed.v !== 1 || !Array.isArray(parsed.rows)) return null;
    return parsed.rows.filter(
      (r): r is Row => Array.isArray(r) && r.length === 3 && typeof r[0] === "string" && isEntry(r[1]) && typeof r[2] === "number",
    );
  } catch {
    return null;
  }
}

/** A ledger store over `storage` (window.localStorage in the app); null storage = nothing is kept. */
export function storageProposalStore(storage: Pick<Storage, "getItem" | "setItem"> | null, now: () => number = Date.now): ProposalLedgerStore {
  return {
    load() {
      if (!storage) return null;
      const rows = readRows(storage);
      if (!rows) return null;
      const t = now();
      return rows.slice(-MAX_LEDGER_ENTRIES).map(([id, entry, at]): [string, ProposalEntry] =>
        entry.status === "running" && t - at > STALE_RUNNING_MS
          ? [id, { status: "failed", proposalId: entry.proposalId, text: STALE_RUNNING_TEXT, retryable: false }]
          : [id, entry],
      );
    },
    save(entries) {
      if (!storage) return;
      // Keep each unchanged entry's time; a changed or new one is stamped now.
      const before = new Map((readRows(storage) ?? []).map(([id, e, at]) => [id, { json: JSON.stringify(e), at }]));
      const t = now();
      const rows: Row[] = entries.slice(-MAX_LEDGER_ENTRIES).map(([id, e]) => {
        const old = before.get(id);
        return [id, e, old && old.json === JSON.stringify(e) ? old.at : t];
      });
      try {
        storage.setItem(PROPOSALS_KEY, JSON.stringify({ v: 1, rows }));
      } catch {
        /* storage full or disabled: this window still remembers */
      }
    },
  };
}

/** window.localStorage when it can be used, else null. */
export function browserStorage(): Storage | null {
  try {
    return typeof window !== "undefined" && window.localStorage ? window.localStorage : null;
  } catch {
    return null;
  }
}
