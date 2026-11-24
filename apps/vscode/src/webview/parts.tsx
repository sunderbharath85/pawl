import { useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { SPECIES_META } from "@pawl/pet-engine";
import {
  DEFAULT_SPECIES,
  type Agent,
  type ChatMessage,
  type HubEvent,
  type PermissionOption,
  type PermissionRequest,
  type Pet,
  type Session,
  type SessionStatus,
  type ToolLocation,
} from "@pawl/protocol";
import {
  agentFor,
  approveOptionId,
  ownerName,
  proposalExpired,
  type ProposalEntry,
  groupByMachine,
  isLocal,
  machineDisplayName,
  offlineReason,
  petFor,
  remoteOptionBlocked,
  type HubState,
} from "@pawl/hub-client";
import type { ConnInfo } from "../shared/messages";
import { buildTranscript, type TranscriptItem } from "./transcript";
import { post } from "./vscodeApi";

// ---------------- helpers ----------------

function basename(p: string): string {
  const parts = p.split(/[\\/]+/).filter(Boolean);
  return parts[parts.length - 1] ?? p;
}

function relativeTime(ts: number, now: number): string {
  const s = Math.max(0, Math.round((now - ts) / 1000));
  if (s < 45) return "just now";
  const m = Math.round(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.round(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.round(h / 24)}d ago`;
}

function agentName(agents: Agent[], id: string, machineId?: string): string {
  return agentFor(agents, id, machineId)?.name ?? id;
}

/** "🐕 Biscuit" for the pet watching this (machine, agent), else just the default emoji. */
function petLabel(pets: Pet[], owner: { machineId: string; agentId: string }): string {
  const pet = petFor(pets, owner.machineId, owner.agentId);
  const emoji = SPECIES_META[pet?.species ?? DEFAULT_SPECIES].emoji;
  return pet ? `${emoji} ${pet.name}` : emoji;
}

const STATUS_LABEL: Record<SessionStatus, string> = {
  idle: "idle",
  thinking: "thinking",
  working: "working",
  waiting_permission: "needs you",
  done: "done",
  error: "error",
};

const SOURCE_LABEL: Record<Session["source"], string> = {
  owned: "pet",
  vscode_hook: "VS Code",
  cli_hook: "terminal",
  wrapper: "editor",
};

const BUSY: ReadonlySet<SessionStatus> = new Set(["thinking", "working", "waiting_permission"]);

function isBusy(s: Session): boolean {
  return !s.endedAt && BUSY.has(s.status);
}

function StatusDot({ status, ended }: { status: SessionStatus; ended: boolean }) {
  return <span className={`dot status-${ended ? "ended" : status}`} aria-hidden="true" />;
}

function openLocation(loc: ToolLocation, cwd?: string): void {
  post({ type: "openFile", path: loc.path, line: loc.line, cwd });
}

function Locations({ locations, cwd }: { locations: ToolLocation[]; cwd?: string }) {
  if (locations.length === 0) return null;
  return (
    <span className="locations">
      {locations.slice(0, 4).map((loc, i) => (
        <button
          key={`${loc.path}:${loc.line ?? ""}:${i}`}
          className="link path"
          title={loc.path}
          onClick={() => openLocation(loc, cwd)}
        >
          {basename(loc.path)}
          {loc.line ? `:${loc.line}` : ""}
        </button>
      ))}
      {locations.length > 4 && <span className="muted">+{locations.length - 4}</span>}
    </span>
  );
}

// ---------------- toasts & banner ----------------

export interface Toast {
  id: number;
  text: string;
}

export function Toasts({ toasts, onDismiss }: { toasts: Toast[]; onDismiss(id: number): void }) {
  if (toasts.length === 0) return null;
  return (
    <div className="toasts" role="status">
      {toasts.map((t) => (
        <div key={t.id} className="toast">
          <span>{t.text}</span>
          <button className="icon" title="Dismiss" onClick={() => onDismiss(t.id)}>
            ×
          </button>
        </div>
      ))}
    </div>
  );
}

export function ConnBanner({ conn }: { conn: ConnInfo }) {
  if (conn.status === "connected") return null;
  if (!conn.offline) {
    return <div className="banner connecting">Connecting to the hub…</div>;
  }
  let detail = "The pet reconnects by itself as soon as the hub is up.";
  if (conn.missingToken) detail = "The hub has not been started on this machine yet.";
  else if (conn.status === "unauthorized") detail = "The hub rejected our token — it may have been regenerated.";
  return (
    <div className="banner offline" role="alert">
      <div>
        <strong>Hub offline</strong>
        <div className="muted">{detail}</div>
      </div>
      <div className="actions">
        <button className="primary" onClick={() => post({ type: "command", command: "startHub" })}>
          Start hub
        </button>
        <button onClick={() => post({ type: "command", command: "reconnect" })}>Retry</button>
      </div>
    </div>
  );
}

// ---------------- approvals ----------------

const OPTION_ORDER: Record<PermissionOption["kind"], number> = { allow_once: 0, allow_always: 1, reject_once: 2, reject_always: 3 };

/** Which machine this webview's hub runs on, and every known machine (for names and online state). */
export type Place = Pick<HubState, "machine" | "machines">;

/** "Mac mini" chip for another machine's item; greyed with "last seen" when that machine is offline. */
function MachineChip({ place, machineId, now }: { place: Place; machineId: string; now: number }) {
  if (isLocal(place, machineId)) return null;
  const m = place.machines[machineId];
  return (
    <span className={`machine-chip${m?.online ? "" : " offline"}`} title={m?.online ? "Runs on that machine" : "Offline"}>
      {machineDisplayName(m)}
      {m?.online ? "" : ` · last seen ${relativeTime(m?.lastSeen ?? 0, now)}`}
    </span>
  );
}

export function ApprovalCard({
  request,
  session,
  agents,
  pets,
  place,
  now,
}: {
  request: PermissionRequest;
  session?: Session;
  agents: Agent[];
  pets: Pet[];
  place: Place;
  now: number;
}) {
  const [busy, setBusy] = useState(false);
  // Another machine's request is forwarded to it; the host asks in a modal before approving (migration step 4).
  const remote = !isLocal(place, request.machineId);
  const offline = offlineReason(place, request.machineId);
  const owner = machineDisplayName(place.machines[request.machineId]);
  // Remotely only allow_once approves; a request offering only always-allow is approved on its machine.
  const noApprove = approveOptionId(place, request) === null;
  const title = request.toolCall.title || request.toolCall.kind || "Tool call";
  const extra = [...request.options]
    .sort((a, b) => OPTION_ORDER[a.kind] - OPTION_ORDER[b.kind])
    .filter((o) => o.kind === "allow_always" || o.kind === "reject_always");
  const resolve = (msg: Parameters<typeof post>[0]) => {
    if (offline) return;
    setBusy(true);
    post(msg);
    // The card disappears when the hub confirms; re-enable if it did not (e.g. hub error).
    setTimeout(() => setBusy(false), 4000);
  };
  return (
    <div className={`card approval${remote ? " remote" : ""}${offline ? " offline" : ""}`}>
      <div className="card-head">
        <span className={`kind kind-${request.toolCall.kind ?? "other"}`}>{request.toolCall.kind ?? "tool"}</span>
        <span className="muted">
          {petLabel(pets, request)} · {agentName(agents, request.agentId, request.machineId)}
          {session ? ` · ${basename(session.cwd)}` : ""} · {relativeTime(request.createdAt, now)}
        </span>
        <MachineChip place={place} machineId={request.machineId} now={now} />
      </div>
      <div className="card-title">{title}</div>
      <Locations locations={request.toolCall.locations ?? []} cwd={session?.cwd} />
      <div className="actions">
        <button
          className="primary"
          disabled={busy || !!offline || noApprove}
          title={offline ?? (noApprove ? `Choose always-allow on ${owner}` : remote ? `Approve on ${owner} (asks you to confirm)` : undefined)}
          onClick={() => resolve({ type: "approve", requestId: request.requestId })}
        >
          {remote ? "Approve…" : "Approve"}
        </button>
        <button
          className="danger"
          disabled={busy || !!offline}
          title={offline ?? undefined}
          onClick={() => resolve({ type: "deny", requestId: request.requestId })}
        >
          Deny
        </button>
        {extra.map((o) => (
          <button
            key={o.optionId}
            className="secondary small"
            disabled={busy || !!offline || !!remoteOptionBlocked(place, request, o.optionId)}
            title={offline ?? remoteOptionBlocked(place, request, o.optionId) ?? o.kind.replace("_", " ")}
            onClick={() => resolve({ type: "resolve", requestId: request.requestId, optionId: o.optionId })}
          >
            {o.name}
          </button>
        ))}
      </div>
      {offline ? (
        <div className="muted small remote-note">{offline}: answer it when it is back.</div>
      ) : remote ? (
        <div className="muted small remote-note">
          Runs on {owner}; approving asks you to confirm first.
          {request.options.some((o) => remoteOptionBlocked(place, request, o.optionId)) ? ` Choose always-allow on ${owner}.` : ""}
        </div>
      ) : null}
    </div>
  );
}

// ---------------- sessions ----------------

export function SessionList({
  sessions,
  agents,
  pets,
  place,
  connected,
  showEnded,
  now,
  onToggleEnded,
  onSelect,
}: {
  sessions: Session[];
  agents: Agent[];
  pets: Pet[];
  place: Place;
  connected: boolean;
  showEnded: boolean;
  now: number;
  onToggleEnded(): void;
  onSelect(id: string): void;
}) {
  const sorted = useMemo(() => {
    const rank = (s: Session) => (s.endedAt ? 3 : s.status === "waiting_permission" ? 0 : isBusy(s) ? 1 : 2);
    return [...sessions].sort((a, b) => rank(a) - rank(b) || b.lastEventAt - a.lastEventAt);
  }, [sessions]);
  const open = sorted.filter((s) => !s.endedAt);
  const ended = sorted.filter((s) => s.endedAt);
  const shown = showEnded ? sorted : open;
  // Grouped by machine; headings only once another machine's sessions are mirrored here.
  const groups = useMemo(() => groupByMachine(place, shown), [place, shown]);

  if (!connected && sessions.length === 0) {
    return <div className="empty">Sessions appear here once the hub is connected.</div>;
  }
  return (
    <div className="sessions">
      {shown.length === 0 && (
        <div className="empty">
          No open sessions.{" "}
          <button className="link" disabled={!connected} onClick={() => post({ type: "command", command: "startSession" })}>
            Start one
          </button>
        </div>
      )}
      {groups.map((g) => (
        <div key={g.machineId} className={`machine-group${g.online ? "" : " offline"}`}>
          {groups.length > 1 && (
            <div className="group-head">
              {g.local ? `${machineDisplayName(g.machine)} (this machine)` : machineDisplayName(g.machine)}
              {!g.online && <span className="muted"> · offline, last seen {relativeTime(g.machine?.lastSeen ?? 0, now)}</span>}
            </div>
          )}
          <ul className="list">
            {g.items.map((s) => (
              <li key={s.id}>
                <button className={`row ${s.endedAt ? "ended" : ""}`} onClick={() => onSelect(s.id)} title={s.cwd}>
                  <StatusDot status={s.status} ended={!!s.endedAt} />
                  <span className="row-main">
                    <span className="row-title">
                      {petLabel(pets, s)} · {agentName(agents, s.agentId, s.machineId)}
                      <span className="muted"> · {s.title?.trim() || basename(s.cwd)}</span>
                    </span>
                    <span className="row-sub muted">
                      {s.endedAt ? "ended" : STATUS_LABEL[s.status]} · {SOURCE_LABEL[s.source]} · {relativeTime(s.lastEventAt, now)}
                    </span>
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </div>
      ))}
      {ended.length > 0 && (
        <button className="link toggle" onClick={onToggleEnded}>
          {showEnded ? "Hide ended sessions" : `Show ${ended.length} ended session${ended.length === 1 ? "" : "s"}`}
        </button>
      )}
    </div>
  );
}

function TranscriptRow({ item, cwd, machineName, place }: { item: TranscriptItem; cwd: string; machineName: string; place: Place }) {
  switch (item.kind) {
    case "user":
      return <div className="msg user">{item.text}</div>;
    case "agent":
      return <div className="msg agent">{item.text}</div>;
    case "thought":
      return (
        <details className="thought">
          <summary>Thinking…</summary>
          <div className="msg-thought">{item.text}</div>
        </details>
      );
    case "tool":
      return (
        <div className={`tool status-${item.status ?? "pending"}`}>
          <span className="tool-status" aria-hidden="true">
            {item.status === "completed" ? "✓" : item.status === "failed" ? "✗" : item.status === "in_progress" ? "…" : "○"}
          </span>
          <span className={`kind kind-${item.toolKind ?? "other"}`}>{item.toolKind ?? "tool"}</span>
          <span className="tool-title">{item.title}</span>
          <Locations locations={item.locations} cwd={cwd} />
        </div>
      );
    case "plan":
      return (
        <div className="plan">
          <div className="plan-title">Plan</div>
          <ul>
            {item.entries.map((e, i) => (
              <li key={i} className={`plan-${e.status}`}>
                <span aria-hidden="true">{e.status === "completed" ? "☑" : e.status === "in_progress" ? "◐" : "☐"}</span> {e.content}
              </li>
            ))}
          </ul>
        </div>
      );
    case "permission":
      return (
        <div className="note permission">
          🔔 {item.text}
          {item.fromMachineId
            ? ` · from ${machineDisplayName(place.machines[item.fromMachineId] ?? (item.fromMachineId === place.machine?.id ? place.machine : undefined))}`
            : ""}
        </div>
      );
    case "stop":
      return <div className="divider">{item.reason}</div>;
    case "error":
      return <div className="note error">⚠ {item.text}</div>;
    case "note":
      return <div className="note">{item.text}</div>;
    case "truncated":
      return (
        <div className="note" title={item.bytes ? `${Math.max(1, Math.round(item.bytes / 1024))} KB not copied to this machine` : undefined}>
          📦 Large output — open on {machineName}
        </div>
      );
  }
}

export function SessionDetail({
  session,
  events,
  agents,
  pets,
  place,
  pending,
  connected,
  now,
  workspaceFolders,
  onBack,
}: {
  session: Session;
  events: HubEvent[];
  agents: Agent[];
  pets: Pet[];
  place: Place;
  pending: PermissionRequest[];
  connected: boolean;
  now: number;
  workspaceFolders: string[];
  onBack(): void;
}) {
  const items = useMemo(() => buildTranscript(events), [events]);
  const scroller = useRef<HTMLDivElement | null>(null);
  const stick = useRef(true);

  // Ask the host to backfill whenever this session is shown while connected (and again after a
  // reconnect). The host loads the latest window the first time, then catches up by `seq`.
  useEffect(() => {
    if (connected) post({ type: "loadEvents", sessionId: session.id });
  }, [connected, session.id]);

  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [items]);

  const canPrompt = connected && session.source === "owned" && !session.endedAt;
  // Another machine's session: prompts and cancels are forwarded to it (migration step 4).
  const remote = !isLocal(place, session.machineId);
  const machine = place.machines[session.machineId];
  const machineName = machineDisplayName(machine);
  const offline = offlineReason(place, session.machineId);
  const inWorkspace = !remote && workspaceFolders.some((f) => session.cwd.toLowerCase().startsWith(f.toLowerCase()));

  return (
    <div className={`detail${offline ? " offline" : ""}`}>
      <div className="detail-head">
        <button className="icon" title="Back to sessions" onClick={onBack}>
          ‹
        </button>
        <StatusDot status={session.status} ended={!!session.endedAt} />
        <div className="detail-title">
          <div>
            {petLabel(pets, session)} · {agentName(agents, session.agentId, session.machineId)}{" "}
            <span className="muted">· {session.endedAt ? "ended" : STATUS_LABEL[session.status]}</span>{" "}
            <MachineChip place={place} machineId={session.machineId} now={now} />
          </div>
          <div className="muted small" title={session.cwd}>
            {session.title?.trim() ? `${session.title.trim()} · ` : ""}
            {basename(session.cwd)}
            {remote || inWorkspace ? "" : " (outside this workspace)"} · {SOURCE_LABEL[session.source]} · started{" "}
            {relativeTime(session.startedAt, now)}
          </div>
        </div>
        {connected && session.source === "owned" && isBusy(session) && (
          <button
            className="danger small"
            disabled={!!offline}
            title={offline ?? (remote ? `Cancels the turn on ${machineName}` : undefined)}
            onClick={() => post({ type: "cancel", sessionId: session.id })}
          >
            Cancel
          </button>
        )}
      </div>

      {pending.length > 0 && (
        <div className="note permission">🔔 {remote ? `${machineName} is waiting for your approval above.` : "Waiting for your approval above."}</div>
      )}

      <div
        className="transcript"
        ref={scroller}
        onScroll={(e) => {
          const el = e.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 40;
        }}
      >
        {items.length === 0 ? (
          <div className="empty">{session.source === "owned" ? "Nothing yet." : "This session reports through hooks; only milestones show up here."}</div>
        ) : (
          items.map((item) => <TranscriptRow key={item.key} item={item} cwd={session.cwd} machineName={machineName} place={place} />)
        )}
      </div>

      {canPrompt ? (
        <Composer
          placeholder={offline ?? (isBusy(session) ? "Queue a follow-up…" : remote ? `Reply to the agent on ${machineName}…` : "Reply to the agent…")}
          disabled={!!offline}
          onSend={(text) => {
            stick.current = true;
            post({ type: "prompt", sessionId: session.id, text });
          }}
        />
      ) : (
        session.source !== "owned" && (
          <div className="muted small hint">Started outside the pet — reply in the tool that runs it.</div>
        )
      )}
    </div>
  );
}

// ---------------- composer & chat ----------------

function Composer({ placeholder, disabled, onSend }: { placeholder: string; disabled?: boolean; onSend(text: string): void }) {
  const [text, setText] = useState("");
  const ref = useRef<HTMLTextAreaElement | null>(null);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 160)}px`;
  }, [text]);
  const send = () => {
    const t = text.trim();
    if (!t || disabled) return;
    onSend(t);
    setText("");
  };
  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
      e.preventDefault();
      send();
    }
  };
  return (
    <div className="composer">
      <textarea
        ref={ref}
        rows={1}
        value={text}
        placeholder={placeholder}
        disabled={disabled}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKeyDown}
      />
      <button className="primary" disabled={disabled || !text.trim()} onClick={send} title="Send (Enter)">
        Send
      </button>
    </div>
  );
}

