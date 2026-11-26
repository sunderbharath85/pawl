import type { PermissionOption, PermissionRequest } from "@pawl/protocol";
import { TOOL_KIND_ICON, agentName, permissionSummary, sessionTitle, shortPath, timeAgo } from "../../lib/format";
import { assistantAsker, assistantStartCard, isAssistantRequest, petFor, requestCommandText } from "@pawl/hub-client";
import { usePanel } from "../context";
import { PetAvatar } from "../PetAvatar";
import { useVoiceEnabled } from "../../lib/voice";
import { VoiceCodeChip } from "../../lib/VoiceCodeChip";
import { usePermissionAnswer } from "../usePermissionAnswer";
import { useDefaultPet } from "../useDefaultPet";

const OPTION_CLASS: Record<PermissionOption["kind"], string> = {
  allow_once: "pill pill-good",
  allow_always: "pill",
  reject_once: "pill pill-bad",
  reject_always: "pill pill-bad",
};

interface Props {
  req: PermissionRequest;
  compact?: boolean;
  highlight?: boolean;
}

export function PermissionCard({ req, compact = false, highlight = false }: Props) {
  const { state, navigate, now } = usePanel();
  const answer = usePermissionAnswer(req);
  const voiceOn = useVoiceEnabled();
  const { options, remote, machineName, offline, busy, confirm, problem, answered, resolve } = answer;
  const pet = petFor(Object.values(state.pets), req.machineId, req.agentId);
  const defaultPet = useDefaultPet();
  const session = state.sessions[req.sessionId];
  // The assistant's own session (Assistant mode): the default pet itself is asking (a start card carries the prompt).
  const asking = isAssistantRequest(req, state.sessions);
  const start = assistantStartCard(req);
  const kind = req.toolCall.kind ?? "other";
  const machine = state.machines[req.machineId];

  const raw = req.toolCall.rawInput;
  const rawText = raw === undefined || raw === null ? "" : typeof raw === "string" ? raw : JSON.stringify(raw, null, 2);
  const locations = req.toolCall.locations ?? [];

  return (
    <article
      className={`perm-card${compact ? " is-compact" : ""}${highlight ? " is-highlight" : ""}${remote ? " is-remote" : ""}${offline ? " is-offline" : ""}`}
    >
      <div className="perm-head">
        <PetAvatar species={asking ? defaultPet.species : (pet?.species ?? "dog")} activity="alert" size={compact ? 40 : 56} />
        <div className="perm-title">
          <div className="perm-who">
            {asking ? (
              <>
                <strong>{assistantAsker(defaultPet.name)}</strong> asks, working with{" "}
                <strong>{agentName(state.agents, req.agentId, req.machineId)}</strong>
              </>
            ) : (
              <>
                <strong>{pet?.name ?? "Your pet"}</strong> fetched a request from{" "}
                <strong>{agentName(state.agents, req.agentId, req.machineId)}</strong>
              </>
            )}
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
            <span className="perm-summary">{start ? `${start.title}?` : permissionSummary(req)}</span>
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
      {/* Never cut: all of what would run (the block scrolls). A start card shows its whole prompt, even compact. */}
      {start ? (
        <div className="perm-prompt-wrap">
          {start.prompt ? (
            <>
              <div className="perm-prompt-label">With this prompt (all of it):</div>
              <div className="perm-prompt" tabIndex={0} role="region" aria-label="The prompt it would send">
                {start.prompt}
              </div>
            </>
          ) : (
            <div className="perm-prompt-label">With no prompt: the session starts and waits.</div>
          )}
        </div>
      ) : (
        !compact && rawText && <pre className="perm-raw">{rawText}</pre>
      )}
      {voiceOn && req.voiceCode ? <VoiceCodeChip code={req.voiceCode} /> : null}
      {!compact && renderButtons()}
    </article>
  );

  function renderButtons() {
    if (confirm) {
      return (
        <div className="perm-actions perm-confirm" role="group" aria-label={`Approve on ${machineName}?`}>
          <span className="perm-confirm-text">
            {confirm.label} on <strong className="perm-confirm-machine">{machineName}</strong>:
            <code className="perm-confirm-command">{requestCommandText(req.toolCall)}</code>
          </span>
          <button
            className="pill pill-good"
            autoFocus
            disabled={busy !== null}
            onClick={() => void resolve(confirm.optionId, confirm.label, true)}
            onKeyDown={(e) => {
              if (e.key === "Escape") answer.cancelConfirm();
            }}
          >
            {busy === confirm.optionId ? "…" : `${confirm.label} on ${machineName}`}
          </button>
          <button className="pill pill-ghost" disabled={busy !== null} onClick={() => answer.cancelConfirm()}>
            Back
          </button>
        </div>
      );
    }
    const blocked = busy !== null || !!offline || answered;
    return (
      <div className="perm-actions">
        {options.map((o) => {
          const choose = answer.blockedReason(o.optionId);
          return (
            <button
              key={o.optionId}
              className={OPTION_CLASS[o.kind]}
              disabled={blocked || !!choose}
              title={offline ?? choose ?? undefined}
              onClick={() => void resolve(o.optionId, o.name)}
            >
              {busy === o.optionId ? "…" : answer.asksConfirm(o.optionId) ? `${o.name}…` : o.name}
            </button>
          );
        })}
        {answer.needsPlainDeny && (
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
            {options.some((o) => answer.blockedReason(o.optionId)) ? ` Choose always-allow on ${machineName}.` : ""}
          </span>
        ) : null}
      </div>
    );
  }
}
