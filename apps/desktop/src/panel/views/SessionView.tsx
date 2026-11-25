// One session: a tidy header (pet, status, agent, machine, source, folder) and the conversation, rendered with AI
// Elements from hub-client's transcript (what the person asked, what the agent said and did), then the prompt box
// (owned sessions) or a note that prompts and approvals stay in the terminal (hook sessions).
import { useEffect, useState } from "react";
import type { HubEvent } from "@pawl/protocol";
import { SOURCE_LABEL, STATUS_META, agentName, errorText, isActive, sessionTitle, timeAgo } from "../../lib/format";
import { folderName, intentErrorText, isLocal, machineDisplayName, offlineReason, sessionBackfill } from "@pawl/hub-client";
import { usePanel } from "../context";
import * as I from "../icons";
import { ACTIVITY_LABEL, PetAvatar } from "../PetAvatar";
import { SessionTranscript } from "../transcript/Transcript";

const EMPTY: HubEvent[] = [];

export function SessionView({ sessionId }: { sessionId: string }) {
  const { client, state, views, now, toast, meta } = usePanel();
  const session = state.sessions[sessionId];
  const events = state.events[sessionId] ?? EMPTY;
  const pet =
    views.find((v) => v.sessionId === sessionId) ??
    views.find((v) => v.machineId === session?.machineId && v.agentId === session?.agentId);
  const pending = Object.values(state.pending).filter((p) => p.sessionId === sessionId);
  const [loading, setLoading] = useState(false);

  // Backfill on open and catch up after every reconnect, using `seq` as the cursor
  // (fetched events are merged into state.events by the client, deduped by global id).
  useEffect(() => {
    if (meta.phase !== "connected") return;
    let alive = true;
    setLoading(true);
    sessionBackfill
      .sync(client, sessionId)
      .catch((err) => alive && toast(`Could not load events: ${errorText(err)}`, "error"))
      .finally(() => alive && setLoading(false));
    return () => {
      alive = false;
    };
  }, [client, sessionId, meta.phase, toast]);

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
  // Another machine's session: prompts and cancels are forwarded to its owner (migration step 4).
  const remote = !isLocal(state, session.machineId);
  const machine = state.machines[session.machineId];
  const machineName = machineDisplayName(machine);
  const offline = offlineReason(state, session.machineId);

  return (
    <section className={`session-view${offline ? " is-offline" : ""}`}>
      <header className="session-head">
        <PetAvatar species={pet?.species ?? "dog"} activity={pet?.activity ?? "idle"} size={64} />
        <div className="session-head-main">
          <h1 title={sessionTitle(session, state.agents)}>{sessionTitle(session, state.agents)}</h1>
          <div className="session-head-meta">
            <span className={`status-pill tone-${status.tone}`}>
              <span className={`dot tone-${status.tone}`} /> {status.label}
            </span>
            <span className="chip">{agentName(state.agents, session.agentId, session.machineId)}</span>
            {remote && (
              <span className={`chip chip-machine${offline ? " is-offline" : ""}`} title={`Runs on ${machineName}`}>
                <I.Monitor size={13} /> {machineName}
                {offline ? ` · offline, last seen ${timeAgo(machine?.lastSeen ?? 0, now)}` : ""}
              </span>
            )}
            <span className="chip chip-soft">{SOURCE_LABEL[session.source]}</span>
            <span className="chip chip-soft mono" title={session.cwd}>
              <I.Folder size={13} /> {folderName(session.cwd)}
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
            disabled={!!offline}
            title={offline ?? (remote ? `Cancels the turn on ${machineName}` : undefined)}
            onClick={() =>
              client
                .cancel(session.id)
                .catch((err) => toast(remote ? intentErrorText(err, machineName, "cancel") : errorText(err), "error"))
            }
          >
            <I.Stop size={14} /> Cancel turn
          </button>
        )}
      </header>

      <SessionTranscript
        session={session}
        events={events}
        pending={pending}
        working={active && session.status !== "waiting_permission"}
        empty={loading ? "Sniffing out the history…" : "Nothing here yet. The conversation shows up as the agent works."}
      />

      {canPrompt ? (
        <PromptBar sessionId={session.id} busy={active} disabledReason={offline} remoteName={remote ? machineName : null} />
      ) : (
        <div className="prompt-note">
          {session.endedAt
            ? "This session has ended."
            : `This session runs in ${SOURCE_LABEL[session.source]}. Your pet watches and tells you when it needs you; prompts and approvals stay there.`}
        </div>
      )}
    </section>
  );
}

function PromptBar({
  sessionId,
  busy,
  disabledReason,
  remoteName,
}: {
  sessionId: string;
  busy: boolean;
  disabledReason: string | null;
  /** The owner's name when the session runs on another machine (the prompt is forwarded there). */
  remoteName: string | null;
}) {
  const { client, toast } = usePanel();
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);

  const send = async () => {
    const t = text.trim();
    if (!t || sending || disabledReason) return;
    setSending(true);
    try {
      await client.prompt(sessionId, t);
      setText("");
    } catch (err) {
      toast(remoteName ? intentErrorText(err, remoteName, "prompt") : errorText(err), "error");
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
        disabled={!!disabledReason}
        title={disabledReason ?? undefined}
        placeholder={
          disabledReason ??
          (busy
            ? "The agent is mid-turn; send a follow-up when it finishes (or cancel)…"
            : remoteName
              ? `Ask the agent on ${remoteName} to do something…`
              : "Ask the agent to do something…")
        }
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault();
            void send();
          }
        }}
      />
      <button className="pill pill-primary send" type="submit" disabled={!text.trim() || sending || !!disabledReason}>
        <I.Send size={15} /> Send
      </button>
    </form>
  );
}
