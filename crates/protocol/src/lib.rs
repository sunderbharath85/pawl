//! pawl-protocol: Rust mirror of `packages/protocol/src/index.ts` (the source of truth).
//! JSON must be identical: struct fields camelCase, enum tags/values snake_case.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const PROTOCOL_VERSION: u32 = 9;
pub const HUB_DEFAULT_PORT: u16 = 47800;
pub const HUB_HOST: &str = "127.0.0.1";
pub const HUB_DATA_DIR_NAME: &str = "pawl";
pub const HUB_TOKEN_FILE: &str = "token";

pub type AgentId = String;
pub type SessionId = String;
pub type PetId = String;
pub type RequestId = String;
pub type UnixMs = i64;

// ---------- machines (one hub per machine) ----------
/// Random, generated once per hub, stored in the data dir. 12 chars of [a-z0-9]; never contains ':'.
pub type MachineId = String;

pub fn is_valid_machine_id(id: &str) -> bool {
    id.len() == 12 && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineOs {
    Windows,
    Macos,
    Linux,
    /// Phones (migration step 7) never appear in `machines`, only in `devices`.
    Ios,
    Android,
    Other,
}

impl MachineOs {
    pub fn current() -> Self {
        match std::env::consts::OS {
            "windows" => MachineOs::Windows,
            "macos" => MachineOs::Macos,
            "linux" => MachineOs::Linux,
            _ => MachineOs::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Machine {
    pub id: MachineId,
    /// Display name, e.g. "Mac mini". Defaults to the OS host name.
    pub name: String,
    pub os: MachineOs,
    pub last_seen: UnixMs,
    pub online: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub_version: Option<String>,
    /// Last local user input seen by that machine's clients (desktop cursor/keys, VS Code focus/edits).
    /// The active machine is the online machine with the latest value within USER_ACTIVE_WINDOW_MS;
    /// if none qualifies, every machine treats itself as active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_active_at: Option<UnixMs>,
}

/// Clients report user activity at most this often (ClientMsg `user_activity`).
pub const USER_ACTIVITY_REPORT_MS: i64 = 10_000;
/// Activity older than this does not make a machine the active one.
pub const USER_ACTIVE_WINDOW_MS: i64 = 10 * 60_000;

/// Global id `${machine_id}:${local_id}`. Every id on the wire is global.
pub fn global_id(machine_id: &str, local_id: impl std::fmt::Display) -> String {
    format!("{machine_id}:{local_id}")
}

/// Split a global id into (machine_id, local_id); None if it is not a valid global id.
pub fn parse_global_id(id: &str) -> Option<(&str, &str)> {
    let (m, l) = id.split_once(':')?;
    (is_valid_machine_id(m) && !l.is_empty()).then_some((m, l))
}

pub fn now_ms() -> UnixMs {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `dirs::data_dir()/pawl` (Win %APPDATA%\pawl). Callers create it.
pub fn data_dir() -> Option<std::path::PathBuf> {
    dirs::data_dir().map(|d| d.join(HUB_DATA_DIR_NAME))
}

// ---------- devices + peer link (migration step 2) ----------
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    Hub,
    Phone,
}

/// A paired device (the pairing roster; never includes this hub). A hub's device id is its machine id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub kind: DeviceKind,
    pub name: String,
    /// Base64 (standard, padded) X25519 public key.
    pub public_key: String,
    pub paired_at: UnixMs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen: Option<UnixMs>,
    /// The device's peer-listener address "host:port", when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    /// Live link path (absent when offline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<LinkPath>,
    /// Set when another paired hub introduced this device (group roster, migration step 7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub introduced_by: Option<MachineId>,
    /// OS from the device's pair_request / hello / roster_intro, when known (phones: ios / android).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<MachineOs>,
}

/// A device announced through `roster_intro`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntroDevice {
    pub id: String,
    pub kind: DeviceKind,
    pub name: String,
    /// Base64 (standard, padded) X25519 public key.
    pub public_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<MachineOs>,
}

/// This hub's peer-link status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerStatus {
    /// Bound peer-listener address, or None when the listener is off (the default). Serialized as null.
    pub listening: Option<String>,
    /// This hub's base64 X25519 public key.
    pub public_key: String,
    /// Relay connection (migration step 6). `url` None = relay off.
    pub relay: RelayStatusInfo,
}

/// `last_error`: unreachable | timeout | bad_key | unavailable | rate_limited | bad_auth | bad_frame |
/// unsupported_version | replaced | closed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayStatusInfo {
    pub url: Option<String>,
    pub connected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// How a paired machine is reached right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkPath {
    Lan,
    Relay,
}

/// A folder a hub runs agents in (its repo index, migration step 5): canonical cwds of its local sessions plus
/// config.toml `[repos] paths`. Remote start is allowed only in these folders or under `[peer] start_roots`.
/// `name` is the folder name, made unique per machine ("api (work)").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoRef {
    pub name: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<UnixMs>,
}

/// A hub shares at most this many repos (newest first).
pub const REPO_INDEX_MAX: usize = 200;

/// A command the bot understood but does not run by itself (another machine's agent). The client shows it with a
/// confirm step and, on confirm, sends the ordinary ClientMsg, which the hubs validate as usual:
/// start -> start_session{agent_id, cwd, prompt: text, machine_id, confirmed: true}; prompt -> prompt{session_id, text}.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatProposal {
    pub id: String,
    pub kind: ChatProposalKind,
    pub machine_id: MachineId,
    pub agent_id: AgentId,
    /// start: a folder from that machine's repo index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
    /// prompt: the open session that gets `text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    pub text: String,
    /// One line for the button, e.g. "Start Claude Code on Mac mini in ai-pet: run the tests".
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatProposalKind {
    Start,
    Prompt,
}

/// Heartbeat a hub sends each paired hub every PRESENCE_INTERVAL_MS and on change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Presence {
    pub machine: Machine,
    pub agents: Vec<Agent>,
    #[serde(default)]
    pub repos: Vec<RepoRef>,
    #[serde(default)]
    pub pets: Vec<Pet>,
    /// Last local user input on this machine (step 3 uses it for "runs to you").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_active_at: Option<UnixMs>,
    pub ts: UnixMs,
    /// The sender's PROTOCOL_VERSION (v9+; absent = 8 or older). From 9 on, start / prompt / cancel go to it
    /// as A2A JSON-RPC (`PeerBody::A2a`) instead of intents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
}

