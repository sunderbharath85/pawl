// Voice in the pet window: the mic marker (on the pet and in its bubble's header), the bubble's listening panel and
// its mic button. All static: no animation (the pet stays still unless the user talks to it).
import { levelPercent, type ListenPhase, type MicMarker as Marker } from "../lib/voice";

/** A small microphone; crossed out when muted. Always shown while the mic is live. */
export function MicIcon({ muted, size = 14 }: { muted?: boolean; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 16 16" aria-hidden focusable="false">
      <rect x="5.5" y="1.5" width="5" height="8.5" rx="2.5" fill="currentColor" />
      <path d="M3.5 7.5a4.5 4.5 0 0 0 9 0M8 12v2.5M5.5 14.5h5" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      {muted && <path d="M2.5 2.5l11 11" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />}
    </svg>
  );
}

const MARKER_LABEL: Record<NonNullable<Marker>, string> = {
  live: "The microphone is on",
  muted: "The microphone is muted",
};

/** The marker on the pet (top-left of its box) or in the bubble's header. */
export function MicMarker({ marker, where }: { marker: Marker; where: "pet" | "head" }) {
  if (!marker) return null;
  return (
    <span className={`mic-marker on-${where} is-${marker}`} role="img" aria-label={MARKER_LABEL[marker]} title={MARKER_LABEL[marker]}>
      <MicIcon muted={marker === "muted"} size={where === "pet" ? 13 : 12} />
    </span>
  );
}

/** "Listening…" with the level, then "Transcribing…", on top of the default pet's bubble. */
export function ListeningPanel({ phase, level, onStop, onCancel }: { phase: ListenPhase; level: number; onStop(): void; onCancel(): void }) {
  if (!phase) return null;
  return (
    <div className={`pb-voice is-${phase}`} role="status" aria-live="polite">
      <span className="pb-voice-icon" aria-hidden>
        <MicIcon size={16} />
      </span>
      <span className="pb-voice-text">
        <strong>{phase === "listening" ? "Listening…" : "Transcribing…"}</strong>
        {phase === "listening" ? (
          <span className="pb-voice-level" aria-hidden>
            <span style={{ width: `${levelPercent(level)}%` }} />
          </span>
        ) : (
          <span className="pb-voice-hint">Turning what you said into text, on this PC.</span>
        )}
      </span>
      {phase === "listening" ? (
        <span className="pb-voice-actions">
          <button type="button" className="pill pill-primary" onClick={onStop} title="Send what you said">
            Done
          </button>
          <button type="button" className="pill" onClick={onCancel} title="Drop it (Esc)">
            Cancel
          </button>
        </span>
      ) : null}
    </div>
  );
}

/** The composer's mic button: start talking, or stop (send) while listening. */
export function MicButton({ listening, disabledReason, onClick }: { listening: boolean; disabledReason: string | null; onClick(): void }) {
  return (
    <button
      type="button"
      className={`pb-mic${listening ? " is-on" : ""}`}
      aria-pressed={listening}
      aria-label={listening ? "Stop and send" : "Talk"}
      title={disabledReason ?? (listening ? "Stop and send" : "Talk (or hold the pet)")}
      disabled={!!disabledReason && !listening}
      onClick={onClick}
    >
      <MicIcon size={16} />
    </button>
  );
}
