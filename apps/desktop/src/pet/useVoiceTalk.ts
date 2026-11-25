// Talking to the default pet (v0.2 voice), for the pet window. It:
// - opens the bubble in listening mode on a wake word or whenever an utterance starts (press-and-hold, the bubble's
//   mic button, the global shortcut);
// - sends each final transcript ONCE (per id) to the default pet's chat as an ordinary message, so it goes through
//   the same parser and proposals and can never approve anything by itself; an empty one gets "I didn't catch that";
// - keeps the wake word equal to the pet's name (the hub's bot.petName), on load and on rename;
// - reads new replies aloud when the user turned that on, and stops when the user talks or closes the bubble.
// Settings > Voice's wake-word test borrows the engine for a moment: then nothing here reacts.
import { useCallback, useEffect, useRef } from "react";
import type { HubClient, HubState } from "@pawl/hub-client";
import { approvalReadBack, isLocal, machineDisplayName } from "@pawl/hub-client";
import { agentName, errorText } from "../lib/format";
import { notifiesHere } from "../lib/where";
import { speak, speechBusy, stopSpeaking } from "../lib/speech";
import { canTalk, listenPhase, micMarker, syncKeyword, useVoice, voiceBackend, type ListenPhase, type MicMarker, type VoiceEvent } from "../lib/voice";
import { markVoiceSent, wakeTestRunning } from "../lib/voiceShared";

export const DIDNT_CATCH = "I didn't catch that.";

/** The codes of the requests waiting now: never spoken (see stripVoiceCodes). */
function pendingCodes(state: HubState): string[] {
  return Object.values(state.pending)
    .map((p) => p.voiceCode)
    .filter((c): c is string => !!c);
}

/** What the pet window does with a voice event (pure). "cancel": drop the utterance (voice_listen cancel). */
export type VoiceReaction = { kind: "none" } | { kind: "open" } | { kind: "send"; text: string } | { kind: "say"; text: string } | { kind: "cancel" };

/**
 * While the pet talks (and `SPEECH_QUIET_MS` after), the microphone may hear the pet itself. `busy`: that is now.
 * `manual`: the user started this utterance from Pawl's own UI (holding the pet, the mic button), which first
 * stopped the pet's voice: a person, not an echo.
 */
export interface SpeechGuard {
  busy: boolean;
  manual: boolean;
}

const QUIET: SpeechGuard = { busy: false, manual: false };

/**
 * A wake word, or any utterance starting (state -> hearing): open the bubble in listening mode. A final transcript:
 * send it to the chat once per id, or say "I didn't catch that" when it is empty. While Settings tests the wake
 * word, nothing. While the pet is talking: wake words are ignored, an utterance that starts is cancelled (unless the
 * user started it from Pawl's UI) and a transcript is dropped, so the pet can never answer, or approve, by hearing
 * itself.
 */
export function voiceReaction(e: VoiceEvent, seen: Set<string>, testing: boolean, guard: SpeechGuard = QUIET): VoiceReaction {
  if (testing) return { kind: "none" };
  const echo = guard.busy && !guard.manual;
  if (e.type === "wake") return echo ? { kind: "none" } : { kind: "open" };
  if (e.type === "state" && e.state === "hearing") return echo ? { kind: "cancel" } : { kind: "open" };
  if (e.type !== "transcript" || seen.has(e.id)) return { kind: "none" };
  seen.add(e.id);
  if (echo) return { kind: "none" };
  const text = e.text.trim();
  return text ? { kind: "send", text } : { kind: "say", text: DIDNT_CATCH };
}

/** An utterance the user started from Pawl's UI counts as theirs this long (15 s of speech + transcription). */
const MANUAL_MS = 20_000;

export interface VoiceTalk {
  /** Voice is available here and turned on. */
  enabled: boolean;
  marker: MicMarker;
  phase: ListenPhase;
  level: number;
  /** Push-to-talk can start now. */
  ready: boolean;
  /** Why the mic button is unavailable (null: it works). */
  unavailable: string | null;
  /** For the pet's right-click menu: muted or not while voice is on; undefined hides the item. */
  menuMuted: boolean | undefined;
  start(): void;
  stop(): void;
  cancel(): void;
}

export interface VoiceTalkInput {
  client: HubClient;
  state: HubState;
  now: number;
  /** The hub's name for the pet (null until known): the wake word. */
  hubName: string | null;
  bubbleOpen: boolean;
  /** Open the default pet's bubble (listening mode shows by itself from the state). */
  openBubble(): void;
  say(text: string): void;
  chatSent(messageId: string): void;
  speech: { voiceURI: string; rate: number };
}

