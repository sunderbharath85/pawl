// A session's events as a conversation a person can read (pure, no DOM): what they asked, what the agent said and
// thought, the tools it ran (input and output), its plan, permission requests and short notices. Owned (ACP)
// sessions map from their structured events; hook sessions (Claude Code, Codex, Copilot in a terminal or editor)
// from the agent's own hook payloads (`HookPayload.raw`). Text the harness injected into a user turn is split off
// (see `parsePrompt`). Nothing is ever shown as raw JSON except a tool's own input or output.
import {
  isTruncatedPayload,
  type HookPayload,
  type HubEvent,
  type PermissionOption,
  type PermissionResolvedPayload,
  type PlanEntry,
  type Session,
  type ToolKind,
} from "@pawl/protocol";
import { parsePrompt, type PromptAttachment } from "./injected";

export type ToolStatus = "running" | "done" | "error" | "denied";

/** A tool's input or output as text to show in a code block. */
export interface CodeText {
  text: string;
  /** A Code Block language: "bash", "diff", "json", "text", a file's language… */
  language: string;
  /** Lines left out (the text was too long); 0 when complete. */
  omittedLines: number;
}

export interface ToolItem {
  kind: "tool";
  id: string;
  ts: number;
  /** The tool's own name ("Bash", "Edit") when known, else its kind in words. */
  name: string;
  /** What it did, in one line: its description, else the command or file. */
  title: string;
  toolKind: ToolKind;
  status: ToolStatus;
  input?: CodeText | undefined;
  output?: CodeText | undefined;
  /** Why it failed or was refused, one line. */
  error?: string | undefined;
}

export type NoticeTone = "muted" | "info" | "waiting" | "error";

export type TranscriptItem =
  | { kind: "user"; id: string; ts: number; text: string; attachments: PromptAttachment[] }
  /** Markdown. */
  | { kind: "assistant"; id: string; ts: number; text: string }
  | { kind: "reasoning"; id: string; ts: number; text: string }
  | ToolItem
  | { kind: "plan"; id: string; ts: number; entries: PlanEntry[] }
  | {
      kind: "permission";
      id: string;
      ts: number;
      requestId?: string | undefined;
      title: string;
      toolKind: ToolKind;
      outcome: "pending" | "approved" | "denied" | "cancelled";
      by?: "user" | "rule" | "system" | undefined;
      /** Answered by voice (v0.2). */
      via?: "voice" | undefined;
    }
  | { kind: "notice"; id: string; ts: number; text: string; tone: NoticeTone; detail?: string | undefined };

/** Tool output beyond this is cut (the UI says how many lines were left out). */
export const OUTPUT_MAX_LINES = 60;
export const OUTPUT_MAX_CHARS = 6000;

// ---------------------------------------------------------------- small helpers

type Obj = Record<string, unknown>;

function obj(v: unknown): Obj {
  return v && typeof v === "object" && !Array.isArray(v) ? (v as Obj) : {};
}

function str(v: unknown): string | undefined {
  return typeof v === "string" && v.trim() !== "" ? v : undefined;
}

function pick(o: Obj, keys: readonly string[]): string | undefined {
  for (const k of keys) {
    const v = str(o[k]);
    if (v) return v;
  }
  return undefined;
}

function oneLine(text: string, max = 160): string {
  const t = text.replace(/\s+/g, " ").trim();
  return t.length > max ? `${t.slice(0, max - 1)}…` : t;
}

function clipCodeTo(text: string, language: string, limit: Limit): CodeText {
  return clipCode(text, language, limit.lines, limit.chars);
}

/** Cut long text to OUTPUT_MAX_LINES lines / OUTPUT_MAX_CHARS characters. */
export function clipCode(text: string, language: string, maxLines = OUTPUT_MAX_LINES, maxChars = OUTPUT_MAX_CHARS): CodeText {
  let t = text.replace(/\r\n/g, "\n").replace(/\n+$/, "");
  const lines = t.split("\n");
  let omittedLines = 0;
  if (lines.length > maxLines) {
    omittedLines = lines.length - maxLines;
    t = lines.slice(0, maxLines).join("\n");
  }
  if (t.length > maxChars) {
    const kept = t.slice(0, maxChars);
    omittedLines += t.slice(maxChars).split("\n").length - 1 + (kept.endsWith("\n") ? 0 : 1);
    t = kept.replace(/\n[^\n]*$/, "") || kept;
  }
  return { text: t, language, omittedLines };
}

const EXT_LANG: Record<string, string> = {
  ts: "typescript",
  tsx: "tsx",
  js: "javascript",
  jsx: "jsx",
  mjs: "javascript",
  cjs: "javascript",
  json: "json",
  md: "markdown",
  py: "python",
  sh: "bash",
  bash: "bash",
  css: "css",
  html: "html",
  yml: "yaml",
  yaml: "yaml",
  sql: "sql",
};

