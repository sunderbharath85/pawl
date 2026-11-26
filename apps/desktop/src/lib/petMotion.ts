// When the pet on screen moves (pure). By default they stay still in a calm pose: agent activity shows as a static
// badge or "!" marker, the bubble and notifications, never as walking, digging or barking. A pet animates only while
// the person talks to it in its own bubble: a thinking pose while the reply is on its way, one short happy reaction
// when it arrives, then still again. "Always" brings back the old behaviour (the pose follows what agents do).
// prefers-reduced-motion keeps every pet still.
import type { PetActivity } from "@pawl/pet-engine";

export type PetAnimationMode = "chat" | "always";

export const PET_ANIMATION_LABEL: Record<PetAnimationMode, string> = {
  chat: "Only while we chat",
  always: "Always (shows what agents are doing)",
};

/** The reaction to a reply: one short cycle. */
export const REACT_MS = 1600;
/** Give up waiting for the default pet's reply after this long (the bot answers in seconds). */
export const CHAT_WAIT_MAX_MS = 60_000;
/** Give up waiting for a session's first reply after this long (an agent can think for a while). */
export const SESSION_WAIT_MAX_MS = 5 * 60_000;

/** Where a conversation in a pet's bubble is: nothing going on, waiting for the reply, or reacting to it. */
export type ChatPhase = "idle" | "waiting" | "reacting";

function phaseAfter(sentAt: number | undefined, replyAt: number | undefined, now: number, waitMax: number): ChatPhase {
  if (sentAt === undefined) return "idle";
  if (replyAt === undefined) return now - sentAt < waitMax ? "waiting" : "idle";
  return now - replyAt < REACT_MS ? "reacting" : "idle";
}

/**
 * The default pet's chat: the latest message the person sent from this bubble (`sentIds`), and the bot's first reply
 * after it (messages in order). Assistant mode streams the reply into one message: the pet keeps thinking while it is
 * `streaming` (up to SESSION_WAIT_MAX_MS, an agent can work a while) and reacts when it is finished (`doneAt`: when
 * this window saw it finish; else the message's own time).
 */
export function chatPhase(
  messages: readonly { id: string; role: "user" | "pet"; ts: number; streaming?: boolean | undefined }[],
  sentIds: ReadonlySet<string>,
  now: number,
  doneAt?: ReadonlyMap<string, number>,
): ChatPhase {
  // A reply still streaming for a message sent from here keeps the pet thinking, even when a later message got a quick
  // answer meanwhile (Assistant mode: "I'm still on your last message").
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i]!;
    if (m.role !== "pet" || !m.streaming) continue;
    const asked = messages
      .slice(0, i)
      .reverse()
      .find((u) => u.role === "user" && sentIds.has(u.id));
    if (asked && now - asked.ts < SESSION_WAIT_MAX_MS) return "waiting";
    break;
  }
  let sent = -1;
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i]!.role === "user" && sentIds.has(messages[i]!.id)) {
      sent = i;
      break;
    }
  }
  if (sent < 0) return "idle";
  const reply = messages.slice(sent + 1).find((m) => m.role === "pet");
  if (reply?.streaming) return phaseAfter(messages[sent]!.ts, undefined, now, SESSION_WAIT_MAX_MS);
  return phaseAfter(messages[sent]!.ts, reply ? (doneAt?.get(reply.id) ?? reply.ts) : undefined, now, CHAT_WAIT_MAX_MS);
}

/** What counts as a session's answer to a prompt: its first words, the end of the turn, or an error. */
const REPLY_EVENTS = new Set(["agent_message_chunk", "stop", "error"]);

/**
 * A session picked in the bubble's switcher: the person prompted it from the bubble at `sentAt`; the agent's first reply after it ends
 * the wait. (A small allowance for clock differences between this machine and the session's.)
 */
export function sessionPhase(events: readonly { type: string; ts: number }[], sentAt: number | undefined, now: number): ChatPhase {
  if (sentAt === undefined) return "idle";
  const reply = events.find((e) => e.ts >= sentAt - 1000 && REPLY_EVENTS.has(e.type));
  return phaseAfter(sentAt, reply?.ts, now, SESSION_WAIT_MAX_MS);
}

export interface PetPose {
  activity: PetActivity;
  /** False: the pet is drawn still (no CSS animation at all). */
  animate: boolean;
}

/** The pose and whether it moves, from the setting, the system's reduced-motion wish and the chat. */
export function petPose(input: { mode: PetAnimationMode; reducedMotion: boolean; agentActivity: PetActivity; phase: ChatPhase }): PetPose {
  const { mode, reducedMotion, agentActivity, phase } = input;
  if (mode === "always") return { activity: agentActivity, animate: !reducedMotion };
  switch (phase) {
    case "waiting":
      return { activity: "thinking", animate: !reducedMotion };
    case "reacting":
      return { activity: "happy", animate: !reducedMotion };
    case "idle":
      return { activity: "idle", animate: false };
  }
}
