import { useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { SPECIES_META } from "@krypto/pet-engine";
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
} from "@krypto/protocol";
import { agentFor, petFor } from "../shared/machine";
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

export function ApprovalCard({
  request,
  session,
  agents,
  pets,
  now,
}: {
  request: PermissionRequest;
  session?: Session;
  agents: Agent[];
  pets: Pet[];
  now: number;
}) {
  const [busy, setBusy] = useState(false);
  const title = request.toolCall.title || request.toolCall.kind || "Tool call";
  const extra = [...request.options]
    .sort((a, b) => OPTION_ORDER[a.kind] - OPTION_ORDER[b.kind])
    .filter((o) => o.kind === "allow_always" || o.kind === "reject_always");
  const resolve = (msg: Parameters<typeof post>[0]) => {
    setBusy(true);
    post(msg);
    // The card disappears when the hub confirms; re-enable if it did not (e.g. hub error).
    setTimeout(() => setBusy(false), 4000);
  };
  return (
    <div className="card approval">
      <div className="card-head">
        <span className={`kind kind-${request.toolCall.kind ?? "other"}`}>{request.toolCall.kind ?? "tool"}</span>
        <span className="muted">
          {petLabel(pets, request)} · {agentName(agents, request.agentId, request.machineId)}
          {session ? ` · ${basename(session.cwd)}` : ""} · {relativeTime(request.createdAt, now)}
        </span>
      </div>
      <div className="card-title">{title}</div>
      <Locations locations={request.toolCall.locations ?? []} cwd={session?.cwd} />
      <div className="actions">
        <button className="primary" disabled={busy} onClick={() => resolve({ type: "approve", requestId: request.requestId })}>
          Approve
        </button>
        <button className="danger" disabled={busy} onClick={() => resolve({ type: "deny", requestId: request.requestId })}>
          Deny
        </button>
        {extra.map((o) => (
          <button
            key={o.optionId}
            className="secondary small"
            disabled={busy}
            title={o.kind.replace("_", " ")}
            onClick={() => resolve({ type: "resolve", requestId: request.requestId, optionId: o.optionId })}
          >
            {o.name}
          </button>
        ))}
      </div>
    </div>
  );
}

// ---------------- sessions ----------------

export function SessionList({
  sessions,
  agents,
  pets,
  connected,
  showEnded,
  now,
  onToggleEnded,
  onSelect,
}: {
  sessions: Session[];
  agents: Agent[];
  pets: Pet[];
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
      <ul className="list">
        {shown.map((s) => (
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
      {ended.length > 0 && (
        <button className="link toggle" onClick={onToggleEnded}>
          {showEnded ? "Hide ended sessions" : `Show ${ended.length} ended session${ended.length === 1 ? "" : "s"}`}
        </button>
      )}
    </div>
  );
}

function TranscriptRow({ item, cwd }: { item: TranscriptItem; cwd: string }) {
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
      return <div className="note permission">🔔 {item.text}</div>;
    case "stop":
      return <div className="divider">{item.reason}</div>;
    case "error":
      return <div className="note error">⚠ {item.text}</div>;
    case "note":
      return <div className="note">{item.text}</div>;
  }
}

export function SessionDetail({
  session,
  events,
  agents,
  pets,
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
  const inWorkspace = workspaceFolders.some((f) => session.cwd.toLowerCase().startsWith(f.toLowerCase()));

  return (
    <div className="detail">
      <div className="detail-head">
        <button className="icon" title="Back to sessions" onClick={onBack}>
          ‹
        </button>
        <StatusDot status={session.status} ended={!!session.endedAt} />
        <div className="detail-title">
          <div>
            {petLabel(pets, session)} · {agentName(agents, session.agentId, session.machineId)}{" "}
            <span className="muted">· {session.endedAt ? "ended" : STATUS_LABEL[session.status]}</span>
          </div>
          <div className="muted small" title={session.cwd}>
            {session.title?.trim() ? `${session.title.trim()} · ` : ""}
            {basename(session.cwd)}
            {inWorkspace ? "" : " (outside this workspace)"} · {SOURCE_LABEL[session.source]} · started{" "}
            {relativeTime(session.startedAt, now)}
          </div>
        </div>
        {connected && session.source === "owned" && isBusy(session) && (
          <button className="danger small" onClick={() => post({ type: "cancel", sessionId: session.id })}>
            Cancel
          </button>
        )}
      </div>

      {pending.length > 0 && <div className="note permission">🔔 Waiting for your approval above.</div>}

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
          items.map((item) => <TranscriptRow key={item.key} item={item} cwd={session.cwd} />)
        )}
      </div>

      {canPrompt ? (
        <Composer
          placeholder={isBusy(session) ? "Queue a follow-up…" : "Reply to the agent…"}
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

export function ChatPanel({
  chat,
  pets,
  agents,
  connected,
  petId,
  onPetChange,
}: {
  chat: ChatMessage[];
  pets: Pet[];
  agents: Agent[];
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
