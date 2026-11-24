// Items for "Pawl: Show Machines" (read-only). Pure: no runtime `vscode` import, so it is unit-tested.
import {
  OS_LABEL,
  groupDevices,
  introducedByText,
  linkPathLabel,
  machineCounts,
  machineDisplayName,
  machinesLine,
  phoneCounts,
  phoneOs,
  sortedMachines,
  type HubState,
} from "@pawl/hub-client";
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
    const via = online && !self ? linkPathLabel(devices.get(m.id)?.path) : null;
    return {
      label: `${online ? "$(circle-filled)" : "$(circle-outline)"} ${machineDisplayName(m)}`,
      description: [self ? "this machine" : undefined, OS_LABEL[m.os] ?? m.os, via ?? undefined].filter(Boolean).join(" · "),
      detail: [presence, addr, m.hubVersion ? `pawl ${m.hubVersion}` : undefined].filter(Boolean).join(" · "),
    };
  });
  const { hubs, phones, other } = groupDevices(state.devices);
  // Paired hubs not known as machines yet, and kinds from a newer hub.
  for (const d of [...hubs, ...other]) {
    if (d.id === own || d.id in state.machines) continue;
    items.push({ label: `$(circle-outline) ${d.name}`, description: "paired", detail: d.addr });
  }
  const { total, online } = machineCounts(state);
  const title = total === 0 ? "Pawl: Machines" : `Pawl: ${connected ? machinesLine(total, online) : `${total} machine${total === 1 ? "" : "s"} · hub offline`}`;
  if (items.length === 0) items.push({ label: "$(info) The hub has not introduced this machine yet." });
  else if (items.length === 1) items.push({ label: "$(info) No other machines paired yet", detail: "Pair one from the Pawl desktop app (Machines) or with `pawl peer invite`." });
  // Phones are devices, never machines: listed after them.
  for (const p of phones) {
    const linked = connected && !!p.path;
    const os = phoneOs(p);
    items.push({
      label: `${linked ? "$(circle-filled)" : "$(circle-outline)"} $(device-mobile) ${p.name}`,
      description: [os ? `${OS_LABEL[os]} phone` : "phone", linked ? linkPathLabel(p.path) : null].filter(Boolean).join(" · "),
      detail: [!connected ? "hub offline" : presenceLabel({ online: !!p.path, lastSeen: p.lastSeen ?? 0 }, now), introducedByText(p, state)]
        .filter(Boolean)
        .join(" · "),
    });
  }
  return { title, items };
}

/** The status bar tooltip's phone line ("$(device-mobile) 2 phones paired · 1 online"), or null when none is paired. */
export function phonesTooltipLine(state: Pick<HubState, "devices">, connected: boolean): string | null {
  const { total, online } = phoneCounts(state);
  if (total === 0) return null;
  return `$(device-mobile) ${total} phone${total === 1 ? "" : "s"} paired${connected ? ` · ${online} online` : ""}`;
}
