// @krypto/protocol — wire types shared by hub, desktop, VS Code, mobile.
// SOURCE OF TRUTH for hub <-> client messages. The Rust mirror lives in
// crates/protocol/src/lib.rs and must stay byte-compatible (camelCase fields,
// snake_case `type` tags). Change both together.

export const PROTOCOL_VERSION = 8;
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

/** ios / android are phones (migration step 7); phones never appear in `machines`, only in `devices`. */
export type MachineOs = "windows" | "macos" | "linux" | "ios" | "android" | "other";

export interface Machine {
  id: MachineId;
  /** Display name, e.g. "Mac mini", "Windows PC". Defaults to the OS host name. */
  name: string;
  os: MachineOs;
  lastSeen: UnixMs;
  online: boolean;
  hubVersion?: string;
  /**
   * Last local user input seen by that machine's clients (desktop cursor/keys, VS Code focus/edits).
   * The active machine is the online machine with the latest value within USER_ACTIVE_WINDOW_MS;
   * if none qualifies, every machine treats itself as active.
   */
  userActiveAt?: UnixMs;
}

/** Clients report user activity at most this often (ClientMsg `user_activity`). */
export const USER_ACTIVITY_REPORT_MS = 10_000;
/** Activity older than this does not make a machine the active one. */
export const USER_ACTIVE_WINDOW_MS = 10 * 60_000;

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
  /** The machine the decision came from when it was not the owner (a remote approval). */
  byMachineId?: MachineId;
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
  /** Commands the bot understood for another machine; several = it is asking which one was meant. */
  proposals?: ChatProposal[];
}

/**
 * A command the bot understood but does not run by itself (another machine's agent). The client shows it with a
 * confirm step and, on confirm, sends the ordinary ClientMsg, which the hubs validate as usual:
 * start -> start_session{agentId, cwd, prompt: text, machineId, confirmed: true}; prompt -> prompt{sessionId, text}.
 */
export interface ChatProposal {
  id: string;
  kind: "start" | "prompt";
  machineId: MachineId;
  agentId: AgentId;
  /** start: a folder from that machine's repo index. */
  cwd?: string;
  repoName?: string;
  /** prompt: the open session that gets `text`. */
  sessionId?: SessionId;
  text: string;
  /** One line for the button, e.g. "Start Claude Code on Mac mini in ai-pet: run the tests". */
  summary: string;
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
  /** Live link path (absent when offline). */
  path?: LinkPath;
  /** Set when another paired hub introduced this device (group roster, migration step 7). */
  introducedBy?: MachineId;
  /** OS from the device's pair_request / hello / roster_intro, when known (phones: ios / android). */
  os?: MachineOs;
}

/** A device announced through `roster_intro`. */
export interface IntroDevice {
  id: string;
  kind: DeviceKind;
  name: string;
  /** Base64 (standard, padded) X25519 public key. */
  publicKey: string;
  addr?: string;
  os?: MachineOs;
}

/** This hub's peer-link status. */
export interface PeerStatus {
  /** Bound peer-listener address, or null when the listener is off (the default). */
  listening: string | null;
  /** This hub's base64 X25519 public key. */
  publicKey: string;
  /** Relay connection (migration step 6). `url` null = relay off. */
  relay: RelayStatus;
}

/**
 * `lastError`: unreachable | timeout | bad_key | unavailable | rate_limited | bad_auth | bad_frame |
 * unsupported_version | replaced | closed.
 */
export interface RelayStatus {
  url: string | null;
  connected: boolean;
  lastError?: string;
}

/** How a paired machine is reached right now. */
export type LinkPath = "lan" | "relay";

/**
 * A folder a hub runs agents in (its repo index, migration step 5): canonical cwds of its local sessions plus
 * config.toml `[repos] paths`. Remote start is allowed only in these folders or under `[peer] start_roots`.
 * `name` is the folder name, made unique per machine ("api (work)").
 */
export interface RepoRef {
  name: string;
  path: string;
  lastUsedAt?: UnixMs;
}

/** A hub shares at most this many repos (newest first). */
export const REPO_INDEX_MAX = 200;

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

/**
 * Owner -> peer live update for the read-only mirror. Every id inside carries the owner's machineId;
 * receivers drop any item whose machineId is not the sender's device id.
 */
