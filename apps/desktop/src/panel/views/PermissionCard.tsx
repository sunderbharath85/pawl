import { useState } from "react";
import type { PermissionOption, PermissionRequest } from "@krypto/protocol";
import {
  TOOL_KIND_ICON,
  agentName,
  errorText,
  permissionSummary,
  sessionTitle,
  shortPath,
  timeAgo,
} from "../../lib/format";
import { machineDisplayName, petFor, readOnlyReason } from "@krypto/hub-client";
import { usePanel } from "../context";
import { PetAvatar } from "../PetAvatar";

const OPTION_CLASS: Record<PermissionOption["kind"], string> = {
  allow_once: "pill pill-good",
  allow_always: "pill",
  reject_once: "pill pill-bad",
  reject_always: "pill pill-bad",
};

const OPTION_ORDER: Record<PermissionOption["kind"], number> = {
  allow_once: 0,
  allow_always: 1,
  reject_once: 2,
  reject_always: 3,
};

interface Props {
  req: PermissionRequest;
  compact?: boolean;
  highlight?: boolean;
}

export function PermissionCard({ req, compact = false, highlight = false }: Props) {
  const { client, state, navigate, toast, now } = usePanel();
  const [busy, setBusy] = useState<string | null>(null);
  const pet = petFor(Object.values(state.pets), req.machineId, req.agentId);
  const session = state.sessions[req.sessionId];
  const kind = req.toolCall.kind ?? "other";
  const options = [...req.options].sort((a, b) => OPTION_ORDER[a.kind] - OPTION_ORDER[b.kind]);
  // Another machine's request is read-only here until remote control (migration step 4).
  const readOnly = readOnlyReason(state, req.machineId);
  const machine = state.machines[req.machineId];
  const offline = !!readOnly && !machine?.online;

  const resolve = async (optionId: string | null, label: string) => {
    if (readOnly) return;
    setBusy(optionId ?? "cancel");
    try {
      await client.resolvePermission(req.requestId, optionId);
      toast(`${label}: ${permissionSummary(req)}`);
    } catch (err) {
      toast(errorText(err), "error");
      setBusy(null);
    }
  };

  const raw = req.toolCall.rawInput;
  const rawText = raw === undefined || raw === null ? "" : typeof raw === "string" ? raw : JSON.stringify(raw, null, 2);
  const locations = req.toolCall.locations ?? [];

  return (
    <article
      className={`perm-card${compact ? " is-compact" : ""}${highlight ? " is-highlight" : ""}${readOnly ? " is-remote" : ""}${offline ? " is-offline" : ""}`}
    >
      <div className="perm-head">
        <PetAvatar species={pet?.species ?? "dog"} activity="alert" size={compact ? 40 : 56} />
        <div className="perm-title">
          <div className="perm-who">
            <strong>{pet?.name ?? "Your pet"}</strong> fetched a request from{" "}
            <strong>{agentName(state.agents, req.agentId, req.machineId)}</strong>
            {readOnly && (
              <>
                {" "}
                on <strong>{machineDisplayName(machine)}</strong>
              </>
            )}
            <span className="muted"> · {timeAgo(req.createdAt, now)}</span>
            {offline && <span className="muted"> · machine offline, last seen {timeAgo(machine?.lastSeen ?? 0, now)}</span>}
          </div>
          <div className="perm-what">
            <span className={`kind-chip kind-${kind}`}>
              {TOOL_KIND_ICON[kind]} {kind.replace("_", " ")}
            </span>
            <span className="perm-summary">{permissionSummary(req)}</span>
          </div>
          {session && !compact && (
            <button className="link" onClick={() => navigate({ view: "session", sessionId: session.id })}>
              {sessionTitle(session, state.agents)} →
            </button>
          )}
        </div>
        {compact && renderButtons()}
      </div>
      {!compact && locations.length > 0 && (
        <ul className="perm-locations">
          {locations.map((l, i) => (
            <li key={i} title={l.path}>
              {shortPath(l.path, 3)}
              {l.line ? `:${l.line}` : ""}
            </li>
          ))}
        </ul>
      )}
      {!compact && rawText && (
        <pre className="perm-raw">{rawText.length > 1600 ? rawText.slice(0, 1600) + "\n…" : rawText}</pre>
      )}
      {!compact && renderButtons()}
    </article>
  );

  function renderButtons() {
    return (
      <div className="perm-actions">
        {options.map((o) => (
          <button
            key={o.optionId}
            className={OPTION_CLASS[o.kind]}
            disabled={busy !== null || !!readOnly}
            title={readOnly ?? undefined}
            onClick={() => void resolve(o.optionId, o.name)}
          >
            {busy === o.optionId ? "…" : o.name}
          </button>
        ))}
        {!options.some((o) => o.kind.startsWith("reject")) && (
          <button
            className="pill pill-bad"
            disabled={busy !== null || !!readOnly}
            title={readOnly ?? undefined}
            onClick={() => void resolve(null, "Denied")}
          >
            Deny
          </button>
        )}
        {readOnly && <span className="remote-note">{readOnly}</span>}
      </div>
    );
  }
}
