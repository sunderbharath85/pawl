import { useCallback, useEffect, useState } from "react";

/** Local, per-machine desktop preferences (localStorage is shared by both windows). */
export interface DesktopSettings {
  notifyPermissions: boolean;
  notifyTurns: boolean;
  notifyHub: boolean;
  /** Pet sprite size on the strip, px. */
  petSize: number;
  lastCwd: string;
  lastAgentId: string;
}

const KEY = "aipet.desktop.settings.v1";

export const DEFAULT_SETTINGS: DesktopSettings = {
  notifyPermissions: true,
  notifyTurns: true,
  notifyHub: true,
  petSize: 112,
  lastCwd: "",
  lastAgentId: "",
};

export function readSettings(): DesktopSettings {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return DEFAULT_SETTINGS;
    const parsed = JSON.parse(raw) as Partial<DesktopSettings>;
    return { ...DEFAULT_SETTINGS, ...parsed };
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
  window.dispatchEvent(new CustomEvent("aipet-settings", { detail: next }));
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
    window.addEventListener("aipet-settings", onLocal);
    return () => {
      window.removeEventListener("storage", onStorage);
      window.removeEventListener("aipet-settings", onLocal);
    };
  }, []);
  const update = useCallback((patch: Partial<DesktopSettings>) => setSettings(writeSettings(patch)), []);
  return [settings, update];
}
