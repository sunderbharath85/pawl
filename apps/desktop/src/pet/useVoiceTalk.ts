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
import { approvalReadBack, isAssistantReply, isLocal, isLookRequest, lookQuestion, machineDisplayName, stripApprovalWords } from "@pawl/hub-client";
import { agentName, errorText } from "../lib/format";
import { notifiesHere } from "../lib/where";
import { setSpeechPause, speak, speechBusy, stopSpeaking } from "../lib/speech";
import { canTalk, listenPhase, micMarker, syncKeyword, useVoice, voiceBackend, type HearingTrigger, type ListenPhase, type MicMarker, type VoiceEvent, type VoiceState } from "../lib/voice";
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
 * `utterance`: what started the utterance in progress (from its `hearing` event).
 */
export interface SpeechGuard {
  busy: boolean;
  utterance?: HearingTrigger | undefined;
}

const QUIET: SpeechGuard = { busy: false };

/**
 * A wake word, or any utterance starting (state -> hearing): open the bubble in listening mode. A final transcript:
 * send it to the chat once per id, or say "I didn't catch that" when it is empty. While Settings tests the wake
 * word, nothing.
 *
 * While the pet talks, the engine is paused (no wake words; it cancels wake-started utterances itself). As a second
 * line: a wake word is ignored, an utterance the wake word started is cancelled and its transcript dropped, so the
 * pet can never answer, or approve, by hearing itself. Push-to-talk from Pawl's UI or the hotkey is the user: it is
 * kept, and the pet stops talking instead.
 */
export function voiceReaction(e: VoiceEvent, seen: Set<string>, testing: boolean, guard: SpeechGuard = QUIET): VoiceReaction {
  if (testing) return { kind: "none" };
  if (e.type === "wake") return guard.busy ? { kind: "none" } : { kind: "open" };
  if (e.type === "state" && e.state === "hearing") return guard.busy && e.trigger === "wake" ? { kind: "cancel" } : { kind: "open" };
  if (e.type !== "transcript" || seen.has(e.id)) return { kind: "none" };
  seen.add(e.id);
  if (guard.busy && guard.utterance === "wake") return { kind: "none" };
  const text = e.text.trim();
  return text ? { kind: "send", text } : { kind: "say", text: DIDNT_CATCH };
}

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
  /** "Look at this" said aloud: open the screen picker with this question (undefined: no screen access here). */
  look?: ((question: string) => void) | undefined;
  speech: { voiceURI: string; rate: number };
}

/**
 * What to say for a reply: the brain's short spoken form (`speech`: an assistant reply to a spoken message, on its
 * final update; no code, markdown or links) when there is one, else the text.
 */
export function spokenReply(m: { text: string; speech?: string | undefined }): string {
  return m.speech?.trim() || m.text;
}

/**
 * What the pet says for a reply. An assistant reply (`assistant`) loses the words approve / deny / allow and standalone
 * 1-2 digit numbers first: its text may come from a web page or a file, and must never sound like an answer to a card.
 */
export function replySpeech(m: { text: string; speech?: string | undefined }, assistant: boolean): string {
  const said = spokenReply(m);
  return assistant ? stripApprovalWords(said) : said;
}

/**
 * Read an assistant reply aloud only once the voice engine is paused (fail closed): the pause is asked for first, and
 * if `voice_pause` fails the reply is not read (it stays in the chat) and `onFail` is told. If the user started talking
 * meanwhile, or nothing was left to say, the engine is resumed and nothing is said. Resolves true when it was read.
 */
export async function speakWithEnginePaused(
  say: () => void,
  deps: { pause(on: boolean): Promise<void>; mayStart(): boolean; busy(): boolean; onFail(err: unknown): void },
): Promise<boolean> {
  try {
    await deps.pause(true);
  } catch (err) {
    deps.onFail(err);
    return false;
  }
  if (!deps.mayStart()) {
    deps.pause(false).catch(() => {});
    return false;
  }
  say();
  // Speech resumes the engine itself after its quiet time; if nothing was said, resume it now.
  if (!deps.busy()) {
    deps.pause(false).catch(() => {});
    return false;
  }
  return true;
}

/** A logger that writes only its first message (a failing voice_pause is reported once, not on every reply). */
export function logOnce(log: (...args: unknown[]) => void): (...args: unknown[]) => void {
  let done = false;
  return (...args) => {
    if (done) return;
    done = true;
    log(...args);
  };
}

/**
 * The pet only starts talking while the user is not: during an utterance (wake word or push-to-talk, hearing or
 * transcribing) the microphone would record its voice into what the user says. The reply stays in the chat.
 */
export function mayTalkNow(state: VoiceState | undefined): boolean {
  return state !== "hearing" && state !== "transcribing";
}

/** The default pet's replies this window knows: every id seen, and the ones still streaming. */
export interface ReplyReader {
  seen: ReadonlySet<string>;
  streaming: ReadonlySet<string>;
}

/**
 * The default pet's replies to read aloud now (ids, in chat order), and what to remember. Never the history there
 * when the window opened (`reader` null). A new reply is read when it arrives finished; a streamed one (Assistant
 * mode) once, when its `streaming` ends, even if a later reply ("I'm still on your last message") came meanwhile.
 */