/// The first PROTOCOL_VERSION whose hubs talk A2A to each other over the peer link.
pub const A2A_PEER_MIN_PROTOCOL: u32 = 9;
/// Custom A2A protocol binding (spec 5.8, 12) of the encrypted peer link: JSON-RPC 2.0 A2A 1.0 requests
/// and responses inside `PeerBody::A2a` frames.
pub const A2A_PEER_BINDING: &str = "https://sunderbharath85.github.io/pawl/a2a/binding/peer-link/v1";

/// One frame of an A2A call over the peer link (`PeerBody::A2a`), tagged `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum A2aPeerFrame {
    /// Origin -> owner: a JSON-RPC request (A2A 1.0). `client` = the local client that asked; `confirmed` = the
    /// user confirmed on that device (required for a start, as with intents).
    Request { rpc: Value, client: ClientKind, confirmed: bool },
    /// Owner -> origin: the JSON-RPC response of a unary method.
    Response { rpc: Value },
    /// Owner -> origin: one JSON-RPC response of a stream (`SendStreamingMessage`, `SubscribeToTask`).
    Event { rpc: Value },
    /// Owner -> origin: the stream is over.
    End,
}

pub const PEER_PROTOCOL_VERSION: u8 = 1;
pub const PEER_DEFAULT_PORT: u16 = 47801;
/// The only route the peer listener serves (WebSocket).
pub const PEER_PATH: &str = "/peer";
pub const PRESENCE_INTERVAL_MS: i64 = 15_000;
/// A peer with no authenticated frame for this long is offline.
pub const PRESENCE_OFFLINE_MS: i64 = 45_000;

/// Hub <-> hub frame (JSON text over WebSocket). `box` is a NaCl box (X25519 + XSalsa20-Poly1305,
/// `crypto_box::SalsaBox`) of the UTF-8 JSON `PeerBody`, from the sender's secret key to the receiver's
/// public key. Invite URI:
/// `pawl://peer?v=1&mid=<machineId>&pk=<base64url pk>&addr=<host:port>&code=<base64url 16 bytes>&exp=<unix ms>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerEnvelope {
    pub v: u8,
    /// Sender device id.
    pub from: String,
    /// Receiver device id.
    pub to: String,
    /// Per (from, to), strictly increasing across restarts; the receiver drops seq <= the last it accepted.
    pub seq: u64,
    /// Base64, 24 bytes: 15 random || dir (1 if sender pk < receiver pk bytewise, else 0) || seq u64 BE.
    /// NaCl box has no AAD, so binding seq and direction into the nonce authenticates them: open() rejects a
    /// nonce whose seq != `seq` or whose dir is wrong (stops seq-bumped replays and reflected frames).
    pub nonce: String,
    /// Base64 ciphertext.
    #[serde(rename = "box")]
    pub ciphertext: String,
    /// Base64 sender public key. Only on `pair_request`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pk: Option<String>,
}

/// One reason for every bad invite (wrong, expired or used code) so the listener is no oracle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairRejectReason {
    InvalidInvite,
    BadRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByeReason {
    Revoked,
    Shutdown,
}

/// Owner -> peer live update for the read-only mirror. Every id inside carries the owner's machine id;
/// receivers drop any item whose machine id is not the sender's device id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum MirrorUpdate {
    Session { session: Session },
    Event { event: HubEvent },
    Pet { pet: Pet },
    PermissionRequest { request: PermissionRequest },
    PermissionResolved {
        request_id: RequestId,
        session_id: SessionId,
        outcome: PermissionOutcome,
        by: ResolvedBy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by_machine_id: Option<MachineId>,
    },
    Notification { notification: HubNotification },
}

