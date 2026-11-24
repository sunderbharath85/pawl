// Where the user is, for notifications (PLAN.md migration step 3): the machine the user is active
// on tells them once; every other machine stays quiet.
import { activeMachineId, intentErrorText, isLocal, machineDisplayName, ownerName, type HubState, type IntentAction } from "@pawl/hub-client";
import { parseGlobalId } from "@pawl/protocol";
import { errorText } from "./format";

/** Notify here only when the user is active on this machine (else the active machine does). */
export function notifiesHere(state: Pick<HubState, "machine" | "machines">, now = Date.now()): boolean {
  const own = state.machine?.id;
  return !own || activeMachineId(state, now) === own;
}

/**
 * Text for a failed answer to a permission request (or a prompt / cancel): the intent wording naming the
 * owner for another machine's item, the plain error for this machine's.
 */
export function answerErrorText(
  state: Pick<HubState, "machine" | "machines" | "pending" | "sessions">,
  id: string,
  action: IntentAction,
  err: unknown,
): string {
  const machineId = state.pending[id]?.machineId ?? state.sessions[id]?.machineId ?? parseGlobalId(id)?.machineId;
  if (machineId === undefined || isLocal(state, machineId)) return errorText(err);
  return intentErrorText(err, ownerName(state, machineId), action);
}

/** " on Mac mini" for another machine's item, "" for this machine's. */
export function onMachine(state: Pick<HubState, "machine" | "machines">, machineId: string): string {
  return isLocal(state, machineId) ? "" : ` on ${machineDisplayName(state.machines[machineId])}`;
}