export type MirrorUpdate =
  | { type: "session"; session: Session }
  | { type: "event"; event: HubEvent }
  | { type: "pet"; pet: Pet }
  | { type: "permission_request"; request: PermissionRequest }
  | { type: "permission_resolved"; requestId: RequestId; sessionId: SessionId; outcome: PermissionOutcome; by: ResolvedBy; byMachineId?: MachineId }
  | { type: "notification"; notification: HubNotification };

// ---------- intents: act on another machine's session (migration step 4) ----------
/**
 * What a hub asks an owning hub to do. The owner validates and applies it, writes the audit log,
 * then answers intent_ack; the resulting state reaches everyone through the mirror.
 * approve: optionId must be an allow_once option of a pending request (allow_always can only be chosen on the
 * owner: `forbidden`); deny: a reject_* option or null.
 * start: the agent runs on the owner (the machine that has the code). cwd is canonicalized there and must be a folder
 * the owner already knows (a previous local session's cwd, or under config.toml `[peer] start_roots`); network-share and
 * device paths are refused (`forbidden`). Sessions started or prompted by another machine never get rule auto-approval:
 * every request they raise asks a human.
 */
export type Intent =
  | { kind: "approve"; requestId: RequestId; optionId: string }
  | { kind: "deny"; requestId: RequestId; optionId: string | null }
  | { kind: "prompt"; sessionId: SessionId; text: string }
  | { kind: "start"; agentId: AgentId; cwd: string; prompt?: string }
  | { kind: "cancel"; sessionId: SessionId };

export type IntentKind = Intent["kind"];

export type IntentRejectReason =
  | "already_decided"
  | "not_found"
  | "not_owner"
  | "forbidden"
  | "unconfirmed"
  | "invalid"
  | "rate_limited";

/** The origin hub gives up waiting for intent_ack after this long (client gets `timeout`). */
export const INTENT_TIMEOUT_MS = 15_000;
/** Owners reject intents beyond this many per second per peer (`rate_limited`). */
export const INTENT_RATE_PER_SEC = 20;

/** Event payloads larger than this (serialized) are mirrored as a TruncatedPayload. */
export const MIRROR_PAYLOAD_MAX_BYTES = 64 * 1024;
/** A sync sends at most this many of the newest events (sync_done.truncated says if more existed). */
export const MIRROR_SYNC_MAX_EVENTS = 2000;
/** Senders split mirror_events batches so each serialized body stays under this. */
export const MIRROR_FRAME_MAX_BYTES = 200 * 1024;

/** Stands in for a mirrored event payload that was too large; open the session on its machine to see it. */
export interface TruncatedPayload {
  truncated: true;
  bytes: number;
}

export function isTruncatedPayload(p: unknown): p is TruncatedPayload {
  return typeof p === "object" && p !== null && (p as { truncated?: unknown }).truncated === true;
}

export type PeerBody =
  | { type: "pair_request"; code: string; machine: Machine; addr?: string; ts: UnixMs }
  /** `reqSeq` echoes the pair_request envelope seq, so an old captured pair_accept cannot answer a new request. */
  | { type: "pair_accept"; machine: Machine; addr?: string; reqSeq: number; ts: UnixMs }
  | { type: "pair_reject"; reason: PairRejectReason; ts: UnixMs }
  /** First frame on every link after pairing, both directions. */
  | { type: "hello"; machine: Machine; addr?: string; ts: UnixMs }
  | { type: "presence"; presence: Presence }
  | { type: "bye"; reason: ByeReason; ts: UnixMs }
  /** Live mirror update, owner -> peer (migration step 3). */
  | { type: "mirror"; update: MirrorUpdate }
  /** Sent by a peer after hello: "send me your state and events with seq > sinceSeq" (0 = everything). */
  | { type: "sync_request"; sinceSeq: number }
  /** Owner's reply to sync_request: open + recent sessions, pets, pending permissions (replaces the mirror's copy). */
  | { type: "mirror_snapshot"; sessions: Session[]; pets: Pet[]; pendingPermissions: PermissionRequest[] }
  /** Batches of events, oldest first, each batch under MIRROR_FRAME_MAX_BYTES. */
  | { type: "mirror_events"; events: HubEvent[] }
  /** End of a sync. `seq` = highest event seq sent; `truncated` = older events were skipped. */
  | { type: "sync_done"; seq: number; truncated: boolean }
  /**
   * Origin hub -> owner (migration step 4). `client` = the local client that asked; `confirmed` = the user confirmed
   * on that device (required for approve and start). intentId is unique; the owner answers a repeat with the same ack.
   */
  | { type: "intent"; intentId: string; client: ClientKind; confirmed: boolean; intent: Intent; ts: UnixMs }
  /** Owner -> origin. `result` has the same shape as the matching ClientMsg ack result (start: `{session}`). */
  | { type: "intent_ack"; intentId: string; status: "applied" | "rejected"; reason?: IntentRejectReason; result?: unknown }
  /**
   * Group roster (migration step 7). After pairing a new device a hub sends this to every other linked hub (the new
   * device) and to the new device (the other hubs). Accepted only from a paired hub; never replaces an existing
   * device's key; receivers audit it and tell the user.
   */
  | { type: "roster_intro"; devices: IntroDevice[] };