export function languageOfPath(path: string | undefined): string {
  const ext = /\.([a-z0-9]+)$/i.exec(path ?? "")?.[1]?.toLowerCase();
  return (ext && EXT_LANG[ext]) || "text";
}

const KIND_OF_TOOL: Record<string, ToolKind> = {
  bash: "execute",
  shell: "execute",
  powershell: "execute",
  run_in_terminal: "execute",
  read: "read",
  view: "read",
  notebookread: "read",
  edit: "edit",
  multiedit: "edit",
  write: "edit",
  notebookedit: "edit",
  create: "edit",
  apply_patch: "edit",
  grep: "search",
  glob: "search",
  ls: "search",
  search: "search",
  webfetch: "fetch",
  websearch: "fetch",
  fetch: "fetch",
  task: "other",
  agent: "other",
};

function kindOfToolName(name: string): ToolKind {
  return KIND_OF_TOOL[name.toLowerCase()] ?? "other";
}

const KIND_WORD: Record<ToolKind, string> = {
  read: "Read",
  edit: "Edit",
  delete: "Delete",
  move: "Move",
  search: "Search",
  execute: "Run",
  think: "Think",
  fetch: "Fetch",
  switch_mode: "Switch mode",
  other: "Tool",
};

/** A tool's input object (Copilot sends it as a JSON string). */
function toolArgs(v: unknown): Obj {
  if (typeof v === "string") {
    try {
      return obj(JSON.parse(v));
    } catch {
      return { command: v };
    }
  }
  return obj(v);
}

/**
 * `path` relative to the session's folder when it is inside it ("src/a.ts" for "E:\\work\\api\\src\\a.ts" in
 * "E:\\work\\api"), else unchanged. Separators and (for drive paths) case are ignored when comparing.
 */
export function relativePath(path: string, cwd: string | null | undefined): string {
  if (!cwd) return path;
  const norm = (p: string) => p.replace(/\\/g, "/").replace(/\/+$/, "");
  const p = norm(path);
  const base = norm(cwd);
  if (!base) return path;
  const fold = /^[a-z]:\//i.test(base) ? (x: string) => x.toLowerCase() : (x: string) => x;
  if (fold(p) === fold(base)) return ".";
  return fold(p).startsWith(fold(base) + "/") ? p.slice(base.length + 1) : path;
}

const PATH_KEYS = new Set(["file_path", "filePath", "path", "notebook_path"]);

/** One-line title: the tool's description, else its command, pattern, query, URL or file, else its name. */
function toolTitle(name: string, input: Obj, cwd?: string): string {
  for (const k of ["description", "command", "pattern", "query", "url", "file_path", "filePath", "path", "notebook_path", "prompt"]) {
    const v = str(input[k]);
    if (v) return oneLine(PATH_KEYS.has(k) ? relativePath(v, cwd) : v);
  }
  return name;
}

function diffText(path: string | undefined, before: string, after: string): string {
  const head = path ? `--- ${path}\n+++ ${path}\n` : "";
  const minus = before ? before.split("\n").map((l) => `-${l}`) : [];
  const plus = after ? after.split("\n").map((l) => `+${l}`) : [];
  return head + [...minus, ...plus].join("\n");
}

/** How much of a text to keep (`Infinity`: all of it). */
interface Limit {
  lines: number;
  chars: number;
}
const SHOWN: Limit = { lines: OUTPUT_MAX_LINES, chars: OUTPUT_MAX_CHARS };
/** What a permission request would run is never cut: the person approves all of it. */
const WHOLE: Limit = { lines: Infinity, chars: Infinity };

/** A tool's input to show: the command, a diff for edits, the written file, else its arguments as JSON. */
function toolInput(name: string, input: Obj, cwd?: string, limit: Limit = SHOWN): CodeText | undefined {
  const clipCode = (text: string, language: string) => clipCodeTo(text, language, limit);
  const command = str(input.command) ?? str(input.cmd);
  if (command) return clipCode(command, "bash");
  const rawPath = pick(input, ["file_path", "filePath", "path", "notebook_path"]);
  const path = rawPath ? relativePath(rawPath, cwd) : undefined;
  if (typeof input.old_string === "string" || typeof input.new_string === "string") {
    return clipCode(diffText(path, String(input.old_string ?? ""), String(input.new_string ?? "")), "diff");
  }
  if (Array.isArray(input.edits)) {
    const parts = input.edits.map((e) => obj(e)).map((e) => diffText(undefined, String(e.old_string ?? ""), String(e.new_string ?? "")));
    return clipCode(diffText(path, "", "").trimEnd() + (path ? "\n" : "") + parts.join("\n"), "diff");
  }
  if (typeof input.content === "string" && path) return clipCode(input.content, languageOfPath(path));
  // Reads and searches say it all in the title.
  const rest = Object.fromEntries(Object.entries(input).filter(([k]) => k !== "description"));
  const keys = Object.keys(rest);
  if (keys.length === 0) return undefined;
  if (keys.length === 1 && ["file_path", "filePath", "path", "pattern", "query", "url"].includes(keys[0]!)) return undefined;
  if (kindOfToolName(name) === "read" && keys.every((k) => ["file_path", "filePath", "path", "offset", "limit"].includes(k))) return undefined;
  return clipCode(JSON.stringify(rest, null, 2), "json");
}

