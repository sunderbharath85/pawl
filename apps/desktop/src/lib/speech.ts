// "Read replies aloud" (Settings > Voice): the pet's replies spoken with the webview's speech synthesis, which uses
// the system's voices (no download, nothing leaves this PC on Windows and macOS). UI-side only.
import { useEffect, useState } from "react";
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

/** Speak a reply (stopping whatever was being said). */
export function speak(reply: string, opts: { voiceURI?: string; rate?: number } = {}): void {
  const s = synth();
  const text = spokenText(reply);
  if (!s || !text) return;
  s.cancel();
  const u = new SpeechSynthesisUtterance(text);
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