export type PeerBodyType = PeerBody["type"];

// ---------- relay (migration step 6) ----------
/**
 * The relay (Cloudflare Worker + one Durable Object mailbox per device) routes PeerEnvelopes between devices that
 * are not on the same LAN. It sees only public keys and ciphertext. A device's relay address IS its X25519 public key
 * (base64url, unpadded, 43 chars), so nobody can claim another device's mailbox.
 * Connect: `wss://<relay>/v1/connect?pk=<pk>&k=<relay access key>`. Every frame is JSON text.
 * The access key (base64url, 32 random bytes, a Worker secret RELAY_ACCESS_KEY) is checked by the Worker in constant
 * time before any mailbox is touched, so only the owner's devices can use a deployed relay. It travels inside invite
 * URIs (`rk=`) and is stored write-only in hub settings.
 */
export const RELAY_PROTOCOL_VERSION = 1;
/** Text the device boxes in its auth answer: `${RELAY_AUTH_CONTEXT}:${challengeNonce}:${relayHost}`. */
export const RELAY_AUTH_CONTEXT = "krypto-relay-auth-v1";
/** A mailbox accepts frames only from these many allowed senders. */
export const RELAY_MAX_ALLOWED = 64;
/** Frames queued for an offline device: at most this many, this many bytes, kept this long. */
export const RELAY_QUEUE_MAX_FRAMES = 256;
export const RELAY_QUEUE_MAX_BYTES = 2 * 1024 * 1024;
export const RELAY_QUEUE_TTL_MS = 24 * 60 * 60 * 1000;
/** Same cap as the LAN link. */
export const RELAY_FRAME_MAX_BYTES = 320 * 1024;

/** Relay -> device. */
export type RelayServerFrame =
  /**
   * First frame: prove you hold the secret key for `pk`. `relayPk` is a fresh X25519 key for this connection.
   * `nonce` byte 15 is always >= 2: envelope nonces use 0/1 there, so the auth box (same long-term key) can never reuse
   * an envelope's nonce. Clients refuse a challenge whose nonce byte 15 is 0 or 1, or whose relayPk is their own key
   * or any paired device's key.
   */
  | { t: "challenge"; nonce: string; relayPk: string; v: 1 }
  | { t: "ok"; queued: number }
  /** A frame from an authenticated sender (`from` = its public key). Check it matches the roster key for env.from. */
  | { t: "msg"; from: string; env: PeerEnvelope }
  | { t: "error"; code: RelayErrorCode; to?: string }
  | { t: "pong"; ts: UnixMs };

export type RelayErrorCode =
  | "bad_auth"
  | "bad_frame"
  | "too_large"
  | "not_allowed"
  | "queue_full"
  | "rate_limited";

/** Device -> relay. */
export type RelayClientFrame =
  /** `box` = nacl.box(utf8(`${RELAY_AUTH_CONTEXT}:${nonce}:${host}`), challenge nonce, relayPk, device secret key). */
  | { t: "auth"; box: string }
  /** Replace this mailbox's allowed senders (public keys of paired devices). */
  | { t: "allow"; pks: string[] }
  /** One-shot pairing slot: a frame carrying this token may arrive once from any sender before `exp`. Max 4 live. */
  | { t: "allow_pairing"; token: string; exp: UnixMs }
  /** `pairing` = the inviter's token from the invite URI (only for the first pair_request). */
  | { t: "send"; to: string; env: PeerEnvelope; pairing?: string }
  /** Phone only: content-free wake ("Your pet needs you") when a frame is queued while it is offline. */
  | { t: "register_push"; expoToken: string }
  /** Phone only (step 7): forget this mailbox's push token (push turned off or phone unpaired). */
  | { t: "unregister_push" }
  | { t: "ping"; ts: UnixMs };

