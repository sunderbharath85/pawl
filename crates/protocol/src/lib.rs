//! krypto-protocol: Rust mirror of `packages/protocol/src/index.ts` (the source of truth).
//! JSON must be identical: struct fields camelCase, enum tags/values snake_case.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const PROTOCOL_VERSION: u32 = 1;
pub const HUB_DEFAULT_PORT: u16 = 47800;
pub const HUB_HOST: &str = "127.0.0.1";
pub const HUB_DATA_DIR_NAME: &str = "krypto";
pub const HUB_TOKEN_FILE: &str = "token";

pub type AgentId = String;
pub type SessionId = String;
pub type PetId = String;
pub type RequestId = String;
pub type UnixMs = i64;

pub fn now_ms() -> UnixMs {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `dirs::data_dir()/krypto` (Win %APPDATA%\krypto). Callers create it.
pub fn data_dir() -> Option<std::path::PathBuf> {
    dirs::data_dir().map(|d| d.join(HUB_DATA_DIR_NAME))
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Agent {
    pub id: AgentId,
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
    pub id: SessionId,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HubEvent {
    pub id: i64,
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
    pub request_id: RequestId,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    pub tool_call: ToolCallPayload,
    pub options: Vec<PermissionOption>,
    pub created_at: UnixMs,
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
    pub id: PetId,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pet_id: Option<PetId>,
    pub role: ChatRole,
    pub text: String,
    pub ts: UnixMs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
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
    pub level: NotificationLevel,
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    pub ts: UnixMs,
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
    ListAgents { id: String, #[serde(default)] refresh: Option<bool> },
    ListSessions { id: String, #[serde(default)] include_ended: Option<bool> },
    GetEvents { id: String, session_id: SessionId, #[serde(default)] since_id: Option<i64>, #[serde(default)] limit: Option<u32> },
    StartSession { id: String, agent_id: AgentId, cwd: String, #[serde(default)] prompt: Option<String> },
    Prompt { id: String, session_id: SessionId, text: String },
    Cancel { id: String, session_id: SessionId },
    ResolvePermission { id: String, request_id: RequestId, option_id: Option<String> },
    ListHistory { id: String, agent_id: AgentId, #[serde(default)] cwd: Option<String> },
    ResumeSession { id: String, agent_id: AgentId, acp_session_id: String, cwd: String },
    Chat { id: String, #[serde(default)] pet_id: Option<PetId>, text: String },
    UpdatePet { id: String, pet_id: PetId, patch: PetPatch },
    SetRules { id: String, rules: Vec<Rule> },
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
            | ClientMsg::SetRules { id, .. } => id,
        }
    }
}

// ---------- hub -> client ----------
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Welcome {
    pub protocol_version: u32,
    pub hub_version: String,
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
    PermissionResolved { request_id: RequestId, session_id: SessionId, outcome: PermissionOutcome, by: ResolvedBy },
    Pet { pet: Pet },
    Chat { message: ChatMessage },
    Notification { notification: HubNotification },
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
    fn client_msg_wire_shape() {
        let j = r#"{"type":"start_session","id":"1","agentId":"codex","cwd":"/r","prompt":"hi"}"#;
        let m: ClientMsg = serde_json::from_str(j).unwrap();
        assert!(matches!(m, ClientMsg::StartSession { .. }));
        let j = r#"{"type":"resolve_permission","id":"2","requestId":"r","optionId":null}"#;
        let m: ClientMsg = serde_json::from_str(j).unwrap();
        assert_eq!(m, ClientMsg::ResolvePermission { id: "2".into(), request_id: "r".into(), option_id: None });
    }

    #[test]
    fn server_msg_wire_shape() {
        let m = ServerMsg::PermissionResolved {
            request_id: "r".into(),
            session_id: "s".into(),
            outcome: PermissionOutcome::Selected { option_id: "o".into() },
            by: ResolvedBy::Rule,
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(
            v,
            serde_json::json!({"type":"permission_resolved","requestId":"r","sessionId":"s","outcome":{"kind":"selected","optionId":"o"},"by":"rule"})
        );
        let e = HubEvent { id: 1, session_id: "s".into(), kind: EventType::ToolCall, payload: Value::Null, ts: 0 };
        assert_eq!(serde_json::to_value(&e).unwrap()["type"], "tool_call");
        let r = Rule { id: "x".into(), matcher: RuleMatch { tool_kind: Some(ToolKind::Read), ..Default::default() }, action: RuleAction::AutoAllow };
        assert_eq!(serde_json::to_value(&r).unwrap(), serde_json::json!({"id":"x","match":{"toolKind":"read"},"action":"auto_allow"}));
    }
}
