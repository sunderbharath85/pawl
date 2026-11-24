import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { SPECIES_META } from "@ai-pet/pet-engine";
import { agentName, clockTime, errorText } from "../../lib/format";
import { petList } from "../../lib/pets";
import { usePanel } from "../context";
import * as I from "../icons";
import { PetAvatar } from "../PetAvatar";

const SUGGESTIONS = ["What's everyone doing?", "Summarize the last session", "Approve it", "Tell Codex to run the tests"];

export function ChatView({ initialPetId }: { initialPetId?: string }) {
  const { client, state, views, toast, meta, navigate } = usePanel();
  const pets = useMemo(() => petList(state), [state]);
  const [petId, setPetId] = useState<string | undefined>(() => initialPetId ?? pets[0]?.id);
  const pet = pets.find((p) => p.id === petId) ?? pets[0];
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const scrollRef = useRef<HTMLDivElement>(null);
  const realPet = pet && state.pets[pet.id] ? pet : undefined;

  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [state.chat.length]);

  const send = async (raw: string) => {
    const t = raw.trim();
    if (!t || sending) return;
    setSending(true);
    try {
      await client.chat(t, realPet?.id);
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
        {pets.length > 1 && (
          <div className="pet-chips">
            {pets.map((p) => (
              <button key={p.id} className={`pet-chip${p.id === pet?.id ? " is-active" : ""}`} onClick={() => setPetId(p.id)}>
                <PetAvatar species={p.species} activity={activityOf(p.id)} size={26} />
                {p.name}
              </button>
            ))}
          </div>
        )}
      </header>

      <div className="chat-log" ref={scrollRef}>
        {state.chat.length === 0 && (
          <div className="empty">
            <PetAvatar species={pet?.species ?? "dog"} activity="happy" size={110} />
            <h3>
              {SPECIES_META[pet?.species ?? "dog"].sound} I'm {pet?.name ?? "your pet"}.
            </h3>
            <p>
              {realPet ? `I keep an eye on ${agentName(state.agents, realPet.agentId)}.` : "I keep an eye on your agents."} Try one of
              these:
            </p>
          </div>
        )}
        {state.chat.map((m) => {
          const from = m.petId ? pets.find((p) => p.id === m.petId) : pet;
          return m.role === "user" ? (
            <div key={m.id} className="msg msg-user">
              <div className="msg-bubble">{m.text}</div>
              <time>{clockTime(m.ts)}</time>
            </div>
          ) : (
            <div key={m.id} className="msg msg-pet">
              <PetAvatar species={from?.species ?? "dog"} activity={activityOf(from?.id)} size={36} />
              <div>
                <div className="msg-name">{from?.name ?? "Pet"}</div>
                <div className="msg-bubble">{m.text}</div>
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
            <PetAvatar species={pet?.species ?? "dog"} activity="thinking" size={36} />
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
          placeholder={`Say something to ${pet?.name ?? "your pet"}…`}
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
