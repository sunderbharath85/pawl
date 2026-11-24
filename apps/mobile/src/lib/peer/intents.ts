// Intents from the phone (PLAN.md step 4 rules, `client: "mobile"`). Pure helpers + the error type. The owner hub
// is the authority; the phone checks what it can before sending (confirmation, option kinds, link state).
import { intentErrorText, type IntentAction } from "@krypto/hub-client";
import type { Intent, IntentRejectReason, PermissionRequest } from "@krypto/protocol";

/** Phone-side codes on top of the owner's IntentRejectReason. */
export type PhoneIntentCode =
  | IntentRejectReason
  | "offline"
  | "timeout"
  /** The request has no allow_once option; always-allow can only be chosen on its own machine. */
  | "no_allow_once"
  /** A biometric / passcode check is required first and was not done (or failed). */
  | "auth_required"
  | "closed";

export class IntentError extends Error {
  constructor(
    readonly code: PhoneIntentCode,
    readonly machineId: string,
    readonly action: IntentAction,
  ) {
    super(code);
    this.name = "IntentError";
  }
}

export interface IntentApplied {
  intentId: string;
  /** Same shape as the matching ClientMsg ack result (start: `{session}`). */
  result?: unknown;
}

/** Only allow_once can be chosen from another device (allow_always -> `forbidden` on the owner). */
export function approveOption(request: Pick<PermissionRequest, "options">): string | null {
  return (request.options ?? []).find((o) => o.kind === "allow_once")?.optionId ?? null;
}

/** Deny picks reject_once, else no option (cancelled), like hub-client's `deny`. */
export function denyOption(request: Pick<PermissionRequest, "options">): string | null {
  return (request.options ?? []).find((o) => o.kind === "reject_once")?.optionId ?? null;
}

/** approve and start need the user's confirmation on this device (biometrics on the phone). */
export function intentNeedsConfirmation(intent: Intent): boolean {
  return intent.kind === "approve" || intent.kind === "start";
}

/** The machine an intent goes to (the owner of its target), or null for `start` (named by the caller). */
export function intentTarget(intent: Intent): string | null {
  switch (intent.kind) {
    case "approve":
    case "deny":
      return ownerOf(intent.requestId);
    case "prompt":
    case "cancel":
      return ownerOf(intent.sessionId);
    case "start":
      return null;
  }
}

function ownerOf(id: string): string | null {
  const i = id.indexOf(":");
  return i > 0 ? id.slice(0, i) : null;
}

/** Friendly text for a failed phone intent. */
export function phoneIntentErrorText(err: unknown, machineName: string, action: IntentAction): string {
  const code = err instanceof IntentError ? err.code : "";
  switch (code) {
    case "no_allow_once":
      return `This request can only be approved for always — choose that on ${machineName} itself.`;
    case "auth_required":
      return "Confirm with Face ID, fingerprint or your passcode to do that.";
    case "closed":
      return `Not connected to the relay, so Krypto couldn't reach ${machineName}.`;
    case "offline":
      return `${machineName} is offline. Krypto will reach it again when it's back.`;
    default:
      return intentErrorText(err, machineName, action);
  }
}

/** 16 random bytes as hex: a unique intent id. */
export function newIntentId(random: (n: number) => Uint8Array): string {
  let s = "";
  for (const b of random(16)) s += b.toString(16).padStart(2, "0");
  return s;
}
