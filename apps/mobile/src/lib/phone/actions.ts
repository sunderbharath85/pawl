// Screen-level actions: every approve / start goes through the biometric gate first; results become short,
// friendly text. Deny, prompt and cancel need no confirmation (step-4 rules).
import { describePermission } from "@pawl/pet-engine";
import type { PermissionRequest } from "@pawl/protocol";
import { useCallback, useEffect, useState } from "react";
import { tapHaptic } from "@/components/ui";
import { phoneIntentErrorText, type GateAvailability, type IntentApplied, type PhoneSnapshot } from "@/lib/peer";
import { usePhone } from "./phone-provider";

export type ActionResult = { ok: true; applied: IntentApplied } | { ok: false; message: string; cancelled?: boolean };

export function machineName(snapshot: PhoneSnapshot, machineId: string): string {
  return snapshot.machines[machineId]?.name ?? "that computer";
}

/** Gate availability, refreshed when the app asks (e.g. after the user changed the screen lock). */
export function useGateAvailability(): { availability: GateAvailability | null; refresh(): void } {
  const { gate } = usePhone();
  const [availability, setAvailability] = useState<GateAvailability | null>(null);
  const refresh = useCallback(() => {
    void gate.availability().then(setAvailability);
  }, [gate]);
  useEffect(refresh, [refresh]);
  return { availability, refresh };
}

export function useActions() {
  const { node, gate, snapshot } = usePhone();

  const fail = (err: unknown, machineId: string, action: Parameters<typeof phoneIntentErrorText>[2]): ActionResult => {
    tapHaptic("error");
    return { ok: false, message: phoneIntentErrorText(err, machineName(node.getSnapshot(), machineId), action) };
  };

  const approve = useCallback(
    async (request: PermissionRequest): Promise<ActionResult> => {
      const name = machineName(snapshot, request.machineId);
      const check = await gate.confirm(`Approve on ${name}: ${describePermission(request)}`);
      // A refused or busy check sends nothing; "cancelled" and "busy" are not errors worth a banner.
      if (!check.confirmed) return { ok: false, message: check.message, cancelled: check.code === "cancelled" || check.code === "busy" };
      try {
        const applied = await node.approve(request.requestId, { confirmed: true });
        tapHaptic("success");
        return { ok: true, applied };
      } catch (e) {
        return fail(e, request.machineId, "approve");
      }
    },
    [gate, node, snapshot],
  );

  const deny = useCallback(
    async (request: PermissionRequest): Promise<ActionResult> => {
      try {
        const applied = await node.deny(request.requestId);
        tapHaptic("warning");
        return { ok: true, applied };
      } catch (e) {
        return fail(e, request.machineId, "deny");
      }
    },
    [node],
  );

  const prompt = useCallback(
    async (sessionId: string, machineId: string, text: string): Promise<ActionResult> => {
      try {
        return { ok: true, applied: await node.prompt(sessionId, text) };
      } catch (e) {
        return fail(e, machineId, "prompt");
      }
    },
    [node],
  );

  const cancel = useCallback(
    async (sessionId: string, machineId: string): Promise<ActionResult> => {
      try {
        return { ok: true, applied: await node.cancel(sessionId) };
      } catch (e) {
        return fail(e, machineId, "cancel");
      }
    },
    [node],
  );

  const start = useCallback(
    async (args: { machineId: string; agentId: string; agentName: string; cwd: string; repoName: string; prompt?: string }): Promise<ActionResult> => {
      const name = machineName(snapshot, args.machineId);
      const check = await gate.confirm(`Start ${args.agentName} on ${name} in ${args.repoName}`);
      // A refused or busy check sends nothing; "cancelled" and "busy" are not errors worth a banner.
      if (!check.confirmed) return { ok: false, message: check.message, cancelled: check.code === "cancelled" || check.code === "busy" };
      try {
        const applied = await node.startSession({ ...args, confirmed: true });
        tapHaptic("success");
        return { ok: true, applied };
      } catch (e) {
        return fail(e, args.machineId, "start");
      }
    },
    [gate, node, snapshot],
  );

  return { approve, deny, prompt, cancel, start };
}

/** "Approved from Windows PC" / "Denied on Mac mini" / "Approved from this phone" for a decision. */
export function decisionText(
  snapshot: PhoneSnapshot,
  r: { verdict: "approve" | "deny" | "cancel" | "answer"; by: string; byMachineId?: string; machineId: string },
  deviceName: (id: string | undefined) => string | null,
): string {
  const verb = r.verdict === "approve" ? "Approved" : r.verdict === "deny" ? "Denied" : r.verdict === "cancel" ? "Cancelled" : "Answered";
  if (r.by === "rule") return `${verb} by a rule on ${machineName(snapshot, r.machineId)}`;
  if (r.by === "system") return r.verdict === "cancel" ? "Withdrawn by the agent" : `${verb} automatically`;
  if (r.byMachineId) return `${verb} from ${deviceName(r.byMachineId) ?? "another device"}`;
  return `${verb} on ${machineName(snapshot, r.machineId)}`;
}
