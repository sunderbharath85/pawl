import * as vscode from "vscode";
import {
  approveOptionId,
  intentErrorText,
  isAllowOption,
  isAlreadyDecided,
  isLocal,
  needsConfirmation,
  ownerName,
  pickPermissionOption,
  remoteOptionBlocked,
  type IntentAction,
} from "@krypto/hub-client";
import type { PermissionRequest } from "@krypto/protocol";
import type { HubConnection } from "./connection";
import { remoteApprovalText, type ConfirmText } from "./remoteText";

/** A modal warning with one go-ahead button. True only if the user picked it. */
export async function confirmModal(text: ConfirmText): Promise<boolean> {
  const pick = await vscode.window.showWarningMessage(text.message, { modal: true, detail: text.detail }, text.button);
  return pick === text.button;
}

/** Wrap a failed request about another machine's item in the owner-naming text (local errors pass through). */
export function remoteError(hub: HubConnection, machineId: string, action: IntentAction, err: unknown): unknown {
  const state = hub.state;
  if (isLocal(state, machineId)) return err;
  return new Error(intentErrorText(err, ownerName(state, machineId), action));
}

export type Decision = { kind: "approve" } | { kind: "deny" } | { kind: "option"; optionId: string | null };

/**
 * Answer a permission request from anywhere in VS Code (webview card, command, notification). Another
 * machine's approval first asks in a modal ("Approve on Mac mini: Run npm test?"), then sends
 * `confirmed: true`; deny needs no confirmation. "Already answered" is an info message, not an error.
 * Resolves false when the user backed out or someone else answered first.
 */
export async function answerRequest(hub: HubConnection, requestId: string, decision: Decision): Promise<boolean> {
  const api = hub.api;
  const state = hub.state;
  const req: PermissionRequest | undefined = state.pending[requestId];
  if (!req) {
    void vscode.window.showInformationMessage("Krypto: that request was already answered.");
    return false;
  }
  // Approve: allow_once (this machine's request may fall back to always-allow; another machine's may not).
  const optionId =
    decision.kind === "option"
      ? decision.optionId
      : decision.kind === "approve"
        ? approveOptionId(state, req)
        : pickPermissionOption(req, "reject");
  if (decision.kind === "approve" && optionId === null) {
    throw new Error(
      pickPermissionOption(req, "allow") ? `Choose always-allow on ${ownerName(state, req.machineId)}.` : "this request has no allow option",
    );
  }
  // Always-allow on another machine's request is chosen on that machine only.
  const blocked = remoteOptionBlocked(state, req, optionId);
  if (blocked) throw new Error(`${blocked}.`);
  let confirmed = false;
  if (needsConfirmation(state, req, optionId)) {
    if (!(await confirmModal(remoteApprovalText(state, req)))) return false;
    confirmed = true;
  }
  try {
    await api.resolvePermission(requestId, optionId, confirmed ? { confirmed: true } : {});
    return true;
  } catch (err) {
    if (isAlreadyDecided(err)) {
      void vscode.window.showInformationMessage("Krypto: already answered.");
      return false;
    }
    throw remoteError(hub, req.machineId, isAllowOption(req, optionId) ? "approve" : "deny", err);
  }
}
