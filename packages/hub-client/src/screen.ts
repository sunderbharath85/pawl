// Look at the screen (v0.2 item 4), the pure part: where a picture would go with the bot's current provider, whether
// a session can take a picture with a prompt, and the "look at this" phrase. Every capture is the user's choice at
// that moment (pick a source, see the preview, press Send); the picture is used once and dropped.
import type { Agent, HubSettings, MachineId, Session } from "@pawl/protocol";
import { botProviderNow } from "./settings";

/** The question sent with a picture when the user gives none. */
export const LOOK_DEFAULT_QUESTION = "What do you see?";

/** The one-line warning in the picker. */
export const CAPTURE_WARNING = "A capture shows everything in it, including passwords or private messages. Check the preview before you send it.";

export interface PictureDestination {
  /** "to Claude Code (Anthropic's cloud, through your plan)" / "to OpenAI with your key" / "nowhere: …". */
  text: string;
  /** A provider is set up that may look at pictures (Send makes sense). */
  canSee: boolean;
  /** The picture leaves this PC (a cloud provider or agent, or one Pawl can't vouch for). */
  leavesPc: boolean;
  /** What happens to it afterwards: Pawl keeps nothing; the receiver may. */
  copy: string;
}

const NO_COPY = "Pawl keeps no copy.";
const SERVICE_COPY = "Pawl keeps no copy; the service may keep it under its own terms.";
const AGENT_COPY = "Pawl keeps no copy; the agent may keep it in its own history.";

/**
 * The host of an Ollama URL as the hub reads it (its `ollama_chat_url`): "http://" is added when the URL has no
 * scheme ("192.168.1.5:11434", "gpu-box:11434"). Null when it is not a URL.
 */
