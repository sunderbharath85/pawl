// "Show my screen" (v0.2 item 4) inside the pet's bubble: pick a screen or window (thumbnails) -> see the capture ->
// Send it once with a question, or Cancel. Each capture needs this click in this moment; the picture is held only in
// this component's state and dropped when it is sent or the flow closes. The line under the preview says where it
// goes (the bot's provider, or the session's agent).
import { useEffect, useRef, useState } from "react";
import { CAPTURE_WARNING, type PictureDestination } from "@pawl/hub-client";
import type { ChatImage } from "@pawl/protocol";
import { errorText } from "../lib/format";
import { captureDataUrl, pngDataUrl, screenBackend, screenRetry, sortSources, toChatImage, type ScreenCapture, type ScreenSource } from "../lib/screen";

type Phase = { kind: "loading" } | { kind: "pick"; sources: ScreenSource[] } | { kind: "capturing"; source: ScreenSource } | { kind: "preview"; source: ScreenSource; capture: ScreenCapture } | { kind: "sending" } | { kind: "error"; text: string };

export interface ScreenLookProps {
  /** "Show my screen" / "Add a picture to this prompt". */
  title: string;
  question: string;
  destination: PictureDestination;
  onSend(image: ChatImage, question: string): Promise<void>;
  onClose(): void;
}

export function ScreenLook({ title, question, destination, onSend, onClose }: ScreenLookProps) {
  const api = screenBackend();
  const [phase, setPhase] = useState<Phase>({ kind: "loading" });
  const [text, setText] = useState(question);
  const alive = useRef(true);
  /** This picker's session with the desktop's screen gate (null: none open). */
  const nonce = useRef<string | null>(null);
  /** Which start an answer belongs to: answers from an older start are ignored. */
  const gen = useRef(0);

  const endSession = () => {
    const n = nonce.current;
    nonce.current = null;
    if (n && api) api.end(n).catch(() => {});
  };

  const failFor = (g: number) => (err: unknown) => {
    if (alive.current && g === gen.current) setPhase({ kind: "error", text: errorText(err) });
  };

  /** Open a picker session (a new one replaces the old) and list what can be captured. */
  const start = () => {
    if (!api) return;
    const g = ++gen.current;
    setPhase({ kind: "loading" });
    endSession();
    api.begin().then((n) => {
      if (!alive.current || g !== gen.current) {
        api.end(n).catch(() => {});
        return;
      }
      nonce.current = n;
      loadSources();
    }, failFor(g));
  };

  const loadSources = () => {
    if (!api || !nonce.current) return start();
    const g = gen.current;
    setPhase({ kind: "loading" });
    api.sources(nonce.current).then((list) => {
      if (alive.current && g === gen.current) setPhase({ kind: "pick", sources: sortSources(list) });
    }, failFor(g));
  };

  // Begin when the picker opens (a tick later: React's development double mount then begins only once), end when it
  // closes (Cancel, Send, the bubble closing, another session).
  useEffect(() => {
    alive.current = true;
    const t = setTimeout(start, 0);
    return () => {
      clearTimeout(t);
      alive.current = false;
      gen.current++;
      endSession();
    };
    // Once per picker.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [api]);

  const pick = (source: ScreenSource) => {
    if (!api || !nonce.current) return start();
    const g = gen.current;
    setPhase({ kind: "capturing", source });
    api.capture(nonce.current, source.id).then((capture) => {
      if (alive.current && g === gen.current) setPhase({ kind: "preview", source, capture });
    }, failFor(g));
  };

  const retry = (message: string) => {
    const how = screenRetry(message);
    if (how === "restart") start();
    else if (how === "wait") {
      setPhase({ kind: "loading" });
      setTimeout(() => alive.current && loadSources(), 2000);
    } else loadSources();
  };

  const send = async () => {
    if (phase.kind !== "preview") return;
    const image = toChatImage(phase.capture);
    // From here on the picture lives only in this call: the preview is dropped at once, and the picker session ends.
    setPhase({ kind: "sending" });
    endSession();
    try {
      await onSend(image, text.trim() || question);
      onClose();
    } catch (err) {
      // Sending failed (the hub said why). The picker session has ended, so "Try again" starts a new one.
      if (alive.current) setPhase({ kind: "error", text: errorText(err) });
    }
  };

  if (!api) return null;
  return (
    <div className="pb-look" role="group" aria-label={title}>
      <div className="pb-look-head">
        <strong>{title}</strong>
        <button type="button" className="pill pill-ghost" onClick={onClose}>
          Cancel
        </button>
      </div>
      {/* The warning while choosing what to capture (the preview then shows exactly what goes). */}
      {phase.kind !== "preview" && phase.kind !== "sending" && <p className="pb-look-warning">{CAPTURE_WARNING}</p>}
      {phase.kind === "loading" && <p className="pb-look-note">Looking for screens and windows…</p>}
      {phase.kind === "pick" &&
        (phase.sources.length === 0 ? (
          <p className="pb-look-note">No screen or window to show.</p>
        ) : (
          <div className="pb-look-sources" role="listbox" aria-label="Pick a screen or window">
            {phase.sources.map((s) => (
              <button key={s.id} type="button" role="option" aria-selected={false} className="pb-look-source" onClick={() => pick(s)} title={`${s.app}: ${s.title}`}>
                <img src={pngDataUrl(s.thumb)} alt="" draggable={false} />
                <span className="pb-look-source-title">{s.kind === "screen" ? s.title : s.app}</span>
                {s.kind === "window" && <span className="pb-look-source-sub">{s.title}</span>}
              </button>
            ))}
          </div>
        ))}
      {phase.kind === "capturing" && <p className="pb-look-note">Taking a picture of {phase.source.kind === "screen" ? phase.source.title : phase.source.app}…</p>}
      {phase.kind === "preview" && (
        <>
          <img className="pb-look-preview" src={captureDataUrl(phase.capture)} alt={`The capture of ${phase.source.title}`} draggable={false} />
          <p className={`pb-look-dest${destination.canSee ? "" : " is-none"}`}>
            Goes {destination.text}
            {destination.canSee ? `, for this one message. ${destination.copy}` : ". Set up a model that can see pictures in Settings > Bot."}
          </p>
          <form
            className="pb-look-send"
            onSubmit={(e) => {
              e.preventDefault();
              void send();
            }}
          >
            <input className="pb-look-question" value={text} maxLength={500} aria-label="Your question about it" onChange={(e) => setText(e.target.value)} />
            <span className="pb-look-actions">
              <button type="submit" className="pill pill-primary" disabled={!destination.canSee}>
                Send
              </button>
              <button type="button" className="pill" onClick={loadSources}>
                Pick another
              </button>
            </span>
          </form>
        </>
      )}
      {phase.kind === "sending" && <p className="pb-look-note">Sending…</p>}
      {phase.kind === "error" && (
        <p className="pb-look-note is-problem" role="alert">
          {phase.text}{" "}
          <button type="button" className="link" onClick={() => retry(phase.text)}>
            Try again
          </button>
        </p>
      )}
    </div>
  );
}