// ---------- settings (migration step 5b: everything configurable in the UI) ----------
/** A user-defined ACP agent (config.toml [[custom_agents]]). The hub runs `command` when a session starts. */
export interface CustomAgentSetting {
  id: string;
  name: string;
  command: string[];
}

/**
 * The hub's editable settings (config.toml + defaults). Secrets are never sent: only whether they are set.
 * `overriddenBy` lists keys whose effective value comes from a CLI flag or env var instead (e.g. "hub.port": "--port").
 */
export interface HubSettings {
  hub: {
    port: number;
    offline: boolean;
    hookSessionStaleHours: number;
    ownedSessionIdleHours: number;
    eventRetentionDays: number;
    /** "HH:MM" local time, or "" for off. */
    dailyReportTime: string;
  };
  bot: { model: string; anthropicApiKeySet: boolean; ollamaUrl: string | null; ollamaModel: string | null };
  watchdog: { waitingNudgeAfterMs: number; repeatEveryMs: number; tickSecs: number };
  /**
   * `listen` = peer-listener address ("ip:port") or null when off; `acceptIntents` = remote control on;
   * `acceptRosterIntros` = add devices another paired hub introduces (`roster_intro`, step 7; default true).
   */
  peer: { listen: string | null; acceptIntents: boolean; startRoots: string[]; acceptRosterIntros: boolean };
  repos: { paths: string[] };
  /** Relay URL (wss://...) or null when off (migration step 6); the access key is write-only. */
  relay: { url: string | null; accessKeySet: boolean };
  customAgents: CustomAgentSetting[];
  overriddenBy: Record<string, string>;
}

/**
 * Partial update. Omitted keys are unchanged. Arrays replace the whole list. `bot.anthropicApiKey`: a string sets it,
 * null clears it (write-only; never read back).
 */
export interface HubSettingsPatch {
  hub?: Partial<HubSettings["hub"]>;
  bot?: Partial<Omit<HubSettings["bot"], "anthropicApiKeySet">> & { anthropicApiKey?: string | null };
  watchdog?: Partial<HubSettings["watchdog"]>;
  peer?: Partial<HubSettings["peer"]>;
  repos?: Partial<HubSettings["repos"]>;
  relay?: { url?: string | null; accessKey?: string | null };
  customAgents?: CustomAgentSetting[];
}

/** Agent hooks the hub can install (same targets as `krypto install-hooks`). copilot-vscode is per repo. */
export type HookTargetName = "claude" | "codex" | "copilot-cli" | "copilot-vscode";

export interface HookStatus {
  target: HookTargetName;
  /** copilot-vscode only: the repo whose .github/hooks it covers. */
  repo?: string;
  /** The agent itself was found on this machine. */
  agentInstalled: boolean;
  state: "installed" | "outdated" | "missing" | "unsupported";
  /** The agent config file the hooks live in. */
  configPath: string;
  detail?: string;
}

/** Views the desktop panel can be asked to show (`open_panel`). */
export type PanelView = "home" | "settings" | "machines" | "approvals" | "pets" | "rules" | "chat" | "setup";

// ---------- hooks ----------
export type HookSource = "claude" | "codex" | "copilot";

// ---------- client -> hub ----------
export type ClientKind = "desktop" | "vscode" | "mobile" | "cli";

