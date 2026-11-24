// Pet name rules (mirrors the hub's `update_pet` checks): trimmed, 1–40 characters, and
// not already used by another pet on any known machine (case-insensitive), so paired
// machines never show two "Biscuit"s.
import type { Pet } from "@krypto/protocol";

export const PET_NAME_MAX = 40;

export type PetNameError = "invalid" | "name_taken";
export type PetNameCheck = { ok: true; name: string } | { ok: false; code: PetNameError; error: string };

const fold = (s: string) => s.trim().toLocaleLowerCase();

export function petNameMessage(code: PetNameError, name?: string): string {
  if (code === "name_taken") {
    return name ? `Another pet already answers to “${name}”. Pick a different name.` : "Another pet already has that name. Pick a different one.";
  }
  return `Pet names need 1–${PET_NAME_MAX} characters.`;
}

/** Validate a new name for pet `petId` against every other known pet. */
export function checkPetName(raw: string, petId: string, pets: Iterable<Pet>): PetNameCheck {
  const name = raw.trim();
  const length = [...name].length;
  if (length < 1 || length > PET_NAME_MAX) return { ok: false, code: "invalid", error: petNameMessage("invalid") };
  const key = fold(name);
  for (const p of pets) {
    if (p.id !== petId && fold(p.name) === key) return { ok: false, code: "name_taken", error: petNameMessage("name_taken", p.name) };
  }
  return { ok: true, name };
}

/** Friendly text for a hub rejection of a pet rename (`name_taken` / `invalid`), else undefined. */
export function petRenameErrorText(err: unknown, attempted?: string): string | undefined {
  const code = err && typeof err === "object" && "code" in err ? String((err as { code: unknown }).code) : "";
  if (code === "name_taken" || code === "invalid") return petNameMessage(code, attempted);
  return undefined;
}