export function repliesToRead(
  replies: readonly { id: string; streaming?: boolean | undefined }[],
  reader: ReplyReader | null,
): { read: string[]; reader: ReplyReader } {
  if (!reader) return { read: [], reader: { seen: new Set(replies.map((r) => r.id)), streaming: new Set() } };
  const seen = new Set(reader.seen);
  const streaming = new Set(reader.streaming);
  const read: string[] = [];
  for (const r of replies) {
    if (!seen.has(r.id)) {
      seen.add(r.id);
      if (r.streaming) streaming.add(r.id);
      else read.push(r.id);
    } else if (streaming.has(r.id) && !r.streaming) {
      streaming.delete(r.id);
      read.push(r.id);
    }
  }
  // Keep the memory small: ids of replies no longer in the chat are dropped.
  const present = new Set(replies.map((r) => r.id));
  for (const id of seen) if (!present.has(id)) seen.delete(id);
  for (const id of streaming) if (!present.has(id)) streaming.delete(id);
  return { read, reader: { seen, streaming } };
}

export function useVoiceTalk(input: VoiceTalkInput): VoiceTalk {
  const latest = useRef(input);
  latest.current = input;
  const sent = useRef(new Set<string>());
  /** What started the utterance in progress (its `hearing` event), until its transcript or the end of it. */
  const utterance = useRef<HearingTrigger | undefined>(undefined);

  const onEvent = useCallback((e: VoiceEvent) => {
    const { client, openBubble, say, chatSent } = latest.current;
    if (e.type === "state" && e.state === "hearing") utterance.current = e.trigger;
    const r = voiceReaction(e, sent.current, wakeTestRunning(), { busy: speechBusy(), utterance: utterance.current });
    if (e.type === "transcript" || (e.type === "state" && (e.state === "idle" || e.state === "wake_listening" || e.state === "muted"))) utterance.current = undefined;
    if (r.kind === "cancel") {
      // The wake word while the pet was talking: maybe its own voice. Dropped, never transcribed.
      voiceBackend()?.listen("cancel").catch(() => {});
    } else if (r.kind === "open") {
      stopSpeaking();
      openBubble();
    } else if (r.kind === "say") {
      say(r.text);
    } else if (r.kind === "send" && latest.current.look && isLookRequest(r.text)) {
      latest.current.look(lookQuestion(r.text));
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
  /** The user is talking right now (an utterance in progress): the pet must not start reading anything aloud. */
  const userTalking = useRef(false);
  userTalking.current = !mayTalkNow(voice.state);
  /** The engine's pause (voice_pause): an assistant reply is read only after it worked. */
  const engine = useRef(voice.api);
  engine.current = voice.api;
  const warnPause = useRef(logOnce((...args) => console.warn(...args)));

  // The wake word follows the pet's name.
  const { configure } = voice;
  useEffect(() => {
    if (!config) return;
    const next = syncKeyword(config, input.hubName);
    if (next) configure({ keyword: next.keyword, spokenAs: next.spokenAs }).catch((err: unknown) => console.warn("[desktop] voice keyword sync failed", err));
  }, [config, input.hubName, configure]);

  // Read new replies aloud (never the history that was there when the window opened; a streamed one once it is done).
  const reader = useRef<ReplyReader | null>(null);
  const chat = input.state.chat;
  useEffect(() => {
    const replies = chat.filter((m) => m.role === "pet" && !m.petId);
    const next = repliesToRead(replies, reader.current);
    reader.current = next.reader;
    // Several at once (rare): the newest one.
    const id = next.read[next.read.length - 1];
    const reply = id ? replies.find((m) => m.id === id) : undefined;
    if (!reply || !config?.enabled || !config.readReplies || userTalking.current) return;
    const st = latest.current.state;
    const assistant = isAssistantReply(reply, st);
    const text = replySpeech(reply, assistant);
    const opts = { ...latest.current.speech, codes: pendingCodes(st) };
    if (!assistant) {
      speak(text, opts);
      return;
    }
    // An assistant reply (its words may come from a web page): read only once the microphone is paused. No engine to
    // pause, or the pause failed: shown in the chat only.
    const api = engine.current;
    if (!api) return;
    void speakWithEnginePaused(() => speak(text, opts), {
      pause: (on) => api.pause(on),
      mayStart: () => !userTalking.current,
      busy: () => speechBusy(),
      onFail: (err) => warnPause.current("[desktop] voice_pause failed: assistant replies are shown, not read aloud, until it works", err),
    });
  }, [chat, config?.enabled, config?.readReplies]);

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
    if (!fresh || !notifiesHere(st) || userTalking.current) return;
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

  // While the pet speaks (and about a second after), the engine is paused so its own voice can't wake it or be heard
  // as an answer (lib/speech.ts).
  const api = voice.api;
  const voiceOn = !!voice.available && !!config?.enabled;
  useEffect(() => {
    if (!api || !voiceOn) return;
    setSpeechPause((on) => {
      api.pause(on).catch((err: unknown) => warnPause.current("[desktop] voice_pause failed", err));
    });
    return () => setSpeechPause(null);
  }, [api, voiceOn]);

  const listen = useCallback(
    (action: "start" | "stop" | "cancel") => {
      if (action === "start") stopSpeaking();
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
