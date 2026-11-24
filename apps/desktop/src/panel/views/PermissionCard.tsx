import { useState } from "react";
import type { PermissionOption, PermissionRequest } from "@pawl/protocol";
import {
  TOOL_KIND_ICON,
  agentName,
  errorText,
  permissionSummary,
  sessionTitle,
  shortPath,
  timeAgo,
} from "../../lib/format";
import {
  intentErrorText,
  isAllowOption,
  isAlreadyDecided,
  isLocal,
  machineDisplayName,
  needsConfirmation,
  offlineReason,
  petFor,
  remoteOptionBlocked,
} from "@pawl/hub-client";
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
  /** The allow option waiting for "Approve on Mac mini" (another machine's request). */
  const [confirm, setConfirm] = useState<{ optionId: string; label: string } | null>(null);
  /** Result of the last answer that did not go through (shown inline; the card stays until the mirror resolves it). */
  const [problem, setProblem] = useState<string | null>(null);
  const [answered, setAnswered] = useState(false);
  const pet = petFor(Object.values(state.pets), req.machineId, req.agentId);
  const session = state.sessions[req.sessionId];
  const kind = req.toolCall.kind ?? "other";
  const options = [...req.options].sort((a, b) => OPTION_ORDER[a.kind] - OPTION_ORDER[b.kind]);
  // Another machine's request: answering is forwarded to its owner (migration step 4).
  const remote = !isLocal(state, req.machineId);
  const machine = state.machines[req.machineId];
  const machineName = machineDisplayName(machine);
  const offline = offlineReason(state, req.machineId);

  const resolve = async (optionId: string | null, label: string, confirmed = false) => {
    // Always-allow on another machine's request is chosen on that machine only.
    if (offline || answered || remoteOptionBlocked(state, req, optionId)) return;
    // Approving another machine's request needs a confirm step on this device first.
    if (!confirmed && needsConfirmation(state, req, optionId) && optionId) {
      setConfirm({ optionId, label });
      setProblem(null);
      return;
    }
    setBusy(optionId ?? "cancel");
    setProblem(null);
    try {
      await client.resolvePermission(req.requestId, optionId, confirmed ? { confirmed: true } : {});
      toast(remote ? `${label} on ${machineName}: ${permissionSummary(req)}` : `${label}: ${permissionSummary(req)}`);
      // Keep showing the request until the mirror's permission_resolved removes it.
      setAnswered(true);
    } catch (err) {
      if (isAlreadyDecided(err)) {
        setAnswered(true);
        setProblem("Already answered.");
      } else {
        const text = remote ? intentErrorText(err, machineName, isAllowOption(req, optionId) ? "approve" : "deny") : errorText(err);
        if (remote) setProblem(text);
        else toast(text, "error");
      }
      setBusy(null);
    } finally {
      setConfirm(null);
    }
  };

  const raw = req.toolCall.rawInput;
  const rawText = raw === undefined || raw === null ? "" : typeof raw === "string" ? raw : JSON.stringify(raw, null, 2);
  const locations = req.toolCall.locations ?? [];

  return (
    <article
      className={`perm-card${compact ? " is-compact" : ""}${highlight ? " is-highlight" : ""}${remote ? " is-remote" : ""}${offline ? " is-offline" : ""}`}
    >
      <div className="perm-head">
        <PetAvatar species={pet?.species ?? "dog"} activity="alert" size={compact ? 40 : 56} />
        <div className="perm-title">
          <div className="perm-who">
            <strong>{pet?.name ?? "Your pet"}</strong> fetched a request from{" "}
            <strong>{agentName(state.agents, req.agentId, req.machineId)}</strong>
            {remote && (
              <>
                {" "}
                on <strong>{machineName}</strong>
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
    if (confirm) {
      return (
        <div className="perm-actions perm-confirm" role="group" aria-label={`Approve on ${machineName}?`}>
          <span className="perm-confirm-text">
            {confirm.label} on <strong className="perm-confirm-machine">{machineName}</strong>: {permissionSummary(req)}?
          </span>
          <button
            className="pill pill-good"
            autoFocus
            disabled={busy !== null}
            onClick={() => void resolve(confirm.optionId, confirm.label, true)}
            onKeyDown={(e) => {
              if (e.key === "Escape") setConfirm(null);
            }}
          >
            {busy === confirm.optionId ? "…" : `${confirm.label} on ${machineName}`}
          </button>
          <button className="pill pill-ghost" disabled={busy !== null} onClick={() => setConfirm(null)}>
            Back
          </button>
        </div>
      );
    }
    const blocked = busy !== null || !!offline || answered;
    return (
      <div className="perm-actions">
        {options.map((o) => {
          const choose = remoteOptionBlocked(state, req, o.optionId);
          return (
            <button
              key={o.optionId}
              className={OPTION_CLASS[o.kind]}
              disabled={blocked || !!choose}
              title={offline ?? choose ?? undefined}
              onClick={() => void resolve(o.optionId, o.name)}
            >
              {busy === o.optionId ? "…" : needsConfirmation(state, req, o.optionId) ? `${o.name}…` : o.name}
            </button>
          );
        })}
        {!options.some((o) => o.kind.startsWith("reject")) && (
          <button className="pill pill-bad" disabled={blocked} title={offline ?? undefined} onClick={() => void resolve(null, "Denied")}>
            Deny
          </button>
        )}
        {offline ? (
          <span className="remote-note">{offline}: answer it when it is back, or on {machineName}.</span>
        ) : problem ? (
          <span className="remote-note is-problem" role="alert">
            {problem}
          </span>
        ) : answered ? (
          <span className="remote-note">Sent{remote ? ` to ${machineName}` : ""}…</span>
        ) : remote ? (
          <span className="remote-note">
            Runs on {machineName}; approving asks you to confirm first.
            {options.some((o) => remoteOptionBlocked(state, req, o.optionId)) ? ` Choose always-allow on ${machineName}.` : ""}
          </span>
        ) : null}
      </div>
    );
  }
}
