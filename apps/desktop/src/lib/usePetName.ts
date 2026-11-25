// The default pet's name in this app's windows: the hub's `bot.petName` (see lib/petName.ts), the last one seen
// (shown while the hub is down), the one-time move of a name chosen in an older version, and renaming through
// update_settings with the hub's own validation.
import { useEffect, useRef } from "react";
import { parseSettingsError, SETTINGS_PROTOCOL_VERSION, settingsErrorText, type HubClient, type HubState } from "@pawl/hub-client";
import { legacyNameToSend, shownPetName } from "./petName";
import { checkDefaultPetName } from "./petScreen";
import { useSettings } from "./settings";

export type RenameResult = { ok: true; name: string } | { ok: false; error: string };

/** The hub's name for the default pet, or null when it is not known (not connected yet, or a hub without it). */
export function hubPetName(state: HubState): string | null {
  const name: unknown = state.settings?.bot?.petName;
  return typeof name === "string" && name.trim() ? name.trim() : null;
}

/**
 * Rename the default pet: checked here like every pet name (1-40 characters, no invisible characters, not another
 * pet's), then by the hub (it answers a bad name with a SettingsError, shown as its reason).
 */
export async function renameDefaultPet(client: HubClient, state: HubState, raw: string): Promise<RenameResult> {
  const current = hubPetName(state);
  if (current === null) return { ok: false, error: "This hub can't rename the pet yet. Update pawl and restart the hub." };
  const check = checkDefaultPetName(raw, Object.values(state.pets));
  if (!check.ok) return { ok: false, error: check.error };
  if (check.name === current) return { ok: true, name: current };
  try {
    await client.updateSettings({ bot: { petName: check.name } });
    return { ok: true, name: check.name };
  } catch (err) {
    // The hub's reasons read on from the field ("must be 1-40 characters …"): shown as "The name must be …".
    const fe = parseSettingsError(err);
    return { ok: false, error: fe ? `The name ${fe.reason.replace(/\.?$/, ".")}` : settingsErrorText(err) };
  }
}

export interface DefaultPetName {
  /** The name to show everywhere (the hub's, else the one being moved to it, else the last seen, else "Your pet"). */
  name: string;
  /** The hub is connected and has the setting, so a rename can be saved. */
  canRename: boolean;
}

/**
 * The default pet's name for a window. It asks the hub for its settings once per connection (after that the hub
 * sends every change) and keeps the name it saw. `migrate`: this window (the pet window, always running) sends the
 * name an older version kept on this machine to the hub, once.
 */
export function useDefaultPetName(client: HubClient, state: HubState, connected: boolean, opts: { migrate?: boolean } = {}): DefaultPetName {
  const [settings, setSettings] = useSettings();
  const hubName = hubPetName(state);
  const supported = state.protocolVersion === undefined || state.protocolVersion >= SETTINGS_PROTOCOL_VERSION;

  useEffect(() => {
    if (!connected || !supported) return;
    client.getSettings().catch((err: unknown) => console.warn("[desktop] get_settings failed", err));
  }, [client, connected, supported]);

  useEffect(() => {
    if (hubName && hubName !== settings.lastPetName) setSettings({ lastPetName: hubName });
  }, [hubName, settings.lastPetName, setSettings]);

  // The one-time move: tried once per connection; a refused name (taken, invalid) ends it and the hub's name stays.
  const tried = useRef(false);
  useEffect(() => {
    if (!connected) tried.current = false;
  }, [connected]);
  const migrate = !!opts.migrate;
  useEffect(() => {
    if (!migrate || !connected || hubName === null || settings.petNameMigrated || tried.current) return;
    const done = () => setSettings({ petNameMigrated: true, legacyPetName: "" });
    const to = legacyNameToSend({ legacy: settings.legacyPetName, hubName, pets: Object.values(state.pets) });
    if (!to) {
      done();
      return;
    }
    tried.current = true;
    client.updateSettings({ bot: { petName: to } }).then(done, (err: unknown) => {
      if (parseSettingsError(err)) done();
      else console.warn("[desktop] could not move the pet's name to the hub; trying again on the next connection", err);
    });
  }, [migrate, connected, hubName, settings.petNameMigrated, settings.legacyPetName, state.pets, client, setSettings]);

  const legacy = settings.petNameMigrated ? "" : settings.legacyPetName;
  return {
    name: shownPetName({ hubName, cached: settings.lastPetName, legacy }),
    canRename: connected && hubName !== null,
  };
}
