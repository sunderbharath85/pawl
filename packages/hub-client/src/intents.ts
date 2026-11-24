// Acting on another machine's session (PLAN.md migration step 4). The owner hub decides; this hub only
// forwards an intent. Remote approvals and remote starts need a confirmation on this device first.
import type { IntentKind, MachineId, PermissionRequest } from "@pawl/protocol";
import { isLocal, machineDisplayName } from "./machine";
import type { HubState } from "./state";

/** Error codes a forwarded request can fail with (owner `intent_ack` reasons plus the origin's own). */
export const INTENT_ERROR_CODES = [
  "already_decided",
  "not_found",
  "not_owner",
  "forbidden",
  "unconfirmed",
  "invalid",
  "rate_limited",
  "offline",
  "timeout",
] as const;
export type IntentErrorCode = (typeof INTENT_ERROR_CODES)[number];

/** The option id is one of the request's allow_* options. */
export function isAllowOption(request: Pick<PermissionRequest, "options">, optionId: string | null): boolean {
  if (optionId === null) return false;
  return (request.options ?? []).some((o) => o.optionId === optionId && (o.kind === "allow_once" || o.kind === "allow_always"));
}

/**
 * Why `optionId` can't be chosen from here, or null: always-allow on another machine's request must be chosen
 * on that machine itself ("Choose always-allow on Mac mini"). Remotely only allow_once and reject_* go.
 */
export function remoteOptionBlocked(
  state: Pick<HubState, "machine" | "machines">,
  request: Pick<PermissionRequest, "machineId" | "options">,
  optionId: string | null,
): string | null {
  if (optionId === null || isLocal(state, request.machineId)) return null;
  const option = (request.options ?? []).find((o) => o.optionId === optionId);
  if (option?.kind !== "allow_always") return null;
  return `Choose always-allow on ${ownerName(state, request.machineId)}`;
}

/**
 * The option "Approve" picks: allow_once, or for this machine's own request allow_always when that is the only
 * allow option. Another machine's request without allow_once has nothing to approve with from here (null).
 */
export function approveOptionId(state: Pick<HubState, "machine">, request: Pick<PermissionRequest, "machineId" | "options">): string | null {
  const opts = request.options ?? [];
  const once = opts.find((o) => o.kind === "allow_once")?.optionId;
  if (once !== undefined) return once;
  if (!isLocal(state, request.machineId)) return null;
  return opts.find((o) => o.kind === "allow_always")?.optionId ?? null;
}

/**
 * True when answering `request` with `optionId` must be confirmed on this device first: an allow_* option
 * of another machine's request. Deny (a reject_* option or null) never needs it, nor does a local request.
 */
export function needsConfirmation(
  state: Pick<HubState, "machine">,
  request: Pick<PermissionRequest, "machineId" | "options">,
  optionId: string | null,
): boolean {
  return !isLocal(state, request.machineId) && isAllowOption(request, optionId);
}

/** Starting a session on another machine must be confirmed on this device first. */
export function startNeedsConfirmation(state: Pick<HubState, "machine">, machineId: MachineId | undefined): boolean {
  return machineId !== undefined && machineId !== "" && machineId !== state.machine?.id;
}

/** Why acting on `machineId`'s items cannot work right now ("Mac mini is offline"), or null. */
export function offlineReason(state: Pick<HubState, "machine" | "machines">, machineId: MachineId): string | null {
  if (isLocal(state, machineId)) return null;
  const m = state.machines[machineId];
  return m?.online ? null : `${m?.name?.trim() || "That machine"} is offline`;
}

/** Display name of an item's machine for messages: its name, or "that machine" when unknown. */
export function ownerName(state: Pick<HubState, "machines">, machineId: MachineId): string {
  return state.machines[machineId] ? machineDisplayName(state.machines[machineId]) : "that machine";
}

/** What was asked, for wording errors. `resolve` = a permission answer the caller did not split into approve/deny. */
export type IntentAction = IntentKind | "resolve";

const ACTION_WORD: Record<IntentAction, string> = {
  approve: "approve that",
  deny: "deny that",
  resolve: "answer that",
  prompt: "send that",
  start: "start that session",
  cancel: "cancel that",
};

function codeOf(err: unknown): string {
  return err && typeof err === "object" && "code" in err ? String((err as { code: unknown }).code) : "";
}

/** True for a request someone already answered (here, on its own machine, or from another machine). */
export function isAlreadyDecided(err: unknown): boolean {
  return codeOf(err) === "already_decided";
}

/**
 * Friendly text for a failed request about another machine's item (the hub forwards it as an intent).
 * `machineName` is the owner's display name. Unknown codes fall back to the error's message.
 */
export function intentErrorText(err: unknown, machineName: string, action: IntentAction): string {
  const code = codeOf(err);
  switch (code) {
    case "already_decided":
      return "Already answered.";
    case "offline":
      return `${machineName} is offline.`;
    case "timeout":
      return `No answer from ${machineName} yet — check that machine.`;
    case "unconfirmed":
      return `${machineName} needs you to confirm on this device first.`;
    case "forbidden": {
      // The owner refuses by policy: always-allow chosen remotely, a start outside its known folders, or no
      // remote actions at all.
      const off = `it does not accept actions from other machines (accept_intents is off in its config.toml)`;
      if (action === "start") return `${machineName} only lets other machines start agents in folders it already uses, or ${off}.`;
      if (action === "approve" || action === "resolve") return `Always-allow must be chosen on ${machineName} itself, or ${off}.`;
      return `${machineName} refused: ${off}.`;
    }
    case "rate_limited":
      return `Too many requests to ${machineName} at once. Wait a moment and try again.`;
    case "invalid":
      return action === "start"
        ? `${machineName} can't start that: the agent must be installed there and the folder must be one of its repos.`
        : action === "prompt"
          ? `${machineName} refused that message (too long, or the session can't take prompts).`
          : `${machineName} refused that: the option isn't valid for this request.`;
    case "not_found":
      return action === "start"
        ? `${machineName} is not paired with this machine.`
        : `${machineName} no longer has that ${action === "prompt" || action === "cancel" ? "session" : "request"}.`;
    case "not_owner":
      return `That belongs to another machine, not ${machineName}.`;
    case "unauthorized":
      return "The hub did not accept our token.";
    case "closed":
    case "disconnected":
      return `Pawl isn't connected to the hub, so it couldn't ${ACTION_WORD[action]}.`;
    default:
      if (code) return `${machineName} couldn't ${ACTION_WORD[action]} (${code}).`;
      return err instanceof Error && err.message ? err.message : `Couldn't ${ACTION_WORD[action]}.`;
  }
}

/** Distinct working folders of `machineId`'s sessions, most recent first (suggestions for a new session). */
export function recentCwds(state: Pick<HubState, "sessions">, machineId: MachineId, limit = 8): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  const sessions = Object.values(state.sessions)
    .filter((s) => s.machineId === machineId && s.cwd)
    .sort((a, b) => b.lastEventAt - a.lastEventAt);
  for (const s of sessions) {
    const key = s.cwd.replace(/[\\/]+$/, "").toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(s.cwd);
    if (out.length >= limit) break;
  }
  return out;
}
