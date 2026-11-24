import { activeMachineId, type HubState } from "@pawl/hub-client";
import { derivePetViews, lastSignificantEvents, type PetActivity, type PetView } from "@pawl/pet-engine";

/** Higher = more important to surface in the status bar. */
export const ACTIVITY_PRIORITY: Record<PetActivity, number> = {
  alert: 7,
  working: 6,
  thinking: 5,
  planning: 4,
  sad: 3,
  happy: 2,
  idle: 1,
  napping: 0,
};

export const ACTIVITY_LABEL: Record<PetActivity, string> = {
  napping: "napping",
  idle: "idle",
  thinking: "thinking",
  working: "working",
  planning: "planning",
  alert: "needs you",
  happy: "done",
  sad: "hit a snag",
};

export function petViewsOf(state: HubState, now = Date.now()): PetView[] {
  try {
    return derivePetViews({
      pets: Object.values(state.pets),
      sessions: Object.values(state.sessions),
      pending: Object.values(state.pending),
      lastEvents: lastSignificantEvents(state.events),
      machines: state.machines,
      // Visitors and "runs to you" (PLAN.md migration step 3).
      localMachineId: state.machine?.id,
      activeMachineId: activeMachineId(state, now),
      now,
    });
  } catch {
    return []; // never let pet derivation break the status bar
  }
}

/** The pet that best represents "what's going on" right now. */
export function primaryView(views: PetView[]): PetView | undefined {
  let best: PetView | undefined;
  for (const v of views) {
    if (!best) {
      best = v;
      continue;
    }
    const d = ACTIVITY_PRIORITY[v.activity] - ACTIVITY_PRIORITY[best.activity];
    if (d > 0 || (d === 0 && v.since > best.since)) best = v;
  }
  return best;
}
