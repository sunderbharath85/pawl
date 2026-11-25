// The pet's chat bubble (opened by a click on the pet, or by itself when an approval arrives). The default pet's
// bubble is the orchestrator: every pending approval on top, then the chat with the general bot (no pet id) and its
// proposals. A session pet's bubble is that session's: its approvals, its recent messages and a prompt box.
// There is deliberately no "open the panel" button: the panel opens from the pet's right-click menu or the tray.
import { useEffect, useLayoutEffect, useMemo, useRef, useState, type CSSProperties, type ReactNode } from "react";
import {
  requestCommandText,
  intentErrorText,
  isAlreadyDecided,
  isLocal,
  machineDisplayName,
  offlineReason,
  ownerName,
  petFor,
  sessionBackfill,
  type HubClient,
  type HubState,
} from "@pawl/hub-client";
import { describePermission } from "@pawl/pet-engine";
import type { HubEvent, PermissionRequest, Session } from "@pawl/protocol";
import type { ConnectionMeta } from "../lib/connection";
import { agentName, clockTime, errorText, sessionTitle } from "../lib/format";
import { draftFor, DEFAULT_TARGET, exampleChips, isNotSureReply, type Draft } from "../lib/bubble";
import { PetReplyText } from "../lib/ReactionIcon";
import type { RenameResult } from "../lib/usePetName";
import type { ListenPhase, MicMarker as Marker } from "../lib/voice";
import { useVoiceSentIds } from "../lib/voiceShared";
import { ListeningPanel, MicButton, MicMarker } from "./VoiceBits";
import { BUBBLE_W, type BubblePlace } from "../lib/petLayout";
import { repoName, type DefaultPetState, type SessionPet } from "../lib/petScreen";
import { answerErrorText } from "../lib/where";
import { buildFeed } from "../panel/feed";
import { ChatProposalsView, type ProposalEnv } from "../panel/views/ChatProposals";

export interface BubbleEnv {
  client: HubClient;
  state: HubState;
  now: number;
  connected: boolean;
  /** Show a short message in the bubble (errors, "Already answered."). */
  say(text: string): void;
  /** A request someone already answered: hide it at once (the mirror drops it soon after). */
  dismiss(requestId: string): void;
  /** The person sent this chat message from the default pet's bubble (the pet animates until the reply). */
  chatSent(messageId: string): void;
  /** The person prompted this session from its pet's bubble (that pet animates until the agent's first reply). */
  promptSent(sessionId: string, at: number): void;
}

const EMPTY_EVENTS: HubEvent[] = [];
const CHAT_SHOWN = 6;
const FEED_SHOWN = 4;
const LINE_MAX = 280;

function clip(text: string, max = LINE_MAX): string {
  const t = text.trim();
  return t.length > max ? `${t.slice(0, max - 1)}…` : t;
}

interface ShellProps {
  place: BubblePlace;
  /** The window's height (an "above" bubble is anchored by its bottom edge). */
  windowHeight: number;
  label: string;
  title: ReactNode;
  sub?: ReactNode;
  footer?: ReactNode;
  note?: string | null;
  onClose(): void;
  children?: ReactNode;
}

export function BubbleShell({ place, windowHeight, label, title, sub, footer, note, onClose, children }: ShellProps) {
  const style: CSSProperties & Record<"--tail-x", string> = {
    left: place.left,
    width: BUBBLE_W,
    maxHeight: place.maxHeight,
    "--tail-x": `${place.tailX}px`,
    ...(place.vertical === "above" ? { bottom: windowHeight - place.anchorY } : { top: place.anchorY }),
  };
  return (
    <div className={`pet-bubble is-${place.vertical}`} style={style} data-pet-hit role="dialog" aria-label={label}>
      <header className="pb-head">
        <div className="pb-head-text">
          <div className="pb-title">{title}</div>
          {sub ? <div className="pb-sub">{sub}</div> : null}
        </div>
        <button type="button" className="pb-close" aria-label="Close" title="Close (Esc)" onClick={onClose}>
          ×
        </button>
      </header>
      {note ? (
        <div className="pb-note is-problem" role="status">
          {note}
        </div>
      ) : null}
      {children ? <div className="pb-body">{children}</div> : null}
      {footer}
    </div>
  );
}

