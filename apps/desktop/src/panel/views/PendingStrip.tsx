import { permissionSummary } from "../../lib/format";
import { usePanel } from "../context";
import { PermissionCard } from "./PermissionCard";

/** The oldest pending approval pinned above every view (except Approvals itself). */
export function PendingStrip() {
  const { state, navigate } = usePanel();
  const pending = Object.values(state.pending).sort((a, b) => a.createdAt - b.createdAt);
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
