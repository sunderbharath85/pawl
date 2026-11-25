import { useEffect, useMemo, useRef } from "react";
import { decisionText, isLocal, recentDecisions } from "@pawl/hub-client";
import type { PermissionRequest } from "@pawl/protocol";
import { sessionTitle, timeAgo } from "../../lib/format";
import { usePanel } from "../context";
import { PetAvatar } from "../PetAvatar";
import { PermissionCard } from "./PermissionCard";

export function ApprovalsView({ focusRequestId }: { focusRequestId?: string }) {
  const { state, navigate, now } = usePanel();
  // What was answered lately (from the events this window has, and the requests it saw waiting), and how: by you,
  // by voice, by a rule.
  const seenRequests = useRef(new Map<string, PermissionRequest>());
  for (const r of Object.values(state.pending)) seenRequests.current.set(r.requestId, r);
  if (seenRequests.current.size > 200) seenRequests.current = new Map([...seenRequests.current].slice(-100));
  const recent = useMemo(() => recentDecisions(state.events, 8, { known: seenRequests.current.values() }), [state.events, state.pending]);
  // This machine's requests first (answerable here), then other machines' (read-only), oldest first.
  const pending = Object.values(state.pending).sort(
    (a, b) => Number(isLocal(state, b.machineId)) - Number(isLocal(state, a.machineId)) || a.createdAt - b.createdAt,
  );
  const focusRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    focusRef.current?.scrollIntoView({ block: "center", behavior: "smooth" });
  }, [focusRequestId]);

  return (
    <section className="view">
      <header className="view-head">
        <h1>Approvals</h1>
        <p>Agents ask before running commands, editing files or reaching the network. Your pet brings every request here.</p>
      </header>
      {pending.length === 0 ? (
        <div className="empty">
          <PetAvatar species="dog" activity="happy" size={110} />
          <h3>All clear!</h3>
          <p>No one is waiting on you. Tail wags all round.</p>
        </div>
      ) : (
        <div className="stack">
          {pending.map((req) => (
            <div key={req.requestId} ref={req.requestId === focusRequestId ? focusRef : undefined}>
              <PermissionCard req={req} highlight={req.requestId === focusRequestId} />
            </div>
          ))}
        </div>
      )}
      {recent.length > 0 && (
        <div className="recent-decisions">
          <h2 className="section-title">Recently answered</h2>
          <ul className="mini-list">
            {recent.map((d) => {
              const session = state.sessions[d.sessionId];
              return (
                <li key={d.requestId}>
                  <button className="mini-row" onClick={() => navigate({ view: "session", sessionId: d.sessionId })}>
                    <span className={`dot tone-${d.outcome === "approved" ? "done" : "error"}`} />
                    <span className="mini-title">{d.title}</span>
                    {session && <span className="muted">{sessionTitle(session, state.agents)}</span>}
                    <span className={`chip ${d.via === "voice" ? "chip-voice" : "chip-soft"}`}>{decisionText(d.outcome, d)}</span>
                    <span className="muted">{timeAgo(d.ts, now)}</span>
                  </button>
                </li>
              );
            })}
          </ul>
        </div>
      )}
    </section>
  );
}