/** A small speech bubble with just a line (a toast while the chat bubble is closed). */
export function SayBubble({ place, windowHeight, text, onClick }: { place: BubblePlace; windowHeight: number; text: string; onClick(): void }) {
  const style: CSSProperties & Record<"--tail-x", string> = {
    left: place.left,
    width: BUBBLE_W,
    "--tail-x": `${place.tailX}px`,
    ...(place.vertical === "above" ? { bottom: windowHeight - place.anchorY } : { top: place.anchorY }),
  };
  return (
    <div className={`pet-say is-${place.vertical}`} style={style} role="status">
      <button type="button" className="pet-say-text" data-pet-hit onClick={onClick}>
        {text}
      </button>
    </div>
  );
}

// ---------------------------------------------------------------- approvals

export function ApprovalCard({ req, env }: { req: PermissionRequest; env: BubbleEnv }) {
  const { client, state } = env;
  const remote = !isLocal(state, req.machineId);
  const machine = machineDisplayName(state.machines[req.machineId]);
  const pet = petFor(Object.values(state.pets), req.machineId, req.agentId);
  const agent = agentName(state.agents, req.agentId, req.machineId);
  const action = describePermission(req);
  // The whole command (never only the agent's title), shown and repeated in the confirm step.
  const command = requestCommandText(req.toolCall);
  // Another machine accepts allow_once only; always-allow is chosen on that machine.
  const blocked = remote && !(req.options ?? []).some((o) => o.kind === "allow_once") ? `Choose always-allow on ${machine}` : null;
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState<"approve" | "deny" | null>(null);
  useEffect(() => {
    if (!busy) return;
    const t = setTimeout(() => setBusy(null), 15_000);
    return () => clearTimeout(t);
  }, [busy]);

  const run = (which: "approve" | "deny", call: () => Promise<unknown>) => {
    if (busy) return;
    setBusy(which);
    call().catch((err: unknown) => {
      setBusy(null);
      setConfirming(false);
      if (isAlreadyDecided(err)) {
        env.dismiss(req.requestId);
        env.say("Already answered.");
        return;
      }
      env.say(answerErrorText(client.getState(), req.requestId, which, err));
    });
  };

  const who = `${pet ? `${pet.name} (${agent})` : agent}${remote ? ` on ${machine}` : ""}`;
  return (
    <div className={`pb-approval${remote ? " is-remote" : ""}`} role="group" aria-label={`${who} needs permission`}>
      <div className="pb-approval-who">{who}</div>
      {confirming ? (
        <div className="pb-approval-text">
          Approve on <strong>{machine}</strong>:
        </div>
      ) : (
        <div className="pb-approval-text">{action}?</div>
      )}
      {command !== action && <pre className="pb-command">{command}</pre>}
      <div className="pb-actions">
        {confirming ? (
          <>
            <button type="button" className="pill pill-good" autoFocus disabled={busy !== null} onClick={() => run("approve", () => client.approve(req.requestId, { confirmed: true }))}>
              {busy === "approve" ? "…" : `Approve on ${machine}`}
            </button>
            <button type="button" className="pill pill-ghost" disabled={busy !== null} onClick={() => setConfirming(false)}>
              Back
            </button>
          </>
        ) : (
          <>
            <button
              type="button"
              className="pill pill-good"
              disabled={busy !== null || !!blocked}
              title={blocked ?? undefined}
              onClick={() => (remote ? setConfirming(true) : run("approve", () => client.approve(req.requestId)))}
            >
              {busy === "approve" ? "…" : remote ? "Approve…" : "Approve"}
            </button>
            <button type="button" className="pill pill-bad" disabled={busy !== null} onClick={() => run("deny", () => client.deny(req.requestId))}>
              {busy === "deny" ? "…" : "Deny"}
            </button>
          </>
        )}
      </div>
      {blocked ? <div className="pb-hint">{blocked}.</div> : null}
    </div>
  );
}

// ---------------------------------------------------------------- composer

interface ComposerProps {
  /** Whose box this is (the default pet or a session id): a draft never moves to another target. */
  target: string;
  placeholder: string;
  disabledReason: string | null;
  autoFocus: boolean;
  onSend(text: string): Promise<unknown>;
  /** Another control before Send (the default pet's mic button). */
  extra?: ReactNode;
}

