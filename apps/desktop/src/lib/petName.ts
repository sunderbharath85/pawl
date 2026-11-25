// The default pet's own name (pure). It lives in the hub (`bot.petName`, picked from the hub's pet-name pool on first
// start, never "Pawl", the product's name), so every window and machine shows the same one. This app keeps the last
// name it saw, to show while the hub is down, and once sends a name the person had chosen in an older version
// (when the name was a desktop setting).
import type { Pet } from "@pawl/protocol";
import { checkDefaultPetName } from "./petScreen";

/** Shown when no name is known at all (a first start with the hub down). */
export const FALLBACK_PET_NAME = "Your pet";

/** The product's name, which older versions showed as the pet's name: never a chosen name. */
const PRODUCT_NAME = "pawl";

function chosen(name: string | null | undefined): string {
  const t = (name ?? "").trim();
  return t.toLowerCase() === PRODUCT_NAME ? "" : t;
}

/**
 * The name to show: the hub's; while the hub is not reached, a name chosen in an older version that is still on its
 * way to the hub, else the last one seen; else "Your pet".
 */
export function shownPetName(input: { hubName?: string | null; cached?: string | null; legacy?: string | null }): string {
  return (input.hubName ?? "").trim() || chosen(input.legacy) || chosen(input.cached) || FALLBACK_PET_NAME;
}

/**
 * The name an older version kept on this machine, to send to the hub once: only one the person had actually chosen
 * (not "Pawl"), not already the hub's, and valid as a pet name (1-40 characters, no invisible characters, not
 * another pet's). Null: nothing to send.
 */
export function legacyNameToSend(input: { legacy: string | null | undefined; hubName: string; pets: Iterable<Pet> }): string | null {
  const legacy = chosen(input.legacy);
  if (!legacy || legacy === input.hubName.trim()) return null;
  const check = checkDefaultPetName(legacy, input.pets);
  return check.ok ? check.name : null;
}
