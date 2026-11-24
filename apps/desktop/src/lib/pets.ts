import { useMemo } from "react";
import { activeMachineId, lastEventsBySession, type HubState } from "@pawl/hub-client";
import { derivePetViews, type PetView } from "@pawl/pet-engine";
import { DEFAULT_SPECIES, type Pet } from "@pawl/protocol";

/** Client-only placeholder id (never sent to the hub; real pet ids are global). */
export const FALLBACK_PET_ID = "local-dog";

/** Shown when the hub has no pets yet (or is offline) so the strip is never empty. */
export function fallbackPet(machineId: string): Pet {
  return {
    id: FALLBACK_PET_ID,
    machineId,
    agentId: "",
    species: DEFAULT_SPECIES,
    skin: "default",
    name: "Biscuit",
    personality: "eager, loyal, a bit goofy",
    mood: 70,
    xp: 0,
  };
}

/** Every machine's pets; plus the placeholder dog when this machine has none of its own yet. */
export function petList(state: HubState): Pet[] {
  const pets = Object.values(state.pets);
  const own = state.machine?.id;
  const hasLocal = pets.some((p) => !own || p.machineId === own);
  return hasLocal ? pets : [fallbackPet(own ?? ""), ...pets];
}

export function usePetViews(state: HubState, now: number): PetView[] {
  return useMemo(() => {
    try {
      return derivePetViews({
        pets: petList(state),
        sessions: Object.values(state.sessions),
        pending: Object.values(state.pending),
        lastEvents: lastEventsBySession(state),
        machines: state.machines,
        // Visitors and "runs to you" (PLAN.md migration step 3).
        localMachineId: state.machine?.id,
        activeMachineId: activeMachineId(state, now),
        now,
      });
    } catch (err) {
      console.error("[desktop] derivePetViews failed", err);
      return [];
    }
  }, [state, now]);
}