function Composer({ target, placeholder, disabledReason, autoFocus, onSend, extra }: ComposerProps) {
  const [draft, setDraft] = useState<Draft>({ target, text: "" });
  const text = draftFor(draft, target);
  const setText = (t: string) => setDraft({ target, text: t });
  const [sending, setSending] = useState(false);
  const submit = async () => {
    const t = text.trim();
    if (!t || sending || disabledReason) return;
    setSending(true);
    try {
      await onSend(t);
      setDraft((d) => (d.target === target ? { target, text: "" } : d));
    } catch {
      /* the bubble says why; keep the text */
    } finally {
      setSending(false);
    }
  };
  return (
    <form
      className="pb-composer"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <textarea
        rows={2}
        value={text}
        autoFocus={autoFocus}
        disabled={!!disabledReason}
        title={disabledReason ?? undefined}
        placeholder={disabledReason ?? placeholder}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault();
            void submit();
          }
        }}
      />
      {extra}
      <button type="submit" className="pill pill-primary" disabled={!text.trim() || sending || !!disabledReason}>
        {sending ? "…" : "Send"}
      </button>
    </form>
  );
}

function useStickToBottom(dep: unknown) {
  const ref = useRef<HTMLDivElement>(null);
  useLayoutEffect(() => {
    const el = ref.current?.closest(".pb-body");
    if (el) el.scrollTop = el.scrollHeight;
  }, [dep]);
  return ref;
}

// ---------------------------------------------------------------- the default pet (orchestrator)

export interface DefaultBubbleProps {
  env: BubbleEnv;
  place: BubblePlace;
  windowHeight: number;
  name: string;
  main: DefaultPetState;
  pending: PermissionRequest[];
  setupDone: boolean;
  focusInput: boolean;
  note: string | null;
  /** "Rename…" was picked in the pet's menu: show the rename field on top. */
  renaming?: boolean;
  onRename?(raw: string): Promise<RenameResult>;
  onRenameDone?(): void;
  /** Voice (Settings > Voice): the mic marker, the listening panel and the mic button. */
  voice?: BubbleVoice;
  onClose(): void;
}

export interface BubbleVoice {
  marker: Marker;
  phase: ListenPhase;
  level: number;
  /** Voice is on (the mic button shows). */
  enabled: boolean;
  /** Why talking is unavailable right now (null: it works). */
  unavailable: string | null;
  /** The mic button: start talking, or stop and send. */
  onMic(): void;
  onCancel(): void;
}

/** The default pet's rename field (from "Rename…" in its menu): Enter saves through the hub, Esc cancels. */
export function RenameField({ name, onRename, onDone }: { name: string; onRename(raw: string): Promise<RenameResult>; onDone(): void }) {
  const [draft, setDraft] = useState(name);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    input.current?.focus();
    input.current?.select();
  }, []);
  const save = async () => {
    if (saving) return;
    setSaving(true);
    const res = await onRename(draft);
    setSaving(false);
    if (res.ok) onDone();
    else setError(res.error);
  };
  return (
    <form
      className="pb-rename"
      onSubmit={(e) => {
        e.preventDefault();
        void save();
      }}
    >
      <label className="pb-rename-label" htmlFor="pb-rename-input">
        New name for {name}
      </label>
      <span className="pb-rename-row">
        <input
          ref={input}
          id="pb-rename-input"
          className={`pb-rename-input${error ? " is-invalid" : ""}`}
          value={draft}
          maxLength={80}
          disabled={saving}
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? "pb-rename-error" : undefined}
          onChange={(e) => {
            setDraft(e.target.value);
            setError(null);
          }}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              // Esc leaves the rename field, not the bubble.
              e.preventDefault();
              e.stopPropagation();
              onDone();
            }
          }}
        />
        <button type="submit" className="pill pill-primary" disabled={saving || !draft.trim()}>
          {saving ? "Saving…" : "Save"}
        </button>
        <button type="button" className="pill" disabled={saving} onClick={onDone}>
          Cancel
        </button>
      </span>
      {error ? (
        <span className="pb-rename-error" id="pb-rename-error" role="alert">
          {error}
        </span>
      ) : null}
    </form>
  );
}

