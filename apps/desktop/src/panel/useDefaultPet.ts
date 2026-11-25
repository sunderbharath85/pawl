// The one pet the panel shows: the default pet (it orchestrates every session). Its own name (the hub's
// `bot.petName`), its species (this machine's setting) and what it is up to across every session and machine.
import { useMemo } from "react";
import type { PetActivity } from "@pawl/pet-engine";
import type { Species } from "@pawl/protocol";
import { defaultPetState, type DefaultPetMood } from "../lib/petScreen";
import { useSettings } from "../lib/settings";
import { useDefaultPetName } from "../lib/usePetName";
import { usePanel } from "./context";

export interface DefaultPetView {
  name: string;
  species: Species;
  activity: PetActivity;
  mood: DefaultPetMood;
  line: string;
}

export function useDefaultPet(): DefaultPetView {
  const { client, state, views, meta } = usePanel();
  const [settings] = useSettings();
  const connected = meta.phase === "connected";
  const { name } = useDefaultPetName(client, state, connected);
  const pet = useMemo(() => defaultPetState(state, views), [state, views]);
  return {
    name,
    species: settings.defaultPetSpecies,
    activity: connected ? pet.activity : "napping",
    mood: connected ? pet.mood : "napping",
    line: connected ? pet.line : "The hub is not connected.",
  };
}
