// Voice approvals (v0.2 item 3), the pure part for every client: the two-digit code each request carries (shown as
// "say: approve 4 7"), whether a spoken approval needs it, the request read back aloud (plain words: no markdown, no
// emoji), and how a decision made by voice is labelled. The hub enforces the rules; this only words them.
import type { PermissionRequest, PermissionResolvedPayload, ToolKind } from "@pawl/protocol";

/** "47" -> "4 7" (each digit on its own, as it is said); null when there is no usable code. */
export function formatVoiceCode(code: string | null | undefined): string | null {
  const digits = (code ?? "").replace(/\D/g, "");
  return digits ? digits.split("").join(" ") : null;
}

/** The chip on an approval card: "say: approve 4 7" (null without a code). */
export function voiceCodeChipText(code: string | null | undefined): string | null {
  const c = formatVoiceCode(code);
  return c ? `say: approve ${c}` : null;
}

/** Kinds a spoken "approve" may answer on this machine after the read-back; everything else needs the code. */
const PLAIN_APPROVE: ReadonlySet<ToolKind> = new Set<ToolKind>(["read", "edit"]);

/**
 * Whether a spoken approval must include the code: always for execute, delete, fetch and any other kind, and for every
 * request from another machine; reads and edits on this machine need only "approve" (after the read-back).
 */
export function voiceCodeNeeded(req: Pick<PermissionRequest, "toolCall">, local: boolean): boolean {
  return !local || !PLAIN_APPROVE.has(req.toolCall.kind ?? "other");
}

// Emoji and pictographs, with their joiners and variation selectors.
const EMOJI = /[\p{Extended_Pictographic}\u{1F1E6}-\u{1F1FF}\u{1F3FB}-\u{1F3FF}‍︎️⃣]/gu;

/** Text as it should be spoken: no markdown marks, no code fences, no emoji, link text without its address, one line. */
export function spokenPlain(text: string): string {
  return text
    .replace(/```[\s\S]*?```/g, " ")
    .replace(/`([^`]*)`/g, "$1")
    .replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/https?:\/\/(\S+)/g, (_m, rest: string) => rest.split(/[/?#]/)[0] ?? rest)
    .replace(/(\*\*|__|\*|_|~~)(?=\S)([^*_~]+?)\1/g, "$2")
    .replace(/^\s{0,3}(#{1,6}|>|[-*+]|\d+[.)])\s+/gm, "")
    .replace(EMOJI, "")
    .replace(/\s+/g, " ")
    .trim();
}

type Obj = Record<string, unknown>;
const asObj = (v: unknown): Obj => (v && typeof v === "object" && !Array.isArray(v) ? (v as Obj) : {});
const text = (v: unknown): string | undefined => (typeof v === "string" && v.trim() ? v.trim() : undefined);

/** At most this many characters of a command or title are read out ("…and more" after). */
export const READ_BACK_MAX = 90;

function clipSpoken(s: string, max = READ_BACK_MAX): string {
  if (s.length <= max) return s;
  const cut = s.slice(0, max);
  const at = cut.lastIndexOf(" ");
  return `${(at > max * 0.6 ? cut.slice(0, at) : cut).trim()}, and more`;
}

/** The last two parts of a path ("src/app.ts"): enough to recognise it, short enough to say. */
function shortPath(p: string): string {
  const parts = p.replace(/\\/g, "/").replace(/\/+$/, "").split("/").filter(Boolean);
  return parts.slice(-2).join("/") || p;
}

/** What the request would do, in a few words ("run bun test", "edit src/app.ts", "open example.com"). */
export function requestAction(req: Pick<PermissionRequest, "toolCall">): string {
  const tc = req.toolCall;
  const raw = tc.rawInput;
  const args = asObj(raw);
  const command = typeof raw === "string" ? text(raw) : (text(args.command) ?? text(args.cmd));
  const file = text(args.file_path) ?? text(args.filePath) ?? text(args.path) ?? text(args.notebook_path) ?? text(tc.locations?.[0]?.path);
  const url = text(args.url);
  const title = text(tc.title);
  const say = (s: string) => clipSpoken(spokenPlain(s));
  switch (tc.kind) {
    case "execute":
      return `run ${say(command ?? title ?? "a command")}`;
    case "edit":
      return file ? `edit ${shortPath(file)}` : say(title ?? "edit a file");
    case "read":
      return file ? `read ${shortPath(file)}` : say(title ?? "read a file");
    case "delete":
      return file ? `delete ${shortPath(file)}` : say(title ?? "delete files");
    case "move":
      return file ? `move ${shortPath(file)}` : say(title ?? "move files");
    case "fetch":
      return url ? `open ${spokenPlain(url)}` : say(title ?? "reach the network");
    default:
      return say(title ?? command ?? "use a tool");
  }
}

export interface ReadBackContext {
  /** "Claude Code". */
  agentName: string;
  /** The machine the request runs on ("Desk PC"). */
  machineName: string;
  /** It runs on this machine. */
  local: boolean;
}

/**
 * Remove voice codes from text that is about to be spoken: the pet's own voice must never say a code, or the
 * microphone could hear "approve 4 7" and approve the request by itself. Removes every code in `codes` (however it
 * is spaced or punctuated), any digits right after "approve", and any spaced digit pair ("4 7", the way codes are
 * written for people). Other numbers stay.
 */
export function stripVoiceCodes(text: string, codes: Iterable<string | null | undefined> = []): string {
  let out = text;
  for (const c of codes) {
    const digits = (c ?? "").replace(/\D/g, "");
    if (digits.length < 2) continue;
    out = out.replace(new RegExp(`(?<!\\d)${digits.split("").join("[\\s,.\\-]*")}(?!\\d)`, "g"), " ");
  }
  return out
    .replace(/\b(approve)\b[\s,:]*(?:\d[\s,.\-]*)+/gi, "$1 ")
    .replace(/(?<![\d.])\b\d +\d\b(?![\d.])/g, " ")
    .replace(/\s+([,.!?])/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
}

/**
 * The request read back aloud. The code is NEVER spoken (the microphone could hear the pet say it):
 * "Claude Code wants to run bun test on Desk PC. Say approve with the code on the card, or deny." When plain
 * "approve" is enough (a read or edit on this machine), or the request has no code: "Say approve, or deny."
 * "Always allow" is never offered. Denying needs no code.
 */
export function approvalReadBack(req: Pick<PermissionRequest, "toolCall" | "voiceCode">, ctx: ReadBackContext): string {
  const who = spokenPlain(ctx.agentName) || "An agent";
  const where = spokenPlain(ctx.machineName);
  const code = formatVoiceCode(req.voiceCode);
  const answer = code && voiceCodeNeeded(req, ctx.local) ? "Say approve with the code on the card, or deny." : "Say approve, or deny.";
  return stripVoiceCodes(`${who} wants to ${requestAction(req)}${where ? ` on ${where}` : ""}. ${answer}`, [req.voiceCode]);
}

/** "approved by voice" / "denied by a rule" / "approved": the outcome with how it was given. */
export function decisionText(outcome: "approved" | "denied" | "cancelled", p: Pick<Partial<PermissionResolvedPayload>, "by" | "via">): string {
  const word = outcome === "cancelled" ? "withdrawn" : outcome;
  if (outcome !== "cancelled" && p.via === "voice") return `${word} by voice`;
  if (outcome !== "cancelled" && p.by === "rule") return `${word} by a rule`;
  return word;
}
