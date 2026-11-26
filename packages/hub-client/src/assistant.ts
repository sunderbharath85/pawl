// Assistant mode (docs/CONTRACTS.md "v0.2 Assistant mode: the pet does the work"), the pure part: Settings > Assistant
// (on/off, agent, workspace, Pawl's session tools, the browser), its status line and confirms, the one-line activity
// under the bubble from the assistant session's latest event, and streaming replies. With it on, the default pet's chat
// goes to a real agent session (purpose "assistant") working in its own folder. Its changes, commands, web use and new
// sessions are ordinary approval cards under the user's rules (an agent may read files in its own folder without
// asking: Claude Code does), and it can never answer approvals.
import type {
  Agent,
  AgentId,
  ChatMessage,
  HubEvent,
  HubSettings,
  HubSettingsAssistant,
  HubSettingsAssistantPatch,
  HubSettingsPatch,
  PermissionRequest,
  PlanPayload,
  Session,
  ToolCallPayload,
  ToolKind,
} from "@pawl/protocol";
import type { RiskyChange } from "./settings";
import { broadFolderReason } from "./settings";

export const ASSISTANT_AUTO = "auto";
/**
 * What asks first (never "every step": an agent reads files in its own folder without asking, e.g. Claude Code's
 * read-only tools).
 */
export const ASSISTANT_ASKS = "Changes, commands, web use and new sessions ask you first, by your approval rules.";
/** What "auto" tries, in order (then the first installed ACP agent). */
export const ASSISTANT_AUTO_ORDER: readonly AgentId[] = ["claude-code", "codex", "github-copilot-cli"];
/** The workspace the hub uses when none is set (`%USERPROFILE%\Pawl`, `~/Pawl`); patching "" resets to it. */
export const ASSISTANT_DEFAULT_WORKSPACE = "~/Pawl";

/** Settings > Assistant, or null for a hub from before Assistant mode. */
export function assistantOf(s: HubSettings | null | undefined): HubSettingsAssistant | null {
  return s?.assistant ?? null;
}

export function isAssistantSession(s: Pick<Session, "purpose"> | null | undefined): boolean {
  return s?.purpose === "assistant";
}

/**
 * A reply from the assistant (its words can carry text from web pages, files or other agents): it belongs to an
 * assistant session, or to the live one, or carries the brain's spoken form (`speech`, only on assistant replies).
 */
export function isAssistantReply(
  m: Pick<ChatMessage, "sessionId" | "speech">,
  ctx: { sessions: Readonly<Record<string, Pick<Session, "purpose">>>; settings?: HubSettings | null | undefined },
): boolean {
  if (m.speech !== undefined) return true;
  if (!m.sessionId) return false;
  return isAssistantSession(ctx.sessions[m.sessionId]) || assistantOf(ctx.settings)?.sessionId === m.sessionId;
}

/** The assistant is still writing this reply (`chat_update` replaces its text until `streaming` is false). */
export function isStreaming(m: Pick<ChatMessage, "streaming"> | null | undefined): boolean {
  return m?.streaming === true;
}

// ---------------------------------------------------------------- agents

/** The agents the assistant can use: ACP agents installed on this machine (outside A2A agents are not ACP). */
export function assistantAgents(agents: readonly Agent[], machineId: string | undefined): Agent[] {
  return agents.filter((a) => a.installed && a.source !== "a2a" && (machineId === undefined || a.machineId === machineId));
}

function nameOf(agents: readonly Agent[], id: string): string {
  const known: Record<string, string> = { "claude-code": "Claude Code", codex: "Codex", "github-copilot-cli": "Copilot CLI" };
  return agents.find((a) => a.id === id)?.name?.trim() || known[id] || id;
}

/**
 * The hub's caution about the agent (`agentNote`, set when the resolved agent is not Claude Code: Pawl can't lock that
 * agent's own approval settings, so it may run some tool calls without a card). Shown as-is; null when there is none.
 */
export function assistantAgentNote(a: Pick<HubSettingsAssistant, "agentNote">): string | null {
  return a.agentNote?.trim() || null;
}

/** The agent it works with ("Claude Code"), or null when none is installed. */
export function assistantAgentName(a: Pick<HubSettingsAssistant, "resolvedAgent">, agents: readonly Agent[]): string | null {
  return a.resolvedAgent ? nameOf(agents, a.resolvedAgent) : null;
}

