// Machine helpers shared by every client (PLAN.md migration steps 1-3): one hub per machine, every
// id global (`${machineId}:${localId}`), pets matched to agents by (machineId, agentId), and the
// "active machine" (where the user is) that decides where pets run to and who notifies.
import {
  parseGlobalId,
  USER_ACTIVE_WINDOW_MS,
  type Agent,
  type Machine,
  type MachineId,
  type MachineOs,
  type Pet,
} from "@krypto/protocol";
import type { HubState } from "./state";

/** Max machine display name length (Unicode code points), matching the hub's validation. */
export const MACHINE_NAME_MAX = 40;

/** Trimmed machine name if it is 1–`MACHINE_NAME_MAX` code points long, else null. */
export function normalizeMachineName(name: string): string | null {
  const trimmed = name.trim();
  const length = [...trimmed].length;
  return length >= 1 && length <= MACHINE_NAME_MAX ? trimmed : null;
}

export const OS_LABEL: Record<MachineOs, string> = {
  windows: "Windows",
  macos: "macOS",
  linux: "Linux",
  other: "Other OS",
};

/** Shown before the hub has said hello (no machine known yet). */
export const UNKNOWN_MACHINE_NAME = "This machine";

export function machineDisplayName(machine: Machine | undefined): string {
  return machine?.name?.trim() || UNKNOWN_MACHINE_NAME;
}

/** One-line description for tooltips: "Windows PC · Windows · id abc123def456". */
export function machineSummary(machine: Machine | undefined): string {
  if (!machine) return "Waiting for the hub to introduce this machine";
  return `${machineDisplayName(machine)} · ${OS_LABEL[machine.os] ?? machine.os} · id ${machine.id}`;
}

/** "3 machines · 2 online" (this machine included); just "3 machines" when `online` is unknown. */
export function machinesLine(total: number, online?: number): string {
  const n = `${total} machine${total === 1 ? "" : "s"}`;
  return online === undefined ? n : `${n} · ${online} online`;
}

/** The pet that watches `agentId` on `machineId` (agent ids repeat across machines). */
export function petFor(pets: Iterable<Pet>, machineId: string, agentId: string): Pet | undefined {
  for (const p of pets) if (p.machineId === machineId && p.agentId === agentId) return p;
  return undefined;
}

/** The agent `agentId` installed on `machineId`; falls back to an id-only match when the machine is unknown. */
export function agentFor(agents: readonly Agent[], agentId: string, machineId?: string): Agent | undefined {
  if (machineId !== undefined) {
    const exact = agents.find((a) => a.id === agentId && a.machineId === machineId);
    if (exact) return exact;
  }
  return agents.find((a) => a.id === agentId);
}

const RENAME_ERRORS: Record<string, string> = {
  invalid: `Use 1–${MACHINE_NAME_MAX} characters for the machine name.`,
  no_machine: "This hub predates machine names. Update krypto and restart the hub, then try again.",
  not_owner: "Only this machine's own hub can rename it.",
  not_found: "The hub does not know this machine any more. Reconnect and try again.",
  unauthorized: "The hub did not accept our token.",
};

/** Human text for a failed `updateMachine` (HubRequestError carries the hub's error code). */
export function renameErrorText(err: unknown): string {
  const code = err && typeof err === "object" && "code" in err ? String((err as { code: unknown }).code) : "";
  if (code && RENAME_ERRORS[code]) return RENAME_ERRORS[code];
  if (err instanceof Error) return err.message;
  return String(err);
}

export type MachineNameCheck = { ok: true; name: string } | { ok: false; error: string };

/** Client-side mirror of the hub's rule: trimmed, 1–40 characters. */
export function checkMachineName(raw: string): MachineNameCheck {
  const name = normalizeMachineName(raw);
  if (name !== null) return { ok: true, name };
  return raw.trim().length === 0
    ? { ok: false, error: "Give this machine a name." }
    : { ok: false, error: `Keep it to ${MACHINE_NAME_MAX} characters or fewer.` };
}

/**
 * Where the user is: the online machine with the latest `userActiveAt` within USER_ACTIVE_WINDOW_MS
 * (this machine counts as online). If none qualifies, this machine. Ties go to this machine.
 * Returns "" before the hub has introduced this machine.
 */
export function activeMachineId(state: Pick<HubState, "machine" | "machines">, now: number): MachineId {
  const own = state.machine?.id ?? "";
  let best: Machine | undefined;
  for (const m of Object.values(state.machines)) {
    const self = m.id === own;
    if (!self && !m.online) continue;
    const at = m.userActiveAt;
    if (at === undefined || now - at > USER_ACTIVE_WINDOW_MS) continue;
    if (!best) {
      best = m;
      continue;
    }
    const bestAt = best.userActiveAt!;
    if (at > bestAt || (at === bestAt && (self || (best.id !== own && m.id < best.id)))) best = m;
  }
  return best?.id ?? own;
}

/**
 * True if `id` (a machine id, or any global id such as a session, pet, request or event id) belongs to
 * this hub's machine. Before the hub has introduced its machine everything counts as local.
 */
export function isLocal(state: Pick<HubState, "machine">, id: string): boolean {
  const own = state.machine?.id;
  if (!own) return true;
  const machineId = parseGlobalId(id)?.machineId ?? id;
  return machineId === own;
}

export interface MachineGroup<T> {
  machineId: MachineId;
  /** Undefined when the machine is not (or no longer) known. */
  machine: Machine | undefined;
  local: boolean;
  /** This machine always counts as online. */
  online: boolean;
  items: T[];
}

/**
 * Group items (sessions, pets, requests…) by their machine: this machine first, then online machines,
 * then by name; machines missing from `state.machines` last. Item order inside a group is kept.
 */
export function groupByMachine<T extends { machineId: MachineId }>(
  state: Pick<HubState, "machine" | "machines">,
  items: readonly T[],
): MachineGroup<T>[] {
  const own = state.machine?.id;
  const groups = new Map<MachineId, MachineGroup<T>>();
  for (const item of items) {
    let g = groups.get(item.machineId);
    if (!g) {
      const machine = state.machines[item.machineId] ?? (item.machineId === own ? state.machine : undefined);
      const local = !own || item.machineId === own;
      g = { machineId: item.machineId, machine, local, online: local || machine?.online === true, items: [] };
      groups.set(item.machineId, g);
    }
    g.items.push(item);
  }
  return [...groups.values()].sort(
    (a, b) =>
      Number(b.local) - Number(a.local) ||
      Number(!!b.machine) - Number(!!a.machine) ||
      Number(b.online) - Number(a.online) ||
      machineDisplayName(a.machine).localeCompare(machineDisplayName(b.machine)) ||
      (a.machineId < b.machineId ? -1 : a.machineId > b.machineId ? 1 : 0),
  );
}

/**
 * Why another machine's pet can't be edited here, or null for this machine's own pets. (Sessions and
 * approvals of other machines can be acted on since migration step 4; pets stay with their machine.)
 */
export function readOnlyReason(state: Pick<HubState, "machine" | "machines">, machineId: MachineId): string | null {
  if (isLocal(state, machineId)) return null;
  const name = state.machines[machineId]?.name?.trim() || "its own machine";
  return `Change it on ${name}: pets belong to their machine.`;
}
