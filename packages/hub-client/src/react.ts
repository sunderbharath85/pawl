// @pawl/hub-client/react — React binding (React >= 18).
import { useCallback, useSyncExternalStore } from "react";
import type { HubClient } from "./client";
import type { HubState } from "./state";

/** Subscribe a component to the client's state. Re-renders on every state change. */
export function useHubState(client: HubClient): HubState {
  const subscribe = useCallback((onChange: () => void) => client.subscribe(() => onChange()), [client]);
  const getSnapshot = useCallback(() => client.getState(), [client]);
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

/**
 * Subscribe to a derived slice. `selector` must return a stable value for equal inputs
 * (e.g. a field of the state) — returning a fresh object each call would loop.
 */
export function useHubSelector<T>(client: HubClient, selector: (state: HubState) => T): T {
  const subscribe = useCallback((onChange: () => void) => client.subscribe(() => onChange()), [client]);
  const getSnapshot = useCallback(() => selector(client.getState()), [client, selector]);
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
