import { useMemo } from "react";
import { lastEventsBySession, type HubState } from "@krypto/hub-client";
import { derivePetViews, type PetView } from "@krypto/pet-engine";
import { DEFAULT_SPECIES, type Pet } from "@krypto/protocol";

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

export function petList(state: HubState): Pet[] {
  const pets = Object.values(state.pets);
  return pets.length > 0 ? pets : [fallbackPet(state.machine?.id ?? "")];
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
        now,
      });
    } catch (err) {
      console.error("[desktop] derivePetViews failed", err);
      return [];
    }
  }, [state, now]);
}
