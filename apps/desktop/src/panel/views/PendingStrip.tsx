import { isLocal } from "@pawl/hub-client";
import { permissionSummary } from "../../lib/format";
import { usePanel } from "../context";
import { PermissionCard } from "./PermissionCard";

/** The oldest pending approval pinned above every view (except Approvals itself). */
export function PendingStrip() {
  const { state, navigate, route } = usePanel();
  // The session view asks its own requests in the conversation: not up here too.
  const open = route.view === "session" ? route.sessionId : null;
  // Pin the oldest request answerable here; other machines' requests come after (read-only).
  const pending = Object.values(state.pending).filter((p) => p.sessionId !== open).sort(
    (a, b) => Number(isLocal(state, b.machineId)) - Number(isLocal(state, a.machineId)) || a.createdAt - b.createdAt,
  );
  if (pending.length === 0) return null;
  const first = pending[0]!;
  return (
    <div className="pending-strip">
      <PermissionCard req={first} compact />
      {pending.length > 1 && (
        <button
          className="pending-more"
          onClick={() => navigate({ view: "approvals" })}
          title={pending.slice(1).map(permissionSummary).join("\n")}
        >
          +{pending.length - 1} more waiting →
        </button>
      )}
    </div>
  );
}
