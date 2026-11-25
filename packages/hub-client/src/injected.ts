// Text an agent harness injects into a user turn (Claude Code and friends), told apart from what the person typed.
// A hook's `UserPromptSubmit.prompt` carries the person's words plus things like background-task notifications,
// system reminders, messages from other terminals and pasted blocks. Nothing is hidden: every block taken out of the
// person's words is shown as its own notice or attachment (collapsed). Pure, no DOM.

export type PromptAttachment =
  /** A block of pasted text, shown collapsed. */
  | { kind: "pasted"; text: string }
  /** An image the person attached ("[Image #40]"); only its label is known here. */
  | { kind: "image"; label: string };

export interface PromptNotice {
  /** "task": a background task finished; "message": from another terminal or agent; "injected": other harness text. */
  kind: "task" | "message" | "injected";
  text: string;
  /** The full body, shown collapsed. */
  detail?: string | undefined;
}

export interface ParsedPrompt {
  /** What the person wrote, without the injected parts (trimmed; may be empty). */
  text: string;
  attachments: PromptAttachment[];
  /** Injected parts, each shown as a line of its own, in order. */
  notices: PromptNotice[];
}

const BLOCK_RE = /<(system-reminder|task-notification|hc|pasted_content)\b([^<>]*)>/gi;
const IMAGE_RE = /\[Image #(\d+)\]/g;
/** The hub cuts long strings: "…" (titles) or "…[truncated N chars]" (raw payloads). */
const TRUNCATED_RE = /…(\[truncated \d+ chars\])?\s*$/;

function attr(attrs: string, name: string): string | null {
  const m = new RegExp(`${name}\\s*=\\s*"([^"]*)"`, "i").exec(attrs) ?? new RegExp(`${name}\\s*=\\s*'([^']*)'`, "i").exec(attrs);
  return m ? m[1]! : null;
}

function tag(body: string, name: string): string | null {
  const m = new RegExp(`<${name}>([\\s\\S]*?)(?:</${name}>|$)`, "i").exec(body);
  const v = m?.[1]?.trim();
  return v ? v : null;
}

function oneLine(text: string, max = 140): string {
  const t = text.replace(/\s+/g, " ").trim();
  return t.length > max ? `${t.slice(0, max - 1)}…` : t;
}

/** A tagged body as readable lines ("<status>done</status>" -> "status: done"). */
function untag(body: string): string {
  return body
    .replace(/<([a-z_-]+)>([\s\S]*?)<\/\1>/gi, (_m, k: string, v: string) => `${k}: ${v.trim()}\n`)
    .replace(/\n{2,}/g, "\n")
    .trim();
}

/** "Background task finished: …" from a `<task-notification>` body. */
function taskNotice(body: string): PromptNotice {
  const status = (tag(body, "status") ?? "").toLowerCase();
  const summary = tag(body, "summary");
  const failed = status === "failed" || status === "error" || status === "killed";
  const head = failed ? "Background task failed" : status === "running" ? "Background task update" : "Background task finished";
  const detail = untag(body);
  return { kind: "task", text: summary ? `${head}: ${oneLine(summary)}` : head, ...(detail ? { detail } : null) };
}

/**
 * Split a prompt into the person's words, attachments and injected notices. Only complete blocks are taken out:
 * `<system-reminder>` becomes a collapsed "Injected text" notice, `<task-notification>` "Background task finished:
 * <summary>", `<hc from="…">` "Message from <name>" (body collapsed), `<pasted_content>` a collapsed pasted-text
 * attachment, and `[Image #N]` an image chip. An opening tag without its closing tag stays in the text as typed,
 * unless the text was cut off by the hub ("…" at the end): then the block runs to the end, and is still shown.
 */
export function parsePrompt(raw: string): ParsedPrompt {
  const input = raw ?? "";
  const truncated = TRUNCATED_RE.test(input);
  const attachments: PromptAttachment[] = [];
  const notices: PromptNotice[] = [];
  let out = "";
  let pos = 0;
  BLOCK_RE.lastIndex = 0;
  for (;;) {
    BLOCK_RE.lastIndex = pos;
    const m = BLOCK_RE.exec(input);
    if (!m) break;
    const name = m[1]!.toLowerCase();
    const attrs = m[2] ?? "";
    const bodyStart = m.index + m[0].length;
    const closeRe = new RegExp(`</${name}\\s*>`, "gi");
    closeRe.lastIndex = bodyStart;
    const close = closeRe.exec(input);
    if (!close && !truncated) {
      // Not a block: keep the tag as text and look further on.
      out += input.slice(pos, bodyStart);
      pos = bodyStart;
      continue;
    }
    out += input.slice(pos, m.index);
    const body = close ? input.slice(bodyStart, close.index) : input.slice(bodyStart);
    pos = close ? close.index + close[0].length : input.length;
    const cut = close ? "" : " (cut off)";
    switch (name) {
      case "system-reminder": {
        const detail = body.trim();
        notices.push({ kind: "injected", text: `Injected text${cut}`, ...(detail ? { detail } : null) });
        break;
      }
      case "task-notification":
        notices.push(taskNotice(body));
        break;
      case "hc": {
        const from = (attr(attrs, "from") ?? "").trim();
        const detail = body.trim();
        notices.push({ kind: "message", text: (from ? `Message from ${from}` : "Message from another terminal") + cut, ...(detail ? { detail } : null) });
        break;
      }
      case "pasted_content": {
        const text = body.replace(/^\n+|\n+$/g, "");
        if (text.trim()) attachments.push({ kind: "pasted", text });
        break;
      }
    }
    if (!close) break;
  }
  out += input.slice(pos);
  out = out.replace(IMAGE_RE, (_all, n: string) => {
    attachments.push({ kind: "image", label: `Image #${n}` });
    return "";
  });
  // Tidy what is left: no spaces at line ends, no runs of blank lines (indentation inside lines is kept).
  const text = out
    .split("\n")
    .map((l) => l.replace(/[ \t]+$/g, ""))
    .join("\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
  return { text, attachments, notices };
}

export const TITLE_MAX = 80;

/**
 * A session title fit to show: injected blocks removed (the same rules as `parsePrompt`), one line, at most TITLE_MAX
 * characters. When nothing of the person's words is left, "<agent> in <repo>".
 */
export function cleanTitle(text: string | null | undefined, fallback: { agent: string; repo: string }): string {
  const t = oneLine(parsePrompt(text ?? "").text, TITLE_MAX);
  return t && t !== "…" ? t : `${fallback.agent} in ${fallback.repo}`;
}

/** "api" for "E:/work/api/" (the folder a session runs in); "~" when unknown. */
export function folderName(cwd: string | null | undefined): string {
  // An outside A2A agent's session works where the agent runs: its "folder" is the agent's URL; show its host.
  const url = /^[a-z][a-z0-9+.-]*:\/\/([^/?#]+)/i.exec(cwd ?? "");
  if (url) return url[1]!;
  const parts = (cwd ?? "").replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || "~";
}