/**
 * Chat proposals (PLAN.md migration step 5): commands the bot understood for another machine's agent, as
 * buttons. A click asks the host, which confirms in a modal ("Run on Mac mini?") before anything is sent.
 * Single use per message (alternatives: once one runs the rest are spent) and only for 10 minutes.
 */
function ProposalButtons({
  message,
  entry,
  place,
  connected,
  now,
  onOpenSession,
}: {
  message: ChatMessage;
  entry: ProposalEntry | undefined;
  place: Place;
  connected: boolean;
  now: number;
  onOpenSession(id: string): void;
}) {
  const list = message.proposals ?? [];
  if (list.length === 0) return null;
  const expired = proposalExpired(message, now);
  const open = connected && !expired && (!entry || (entry.status === "failed" && entry.retryable));
  return (
    <div className="proposals">
      {list.length > 1 && open && <div className="muted small">Pick one:</div>}
      {list.map((p) => {
        const offline = offlineReason(place, p.machineId);
        return (
          <button
            key={p.id}
            className={`secondary proposal${entry?.proposalId === p.id ? " used" : ""}`}
            disabled={!open || !!offline}
            title={offline ?? (expired ? "This suggestion expired; ask again." : `Runs on ${ownerName(place, p.machineId)} after you confirm`)}
            onClick={() => post({ type: "proposal", messageId: message.id, proposalId: p.id })}
          >
            {p.summary}
          </button>
        );
      })}
      {entry?.status === "running" && <div className="muted small">Sending…</div>}
      {entry?.status === "done" && (
        <div className="small proposal-ok">
          {entry.text}{" "}
          {entry.sessionId && (
            <button className="link" onClick={() => onOpenSession(entry.sessionId!)}>
              Open session
            </button>
          )}
        </div>
      )}
      {entry?.status === "failed" && (
        <div className="small proposal-error" role="alert">
          {entry.text}
          {entry.retryable && !expired ? " You can try again." : ""}
        </div>
      )}
      {expired && !entry && <div className="muted small">This suggestion expired. Ask again to get a fresh one.</div>}
    </div>
  );
}

