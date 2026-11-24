// Persistent pairing + app settings in expo-secure-store (Keychain / Android Keystore-backed).
// Device secret keys live under their own keys so the record list can be read without touching them.
import * as SecureStore from "expo-secure-store";
import type { PairedHub } from "@/lib/relay/pairing";

/** Public part of a paired hub (everything except the device secret key). */
export type HubRecord = Omit<PairedHub, "deviceSk">;

export interface AppSettings {
  activeHubId?: string;
  /** User wants push notifications (permission is asked when turning this on). */
  notificationsEnabled: boolean;
  /** Last Expo push token registered with the relay. */
  expoPushToken?: string;
}

const HUBS_KEY = "krypto.hubs.v1";
const SETTINGS_KEY = "krypto.settings.v1";
const SK_PREFIX = "krypto.sk.v1.";

const DEFAULT_SETTINGS: AppSettings = { notificationsEnabled: true };

// Only "this device, after first unlock" — keys never sync to iCloud/other devices.
const OPTS: SecureStore.SecureStoreOptions = { keychainAccessible: SecureStore.AFTER_FIRST_UNLOCK_THIS_DEVICE_ONLY };

/** SecureStore keys allow [A-Za-z0-9._-] only. */
function skKey(hubId: string): string {
  return SK_PREFIX + hubId.replace(/[^A-Za-z0-9._-]/g, (c) => `_${c.charCodeAt(0).toString(16)}`);
}

function isRecord(v: unknown): v is HubRecord {
  if (typeof v !== "object" || v === null) return false;
  const r = v as Record<string, unknown>;
  return (
    typeof r.hubId === "string" &&
    typeof r.hubName === "string" &&
    typeof r.relay === "string" &&
    typeof r.hubPk === "string" &&
    typeof r.deviceId === "string" &&
    typeof r.devicePk === "string" &&
    typeof r.pairedAt === "number"
  );
}

export async function loadHubRecords(): Promise<HubRecord[]> {
  const raw = await SecureStore.getItemAsync(HUBS_KEY, OPTS);
  if (!raw) return [];
  try {
    const parsed: unknown = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter(isRecord) : [];
  } catch {
    return [];
  }
}

async function saveHubRecords(records: HubRecord[]): Promise<void> {
  await SecureStore.setItemAsync(HUBS_KEY, JSON.stringify(records), OPTS);
}

/** Store a newly paired hub (replaces an existing pairing with the same hub id). */
export async function addPairedHub(hub: PairedHub): Promise<HubRecord[]> {
  const { deviceSk, ...record } = hub;
  await SecureStore.setItemAsync(skKey(hub.hubId), deviceSk, OPTS);
  const records = (await loadHubRecords()).filter((r) => r.hubId !== hub.hubId);
  records.push(record);
  await saveHubRecords(records);
  return records;
}

export async function updateHubRecord(hubId: string, patch: Partial<Pick<HubRecord, "hubName" | "token">>): Promise<HubRecord[]> {
  const records = (await loadHubRecords()).map((r) => (r.hubId === hubId ? { ...r, ...patch } : r));
  await saveHubRecords(records);
  return records;
}

export async function removeHub(hubId: string): Promise<HubRecord[]> {
  await SecureStore.deleteItemAsync(skKey(hubId), OPTS);
  const records = (await loadHubRecords()).filter((r) => r.hubId !== hubId);
  await saveHubRecords(records);
  return records;
}

export async function loadDeviceSecretKey(hubId: string): Promise<string | null> {
  return SecureStore.getItemAsync(skKey(hubId), OPTS);
}

export async function loadSettings(): Promise<AppSettings> {
  const raw = await SecureStore.getItemAsync(SETTINGS_KEY, OPTS);
  if (!raw) return { ...DEFAULT_SETTINGS };
  try {
    const parsed = JSON.parse(raw) as Partial<AppSettings>;
    return {
      ...DEFAULT_SETTINGS,
      ...(typeof parsed.activeHubId === "string" ? { activeHubId: parsed.activeHubId } : {}),
      ...(typeof parsed.notificationsEnabled === "boolean" ? { notificationsEnabled: parsed.notificationsEnabled } : {}),
      ...(typeof parsed.expoPushToken === "string" ? { expoPushToken: parsed.expoPushToken } : {}),
    };
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

export async function saveSettings(settings: AppSettings): Promise<void> {
  await SecureStore.setItemAsync(SETTINGS_KEY, JSON.stringify(settings), OPTS);
}
