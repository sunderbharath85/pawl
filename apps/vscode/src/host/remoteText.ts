// Wording for acting on another machine's items from VS Code (PLAN.md migration step 4). Pure: no runtime
// `vscode` import, so it is unit-tested.
import { agentFor, isLocal, machineDisplayName, ownerName, type HubState } from "@krypto/hub-client";
import { describePermission } from "@krypto/pet-engine";
import type { Agent, PermissionRequest } from "@krypto/protocol";

export interface ConfirmText {
  /** Modal title line, machine name first. */
  message: string;
  detail: string;
  /** The only button that goes ahead (the modal's Cancel never does). */
  button: string;
}

/** "Approve on Mac mini: Run npm test?" for another machine's request. */
export function remoteApprovalText(state: Pick<HubState, "machines" | "agents">, req: PermissionRequest): ConfirmText {
  const name = ownerName(state, req.machineId);
  const agent = agentFor(state.agents, req.agentId, req.machineId)?.name ?? req.agentId;
  const where = req.toolCall.locations?.[0]?.path;
  return {
    message: `Approve on ${name}: ${describePermission(req)}?`,
    detail: `${agent} on ${name} asked for this${where ? ` (${where})` : ""}. Approving lets it run there, on ${name}.`,
    button: `Approve on ${name}`,
  };
}

/** "Start Claude Code on Mac mini in /repo?" for a session on another machine. */
export function remoteStartText(state: Pick<HubState, "machines">, machineId: string, agent: Pick<Agent, "name">, cwd: string): ConfirmText {
  const name = ownerName(state, machineId);
  return {
    message: `Start ${agent.name} on ${name} in ${cwd}?`,
    detail: `It runs on ${name}, with access to that folder there.`,
    button: `Start on ${name}`,
  };
}

/** " · Mac mini" suffix for another machine's item in pickers, "" for this machine's. */
export function machineSuffix(state: Pick<HubState, "machine" | "machines">, machineId: string): string {
  return isLocal(state, machineId) ? "" : ` · ${machineDisplayName(state.machines[machineId])}`;
}