export function useVoiceTalk(input: VoiceTalkInput): VoiceTalk {
  const latest = useRef(input);
  latest.current = input;
  const sent = useRef(new Set<string>());
  /** When the user last started talking from Pawl's UI (0: not now). */
  const manualAt = useRef(0);

  const onEvent = useCallback((e: VoiceEvent) => {
    const { client, openBubble, say, chatSent } = latest.current;
    const now = Date.now();
    const r = voiceReaction(e, sent.current, wakeTestRunning(), { busy: speechBusy(now), manual: now - manualAt.current < MANUAL_MS });
    if (e.type === "transcript" || (e.type === "state" && (e.state === "idle" || e.state === "wake_listening"))) manualAt.current = 0;
    if (r.kind === "cancel") {
      // Heard while the pet was talking: maybe its own voice. Dropped, never transcribed.
      voiceBackend()?.listen("cancel").catch(() => {});
    } else if (r.kind === "open") {
      stopSpeaking();
      openBubble();
    } else if (r.kind === "say") {
      say(r.text);
    } else if (r.kind === "send") {
      // An ordinary chat message: the same parser and proposals as typed text; it never approves anything by itself.
      client.chat(r.text, undefined, { via: "voice" }).then(
        (m) => {
          chatSent(m.id);
          markVoiceSent(m.id);
        },
        (err: unknown) => say(errorText(err)),
      );
    }
  }, []);
  const voice = useVoice(onEvent);
  const config = voice.status?.config;

  // The wake word follows the pet's name.
  const { configure } = voice;
  useEffect(() => {
    if (!config) return;
    const next = syncKeyword(config, input.hubName);
    if (next) configure({ keyword: next.keyword, spokenAs: next.spokenAs }).catch((err: unknown) => console.warn("[desktop] voice keyword sync failed", err));
  }, [config, input.hubName, configure]);

  // Read new replies aloud (never the history that was there when the window opened).
  const lastSeen = useRef<string | null | undefined>(undefined);
  const lastPet = [...input.state.chat].reverse().find((m) => m.role === "pet" && !m.petId);
  useEffect(() => {
    const id = lastPet?.id ?? null;
    if (lastSeen.current === undefined) {
      lastSeen.current = id;
      return;
    }
    if (id === lastSeen.current) return;
    lastSeen.current = id;
    if (lastPet && config?.enabled && config.readReplies) speak(lastPet.text, { ...latest.current.speech, codes: pendingCodes(latest.current.state) });
  }, [lastPet, config?.enabled, config?.readReplies]);

  // A new approval, read back (voice on, "Read replies aloud" on, and this is where the user is): who wants what,
  // where, and what to say ("Say approve 4 7, or deny."). Talking stops it (see onEvent).
  const seenRequests = useRef<Set<string> | null>(null);
  const pending = input.state.pending;
  useEffect(() => {
    const list = Object.values(pending);
    const ids = new Set(list.map((p) => p.requestId));
    const before = seenRequests.current;
    seenRequests.current = ids;
    if (!before || !config?.enabled || !config.readReplies) return;
    const fresh = list.filter((p) => !before.has(p.requestId)).sort((a, b) => a.createdAt - b.createdAt)[0];
    const st = latest.current.state;
    if (!fresh || !notifiesHere(st)) return;
    speak(
      approvalReadBack(fresh, {
        agentName: agentName(st.agents, fresh.agentId, fresh.machineId),
        machineName: machineDisplayName(st.machines[fresh.machineId] ?? (isLocal(st, fresh.machineId) ? st.machine : undefined)),
        local: isLocal(st, fresh.machineId),
      }),
      { ...latest.current.speech, codes: pendingCodes(st) },
    );
  }, [pending, config?.enabled, config?.readReplies]);

  // Closing the bubble stops the voice.
  useEffect(() => {
    if (!input.bubbleOpen) stopSpeaking();
  }, [input.bubbleOpen]);

  const api = voice.api;
  const listen = useCallback(
    (action: "start" | "stop" | "cancel") => {
      if (action === "start") {
        // The user, from Pawl's UI: this utterance is theirs even right after the pet spoke.
        manualAt.current = Date.now();
        stopSpeaking();
      }
      api?.listen(action).catch((err: unknown) => latest.current.say(errorText(err)));
    },
    [api],
  );

  const enabled = !!voice.available && !!config?.enabled;
  const ready = enabled && canTalk(voice);
  return {
    enabled,
    marker: voice.available ? micMarker(voice, input.now) : null,
    phase: enabled ? listenPhase(voice.state) : null,
    level: voice.level,
    ready,
    unavailable: !enabled
      ? "Turn on voice in Settings > Voice."
      : voice.state === "muted"
        ? "The microphone is muted."
        : ready || voice.state === "hearing"
          ? null
          : "Voice is getting ready…",
    menuMuted: enabled ? config.muted : undefined,
    start: () => listen("start"),
    stop: () => listen("stop"),
    cancel: () => listen("cancel"),
  };
}