export interface AssistantAgentChoice {
  value: string;
  label: string;
  /** False for a chosen agent that is not installed (kept in the list so the choice shows). */
  installed: boolean;
}

/**
 * The Agent dropdown: "Automatic (Claude Code)" (what "auto" picked; "none installed" when nothing is), then every
 * installed ACP agent; a chosen agent that is not installed stays in the list, marked.
 */
export function assistantAgentChoices(a: Pick<HubSettingsAssistant, "agent" | "resolvedAgent">, agents: readonly Agent[], machineId?: string): AssistantAgentChoice[] {
  const installed = assistantAgents(agents, machineId);
  const autoPick = a.agent === ASSISTANT_AUTO ? a.resolvedAgent : autoAgent(installed);
  const out: AssistantAgentChoice[] = [
    { value: ASSISTANT_AUTO, label: `Automatic (${autoPick ? nameOf(agents, autoPick) : "none installed"})`, installed: !!autoPick },
  ];
  for (const ag of installed) out.push({ value: ag.id, label: nameOf(agents, ag.id), installed: true });
  if (a.agent !== ASSISTANT_AUTO && !installed.some((ag) => ag.id === a.agent)) {
    out.push({ value: a.agent, label: `${nameOf(agents, a.agent)} (not installed)`, installed: false });
  }
  return out;
}

/** What "auto" would pick from these installed agents (the hub decides; this is for the label before it says). */
export function autoAgent(installed: readonly Pick<Agent, "id">[]): AgentId | undefined {
  for (const id of ASSISTANT_AUTO_ORDER) if (installed.some((a) => a.id === id)) return id;
  return installed[0]?.id;
}

/**
 * Why the chosen agent can't be used, or null: a chosen agent that is not installed, or nothing installed for
 * "auto". (The hub's own `error` says more once it tried; see `assistantStatus`.)
 */
export function assistantAgentProblem(a: Pick<HubSettingsAssistant, "agent" | "resolvedAgent">, agents: readonly Agent[], machineId?: string): string | null {
  const installed = assistantAgents(agents, machineId);
  if (a.agent === ASSISTANT_AUTO) {
    return a.resolvedAgent || installed.length > 0 ? null : "No coding agent is installed on this machine. Install Claude Code, Codex or Copilot CLI, then pick it here.";
  }
  if (installed.some((ag) => ag.id === a.agent)) return null;
  return `${nameOf(agents, a.agent)} is not installed on this machine. Install it, or pick another agent.`;
}

// ---------------------------------------------------------------- paths

function trimSep(p: string): string {
  return p.length > 1 ? p.replace(/[\\/]+$/, "") : p;
}

/** "~/Pawl" for a folder under the home folder (forward slashes after "~"), else the path itself. */
export function tildePath(path: string, home: string | null | undefined): string {
  const p = trimSep(path.trim());
  if (!home) return p;
  const h = trimSep(home.trim());
  const win = /^[a-z]:[\\/]/i.test(h) || h.includes("\\");
  const norm = (s: string) => (win ? s.replace(/\\/g, "/").toLowerCase() : s);
  if (norm(p) === norm(h)) return "~";
  if (norm(p).startsWith(`${norm(h)}/`)) return `~/${p.slice(h.length + 1).replace(/\\/g, "/")}`;
  return p;
}

/** The default workspace spelled out for this user (`C:\Users\you\Pawl`), or "~/Pawl" while the home folder is unknown. */
export function defaultAssistantWorkspace(home: string | null | undefined): string {
  if (!home) return ASSISTANT_DEFAULT_WORKSPACE;
  const h = trimSep(home.trim());
  return `${h}${h.includes("\\") || /^[a-z]:/i.test(h) ? "\\" : "/"}Pawl`;
}

export function isDefaultWorkspace(path: string, home: string | null | undefined): boolean {
  const t = tildePath(path, home);
  return t === "~/Pawl" || trimSep(path.trim()) === ASSISTANT_DEFAULT_WORKSPACE;
}

// ---------------------------------------------------------------- texts

/** "Ollie does the work with Claude Code in ~/Pawl. Changes, commands, web use and new sessions ask you first, by your approval rules." */
export function assistantIntro(petName: string, agentName: string | null, workspace: string): string {
  return `${petName || "Your pet"} does the work with ${agentName ?? "a coding agent"} in ${workspace}. ${ASSISTANT_ASKS}`;
}

/** Under the bubble's text box: "Working with Claude Code in ~/Pawl". */
export function assistantWorkingLine(agentName: string | null, workspace: string): string {
  return agentName ? `Working with ${agentName} in ${workspace}` : `Assistant: no coding agent installed`;
}