/**
 * A permission request's tool input as code to show: the command, a diff, else its arguments as JSON. Never cut
 * (`omittedLines` is always 0): an agent must not be able to hide the end of what is approved.
 */
export function requestInputCode(rawInput: unknown): CodeText | undefined {
  if (typeof rawInput === "string") return rawInput.trim() ? clipCodeTo(rawInput, "bash", WHOLE) : undefined;
  return toolInput("", toolArgs(rawInput), undefined, WHOLE);
}

/**
 * What a permission request would do, for a confirm line ("Approve on Mac mini: <this>?"): the whole command when
 * there is one, else the whole input, else the agent's title. Never only the title when there is more: the title is
 * the agent's own words.
 */
export function requestCommandText(toolCall: { rawInput?: unknown; title?: string | undefined } | undefined): string {
  const raw = toolCall?.rawInput;
  if (typeof raw === "string" && raw.trim()) return raw.trim();
  const args = toolArgs(raw);
  const command = str(args.command) ?? str(args.cmd);
  if (command) return command.trim();
  const code = requestInputCode(raw);
  if (code) return code.text;
  return str(toolCall?.title)?.trim() ?? "a tool call";
}

/** A tool's output to show: stdout and stderr, a file's text, a result string, else JSON. */
function toolOutput(response: unknown, pathHint?: string): CodeText | undefined {
  if (response === undefined || response === null) return undefined;
  if (typeof response === "string") return response.trim() ? clipCode(response, "text") : undefined;
  if (Array.isArray(response)) {
    // ACP content blocks / MCP results: [{type:"text", text}], [{type:"content", content:{text}}].
    const texts = response.map((b) => {
      const o = obj(b);
      return str(o.text) ?? str(obj(o.content).text) ?? "";
    });
    const joined = texts.filter(Boolean).join("\n");
    return joined ? clipCode(joined, "text") : undefined;
  }
  const o = obj(response);
  const stdout = typeof o.stdout === "string" ? o.stdout : undefined;
  const stderr = typeof o.stderr === "string" ? o.stderr : undefined;
  if (stdout !== undefined || stderr !== undefined) {
    const text = [stdout, stderr].filter((x) => x && x.trim()).join("\n") + (o.interrupted === true ? "\n(interrupted)" : "");
    return text.trim() ? clipCode(text, "text") : undefined;
  }
  const file = obj(o.file);
  if (typeof file.content === "string") return clipCode(file.content, languageOfPath(str(file.filePath) ?? pathHint));
  const result = pick(o, ["textResultForLlm", "output", "result", "content", "text", "message"]);
  if (result) return clipCode(result, "text");
  // Edits report their patch; the input already shows the change.
  if ("structuredPatch" in o || "oldString" in o || "newString" in o) return undefined;
  const keys = Object.keys(o);
  return keys.length ? clipCode(JSON.stringify(o, null, 2), "json") : undefined;
}

/** "PreCompact" -> "Pre compact". */
function words(event: string): string {
  const w = event
    .replace(/[-_]+/g, " ")
    .replace(/([a-z])([A-Z])/g, "$1 $2")
    .trim()
    .toLowerCase();
  return w ? w[0]!.toUpperCase() + w.slice(1) : "Event";
}

/** "the terminal" or "VS Code": where a hook session's prompts and approvals happen. */
export function hookPlace(session: Pick<Session, "source"> | undefined): string {
  return session?.source === "vscode_hook" ? "VS Code" : "the terminal";
}

// ---------------------------------------------------------------- the builder

class Builder {
  readonly items: TranscriptItem[] = [];
  /** Tool items by tool call id (ACP toolCallId / hook tool_use_id). */
  private readonly tools = new Map<string, number>();
  /** Hook tool calls without an id, oldest first (paired by name). */
  private readonly open: number[] = [];
  /** Permission items by request id, and the options each offered. */
  private readonly permissions = new Map<string, { index: number; options: PermissionOption[]; toolCallId?: string | undefined }>();
  /** TodoWrite calls (shown as the plan, not as tools). */
  private readonly todoIds = new Set<string>();

