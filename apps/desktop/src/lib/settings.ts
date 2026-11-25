import { useCallback, useEffect, useState } from "react";
import { DEFAULT_SPECIES, type Species } from "@pawl/protocol";
import { SPECIES_META } from "@pawl/pet-engine";
import type { PetAnimationMode } from "./petMotion";
import type { PetsOnScreen } from "./petScreen";

/** Local, per-machine desktop preferences (localStorage is shared by both windows). */
export interface DesktopSettings {
  notifyPermissions: boolean;
  notifyTurns: boolean;
  notifyHub: boolean;
  /** The default pet's size on screen, px (session pets are drawn smaller). */
  petSize: number;
  /** Name label under each pet on screen (on by default: every pet has a name). */
  showPetNames: boolean;
  /** "Pets on screen": the default pet only (default), or plus one pet per active session. */
  petsOnScreen: PetsOnScreen;
  /** "Pet animation": still unless the person chats with the pet (default), or always following the agents. */
  petAnimation: PetAnimationMode;
  /** The default pet's species (its name lives in the hub: `bot.petName`). Local to this machine. */
  defaultPetSpecies: Species;
  /** The default pet's name as last seen from the hub, shown while the hub is down. */
  lastPetName: string;
  /**
   * A name chosen in an older version, when the name was a desktop setting (`defaultPetName`): sent to the hub once
   * (see lib/petName.ts), then cleared.
   */
  legacyPetName: string;
  /** That one-time move to the hub is done. */
  petNameMigrated: boolean;
  lastCwd: string;
  lastAgentId: string;
  /** The first-run setup opened once on its own (it is reopened from Settings). */
  setupOffered: boolean;
  /** The first-run setup was finished (not just skipped). */
  setupDone: boolean;
  /** "Read replies aloud": the system voice (its voiceURI; "" = the system default) and the speaking rate (0.5-2). */
  speechVoice: string;
  speechRate: number;
}

const KEY = "pawl.desktop.settings.v1";

export const DEFAULT_SETTINGS: DesktopSettings = {
  notifyPermissions: true,
  notifyTurns: true,
  notifyHub: true,
  petSize: 112,
  showPetNames: true,
  petsOnScreen: "default",
  petAnimation: "chat",
  defaultPetSpecies: DEFAULT_SPECIES,
  lastPetName: "",
  legacyPetName: "",
  petNameMigrated: false,
  lastCwd: "",
  lastAgentId: "",
  setupOffered: false,
  setupDone: false,
  speechVoice: "",
  speechRate: 1,
};

/** Stored settings merged over the defaults; keys that are missing (older versions) or malformed get the default. */
export function normalizeSettings(stored: Partial<DesktopSettings> | null | undefined): DesktopSettings {
  const s: DesktopSettings = { ...DEFAULT_SETTINGS };
  const raw: Record<string, unknown> = stored && typeof stored === "object" && !Array.isArray(stored) ? stored : {};
  // Every known key takes the stored value only when it has the default's type (a boolean stays a boolean,
  // a string a string); unknown keys are dropped.
  for (const key of Object.keys(DEFAULT_SETTINGS) as Array<keyof DesktopSettings>) {
    const v = raw[key];
    if (v !== undefined && typeof v === typeof DEFAULT_SETTINGS[key]) (s as unknown as Record<string, unknown>)[key] = v;
  }
  if (s.petsOnScreen !== "default" && s.petsOnScreen !== "sessions") s.petsOnScreen = DEFAULT_SETTINGS.petsOnScreen;
  if (s.petAnimation !== "chat" && s.petAnimation !== "always") s.petAnimation = DEFAULT_SETTINGS.petAnimation;
  // An older version's local pet name: kept as legacyPetName until it was offered to the hub.
  const old = raw["defaultPetName"];
  if (!s.petNameMigrated && !s.legacyPetName && typeof old === "string") s.legacyPetName = old.trim();
  if (!Object.prototype.hasOwnProperty.call(SPECIES_META, s.defaultPetSpecies)) s.defaultPetSpecies = DEFAULT_SETTINGS.defaultPetSpecies;
  if (typeof s.petSize !== "number" || !Number.isFinite(s.petSize)) s.petSize = DEFAULT_SETTINGS.petSize;
  s.petSize = Math.min(168, Math.max(72, s.petSize));
  if (!Number.isFinite(s.speechRate)) s.speechRate = DEFAULT_SETTINGS.speechRate;
  s.speechRate = Math.min(2, Math.max(0.5, s.speechRate));
  return s;
}

export function readSettings(): DesktopSettings {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return DEFAULT_SETTINGS;
    return normalizeSettings(JSON.parse(raw) as Partial<DesktopSettings>);
  } catch {
    return DEFAULT_SETTINGS;
  }
}

export function writeSettings(patch: Partial<DesktopSettings>): DesktopSettings {
  const next = { ...readSettings(), ...patch };
  try {
    localStorage.setItem(KEY, JSON.stringify(next));
  } catch {
    /* storage full/disabled: keep in-memory only */
  }
  window.dispatchEvent(new CustomEvent("pawl-settings", { detail: next }));
  return next;
}

/** Settings + setter; stays in sync across windows via the `storage` event. */
export function useSettings(): [DesktopSettings, (patch: Partial<DesktopSettings>) => void] {
  const [settings, setSettings] = useState(readSettings);
  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key === KEY) setSettings(readSettings());
    };
    const onLocal = () => setSettings(readSettings());
    window.addEventListener("storage", onStorage);
    window.addEventListener("pawl-settings", onLocal);
    return () => {
      window.removeEventListener("storage", onStorage);
      window.removeEventListener("pawl-settings", onLocal);
    };
  }, []);
  const update = useCallback((patch: Partial<DesktopSettings>) => setSettings(writeSettings(patch)), []);
  return [settings, update];
}