export type AssistantTone = "idle" | "done" | "working" | "error";

/** The status line: Off / Ready / Working / the error. */
export function assistantStatus(a: Pick<HubSettingsAssistant, "enabled" | "state" | "error">): { label: string; tone: AssistantTone; error: string | null } {
  if (!a.enabled || a.state === "off") return { label: "Off", tone: "idle", error: null };
  if (a.state === "error") return { label: "Can't start", tone: "error", error: a.error?.trim() || "The assistant could not start." };
  if (a.state === "working") return { label: "Working", tone: "working", error: null };
  return { label: "Ready", tone: "done", error: null };
}

export function assistantPatch(base: HubSettingsAssistant, next: HubSettingsAssistantPatch): HubSettingsPatch {
  const assistant: HubSettingsAssistantPatch = {};
  for (const k of ["enabled", "agent", "workspace", "sessions", "browser"] as const) {
    const v = next[k];
    if (v !== undefined && v !== base[k]) (assistant as Record<string, unknown>)[k] = v;
  }
  return Object.keys(assistant).length ? { assistant } : {};
}

const A2A_SERVER_ON = "This turns on Pawl's A2A server, on 127.0.0.1 only.";

/**
 * Confirms for Settings > Assistant: turning it on, letting it start and steer other sessions, letting it use the
 * browser, and a very broad workspace (a drive, a home folder).
 */
export function assistantRiskyChanges(base: HubSettings, patch: HubSettingsPatch, ctx: { homeDir?: string | null | undefined } = {}): RiskyChange[] {
  const p = patch.assistant;
  const cur = assistantOf(base);
  if (!p || !cur) return [];
  const pet = base.bot.petName?.trim() || "Your pet";
  const enabled = p.enabled ?? cur.enabled;
  const sessions = p.sessions ?? cur.sessions;
  const workspace = p.workspace ?? cur.workspace;
  const serverOff = base.a2a ? !base.a2a.server : false;
  const out: RiskyChange[] = [];
  if (p.enabled === true && !cur.enabled) {
    const where = workspace ? tildePath(workspace, ctx.homeDir) : ASSISTANT_DEFAULT_WORKSPACE;
    out.push({
      key: "assistant.enabled",
      title: `Let ${pet} do the work?`,
      body:
        `${pet}'s chat goes to a coding agent on this machine, working in ${where}, where it may read files without asking. ` +
        `${ASSISTANT_ASKS} It can never approve anything itself, and text from web pages, files and other agents is never taken as instructions.` +
        (sessions ? " It can also start and steer coding agents in your repos; each start asks you first." : "") +
        (sessions && serverOff ? ` ${A2A_SERVER_ON}` : ""),
      confirm: "Turn on",
    });
  } else if (enabled && p.sessions === true && !cur.sessions) {
    out.push({
      key: "assistant.sessions",
      title: `Let ${pet} start and steer other sessions?`,
      body:
        `${pet} can then see your agents, start coding agents in your repos and in its workspace, prompt them, check on them and stop them. ` +
        "Each start asks you first, and those agents follow your approval rules as usual." +
        (serverOff ? ` ${A2A_SERVER_ON}` : ""),
      confirm: "Allow",
    });
  }
  if (enabled && p.browser === true && !cur.browser && base.browser?.enabled) {
    out.push({
      key: "assistant.browser",
      title: `Let ${pet} use your Chrome?`,
      body:
        `${pet} can open pages in your own Chrome, signed in as you, while browser use is on (Settings > Browser). ` +
        "Every click and every typing asks you first; it never types passwords or payment details.",
      confirm: "Allow",
    });
  }
  if (p.workspace !== undefined && p.workspace !== cur.workspace && p.workspace.trim()) {
    const why = broadFolderReason(p.workspace, ctx.homeDir);
    if (why) {
      out.push({
        key: "assistant.workspace",
        title: `Work in ${p.workspace}?`,
        body:
          `${p.workspace} is ${why}. ${pet} works anywhere under its workspace: it may read any file there without asking, and may start agents there. ` +
          "Changes, commands, web use and new sessions still ask you first, but a folder of its own (the default is ~/Pawl) keeps your other files out of its reach.",
        confirm: "Use this folder",
      });
    }
  }
  return out;
}

// ---------------------------------------------------------------- activity (one line in the bubble)

