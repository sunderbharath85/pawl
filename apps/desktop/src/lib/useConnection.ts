import { useCallback, useSyncExternalStore } from "react";
import type { HubClient } from "@pawl/hub-client";
import { getConnection, type ConnectionMeta, type HubConnection } from "./connection";

export interface UseConnection {
  conn: HubConnection;
  meta: ConnectionMeta;
  client: HubClient;
}

/** The window's hub connection + its phase. Re-renders when the phase/client changes. */
export function useConnection(): UseConnection {
  const conn = getConnection();
  const subscribe = useCallback((cb: () => void) => conn.subscribeMeta(cb), [conn]);
  const getSnapshot = useCallback(() => conn.getMeta(), [conn]);
  const meta = useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
  return { conn, meta, client: conn.client };
}
