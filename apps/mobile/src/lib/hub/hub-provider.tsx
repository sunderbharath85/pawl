// HubProvider: paired hubs (SecureStore), the active hub's HubClient running over a RelayLink, and app settings.
import { HubClient, initialHubState, type HubState } from "@krypto/hub-client";
import Constants from "expo-constants";
import * as Device from "expo-device";
import { createContext, use, useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { AppState } from "react-native";
import {
  addPairedHub,
  loadDeviceSecretKey,
  loadHubRecords,
  loadSettings,
  removeHub,
  saveSettings,
  type AppSettings,
  type HubRecord,
} from "@/lib/hubs/storage";
import { pairWithHub } from "@/lib/relay/pairing";
import type { PairingOffer } from "@/lib/relay/pairing-link";
import { installNaclPrng } from "@/lib/relay/prng";
import { RelayLink, type RelayLinkStatus } from "@/lib/relay/relay-link";

installNaclPrng();

export const CLIENT_VERSION = Constants.expoConfig?.version ?? "0.0.0";

export interface HubContextValue {
  /** Records + settings loaded from secure storage. */
  ready: boolean;
  records: HubRecord[];
  active: HubRecord | null;
  client: HubClient | null;
  link: RelayLink | null;
  settings: AppSettings;
  /** Last problem loading the active hub's keys (e.g. secure storage wiped). */
  loadError?: string;
  pair(offer: PairingOffer, signal?: AbortSignal): Promise<HubRecord>;
  unpair(hubId: string): Promise<void>;
  setActiveHub(hubId: string): Promise<void>;
  updateSettings(patch: Partial<AppSettings>): Promise<AppSettings>;
}

const HubContext = createContext<HubContextValue | null>(null);

function defaultDeviceName(): string {
  return Device.deviceName ?? Device.modelName ?? (process.env.EXPO_OS === "ios" ? "iPhone" : "Android phone");
}

export function HubProvider({ children }: { children: ReactNode }) {
  const [ready, setReady] = useState(false);
  const [records, setRecords] = useState<HubRecord[]>([]);
  const [settings, setSettings] = useState<AppSettings>({ notificationsEnabled: true });
  const settingsRef = useRef(settings);
  const [conn, setConn] = useState<{ hubId: string; client: HubClient; link: RelayLink } | null>(null);
  const [loadError, setLoadError] = useState<string | undefined>();

  // Load persisted state once.
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const [recs, s] = await Promise.all([loadHubRecords(), loadSettings()]);
        if (cancelled) return;
        setRecords(recs);
        settingsRef.current = s;
        setSettings(s);
      } catch (err) {
        if (!cancelled) setLoadError(err instanceof Error ? err.message : String(err));
      } finally {
        if (!cancelled) setReady(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const active = useMemo(() => {
    if (records.length === 0) return null;
    return records.find((r) => r.hubId === settings.activeHubId) ?? records[records.length - 1] ?? null;
  }, [records, settings.activeHubId]);

  // (Re)build the connection when the active hub changes.
  const activeKey = active ? `${active.hubId}|${active.deviceId}|${active.relay}|${active.hubPk}|${active.token ?? ""}` : "";
  useEffect(() => {
    if (!active) {
      setConn(null);
      return;
    }
    let cancelled = false;
    let built: { hubId: string; client: HubClient; link: RelayLink } | null = null;
    void (async () => {
      const sk = await loadDeviceSecretKey(active.hubId).catch(() => null);
      if (cancelled) return;
      if (!sk) {
        setLoadError("This phone's key for the hub is missing. Unpair and pair again.");
        setConn(null);
        return;
      }
      try {
        const link = new RelayLink({
          relay: active.relay,
          hubId: active.hubId,
          hubPk: active.hubPk,
          deviceId: active.deviceId,
          deviceSk: sk,
        });
        const client = new HubClient({
          token: active.token ?? "",
          client: "mobile",
          clientVersion: CLIENT_VERSION,
          WebSocketImpl: link.SocketImpl,
          reconnect: true,
          // Relay round-trips are slower than localhost.
          requestTimeoutMs: 20_000,
          maxReconnectDelayMs: 15_000,
        });
        const token = settingsRef.current.expoPushToken;
        if (token && settingsRef.current.notificationsEnabled) link.registerPush(token);
        client.connect();
        built = { hubId: active.hubId, client, link };
        setLoadError(undefined);
        setConn(built);
      } catch (err) {
        setLoadError(err instanceof Error ? err.message : String(err));
        setConn(null);
      }
    })();
    return () => {
      cancelled = true;
      if (built) {
        built.client.close();
        built.link.dispose();
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeKey]);

  // Reconnect immediately when the app returns to the foreground (skips the backoff wait).
  useEffect(() => {
    if (!conn) return;
    const sub = AppState.addEventListener("change", (s) => {
      if (s !== "active") return;
      const status = conn.client.getState().status;
      if (status === "disconnected") {
        conn.client.close();
        conn.client.connect();
      }
    });
    return () => sub.remove();
  }, [conn]);

  const updateSettings = useCallback(async (patch: Partial<AppSettings>) => {
    const next = { ...settingsRef.current, ...patch };
    settingsRef.current = next;
    setSettings(next);
    await saveSettings(next);
    return next;
  }, []);

  const pair = useCallback(
    async (offer: PairingOffer, signal?: AbortSignal) => {
      const paired = await pairWithHub(offer, { deviceName: defaultDeviceName(), ...(signal ? { signal } : {}) });
      const recs = await addPairedHub(paired);
      setRecords(recs);
      await updateSettings({ activeHubId: paired.hubId });
      const { deviceSk: _sk, ...record } = paired;
      return record;
    },
    [updateSettings],
  );

  const unpair = useCallback(
    async (hubId: string) => {
      if (conn?.hubId === hubId) conn.link.clearPush();
      const recs = await removeHub(hubId);
      setRecords(recs);
      if (settingsRef.current.activeHubId === hubId) {
        const nextActive = recs[recs.length - 1]?.hubId;
        await updateSettings(nextActive ? { activeHubId: nextActive } : { activeHubId: undefined });
      }
    },
    [conn, updateSettings],
  );

  const setActiveHub = useCallback(async (hubId: string) => {
    await updateSettings({ activeHubId: hubId });
  }, [updateSettings]);

  const value = useMemo<HubContextValue>(
    () => ({
      ready,
      records,
      active,
      client: conn && active && conn.hubId === active.hubId ? conn.client : null,
      link: conn && active && conn.hubId === active.hubId ? conn.link : null,
      settings,
      ...(loadError ? { loadError } : {}),
      pair,
      unpair,
      setActiveHub,
      updateSettings,
    }),
    [ready, records, active, conn, settings, loadError, pair, unpair, setActiveHub, updateSettings],
  );

  return <HubContext value={value}>{children}</HubContext>;
}

export function useHub(): HubContextValue {
  const ctx = use(HubContext);
  if (!ctx) throw new Error("useHub must be used inside <HubProvider>");
  return ctx;
}

const DISCONNECTED: HubState = initialHubState();
const noopSubscribe = () => () => {};
const disconnectedSnapshot = () => DISCONNECTED;

/** Live hub state of the active hub (a stable "disconnected" snapshot when nothing is paired). */
export function useHubState(): HubState {
  const { client } = useHub();
  const subscribe = useCallback((onChange: () => void) => (client ? client.subscribe(() => onChange()) : () => {}), [client]);
  const getSnapshot = useCallback(() => (client ? client.getState() : DISCONNECTED), [client]);
  return useSyncExternalStore(client ? subscribe : noopSubscribe, client ? getSnapshot : disconnectedSnapshot);
}

const NO_LINK: RelayLinkStatus = { relayConnected: false };

export function useRelayStatus(): RelayLinkStatus {
  const { link } = useHub();
  const subscribe = useCallback((onChange: () => void) => (link ? link.subscribe(onChange) : () => {}), [link]);
  const getSnapshot = useCallback(() => (link ? link.getStatus() : NO_LINK), [link]);
  return useSyncExternalStore(subscribe, getSnapshot);
}

/** The active client or a thrown error with a friendly message (for actions). */
export function requireClient(client: HubClient | null): HubClient {
  if (!client) throw new Error("Not connected to a hub. Pair your phone in Settings.");
  return client;
}