  constructor(
    private readonly place: string,
    readonly cwd?: string,
  ) {}

  private last(): TranscriptItem | undefined {
    return this.items[this.items.length - 1];
  }

  notice(id: string, ts: number, text: string, tone: NoticeTone = "muted", detail?: string): void {
    this.items.push({ kind: "notice", id, ts, text, tone, ...(detail ? { detail } : null) });
  }

  /** A user turn: injected notices first, then the person's words (if any are left). */
  user(id: string, ts: number, raw: string): void {
    const parsed = parsePrompt(raw);
    parsed.notices.forEach((n, i) => this.notice(`${id}:n${i}`, ts, n.text, n.kind === "injected" ? "muted" : "info", n.detail));
    if (parsed.text || parsed.attachments.length) {
      this.items.push({ kind: "user", id, ts, text: parsed.text, attachments: parsed.attachments });
    }
  }

  /** Streamed text: appended to the previous item of the same kind, else a new one. */
  stream(kind: "assistant" | "reasoning", id: string, ts: number, text: string): void {
    if (!text) return;
    const last = this.last();
    if (last && last.kind === kind) {
      this.items[this.items.length - 1] = { ...last, text: last.text + text };
      return;
    }
    this.items.push({ kind, id, ts, text });
  }

  /** A whole message (hook sessions report the reply at the end of the turn). */
  message(kind: "assistant" | "user", id: string, ts: number, text: string): void {
    const t = text.trim();
    if (!t) return;
    if (kind === "user") return this.user(id, ts, t);
    const last = this.last();
    if (last && last.kind === "assistant" && last.text.trim() === t) return;
    this.items.push({ kind: "assistant", id, ts, text: t });
  }

  plan(id: string, ts: number, entries: PlanEntry[]): void {
    // The latest plan replaces the one shown since the person last wrote.
    for (let i = this.items.length - 1; i >= 0; i--) {
      const it = this.items[i]!;
      if (it.kind === "user") break;
      if (it.kind === "plan") {
        this.items[i] = { ...it, entries, ts };
        return;
      }
    }
    this.items.push({ kind: "plan", id, ts, entries });
  }

  /**
   * Create or update the call `callId`. `create` fills a new item only (a hook result repeats less than its call:
   * it must not replace the call's description or input).
   */
  tool(
    callId: string | undefined,
    ts: number,
    patch: Partial<Omit<ToolItem, "kind" | "id" | "ts">>,
    opts: { pairBy?: string; create?: Partial<Omit<ToolItem, "kind" | "id" | "ts">> } = {},
  ): void {
    const nameForPairing = opts.pairBy;
    let index = callId !== undefined ? this.tools.get(callId) : undefined;
    if (index === undefined && callId === undefined && nameForPairing !== undefined && patch.status !== "running") {
      // A hook result without an id: the oldest open call of that tool.
      const at = this.open.findIndex((i) => (this.items[i] as ToolItem).name === nameForPairing);
      if (at >= 0) index = this.open.splice(at, 1)[0];
    }
    if (index !== undefined) {
      const prev = this.items[index] as ToolItem;
      const next: ToolItem = { ...prev };
      for (const [k, v] of Object.entries(patch)) if (v !== undefined) (next as unknown as Obj)[k] = v;
      // A refused call stays refused even if the agent reports it failed afterwards.
      if (prev.status === "denied" && next.status === "error") next.status = "denied";
      // A late "running" (a PreToolUse after its result, an update out of order) never reopens a finished call.
      if (prev.status !== "running" && next.status === "running") next.status = prev.status;
      this.items[index] = next;
      return;
    }
    patch = { ...opts.create, ...Object.fromEntries(Object.entries(patch).filter(([, v]) => v !== undefined)) };
    const item: ToolItem = {
      kind: "tool",
      id: `tool:${callId ?? `${ts}:${this.items.length}`}`,
      ts,
      name: patch.name ?? KIND_WORD[patch.toolKind ?? "other"],
      title: patch.title ?? patch.name ?? "Tool call",
      toolKind: patch.toolKind ?? "other",
      status: patch.status ?? "running",
      ...(patch.input ? { input: patch.input } : null),
      ...(patch.output ? { output: patch.output } : null),
      ...(patch.error ? { error: patch.error } : null),
    };
    this.items.push(item);
    if (callId !== undefined) this.tools.set(callId, this.items.length - 1);
    else if (item.status === "running") this.open.push(this.items.length - 1);
  }

