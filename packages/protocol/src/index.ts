// @krypto/protocol — wire types shared by hub, desktop, VS Code, mobile.
// SOURCE OF TRUTH for hub <-> client messages. The Rust mirror lives in
// crates/protocol/src/lib.rs and must stay byte-compatible (camelCase fields,
// snake_case `type` tags). Change both together.

export const PROTOCOL_VERSION = 1;
export const HUB_DEFAULT_PORT = 47800;
export const HUB_HOST = "127.0.0.1";
/** Folder name under the OS data dir (Win %APPDATA%, mac ~/Library/Application Support, linux ~/.local/share). */
export const HUB_DATA_DIR_NAME = "krypto";
/** Plain-text bearer token file inside the data dir. Env override: KRYPTO_HUB_TOKEN. URL override: KRYPTO_HUB_URL. */
export const HUB_TOKEN_FILE = "token";

// ---------- primitives ----------
export type AgentId = string; // e.g. "claude-code", "codex", "gemini", or a custom id
export type SessionId = string; // hub-assigned uuid
export type PetId = string;
export type RequestId = string;
export type UnixMs = number;

// ---------- agents ----------
export interface AgentCapabilities {
  loadSession: boolean;
  listSessions: boolean;
  resumeSession: boolean;
  promptImage: boolean;
  promptAudio: boolean;
}

export interface Agent {
  id: AgentId;
  name: string;
  /** argv used to launch the ACP server over stdio, e.g. ["npx","-y","@agentclientprotocol/claude-agent-acp"] */
  command: string[];
  env?: Record<string, string>;
  version?: string;
  description?: string;
  icon?: string;
  installed: boolean;
  source: "registry" | "custom";
  /** Filled after the first `initialize` with this agent. */
  capabilities?: AgentCapabilities;
}

// ---------- sessions ----------
export type SessionSource = "owned" | "vscode_hook" | "cli_hook" | "wrapper";
export type SessionStatus =
  | "idle"
  | "thinking"
  | "working"
  | "waiting_permission"
  | "done"
  | "error";

export interface Session {
  id: SessionId;
  agentId: AgentId;
  /** ACP-side session id (owned/resumed sessions). */
  acpSessionId?: string;
  /** Hook-side id, e.g. Claude Code `session_id`. */
  externalId?: string;
  cwd: string;
  title?: string;
  source: SessionSource;
  status: SessionStatus;
  startedAt: UnixMs;
  lastEventAt: UnixMs;
  endedAt?: UnixMs;
}

/** Past session from ACP `session/list`. */
export interface HistorySession {
  agentId: AgentId;
  acpSessionId: string;
  cwd: string;
  title?: string;
  updatedAt?: UnixMs;
}

// ---------- events ----------
export type EventType =
  | "session_start"
  | "session_end"
  | "user_message"
  | "agent_thought_chunk"
  | "agent_message_chunk"
  | "tool_call"
  | "tool_call_update"
  | "plan"
  | "request_permission"
  | "permission_resolved"
  | "stop"
  | "error"
  | "notification"
  | "hook";

export type ToolKind =
  | "read"
  | "edit"
  | "delete"
  | "move"
  | "search"
  | "execute"
  | "think"
  | "fetch"
  | "switch_mode"
  | "other";

export type ToolCallStatus = "pending" | "in_progress" | "completed" | "failed";

export interface ToolLocation {
  path: string;
  line?: number;
}

export type StopReason =
  | "end_turn"
  | "max_tokens"
  | "max_turn_requests"
  | "refusal"
  | "cancelled";

/** Known payload shapes by event type. Unknown/extra fields are allowed. */
export interface TextPayload {
  text: string;
}
export interface ToolCallPayload {
  toolCallId: string;
  title?: string;
  kind?: ToolKind;
  status?: ToolCallStatus;
  locations?: ToolLocation[];
  rawInput?: unknown;
}
export interface PlanEntry {
  content: string;
  priority: "high" | "medium" | "low";
  status: "pending" | "in_progress" | "completed";
}
export interface PlanPayload {
  entries: PlanEntry[];
}
export interface StopPayload {
  stopReason: StopReason;
}
export interface ErrorPayload {
  message: string;
}
export interface HookPayload {
  source: HookSource;
  hookEvent: string; // e.g. "Stop", "SessionStart", "Notification", "agent-turn-complete"
  message?: string;
  raw: unknown;
}
/** user = a person (desktop/vscode/phone/chat); rule = rules engine; system = automatic cancel (turn ended, agent withdrew, process exit). */
export type ResolvedBy = "user" | "rule" | "system";

export interface PermissionResolvedPayload {
  requestId: RequestId;
  outcome: PermissionOutcome;
  by: ResolvedBy;
}

export interface HubEvent {
  /** Monotonic SQLite rowid. */
  id: number;
  sessionId: SessionId;
  type: EventType;
  payload: unknown;
  ts: UnixMs;
}

// ---------- permissions ----------
export type PermissionOptionKind =
  | "allow_once"
  | "allow_always"
  | "reject_once"
  | "reject_always";

export interface PermissionOption {
  optionId: string;
  name: string;
  kind: PermissionOptionKind;
}

export interface PermissionRequest {
  requestId: RequestId;
  sessionId: SessionId;
  agentId: AgentId;
  toolCall: ToolCallPayload;
  options: PermissionOption[];
  createdAt: UnixMs;
}

