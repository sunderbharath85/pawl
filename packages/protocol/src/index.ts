// @krypto/protocol — wire types shared by hub, desktop, VS Code, mobile.
// SOURCE OF TRUTH for hub <-> client messages. The Rust mirror lives in
// crates/protocol/src/lib.rs and must stay byte-compatible (camelCase fields,
// snake_case `type` tags). Change both together.

export const PROTOCOL_VERSION = 3;
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

// ---------- machines (one hub per machine) ----------
/** Random, generated once per hub, stored in the data dir. 12 chars of [a-z0-9]; never contains ":". */
export type MachineId = string;
export const MACHINE_ID_RE = /^[a-z0-9]{12}$/;

export type MachineOs = "windows" | "macos" | "linux" | "other";

export interface Machine {
  id: MachineId;
  /** Display name, e.g. "Mac mini", "Windows PC". Defaults to the OS host name. */
  name: string;
  os: MachineOs;
  lastSeen: UnixMs;
  online: boolean;
  hubVersion?: string;
}

/**
 * Global ids: `${machineId}:${localId}` for agents' sessions, pets, permission requests,
 * chat messages and notifications; `${machineId}:${seq}` for events.
 * Every id on the wire is global. Treat ids as opaque strings; use parseGlobalId only for routing.
 */
export function globalId(machineId: MachineId, localId: string | number): string {
  return `${machineId}:${localId}`;
}

export function parseGlobalId(id: string): { machineId: MachineId; localId: string } | null {
  const i = id.indexOf(":");
  if (i <= 0) return null;
  const machineId = id.slice(0, i);
  const localId = id.slice(i + 1);
  return MACHINE_ID_RE.test(machineId) && localId.length > 0 ? { machineId, localId } : null;
}

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
  /** The machine this agent is installed on (agents are per machine). */
  machineId: MachineId;
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
  /** Global: `${machineId}:${localId}`. */
  id: SessionId;
  /** Owning machine: the only source of truth for this session. */
  machineId: MachineId;
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
  machineId: MachineId;
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
  /** Global: `${machineId}:${seq}`. */
  id: string;
  /** Monotonic per-machine sequence (SQLite rowid). Cursor for get_events.sinceSeq. */
  seq: number;
  machineId: MachineId;
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
  /** Global: `${machineId}:${localId}`. */
  requestId: RequestId;
  machineId: MachineId;
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
  /** Global: `${machineId}:${localId}`. One pet per agent per machine. */
  id: PetId;
  machineId: MachineId;
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
  machineId: MachineId;
  petId?: PetId;
  role: "user" | "pet";
  text: string;
  ts: UnixMs;
  sessionId?: SessionId;
}

export type NotificationLevel = "info" | "success" | "warning" | "alert";
export interface HubNotification {
  id: string;
  machineId: MachineId;
  level: NotificationLevel;
  title: string;
  body: string;
  sessionId?: SessionId;
  agentId?: AgentId;
  ts: UnixMs;
}

// ---------- devices + peer link (migration step 2) ----------
export type DeviceKind = "hub" | "phone";

/** A paired device (the pairing roster; never includes this hub). A hub's device id is its machineId. */
export interface Device {
  id: string;
  kind: DeviceKind;
  name: string;
  /** Base64 (standard, padded) X25519 public key. */
  publicKey: string;
  pairedAt: UnixMs;
  lastSeen?: UnixMs;
  /** The device's peer-listener address "host:port", when known. */
  addr?: string;
}

/** This hub's peer-link status. */
export interface PeerStatus {
  /** Bound peer-listener address, or null when the listener is off (the default). */
  listening: string | null;
  /** This hub's base64 X25519 public key. */
  publicKey: string;
}

/** A repo a hub can run agents in. Filled in migration step 5; hubs send [] until then. */
export interface RepoRef {
  name: string;
  path: string;
}

/** Heartbeat a hub sends each paired hub every PRESENCE_INTERVAL_MS and on change. */
export interface Presence {
  machine: Machine;
  agents: Agent[];
  repos: RepoRef[];
  pets: Pet[];
  /** Last local user input on this machine (step 3 uses it for "runs to you"). */
  userActiveAt?: UnixMs;
  ts: UnixMs;
}

export const PEER_PROTOCOL_VERSION = 1;
export const PEER_DEFAULT_PORT = 47801;
/** The only route the peer listener serves (WebSocket). */
export const PEER_PATH = "/peer";
export const PRESENCE_INTERVAL_MS = 15_000;
/** A peer with no authenticated frame for this long is offline. */
export const PRESENCE_OFFLINE_MS = 45_000;

/**
 * Hub <-> hub frame (JSON text over WebSocket). `box` is a NaCl box (X25519 + XSalsa20-Poly1305)
 * of the UTF-8 JSON `PeerBody`, from the sender's secret key to the receiver's public key.
 * Invite URI: `krypto://peer?v=1&mid=<machineId>&pk=<base64url pk>&addr=<host:port>&code=<base64url 16 bytes>&exp=<unix ms>`.
 */
