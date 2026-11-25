// Small bits of voice state both windows share through localStorage (same origin, so the panel and the pet window
// see each other's writes; the storage event tells the other window).
import { useEffect, useState } from "react";

const SENT_KEY = "pawl.voice.sent.v1";
const SENT_MAX = 50;
const TEST_KEY = "pawl.voice.wakeTestUntil";

function readIds(): string[] {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(SENT_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

/** Chat messages that were spoken (shown as "You (voice): …"): the last 50 ids. */
export function markVoiceSent(messageId: string): void {
  const ids = readIds().filter((id) => id !== messageId);
  ids.push(messageId);
  try {
    localStorage.setItem(SENT_KEY, JSON.stringify(ids.slice(-SENT_MAX)));
    window.dispatchEvent(new StorageEvent("storage", { key: SENT_KEY }));
  } catch {
    /* storage full or blocked: the message just shows as typed */
  }
}

export function useVoiceSentIds(): ReadonlySet<string> {
  const [ids, setIds] = useState<ReadonlySet<string>>(() => new Set(readIds()));
  useEffect(() => {
    const on = (e: StorageEvent) => {
      if (e.key === SENT_KEY || e.key === null) setIds(new Set(readIds()));
    };
    window.addEventListener("storage", on);
    return () => window.removeEventListener("storage", on);
  }, []);
  return ids;
}

/**
 * Settings > Voice is testing the wake word: until then a wake (and the utterance it starts) belongs to the test, so
 * the pet window neither opens its bubble nor sends anything to the chat.
 */
export function startWakeTest(ms: number): void {
  localStorage.setItem(TEST_KEY, String(Date.now() + ms));
}

export function endWakeTest(): void {
  localStorage.removeItem(TEST_KEY);
}

export function wakeTestRunning(now = Date.now()): boolean {
  const until = Number(localStorage.getItem(TEST_KEY) ?? 0);
  return Number.isFinite(until) && until > now;
}