function obj(v: unknown): Record<string, unknown> {
  return v && typeof v === "object" ? (v as Record<string, unknown>) : {};
}

function clip(t: string, max = 60): string {
  const s = t.replace(/\s+/g, " ").trim();
  return s.length > max ? `${s.slice(0, max - 1)}…` : s;
}

function baseName(p: string): string {
  return p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || p;
}

const TOOL_VERB: Record<ToolKind, string> = {
  read: "Reading",
  edit: "Editing",
  delete: "Deleting",
  move: "Moving",
  search: "Searching",
  execute: "Running a command",
  think: "Thinking",
  fetch: "Looking on the web",
  switch_mode: "Switching mode",
  other: "Using a tool",
};

/** Pawl's own tools (`pawl mcp`) and the browser's (chrome-devtools), as said in the bubble. */
const NAMED_TOOLS: Readonly<Record<string, string>> = {
  pawl_agents: "Looking at your agents",
  pawl_start: "Starting a session",
  pawl_prompt: "Steering a session",
  pawl_status: "Checking on a session",
  pawl_task: "Checking on a session",
  pawl_cancel: "Stopping a session",
  navigate_page: "Opening a page",
  new_page: "Opening a page",
  click: "Clicking on the page",
  click_at: "Clicking on the page",
  fill: "Typing on the page",
  fill_form: "Typing on the page",
  take_snapshot: "Looking at the page",
  take_screenshot: "Looking at the page",
  list_pages: "Looking at the tabs",
};

/**
 * The MCP tool a title names: "mcp__pawl__pawl_start" (Claude Code), "pawl_status (MCP)" or a bare "pawl_status".
 * Null for an ordinary title ("Read notes.md").
 */
function mcpToolName(title: string): string | null {
  const t = title.trim();
  const mcp = /^mcp__.+__([a-z0-9_]+)$/i.exec(t);
  if (mcp) return mcp[1]!.toLowerCase();
  const bare = /^([a-z0-9]+(?:_[a-z0-9]+)*)(?:\s*\(mcp\))?$/i.exec(t);
  return bare ? bare[1]!.toLowerCase() : null;
}

/** "Reading notes.md", "Starting a session", "Running a command": one tool call, merged from its call + updates. */
export function toolActivity(tc: Partial<ToolCallPayload>): string {
  const title = (tc.title ?? "").trim();
  const named = mcpToolName(title);
  if (named && NAMED_TOOLS[named]) return NAMED_TOOLS[named]!;
  const verb = TOOL_VERB[tc.kind ?? "other"] ?? TOOL_VERB.other;
  const path = tc.locations?.[0]?.path;
  if (path) return clip(`${verb} ${baseName(path)}`);
  if (tc.kind && tc.kind !== "other") return verb;
  return title ? clip(title) : verb;
}

/**
 * The assistant session's one-line status in the bubble, from its latest event: "Waiting for your OK" while an
 * approval waits, "Reading notes.md" for a tool call under way, "Thinking", "Working on: <plan step>", "Hit a snag:
 * <error>". Null when it is not doing anything (its turn ended, or nothing happened yet).
 */
export function assistantActivity(
  events: readonly HubEvent[],
  opts: { pending?: number | undefined; session?: Pick<Session, "status" | "endedAt"> | null | undefined } = {},
): string | null {
  if (opts.session?.endedAt) return null;
  if ((opts.pending ?? 0) > 0) return "Waiting for your OK";
  for (let i = events.length - 1; i >= 0; i--) {
    const ev = events[i]!;
    switch (ev.type) {
      case "stop":
      case "session_end":
        return null;
      case "error": {
        const msg = obj(ev.payload).message;
        return typeof msg === "string" && msg.trim() ? `Hit a snag: ${clip(msg, 50)}` : "Hit a snag";
      }
      case "request_permission":
        // Answered (the card is gone): it goes on with that step.
        return opts.pending === undefined ? "Waiting for your OK" : "Thinking";
      case "tool_call":
      case "tool_call_update": {
        const p = obj(ev.payload) as Partial<ToolCallPayload>;
        // The call's kind, title and files come with the first event; updates often carry only the status.
        const merged: Partial<ToolCallPayload> = {};
        for (let j = 0; j <= i; j++) {
          const e = events[j]!;
          if (e.type !== "tool_call" && e.type !== "tool_call_update") continue;
          const q = obj(e.payload) as Partial<ToolCallPayload>;
          if (q.toolCallId !== p.toolCallId) continue;
          if (q.title) merged.title = q.title;
          if (q.kind) merged.kind = q.kind;
          if (q.locations?.length) merged.locations = q.locations;
          if (q.status) merged.status = q.status;
        }
        if (merged.status === "completed" || merged.status === "failed") return "Thinking";
        return toolActivity(merged);
      }
      case "plan": {
        const entries = (obj(ev.payload) as Partial<PlanPayload>).entries ?? [];
        const now = entries.find((e) => e.status === "in_progress");
        return now ? `Working on: ${clip(now.content, 48)}` : "Making a plan";
      }
      case "agent_thought_chunk":
      case "user_message":
      case "permission_resolved":
        return "Thinking";
      case "agent_message_chunk":
        return "Writing";
      case "session_start":
        return "Starting up";
      default:
        // notification, hook: look further back.
        continue;
    }
  }
  return null;
}

