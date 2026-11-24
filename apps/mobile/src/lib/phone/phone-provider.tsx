// PhoneProvider: opens the PhoneNode with the Expo adapters (secure store, native CSPRNG), keeps it in step with the
// app lifecycle, registers the push token after pairing, and exposes the snapshot + the biometric gate to screens.
import { useRouter } from "expo-router";
import * as Notifications from "expo-notifications";
import { createContext, use, useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore, type ReactNode } from "react";
import { AppState } from "react-native";
import { defaultDeviceName, expoAuthenticator, expoRandomBytes, installNaclPrng, phoneOs, secureStore } from "@/lib/expo/adapters";
import { disablePush, easProjectId, installNotificationHandler, setupPush, type PushSetup } from "@/lib/expo/push";
import { ApprovalGate, PhoneNode, type PhoneSnapshot } from "@/lib/peer";

installNaclPrng();
installNotificationHandler();

export interface PhoneContextValue {
  node: PhoneNode;
  snapshot: PhoneSnapshot;
  gate: ApprovalGate;
  /** Last push setup outcome (null = not tried yet). */
  push: PushSetup | null;
  setPushEnabled(on: boolean): Promise<void>;
}

const PhoneContext = createContext<PhoneContextValue | null>(null);

function useNodeSnapshot(node: PhoneNode): PhoneSnapshot {
  const subscribe = useCallback((fn: () => void) => node.subscribe(fn), [node]);
  return useSyncExternalStore(subscribe, () => node.getSnapshot(), () => node.getSnapshot());
}

function Ready({ node, children }: { node: PhoneNode; children: ReactNode }) {
  const snapshot = useNodeSnapshot(node);
  const gate = useMemo(() => new ApprovalGate(expoAuthenticator, phoneOs), []);
  const [push, setPush] = useState<PushSetup | null>(null);
  const trying = useRef(false);
  const router = useRouter();

  // Lifecycle: reconnect + re-hello on foreground, persist on background.
  useEffect(() => {
    const sub = AppState.addEventListener("change", (state) => {
      if (state === "active") node.onForeground();
      else if (state === "background") void node.onBackground();
    });
    return () => sub.remove();
  }, [node]);

  // Push token -> relay, once paired and while enabled.
  const paired = snapshot.paired;
  const enabled = snapshot.pushEnabled;
  const hasToken = snapshot.pushToken !== null;
  useEffect(() => {
    // Push needs the user's EAS project id in the app config; without it the switch stays off (Settings explains).
    if (!paired || !enabled || trying.current || !easProjectId()) return;
    trying.current = true;
    void setupPush()
      .then((res) => {
        setPush(res);
        if (res.ok && res.token !== node.getSnapshot().pushToken) node.setPushToken(res.token);
      })
      .finally(() => {
        trying.current = false;
      });
  }, [paired, enabled, hasToken, node]);

  // Tapping a wake opens Approvals.
  const lastResponse = Notifications.useLastNotificationResponse();
  useEffect(() => {
    if (lastResponse && lastResponse.actionIdentifier === Notifications.DEFAULT_ACTION_IDENTIFIER) {
      router.navigate("/approvals");
    }
  }, [lastResponse, router]);

  const setPushEnabled = useCallback(
    async (on: boolean) => {
      // Off: the node sends `unregister_push` to the relay (now, or on its next session); Expo forgets the device too.
      node.setPushEnabled(on);
      if (!on) {
        await disablePush();
        setPush(null);
      }
    },
    [node],
  );

  const value = useMemo<PhoneContextValue>(() => ({ node, snapshot, gate, push, setPushEnabled }), [node, snapshot, gate, push, setPushEnabled]);
  return <PhoneContext value={value}>{children}</PhoneContext>;
}

export function PhoneProvider({ children, fallback }: { children: ReactNode; fallback: (error: string | null) => ReactNode }) {
  const [node, setNode] = useState<PhoneNode | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    let opened: PhoneNode | null = null;
    PhoneNode.open({ store: secureStore, random: expoRandomBytes, name: defaultDeviceName(), os: phoneOs })
      .then((n) => {
        opened = n;
        if (cancelled) return;
        n.start();
        setNode(n);
      })
      .catch(() => setError("Pawl couldn't open its secure storage on this phone."));
    return () => {
      cancelled = true;
      void opened?.stop();
    };
  }, []);

  if (!node) return <>{fallback(error)}</>;
  return <Ready node={node}>{children}</Ready>;
}

export function usePhone(): PhoneContextValue {
  const v = use(PhoneContext);
  if (!v) throw new Error("usePhone outside PhoneProvider");
  return v;
}
