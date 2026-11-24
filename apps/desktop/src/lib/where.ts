// Where the user is, for notifications (PLAN.md migration step 3): the machine the user is active
// on tells them once; every other machine stays quiet.
import { activeMachineId, isLocal, machineDisplayName, type HubState } from "@krypto/hub-client";

/** Notify here only when the user is active on this machine (else the active machine does). */
export function notifiesHere(state: Pick<HubState, "machine" | "machines">, now = Date.now()): boolean {
  const own = state.machine?.id;
  return !own || activeMachineId(state, now) === own;
}

/** " on Mac mini" for another machine's item, "" for this machine's. */
export function onMachine(state: Pick<HubState, "machine" | "machines">, machineId: string): string {
  return isLocal(state, machineId) ? "" : ` on ${machineDisplayName(state.machines[machineId])}`;
}
