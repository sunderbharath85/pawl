// Items for "Krypto: Show Machines" (read-only). Pure: no runtime `vscode` import, so it is unit-tested.
import { OS_LABEL, machineCounts, machineDisplayName, machinesLine, sortedMachines, type HubState } from "@krypto/hub-client";
import { presenceLabel } from "../shared/machine";

export interface MachinePickItem {
  label: string;
  description?: string;
  detail?: string;
}

export function machinePickItems(
  state: Pick<HubState, "machine" | "machines" | "devices" | "peer">,
  connected: boolean,
  now = Date.now(),
): { title: string; items: MachinePickItem[] } {
  const own = state.machine?.id;
  const devices = new Map(state.devices.map((d) => [d.id, d]));
  const items: MachinePickItem[] = sortedMachines(state).map((m) => {
    const self = m.id === own;
    const online = connected && (self || m.online);
    const presence = !connected ? "hub offline" : self ? "online" : presenceLabel(m, now);
    const addr = self
      ? state.peer?.listening
        ? `listening on ${state.peer.listening}`
        : "peer listener off"
      : devices.get(m.id)?.addr;
    return {
      label: `${online ? "$(circle-filled)" : "$(circle-outline)"} ${machineDisplayName(m)}`,
      description: [self ? "this machine" : undefined, OS_LABEL[m.os] ?? m.os].filter(Boolean).join(" · "),
      detail: [presence, addr, m.hubVersion ? `krypto ${m.hubVersion}` : undefined].filter(Boolean).join(" · "),
    };
  });
  // Roster entries without a machine (phones, from a later step) are listed too.
  for (const d of state.devices) {
    if (d.id === own || d.id in state.machines) continue;
    items.push({
      label: `$(circle-outline) ${d.name}`,
      description: d.kind === "phone" ? "phone" : "paired",
      detail: d.addr,
    });
  }
  const { total, online } = machineCounts(state);
  const title = total === 0 ? "Krypto: Machines" : `Krypto: ${connected ? machinesLine(total, online) : `${total} machine${total === 1 ? "" : "s"} · hub offline`}`;
  if (items.length === 0) items.push({ label: "$(info) The hub has not introduced this machine yet." });
  else if (items.length === 1) items.push({ label: "$(info) No other machines paired yet", detail: "Pair one from the Krypto desktop app (Machines) or with `krypto peer invite`." });
  return { title, items };
}