// ---------- intents: act on another machine's session (migration step 4) ----------
/// What a hub asks an owning hub to do. The owner validates and applies it, writes the audit log,
/// then answers intent_ack; the resulting state reaches everyone through the mirror.
/// approve: option_id must be an allow_once option of a pending request (allow_always only on the owner: `forbidden`);
/// deny: a reject_* option or None.
/// start: the agent runs on the owner (the machine that has the code). cwd is canonicalized there and must be a folder
/// the owner already knows (a previous local session's cwd, or under config.toml `[peer] start_roots`); network-share
/// and device paths are refused (`forbidden`). Sessions started or prompted by another machine never get rule
/// auto-approval: every request they raise asks a human.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Intent {
    Approve { request_id: RequestId, option_id: String },
    Deny { request_id: RequestId, option_id: Option<String> },
    Prompt { session_id: SessionId, text: String },
    Start {
        agent_id: AgentId,
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    Cancel { session_id: SessionId },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentStatus {
    Applied,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentRejectReason {
    AlreadyDecided,
    NotFound,
    NotOwner,
    Forbidden,
    Unconfirmed,
    Invalid,
    RateLimited,
}

/// The origin hub gives up waiting for intent_ack after this long (client gets `timeout`).
pub const INTENT_TIMEOUT_MS: i64 = 15_000;
/// Owners reject intents beyond this many per second per peer (`rate_limited`).
pub const INTENT_RATE_PER_SEC: u32 = 20;

/// Event payloads larger than this (serialized) are mirrored as a TruncatedPayload.
pub const MIRROR_PAYLOAD_MAX_BYTES: usize = 64 * 1024;
/// A sync sends at most this many of the newest events (sync_done.truncated says if more existed).
pub const MIRROR_SYNC_MAX_EVENTS: usize = 2000;
/// Senders split mirror_events batches so each serialized body stays under this.
pub const MIRROR_FRAME_MAX_BYTES: usize = 200 * 1024;

/// Stands in for a mirrored event payload that was too large: `{"truncated":true,"bytes":n}`.
pub fn truncated_payload(bytes: usize) -> Value {
    serde_json::json!({ "truncated": true, "bytes": bytes })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum PeerBody {
    PairRequest {
        code: String,
        machine: Machine,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        addr: Option<String>,
        ts: UnixMs,
    },
    /// `req_seq` echoes the pair_request envelope seq, so an old captured pair_accept cannot answer a new request.
    PairAccept {
        machine: Machine,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        addr: Option<String>,
        req_seq: u64,
        ts: UnixMs,
    },
    PairReject { reason: PairRejectReason, ts: UnixMs },
    /// First frame on every link after pairing, both directions.
    Hello {
        machine: Machine,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        addr: Option<String>,
        ts: UnixMs,
    },
    Presence { presence: Presence },
    Bye { reason: ByeReason, ts: UnixMs },
    /// Live mirror update, owner -> peer (migration step 3).
    Mirror { update: MirrorUpdate },
    /// Sent by a peer after hello: "send me your state and events with seq > since_seq" (0 = everything).
    SyncRequest { since_seq: i64 },
    /// Owner's reply to sync_request: open + recent sessions, pets, pending permissions (replaces the mirror's copy).
    MirrorSnapshot { sessions: Vec<Session>, pets: Vec<Pet>, pending_permissions: Vec<PermissionRequest> },
    /// Batches of events, oldest first, each batch under MIRROR_FRAME_MAX_BYTES.
    MirrorEvents { events: Vec<HubEvent> },
    /// End of a sync. `seq` = highest event seq sent; `truncated` = older events were skipped.
    SyncDone { seq: i64, truncated: bool },
    /// Origin hub -> owner (migration step 4). `client` = the local client that asked; `confirmed` = the user
    /// confirmed on that device (required for approve and start). The owner answers a repeated intent_id with the same ack.
    Intent { intent_id: String, client: ClientKind, confirmed: bool, intent: Intent, ts: UnixMs },
    /// Owner -> origin. `result` has the same shape as the matching ClientMsg ack result (start: `{session}`).
    IntentAck {
        intent_id: String,
        status: IntentStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<IntentRejectReason>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
    },
    /// Group roster (migration step 7): sent after pairing a new device to every other linked hub (the new device) and
    /// to the new device (the other hubs). Accepted only from a paired hub; never replaces an existing device's key.
    RosterIntro { devices: Vec<IntroDevice> },
    /// A2A over the encrypted link (protocol v9, between v9 hubs only): one call `call_id` (unique per origin),
    /// its request and its answer frames. Replaces the start / prompt / cancel intents; approve / deny stay intents.
    A2a { call_id: String, frame: A2aPeerFrame },
}

// ---------- agents ----------
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    pub load_session: bool,
    pub list_sessions: bool,
    pub resume_session: bool,
    pub prompt_image: bool,
    pub prompt_audio: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSource {
    Registry,
    Custom,
    /// An outside A2A agent the user added by URL (Settings, `[[a2a.agents]]`); its id is
    /// `a2a:<slug>` (A2A phase A3). No command: the hub talks A2A to it.
    A2a,
}

/// Agent ids of outside A2A agents start with this (`a2a:<slug>`).
pub const A2A_AGENT_PREFIX: &str = "a2a:";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: AgentId,
    /// The machine this agent is installed on.
    pub machine_id: MachineId,
    pub name: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    pub installed: bool,
    pub source: AgentSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<AgentCapabilities>,
}

// ---------- sessions ----------
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionSource {
    Owned,
    VscodeHook,
    CliHook,
    Wrapper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Idle,
    Thinking,
    Working,
    WaitingPermission,
    Done,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// Global `${machine_id}:${local_id}`.
    pub id: SessionId,
    /// Owning machine: the only source of truth for this session.
    pub machine_id: MachineId,
    pub agent_id: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub source: SessionSource,
    pub status: SessionStatus,
    pub started_at: UnixMs,
    pub last_event_at: UnixMs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<UnixMs>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySession {
    pub machine_id: MachineId,
    pub agent_id: AgentId,
    pub acp_session_id: String,
    pub cwd: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<UnixMs>,
}

// ---------- events ----------
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    SessionStart,
    SessionEnd,
    UserMessage,
    AgentThoughtChunk,
    AgentMessageChunk,
    ToolCall,
    ToolCallUpdate,
    Plan,
    RequestPermission,
    PermissionResolved,
    Stop,
    Error,
    Notification,
    Hook,
}

impl EventType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EventType::SessionStart => "session_start",
            EventType::SessionEnd => "session_end",
            EventType::UserMessage => "user_message",
            EventType::AgentThoughtChunk => "agent_thought_chunk",
            EventType::AgentMessageChunk => "agent_message_chunk",
            EventType::ToolCall => "tool_call",
            EventType::ToolCallUpdate => "tool_call_update",
            EventType::Plan => "plan",
            EventType::RequestPermission => "request_permission",
            EventType::PermissionResolved => "permission_resolved",
            EventType::Stop => "stop",
            EventType::Error => "error",
            EventType::Notification => "notification",
            EventType::Hook => "hook",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    SwitchMode,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolLocation {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextPayload {
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallPayload {
    pub tool_call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ToolKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ToolCallStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locations: Option<Vec<ToolLocation>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    pub content: String,
    pub priority: PlanPriority,
    pub status: PlanEntryStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanPayload {
    pub entries: Vec<PlanEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StopPayload {
    pub stop_reason: StopReason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorPayload {
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookSource {
    Claude,
    Codex,
    Copilot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookPayload {
    pub source: HookSource,
    pub hook_event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub raw: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolvedBy {
    /// A person (desktop, VS Code, phone, chat).
    User,
    /// The rules engine.
    Rule,
    /// Automatic cancel: turn ended, agent withdrew the request, process exit.
    System,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionResolvedPayload {
    pub request_id: RequestId,
    pub outcome: PermissionOutcome,
    pub by: ResolvedBy,
    /// The machine the decision came from when it was not the owner (a remote approval).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub by_machine_id: Option<MachineId>,
    /// Some("voice") when the user answered by speaking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubEvent {
    /// Global `${machine_id}:${seq}`.
    pub id: String,
    /// Monotonic per-machine sequence (SQLite rowid); cursor for GetEvents.since_seq.
    pub seq: i64,
    pub machine_id: MachineId,
    pub session_id: SessionId,
    #[serde(rename = "type")]
    pub kind: EventType,
    pub payload: Value,
    pub ts: UnixMs,
}

// ---------- permissions ----------
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOptionKind {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    pub kind: PermissionOptionKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequest {
    /// Global `${machine_id}:${local_id}`.
    pub request_id: RequestId,
    pub machine_id: MachineId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub tool_call: ToolCallPayload,
    pub options: Vec<PermissionOption>,
    pub created_at: UnixMs,
    /// Two digits the owning hub assigns; a voice approval of a risky or remote request must include it (v0.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum PermissionOutcome {
    Selected { option_id: String },
    Cancelled,
}

// ---------- rules ----------
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    AutoAllow,
    Ask,
    Deny,
}

/// Security hard floor: tool kinds that are NEVER auto-approved, whatever the rules say.
/// Unknown/`Other` kinds count as risky. An `AutoAllow` rule must name a `tool_kind` outside this
/// list; the hub rejects any other auto_allow rule (ack error "invalid_rule").
pub const NEVER_AUTO_ALLOW: &[ToolKind] = &[ToolKind::Execute, ToolKind::Delete, ToolKind::Fetch, ToolKind::Other];

pub fn is_auto_allowable(kind: Option<ToolKind>) -> bool {
    matches!(kind, Some(k) if !NEVER_AUTO_ALLOW.contains(&k))
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleMatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_kind: Option<ToolKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: String,
    #[serde(rename = "match")]
    pub matcher: RuleMatch,
    pub action: RuleAction,
}

// ---------- pets ----------
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Species {
    #[default]
    Dog,
    Cat,
    Bird,
    Robot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pet {
    /// Global `${machine_id}:${local_id}`. One pet per agent per machine.
    pub id: PetId,
    pub machine_id: MachineId,
    pub agent_id: AgentId,
    pub species: Species,
    pub skin: String,
    pub name: String,
    pub personality: String,
    pub mood: u8,
    pub xp: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PetPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub species: Option<Species>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Pet,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub machine_id: MachineId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pet_id: Option<PetId>,
    pub role: ChatRole,
    pub text: String,
    pub ts: UnixMs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    /// Commands the bot understood for another machine; several = it is asking which one was meant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proposals: Vec<ChatProposal>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationLevel {
    Info,
    Success,
    Warning,
    Alert,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubNotification {
    pub id: String,
    pub machine_id: MachineId,
    pub level: NotificationLevel,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    pub ts: UnixMs,
}

// ---------- relay (migration step 6) ----------
/// The relay (Cloudflare Worker + one Durable Object mailbox per device) routes PeerEnvelopes between devices that
/// are not on the same LAN. It sees only public keys and ciphertext. A device's relay address IS its X25519 public
/// key (base64url, unpadded, 43 chars). Connect: `wss://<relay>/v1/connect?pk=<pk>&k=<relay access key>`. JSON text
/// frames. The access key (Worker secret RELAY_ACCESS_KEY) is checked by the Worker in constant time before any
/// mailbox is touched; it travels inside invite URIs (`rk=`) and is stored write-only in hub settings.
pub const RELAY_PROTOCOL_VERSION: u8 = 1;
/// Text the device boxes in its auth answer: `{RELAY_AUTH_CONTEXT}:{challenge_nonce}:{relay_host}`.
pub const RELAY_AUTH_CONTEXT: &str = "pawl-relay-auth-v1";
pub const RELAY_MAX_ALLOWED: usize = 64;
pub const RELAY_QUEUE_MAX_FRAMES: usize = 256;
pub const RELAY_QUEUE_MAX_BYTES: usize = 2 * 1024 * 1024;
pub const RELAY_QUEUE_TTL_MS: i64 = 24 * 60 * 60 * 1000;
pub const RELAY_FRAME_MAX_BYTES: usize = 320 * 1024;

/// Relay -> device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum RelayServerFrame {
    /// First frame: prove you hold the secret key for `pk`. `relay_pk` is a fresh X25519 key for this connection.
    /// `nonce` byte 15 is always >= 2 (envelope nonces use 0/1 there), so the auth box can never reuse an envelope's
    /// nonce under the same long-term key. Clients refuse a nonce with byte 15 in {0, 1} or a relay_pk equal to their
    /// own key or any paired device's key.
    Challenge { nonce: String, relay_pk: String, v: u8 },
    Ok { queued: u32 },
    /// A frame from an authenticated sender (`from` = its public key). Check it matches the roster key for env.from.
    Msg { from: String, env: PeerEnvelope },
    Error {
        code: RelayErrorCode,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
    },
    Pong { ts: UnixMs },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayErrorCode {
    BadAuth,
    BadFrame,
    TooLarge,
    NotAllowed,
    QueueFull,
    RateLimited,
}

/// Device -> relay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum RelayClientFrame {
    /// `box` = NaCl box of `{RELAY_AUTH_CONTEXT}:{nonce}:{host}` with the challenge nonce, to relay_pk, from the device key.
    Auth {
        #[serde(rename = "box")]
        ciphertext: String,
    },
    /// Replace this mailbox's allowed senders (public keys of paired devices).
    Allow { pks: Vec<String> },
    /// One-shot pairing slot: a frame carrying this token may arrive once from any sender before `exp`. Max 4 live.
    AllowPairing { token: String, exp: UnixMs },
    /// `pairing` = the inviter's token from the invite URI (only for the first pair_request).
    Send {
        to: String,
        env: PeerEnvelope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pairing: Option<String>,
    },
    /// Phone only: content-free wake when a frame is queued while it is offline.
    RegisterPush { expo_token: String },
    /// Phone only (step 7): forget this mailbox's push token (push turned off or phone unpaired).
    UnregisterPush,
    Ping { ts: UnixMs },
}

// ---------- settings (migration step 5b: everything configurable in the UI) ----------
/// A user-defined ACP agent (config.toml [[custom_agents]]). The hub runs `command` when a session starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomAgentSetting {
    pub id: String,
    pub name: String,
    pub command: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsHub {
    pub port: u16,
    pub offline: bool,
    pub hook_session_stale_hours: u32,
    pub owned_session_idle_hours: u32,
    pub event_retention_days: u32,
    /// "HH:MM" local time, or "" for off.
    pub daily_report_time: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsBot {
    pub model: String,
    pub anthropic_api_key_set: bool,
    pub ollama_url: Option<String>,
    pub ollama_model: Option<String>,
    /// The agent the default pet hands work to when the user names none; None = automatic (the most recent
    /// session's agent, else the first installed one).
    #[serde(default)]
    pub default_agent: Option<AgentId>,
    /// Which model provider the pet uses; Auto = the first configured (anthropic, openai, gemini, ollama).
    #[serde(default)]
    pub provider: BotProvider,
    #[serde(default)]
    pub openai_api_key_set: bool,
    #[serde(default)]
    pub openai_model: String,
    #[serde(default)]
    pub gemini_api_key_set: bool,
    #[serde(default)]
    pub gemini_model: String,
    /// Provider Acp: the installed agent that thinks for the pet (default "claude-code") and its model ("sonnet").
    #[serde(default)]
    pub acp_agent: AgentId,
    #[serde(default)]
    pub acp_model: String,
    /// Let the model turn free-form messages into intents; they only ever become cards or proposals the user clicks.
    #[serde(default)]
    pub understand: bool,
    /// How the pet talks: Plain (default; no *actions* or barks) or Playful.
    #[serde(default)]
    pub style: BotStyle,
    /// The default (orchestrator) pet's own name; the hub picks one from its name pool on first start.
    #[serde(default)]
    pub pet_name: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BotStyle {
    #[default]
    Plain,
    Playful,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BotProvider {
    #[default]
    Auto,
    Anthropic,
    Openai,
    Gemini,
    Ollama,
    /// An installed coding agent over ACP (Claude Code + Sonnet by default), every tool denied.
    Acp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsWatchdog {
    pub waiting_nudge_after_ms: i64,
    pub repeat_every_ms: i64,
    pub tick_secs: u64,
}

/// `listen` = peer-listener address ("ip:port") or None when off; `accept_intents` = remote control on;
/// `accept_roster_intros` = add devices another paired hub introduces (`roster_intro`, step 7; default true).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsPeer {
    pub listen: Option<String>,
    pub accept_intents: bool,
    pub start_roots: Vec<String>,
    pub accept_roster_intros: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsRepos {
    pub paths: Vec<String>,
}

/// Relay URL (wss://...) or None when off (migration step 6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsRelay {
    pub url: Option<String>,
    pub access_key_set: bool,
}

/// Default port of the localhost A2A server (`[a2a] port`; it only ever binds 127.0.0.1).
pub const A2A_DEFAULT_PORT: u16 = 47802;

/// `[a2a]` (PLAN.md "A2A orchestration", phase A2): Pawl as an A2A agent on 127.0.0.1. `server` = the
/// localhost A2A server is on (off by default); `running` / `url` / `error` = its live state. Callers are
/// outside A2A clients, each with its own bearer token (write-only: only `token_set`; `a2a_rotate_token`
/// shows a new one once) and its own `allow_approvals` (default false: it can never answer permission
/// requests; true: allow once or deny, never "always", never execute / delete / fetch).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsA2a {
    pub server: bool,
    pub port: u16,
    #[serde(default)]
    pub running: bool,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub callers: Vec<A2aCallerSetting>,
    /// Outside A2A agents Pawl delegates to (phase A3).
    #[serde(default)]
    pub agents: Vec<A2aAgentSetting>,
}

/// An outside A2A agent (`[[a2a.agents]]`): `id` `[a-z0-9-]{1,40}` (its agent id is `a2a:<id>`), the URL
/// it was added by (https, or http to this machine), its bearer token (write-only: `token_set`), the
/// last card fetched (`card`) and the last fetch error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aAgentSetting {
    pub id: String,
    pub name: String,
    pub url: String,
    pub token_set: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<A2aAgentCardSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// What an outside agent's Agent Card says, for Settings. `signature`: "unsigned" (no signatures) or
/// "unverified" (signed; Pawl does not verify card signatures). `protocol_version`: "1.0" or "0.3".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aAgentCardSummary {
    pub name: String,
    pub description: String,
    pub version: String,
    pub protocol_version: String,
    pub streaming: bool,
    pub skills: Vec<A2aSkillSummary>,
    pub signature: String,
    pub fetched_at: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aSkillSummary {
    pub id: String,
    pub name: String,
    pub description: String,
}

/// One outside agent in a patch (`agents` replaces the list). `token`: a string sets it, null clears
/// it, absent keeps it (while the id stays).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aAgentPatch {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub url: String,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub token: Option<Option<String>>,
}

/// One outside A2A caller of the localhost server (`[[a2a.callers]]`). `id`: `[a-z0-9-]{1,40}`, unique; it
/// appears in the audit log as `a2a:<id>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aCallerSetting {
    pub id: String,
    pub name: String,
    pub token_set: bool,
    pub allow_approvals: bool,
}

/// `callers` replaces the whole list; a caller keeps its token while its id stays, a new id starts
/// without one (`a2a_rotate_token`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsA2aPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub callers: Option<Vec<A2aCallerPatch>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<A2aAgentPatch>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aCallerPatch {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub allow_approvals: bool,
}

/// One delegation (A2A task, one per prompt turn) of this machine's sessions (`list_tasks`).
/// `state`: "submitted" | "working" | "input_required" | "completed" | "canceled" | "rejected" | "failed".
/// `caller`: who started it: "local", "peer:<machineId>" or "a2a:<callerId>".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct A2aTaskSummary {
    pub task_id: String,
    pub session_id: SessionId,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    pub caller: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<UnixMs>,
}

/// The hub's editable settings (config.toml + defaults). Secrets are never sent: only whether they are set.
/// `overridden_by` lists keys whose effective value comes from a CLI flag or env var (e.g. "hub.port" -> "--port").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettings {
    pub hub: HubSettingsHub,
    pub bot: HubSettingsBot,
    pub watchdog: HubSettingsWatchdog,
    pub peer: HubSettingsPeer,
    pub repos: HubSettingsRepos,
    pub relay: HubSettingsRelay,
    pub custom_agents: Vec<CustomAgentSetting>,
    #[serde(default)]
    pub overridden_by: BTreeMap<String, String>,
    /// The localhost A2A server and its callers (A2A phase A2). Boxed: keeps `ServerMsg` small.
    #[serde(default)]
    pub a2a: Box<HubSettingsA2a>,
}

/// `Some(None)` = explicit null (clear), `None` = key absent (unchanged).
fn double_option<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsHubPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_session_stale_hours: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_session_idle_hours: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_retention_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daily_report_time: Option<String>,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsBotPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Write-only: Some(Some(key)) sets it, Some(None) clears it.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub anthropic_api_key: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub ollama_url: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub ollama_model: Option<Option<String>>,
    /// Some(Some(id)) sets the default agent; Some(None) = automatic.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub default_agent: Option<Option<AgentId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<BotProvider>,
    /// Write-only: Some(Some(key)) sets it, Some(None) clears it.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub openai_api_key: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai_model: Option<String>,
    /// Write-only: Some(Some(key)) sets it, Some(None) clears it.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub gemini_api_key: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gemini_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_agent: Option<AgentId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acp_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub understand: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<BotStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pet_name: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsWatchdogPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_nudge_after_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_every_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tick_secs: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsPeerPatch {
    /// Some(None) turns the listener off.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub listen: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_intents: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_roots: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_roster_intros: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsReposPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paths: Option<Vec<String>>,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsRelayPatch {
    /// Some(None) turns the relay off.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub url: Option<Option<String>>,
    /// Write-only: Some(Some(key)) sets it, Some(None) clears it.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub access_key: Option<Option<String>>,
}

/// Shows whether a write-only secret is being set / cleared / left alone, never its value.
fn secret_patch(v: &Option<Option<String>>) -> &'static str {
    match v {
        None => "unchanged",
        Some(None) => "clear",
        Some(Some(_)) => "set (redacted)",
    }
}

impl std::fmt::Debug for HubSettingsBotPatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubSettingsBotPatch")
            .field("model", &self.model)
            .field("anthropic_api_key", &secret_patch(&self.anthropic_api_key))
            .field("ollama_url", &self.ollama_url)
            .field("ollama_model", &self.ollama_model)
            .field("default_agent", &self.default_agent)
            .field("provider", &self.provider)
            .field("openai_api_key", &secret_patch(&self.openai_api_key))
            .field("openai_model", &self.openai_model)
            .field("gemini_api_key", &secret_patch(&self.gemini_api_key))
            .field("gemini_model", &self.gemini_model)
            .field("acp_agent", &self.acp_agent)
            .field("acp_model", &self.acp_model)
            .field("understand", &self.understand)
            .field("style", &self.style)
            .field("pet_name", &self.pet_name)
            .finish()
    }
}

impl std::fmt::Debug for HubSettingsRelayPatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubSettingsRelayPatch")
            .field("url", &self.url)
            .field("access_key", &secret_patch(&self.access_key))
            .finish()
    }
}

/// Partial update. Omitted keys are unchanged; lists replace the whole list.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubSettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hub: Option<HubSettingsHubPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bot: Option<HubSettingsBotPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watchdog: Option<HubSettingsWatchdogPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<HubSettingsPeerPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repos: Option<HubSettingsReposPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<HubSettingsRelayPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_agents: Option<Vec<CustomAgentSetting>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a2a: Option<HubSettingsA2aPatch>,
}

/// Agent hooks the hub can install (same targets as `pawl install-hooks`). copilot-vscode is per repo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookTargetName {
    #[serde(rename = "claude")]
    Claude,
    #[serde(rename = "codex")]
    Codex,
    #[serde(rename = "copilot-cli")]
    CopilotCli,
    #[serde(rename = "copilot-vscode")]
    CopilotVscode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookState {
    Installed,
    Outdated,
    Missing,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookStatus {
    pub target: HookTargetName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    pub agent_installed: bool,
    pub state: HookState,
    pub config_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookAction {
    Preview,
    Apply,
    Remove,
}

/// Views the desktop panel can be asked to show (`open_panel`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelView {
    Home,
    Settings,
    Machines,
    Approvals,
    Pets,
    Rules,
    Chat,
    Setup,
}

// ---------- client -> hub ----------
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientKind {
    Desktop,
    Vscode,
    Mobile,
    Cli,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ClientMsg {
    Hello { id: String, client: ClientKind, client_version: String, token: String },
    /// `machine_id` = another machine's agents as last reported in its presence (read-only; refresh ignored).
    ListAgents {
        id: String,
        #[serde(default)]
        refresh: Option<bool>,
        #[serde(default)]
        machine_id: Option<MachineId>,
    },
    ListSessions { id: String, #[serde(default)] include_ended: Option<bool> },
    GetEvents { id: String, session_id: SessionId, #[serde(default)] since_seq: Option<i64>, #[serde(default)] limit: Option<u32> },
    /// `machine_id` (default: this machine) picks where the agent runs; another machine's start is forwarded as
    /// an intent and needs `confirmed: Some(true)`.
    StartSession {
        id: String,
        agent_id: AgentId,
        cwd: String,
        #[serde(default)]
        prompt: Option<String>,
        #[serde(default)]
        machine_id: Option<MachineId>,
        #[serde(default)]
        confirmed: Option<bool>,
    },
    Prompt { id: String, session_id: SessionId, text: String },
    Cancel { id: String, session_id: SessionId },
    /// For another machine's request the hub forwards an intent; an allow option then needs `confirmed: Some(true)`.
    /// Remote errors: already_decided, not_found, unconfirmed, forbidden, invalid, rate_limited, offline, timeout.
    ResolvePermission {
        id: String,
        request_id: RequestId,
        option_id: Option<String>,
        #[serde(default)]
        confirmed: Option<bool>,
    },
    ListHistory { id: String, agent_id: AgentId, #[serde(default)] cwd: Option<String> },
    ResumeSession { id: String, agent_id: AgentId, acp_session_id: String, cwd: String },
    Chat {
        id: String,
        #[serde(default)]
        pet_id: Option<PetId>,
        text: String,
        /// Some("voice") = a speech transcript (v0.2).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        via: Option<String>,
    },
    UpdatePet { id: String, pet_id: PetId, patch: PetPatch },
    SetRules { id: String, rules: Vec<Rule> },
    /// Rename this hub's machine (local machine only for now).
    UpdateMachine { id: String, machine_id: MachineId, name: String },
    /// One-time pairing invite (single use; ttl_secs 60..=3600, default 600). Needs the peer listener on.
    /// `addr` overrides the advertised "host:port" (required when the listener binds 0.0.0.0 / ::).
    /// Errors: peer_listener_off, invalid. Ack result `{uri, expiresAt}`.
    CreateInvite {
        id: String,
        #[serde(default)]
        ttl_secs: Option<u32>,
        #[serde(default)]
        addr: Option<String>,
        /// A relay-only invite for a phone's QR scanner (no addr); needs the relay (error `relay_off`).
        #[serde(default)]
        for_phone: Option<bool>,
    },
    /// Pair with another hub from its invite URI. Ack result `{device, machine}`.
    /// Errors: bad_uri, self_invite, unreachable, invite_rejected, timeout.
    JoinPeer { id: String, uri: String },
    /// Unpair a device: its links close, it can no longer connect, and the peer is told. Errors: not_found.
    RevokeDevice { id: String, device_id: String },
    /// The user is active on this machine (clients throttle to USER_ACTIVITY_REPORT_MS). Hub stamps the time.
    UserActivity { id: String },
    /// A machine's repo index (default: this machine; another machine: from its latest presence). Ack `{repos}`.
    ListRepos {
        id: String,
        #[serde(default)]
        machine_id: Option<MachineId>,
    },
    /// Settings are local-only: only clients on this machine's localhost API can read or change them.
    GetSettings { id: String },
    /// Validated first ("invalid:<field>: <reason>", nothing changed), then written to config.toml (comments kept)
    /// and applied at runtime where possible; keys needing a restart come back in `restartRequired`.
    UpdateSettings { id: String, patch: Box<HubSettingsPatch> },
    HookStatus { id: String },
    /// preview = the exact change, nothing written; apply / remove write the agent's config.
    InstallHooks {
        id: String,
        target: HookTargetName,
        #[serde(default)]
        repo: Option<String>,
        action: HookAction,
    },
    /// Ask the desktop app to show a view (e.g. from VS Code). Ack `{delivered}`.
    OpenPanel { id: String, view: PanelView },
    /// Local only. The hub acks, starts a replacement process with the same arguments, says bye to peers and exits;
    /// running sessions end. Clients reconnect on their own. The running port is in data_dir/runtime.json.
    RestartHub { id: String },
    /// A2A (phase A2, local only): a new bearer token for the outside A2A caller `caller_id`, returned once
    /// (`{token}`) and never readable again; the previous token stops working at once.
    A2aRotateToken { id: String, caller_id: String },
    /// A2A: this machine's delegations (tasks), newest first (`{tasks: A2aTaskSummary[]}`); `session_id` filters.
    ListTasks {
        id: String,
        #[serde(default)]
        session_id: Option<SessionId>,
    },
}

impl ClientMsg {
    pub fn id(&self) -> &str {
        match self {
            ClientMsg::Hello { id, .. }
            | ClientMsg::ListAgents { id, .. }
            | ClientMsg::ListSessions { id, .. }
            | ClientMsg::GetEvents { id, .. }
            | ClientMsg::StartSession { id, .. }
            | ClientMsg::Prompt { id, .. }
            | ClientMsg::Cancel { id, .. }
            | ClientMsg::ResolvePermission { id, .. }
            | ClientMsg::ListHistory { id, .. }
            | ClientMsg::ResumeSession { id, .. }
            | ClientMsg::Chat { id, .. }
            | ClientMsg::UpdatePet { id, .. }
            | ClientMsg::SetRules { id, .. }
            | ClientMsg::UpdateMachine { id, .. }
            | ClientMsg::CreateInvite { id, .. }
            | ClientMsg::JoinPeer { id, .. }
            | ClientMsg::RevokeDevice { id, .. }
            | ClientMsg::UserActivity { id }
            | ClientMsg::ListRepos { id, .. }
            | ClientMsg::GetSettings { id }
            | ClientMsg::UpdateSettings { id, .. }
            | ClientMsg::HookStatus { id }
            | ClientMsg::InstallHooks { id, .. }
            | ClientMsg::OpenPanel { id, .. }
            | ClientMsg::RestartHub { id }
            | ClientMsg::A2aRotateToken { id, .. }
            | ClientMsg::ListTasks { id, .. } => id,
        }
    }
}

// ---------- hub -> client ----------
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Welcome {
    pub protocol_version: u32,
    pub hub_version: String,
    /// The machine this hub runs on.
    pub machine: Machine,
    /// This machine first, then every paired (not revoked) hub with its online state.
    pub machines: Vec<Machine>,
    /// The pairing roster (never includes this hub).
    pub devices: Vec<Device>,
    // sessions / pets / pending_permissions below cover every machine: this hub's own plus the
    // read-only mirror of paired hubs (each item carries its machine_id).
    pub peer: PeerStatus,
    pub agents: Vec<Agent>,
    pub sessions: Vec<Session>,
    pub pets: Vec<Pet>,
    pub pending_permissions: Vec<PermissionRequest>,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ServerMsg {
    Ack {
        id: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    Agents { agents: Vec<Agent> },
    Session { session: Session },
    Event { event: HubEvent },
    PermissionRequest { request: PermissionRequest },
    /// `by_machine_id` = the machine the decision came from when it was not the owner (a remote approval).
    PermissionResolved {
        request_id: RequestId,
        session_id: SessionId,
        outcome: PermissionOutcome,
        by: ResolvedBy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by_machine_id: Option<MachineId>,
    },
    Pet { pet: Pet },
    Chat { message: ChatMessage },
    Notification { notification: HubNotification },
    /// Sent when a machine is added, renamed, or goes online/offline (not on every heartbeat).
    Machine { machine: Machine },
    /// A paired machine was revoked (here or by the peer).
    MachineRemoved { machine_id: MachineId },
    /// The full pairing roster after any change.
    Devices { devices: Vec<Device> },
    /// Peer listener started or stopped.
    PeerStatus { peer: PeerStatus },
    /// Settings changed (by any local client).
    Settings { settings: HubSettings, restart_required: Vec<String> },
    /// A client asked the desktop app to show this view.
    OpenPanel { view: PanelView },
}

impl ServerMsg {
    pub fn ack_ok(id: impl Into<String>, result: Value) -> Self {
        ServerMsg::Ack { id: id.into(), ok: true, result: Some(result), error: None }
    }
    pub fn ack_err(id: impl Into<String>, error: impl Into<String>) -> Self {
        ServerMsg::Ack { id: id.into(), ok: false, result: None, error: Some(error.into()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_ids() {
        assert_eq!(global_id("abcdef012345", 7), "abcdef012345:7");
        assert_eq!(parse_global_id("abcdef012345:s-1"), Some(("abcdef012345", "s-1")));
        assert_eq!(parse_global_id("ABCDEF012345:x"), None);
        assert_eq!(parse_global_id("short:x"), None);
        assert_eq!(parse_global_id("abcdef012345:"), None);
        assert_eq!(parse_global_id("no-colon"), None);
        let m = ClientMsg::UpdateMachine { id: "1".into(), machine_id: "abcdef012345".into(), name: "Mac mini".into() };
        assert_eq!(serde_json::to_value(&m).unwrap(), serde_json::json!({"type":"update_machine","id":"1","machineId":"abcdef012345","name":"Mac mini"}));
    }

    #[test]
    fn peer_wire_shape() {
        let env = PeerEnvelope { v: 1, from: "a".into(), to: "b".into(), seq: 7, nonce: "n".into(), ciphertext: "c".into(), pk: None };
        assert_eq!(serde_json::to_value(&env).unwrap(), serde_json::json!({"v":1,"from":"a","to":"b","seq":7,"nonce":"n","box":"c"}));
        let b = PeerBody::Bye { reason: ByeReason::Revoked, ts: 1 };
        assert_eq!(serde_json::to_value(&b).unwrap(), serde_json::json!({"type":"bye","reason":"revoked","ts":1}));
        let st = PeerStatus { listening: None, public_key: "k".into(), relay: RelayStatusInfo::default() };
        assert_eq!(serde_json::to_value(&st).unwrap(), serde_json::json!({"listening":null,"publicKey":"k","relay":{"url":null,"connected":false}}));
        let m: ClientMsg = serde_json::from_str(r#"{"type":"create_invite","id":"1"}"#).unwrap();
        assert_eq!(m, ClientMsg::CreateInvite { id: "1".into(), ttl_secs: None, addr: None, for_phone: None });
        let b = PeerBody::SyncRequest { since_seq: 5 };
        assert_eq!(serde_json::to_value(&b).unwrap(), serde_json::json!({"type":"sync_request","sinceSeq":5}));
        let m: ClientMsg = serde_json::from_str(r#"{"type":"user_activity","id":"9"}"#).unwrap();
        assert_eq!(m.id(), "9");
        let b = PeerBody::Intent {
            intent_id: "i1".into(),
            client: ClientKind::Desktop,
            confirmed: true,
            intent: Intent::Approve { request_id: "abcdef012345:r".into(), option_id: "allow".into() },
            ts: 1,
        };
        assert_eq!(
            serde_json::to_value(&b).unwrap(),
            serde_json::json!({"type":"intent","intentId":"i1","client":"desktop","confirmed":true,"intent":{"kind":"approve","requestId":"abcdef012345:r","optionId":"allow"},"ts":1})
        );
        let a = PeerBody::IntentAck { intent_id: "i1".into(), status: IntentStatus::Rejected, reason: Some(IntentRejectReason::AlreadyDecided), result: None };
        assert_eq!(serde_json::to_value(&a).unwrap(), serde_json::json!({"type":"intent_ack","intentId":"i1","status":"rejected","reason":"already_decided"}));
        let m: ClientMsg = serde_json::from_str(r#"{"type":"list_repos","id":"7","machineId":"abcdef012345"}"#).unwrap();
        assert_eq!(m.id(), "7");
        let p = ChatProposal {
            id: "p".into(),
            kind: ChatProposalKind::Start,
            machine_id: "abcdef012345".into(),
            agent_id: "claude-code".into(),
            cwd: Some("/r".into()),
            repo_name: Some("r".into()),
            session_id: None,
            text: "run the tests".into(),
            summary: "Start".into(),
        };
        assert_eq!(serde_json::to_value(&p).unwrap()["kind"], "start");
        let p: HubSettingsPatch = serde_json::from_str(r#"{"bot":{"anthropicApiKey":null},"peer":{"listen":"192.168.1.5:47801"}}"#).unwrap();
        assert_eq!(p.bot.unwrap().anthropic_api_key, Some(None));
        assert_eq!(p.peer.unwrap().listen, Some(Some("192.168.1.5:47801".into())));
        let p: HubSettingsPatch = serde_json::from_str(r#"{"bot":{"anthropicApiKey":"sk-secret-value"},"relay":{"accessKey":"rk-secret"}}"#).unwrap();
        let dbg = format!("{p:?}");
        assert!(!dbg.contains("sk-secret-value") && !dbg.contains("rk-secret") && dbg.contains("set (redacted)"));
        let p: HubSettingsPatch = serde_json::from_str(r#"{"bot":{"model":"m"}}"#).unwrap();
        assert_eq!(p.bot.unwrap().anthropic_api_key, None);
        let m: ClientMsg = serde_json::from_str(r#"{"type":"install_hooks","id":"8","target":"copilot-vscode","repo":"/r","action":"preview"}"#).unwrap();
        assert_eq!(m.id(), "8");
        let s = ServerMsg::MachineRemoved { machine_id: "abcdef012345".into() };
        assert_eq!(serde_json::to_value(&s).unwrap(), serde_json::json!({"type":"machine_removed","machineId":"abcdef012345"}));
    }

    #[test]
    fn client_msg_wire_shape() {
        let j = r#"{"type":"start_session","id":"1","agentId":"codex","cwd":"/r","prompt":"hi"}"#;
        let m: ClientMsg = serde_json::from_str(j).unwrap();
        assert!(matches!(m, ClientMsg::StartSession { .. }));
        let j = r#"{"type":"resolve_permission","id":"2","requestId":"r","optionId":null}"#;
        let m: ClientMsg = serde_json::from_str(j).unwrap();
        assert_eq!(m, ClientMsg::ResolvePermission { id: "2".into(), request_id: "r".into(), option_id: None, confirmed: None });
    }

    #[test]
    fn server_msg_wire_shape() {
        let m = ServerMsg::PermissionResolved {
            request_id: "r".into(),
            session_id: "s".into(),
            outcome: PermissionOutcome::Selected { option_id: "o".into() },
            by: ResolvedBy::Rule,
            by_machine_id: None,
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"type":"permission_resolved","requestId":"r","sessionId":"s","outcome":{"kind":"selected","optionId":"o"},"by":"rule"})
        );
        let e = HubEvent { id: "abcdef012345:1".into(), seq: 1, machine_id: "abcdef012345".into(), session_id: "s".into(), kind: EventType::ToolCall, payload: Value::Null, ts: 0 };
        assert_eq!(serde_json::to_value(&e).unwrap()["type"], "tool_call");
        let r = Rule { id: "x".into(), matcher: RuleMatch { tool_kind: Some(ToolKind::Read), ..Default::default() }, action: RuleAction::AutoAllow };
        assert_eq!(serde_json::to_value(&r).unwrap(), serde_json::json!({"id":"x","match":{"toolKind":"read"},"action":"auto_allow"}));
    }
}