export type ClientMsg =
  | { type: "hello"; id: string; client: ClientKind; clientVersion: string; token: string }
  /** `machineId` = another machine's agents as last reported in its presence (read-only; refresh is ignored). */
  | { type: "list_agents"; id: string; refresh?: boolean; machineId?: MachineId }
  | { type: "list_sessions"; id: string; includeEnded?: boolean }
  | { type: "get_events"; id: string; sessionId: SessionId; sinceSeq?: number; limit?: number }
  /**
   * `machineId` (default: this machine) picks where the agent runs; another machine's start is forwarded as an
   * intent and needs `confirmed: true` (the user confirmed on this device).
   */
  | { type: "start_session"; id: string; agentId: AgentId; cwd: string; prompt?: string; machineId?: MachineId; confirmed?: boolean }
  | { type: "prompt"; id: string; sessionId: SessionId; text: string }
  | { type: "cancel"; id: string; sessionId: SessionId }
  /**
   * optionId null = cancel/deny without choosing an option. For another machine's request the hub forwards an
   * intent to the owner; an allow option then needs `confirmed: true` (the user confirmed on this device).
   * Remote errors (any ClientMsg naming another machine's id): already_decided, not_found, unconfirmed, forbidden,
   * invalid, rate_limited, offline (owner not linked), timeout (no ack within INTENT_TIMEOUT_MS; the outcome may still
   * arrive through the mirror).
   */
  | { type: "resolve_permission"; id: string; requestId: RequestId; optionId: string | null; confirmed?: boolean }
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
   * `forPhone`: a relay-only invite for a phone's QR scanner (no addr); needs the relay (error `relay_off`).
   * Errors: peer_listener_off, relay_off, invalid.
   */
  | { type: "create_invite"; id: string; ttlSecs?: number; addr?: string; forPhone?: boolean }
  /** Pair with another hub from its invite URI. Errors: bad_uri, self_invite, unreachable, invite_rejected, timeout. */
  | { type: "join_peer"; id: string; uri: string }
  /** Unpair a device: its links close, it can no longer connect, and the peer is told. Errors: not_found. */
  | { type: "revoke_device"; id: string; deviceId: string }
  /** The user is active on this machine (throttled by clients to USER_ACTIVITY_REPORT_MS). Hub stamps the time. */
  | { type: "user_activity"; id: string }
  /** A machine's repo index (default: this machine; another machine: from its latest presence). */
  | { type: "list_repos"; id: string; machineId?: MachineId }
  /** Settings are local-only: only clients on this machine's localhost API can read or change them. */
  | { type: "get_settings"; id: string }
  /**
   * Validated first (error "invalid:<field>: <reason>", nothing changed), then written to config.toml (comments kept)
   * and applied at runtime where possible; keys that need a hub restart come back in `restartRequired`.
   */
  | { type: "update_settings"; id: string; patch: HubSettingsPatch }
  | { type: "hook_status"; id: string }
  /** preview = the exact change, nothing written; apply / remove write the agent's config. */
  | { type: "install_hooks"; id: string; target: HookTargetName; repo?: string; action: "preview" | "apply" | "remove" }
  /** Ask the desktop app to show a view (e.g. from VS Code). Ack `{delivered}` = a desktop client was connected. */
  | { type: "open_panel"; id: string; view: PanelView }
  /**
   * Local only. The hub acks, starts a replacement process with the same arguments, says bye to peers and exits;
   * running sessions end. Clients reconnect on their own. The running port is in data_dir/runtime.json.
   */
  | { type: "restart_hub"; id: string };

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
  /** `introduced`: devices the revoked hub introduced that are still paired here (step 7), so the UI can offer to remove them too. */
  revoke_device: { introduced?: Array<{ id: string; name: string; kind: DeviceKind }> };
  user_activity: Record<string, never>;
  list_repos: { repos: RepoRef[] };
  get_settings: { settings: HubSettings; restartRequired: string[]; configPath: string };
  update_settings: { settings: HubSettings; restartRequired: string[]; applied: string[] };
  hook_status: { hooks: HookStatus[] };
  install_hooks: { plan: string; applied: boolean; status: HookStatus };
  open_panel: { delivered: boolean };
  restart_hub: Record<string, never>;
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
  /** Sessions, pets and pending permissions of every machine: this hub's own plus the read-only mirror of paired hubs. */
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
  /** `byMachineId` = the machine the decision came from when it was not the owner (a remote approval). */
  | { type: "permission_resolved"; requestId: RequestId; sessionId: SessionId; outcome: PermissionOutcome; by: ResolvedBy; byMachineId?: MachineId }
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
  | { type: "peer_status"; peer: PeerStatus }
  /** Settings changed (by any local client). */
  | { type: "settings"; settings: HubSettings; restartRequired: string[] }
  /** A client asked the desktop app to show this view. */
  | { type: "open_panel"; view: PanelView };

export type ServerMsgType = ServerMsg["type"];
