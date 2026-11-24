import * as vscode from "vscode";
import {
  ProposalLedger,
  intentErrorText,
  offlineReason,
  ownerName,
  proposalConfirmText,
  proposalDoneText,
  proposalExpired,
  proposalRetryable,
} from "@krypto/hub-client";
import type { HubConnection } from "./connection";
import { confirmModal } from "./remote";

/**
 * Which chat messages' proposals were used in this extension host (PLAN.md migration step 5). Relayed to the
 * webviews with every state push, so all views show the same single-use state.
 */
export const proposalLedger = new ProposalLedger();

/**
 * Run a chat proposal the user clicked: it must still exist, not be expired or used, and its machine online.
 * A modal asks "Run on Mac mini?" with the summary; only its button sends the ordinary ClientMsg
 * (`executeProposal` with `confirmed: true`). The outcome lands in the ledger (shown inline in the chat).
 */
export async function runProposal(hub: HubConnection, messageId: string, proposalId: string): Promise<void> {
  const api = hub.api;
  const state = hub.state;
  const message = state.chat.find((m) => m.id === messageId);
  const p = message?.proposals?.find((x) => x.id === proposalId);
  if (!message || !p) throw new Error("that suggestion is no longer available");
  if (proposalExpired(message, Date.now())) throw new Error("that suggestion expired; ask again for a fresh one");
  if (!proposalLedger.available(messageId)) return;
  const offline = offlineReason(state, p.machineId);
  if (offline) throw new Error(offline);
  const c = proposalConfirmText(state, p);
  if (!(await confirmModal({ message: c.title, detail: c.detail, button: c.button }))) return;
  if (!proposalLedger.begin(messageId, proposalId)) return;
  try {
    const res = await api.executeProposal(p, { confirmed: true });
    const sessionId = res.kind === "started" ? res.session.id : res.sessionId;
    proposalLedger.done(messageId, proposalId, proposalDoneText(hub.state, p), sessionId);
  } catch (err) {
    const text = intentErrorText(err, ownerName(hub.state, p.machineId), p.kind === "start" ? "start" : "prompt");
    proposalLedger.failed(messageId, proposalId, text, proposalRetryable(err));
    void vscode.window.showWarningMessage(`Krypto: ${text}`);
  }
}
