// "Read replies aloud" (Settings > Voice): the pet's replies spoken with the webview's speech synthesis, which uses
// the system's voices (no download, nothing leaves this PC on Windows and macOS). UI-side only.
import { useEffect, useState } from "react";
import { stripVoiceCodes } from "@pawl/hub-client";
import { splitReaction } from "./reactions";

function synth(): SpeechSynthesis | null {
  return typeof window !== "undefined" && "speechSynthesis" in window ? window.speechSynthesis : null;
}

export const speechAvailable = (): boolean => synth() !== null;

/**
 * What to say for a reply: the text without its leading reaction emoji (shown as an icon), without markdown marks
 * and links' addresses, on one line. Empty when nothing is left.
 */
export function spokenText(reply: string): string {
  const { text } = splitReaction(reply);
  return text
    .replace(/```[\s\S]*?```/g, " (code) ")
    .replace(/`([^`]*)`/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/https?:\/\/\S+/g, "a link")
    .replace(/[*_#>~]+/g, "")
    .replace(/\s+/g, " ")
    .trim();
}

// ---------------------------------------------------------------- the pet is talking (the mic must not listen to it)

/**
 * After the pet stops talking, its voice may still echo in the room: the engine stays paused and wake words are
 * ignored this much longer (about a second).
 */
export const SPEECH_QUIET_MS = 1000;

let speaking = false;
let quietUntil = 0;
/** The utterance being said: only its own events count (a cancelled one reports its end late). */
let current: SpeechSynthesisUtterance | null = null;

/** The engine's own pause ends by itself after 30 s: renewed this often while the pet keeps talking. */
export const PAUSE_RENEW_MS = 20_000;

let pauseHandler: ((on: boolean) => void) | null = null;
let paused = false;
let renewTimer: ReturnType<typeof setInterval> | null = null;
let resumeTimer: ReturnType<typeof setTimeout> | null = null;

/**
 * Who pauses the voice engine while the pet speaks (voice_pause, set by the pet window while voice is on; null: no
 * engine). Pausing stops wake words and wake-started utterances; push-to-talk keeps working.
 */
export function setSpeechPause(fn: ((on: boolean) => void) | null): void {
  if (!fn && pauseHandler && paused) pauseHandler(false);
  pauseHandler = fn;
  paused = false;
  if (fn && speaking) pauseOn();
}

function pauseOn(): void {
  if (resumeTimer) clearTimeout(resumeTimer);
  resumeTimer = null;
  if (!pauseHandler) return;
  pauseHandler(true);
  paused = true;
  if (!renewTimer) renewTimer = setInterval(() => pauseHandler?.(true), PAUSE_RENEW_MS);
}

function resumeLater(): void {
  if (renewTimer) clearInterval(renewTimer);
  renewTimer = null;
  if (resumeTimer) clearTimeout(resumeTimer);
  resumeTimer = setTimeout(() => {
    resumeTimer = null;
    if (speaking || !paused) return;
    paused = false;
    pauseHandler?.(false);
  }, SPEECH_QUIET_MS);
}

/** The pet started saying something: the engine pauses. */
export function markSpeechStart(): void {
  speaking = true;
  pauseOn();
}

/** The pet stopped (finished, was cut off, or failed): quiet for `SPEECH_QUIET_MS` more, then the engine resumes. */
export function markSpeechEnd(at = Date.now()): void {
  speaking = false;
  quietUntil = Math.max(quietUntil, at + SPEECH_QUIET_MS);
  resumeLater();
}

/** The pet is talking, or stopped less than `SPEECH_QUIET_MS` ago: anything the mic hears may be its own voice. */
export function speechBusy(now = Date.now()): boolean {
  return speaking || now < quietUntil;
}

/**
 * Speak a reply (stopping whatever was being said). Never a voice code: `codes` (the codes of waiting requests) and
 * any digits after "approve" are removed first, so the microphone cannot hear the pet approve something by itself.
 */
export function speak(reply: string, opts: { voiceURI?: string; rate?: number; codes?: Iterable<string | null | undefined> } = {}): void {
  const s = synth();
  const text = stripVoiceCodes(spokenText(reply), opts.codes ?? []);
  if (!s || !text) return;
  s.cancel();
  const u = new SpeechSynthesisUtterance(text);
  const ended = () => {
    if (current !== u) return;
    current = null;
    markSpeechEnd();
  };
  u.addEventListener("end", ended);
  u.addEventListener("error", ended);
  current = u;
  // Busy from now (the start event comes a little later).
  markSpeechStart();
  const voice = opts.voiceURI ? s.getVoices().find((v) => v.voiceURI === opts.voiceURI) : undefined;
  if (voice) {
    u.voice = voice;
    u.lang = voice.lang;
  }
  u.rate = Math.min(2, Math.max(0.5, opts.rate ?? 1));
  s.speak(u);
}

/** Stop speaking (the user started talking, or closed the bubble). */
export function stopSpeaking(): void {
  const s = synth();
  if (s && (s.speaking || s.pending)) s.cancel();
  current = null;
  if (speaking) markSpeechEnd();
}

/** The system's voices (they load asynchronously in some engines). */
export function useSystemVoices(): SpeechSynthesisVoice[] {
  const [voices, setVoices] = useState<SpeechSynthesisVoice[]>(() => synth()?.getVoices() ?? []);
  useEffect(() => {
    const s = synth();
    if (!s) return;
    const update = () => setVoices(s.getVoices());
    update();
    s.addEventListener("voiceschanged", update);
    return () => s.removeEventListener("voiceschanged", update);
  }, []);
  return voices;
}
