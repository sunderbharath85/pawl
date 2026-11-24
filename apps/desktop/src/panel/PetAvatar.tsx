import { PetSprite } from "@krypto/pet-engine/react";
import { SPECIES_META, petLine, type PetActivity } from "@krypto/pet-engine";
import type { Species } from "@krypto/protocol";
import { ErrorBoundary } from "../lib/ErrorBoundary";
import { usePanel } from "./context";

interface Props {
  species: Species;
  activity: PetActivity;
  size?: number;
  facing?: "left" | "right";
  skin?: string;
  className?: string;
}

/** PetSprite with sprites from context and an emoji fallback if rendering fails. */
export function PetAvatar({ species, activity, size = 48, facing = "right", skin, className }: Props) {
  const { sprites } = usePanel();
  const emoji = SPECIES_META[species]?.emoji ?? "🐕";
  return (
    <span className={`pet-avatar ${className ?? ""}`} style={{ width: size, height: size }}>
      <ErrorBoundary
        resetKey={`${species}:${activity}:${sprites ? 1 : 0}`}
        fallback={
          <span className="pet-avatar-fallback" style={{ fontSize: size * 0.62 }}>
            {emoji}
          </span>
        }
      >
        <PetSprite species={species} activity={activity} size={size} facing={facing} skin={skin} sprites={sprites} />
      </ErrorBoundary>
    </span>
  );
}

export function safePetLine(species: Species, activity: PetActivity, seed?: number): string {
  try {
    return petLine(species, activity, seed);
  } catch {
    return ACTIVITY_LABEL[activity];
  }
}

export const ACTIVITY_LABEL: Record<PetActivity, string> = {
  napping: "Napping in the bed",
  idle: "Hanging out",
  thinking: "Head tilted, thinking",
  working: "Digging and fetching",
  planning: "Carrying the checklist",
  alert: "Needs you!",
  happy: "Zoomies!",
  sad: "A little droopy",
};

const URGENCY: Record<PetActivity, number> = {
  alert: 7,
  sad: 6,
  working: 5,
  planning: 4,
  thinking: 3,
  happy: 2,
  idle: 1,
  napping: 0,
};

export function activityUrgency(a: PetActivity): number {
  return URGENCY[a];
}