export interface PeerEnvelope {
  v: 1;
  /** Sender device id. */
  from: string;
  /** Receiver device id. */
  to: string;
  /** Per (from, to), strictly increasing across restarts; the receiver drops seq <= the last it accepted. */
  seq: number;
  /**
   * Base64, 24 bytes: 15 random || dir (1 if sender pk < receiver pk bytewise, else 0) || seq as u64 big-endian.
   * NaCl box has no AAD, so binding seq and direction into the nonce authenticates them: open() rejects a nonce
   * whose seq != `seq` or whose dir is wrong (stops seq-bumped replays and frames reflected back to their sender).
   */
  nonce: string;
  /** Base64 ciphertext. */
  box: string;
  /** Base64 sender public key. Only on `pair_request` (the receiver does not know the sender yet). */
  pk?: string;
}

/** One reason for every bad invite (wrong, expired or used code) so the listener is no oracle. */
export type PairRejectReason = "invalid_invite" | "bad_request";
export type ByeReason = "revoked" | "shutdown";

export type PeerBody =
  | { type: "pair_request"; code: string; machine: Machine; addr?: string; ts: UnixMs }
  /** `reqSeq` echoes the pair_request envelope seq, so an old captured pair_accept cannot answer a new request. */
  | { type: "pair_accept"; machine: Machine; addr?: string; reqSeq: number; ts: UnixMs }
  | { type: "pair_reject"; reason: PairRejectReason; ts: UnixMs }
  /** First frame on every link after pairing, both directions. */
  | { type: "hello"; machine: Machine; addr?: string; ts: UnixMs }
  | { type: "presence"; presence: Presence }
  | { type: "bye"; reason: ByeReason; ts: UnixMs };

export type PeerBodyType = PeerBody["type"];

// ---------- hooks ----------
export type HookSource = "claude" | "codex" | "copilot";

// ---------- client -> hub ----------
export type ClientKind = "desktop" | "vscode" | "mobile" | "cli";

export type ClientMsg =
  | { type: "hello"; id: string; client: ClientKind; clientVersion: string; token: string }
  | { type: "list_agents"; id: string; refresh?: boolean }
  | { type: "list_sessions"; id: string; includeEnded?: boolean }
  | { type: "get_events"; id: string; sessionId: SessionId; sinceSeq?: number; limit?: number }
  | { type: "start_session"; id: string; agentId: AgentId; cwd: string; prompt?: string }
  | { type: "prompt"; id: string; sessionId: SessionId; text: string }
  | { type: "cancel"; id: string; sessionId: SessionId }
  /** optionId null = cancel/deny without choosing an option */
  | { type: "resolve_permission"; id: string; requestId: RequestId; optionId: string | null }
  | { type: "list_history"; id: string; agentId: AgentId; cwd?: string }
  | { type: "resume_session"; id: string; agentId: AgentId; acpSessionId: string; cwd: string }
  | { type: "chat"; id: string; petId?: PetId; text: string }
  | { type: "update_pet"; id: string; petId: PetId; patch: Partial<Pick<Pet, "species" | "name" | "skin" | "personality">> }
  | { type: "set_rules"; id: string; rules: Rule[] }
  /** Rename this hub's machine (only the local machine for now). */
  | { type: "update_machine"; id: string; machineId: MachineId; name: string }
  /**
   * One-time pairing invite (single use; ttlSecs 60..3600, default 600). Needs the peer listener on.
   * `addr` overrides the advertised "host:port" (required when the listener binds 0.0.0.0 / ::).
   * Errors: peer_listener_off, invalid.
   */
  | { type: "create_invite"; id: string; ttlSecs?: number; addr?: string }
  /** Pair with another hub from its invite URI. Errors: bad_uri, self_invite, unreachable, invite_rejected, timeout. */
  | { type: "join_peer"; id: string; uri: string }
  /** Unpair a device: its links close, it can no longer connect, and the peer is told. Errors: not_found. */
  | { type: "revoke_device"; id: string; deviceId: string };

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
  update_machine: { machine: Machine };
  create_invite: { uri: string; expiresAt: UnixMs };
  join_peer: { device: Device; machine: Machine };
  revoke_device: Record<string, never>;
}

// ---------- hub -> client ----------
export interface Welcome {
  protocolVersion: number;
  hubVersion: string;
  /** The machine this hub runs on. */
  machine: Machine;
  /** This machine first, then every paired (not revoked) hub with its online state. */
  machines: Machine[];
  /** The pairing roster (never includes this hub). */
  devices: Device[];
  peer: PeerStatus;
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
  | { type: "notification"; notification: HubNotification }
  /** Sent when a machine is added, renamed, or goes online/offline (not on every heartbeat). */
  | { type: "machine"; machine: Machine }
  /** A paired machine was revoked (here or by the peer). */
  | { type: "machine_removed"; machineId: MachineId }
  /** The full pairing roster after any change. */
  | { type: "devices"; devices: Device[] }
  /** Peer listener started or stopped. */
  | { type: "peer_status"; peer: PeerStatus };

export type ServerMsgType = ServerMsg["type"];