export type PermissionOutcome =
  | { kind: "selected"; optionId: string }
  | { kind: "cancelled" };

// ---------- rules ----------
export type RuleAction = "auto_allow" | "ask" | "deny";
export interface RuleMatch {
  toolKind?: ToolKind;
  /** glob, matched against every tool location path */
  path?: string;
  agent?: AgentId;
}
/**
 * Security hard floor: tool kinds that are NEVER auto-approved, whatever the rules say.
 * Unknown/"other" kinds count as risky. A rule with action "auto_allow" must name a toolKind
 * that is not in this list; the hub rejects any other auto_allow rule (ack error "invalid_rule").
 */
export const NEVER_AUTO_ALLOW: readonly ToolKind[] = ["execute", "delete", "fetch", "other"];

export function isAutoAllowable(kind: ToolKind | undefined): boolean {
  return kind !== undefined && !NEVER_AUTO_ALLOW.includes(kind);
}

export interface Rule {
  id: string;
  match: RuleMatch;
  action: RuleAction;
}

// ---------- pets ----------
export type Species = "dog" | "cat" | "bird" | "robot";
export const DEFAULT_SPECIES: Species = "dog";

export interface Pet {
  id: PetId;
  agentId: AgentId;
  species: Species;
  skin: string; // "default" unless chosen
  name: string;
  personality: string; // short free text, e.g. "eager, loyal, a bit goofy"
  mood: number; // 0..100
  xp: number;
}

export interface ChatMessage {
  id: string;
  petId?: PetId;
  role: "user" | "pet";
  text: string;
  ts: UnixMs;
  sessionId?: SessionId;
}

export type NotificationLevel = "info" | "success" | "warning" | "alert";
export interface HubNotification {
  id: string;
  level: NotificationLevel;
  title: string;
  body: string;
  sessionId?: SessionId;
  agentId?: AgentId;
  ts: UnixMs;
}

// ---------- hooks ----------
export type HookSource = "claude" | "codex" | "copilot";

// ---------- client -> hub ----------
export type ClientKind = "desktop" | "vscode" | "mobile" | "cli";

export type ClientMsg =
  | { type: "hello"; id: string; client: ClientKind; clientVersion: string; token: string }
  | { type: "list_agents"; id: string; refresh?: boolean }
  | { type: "list_sessions"; id: string; includeEnded?: boolean }
  | { type: "get_events"; id: string; sessionId: SessionId; sinceId?: number; limit?: number }
  | { type: "start_session"; id: string; agentId: AgentId; cwd: string; prompt?: string }
  | { type: "prompt"; id: string; sessionId: SessionId; text: string }
  | { type: "cancel"; id: string; sessionId: SessionId }
  /** optionId null = cancel/deny without choosing an option */
  | { type: "resolve_permission"; id: string; requestId: RequestId; optionId: string | null }
  | { type: "list_history"; id: string; agentId: AgentId; cwd?: string }
  | { type: "resume_session"; id: string; agentId: AgentId; acpSessionId: string; cwd: string }
  | { type: "chat"; id: string; petId?: PetId; text: string }
  | { type: "update_pet"; id: string; petId: PetId; patch: Partial<Pick<Pet, "species" | "name" | "skin" | "personality">> }
  | { type: "set_rules"; id: string; rules: Rule[] };

export type ClientMsgType = ClientMsg["type"];

/** `ack.result` shape per request type. */
export interface AckResults {
  hello: Welcome;
  list_agents: { agents: Agent[] };
  list_sessions: { sessions: Session[] };
  get_events: { events: HubEvent[] };
  start_session: { session: Session };
  prompt: Record<string, never>;
  cancel: Record<string, never>;
  resolve_permission: Record<string, never>;
  list_history: { sessions: HistorySession[] };
  resume_session: { session: Session };
  chat: { message: ChatMessage }; // the stored user message; pet reply arrives as `chat` ServerMsg
  update_pet: { pet: Pet };
  set_rules: { rules: Rule[] };
}

// ---------- hub -> client ----------
export interface Welcome {
  protocolVersion: number;
  hubVersion: string;
  agents: Agent[];
  sessions: Session[];
  pets: Pet[];
  pendingPermissions: PermissionRequest[];
  rules: Rule[];
}

/**
 * Handshake: first client frame MUST be `hello` (within 5 s) or the hub closes.
 * Hub answers `ack{id, ok:true, result: Welcome}`; bad token -> `ack{ok:false, error:"unauthorized"}` then close.
 * Every other ClientMsg gets exactly one `ack` with the same `id`.
 */
export type ServerMsg =
  | { type: "ack"; id: string; ok: boolean; result?: unknown; error?: string }
  | { type: "agents"; agents: Agent[] }
  | { type: "session"; session: Session }
  | { type: "event"; event: HubEvent }
  | { type: "permission_request"; request: PermissionRequest }
  | { type: "permission_resolved"; requestId: RequestId; sessionId: SessionId; outcome: PermissionOutcome; by: ResolvedBy }
  | { type: "pet"; pet: Pet }
  | { type: "chat"; message: ChatMessage }
  | { type: "notification"; notification: HubNotification };

export type ServerMsgType = ServerMsg["type"];
