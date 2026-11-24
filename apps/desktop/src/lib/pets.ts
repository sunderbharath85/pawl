import { useMemo } from "react";
import { lastEventsBySession, type HubState } from "@ai-pet/hub-client";
import { derivePetViews, type PetView } from "@ai-pet/pet-engine";
import { DEFAULT_SPECIES, type Pet } from "@ai-pet/protocol";

/** Shown when the hub has no pets yet (or is offline) so the strip is never empty. */
export const FALLBACK_PET: Pet = {
  id: "local-dog",
  agentId: "",
  species: DEFAULT_SPECIES,
  skin: "default",
  name: "Biscuit",
  personality: "eager, loyal, a bit goofy",
  mood: 70,
  xp: 0,
};

export function petList(state: HubState): Pet[] {
  const pets = Object.values(state.pets);
  return pets.length > 0 ? pets : [FALLBACK_PET];
}

export function usePetViews(state: HubState, now: number): PetView[] {
  return useMemo(() => {
    try {
      return derivePetViews({
        pets: petList(state),
        sessions: Object.values(state.sessions),
        pending: Object.values(state.pending),
        lastEvents: lastEventsBySession(state),
        now,
      });
    } catch (err) {
      console.error("[desktop] derivePetViews failed", err);
      return [];
    }
  }, [state, now]);
}
