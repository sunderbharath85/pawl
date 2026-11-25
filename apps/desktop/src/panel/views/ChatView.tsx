import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { SPECIES_META } from "@pawl/pet-engine";
import { clockTime, errorText } from "../../lib/format";
import { usePanel } from "../context";
import * as I from "../icons";
import { PetAvatar } from "../PetAvatar";
import { ChatProposals } from "./ChatProposals";
import { PetReplyText } from "../../lib/ReactionIcon";
import { useVoiceSentIds } from "../../lib/voiceShared";
import { useDefaultPet } from "../useDefaultPet";

const SUGGESTIONS = ["What's everyone doing?", "Summarize the last session", "Approve it", "Tell Codex to run the tests"];

/** The chat with the default pet: the one pet, which orchestrates every agent (the bubble's "Pawl-wide" chat). */
export function ChatView() {
  const { client, state, views, toast, meta, navigate } = usePanel();
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const voiceIds = useVoiceSentIds();
  const pet = useDefaultPet();
  // Replies from before there was one pet may carry an agent pet's id: they keep that pet's name.
  const pets = state.pets;

  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [state.chat.length]);

  const send = async (raw: string) => {
    const t = raw.trim();
    if (!t || sending) return;
    setSending(true);
    try {
      await client.chat(t);
      setText("");
    } catch (err) {
      toast(errorText(err), "error");
    } finally {
      setSending(false);
    }
  };

  const activityOf = (id: string | undefined) => views.find((v) => v.petId === id)?.activity ?? "idle";
  const waitingReply = useMemo(() => {
    const last = state.chat[state.chat.length - 1];
    return !!last && last.role === "user" && Date.now() - last.ts < 30_000;
  }, [state.chat]);

  return (
    <section className="chat-view">
      <header className="view-head chat-head">
        <div>
          <h1>Chat</h1>
          <p>Ask what your agents are up to, or tell them what to do next.</p>
        </div>
      </header>

      <div className="chat-log" ref={scrollRef}>
        {state.chat.length === 0 && (
          <div className="empty">
            <PetAvatar species={pet.species} activity="happy" size={110} />
            <h3>
              {SPECIES_META[pet.species].sound} I'm {pet.name}.
            </h3>
            <p>I keep an eye on all your agents, on every machine. Try one of these:</p>
          </div>
        )}
        {state.chat.map((m) => {
          const from = m.petId ? pets[m.petId] : undefined;
          const fromName = m.petId ? (from?.name ?? "Pet") : pet.name;
          const fromSpecies = m.petId ? (from?.species ?? "dog") : pet.species;
          return m.role === "user" ? (
            <div key={m.id} className="msg msg-user">
              <div className="msg-bubble">
                {voiceIds.has(m.id) ? <span className="msg-via">You (voice): </span> : null}
                {m.text}
                {m.image ? <span className="shot-chip">screenshot</span> : null}
              </div>
              <time>{clockTime(m.ts)}</time>
            </div>
          ) : (
            <div key={m.id} className="msg msg-pet">
              <PetAvatar species={fromSpecies} activity={from ? activityOf(from.id) : pet.activity} size={36} />
              <div>
                <div className="msg-name">{fromName}</div>
                <div className="msg-bubble">
                  <PetReplyText text={m.text} />
                </div>
                <ChatProposals message={m} />
                <div className="msg-foot">
                  <time>{clockTime(m.ts)}</time>
                  {m.sessionId && state.sessions[m.sessionId] && (
                    <button className="link" onClick={() => navigate({ view: "session", sessionId: m.sessionId! })}>
                      open session →
                    </button>
                  )}
                </div>
              </div>
            </div>
          );
        })}
        {waitingReply && (
          <div className="msg msg-pet">
            <PetAvatar species={pet.species} activity="thinking" size={36} />
            <div className="typing">
              <span />
              <span />
              <span />
            </div>
          </div>
        )}
      </div>

      <div className="suggestions">
        {SUGGESTIONS.map((s) => (
          <button key={s} className="suggestion" onClick={() => void send(s)} disabled={sending || meta.phase !== "connected"}>
            {s}
          </button>
        ))}
      </div>
      <form
        className="prompt-bar"
        onSubmit={(e) => {
          e.preventDefault();
          void send(text);
        }}
      >
        <textarea
          rows={2}
          value={text}
          placeholder={`Say something to ${pet.name}…`}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              void send(text);
            }
          }}
        />
        <button className="pill pill-primary send" type="submit" disabled={!text.trim() || sending || meta.phase !== "connected"}>
          <I.Send size={15} /> Send
        </button>
      </form>
    </section>
  );
}