// ---------------------------------------------------------------- the assistant asking (approval cards)

/**
 * The start card's marker (sde-a2a): `toolCallId` "pawl-start:<uuid>". Only the hub can make it: it renames any
 * agent's tool call carrying that prefix to "agent-pawl-start:…". `rawInput.kind` "pawl_start" is informational only
 * (an agent could put it in its own input), so it never makes a card a start card.
 */
export const START_CARD_ID_PREFIX = "pawl-start:";
export const START_CARD_KIND = "pawl_start";

function isStartCard(tc: Partial<ToolCallPayload> | undefined): boolean {
  return !!tc && typeof tc.toolCallId === "string" && tc.toolCallId.startsWith(START_CARD_ID_PREFIX);
}

/**
 * The request comes from the assistant itself: its own session (purpose "assistant"), or its start card (which is
 * the assistant asking even before its session reached this client).
 */
export function isAssistantRequest(
  req: Pick<PermissionRequest, "sessionId"> & { toolCall?: Partial<ToolCallPayload> | undefined },
  sessions: Readonly<Record<string, Pick<Session, "purpose">>>,
): boolean {
  return isAssistantSession(sessions[req.sessionId]) || isStartCard(req.toolCall);
}

/** "Ollie (your assistant)": who asks on an assistant request's card. */
export function assistantAsker(petName: string | null | undefined): string {
  return `${petName?.trim() || "Your pet"} (your assistant)`;
}

/** The assistant asking to start a session: what it wants to start, and the whole prompt it would send. */
export interface AssistantStartCard {
  /** "Start Claude Code in api" (the hub's title). */
  title: string;
  /** The full prompt, never cut; "" when it starts without one. */
  prompt: string;
  agentId?: string | undefined;
  agentName?: string | undefined;
  /** The folder it would start in (canonical). */
  cwd?: string | undefined;
}

function textOf(v: unknown): string | undefined {
  if (typeof v === "string") return v.trim() ? v : undefined;
  if (Array.isArray(v)) {
    // ACP content blocks: [{type:"text", text}] or [{type:"content", content:{type:"text", text}}].
    const parts = v.map((b) => {
      const o = obj(b);
      return typeof o.text === "string" ? o.text : typeof obj(o.content).text === "string" ? (obj(o.content).text as string) : "";
    });
    const joined = parts.filter(Boolean).join("\n\n");
    return joined.trim() ? joined : undefined;
  }
  return undefined;
}

/**
 * The start card the hub shows when the assistant asks to start a session (sde-a2a), known by its toolCallId
 * "pawl-start:<uuid>" alone:
 * title "Start <Agent> in <folder>", kind execute, rawInput { kind: "pawl_start", agentId, agentName, cwd, prompt }
 * with the FULL prompt ("" for none), options Start (allow once) / Don't start (reject once). Null for any other request.
 */
export function assistantStartCard(req: Pick<PermissionRequest, "toolCall">): AssistantStartCard | null {
  const tc = req.toolCall;
  if (!isStartCard(tc)) return null;
  const input = obj(tc.rawInput);
  const str = (v: unknown) => (typeof v === "string" && v.trim() ? v : undefined);
  const agentName = str(input.agentName);
  const cwd = str(input.cwd) ?? tc.locations?.[0]?.path;
  const title = str(tc.title)?.trim() ?? `Start ${agentName ?? "a session"}${cwd ? ` in ${cwd}` : ""}`;
  return {
    title,
    prompt: textOf(input.prompt) ?? "",
    agentId: str(input.agentId),
    agentName,
    cwd,
  };
}
