import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { HubEvent } from "@ai-pet/protocol";
import {
  SOURCE_LABEL,
  STATUS_META,
  TOOL_KIND_ICON,
  agentName,
  clockTime,
  errorText,
  isActive,
  sessionTitle,
  shortPath,
  timeAgo,
} from "../../lib/format";
import { usePanel } from "../context";
import { buildFeed, STOP_LABEL, type FeedItem } from "../feed";
import * as I from "../icons";
import { ACTIVITY_LABEL, PetAvatar } from "../PetAvatar";
import { PermissionCard } from "./PermissionCard";

const EMPTY: HubEvent[] = [];

export function SessionView({ sessionId }: { sessionId: string }) {
  const { client, state, views, now, toast, meta } = usePanel();
  const session = state.sessions[sessionId];
  const events = state.events[sessionId] ?? EMPTY;
  const feed = useMemo(() => buildFeed(events), [events]);
  const pet = views.find((v) => v.sessionId === sessionId) ?? views.find((v) => v.agentId === session?.agentId);
  const pending = Object.values(state.pending).filter((p) => p.sessionId === sessionId);
  const [loading, setLoading] = useState(false);

  // Backfill history once per session (merged into state.events by the client).
  useEffect(() => {
    if (meta.phase !== "connected") return;
    let alive = true;
    setLoading(true);
    client
      .getEvents(sessionId, undefined, 500)
      .catch((err) => alive && toast(`Could not load events: ${errorText(err)}`, "error"))
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
  }, [client, sessionId, meta.phase, toast]);

  const scrollRef = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  }, [feed.length, events]);

  if (!session) {
    return (
      <section className="view">
        <div className="empty">
          <PetAvatar species="dog" activity="sad" size={100} />
          <h3>That session wandered off</h3>
          <p>It may have been cleared when the hub restarted.</p>
        </div>
      </section>
    );
  }

  const status = STATUS_META[session.status];
  const active = isActive(session);
  const canPrompt = session.source === "owned" && !session.endedAt;

  return (
    <section className="session-view">
      <header className="session-head">
        <PetAvatar species={pet?.species ?? "dog"} activity={pet?.activity ?? "idle"} size={64} />
        <div className="session-head-main">
          <h1 title={session.cwd}>{sessionTitle(session, state.agents)}</h1>
          <div className="session-head-meta">
            <span className={`status-pill tone-${status.tone}`}>
              <span className={`dot tone-${status.tone}`} /> {status.label}
            </span>
            <span className="chip">{agentName(state.agents, session.agentId)}</span>
            <span className="chip chip-soft">{SOURCE_LABEL[session.source]}</span>
            <span className="chip chip-soft mono" title={session.cwd}>
              <I.Folder size={13} /> {shortPath(session.cwd || "~", 3)}
            </span>
            <span className="muted">started {timeAgo(session.startedAt, now)}</span>
          </div>
          {pet && (
            <div className="session-head-pet">
              {pet.name}: {ACTIVITY_LABEL[pet.activity].toLowerCase()}
            </div>
          )}
        </div>
        {canPrompt && active && (
          <button
            className="pill pill-bad"
            onClick={() => client.cancel(session.id).catch((err) => toast(errorText(err), "error"))}
          >
            <I.Stop size={14} /> Cancel turn
          </button>
        )}
      </header>

      <div
        className="feed"
        ref={scrollRef}
        onScroll={(e) => {
          const el = e.currentTarget;
          stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
        }}
      >
        {feed.length === 0 && (
          <div className="feed-empty">
            {loading ? "Sniffing out the history…" : "Nothing here yet. Events stream in as the agent works."}
          </div>
        )}
        {feed.map((item) => (
          <FeedRow key={item.key} item={item} />
        ))}
        {pending.map((req) => (
          <div className="feed-pending" key={req.requestId}>
            <PermissionCard req={req} />
          </div>
        ))}
        {active && session.status !== "waiting_permission" && (
          <div className="typing">
            <span />
            <span />
            <span />
          </div>
        )}
      </div>

      {canPrompt ? (
        <PromptBar sessionId={session.id} busy={active} />
      ) : (
        <div className="prompt-note">
          {session.endedAt
            ? "This session has ended."
            : `This session runs in ${SOURCE_LABEL[session.source]}; your pet can watch and alert, but prompts and approvals stay there.`}
        </div>
      )}
    </section>
  );
}