  /** A turn ended: calls still "running" will not report back any more. */
  endTurn(): void {
    this.items.forEach((it, i) => {
      if (it.kind === "tool" && it.status === "running") this.items[i] = { ...it, status: "done" };
    });
    this.open.length = 0;
  }

  permission(id: string, ts: number, requestId: string | undefined, title: string, toolKind: ToolKind, options: PermissionOption[], toolCallId?: string): void {
    this.items.push({ kind: "permission", id, ts, requestId, title, toolKind, outcome: "pending" });
    if (requestId) this.permissions.set(requestId, { index: this.items.length - 1, options, toolCallId });
  }

  resolved(p: Partial<PermissionResolvedPayload>): void {
    const entry = p.requestId ? this.permissions.get(p.requestId) : undefined;
    if (!entry) return;
    const it = this.items[entry.index];
    if (!it || it.kind !== "permission") return;
    let outcome: "approved" | "denied" | "cancelled" = "cancelled";
    if (p.outcome?.kind === "selected") {
      const kind = entry.options.find((o) => o.optionId === (p.outcome as { optionId: string }).optionId)?.kind;
      outcome = kind?.startsWith("reject") ? "denied" : "approved";
    }
    this.items[entry.index] = p.via === "voice" ? { ...it, outcome, by: p.by, via: "voice" } : { ...it, outcome, by: p.by };
    if (outcome !== "approved" && entry.toolCallId) this.tool(entry.toolCallId, it.ts, { status: "denied" });
  }

  isTodo(id: string | undefined): boolean {
    return !!id && this.todoIds.has(id);
  }

  markTodo(id: string | undefined): void {
    if (id) this.todoIds.add(id);
  }

  where(): string {
    return this.place;
  }
}

// ---------------------------------------------------------------- owned (ACP) events

function acpText(payload: unknown): string {
  const p = obj(payload);
  if (typeof p.text === "string") return p.text;
  const content = obj(p.content);
  return typeof content.text === "string" ? content.text : "";
}

/** ACP tool_call content: text blocks are output, diff blocks are the change. */
function acpContent(content: unknown): { output?: CodeText; diff?: CodeText } {
  if (!Array.isArray(content)) return {};
  const texts: string[] = [];
  const diffs: string[] = [];
  for (const block of content) {
    const b = obj(block);
    if (b.type === "diff") diffs.push(diffText(str(b.path), String(b.oldText ?? ""), String(b.newText ?? "")));
    else {
      const t = str(obj(b.content).text) ?? str(b.text);
      if (t) texts.push(t);
    }
  }
  return {
    ...(texts.length ? { output: clipCode(texts.join("\n"), "text") } : null),
    ...(diffs.length ? { diff: clipCode(diffs.join("\n"), "diff") } : null),
  };
}

const ACP_STATUS: Record<string, ToolStatus> = { pending: "running", in_progress: "running", completed: "done", failed: "error" };

function owned(b: Builder, ev: HubEvent): void {
  const p = obj(ev.payload);
  switch (ev.type) {
    case "user_message":
      b.user(ev.id, ev.ts, acpText(ev.payload));
      return;
    case "agent_message_chunk":
      b.stream("assistant", ev.id, ev.ts, acpText(ev.payload));
      return;
    case "agent_thought_chunk":
      b.stream("reasoning", ev.id, ev.ts, acpText(ev.payload));
      return;
    case "tool_call":
    case "tool_call_update": {
      const callId = str(p.toolCallId) ?? `anon-${ev.id}`;
      const input = obj(p.rawInput);
      const kind = (str(p.kind) as ToolKind | undefined) ?? undefined;
      const { output, diff } = acpContent(p.content);
      const rawOut = p.rawOutput !== undefined ? toolOutput(p.rawOutput) : undefined;
      const status = str(p.status) ? ACP_STATUS[String(p.status)] : undefined;
      const title = str(p.title);
      b.tool(callId, ev.ts, {
        ...(title ? { title: oneLine(title) } : null),
        ...(kind ? { toolKind: kind, ...(ev.type === "tool_call" ? { name: KIND_WORD[kind] } : null) } : null),
        ...(status ? { status } : ev.type === "tool_call" ? { status: "running" as const } : null),
        ...(diff ? { input: diff } : Object.keys(input).length ? { input: toolInput("", input) } : null),
        ...(output ?? rawOut ? { output: output ?? rawOut } : null),
      });
      return;
    }
    case "plan":
      b.plan(ev.id, ev.ts, Array.isArray(p.entries) ? (p.entries as PlanEntry[]) : []);
      return;
    case "request_permission": {
      const tool = obj(p.toolCall);
      b.permission(
        ev.id,
        ev.ts,
        str(p.requestId),
        oneLine(str(tool.title) ?? str(p.title) ?? "Permission requested"),
        (str(tool.kind) as ToolKind | undefined) ?? "other",
        Array.isArray(p.options) ? (p.options as PermissionOption[]) : [],
        str(tool.toolCallId),
      );
      return;
    }
    case "permission_resolved":
      b.resolved(p as Partial<PermissionResolvedPayload>);
      return;
    case "stop": {
      b.endTurn();
      const reason = str(p.stopReason) ?? "end_turn";
      if (reason === "end_turn") return;
      b.notice(ev.id, ev.ts, STOP_TEXT[reason] ?? `Stopped (${reason})`, reason === "cancelled" ? "muted" : "info");
      return;
    }
    case "error":
      b.notice(ev.id, ev.ts, oneLine(str(p.message) ?? "Something went wrong"), "error");
      return;
    case "session_start":
      b.notice(ev.id, ev.ts, "Session started");
      return;
    case "session_end":
      b.endTurn();
      b.notice(ev.id, ev.ts, "Session ended");
      return;
    case "notification": {
      const text = [str(p.title), str(p.body) ?? str(p.message)].filter(Boolean).join(": ");
      b.notice(ev.id, ev.ts, oneLine(text || "Notification"), "info");
      return;
    }
    default:
      b.notice(ev.id, ev.ts, words(String(ev.type)));
  }
}

