// Answering a permission request from the panel, with the rules every surface keeps: another machine's request needs
// a confirm step on this device before an allow option is sent, always-allow on another machine's request is chosen
// on that machine only, nothing is sent while its machine is offline, and the request stays shown until the hub
// says it was resolved. Used by the Approvals cards and the session transcript's confirmation.
import { useState } from "react";
import type { PermissionOption, PermissionRequest } from "@pawl/protocol";
import {
  intentErrorText,
  isAllowOption,
  isAlreadyDecided,
  isLocal,
  machineDisplayName,
  needsConfirmation,
  offlineReason,
  remoteOptionBlocked,
} from "@pawl/hub-client";
import { errorText, permissionSummary } from "../lib/format";
import { usePanel } from "./context";

const OPTION_ORDER: Record<PermissionOption["kind"], number> = {
  allow_once: 0,
  allow_always: 1,
  reject_once: 2,
  reject_always: 3,
};

export interface PermissionAnswer {
  /** The request's options, allow first. */
  options: PermissionOption[];
  /** It offers no reject option: a plain Deny (cancel) is offered instead. */
  needsPlainDeny: boolean;
  remote: boolean;
  machineName: string;
  /** Why nothing can be sent now (its machine is offline), else null. */
  offline: string | null;
  /** The option being sent ("cancel" for a plain Deny), else null. */
  busy: string | null;
  /** The allow option waiting for "Approve on Mac mini" (another machine's request). */
  confirm: { optionId: string; label: string } | null;
  /** Why the last answer did not go through (shown inline). */
  problem: string | null;
  answered: boolean;
  /** Why this option cannot be chosen here (always-allow on another machine's request), else null. */
  blockedReason(optionId: string | null): string | null;
  /** Whether choosing this option asks for the confirm step first. */
  asksConfirm(optionId: string | null): boolean;
  resolve(optionId: string | null, label: string, confirmed?: boolean): Promise<void>;
  cancelConfirm(): void;
}

export function usePermissionAnswer(req: PermissionRequest): PermissionAnswer {
  const { client, state, toast } = usePanel();
  const [busy, setBusy] = useState<string | null>(null);
  const [confirm, setConfirm] = useState<{ optionId: string; label: string } | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const [answered, setAnswered] = useState(false);
  const options = [...req.options].sort((a, b) => OPTION_ORDER[a.kind] - OPTION_ORDER[b.kind]);
  // Another machine's request: answering is forwarded to its owner (migration step 4).
  const remote = !isLocal(state, req.machineId);
  const machineName = machineDisplayName(state.machines[req.machineId]);
  const offline = offlineReason(state, req.machineId);

  const resolve = async (optionId: string | null, label: string, confirmed = false) => {
    // Always-allow on another machine's request is chosen on that machine only.
    if (offline || answered || remoteOptionBlocked(state, req, optionId)) return;
    // Approving another machine's request needs a confirm step on this device first.
    if (!confirmed && needsConfirmation(state, req, optionId) && optionId) {
      setConfirm({ optionId, label });
      setProblem(null);
      return;
    }
    setBusy(optionId ?? "cancel");
    setProblem(null);
    try {
      await client.resolvePermission(req.requestId, optionId, confirmed ? { confirmed: true } : {});
      toast(remote ? `${label} on ${machineName}: ${permissionSummary(req)}` : `${label}: ${permissionSummary(req)}`);
      // Keep showing the request until the mirror's permission_resolved removes it.
      setAnswered(true);
    } catch (err) {
      if (isAlreadyDecided(err)) {
        setAnswered(true);
        setProblem("Already answered.");
      } else {
        const text = remote ? intentErrorText(err, machineName, isAllowOption(req, optionId) ? "approve" : "deny") : errorText(err);
        if (remote) setProblem(text);
        else toast(text, "error");
      }
      setBusy(null);
    } finally {
      setConfirm(null);
    }
  };

  return {
    options,
    needsPlainDeny: !options.some((o) => o.kind.startsWith("reject")),
    remote,
    machineName,
    offline,
    busy,
    confirm,
    problem,
    answered,
    blockedReason: (optionId) => remoteOptionBlocked(state, req, optionId) || null,
    asksConfirm: (optionId) => needsConfirmation(state, req, optionId),
    resolve,
    cancelConfirm: () => setConfirm(null),
  };
}
