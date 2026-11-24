// Machine identity helpers (PLAN.md migration step 1), shared by the extension host and
// the webview: one hub per machine, global ids, pets matched to agents by (machineId, agentId).
import { MACHINE_NAME_MAX, normalizeMachineName } from "@krypto/hub-client";
import type { Agent, Machine, MachineOs, Pet } from "@krypto/protocol";

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

export type MachineNameCheck ={ ok: true; name: string } | { ok: false; error: string };

/** Client-side mirror of the hub's rule: trimmed, 1–40 characters. */
export function checkMachineName(raw: string): MachineNameCheck {
  const name = normalizeMachineName(raw);
  if (name !== null) return { ok: true, name };
  return raw.trim().length === 0
    ? { ok: false, error: "Give this machine a name." }
    : { ok: false, error: `Keep it to ${MACHINE_NAME_MAX} characters or fewer.` };
}
