// Chat proposals (PLAN.md migration step 5): commands the bot understood for another machine's agent. The
// bot never runs them; the user confirms on this device and the client sends the ordinary start_session /
// prompt ClientMsg (step-4 rules apply). Pure helpers plus a tiny single-use ledger.
import { MACHINE_ID_RE, parseGlobalId, type ChatMessage, type ChatProposal, type MachineOs, type RepoRef } from "@pawl/protocol";
import { isAlreadyDecided } from "./intents";
import { machineDisplayName } from "./machine";
import type { HubState } from "./state";

/** A proposal can be used for this long after its chat message (then it is greyed out). */
export const PROPOSAL_TTL_MS = 10 * 60_000;

export function proposalExpired(message: Pick<ChatMessage, "ts">, now: number): boolean {
  return now - message.ts > PROPOSAL_TTL_MS;
}

/**
 * Why a proposal can't be executed as given, or null: malformed, a start aimed at this machine (`ownMachineId`;
 * local starts are never proposals, the hub runs them itself), or a prompt aimed at another machine's session than
 * it names. The hubs validate again; this refuses nonsense before sending.
 */
export function proposalProblem(p: ChatProposal, ownMachineId?: string): string | null {
  if (!MACHINE_ID_RE.test(p.machineId ?? "")) return "it names no valid machine";
  if (!p.agentId) return "it names no agent";
  if (p.kind === "start") {
    if (ownMachineId !== undefined && p.machineId === ownMachineId) return "it targets this machine (local starts are not proposals)";
    return p.cwd?.trim() ? null : "it names no folder";
  }
  if (p.kind === "prompt") {
    if (!p.sessionId) return "it names no session";
    if (!p.text?.trim()) return "it has no message";
    const owner = parseGlobalId(p.sessionId)?.machineId;
    return owner === p.machineId ? null : "its session belongs to another machine";
  }
  return "it is not a start or a prompt";
}

/** Folder key for comparing paths: no trailing separators; case-insensitive on Windows and macOS. */
function folderKey(path: string, os: MachineOs | undefined): string {
  const p = path.trim().replace(/[\\/]+$/, "");
  return os === "windows" || os === "macos" ? p.toLowerCase() : p;
}

/** True if `cwd` is one of `repos` (that machine's repo index), compared as that machine's file system would. */
export function repoIndexHas(repos: readonly RepoRef[], cwd: string, os?: MachineOs): boolean {
  const key = folderKey(cwd, os);
  return key !== "" && repos.some((r) => folderKey(r.path, os) === key);
}

/** Confirm-step wording: machine name first, one go-ahead button. */
export function proposalConfirmText(state: Pick<HubState, "machine" | "machines">, p: ChatProposal): { title: string; detail: string; button: string } {
  const name = state.machines[p.machineId] ? machineDisplayName(state.machines[p.machineId]) : "that machine";
  return { title: `Run on ${name}?`, detail: p.summary, button: `Run on ${name}` };
}

/** Text for a proposal that went through. */
export function proposalDoneText(state: Pick<HubState, "machines">, p: ChatProposal): string {
  const name = state.machines[p.machineId] ? machineDisplayName(state.machines[p.machineId]) : "that machine";
  return p.kind === "start" ? `Started on ${name}.` : `Sent to ${name}.`;
}

/** Errors after which nothing ran on the owner, so the proposal may be tried again (else it stays used). */
const NOT_APPLIED = new Set(["offline", "rate_limited", "forbidden", "invalid", "not_found", "not_owner", "unconfirmed", "closed", "unauthorized", "no_websocket"]);

export function proposalRetryable(err: unknown): boolean {
  if (isAlreadyDecided(err)) return false;
  const code = err && typeof err === "object" && "code" in err ? String((err as { code: unknown }).code) : "";
  return NOT_APPLIED.has(code);
}

export type ProposalEntry =
  | { status: "running"; proposalId: string }
  | { status: "done"; proposalId: string; text: string; sessionId?: string | undefined }
  | { status: "failed"; proposalId: string; text: string; retryable: boolean };

/**
 * Which chat messages' proposals were used, in memory. A message's proposals are alternatives ("which one did
 * you mean?"): once one runs, the others are spent too. A failure that ran nothing frees the message again.
 */
export class ProposalLedger {
  private readonly entries = new Map<string, ProposalEntry>();
  private readonly listeners = new Set<() => void>();
  private version = 0;

  get(messageId: string): ProposalEntry | undefined {
    return this.entries.get(messageId);
  }

  /** True if a proposal of this message may run now (nothing ran or is running). */
  available(messageId: string): boolean {
    const e = this.entries.get(messageId);
    return !e || (e.status === "failed" && e.retryable);
  }

  /** Claim the message for `proposalId`; false if one of its proposals already ran or is running. */
  begin(messageId: string, proposalId: string): boolean {
    if (!this.available(messageId)) return false;
    this.set(messageId, { status: "running", proposalId });
    return true;
  }

  done(messageId: string, proposalId: string, text: string, sessionId?: string): void {
    this.set(messageId, { status: "done", proposalId, text, sessionId });
  }

  failed(messageId: string, proposalId: string, text: string, retryable: boolean): void {
    this.set(messageId, { status: "failed", proposalId, text, retryable });
  }

  /** Changes counter (for useSyncExternalStore-style subscribers). */
  snapshot = (): number => this.version;

  subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => {
      this.listeners.delete(fn);
    };
  };

  private set(messageId: string, entry: ProposalEntry): void {
    this.entries.set(messageId, entry);
    // Keep the ledger small: proposals expire after PROPOSAL_TTL_MS anyway.
    if (this.entries.size > 200) {
      const first = this.entries.keys().next().value;
      if (first !== undefined) this.entries.delete(first);
    }
    this.version++;
    for (const fn of [...this.listeners]) fn();
  }
}