function FeedRow({ item }: { item: FeedItem }) {
  switch (item.kind) {
    case "text":
      if (item.role === "user")
        return (
          <div className="msg msg-user">
            <div className="msg-bubble">{item.text}</div>
            <time>{clockTime(item.ts)}</time>
          </div>
        );
      if (item.role === "thought")
        return (
          <details className="thought">
            <summary>💭 Thinking…</summary>
            <p>{item.text}</p>
          </details>
        );
      return (
        <div className="msg msg-agent">
          <div className="msg-bubble">{item.text}</div>
          <time>{clockTime(item.ts)}</time>
        </div>
      );
    case "tool":
      return (
        <div className={`tool tool-${item.status}`}>
          <span className="tool-icon">{TOOL_KIND_ICON[item.toolKind]}</span>
          <div className="tool-main">
            <div className="tool-title">{item.title}</div>
            {item.locations.length > 0 && (
              <div className="tool-locs mono">
                {item.locations
                  .slice(0, 3)
                  .map((l) => shortPath(l.path, 3) + (l.line ? `:${l.line}` : ""))
                  .join("  ·  ")}
                {item.locations.length > 3 ? `  +${item.locations.length - 3}` : ""}
              </div>
            )}
          </div>
          <span className={`tool-status st-${item.status}`}>{item.status.replace("_", " ")}</span>
        </div>
      );
    case "plan": {
      const done = item.entries.filter((e) => e.status === "completed").length;
      return (
        <div className="plan">
          <div className="plan-head">
            📋 Plan <span className="muted">{done}/{item.entries.length}</span>
          </div>
          <ul>
            {item.entries.map((e, i) => (
              <li key={i} className={`plan-${e.status} prio-${e.priority}`}>
                <span className="plan-box">{e.status === "completed" ? "✓" : e.status === "in_progress" ? "•" : ""}</span>
                {e.content}
              </li>
            ))}
          </ul>
        </div>
      );
    }
    case "permission":
      return (
        <div className="notice notice-alert">
          🐕 Barked for approval: <strong>{item.title}</strong>
        </div>
      );
    case "resolved": {
      const ok = item.outcome?.kind === "selected" && !/reject|deny/i.test(item.outcome.optionId);
      return (
        <div className={`notice ${ok ? "notice-good" : "notice-muted"}`}>
          {item.outcome?.kind === "cancelled" ? "Request cancelled" : ok ? "Approved" : "Denied"}
          {item.by === "rule" ? " by a rule" : ""}
        </div>
      );
    }
    case "stop":
      return (
        <div className={`turn-end reason-${item.reason}`}>
          <span>
            {item.reason === "end_turn" ? "🐾 " : ""}
            {STOP_LABEL[item.reason] ?? item.reason}
          </span>
          <time>{clockTime(item.ts)}</time>
        </div>
      );
    case "error":
      return <div className="notice notice-error">⚠️ {item.message}</div>;
    case "notice":
      return (
        <div className="notice notice-muted">
          {item.icon} {item.text}
        </div>
      );
  }
}

function PromptBar({ sessionId, busy }: { sessionId: string; busy: boolean }) {
  const { client, toast } = usePanel();
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);

  const send = async () => {
    const t = text.trim();
    if (!t || sending) return;
    setSending(true);
    try {
      await client.prompt(sessionId, t);
      setText("");
    } catch (err) {
      toast(errorText(err), "error");
    } finally {
      setSending(false);
    }
  };

  return (
    <form
      className="prompt-bar"
      onSubmit={(e) => {
        e.preventDefault();
        void send();
      }}
    >
      <textarea
        value={text}
        rows={2}
        placeholder={busy ? "The agent is mid-turn; send a follow-up when it finishes (or cancel)…" : "Ask the agent to do something…"}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault();
            void send();
          }
        }}
      />
      <button className="pill pill-primary send" type="submit" disabled={!text.trim() || sending}>
        <I.Send size={15} /> Send
      </button>
    </form>
  );
}