const STOP_TEXT: Record<string, string> = {
  max_tokens: "Stopped: it reached its token limit",
  max_turn_requests: "Stopped: too many steps in one turn",
  refusal: "The agent declined to continue",
  cancelled: "Cancelled",
};

// ---------------------------------------------------------------- hook sessions

function canonical(name: string): string {
  return name.replace(/[^a-z0-9]/gi, "").toLowerCase();
}

function hookEvent(b: Builder, ev: HubEvent, hp: Partial<HookPayload>): void {
  const raw = obj(hp.raw);
  const name = hp.hookEvent ?? "";
  const ev0 = canonical(name);
  const message = str(hp.message);
  const where = b.where();
  const toolName = pick(raw, ["tool_name", "toolName"]);
  const callId = pick(raw, ["tool_use_id", "toolUseId", "tool_call_id", "toolCallId"]);
  const args = toolArgs(raw.tool_input ?? raw.toolArgs ?? raw.toolInput);

  switch (ev0) {
    case "sessionstart": {
      const src = str(raw.source);
      b.notice(ev.id, ev.ts, src === "resume" ? "Session resumed" : src === "compact" ? "Conversation compacted" : "Session started");
      const initial = pick(raw, ["initial_prompt", "initialPrompt"]);
      if (initial) b.user(`${ev.id}:prompt`, ev.ts, initial);
      return;
    }
    case "userpromptsubmit":
    case "userpromptsubmitted":
      b.user(ev.id, ev.ts, pick(raw, ["prompt"]) ?? message ?? "");
      return;
    case "pretooluse": {
      const tool = toolName ?? "Tool";
      if (tool === "TodoWrite" && Array.isArray(args.todos)) {
        b.markTodo(callId);
        b.plan(ev.id, ev.ts, todoEntries(args.todos));
        return;
      }
      b.tool(callId, ev.ts, { name: tool, title: toolTitle(tool, args, b.cwd), toolKind: kindOfToolName(tool), status: "running", input: toolInput(tool, args, b.cwd) });
      return;
    }
    case "posttooluse":
    case "posttoolusefailure": {
      if (b.isTodo(callId) || toolName === "TodoWrite") return;
      const tool = toolName ?? "Tool";
      const failed = ev0 === "posttoolusefailure";
      const err = failed ? oneLine(str(raw.error) ?? str(obj(raw.error).message) ?? "Failed") : undefined;
      const denied = !!err && /denied|rejected|not allowed|declined/i.test(err);
      const resp = raw.tool_response ?? raw.toolResult ?? raw.tool_result ?? raw.toolResponse;
      const path = pick(args, ["file_path", "filePath", "path"]);
      b.tool(
        callId,
        ev.ts,
        { status: failed ? (denied ? "denied" : "error") : "done", output: toolOutput(resp, path), ...(err ? { error: err } : null) },
        { pairBy: tool, create: { name: tool, title: toolTitle(tool, args, b.cwd), toolKind: kindOfToolName(tool), input: toolInput(tool, args, b.cwd) } },
      );
      return;
    }
    case "permissionrequest": {
      const what = toolName ? toolTitle(toolName, args, b.cwd) : (message ?? "a tool call");
      b.notice(ev.id, ev.ts, `Asked for permission in ${where}: ${oneLine(what, 100)}`, "waiting");
      return;
    }
    case "notification": {
      const type = pick(raw, ["notification_type", "notificationType"]) ?? "";
      const msg = pick(raw, ["message"]) ?? "";
      switch (type) {
        case "idle_prompt":
        case "agent_idle":
          b.notice(ev.id, ev.ts, `Waiting for you in ${where}`, "waiting");
          return;
        case "permission_prompt":
          b.notice(ev.id, ev.ts, `Asked for permission in ${where}`, "waiting", msg || undefined);
          return;
        case "elicitation_dialog":
        case "elicitation_url_dialog":
        case "agent_needs_input":
          b.notice(ev.id, ev.ts, `Asked you a question in ${where}`, "waiting", msg || undefined);
          return;
        case "auth_success":
        case "elicitation_complete":
        case "elicitation_response":
        case "shell_completed":
        case "shell_detached_completed":
          return;
        default:
          if (msg) b.notice(ev.id, ev.ts, oneLine(msg), "info");
          return;
      }
    }
    case "stop":
    case "agentstop":
    case "agentturncomplete": {
      const inputs = raw["input-messages"] ?? raw.input_messages;
      if (Array.isArray(inputs)) {
        inputs.forEach((m, i) => typeof m === "string" && b.message("user", `${ev.id}:in${i}`, ev.ts, m));
      }
      const reply = pick(raw, ["last_assistant_message", "lastAssistantMessage", "last-assistant-message"]);
      b.endTurn();
      if (reply) b.message("assistant", ev.id, ev.ts, reply);
      return;
    }
    case "stopfailure":
      b.endTurn();
      b.notice(ev.id, ev.ts, oneLine(message ?? "The turn stopped with an error"), "error");
      return;
    case "erroroccurred":
      b.notice(ev.id, ev.ts, oneLine(message ?? "Something went wrong"), "error");
      return;
    case "subagentstart":
      return;
    case "subagentstop": {
      const who = pick(raw, ["agent_type", "agentType", "agentName", "agent_name"]);
      b.notice(ev.id, ev.ts, who ? `Subagent ${who} finished` : "A subagent finished");
      return;
    }
    case "precompact":
      b.notice(ev.id, ev.ts, "Compacting the conversation");
      return;
    case "postcompact":
      return;
    case "sessionend":
      b.endTurn();
      b.notice(ev.id, ev.ts, "Session ended");
      return;
    case "interrupt":
      b.endTurn();
      b.notice(ev.id, ev.ts, "Turn interrupted");
      return;
    default:
      b.notice(ev.id, ev.ts, message ? `${words(name)}: ${oneLine(message, 100)}` : words(name));
  }
}

