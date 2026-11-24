import { useEffect, useRef } from "react";
import { isLocal } from "@pawl/hub-client";
import { usePanel } from "../context";
import { PetAvatar } from "../PetAvatar";
import { PermissionCard } from "./PermissionCard";

export function ApprovalsView({ focusRequestId }: { focusRequestId?: string }) {
  const { state } = usePanel();
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
    </section>
  );
}
