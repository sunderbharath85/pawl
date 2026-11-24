import { useMemo } from "react";
import { scopeToMachine, type HubClient, type HubState } from "@krypto/hub-client";
import { useHubState } from "@krypto/hub-client/react";

/**
 * Hub state limited to this machine's pets, sessions and approvals. Paired machines show up in
 * `machines` / `devices`; their pets and sessions are not shown before PLAN.md migration step 3.
 */
export function useLocalHubState(client: HubClient): HubState {
  const state = useHubState(client);
  return useMemo(() => scopeToMachine(state, state.machine?.id), [state]);
}