export function ChatPanel({
  chat,
  pets,
  agents,
  place,
  proposals,
  now,
  onOpenSession,
  connected,
  petId,
  onPetChange,
}: {
  chat: ChatMessage[];
  pets: Pet[];
  agents: Agent[];
  place: Place;
  proposals: Record<string, ProposalEntry>;
  now: number;
  onOpenSession(id: string): void;
  connected: boolean;
  petId: string | null;
  onPetChange(id: string | null): void;
}) {
  const scroller = useRef<HTMLDivElement | null>(null);
  const activePet = pets.find((p) => p.id === petId);
  const shown = useMemo(
    () => (activePet ? chat.filter((m) => !m.petId || m.petId === activePet.id) : chat),
    [chat, activePet],
  );
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [shown.length]);
  const petName = (id?: string) => {
    const p = pets.find((x) => x.id === id) ?? activePet;
    return p ? `${SPECIES_META[p.species].emoji} ${p.name}` : `${SPECIES_META[DEFAULT_SPECIES].emoji} Pet`;
  };
  return (
    <div className="chat">
      {pets.length > 1 && (
        <label className="chat-pet">
          Talk to{" "}
          <select value={activePet?.id ?? ""} onChange={(e) => onPetChange(e.target.value || null)}>
            <option value="">any pet</option>
            {pets.map((p) => (
              <option key={p.id} value={p.id}>
                {SPECIES_META[p.species].emoji} {p.name} ({agentName(agents, p.agentId, p.machineId)})
              </option>
            ))}
          </select>
        </label>
      )}
      <div className="transcript" ref={scroller}>
        {shown.length === 0 ? (
          <div className="empty">
            Ask “what’s Claude doing?”, say “tell Codex to run the tests”, or just “approve”.
          </div>
        ) : (
          shown.map((m) => (
            <div key={m.id} className={`msg ${m.role === "user" ? "user" : "agent"}`}>
              {m.role === "pet" && <div className="msg-from">{petName(m.petId)}</div>}
              {m.text}
              {m.role === "pet" && (
                <ProposalButtons
                  message={m}
                  entry={proposals[m.id]}
                  place={place}
                  connected={connected}
                  now={now}
                  onOpenSession={onOpenSession}
                />
              )}
            </div>
          ))
        )}
      </div>
      <Composer
        placeholder={connected ? "Message your pet…" : "Hub offline"}
        disabled={!connected}
        onSend={(text) => post({ type: "chat", text, petId: activePet?.id })}
      />
    </div>
  );
}