export function DefaultBubble({ env, place, windowHeight, name, main, pending, setupDone, focusInput, note, renaming, onRename, onRenameDone, voice, onClose }: DefaultBubbleProps) {
  const voiceIds = useVoiceSentIds();
  const { client, state, now, connected } = env;
  const chat = useMemo(() => state.chat.filter((m) => !m.petId).slice(-CHAT_SHOWN), [state.chat]);
  const last = chat[chat.length - 1];
  const waitingReply = !!last && last.role === "user" && now - last.ts < 30_000;
  const proposalEnv: ProposalEnv = { client, state, now, connected };
  const send = (t: string) =>
    client.chat(t).then(
      (m) => {
        env.chatSent(m.id);
        return m;
      },
      (err: unknown) => {
        env.say(errorText(err));
        throw err;
      },
    );
  // One example at a time: a double tap must not send it twice.
  const [chipBusy, setChipBusy] = useState(false);
  const sendChip = (t: string) => {
    if (chipBusy) return;
    setChipBusy(true);
    send(t)
      .catch(() => {})
      .finally(() => setChipBusy(false));
  };
  // The pet did not understand: offer three examples to tap (fitted to what is going on), never a panel button.
  const chips = useMemo(() => {
    if (!last || last.role !== "pet" || !isNotSureReply(last.text)) return [];
    const recent = Object.values(state.sessions).sort((a, b) => b.lastEventAt - a.lastEventAt)[0];
    return exampleChips({
      pendingCount: pending.length,
      recentRepo: recent ? repoName(recent.cwd) : null,
      agentName: recent ? agentName(state.agents, recent.agentId, recent.machineId) : null,
    });
  }, [last, state.sessions, state.agents, pending.length]);
  const end = useStickToBottom(`${chat.length}:${pending.length}:${waitingReply}`);

  return (
    <BubbleShell
      place={place}
      windowHeight={windowHeight}
      label={`${name}, your default pet`}
      title={
        <>
          {name}
          {voice && <MicMarker marker={voice.marker} where="head" />}
        </>
      }
      sub={main.line}
      note={note}
      onClose={onClose}
      footer={
        <Composer
          target={DEFAULT_TARGET}
          placeholder={`Tell ${name} what to do, or ask what's going on…`}
          disabledReason={connected ? null : "The hub is not connected."}
          autoFocus={focusInput && pending.length === 0 && !renaming && !voice?.phase}
          onSend={send}
          extra={
            voice?.enabled ? <MicButton listening={voice.phase === "listening"} disabledReason={connected ? voice.unavailable : "The hub is not connected."} onClick={voice.onMic} /> : null
          }
        />
      }
    >
      {voice?.phase ? <ListeningPanel phase={voice.phase} level={voice.level} onStop={voice.onMic} onCancel={voice.onCancel} /> : null}
      {renaming && onRename && onRenameDone ? <RenameField name={name} onRename={onRename} onDone={onRenameDone} /> : null}
      {pending.map((req) => (
        <ApprovalCard key={req.requestId} req={req} env={env} />
      ))}
      {main.waiting.map((s) => (
        <WaitingNote key={s.id} session={s} state={state} />
      ))}
      {!setupDone && <div className="pb-hint">New here? Right-click me and choose Open hub to finish setting up.</div>}
      {chat.length === 0 ? (
        <div className="pb-empty">
          I'm {name}. I keep an eye on all your agents. Ask “what's everyone doing?” or tell me “run the tests in api”.
        </div>
      ) : (
        <div className="pb-chat">
          {chat.map((m) =>
            m.role === "user" ? (
              <div key={m.id} className="pb-msg is-user">
                <div className="pb-msg-text">
                  {voiceIds.has(m.id) ? <span className="pb-msg-via">You (voice): </span> : null}
                  {m.text}
                </div>
              </div>
            ) : (
              <div key={m.id} className="pb-msg is-pet">
                <div className="pb-msg-text">
                  <PetReplyText text={m.text} />
                </div>
                <ChatProposalsView message={m} env={proposalEnv} />
                <time className="pb-time">{clockTime(m.ts)}</time>
              </div>
            ),
          )}
          {chips.length > 0 && connected && (
            <div className="pb-chips" role="group" aria-label="Try one of these">
              {chips.map((c) => (
                <button key={c} type="button" className="pb-chip" disabled={chipBusy} onClick={() => sendChip(c)}>
                  {c}
                </button>
              ))}
            </div>
          )}
          {waitingReply && (
            <div className="typing" aria-label={`${name} is typing`}>
              <span />
              <span />
              <span />
            </div>
          )}
        </div>
      )}
      <div ref={end} />
    </BubbleShell>
  );
}

function WaitingNote({ session, state }: { session: Session; state: HubState }) {
  const agent = agentName(state.agents, session.agentId, session.machineId);
  const where = session.source === "vscode_hook" ? "VS Code" : session.source === "cli_hook" ? "its terminal" : "its window";
  const machine = isLocal(state, session.machineId) ? "" : ` on ${machineDisplayName(state.machines[session.machineId])}`;
  return (
    <div className="pb-waiting">
      {agent} is waiting for you in {where} ({repoName(session.cwd)}
      {machine}). Answer it there.
    </div>
  );
}

// ---------------------------------------------------------------- a session pet