function todoEntries(todos: unknown[]): PlanEntry[] {
  return todos.map((t) => {
    const o = obj(t);
    const status = o.status === "completed" || o.status === "in_progress" ? o.status : "pending";
    return { content: str(o.content) ?? str(o.activeForm) ?? "Step", priority: "medium", status };
  });
}

function isHookPayload(p: unknown): p is HookPayload {
  const o = obj(p);
  return typeof o.hookEvent === "string" && "raw" in o;
}

/**
 * The session as a conversation: one item per user turn, reply, thought, tool call, plan, permission request and
 * notice, in order. `session` decides the wording for hook sessions ("in the terminal" / "in VS Code").
 */
export function toTranscript(session: (Pick<Session, "source"> & Partial<Pick<Session, "cwd">>) | undefined, events: readonly HubEvent[]): TranscriptItem[] {
  const b = new Builder(hookPlace(session), session?.cwd);
  let lastTruncated: number | null = null;
  for (const ev of events) {
    if (isTruncatedPayload(ev.payload)) {
      if (lastTruncated === null || lastTruncated !== b.items.length - 1) {
        b.notice(ev.id, ev.ts, "Part of this session was too large to copy here. Open it on its own machine to see it.");
      }
      lastTruncated = b.items.length - 1;
      continue;
    }
    if ((ev.type === "hook" || ev.type === "session_start" || ev.type === "session_end") && isHookPayload(ev.payload)) {
      hookEvent(b, ev, ev.payload);
      continue;
    }
    owned(b, ev);
  }
  return b.items;
}

// ---------------------------------------------------------------- grouping for display

export interface WorkBlock {
  kind: "work";
  id: string;
  ts: number;
  tools: ToolItem[];
  /** "Worked: ran 6 commands, read 2 files" (or "Working: …" while one runs). */
  summary: string;
  running: boolean;
}

export type TranscriptBlock = TranscriptItem | WorkBlock;