export function ollamaHost(url: string | null | undefined): string | null {
  const t = (url ?? "").trim();
  if (!t) return null;
  try {
    return new URL(/^[a-z][a-z0-9+.-]*:\/\//i.test(t) ? t : `http://${t}`).hostname.toLowerCase() || null;
  } catch {
    return null;
  }
}

/** This PC: localhost, 127.0.0.0/8 and ::1. Anything else (a LAN address, another host name) is not. */
export function isThisPcHost(host: string | null | undefined): boolean {
  if (!host) return false;
  const h = host.replace(/^\[|\]$/g, "");
  return h === "localhost" || h === "::1" || /^127(?:\.\d{1,3}){3}$/.test(h);
}

/** Coding agents that send what they get to their maker's cloud (through the user's plan). */
const AGENT_CLOUD: Record<string, string> = {
  "claude-code": "Anthropic",
  "claude-acp": "Anthropic",
  codex: "OpenAI",
  "codex-acp": "OpenAI",
  gemini: "Google",
  copilot: "GitHub",
  "github-copilot-cli": "GitHub",
};

/**
 * Where a picture given to an agent goes: a known cloud agent's maker ("to Claude Code (Anthropic's cloud, through
 * your plan)"); any other agent may send it anywhere ("to My Agent (may leave this PC)").
 */
export function agentPictureDestination(agentId: string, agentName: string): PictureDestination {
  const cloud = AGENT_CLOUD[agentId];
  return {
    text: cloud ? `to ${agentName} (${cloud}'s cloud, through your plan)` : `to ${agentName} (may leave this PC)`,
    canSee: true,
    leavesPc: true,
    copy: AGENT_COPY,
  };
}

/** Where a screen capture sent to the default pet goes, from the bot's provider right now (Settings > Bot). */
export function pictureDestination(
  bot: Partial<HubSettings["bot"]>,
  agents: readonly Pick<Agent, "id" | "name" | "installed" | "machineId">[],
  machineId?: MachineId,
): PictureDestination {
  const now = botProviderNow(bot, agents, machineId);
  switch (now.provider) {
    case "acp": {
      const id = bot.acpAgent || "claude-code";
      const name = agents.find((a) => a.id === id)?.name?.trim() || (id === "claude-code" ? "Claude Code" : id);
      return agentPictureDestination(id, name);
    }
    case "anthropic":
      return { text: "to Anthropic with your key", canSee: true, leavesPc: true, copy: SERVICE_COPY };
    case "openai":
      return { text: "to OpenAI with your key", canSee: true, leavesPc: true, copy: SERVICE_COPY };
    case "gemini":
      return { text: "to Google Gemini with your key", canSee: true, leavesPc: true, copy: SERVICE_COPY };
    case "ollama": {
      const host = ollamaHost(bot.ollamaUrl);
      const local = isThisPcHost(host);
      const model = bot.ollamaModel ? ` (${bot.ollamaModel}, if it can see pictures)` : "";
      return {
        text: local ? `to Ollama on this PC${model}` : `to Ollama at ${host ?? (bot.ollamaUrl ?? "").trim()}${model}`,
        canSee: true,
        leavesPc: !local,
        copy: local ? NO_COPY : SERVICE_COPY,
      };
    }
    default:
      return { text: "nowhere: your model can't see pictures", canSee: false, leavesPc: false, copy: NO_COPY };
  }
}

/**
 * Why a picture can't go with a prompt to this session, or null when it can: only Pawl's own, open sessions on this
 * machine whose agent takes pictures (`promptImage`). A picture never goes to another machine.
 */
export function imageAttachProblem(
  session: Pick<Session, "machineId" | "agentId" | "source" | "endedAt">,
  agents: readonly Pick<Agent, "id" | "name" | "machineId" | "capabilities">[],
  local: boolean,
): string | null {
  if (!local) return "Pictures never go to another machine.";
  if (session.source !== "owned") return "Only sessions Pawl started take pictures.";
  if (session.endedAt) return "This session has ended.";
  const agent = agents.find((a) => a.id === session.agentId && a.machineId === session.machineId) ?? agents.find((a) => a.id === session.agentId);
  const name = agent?.name?.trim() || session.agentId;
  if (agent?.capabilities?.promptImage === true) return null;
  if (!agent?.capabilities) return `${name} hasn't said yet whether it takes pictures.`;
  return `${name} can't take pictures.`;
}

/**
 * The request has to start the message (after an optional "Hey Mochi," / "Mochi:" and "please" / "can you"), so "I'll
 * look at this later" is still an ordinary message.
 */
const LOOK_RE =
  /^\s*(?:(?:hey|hi|ok|okay)\s+)?(?:[\p{L}\p{N}'-]+\s*[,!:]\s*)?(?:(?:please|pls|can you|could you|would you)\s+)?(?:(?:take a\s+)?look at (?:this|that|it|my screen|the screen)|see my screen|(?:let me )?show you my screen|what(?:'s| is) on my screen)\b/iu;

/** "look at this" (typed or said): open the screen picker instead of sending the text. */
export function isLookRequest(text: string): boolean {
  return LOOK_RE.test(text);
}

const FILLER = new Set(["please", "pls", "can", "could", "would", "will", "you", "now", "quick", "quickly", "take", "a", "hey", "hi", "ok", "okay", "and"]);

/**
 * The question that goes with the picture: the default when the text is only the phrase ("Hey Mochi, look at this"),
 * else the text itself ("look at this error: what's wrong?").
 */
export function lookQuestion(text: string): string {
  // The phrase (with its address and "please") is not part of the question.
  const rest = text
    .replace(LOOK_RE, " ")
    .replace(/[^\p{L}\p{N}']+/gu, " ")
    .trim();
  const words = rest.split(" ").filter((w) => w && !FILLER.has(w.toLowerCase()));
  return words.length === 0 ? LOOK_DEFAULT_QUESTION : text.trim();
}

/** Base64 length -> bytes (for the 4 MiB cap). */
export function base64Bytes(data: string): number {
  const len = data.replace(/\s/g, "").length;
  const pad = data.endsWith("==") ? 2 : data.endsWith("=") ? 1 : 0;
  return Math.floor((len * 3) / 4) - pad;
}

/** At most this many bytes in a picture (the hub refuses more). */
export const IMAGE_MAX_BYTES = 4 * 1024 * 1024;