export interface SessionBubbleProps {
  env: BubbleEnv;
  place: BubblePlace;
  windowHeight: number;
  pet: SessionPet;
  pending: PermissionRequest[];
  focusInput: boolean;
  note: string | null;
  onClose(): void;
}

export function SessionBubble({ env, place, windowHeight, pet, pending, focusInput, note, onClose }: SessionBubbleProps) {
  const { client, state, connected } = env;
  const session = state.sessions[pet.sessionId];
  const events = state.events[pet.sessionId] ?? EMPTY_EVENTS;
  const lines = useMemo(
    () =>
      buildFeed(events)
        .filter((i): i is Extract<typeof i, { kind: "text" }> => i.kind === "text" && i.role !== "thought" && i.text.trim() !== "")
        .slice(-FEED_SHOWN),
    [events],
  );
  const end = useStickToBottom(`${lines.length}:${pending.length}`);

  // Load the session's recent events (they are not all mirrored up front).
  useEffect(() => {
    if (!connected) return;
    sessionBackfill.sync(client, pet.sessionId).catch(() => {});
  }, [client, pet.sessionId, connected]);

  const remoteName = pet.remote ? ownerName(state, pet.machineId) : null;
  const canPrompt = !!session && session.source === "owned" && !session.endedAt;
  const disabledReason = !connected
    ? "The hub is not connected."
    : !canPrompt
      ? "Started outside Pawl: type in its own terminal or editor."
      : offlineReason(state, pet.machineId);
  const status = pending.length > 0 ? "Needs you" : session?.status === "waiting_permission" ? "Waiting for you" : session?.status === "thinking" ? "Thinking" : "Working";
  const sub = `${status} · ${session ? sessionTitle(session, state.agents) : pet.label}`;

  return (
    <BubbleShell
      place={place}
      windowHeight={windowHeight}
      label={`${pet.label} session`}
      title={pet.label}
      sub={sub}
      note={note}
      onClose={onClose}
      footer={
        <Composer
          target={pet.sessionId}
          placeholder={`Tell ${pet.name} what to do next…`}
          disabledReason={disabledReason}
          autoFocus={focusInput && pending.length === 0}
          onSend={(t) => {
            const at = Date.now();
            return client.prompt(pet.sessionId, t).then(
              () => env.promptSent(pet.sessionId, at),
              (err: unknown) => {
                env.say(remoteName ? intentErrorText(err, remoteName, "prompt") : errorText(err));
                throw err;
              },
            );
          }}
        />
      }
    >
      {pending.map((req) => (
        <ApprovalCard key={req.requestId} req={req} env={env} />
      ))}
      {session && session.status === "waiting_permission" && pending.length === 0 && <WaitingNote session={session} state={state} />}
      {lines.length === 0 ? (
        <div className="pb-empty">Nothing said yet in this session.</div>
      ) : (
        <div className="pb-chat">
          {lines.map((l) => (
            <div key={l.key} className={`pb-msg ${l.role === "user" ? "is-user" : "is-pet"}`}>
              <div className="pb-msg-text">{clip(l.text)}</div>
            </div>
          ))}
        </div>
      )}
      <div ref={end} />
    </BubbleShell>
  );
}

// ---------------------------------------------------------------- hub not connected

export function SleepyBubble({
  meta,
  place,
  windowHeight,
  name,
  onWake,
  onClose,
}: {
  meta: ConnectionMeta;
  place: BubblePlace;
  windowHeight: number;
  name: string;
  onWake(): void;
  onClose(): void;
}) {
  let title = "Zzz…";
  let body: ReactNode = null;
  switch (meta.phase) {
    case "hub-down":
      title = "Zzz… the hub is asleep.";
      body = (
        <>
          <p className="pb-text">{meta.startError ?? "Wake it so I can watch your agents."}</p>
          <div className="pb-actions">
            <button type="button" className="pill pill-primary" onClick={onWake}>
              Wake the hub
            </button>
          </div>
        </>
      );
      break;
    case "starting":
      title = "Waking the hub… *stretch*";
      break;
    case "connecting":
    case "booting":
      title = "Sniffing for the hub…";
      break;
    case "unauthorized":
      title = "The hub doesn't recognise me.";
      body = <p className="pb-text">Token mismatch: restart the hub or check PAWL_HUB_TOKEN. Right-click me and choose Open hub for details.</p>;
      break;
    case "connected":
      break;
  }
  return (
    <BubbleShell place={place} windowHeight={windowHeight} label={`${name}: hub status`} title={title} onClose={onClose}>
      {body}
    </BubbleShell>
  );
}