const COUNT_WORDS: Array<{ kinds: ToolKind[]; one: string; many: (n: number) => string }> = [
  { kinds: ["execute"], one: "ran 1 command", many: (n) => `ran ${n} commands` },
  { kinds: ["read"], one: "read 1 file", many: (n) => `read ${n} files` },
  { kinds: ["edit", "delete", "move"], one: "changed 1 file", many: (n) => `changed ${n} files` },
  { kinds: ["search"], one: "searched once", many: (n) => `searched ${n} times` },
  { kinds: ["fetch"], one: "fetched 1 page", many: (n) => `fetched ${n} pages` },
  { kinds: ["think", "switch_mode", "other"], one: "used 1 other tool", many: (n) => `used ${n} other tools` },
];

/** "Worked: ran 6 commands, read 2 files". */
export function workSummary(tools: readonly ToolItem[]): string {
  const parts: string[] = [];
  for (const c of COUNT_WORDS) {
    const n = tools.filter((t) => c.kinds.includes(t.toolKind)).length;
    if (n > 0) parts.push(n === 1 ? c.one : c.many(n));
  }
  const failed = tools.filter((t) => t.status === "error").length;
  const denied = tools.filter((t) => t.status === "denied").length;
  if (failed) parts.push(`${failed} failed`);
  if (denied) parts.push(`${denied} refused`);
  const running = tools.some((t) => t.status === "running");
  return `${running ? "Working" : "Worked"}: ${parts.join(", ") || "used tools"}`;
}

function sameCode(a: CodeText | undefined, b: CodeText | undefined): boolean {
  return a === b || (!!a && !!b && a.text === b.text && a.language === b.language && a.omittedLines === b.omittedLines);
}

/** Cheap equality for one transcript item (strings compare natively; no serialising). */
export function sameItem(a: TranscriptItem, b: TranscriptItem): boolean {
  if (a === b) return true;
  if (a.kind !== b.kind || a.id !== b.id || a.ts !== b.ts) return false;
  switch (a.kind) {
    case "user": {
      const o = b as typeof a;
      return a.text === o.text && a.attachments.length === o.attachments.length && a.attachments.every((x, i) => JSON.stringify(x) === JSON.stringify(o.attachments[i]));
    }
    case "assistant":
    case "reasoning":
      return a.text === (b as typeof a).text;
    case "tool": {
      const o = b as ToolItem;
      return a.status === o.status && a.title === o.title && a.name === o.name && a.toolKind === o.toolKind && a.error === o.error && sameCode(a.input, o.input) && sameCode(a.output, o.output);
    }
    case "plan":
      return JSON.stringify(a.entries) === JSON.stringify((b as typeof a).entries);
    case "permission": {
      const o = b as typeof a;
      return a.outcome === o.outcome && a.by === o.by && a.title === o.title && a.requestId === o.requestId;
    }
    case "notice": {
      const o = b as typeof a;
      return a.text === o.text && a.tone === o.tone && a.detail === o.detail;
    }
  }
}

/**
 * `next` with every item that did not change replaced by the object from `prev` (same id), so a renderer can skip
 * them (React.memo). A long session gets one new event at a time: only the items it touched are new objects.
 */
export function reuseItems(prev: readonly TranscriptItem[], next: TranscriptItem[]): TranscriptItem[] {
  if (prev.length === 0) return next;
  const byId = new Map(prev.map((i) => [i.id, i]));
  return next.map((i) => {
    const old = byId.get(i.id);
    return old && sameItem(old, i) ? old : i;
  });
}

/** The same for grouped blocks: a work step is reused when its calls are the very same objects. */
export function reuseBlocks(prev: readonly TranscriptBlock[], next: TranscriptBlock[]): TranscriptBlock[] {
  if (prev.length === 0) return next;
  const byId = new Map(prev.map((b) => [b.id, b]));
  return next.map((b) => {
    const old = byId.get(b.id);
    if (!old) return b;
    if (b.kind !== "work") return old === b ? old : b;
    return old.kind === "work" && old.tools.length === b.tools.length && old.tools.every((t, i) => t === b.tools[i]) ? old : b;
  });
}

/** Runs of at least `min` consecutive tool calls become one collapsible work step. */
export function groupWork(items: readonly TranscriptItem[], min = 2): TranscriptBlock[] {
  const out: TranscriptBlock[] = [];
  let run: ToolItem[] = [];
  const flush = () => {
    if (run.length >= min) {
      out.push({ kind: "work", id: `work:${run[0]!.id}`, ts: run[0]!.ts, tools: run, summary: workSummary(run), running: run.some((t) => t.status === "running") });
    } else out.push(...run);
    run = [];
  };
  for (const it of items) {
    if (it.kind === "tool") run.push(it);
    else {
      flush();
      out.push(it);
    }
  }
  flush();
  return out;
}
