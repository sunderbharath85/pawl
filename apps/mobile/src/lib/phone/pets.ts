// Pet views for the phone (all machines' pets; the phone has no "home" machine), re-derived when a decay / nap
// timer would change them.
import { derivePetViews, lastSignificantEvents, nextPetViewChange, type PetActivity, type PetView } from "@krypto/pet-engine";
import { useEffect, useMemo, useState } from "react";
import type { PhoneSnapshot } from "@/lib/peer";

const PRIORITY: Record<PetActivity, number> = { alert: 0, sad: 1, working: 2, thinking: 3, planning: 4, happy: 5, idle: 6, napping: 7 };

export function usePetViews(snapshot: PhoneSnapshot): PetView[] {
  const [now, setNow] = useState(() => Date.now());
  const input = useMemo(
    () => ({
      pets: Object.values(snapshot.pets),
      sessions: Object.values(snapshot.sessions),
      pending: Object.values(snapshot.pending),
      lastEvents: lastSignificantEvents(snapshot.events),
      machines: snapshot.machines,
      now,
    }),
    [snapshot, now],
  );
  const views = useMemo(() => derivePetViews(input), [input]);
  useEffect(() => {
    const next = nextPetViewChange(input);
    const wait = next === null ? 30_000 : Math.max(250, Math.min(60_000, next - Date.now()));
    const h = setTimeout(() => setNow(Date.now()), wait);
    return () => clearTimeout(h);
  }, [input]);
  return useMemo(
    () =>
      [...views].sort(
        (a, b) =>
          Number(a.remoteOffline) - Number(b.remoteOffline) ||
          PRIORITY[a.activity] - PRIORITY[b.activity] ||
          a.name.localeCompare(b.name),
      ),
    [views],
  );
}
