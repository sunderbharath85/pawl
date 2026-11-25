// Chat proposals (PLAN.md migration step 5): commands the bot understood for another machine's agent. The bot
// never runs them; the user picks one, confirms on this device ("Run on Mac mini?") and the client sends the
// ordinary start_session / prompt. Single use per message, and only for PROPOSAL_TTL_MS.
import { useState, useSyncExternalStore } from "react";
import {
  ProposalLedger,
  intentErrorText,
  offlineReason,
  ownerName,
  proposalConfirmText,
  proposalDoneText,
  proposalExpired,
  proposalRetryable,
} from "@pawl/hub-client";
import type { HubClient, HubState } from "@pawl/hub-client";
import type { ChatMessage, ChatProposal } from "@pawl/protocol";
import { usePanel } from "../context";
import * as I from "../icons";
import { browserStorage, PROPOSALS_KEY, storageProposalStore } from "../../lib/proposalStore";

/**
 * Which messages' proposals were used. Kept in localStorage, shared by the panel and the pet bubble and kept across
 * reloads, so a proposal runs once whichever window confirms it.
 */
export const proposalLedger = new ProposalLedger(storageProposalStore(browserStorage()));
if (typeof window !== "undefined") {
  window.addEventListener("storage", (e) => {
    if (e.key === PROPOSALS_KEY || e.key === null) proposalLedger.reload();
  });
}

/** What the proposal buttons need (the panel passes its context; the pet bubble its own). */
export interface ProposalEnv {
  client: HubClient;
  state: HubState;
  now: number;
  connected: boolean;
  /** Shows an "open session →" link after a run (the panel); left out in the pet bubble. */
  onOpenSession?: ((sessionId: string) => void) | undefined;
}

export function ChatProposals({ message }: { message: ChatMessage }) {
  const { client, state, now, meta, navigate } = usePanel();
  const env: ProposalEnv = { client, state, now, connected: meta.phase === "connected", onOpenSession: (sessionId) => navigate({ view: "session", sessionId }) };
  return <ChatProposalsView message={message} env={env} />;
}

export function ChatProposalsView({ message, env }: { message: ChatMessage; env: ProposalEnv }) {
  const { client, state, now, onOpenSession } = env;
  useSyncExternalStore(proposalLedger.subscribe, proposalLedger.snapshot, proposalLedger.snapshot);
  const [confirming, setConfirming] = useState<string | null>(null);
  const proposals = message.proposals ?? [];
  if (proposals.length === 0) return null;

  const entry = proposalLedger.get(message.id);
  const expired = proposalExpired(message, now);
  const connected = env.connected;
  const open = proposalLedger.available(message.id) && !expired && connected;

  const run = async (p: ChatProposal) => {
    if (!proposalLedger.begin(message.id, p.id)) return;
    setConfirming(null);
    try {
      const res = await client.executeProposal(p, { confirmed: true });
      proposalLedger.done(message.id, p.id, proposalDoneText(state, p), res.kind === "started" ? res.session.id : res.sessionId);
    } catch (err) {
      const text = intentErrorText(err, ownerName(state, p.machineId), p.kind === "start" ? "start" : "prompt");
      proposalLedger.failed(message.id, p.id, text, proposalRetryable(err));
    }
  };

  return (
    <div className="proposals" role="group" aria-label={proposals.length > 1 ? "Which one did you mean?" : "Proposed command"}>
      {proposals.length > 1 && open && <div className="proposals-ask">Pick one:</div>}
      {proposals.map((p) => {
        const offline = offlineReason(state, p.machineId);
        const used = entry && entry.proposalId === p.id;
        if (confirming === p.id && open) {
          const c = proposalConfirmText(state, p);
          return (
            <div key={p.id} className="proposal-confirm" role="group" aria-label={c.title}>
              <span className="proposal-confirm-text">
                <strong className="proposal-confirm-machine">{c.title}</strong> {c.detail}
              </span>
              <span className="proposal-confirm-actions">
                <button className="pill pill-primary" autoFocus disabled={!!offline} onClick={() => void run(p)}>
                  {c.button}
                </button>
                <button className="pill pill-ghost" onClick={() => setConfirming(null)}>
                  Back
                </button>
              </span>
            </div>
          );
        }
        return (
          <button
            key={p.id}
            className={`proposal${used ? " is-used" : ""}`}
            disabled={!open || !!offline || confirming !== null}
            title={offline ?? (expired ? "This suggestion expired; ask again." : `Runs on ${ownerName(state, p.machineId)} after you confirm`)}
            onClick={() => setConfirming(p.id)}
          >
            <I.Monitor size={14} />
            <span>{p.summary}</span>
          </button>
        );
      })}
      {entry?.status === "running" && <span className="hint">Sending to {ownerName(state, proposals.find((p) => p.id === entry.proposalId)?.machineId ?? "")}…</span>}
      {entry?.status === "done" && (
        <span className="proposal-result is-ok">
          {entry.text}{" "}
          {entry.sessionId && onOpenSession && (
            <button className="link" onClick={() => onOpenSession(entry.sessionId!)}>
              open session →
            </button>
          )}
        </span>
      )}
      {entry?.status === "failed" && (
        <span className="proposal-result is-problem" role="alert">
          {entry.text}
          {entry.retryable && !expired ? " You can try again." : ""}
        </span>
      )}
      {expired && !entry && <span className="hint">This suggestion expired. Ask again to get a fresh one.</span>}
    </div>
  );
}
