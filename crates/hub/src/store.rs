//! SQLite persistence (rusqlite, bundled). All methods here are blocking; async code goes through
//! [`Store::run`], which moves the work onto tokio's blocking pool.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use pawl_peer::{Roster, RosterEntry};
use pawl_protocol::{
    global_id, is_valid_machine_id, parse_global_id, Agent, ChatMessage, ChatRole, Device, DeviceKind, EventType,
    HubEvent, Machine, MachineId, MachineOs, PermissionOptionKind, PermissionRequest, Pet, ResolvedBy, Rule, Session,
    SessionId, ToolKind, UnixMs,
};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row, Transaction};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

/// One schema migration. `Code` steps get this hub's machine id (v2 rewrites ids to global form).
enum Migration {
    Sql(&'static str),
    Code(fn(&Transaction<'_>, &str) -> Result<()>),
}

/// Ordered schema migrations; `PRAGMA user_version` = number applied.
const MIGRATIONS: &[Migration] =
    &[
        Migration::Sql(SCHEMA_V1),
        Migration::Code(migrate_v2),
        Migration::Sql(SCHEMA_V3),
        Migration::Sql(SCHEMA_V4),
        Migration::Sql(SCHEMA_V5),
        Migration::Sql(SCHEMA_V6),
        Migration::Sql(SCHEMA_V7),
        Migration::Sql(SCHEMA_V8),
        Migration::Sql(SCHEMA_V9),
        Migration::Sql(SCHEMA_V10),
    ];

const SCHEMA_V1: &str = r#"
CREATE TABLE agents (
    id          TEXT PRIMARY KEY,
    data        TEXT NOT NULL,
    updated_at  INTEGER NOT NULL
);
CREATE TABLE sessions (
    id              TEXT PRIMARY KEY,
    agent_id        TEXT NOT NULL,
    source          TEXT NOT NULL,
    status          TEXT NOT NULL,
    external_id     TEXT,
    acp_session_id  TEXT,
    cwd             TEXT NOT NULL,
    title           TEXT,
    started_at      INTEGER NOT NULL,
    last_event_at   INTEGER NOT NULL,
    ended_at        INTEGER,
    data            TEXT NOT NULL
);
CREATE INDEX sessions_external ON sessions(agent_id, external_id);
CREATE INDEX sessions_last_event ON sessions(last_event_at);
CREATE TABLE events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT NOT NULL,
    type        TEXT NOT NULL,
    payload     TEXT NOT NULL,
    ts          INTEGER NOT NULL
);
CREATE INDEX events_session ON events(session_id, id);
CREATE INDEX events_ts ON events(ts);
CREATE TABLE pets (
    id        TEXT PRIMARY KEY,
    agent_id  TEXT NOT NULL UNIQUE,
    data      TEXT NOT NULL
);
CREATE TABLE rules (
    position  INTEGER PRIMARY KEY,
    id        TEXT NOT NULL,
    data      TEXT NOT NULL
);
CREATE TABLE chat (
    seq         INTEGER PRIMARY KEY AUTOINCREMENT,
    id          TEXT NOT NULL UNIQUE,
    pet_id      TEXT,
    role        TEXT NOT NULL,
    text        TEXT NOT NULL,
    ts          INTEGER NOT NULL,
    session_id  TEXT
);
CREATE TABLE audit_log (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    ts            INTEGER NOT NULL,
    request_id    TEXT NOT NULL,
    session_id    TEXT NOT NULL,
    agent_id      TEXT NOT NULL,
    tool_call_id  TEXT,
    tool_kind     TEXT,
    tool_title    TEXT,
    locations     TEXT,
    decided_by    TEXT NOT NULL,
    actor         TEXT NOT NULL,
    option_id     TEXT,
    option_kind   TEXT,
    outcome       TEXT NOT NULL,
    detail        TEXT
);
CREATE INDEX audit_ts ON audit_log(ts);
CREATE TABLE settings (
    key    TEXT PRIMARY KEY,
    value  TEXT NOT NULL
);
"#;

/// v2 (machine identity): `machine_id` columns everywhere, pets unique per (machine, agent).
const SCHEMA_V2: &str = r#"
ALTER TABLE agents ADD COLUMN machine_id TEXT NOT NULL DEFAULT '';
ALTER TABLE sessions ADD COLUMN machine_id TEXT NOT NULL DEFAULT '';
ALTER TABLE events ADD COLUMN machine_id TEXT NOT NULL DEFAULT '';
ALTER TABLE chat ADD COLUMN machine_id TEXT NOT NULL DEFAULT '';
ALTER TABLE audit_log ADD COLUMN machine_id TEXT NOT NULL DEFAULT '';
CREATE TABLE pets_v2 (
    id          TEXT PRIMARY KEY,
    machine_id  TEXT NOT NULL,
    agent_id    TEXT NOT NULL,
    data        TEXT NOT NULL,
    UNIQUE (machine_id, agent_id)
);
CREATE INDEX sessions_machine ON sessions(machine_id, last_event_at);
"#;

/// v3 (pairing): the device roster with per-peer frame counters, and the pairing audit log.
const SCHEMA_V3: &str = r#"
CREATE TABLE devices (
    id           TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    name         TEXT NOT NULL,
    os           TEXT NOT NULL,
    hub_version  TEXT,
    public_key   TEXT NOT NULL,
    addr         TEXT,
    paired_at    INTEGER NOT NULL,
    last_seen    INTEGER,
    revoked_at   INTEGER,
    last_rx_seq  INTEGER NOT NULL DEFAULT 0,
    last_tx_seq  INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE pairing_audit (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    ts           INTEGER NOT NULL,
    machine_id   TEXT NOT NULL,
    kind         TEXT NOT NULL,
    device_id    TEXT,
    device_name  TEXT,
    remote_addr  TEXT,
    actor        TEXT NOT NULL,
    detail       TEXT
);
CREATE INDEX pairing_audit_ts ON pairing_audit(ts);
"#;

/// v4 (mirror): events carry their owner's `seq` (= rowid for this machine's own events, the owner's
/// seq for mirrored ones), each paired device has a mirror sync cursor, and mirrored pending
/// permission requests are kept so the mirror survives a restart while the peer is offline.
const SCHEMA_V4: &str = r#"
ALTER TABLE events ADD COLUMN seq INTEGER;
UPDATE events SET seq = id;
CREATE UNIQUE INDEX events_machine_seq ON events(machine_id, seq);
CREATE INDEX events_session_seq ON events(session_id, seq);
ALTER TABLE devices ADD COLUMN mirror_seq INTEGER NOT NULL DEFAULT 0;
ALTER TABLE devices ADD COLUMN mirror_bytes INTEGER NOT NULL DEFAULT 0;
CREATE TABLE mirror_pending (
    request_id  TEXT PRIMARY KEY,
    machine_id  TEXT NOT NULL,
    session_id  TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    data        TEXT NOT NULL
);
CREATE INDEX mirror_pending_machine ON mirror_pending(machine_id);
"#;

/// v5 (intents): who decided a permission from another machine (audit_log origin columns), the
/// intent log (both roles), and each paired hub's last reported agents.
const SCHEMA_V5: &str = r#"
ALTER TABLE audit_log ADD COLUMN origin_machine_id TEXT;
ALTER TABLE audit_log ADD COLUMN origin_name TEXT;
ALTER TABLE audit_log ADD COLUMN intent_id TEXT;
ALTER TABLE audit_log ADD COLUMN client TEXT;
CREATE INDEX audit_request ON audit_log(request_id);
CREATE TABLE intent_log (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    ts               INTEGER NOT NULL,
    role             TEXT NOT NULL,
    intent_id        TEXT NOT NULL,
    kind             TEXT NOT NULL,
    peer_machine_id  TEXT NOT NULL,
    peer_name        TEXT,
    client           TEXT NOT NULL,
    target           TEXT,
    option_id        TEXT,
    status           TEXT NOT NULL,
    reason           TEXT
);
CREATE INDEX intent_log_ts ON intent_log(ts);
ALTER TABLE intent_log ADD COLUMN repeats INTEGER NOT NULL DEFAULT 0;
ALTER TABLE devices ADD COLUMN agents TEXT;
ALTER TABLE sessions ADD COLUMN remote_driven INTEGER NOT NULL DEFAULT 0;
"#;

/// v6 (remote commands): chat replies keep their proposals; each paired hub's last reported repos.
const SCHEMA_V6: &str = r#"
ALTER TABLE chat ADD COLUMN proposals TEXT;
ALTER TABLE devices ADD COLUMN repos TEXT;
"#;

/// v7 (settings in the UI): who changed which settings or agent hooks, and when. Never holds values.
const SCHEMA_V7: &str = r#"
CREATE TABLE settings_audit (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ts          INTEGER NOT NULL,
    machine_id  TEXT NOT NULL,
    kind        TEXT NOT NULL,
    actor       TEXT NOT NULL,
    client      TEXT,
    keys        TEXT NOT NULL,
    detail      TEXT
);
CREATE INDEX settings_audit_ts ON settings_audit(ts);
"#;

/// v8 (phone, group roster): which paired hub introduced a device (`roster_intro`), and the intros
/// still owed to devices that were offline when they were due (sent on their next link).
const SCHEMA_V8: &str = r#"
ALTER TABLE devices ADD COLUMN introduced_by TEXT;
CREATE TABLE roster_intro_pending (
    recipient_id  TEXT NOT NULL,
    device_id     TEXT NOT NULL,
    queued_at     INTEGER NOT NULL,
    PRIMARY KEY (recipient_id, device_id)
);
"#;

/// v9 (A2A, phase A2): every call to the localhost A2A server, by caller. Never message text or tokens.
const SCHEMA_V9: &str = r#"
CREATE TABLE a2a_log (
    id      INTEGER PRIMARY KEY AUTOINCREMENT,
    ts      INTEGER NOT NULL,
    caller  TEXT NOT NULL,
    method  TEXT NOT NULL,
    target  TEXT,
    status  TEXT NOT NULL,
    detail  TEXT
);
CREATE INDEX a2a_log_ts ON a2a_log(ts);
"#;

/// v10 (approvals by voice, v0.2): how a decision was given ("voice") and the transcript (300 characters
/// at most), for this machine's decisions (audit_log) and the approve / deny intents it sent (intent_log).
const SCHEMA_V10: &str = r#"
ALTER TABLE audit_log ADD COLUMN via TEXT;
ALTER TABLE audit_log ADD COLUMN transcript TEXT;
ALTER TABLE intent_log ADD COLUMN via TEXT;
ALTER TABLE intent_log ADD COLUMN transcript TEXT;
"#;

/// a2a_log rows kept (oldest pruned first).
pub const A2A_LOG_MAX_ROWS: i64 = 100_000;

/// settings_audit rows kept (oldest pruned first).
pub const SETTINGS_AUDIT_MAX_ROWS: i64 = 10_000;

/// intent_log rows are kept this long, and at most INTENT_LOG_MAX_ROWS of them.
pub const INTENT_LOG_RETENTION_MS: i64 = 30 * 86_400_000;
pub const INTENT_LOG_MAX_ROWS: i64 = 100_000;

/// Mirrored sessions per paired machine (oldest activity evicted first).
pub const MIRROR_MAX_SESSIONS: i64 = 500;
/// Mirrored events per mirrored session (oldest evicted first).
pub const MIRROR_MAX_EVENTS_PER_SESSION: i64 = 2000;
/// Mirrored pets per paired machine (one per agent; new ones beyond this are refused).
pub const MIRROR_MAX_PETS: i64 = 200;
/// Mirrored pending requests per paired machine (new ones beyond this are refused).
pub const MIRROR_MAX_PENDING: i64 = 500;
/// Ended mirrored sessions are kept this long.
pub const MIRROR_RETENTION_MS: i64 = 7 * 86_400_000;
/// Mirrored event payload bytes kept per paired machine (oldest events evicted beyond it).
pub const MIRROR_MAX_PAYLOAD_BYTES: i64 = 256 * 1024 * 1024;

const SETTING_RULES_SET: &str = "rules_set";
/// The machine id hub.db was migrated with; every stored global id carries it.
const SETTING_MACHINE_ID: &str = "machine_id";

/// What happened to a permission request (audit log `outcome`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    Allowed,
    Rejected,
    Cancelled,
    /// Resolution was attempted but the agent connection refused it.
    Failed,
}

/// One permission decision: who decided what, when, and with which result.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct AuditEntry {
    pub id: i64,
    pub ts: UnixMs,
    /// Machine that owns the session (and decided, until step 4 routes remote decisions).
    pub machine_id: MachineId,
    pub request_id: String,
    pub session_id: SessionId,
    pub agent_id: String,
    pub tool_call_id: Option<String>,
    pub tool_kind: Option<ToolKind>,
    pub tool_title: Option<String>,
    pub locations: Vec<String>,
    pub decided_by: ResolvedBy,
    /// e.g. "desktop#3", "cli#1", "rule:auto_allow", "chat:desktop#3", "hub:session_ended".
    pub actor: String,
    pub option_id: Option<String>,
    pub option_kind: Option<PermissionOptionKind>,
    pub outcome: AuditOutcome,
    pub detail: Option<String>,
    /// Set when the decision came from another machine (an intent).
    pub origin: Option<AuditOrigin>,
    /// "voice" when the user answered by speaking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// The spoken words (300 characters at most), with `via: "voice"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
}

/// The machine, intent and client behind a remote decision.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct AuditOrigin {
    pub machine_id: MachineId,
    pub name: Option<String>,
    pub intent_id: String,
    /// The client kind on the origin, e.g. "desktop".
    pub client: String,
}

/// Which side of an intent this hub was on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentRole {
    /// This hub's client asked; the intent went to the owner.
    Origin,
    /// Another hub asked this hub (the owner) to act.
    Owner,
}

/// One intent, as sent (origin) or as handled (owner). Never holds prompt text.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct IntentLogEntry {
    pub id: i64,
    pub ts: UnixMs,
    pub role: IntentRole,
    pub intent_id: String,
    /// approve | deny | prompt | start | cancel
    pub kind: String,
    /// Origin role: the target machine; owner role: the origin machine.
    pub peer_machine_id: MachineId,
    pub peer_name: Option<String>,
    pub client: String,
    /// The request id, session id, or (start) agent id.
    pub target: Option<String>,
    pub option_id: Option<String>,
    /// applied | rejected | offline | timeout
    pub status: String,
    pub reason: Option<String>,
    /// Identical rejections (rate_limited / forbidden) from the same peer not logged separately
    /// since the previous entry for them.
    pub repeats: u32,
    /// Origin role: "voice" when the user spoke the approve / deny.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
    /// The spoken words (300 characters at most), with `via: "voice"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
}

/// What a pairing audit entry records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingAuditKind {
    /// A device was paired (either side of the invite).
    Paired,
    /// A pairing attempt on our listener failed (bad code, expired invite, malformed request).
    PairRejected,
    /// We revoked a device.
    Revoked,
    /// The device revoked us (`bye{revoked}`), so we revoked it too.
    RevokedByPeer,
    /// A paired hub introduced a device (`roster_intro`) and we added it. actor = the introducer.
    Introduced,
    /// An introduced device clashed with one we know (same id, other key; same key, other id; or a
    /// revoked one): skipped. detail = why.
    IntroConflict,
    /// An intro we did not apply: `[peer] accept_roster_intros = false`, or a malformed entry.
    IntroIgnored,
}

/// One pairing event. Never holds invite codes or keys.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct PairingAuditEntry {
    pub id: i64,
    pub ts: UnixMs,
    /// This hub.
    pub machine_id: MachineId,
    pub kind: PairingAuditKind,
    pub device_id: Option<String>,
    pub device_name: Option<String>,
    /// Socket address of the other side, when known (always set for `pair_rejected`).
    pub remote_addr: Option<String>,
    /// e.g. "desktop#3", "cli#1", "peer".
    pub actor: String,
    pub detail: Option<String>,
}

/// One call to the localhost A2A server (`a2a_log`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A2aLogEntry {
    pub id: i64,
    pub ts: UnixMs,
    /// "a2a:<caller id>".
    pub caller: String,
    /// The A2A method ("SendMessage", "GetTask", ...).
    pub method: String,
    /// The task or context the call named, if any.
    pub target: Option<String>,
    /// "ok", or the error ("task_not_found", "forbidden", ...).
    pub status: String,
    pub detail: Option<String>,
}

/// What a settings audit row is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SettingsAuditKind {
    /// `update_settings` (keys = the changed setting keys).
    Settings,
    /// `install_hooks` apply / remove (keys = `hooks.<target>`).
    Hooks,
    /// A pairing join that adopted the inviter's relay (keys = relay.url, relay.accessKey).
    Pairing,
    /// `restart_hub` (keys = the restart-only keys that were pending, if any).
    Restart,
}

/// One settings change. Holds key names only, never values (secrets included).
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct SettingsAuditEntry {
    pub id: i64,
    pub ts: UnixMs,
    /// This hub.
    pub machine_id: MachineId,
    pub kind: SettingsAuditKind,
    /// e.g. "desktop#3".
    pub actor: String,
    /// The local client kind that asked ("desktop", "vscode", "cli", ...).
    pub client: Option<String>,
    /// camelCase setting keys, e.g. ["peer.listen", "bot.anthropicApiKey"].
    pub keys: Vec<String>,
    /// e.g. "apply" / "remove" (+ repo) for hooks.
    pub detail: Option<String>,
}

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    /// This hub's machine; prefix of every global id it stores.
    machine_id: MachineId,
    /// Per-machine budget for mirrored event payload bytes (MIRROR_MAX_PAYLOAD_BYTES; tests lower it).
    mirror_byte_budget: Arc<AtomicI64>,
}

fn enum_str<T: Serialize>(v: &T) -> String {
    match serde_json::to_value(v) {
        Ok(serde_json::Value::String(s)) => s,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

fn enum_from<T: DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
}

fn json<T: Serialize>(v: &T) -> Result<String> {
    Ok(serde_json::to_string(v)?)
}

fn from_json<T: DeserializeOwned>(s: &str) -> rusqlite::Result<T> {
    serde_json::from_str(s).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))
}

/// Columns read by [`row_event`].
const EVENT_COLS: &str = "seq, machine_id, session_id, type, payload, ts";

/// `seq` is the owner's per-machine sequence (the rowid for this machine's own events);
/// `HubEvent.id` is its global form.
fn row_event(r: &Row<'_>) -> rusqlite::Result<HubEvent> {
    let seq: i64 = r.get(0)?;
    let machine_id: String = r.get(1)?;
    let kind: String = r.get(3)?;
    let payload: String = r.get(4)?;
    Ok(HubEvent {
        id: global_id(&machine_id, seq),
        seq,
        machine_id,
        session_id: r.get(2)?,
        kind: enum_from(&kind).unwrap_or(EventType::Hook),
        payload: serde_json::from_str(&payload).unwrap_or(Value::Null),
        ts: r.get(5)?,
    })
}

fn row_session(r: &Row<'_>) -> rusqlite::Result<Session> {
    let data: String = r.get(0)?;
    let mut s: Session = from_json(&data)?;
    // columns are authoritative for the fields updated in place
    s.last_event_at = r.get(1)?;
    s.ended_at = r.get(2)?;
    Ok(s)
}

const SESSION_COLS: &str = "data, last_event_at, ended_at";

/// Columns read by [`row_roster_entry`].
const DEVICE_COLS: &str = "id, kind, name, os, hub_version, public_key, addr, paired_at, last_seen, introduced_by";

/// The OS stored for a roster entry: its machine view's, or the device's own (pair_request / hello
/// / roster_intro) when the machine view does not say.
fn device_os(e: &RosterEntry) -> MachineOs {
    match (e.device.os, e.machine.os) {
        (Some(os), MachineOs::Other) => os,
        (_, os) => os,
    }
}

/// A roster row: the device plus its last known machine view (offline; the hub adds live state).
fn row_roster_entry(r: &Row<'_>) -> rusqlite::Result<RosterEntry> {
    let id: String = r.get(0)?;
    let kind: String = r.get(1)?;
    let name: String = r.get(2)?;
    let os: String = r.get(3)?;
    let paired_at: UnixMs = r.get(7)?;
    let last_seen: Option<UnixMs> = r.get(8)?;
    let os: Option<MachineOs> = enum_from(&os);
    let machine = Machine {
        id: id.clone(),
        name: name.clone(),
        os: os.unwrap_or(MachineOs::Other),
        last_seen: last_seen.unwrap_or(paired_at),
        online: false,
        hub_version: r.get(4)?,
        user_active_at: None,
    };
    let device = Device {
        id,
        kind: enum_from(&kind).unwrap_or(DeviceKind::Hub),
        name,
        public_key: r.get(5)?,
        paired_at,
        last_seen,
        addr: r.get(6)?,
        path: None,
        introduced_by: r.get(9)?,
        os,
    };
    Ok(RosterEntry { device, machine })
}

/// The per-device frame counters.
#[derive(Clone, Copy)]
enum SeqCol {
    Rx,
    Tx,
}

impl SeqCol {
    fn name(self) -> &'static str {
        match self {
            SeqCol::Rx => "last_rx_seq",
            SeqCol::Tx => "last_tx_seq",
        }
    }
}

impl Store {
    /// Open (and migrate) hub.db for this machine. Refuses a db that belongs to another machine id.
    pub fn open(path: &Path, machine_id: &str) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Self::init(conn, machine_id)
    }

    pub fn open_in_memory(machine_id: &str) -> Result<Self> {
        Self::init(Connection::open_in_memory()?, machine_id)
    }

    fn init(mut conn: Connection, machine_id: &str) -> Result<Self> {
        if !is_valid_machine_id(machine_id) {
            bail!("invalid machine id {machine_id:?}");
        }
        // Check ownership before migrating, so a foreign db is never rewritten.
        if let Some(m) = stored_machine_id(&conn)? {
            if m != machine_id {
                bail!(
                    "hub.db belongs to machine {m:?} but machine.toml says {machine_id:?}; \
                     restore machine.toml with id = \"{m}\" (or move hub.db away to start fresh)"
                );
            }
        }
        migrate(&mut conn, machine_id)?;
        if stored_machine_id(&conn)?.is_none() {
            conn.execute("INSERT INTO settings (key, value) VALUES (?1, ?2)", params![SETTING_MACHINE_ID, machine_id])?;
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            machine_id: machine_id.to_string(),
            mirror_byte_budget: Arc::new(AtomicI64::new(MIRROR_MAX_PAYLOAD_BYTES)),
        })
    }

    /// The machine this store belongs to.
    pub fn machine_id(&self) -> &str {
        &self.machine_id
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock leaves SQLite itself consistent (statements are atomic).
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Run blocking store work off the async runtime.
    pub async fn run<R, F>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&Store) -> Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let s = self.clone();
        tokio::task::spawn_blocking(move || f(&s)).await.context("store task failed")?
    }

    // ------------------------------------------------------------ agents

    pub fn replace_agents(&self, agents: &[Agent]) -> Result<()> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        tx.execute("DELETE FROM agents", [])?;
        let now = pawl_protocol::now_ms();
        for a in agents {
            tx.execute(
                "INSERT INTO agents (id, machine_id, data, updated_at) VALUES (?1, ?2, ?3, ?4)",
                params![a.id, a.machine_id, json(a)?, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_agent(&self, a: &Agent) -> Result<()> {
        self.conn().execute(
            "INSERT INTO agents (id, machine_id, data, updated_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET machine_id = excluded.machine_id, data = excluded.data,
                                           updated_at = excluded.updated_at",
            params![a.id, a.machine_id, json(a)?, pawl_protocol::now_ms()],
        )?;
        Ok(())
    }

    pub fn agents(&self) -> Result<Vec<Agent>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT data FROM agents ORDER BY id")?;
        let rows = st.query_map([], |r| from_json::<Agent>(&r.get::<_, String>(0)?))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // ------------------------------------------------------------ sessions

    pub fn upsert_session(&self, s: &Session) -> Result<()> {
        write_session(&self.conn(), s, false)
    }

    pub fn session(&self, id: &str) -> Result<Option<Session>> {
        let c = self.conn();
        Ok(c.query_row(&format!("SELECT {SESSION_COLS} FROM sessions WHERE id = ?1"), [id], row_session).optional()?)
    }

    /// This machine's open sessions plus its sessions with activity since `since` (newest first).
    pub fn sessions_active_since(&self, since: UnixMs) -> Result<Vec<Session>> {
        let c = self.conn();
        let mut st = c.prepare(&format!(
            "SELECT {SESSION_COLS} FROM sessions WHERE machine_id = ?2 AND (ended_at IS NULL OR last_event_at >= ?1)
             ORDER BY last_event_at DESC"
        ))?;
        let rows = st.query_map(params![since, self.machine_id], row_session)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// This machine's sessions whose stored title starts like injected text (`<…`, `[Image…`).
    pub fn sessions_with_injected_titles(&self) -> Result<Vec<Session>> {
        let c = self.conn();
        let mut st = c.prepare(&format!(
            "SELECT {SESSION_COLS} FROM sessions WHERE machine_id = ?1 AND (ltrim(title) LIKE '<%' OR ltrim(title) LIKE '[Image%')"
        ))?;
        let rows = st.query_map(params![self.machine_id], row_session)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Most recent sessions of every machine (this one's and mirrored), ended or not.
    pub fn recent_sessions(&self, limit: u32) -> Result<Vec<Session>> {
        let c = self.conn();
        let mut st = c.prepare(&format!("SELECT {SESSION_COLS} FROM sessions ORDER BY last_event_at DESC LIMIT ?1"))?;
        let rows = st.query_map([limit], row_session)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // ------------------------------------------------------------ events

    /// Insert an event of this machine (rowid = `HubEvent.seq`, id = `machine:seq`) and bump the
    /// session's `lastEventAt`.
    pub fn insert_event(&self, session_id: &str, kind: EventType, payload: &Value, ts: UnixMs) -> Result<HubEvent> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO events (machine_id, session_id, type, payload, ts) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![self.machine_id, session_id, kind.as_str(), payload.to_string(), ts],
        )?;
        let seq = tx.last_insert_rowid();
        tx.execute("UPDATE events SET seq = ?1 WHERE id = ?1", [seq])?;
        tx.execute(
            "UPDATE sessions SET last_event_at = max(last_event_at, ?2) WHERE id = ?1",
            params![session_id, ts],
        )?;
        tx.commit()?;
        Ok(HubEvent {
            id: global_id(&self.machine_id, seq),
            seq,
            machine_id: self.machine_id.clone(),
            session_id: session_id.to_string(),
            kind,
            payload: payload.clone(),
            ts,
        })
    }

    /// Events of one session, ascending. `since_seq` = strictly after that seq (oldest first);
    /// otherwise the latest `limit` events.
    pub fn events(&self, session_id: &str, since_seq: Option<i64>, limit: u32) -> Result<Vec<HubEvent>> {
        let c = self.conn();
        let mut out: Vec<HubEvent> = match since_seq {
            Some(since) => {
                let mut st = c.prepare(&format!(
                    "SELECT {EVENT_COLS} FROM events WHERE session_id = ?1 AND seq > ?2 ORDER BY seq ASC LIMIT ?3"
                ))?;
                let rows = st.query_map(params![session_id, since, limit], row_event)?;
                rows.filter_map(|r| r.ok()).collect()
            }
            None => {
                let mut st =
                    c.prepare(&format!("SELECT {EVENT_COLS} FROM events WHERE session_id = ?1 ORDER BY seq DESC LIMIT ?2"))?;
                let rows = st.query_map(params![session_id, limit], row_event)?;
                let mut v: Vec<HubEvent> = rows.filter_map(|r| r.ok()).collect();
                v.reverse();
                v
            }
        };
        out.shrink_to_fit();
        Ok(out)
    }

    /// Latest events across all sessions (ascending), optionally skipping streaming chunks.
    /// Latest events of this machine (`local`) or of mirrored machines (`!local`), ascending,
    /// optionally skipping streaming chunks.
    pub fn recent_events_of(&self, local: bool, limit: u32, skip_chunks: bool) -> Result<Vec<HubEvent>> {
        let c = self.conn();
        let op = if local { "=" } else { "!=" };
        let chunks = if skip_chunks { " AND type NOT IN ('agent_thought_chunk', 'agent_message_chunk')" } else { "" };
        let mut st =
            c.prepare(&format!("SELECT {EVENT_COLS} FROM events WHERE machine_id {op} ?2{chunks} ORDER BY id DESC LIMIT ?1"))?;
        let rows = st.query_map(params![limit, self.machine_id], row_event)?;
        let mut v: Vec<HubEvent> = rows.filter_map(|r| r.ok()).collect();
        v.reverse();
        Ok(v)
    }

    pub fn recent_events(&self, limit: u32, skip_chunks: bool) -> Result<Vec<HubEvent>> {
        let c = self.conn();
        let sql = if skip_chunks {
            format!(
                "SELECT {EVENT_COLS} FROM events
                 WHERE type NOT IN ('agent_thought_chunk', 'agent_message_chunk') ORDER BY id DESC LIMIT ?1"
            )
        } else {
            format!("SELECT {EVENT_COLS} FROM events ORDER BY id DESC LIMIT ?1")
        };
        let mut st = c.prepare(&sql)?;
        let rows = st.query_map([limit], row_event)?;
        let mut v: Vec<HubEvent> = rows.filter_map(|r| r.ok()).collect();
        v.reverse();
        Ok(v)
    }

    /// Events since `ts` (ascending), skipping streaming chunks.
    /// This machine's events since `ts` (no streaming chunks), oldest first.
    pub fn events_since(&self, ts: UnixMs, limit: u32) -> Result<Vec<HubEvent>> {
        let c = self.conn();
        let mut st = c.prepare(&format!(
            "SELECT {EVENT_COLS} FROM events
             WHERE machine_id = ?3 AND ts >= ?1 AND type NOT IN ('agent_thought_chunk', 'agent_message_chunk')
             ORDER BY id ASC LIMIT ?2"
        ))?;
        let rows = st.query_map(params![ts, limit, self.machine_id], row_event)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Whether a session with this id was ever stored.
    pub fn session_exists(&self, id: &str) -> Result<bool> {
        let c = self.conn();
        Ok(c.query_row("SELECT 1 FROM sessions WHERE id = ?1", [id], |_| Ok(())).optional()?.is_some())
    }

    pub fn prune_events_before(&self, ts: UnixMs) -> Result<usize> {
        Ok(self.conn().execute("DELETE FROM events WHERE ts < ?1", [ts])?)
    }

    // ------------------------------------------------------------ pets

    pub fn upsert_pet(&self, p: &Pet) -> Result<()> {
        self.conn().execute(
            "INSERT INTO pets (id, machine_id, agent_id, data) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET machine_id = excluded.machine_id, agent_id = excluded.agent_id,
                                           data = excluded.data",
            params![p.id, p.machine_id, p.agent_id, json(p)?],
        )?;
        Ok(())
    }

    pub fn pets(&self) -> Result<Vec<Pet>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT data FROM pets ORDER BY rowid")?;
        let rows = st.query_map([], |r| from_json::<Pet>(&r.get::<_, String>(0)?))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // ------------------------------------------------------------ rules

    /// None = never set by a client (use config / defaults).
    pub fn rules(&self) -> Result<Option<Vec<Rule>>> {
        let c = self.conn();
        let set: Option<String> =
            c.query_row("SELECT value FROM settings WHERE key = ?1", [SETTING_RULES_SET], |r| r.get(0)).optional()?;
        if set.is_none() {
            return Ok(None);
        }
        let mut st = c.prepare("SELECT data FROM rules ORDER BY position")?;
        let rows = st.query_map([], |r| from_json::<Rule>(&r.get::<_, String>(0)?))?;
        Ok(Some(rows.filter_map(|r| r.ok()).collect()))
    }

    pub fn set_rules(&self, rules: &[Rule]) -> Result<()> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        tx.execute("DELETE FROM rules", [])?;
        for (i, r) in rules.iter().enumerate() {
            tx.execute("INSERT INTO rules (position, id, data) VALUES (?1, ?2, ?3)", params![i as i64, r.id, json(r)?])?;
        }
        tx.execute(
            "INSERT INTO settings (key, value) VALUES (?1, '1') ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [SETTING_RULES_SET],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ------------------------------------------------------------ chat

    pub fn insert_chat(&self, m: &ChatMessage) -> Result<()> {
        self.conn().execute(
            "INSERT INTO chat (id, machine_id, pet_id, role, text, ts, session_id, proposals)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                m.id,
                m.machine_id,
                m.pet_id,
                enum_str(&m.role),
                m.text,
                m.ts,
                m.session_id,
                if m.proposals.is_empty() { None } else { Some(json(&m.proposals)?) }
            ],
        )?;
        Ok(())
    }

    /// Latest chat messages (ascending).
    pub fn recent_chat(&self, limit: u32) -> Result<Vec<ChatMessage>> {
        let c = self.conn();
        let mut st =
            c.prepare("SELECT id, machine_id, pet_id, role, text, ts, session_id, proposals FROM chat ORDER BY seq DESC LIMIT ?1")?;
        let rows = st.query_map([limit], |r| {
            let role: String = r.get(3)?;
            Ok(ChatMessage {
                id: r.get(0)?,
                machine_id: r.get(1)?,
                pet_id: r.get(2)?,
                role: enum_from(&role).unwrap_or(ChatRole::User),
                text: r.get(4)?,
                ts: r.get(5)?,
                session_id: r.get(6)?,
                proposals: r
                    .get::<_, Option<String>>(7)?
                    .and_then(|j| serde_json::from_str(&j).ok())
                    .unwrap_or_default(),
            })
        })?;
        let mut v: Vec<ChatMessage> = rows.filter_map(|r| r.ok()).collect();
        v.reverse();
        Ok(v)
    }

    // ------------------------------------------------------------ audit

    /// Append an audit entry (the `id` field is ignored and assigned). Returns the new id.
    pub fn insert_audit(&self, a: &AuditEntry) -> Result<i64> {
        let c = self.conn();
        c.execute(
            "INSERT INTO audit_log (ts, request_id, session_id, agent_id, tool_call_id, tool_kind, tool_title, locations,
                                    decided_by, actor, option_id, option_kind, outcome, detail, machine_id,
                                    origin_machine_id, origin_name, intent_id, client, via, transcript)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
            params![
                a.ts,
                a.request_id,
                a.session_id,
                a.agent_id,
                a.tool_call_id,
                a.tool_kind.as_ref().map(enum_str),
                a.tool_title,
                json(&a.locations)?,
                enum_str(&a.decided_by),
                a.actor,
                a.option_id,
                a.option_kind.as_ref().map(enum_str),
                enum_str(&a.outcome),
                a.detail,
                a.machine_id,
                a.origin.as_ref().map(|o| o.machine_id.clone()),
                a.origin.as_ref().and_then(|o| o.name.clone()),
                a.origin.as_ref().map(|o| o.intent_id.clone()),
                a.origin.as_ref().map(|o| o.client.clone()),
                a.via,
                a.transcript
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Whether a permission request with this id was ever decided here (it has an audit entry).
    pub fn request_was_decided(&self, request_id: &str) -> Result<bool> {
        Ok(self
            .conn()
            .query_row(
                "SELECT 1 FROM audit_log WHERE request_id = ?1 AND outcome != 'failed' LIMIT 1",
                [request_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    pub fn insert_intent_log(&self, e: &IntentLogEntry) -> Result<i64> {
        let c = self.conn();
        c.execute(
            "INSERT INTO intent_log (ts, role, intent_id, kind, peer_machine_id, peer_name, client, target, option_id, status, reason, repeats,
                                     via, transcript)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                e.ts,
                enum_str(&e.role),
                e.intent_id,
                e.kind,
                e.peer_machine_id,
                e.peer_name,
                e.client,
                e.target,
                e.option_id,
                e.status,
                e.reason,
                e.repeats,
                e.via,
                e.transcript
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Drop intent_log rows older than INTENT_LOG_RETENTION_MS and keep the newest `max_rows`.
    /// Returns the number of rows removed.
    pub fn prune_intent_log(&self, now: UnixMs, max_rows: i64) -> Result<usize> {
        let c = self.conn();
        let mut n = c.execute("DELETE FROM intent_log WHERE ts < ?1", [now - INTENT_LOG_RETENTION_MS])?;
        n += c.execute(
            "DELETE FROM intent_log WHERE id <= (SELECT id FROM intent_log ORDER BY id DESC LIMIT 1 OFFSET ?1)",
            [max_rows],
        )?;
        Ok(n)
    }

    /// Mark a session of this machine as driven by another machine (started or prompted by an
    /// intent): rules never auto-approve its requests.
    pub fn mark_remote_driven(&self, session_id: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE sessions SET remote_driven = 1 WHERE id = ?1 AND machine_id = ?2",
            params![session_id, self.machine_id],
        )?;
        Ok(())
    }

    /// This machine's sessions that another machine started or prompted.
    pub fn remote_driven_sessions(&self) -> Result<Vec<SessionId>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT id FROM sessions WHERE machine_id = ?1 AND remote_driven = 1")?;
        let rows = st.query_map([&self.machine_id], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The working directories of this machine's own sessions (owned and hook-tracked; never
    /// mirrored ones), each with the last activity there.
    pub fn local_session_folders(&self) -> Result<Vec<(String, UnixMs)>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT cwd, MAX(last_event_at) FROM sessions WHERE machine_id = ?1 AND cwd != '' GROUP BY cwd",
        )?;
        let rows = st.query_map([&self.machine_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Remember a paired hub's repo index (its latest presence), for list_repos while it is offline.
    pub fn set_device_repos(&self, device_id: &str, repos: &[pawl_protocol::RepoRef]) -> Result<()> {
        self.conn().execute(
            "UPDATE devices SET repos = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![device_id, json(&repos)?],
        )?;
        Ok(())
    }

    /// A paired hub's last reported repo index (None if it never reported or is not paired).
    pub fn device_repos(&self, device_id: &str) -> Result<Option<Vec<pawl_protocol::RepoRef>>> {
        let v: Option<Option<String>> = self
            .conn()
            .query_row("SELECT repos FROM devices WHERE id = ?1 AND revoked_at IS NULL", [device_id], |r| r.get(0))
            .optional()?;
        Ok(v.flatten().and_then(|j| serde_json::from_str(&j).ok()))
    }

    /// Latest intent log entries (newest first).
    pub fn intent_log(&self, limit: u32) -> Result<Vec<IntentLogEntry>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT id, ts, role, intent_id, kind, peer_machine_id, peer_name, client, target, option_id, status, reason, repeats,
                    via, transcript
             FROM intent_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit], |r| {
            let role: String = r.get(2)?;
            Ok(IntentLogEntry {
                id: r.get(0)?,
                ts: r.get(1)?,
                role: enum_from(&role).unwrap_or(IntentRole::Owner),
                intent_id: r.get(3)?,
                kind: r.get(4)?,
                peer_machine_id: r.get(5)?,
                peer_name: r.get(6)?,
                client: r.get(7)?,
                target: r.get(8)?,
                option_id: r.get(9)?,
                status: r.get(10)?,
                reason: r.get(11)?,
                repeats: r.get(12)?,
                via: r.get(13)?,
                transcript: r.get(14)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Remember a paired hub's agents (its latest presence), for `list_agents{machineId}` while it is offline.
    pub fn set_device_agents(&self, device_id: &str, agents: &[Agent]) -> Result<()> {
        self.conn().execute(
            "UPDATE devices SET agents = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![device_id, json(&agents)?],
        )?;
        Ok(())
    }

    /// A paired hub's last reported agents (None if it never reported or is not paired).
    pub fn device_agents(&self, device_id: &str) -> Result<Option<Vec<Agent>>> {
        let v: Option<Option<String>> = self
            .conn()
            .query_row("SELECT agents FROM devices WHERE id = ?1 AND revoked_at IS NULL", [device_id], |r| r.get(0))
            .optional()?;
        Ok(v.flatten().and_then(|j| serde_json::from_str(&j).ok()))
    }

    /// Latest audit entries (newest first).
    pub fn audit_log(&self, limit: u32) -> Result<Vec<AuditEntry>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT id, ts, request_id, session_id, agent_id, tool_call_id, tool_kind, tool_title, locations,
                    decided_by, actor, option_id, option_kind, outcome, detail, machine_id,
                    origin_machine_id, origin_name, intent_id, client, via, transcript
             FROM audit_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit], |r| {
            let tool_kind: Option<String> = r.get(6)?;
            let locations: Option<String> = r.get(8)?;
            let decided_by: String = r.get(9)?;
            let option_kind: Option<String> = r.get(12)?;
            let outcome: String = r.get(13)?;
            Ok(AuditEntry {
                id: r.get(0)?,
                ts: r.get(1)?,
                machine_id: r.get(15)?,
                request_id: r.get(2)?,
                session_id: r.get(3)?,
                agent_id: r.get(4)?,
                tool_call_id: r.get(5)?,
                tool_kind: tool_kind.as_deref().and_then(enum_from),
                tool_title: r.get(7)?,
                locations: locations.as_deref().and_then(|l| serde_json::from_str(l).ok()).unwrap_or_default(),
                decided_by: enum_from(&decided_by).unwrap_or(ResolvedBy::User),
                actor: r.get(10)?,
                option_id: r.get(11)?,
                option_kind: option_kind.as_deref().and_then(enum_from),
                outcome: enum_from(&outcome).unwrap_or(AuditOutcome::Failed),
                detail: r.get(14)?,
                origin: match (r.get::<_, Option<String>>(16)?, r.get::<_, Option<String>>(18)?) {
                    (Some(machine_id), Some(intent_id)) => Some(AuditOrigin {
                        machine_id,
                        name: r.get(17)?,
                        intent_id,
                        client: r.get::<_, Option<String>>(19)?.unwrap_or_default(),
                    }),
                    _ => None,
                },
                via: r.get(20)?,
                transcript: r.get(21)?,
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    // ------------------------------------------------------------ devices (pairing roster)

    /// A paired device that is not revoked.
    pub fn roster_entry(&self, device_id: &str) -> Result<Option<RosterEntry>> {
        let c = self.conn();
        let sql = format!("SELECT {DEVICE_COLS} FROM devices WHERE id = ?1 AND revoked_at IS NULL");
        Ok(c.query_row(&sql, [device_id], row_roster_entry).optional()?)
    }

    /// A revoked device's row (revoking keeps its key and counters), so the link layer can answer
    /// its next hello with `bye{revoked}`. None for a paired or unknown id.
    pub fn revoked_roster_entry(&self, device_id: &str) -> Result<Option<RosterEntry>> {
        let c = self.conn();
        let sql = format!("SELECT {DEVICE_COLS} FROM devices WHERE id = ?1 AND revoked_at IS NOT NULL");
        Ok(c.query_row(&sql, [device_id], row_roster_entry).optional()?)
    }

    /// Every paired device that is not revoked, oldest pairing first.
    pub fn roster(&self) -> Result<Vec<RosterEntry>> {
        let c = self.conn();
        let sql = format!("SELECT {DEVICE_COLS} FROM devices WHERE revoked_at IS NULL ORDER BY paired_at, id");
        let mut st = c.prepare(&sql)?;
        let rows = st.query_map([], row_roster_entry)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Insert or re-pair a device: replaces its key and details and clears `revoked_at`. The receive
    /// counter restarts only when the key changed (frames under the old key can no longer be opened);
    /// the send counter never goes back. `introduced_by` is taken as given (a direct pairing has none).
    pub fn upsert_device(&self, e: &RosterEntry) -> Result<()> {
        let d = &e.device;
        self.conn().execute(
            "INSERT INTO devices (id, kind, name, os, hub_version, public_key, addr, paired_at, last_seen, introduced_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                kind = excluded.kind, name = excluded.name, os = excluded.os, hub_version = excluded.hub_version,
                last_rx_seq = CASE WHEN public_key = excluded.public_key THEN last_rx_seq ELSE 0 END,
                public_key = excluded.public_key, addr = excluded.addr, paired_at = excluded.paired_at,
                last_seen = excluded.last_seen, introduced_by = excluded.introduced_by, revoked_at = NULL",
            params![
                d.id,
                enum_str(&d.kind),
                d.name,
                enum_str(&device_os(e)),
                e.machine.hub_version,
                d.public_key,
                d.addr,
                d.paired_at,
                d.last_seen,
                d.introduced_by
            ],
        )?;
        Ok(())
    }

    /// Add a device another hub introduced. Never replaces anything: a no-op (false) when the id or
    /// the key is already stored, paired or revoked.
    pub fn add_introduced_device(&self, e: &RosterEntry) -> Result<bool> {
        let d = &e.device;
        let n = self.conn().execute(
            "INSERT INTO devices (id, kind, name, os, hub_version, public_key, addr, paired_at, last_seen, introduced_by)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10
             WHERE NOT EXISTS (SELECT 1 FROM devices WHERE id = ?1 OR public_key = ?6)",
            params![
                d.id,
                enum_str(&d.kind),
                d.name,
                enum_str(&device_os(e)),
                e.machine.hub_version,
                d.public_key,
                d.addr,
                d.paired_at,
                d.last_seen,
                d.introduced_by
            ],
        )?;
        Ok(n > 0)
    }

    /// Every stored device, paired or revoked (`true` = revoked), oldest pairing first. Intros are
    /// checked against all of them: a revoked id or key is never re-added by an intro.
    pub fn roster_with_revoked(&self) -> Result<Vec<(RosterEntry, bool)>> {
        let c = self.conn();
        let sql = format!("SELECT {DEVICE_COLS}, revoked_at FROM devices ORDER BY paired_at, id");
        let mut st = c.prepare(&sql)?;
        let rows = st.query_map([], |r| Ok((row_roster_entry(r)?, r.get::<_, Option<UnixMs>>(10)?.is_some())))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Remember that `recipient` is owed an intro of each of `device_ids` (sent on its next link).
    pub fn queue_roster_intros(&self, recipient: &str, device_ids: &[String], ts: UnixMs) -> Result<()> {
        let c = self.conn();
        let mut st = c.prepare(
            "INSERT OR IGNORE INTO roster_intro_pending (recipient_id, device_id, queued_at) VALUES (?1, ?2, ?3)",
        )?;
        for id in device_ids.iter().filter(|id| id.as_str() != recipient) {
            st.execute(params![recipient, id, ts])?;
        }
        Ok(())
    }

    /// The device ids `recipient` is still owed an intro of, oldest first.
    pub fn pending_roster_intros(&self, recipient: &str) -> Result<Vec<String>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT device_id FROM roster_intro_pending WHERE recipient_id = ?1 ORDER BY queued_at, device_id",
        )?;
        let rows = st.query_map([recipient], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    /// The intros of `device_ids` reached `recipient`.
    pub fn clear_roster_intros(&self, recipient: &str, device_ids: &[String]) -> Result<()> {
        let c = self.conn();
        let mut st = c.prepare("DELETE FROM roster_intro_pending WHERE recipient_id = ?1 AND device_id = ?2")?;
        for id in device_ids {
            st.execute(params![recipient, id])?;
        }
        Ok(())
    }

    /// An authenticated frame from a paired device: refresh what it says about itself. `addr` None
    /// keeps the stored address (a hub without a listener, or one bound to 0.0.0.0, sends none).
    pub fn update_device_seen(&self, device_id: &str, m: &Machine, addr: Option<&str>, ts: UnixMs) -> Result<()> {
        self.conn().execute(
            "UPDATE devices SET name = ?2, os = ?3, hub_version = ?4, addr = COALESCE(?5, addr),
                    last_seen = MAX(COALESCE(last_seen, 0), ?6)
             WHERE id = ?1 AND revoked_at IS NULL",
            params![device_id, m.name, enum_str(&m.os), m.hub_version, addr, ts],
        )?;
        Ok(())
    }

    /// Mark a device revoked (and drop the intros owed to it or about it). True if it was paired
    /// (and not already revoked).
    pub fn revoke_device(&self, device_id: &str, ts: UnixMs) -> Result<bool> {
        let c = self.conn();
        let n = c.execute("UPDATE devices SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL", params![device_id, ts])?;
        c.execute("DELETE FROM roster_intro_pending WHERE recipient_id = ?1 OR device_id = ?1", [device_id])?;
        Ok(n > 0)
    }

    /// A frame counter (`last_rx_seq` / `last_tx_seq`); 0 for an unknown device.
    fn device_seq(&self, device_id: &str, col: SeqCol) -> Result<u64> {
        let sql = format!("SELECT {} FROM devices WHERE id = ?1", col.name());
        let v: Option<i64> = self.conn().query_row(&sql, [device_id], |r| r.get(0)).optional()?;
        // Stored bit-for-bit as i64 (SQLite has no u64).
        Ok(v.map_or(0, |v| v as u64))
    }

    fn set_device_seq(&self, device_id: &str, col: SeqCol, seq: u64) -> Result<()> {
        let sql = format!("UPDATE devices SET {} = ?2 WHERE id = ?1", col.name());
        self.conn().execute(&sql, params![device_id, seq as i64])?;
        Ok(())
    }

    /// Append a pairing audit entry (`id` is ignored and assigned). Returns the new id.
    pub fn insert_pairing_audit(&self, a: &PairingAuditEntry) -> Result<i64> {
        let c = self.conn();
        c.execute(
            "INSERT INTO pairing_audit (ts, machine_id, kind, device_id, device_name, remote_addr, actor, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![a.ts, a.machine_id, enum_str(&a.kind), a.device_id, a.device_name, a.remote_addr, a.actor, a.detail],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Append a settings audit entry (`id` is ignored and assigned); keeps the newest
    /// SETTINGS_AUDIT_MAX_ROWS. Returns the new id.
    pub fn insert_settings_audit(&self, a: &SettingsAuditEntry) -> Result<i64> {
        let c = self.conn();
        c.execute(
            "INSERT INTO settings_audit (ts, machine_id, kind, actor, client, keys, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![a.ts, a.machine_id, enum_str(&a.kind), a.actor, a.client, serde_json::to_string(&a.keys)?, a.detail],
        )?;
        let id = c.last_insert_rowid();
        c.execute("DELETE FROM settings_audit WHERE id <= ?1", params![id - SETTINGS_AUDIT_MAX_ROWS])?;
        Ok(id)
    }

    /// Log one call to the localhost A2A server; keeps the newest A2A_LOG_MAX_ROWS.
    pub fn insert_a2a_log(&self, e: &A2aLogEntry) -> Result<i64> {
        let c = self.conn();
        c.execute(
            "INSERT INTO a2a_log (ts, caller, method, target, status, detail) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![e.ts, e.caller, e.method, e.target, e.status, e.detail],
        )?;
        let id = c.last_insert_rowid();
        c.execute("DELETE FROM a2a_log WHERE id <= ?1", params![id - A2A_LOG_MAX_ROWS])?;
        Ok(id)
    }

    /// Latest A2A server calls (newest first).
    pub fn a2a_log(&self, limit: u32) -> Result<Vec<A2aLogEntry>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT id, ts, caller, method, target, status, detail FROM a2a_log ORDER BY id DESC LIMIT ?1")?;
        let rows = st.query_map([limit], |r| {
            Ok(A2aLogEntry { id: r.get(0)?, ts: r.get(1)?, caller: r.get(2)?, method: r.get(3)?, target: r.get(4)?, status: r.get(5)?, detail: r.get(6)? })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Latest settings audit entries (newest first).
    pub fn settings_audit(&self, limit: u32) -> Result<Vec<SettingsAuditEntry>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT id, ts, machine_id, kind, actor, client, keys, detail FROM settings_audit ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit], |r| {
            let kind: String = r.get(3)?;
            let keys: String = r.get(6)?;
            Ok(SettingsAuditEntry {
                id: r.get(0)?,
                ts: r.get(1)?,
                machine_id: r.get(2)?,
                kind: enum_from(&kind).unwrap_or(SettingsAuditKind::Settings),
                actor: r.get(4)?,
                client: r.get(5)?,
                keys: serde_json::from_str(&keys).unwrap_or_default(),
                detail: r.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Latest pairing audit entries (newest first).
    pub fn pairing_audit(&self, limit: u32) -> Result<Vec<PairingAuditEntry>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT id, ts, machine_id, kind, device_id, device_name, remote_addr, actor, detail
             FROM pairing_audit ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = st.query_map([limit], |r| {
            let kind: String = r.get(3)?;
            Ok(PairingAuditEntry {
                id: r.get(0)?,
                ts: r.get(1)?,
                machine_id: r.get(2)?,
                kind: enum_from(&kind).unwrap_or(PairingAuditKind::PairRejected),
                device_id: r.get(4)?,
                device_name: r.get(5)?,
                remote_addr: r.get(6)?,
                actor: r.get(7)?,
                detail: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // ------------------------------------------------------------ mirror (paired hubs' data, read-only)
    //
    // Mirror rows live in the normal tables with the owner's machine_id. Every function here refuses
    // this hub's own machine id and re-checks that each item belongs to the machine it is filed
    // under (the hub validates first; this is the last line of defence). Nothing here ever updates a
    // row whose machine_id differs from the item's.

    fn check_mirror_machine(&self, machine_id: &str) -> Result<()> {
        if machine_id == self.machine_id || !is_valid_machine_id(machine_id) {
            bail!("refusing a mirror write for machine {machine_id:?}");
        }
        Ok(())
    }

    /// Upsert a mirrored session of `s.machine_id`, then evict that machine's oldest sessions beyond
    /// [`MIRROR_MAX_SESSIONS`]. Returns the ids evicted (the caller drops them from its cache).
    pub fn mirror_upsert_session(&self, s: &Session) -> Result<Vec<SessionId>> {
        self.check_mirror_machine(&s.machine_id)?;
        if !owned_by(&s.id, &s.machine_id) {
            bail!("session id is not of machine {}", s.machine_id);
        }
        let mut c = self.conn();
        let tx = c.transaction()?;
        write_session(&tx, s, true)?;
        let evicted = cap_mirror_sessions(&tx, &s.machine_id)?;
        tx.commit()?;
        Ok(evicted)
    }

    /// Set the per-machine budget for mirrored payload bytes (default MIRROR_MAX_PAYLOAD_BYTES).
    pub fn set_mirror_byte_budget(&self, bytes: i64) {
        self.mirror_byte_budget.store(bytes.max(0), Ordering::Relaxed);
    }

    /// Store mirrored events of `machine_id`, idempotently (keyed by machine + seq). Events whose id is
    /// not `machine:seq`, that belong elsewhere, or whose session is not mirrored are skipped. Returns
    /// the events that were new. `advance_cursor`: the link is in sync, so the sync cursor moves to
    /// the highest seq that is now stored (inserted or already there); skipped events never move it.
    /// The machine's mirrored payload bytes are kept within the byte budget (oldest evicted).
    pub fn mirror_insert_events(&self, machine_id: &str, events: &[HubEvent], advance_cursor: bool) -> Result<Vec<HubEvent>> {
        self.check_mirror_machine(machine_id)?;
        let mut c = self.conn();
        let tx = c.transaction()?;
        let mut new = Vec::new();
        let mut touched: Vec<&str> = Vec::new();
        let mut max_seq = 0;
        let mut added_bytes: i64 = 0;
        for e in events {
            if e.machine_id != machine_id || e.seq <= 0 || e.id != global_id(machine_id, e.seq) || !owned_by(&e.session_id, machine_id) {
                continue;
            }
            let known = tx
                .query_row("SELECT 1 FROM sessions WHERE id = ?1 AND machine_id = ?2", params![e.session_id, machine_id], |_| Ok(()))
                .optional()?
                .is_some();
            if !known {
                continue;
            }
            let payload = e.payload.to_string();
            let n = tx.execute(
                "INSERT INTO events (machine_id, seq, session_id, type, payload, ts) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(machine_id, seq) DO NOTHING",
                params![machine_id, e.seq, e.session_id, e.kind.as_str(), payload, e.ts],
            )?;
            // Inserted now or already stored: either way it is in the mirror.
            max_seq = max_seq.max(e.seq);
            if n == 1 {
                added_bytes += payload.len() as i64;
                tx.execute(
                    "UPDATE sessions SET last_event_at = max(last_event_at, ?2) WHERE id = ?1 AND machine_id = ?3",
                    params![e.session_id, e.ts, machine_id],
                )?;
                if !touched.contains(&e.session_id.as_str()) {
                    touched.push(&e.session_id);
                }
                new.push(e.clone());
            }
        }
        for sid in touched {
            cap_mirror_session_events(&tx, machine_id, sid)?;
        }
        if added_bytes > 0 {
            enforce_byte_budget(&tx, machine_id, added_bytes, self.mirror_byte_budget.load(Ordering::Relaxed))?;
        }
        if advance_cursor && max_seq > 0 {
            tx.execute(
                "UPDATE devices SET mirror_seq = MAX(mirror_seq, ?2) WHERE id = ?1",
                params![machine_id, max_seq],
            )?;
        }
        tx.commit()?;
        Ok(new)
    }

    /// Upsert a mirrored pet. A machine gets at most [`MIRROR_MAX_PETS`]; one per agent.
    pub fn mirror_upsert_pet(&self, p: &Pet) -> Result<()> {
        self.check_mirror_machine(&p.machine_id)?;
        if !owned_by(&p.id, &p.machine_id) {
            bail!("pet id is not of machine {}", p.machine_id);
        }
        let mut c = self.conn();
        let tx = c.transaction()?;
        write_mirror_pet(&tx, p)?;
        tx.commit()?;
        Ok(())
    }

    /// Record a mirrored pending permission request (at most [`MIRROR_MAX_PENDING`] per machine).
    pub fn mirror_put_pending(&self, r: &PermissionRequest) -> Result<()> {
        self.check_mirror_machine(&r.machine_id)?;
        if !owned_by(&r.request_id, &r.machine_id) || !owned_by(&r.session_id, &r.machine_id) {
            bail!("request ids are not of machine {}", r.machine_id);
        }
        let mut c = self.conn();
        let tx = c.transaction()?;
        write_mirror_pending(&tx, r)?;
        tx.commit()?;
        Ok(())
    }

    /// Drop a mirrored pending request. True if it was there.
    pub fn mirror_remove_pending(&self, machine_id: &str, request_id: &str) -> Result<bool> {
        self.check_mirror_machine(machine_id)?;
        let n = self.conn().execute(
            "DELETE FROM mirror_pending WHERE request_id = ?1 AND machine_id = ?2",
            params![request_id, machine_id],
        )?;
        Ok(n > 0)
    }

    /// Apply a `mirror_snapshot`: it replaces that machine's sessions (and their events), pets and
    /// pending requests. Items not of `machine_id` are skipped.
    pub fn mirror_replace(
        &self,
        machine_id: &str,
        sessions: &[Session],
        pets: &[Pet],
        pending: &[PermissionRequest],
    ) -> Result<()> {
        self.check_mirror_machine(machine_id)?;
        let mut c = self.conn();
        let tx = c.transaction()?;
        let keep: HashSet<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
        let existing: Vec<String> =
            select_rows(&tx, "SELECT id FROM sessions WHERE machine_id = ?1", |r| r.get(0), params![machine_id])?;
        for id in existing.iter().filter(|id| !keep.contains(id.as_str())) {
            tx.execute("DELETE FROM events WHERE session_id = ?1 AND machine_id = ?2", params![id, machine_id])?;
            tx.execute("DELETE FROM sessions WHERE id = ?1 AND machine_id = ?2", params![id, machine_id])?;
        }
        for s in sessions.iter().filter(|s| s.machine_id == machine_id && owned_by(&s.id, machine_id)) {
            write_session(&tx, s, true)?;
        }
        cap_mirror_sessions(&tx, machine_id)?;
        let keep_pets: HashSet<&str> = pets.iter().map(|p| p.id.as_str()).collect();
        let existing: Vec<String> =
            select_rows(&tx, "SELECT id FROM pets WHERE machine_id = ?1", |r| r.get(0), params![machine_id])?;
        for id in existing.iter().filter(|id| !keep_pets.contains(id.as_str())) {
            tx.execute("DELETE FROM pets WHERE id = ?1 AND machine_id = ?2", params![id, machine_id])?;
        }
        for p in pets.iter().filter(|p| p.machine_id == machine_id && owned_by(&p.id, machine_id)) {
            if let Err(e) = write_mirror_pet(&tx, p) {
                tracing::debug!("mirror snapshot: skipped a pet: {e:#}");
            }
        }
        tx.execute("DELETE FROM mirror_pending WHERE machine_id = ?1", params![machine_id])?;
        for r in pending.iter().filter(|r| {
            r.machine_id == machine_id && owned_by(&r.request_id, machine_id) && owned_by(&r.session_id, machine_id)
        }) {
            if let Err(e) = write_mirror_pending(&tx, r) {
                tracing::debug!("mirror snapshot: skipped a request: {e:#}");
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Mirrored sessions (every machine but this one) that are open or had activity since `since`.
    pub fn mirror_sessions(&self, since: UnixMs) -> Result<Vec<Session>> {
        let c = self.conn();
        let mut st = c.prepare(&format!(
            "SELECT {SESSION_COLS} FROM sessions WHERE machine_id != ?1 AND (ended_at IS NULL OR last_event_at >= ?2)
             ORDER BY last_event_at DESC"
        ))?;
        let rows = st.query_map(params![self.machine_id, since], row_session)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Every mirrored pending permission request, oldest first.
    pub fn mirror_pending(&self) -> Result<Vec<PermissionRequest>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT data FROM mirror_pending ORDER BY created_at, request_id")?;
        let rows = st.query_map([], |r| from_json::<PermissionRequest>(&r.get::<_, String>(0)?))?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Whether `id` is a mirrored session (of a machine other than this one).
    pub fn is_mirrored_session(&self, id: &str) -> Result<bool> {
        let c = self.conn();
        Ok(c
            .query_row("SELECT 1 FROM sessions WHERE id = ?1 AND machine_id != ?2", params![id, self.machine_id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    /// The seq this hub has mirrored `machine_id`'s events up to without gaps (0 = nothing yet).
    pub fn mirror_cursor(&self, machine_id: &str) -> Result<i64> {
        let v: Option<i64> = self
            .conn()
            .query_row("SELECT mirror_seq FROM devices WHERE id = ?1", [machine_id], |r| r.get(0))
            .optional()?;
        Ok(v.unwrap_or(0))
    }

    /// End of a sync: the cursor moves to the highest mirrored seq actually stored (never to what the
    /// peer claims). Returns the new cursor.
    pub fn finish_mirror_sync(&self, machine_id: &str) -> Result<i64> {
        self.check_mirror_machine(machine_id)?;
        let c = self.conn();
        let stored: Option<i64> =
            c.query_row("SELECT MAX(seq) FROM events WHERE machine_id = ?1", [machine_id], |r| r.get(0))?;
        c.execute(
            "UPDATE devices SET mirror_seq = MAX(mirror_seq, ?2) WHERE id = ?1",
            params![machine_id, stored.unwrap_or(0)],
        )?;
        let cursor: Option<i64> =
            c.query_row("SELECT mirror_seq FROM devices WHERE id = ?1", [machine_id], |r| r.get(0)).optional()?;
        Ok(cursor.unwrap_or(0))
    }

    /// The peer's seq space restarted (its hub.db was reset): forget its mirrored events and cursor.
    pub fn reset_mirror_events(&self, machine_id: &str) -> Result<()> {
        self.check_mirror_machine(machine_id)?;
        let mut c = self.conn();
        let tx = c.transaction()?;
        tx.execute("DELETE FROM events WHERE machine_id = ?1", [machine_id])?;
        tx.execute("UPDATE devices SET mirror_seq = 0, mirror_bytes = 0 WHERE id = ?1", [machine_id])?;
        tx.commit()?;
        Ok(())
    }

    /// The highest event seq this machine has ever issued (0 if none). Taken from the AUTOINCREMENT
    /// high-water mark, so pruning old events never makes it go back (only a reset hub.db does).
    pub fn local_max_seq(&self) -> Result<i64> {
        let c = self.conn();
        let issued: Option<i64> =
            c.query_row("SELECT seq FROM sqlite_sequence WHERE name = 'events'", [], |r| r.get(0)).optional()?;
        let stored: Option<i64> =
            c.query_row("SELECT MAX(seq) FROM events WHERE machine_id = ?1", [&self.machine_id], |r| r.get(0))?;
        Ok(issued.unwrap_or(0).max(stored.unwrap_or(0)))
    }

    /// Delete everything mirrored from `machine_id` (revoke) and reset its sync cursor.
    pub fn delete_mirror(&self, machine_id: &str) -> Result<()> {
        self.check_mirror_machine(machine_id)?;
        let mut c = self.conn();
        let tx = c.transaction()?;
        for table in ["events", "sessions", "pets", "mirror_pending"] {
            tx.execute(&format!("DELETE FROM {table} WHERE machine_id = ?1"), [machine_id])?;
        }
        tx.execute("UPDATE devices SET mirror_seq = 0, mirror_bytes = 0 WHERE id = ?1", [machine_id])?;
        tx.commit()?;
        Ok(())
    }

    /// Delete mirror rows of machines that are neither this one nor in `paired` (left over from a
    /// revoke that happened while the hub was down). Returns the number of rows removed.
    pub fn delete_unpaired_mirrors(&self, paired: &[String]) -> Result<usize> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let mut keep: Vec<&str> = paired.iter().map(String::as_str).collect();
        keep.push(&self.machine_id);
        let mut machines: Vec<String> = Vec::new();
        for table in ["events", "sessions", "pets", "mirror_pending"] {
            let ms: Vec<String> = select_rows(&tx, &format!("SELECT DISTINCT machine_id FROM {table}"), |r| r.get(0), [])?;
            machines.extend(ms);
        }
        machines.sort();
        machines.dedup();
        let mut n = 0;
        for m in machines.iter().filter(|m| !keep.contains(&m.as_str()) && !m.is_empty()) {
            for table in ["events", "sessions", "pets", "mirror_pending"] {
                n += tx.execute(&format!("DELETE FROM {table} WHERE machine_id = ?1"), [m])?;
            }
        }
        tx.commit()?;
        Ok(n)
    }

    /// Housekeeping: drop mirrored sessions that ended more than [`MIRROR_RETENTION_MS`] ago (with their
    /// events) and enforce the per-machine caps. Returns the number of sessions removed (age and cap).
    pub fn prune_mirror(&self, now: UnixMs) -> Result<usize> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        let old: Vec<(String, String)> = select_rows(
            &tx,
            "SELECT id, machine_id FROM sessions WHERE machine_id != ?1 AND ended_at IS NOT NULL AND ended_at < ?2",
            |r| Ok((r.get(0)?, r.get(1)?)),
            params![self.machine_id, now - MIRROR_RETENTION_MS],
        )?;
        for (id, m) in &old {
            tx.execute("DELETE FROM events WHERE session_id = ?1 AND machine_id = ?2", params![id, m])?;
            tx.execute("DELETE FROM sessions WHERE id = ?1 AND machine_id = ?2", params![id, m])?;
        }
        let mut removed = old.len();
        let machines: Vec<String> =
            select_rows(&tx, "SELECT DISTINCT machine_id FROM sessions WHERE machine_id != ?1", |r| r.get(0), [&self.machine_id])?;
        for m in &machines {
            removed += cap_mirror_sessions(&tx, m)?.len();
            let sids: Vec<String> =
                select_rows(&tx, "SELECT id FROM sessions WHERE machine_id = ?1", |r| r.get(0), [m])?;
            for sid in &sids {
                cap_mirror_session_events(&tx, m, sid)?;
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    // ------------------------------------------------------------ owner side of a sync

    /// This machine's sessions for a `mirror_snapshot`: open ones and those ended since
    /// `ended_since`, newest activity first, at most `limit`.
    pub fn sync_sessions(&self, ended_since: UnixMs, limit: u32) -> Result<Vec<Session>> {
        let c = self.conn();
        let mut st = c.prepare(&format!(
            "SELECT {SESSION_COLS} FROM sessions WHERE machine_id = ?1 AND (ended_at IS NULL OR ended_at >= ?2)
             ORDER BY last_event_at DESC LIMIT ?3"
        ))?;
        let rows = st.query_map(params![self.machine_id, ended_since, limit], row_session)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// This machine's events with seq > `since`: the newest `limit` of them, oldest first, and
    /// whether older matching events were left out.
    pub fn local_events_after(&self, since: i64, limit: usize) -> Result<(Vec<HubEvent>, bool)> {
        let c = self.conn();
        let mut st = c.prepare(&format!(
            "SELECT {EVENT_COLS} FROM events WHERE machine_id = ?1 AND seq > ?2 ORDER BY seq DESC LIMIT ?3"
        ))?;
        let rows = st.query_map(params![self.machine_id, since, limit as i64 + 1], row_event)?;
        let mut v: Vec<HubEvent> = rows.filter_map(|r| r.ok()).collect();
        let truncated = v.len() > limit;
        v.truncate(limit);
        v.reverse();
        Ok((v, truncated))
    }

    // ------------------------------------------------------------ settings

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.conn().query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

/// A name another machine reported for itself, made safe to store and show: control characters
/// dropped, trimmed, at most 40 characters, "Unnamed machine" if nothing is left. It feeds audit
/// actor labels, notifications and every client.
pub fn sanitize_machine_name(name: &str) -> String {
    let kept: String = name.chars().filter(|c| !c.is_control()).collect();
    let t: String = kept.trim().chars().take(40).collect();
    let t = t.trim_end();
    if t.is_empty() {
        "Unnamed machine".to_string()
    } else {
        t.to_string()
    }
}

/// The pairing roster `pawl-peer` reads and updates. Quick synchronous calls on the store's
/// connection; a failing write is logged (the link layer has no way to report it).
impl Roster for Store {
    fn get(&self, device_id: &str) -> Option<RosterEntry> {
        self.roster_entry(device_id).unwrap_or_else(|e| {
            tracing::warn!("roster: read {device_id}: {e:#}");
            None
        })
    }

    fn get_revoked(&self, device_id: &str) -> Option<RosterEntry> {
        self.revoked_roster_entry(device_id).unwrap_or_else(|e| {
            tracing::warn!("roster: read revoked {device_id}: {e:#}");
            None
        })
    }

    fn list(&self) -> Vec<RosterEntry> {
        self.roster().unwrap_or_else(|e| {
            tracing::warn!("roster: list: {e:#}");
            Vec::new()
        })
    }

    fn upsert(&self, mut entry: RosterEntry) {
        entry.device.name = sanitize_machine_name(&entry.device.name);
        entry.machine.name = sanitize_machine_name(&entry.machine.name);
        if let Err(e) = self.upsert_device(&entry) {
            tracing::warn!("roster: save {}: {e:#}", entry.device.id);
        }
    }

    fn update_seen(&self, device_id: &str, machine: &Machine, addr: Option<&str>, ts: UnixMs) {
        let machine = Machine { name: sanitize_machine_name(&machine.name), ..machine.clone() };
        if let Err(e) = self.update_device_seen(device_id, &machine, addr, ts) {
            tracing::warn!("roster: update {device_id}: {e:#}");
        }
    }

    fn revoke(&self, device_id: &str) {
        if let Err(e) = self.revoke_device(device_id, pawl_protocol::now_ms()) {
            tracing::warn!("roster: revoke {device_id}: {e:#}");
        }
    }

    fn last_rx_seq(&self, device_id: &str) -> u64 {
        self.device_seq(device_id, SeqCol::Rx).unwrap_or_else(|e| {
            tracing::warn!("roster: read rx seq of {device_id}: {e:#}");
            // Unknown: refuse every frame rather than accept a replay.
            u64::MAX
        })
    }

    fn set_last_rx_seq(&self, device_id: &str, seq: u64) {
        if let Err(e) = self.set_device_seq(device_id, SeqCol::Rx, seq) {
            tracing::warn!("roster: save rx seq of {device_id}: {e:#}");
        }
    }

    fn last_tx_seq(&self, device_id: &str) -> u64 {
        self.device_seq(device_id, SeqCol::Tx).unwrap_or_else(|e| {
            tracing::warn!("roster: read tx seq of {device_id}: {e:#}");
            0
        })
    }

    fn set_last_tx_seq(&self, device_id: &str, seq: u64) {
        if let Err(e) = self.set_device_seq(device_id, SeqCol::Tx, seq) {
            tracing::warn!("roster: save tx seq of {device_id}: {e:#}");
        }
    }
}

fn migrate(conn: &mut Connection, machine_id: &str) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let target = MIGRATIONS.len() as i64;
    if current > target {
        bail!("hub.db schema v{current} is newer than this pawl (v{target}); upgrade pawl");
    }
    for (i, step) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let version = i + 1;
        let tx = conn.transaction()?;
        match step {
            Migration::Sql(sql) => tx.execute_batch(sql).map_err(anyhow::Error::from),
            Migration::Code(f) => f(&tx, machine_id),
        }
        .with_context(|| format!("migration v{version}"))?;
        tx.pragma_update(None, "user_version", version as i64)?;
        tx.commit()?;
        if current > 0 {
            tracing::info!("migrated hub.db to schema v{version}");
        }
    }
    Ok(())
}

/// The recorded machine id, if the settings table exists and has one.
fn stored_machine_id(conn: &Connection) -> Result<Option<String>> {
    let has_settings: bool = conn
        .query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'settings'", [], |_| Ok(()))
        .optional()?
        .is_some();
    if !has_settings {
        return Ok(None);
    }
    Ok(conn.query_row("SELECT value FROM settings WHERE key = ?1", [SETTING_MACHINE_ID], |r| r.get(0)).optional()?)
}

/// GLOB pattern for an id that is already global: 12 chars of `[a-z0-9]`, ':', at least one char.
fn global_id_glob() -> String {
    format!("{}:?*", "[a-z0-9]".repeat(12))
}

/// `id` in global form for this machine (unchanged if it already is).
fn globalize(machine_id: &str, id: &str) -> String {
    if parse_global_id(id).is_some() {
        id.to_string()
    } else {
        global_id(machine_id, id)
    }
}

/// Globalize a string field of a JSON object in place.
fn globalize_field(obj: &mut serde_json::Map<String, Value>, key: &str, machine_id: &str) {
    if let Some(Value::String(v)) = obj.get_mut(key) {
        if !v.is_empty() {
            *v = globalize(machine_id, v);
        }
    }
}

/// `id` is a global id of `machine_id`.
fn owned_by(id: &str, machine_id: &str) -> bool {
    parse_global_id(id).is_some_and(|(m, _)| m == machine_id)
}

/// `query_rows` with parameters.
fn select_rows<T, F, P>(tx: &Transaction<'_>, sql: &str, f: F, p: P) -> Result<Vec<T>>
where
    F: FnMut(&Row<'_>) -> rusqlite::Result<T>,
    P: rusqlite::Params,
{
    let mut st = tx.prepare(sql)?;
    let rows = st.query_map(p, f)?;
    Ok(rows.collect::<rusqlite::Result<Vec<T>>>()?)
}

/// Insert or update a session row. `mirror`: only ever update a row of the same machine.
fn write_session(c: &Connection, s: &Session, mirror: bool) -> Result<()> {
    let guard = if mirror { " WHERE sessions.machine_id = excluded.machine_id" } else { "" };
    c.execute(
        &format!(
            "INSERT INTO sessions (id, agent_id, source, status, external_id, acp_session_id, cwd, title,
                                   started_at, last_event_at, ended_at, data, machine_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(id) DO UPDATE SET
                machine_id = excluded.machine_id, agent_id = excluded.agent_id, source = excluded.source, status = excluded.status,
                external_id = excluded.external_id, acp_session_id = excluded.acp_session_id, cwd = excluded.cwd,
                title = excluded.title, started_at = excluded.started_at,
                last_event_at = max(sessions.last_event_at, excluded.last_event_at),
                ended_at = excluded.ended_at, data = excluded.data{guard}"
        ),
        params![
            s.id,
            s.agent_id,
            enum_str(&s.source),
            enum_str(&s.status),
            s.external_id,
            s.acp_session_id,
            s.cwd,
            s.title,
            s.started_at,
            s.last_event_at,
            s.ended_at,
            json(s)?,
            s.machine_id
        ],
    )?;
    Ok(())
}

/// Insert or replace a mirrored pet (one per agent per machine; at most MIRROR_MAX_PETS per machine).
fn write_mirror_pet(tx: &Transaction<'_>, p: &Pet) -> Result<()> {
    let foreign = tx
        .query_row("SELECT 1 FROM pets WHERE id = ?1 AND machine_id != ?2", params![p.id, p.machine_id], |_| Ok(()))
        .optional()?
        .is_some();
    if foreign {
        bail!("pet id belongs to another machine");
    }
    let exists = tx
        .query_row(
            "SELECT 1 FROM pets WHERE machine_id = ?1 AND (id = ?2 OR agent_id = ?3)",
            params![p.machine_id, p.id, p.agent_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let count: i64 = tx.query_row("SELECT COUNT(*) FROM pets WHERE machine_id = ?1", [&p.machine_id], |r| r.get(0))?;
    if !exists && count >= MIRROR_MAX_PETS {
        bail!("machine {} already has {MIRROR_MAX_PETS} mirrored pets", p.machine_id);
    }
    // REPLACE only ever removes rows of the same machine: the id and (machine, agent) both match it.
    tx.execute(
        "INSERT OR REPLACE INTO pets (id, machine_id, agent_id, data) VALUES (?1, ?2, ?3, ?4)",
        params![p.id, p.machine_id, p.agent_id, json(p)?],
    )?;
    Ok(())
}

fn write_mirror_pending(tx: &Transaction<'_>, r: &PermissionRequest) -> Result<()> {
    let exists = tx
        .query_row("SELECT machine_id FROM mirror_pending WHERE request_id = ?1", [&r.request_id], |row| row.get::<_, String>(0))
        .optional()?;
    match exists {
        Some(m) if m != r.machine_id => bail!("request id belongs to another machine"),
        Some(_) => {}
        None => {
            let count: i64 =
                tx.query_row("SELECT COUNT(*) FROM mirror_pending WHERE machine_id = ?1", [&r.machine_id], |row| row.get(0))?;
            if count >= MIRROR_MAX_PENDING {
                bail!("machine {} already has {MIRROR_MAX_PENDING} mirrored requests", r.machine_id);
            }
        }
    }
    tx.execute(
        "INSERT INTO mirror_pending (request_id, machine_id, session_id, created_at, data) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(request_id) DO UPDATE SET session_id = excluded.session_id, created_at = excluded.created_at,
                                               data = excluded.data
         WHERE mirror_pending.machine_id = excluded.machine_id",
        params![r.request_id, r.machine_id, r.session_id, r.created_at, json(r)?],
    )?;
    Ok(())
}

/// Keep a mirrored machine's newest MIRROR_MAX_SESSIONS sessions (by activity); drop the rest with
/// their events.
fn cap_mirror_sessions(tx: &Transaction<'_>, machine_id: &str) -> Result<Vec<String>> {
    let extra: Vec<String> = select_rows(
        tx,
        "SELECT id FROM sessions WHERE machine_id = ?1 ORDER BY last_event_at DESC, id LIMIT -1 OFFSET ?2",
        |r| r.get(0),
        params![machine_id, MIRROR_MAX_SESSIONS],
    )?;
    for id in &extra {
        tx.execute("DELETE FROM events WHERE session_id = ?1 AND machine_id = ?2", params![id, machine_id])?;
        tx.execute("DELETE FROM sessions WHERE id = ?1 AND machine_id = ?2", params![id, machine_id])?;
    }
    Ok(extra)
}

/// Keep a mirrored machine's event payload bytes within `budget`. `devices.mirror_bytes` is a cheap
/// upper bound (incremented on insert, never decremented by other deletes); only when it passes the
/// budget is the exact sum taken, and if that is over too, the oldest events go until 90% of the
/// budget is left.
fn enforce_byte_budget(tx: &Transaction<'_>, machine_id: &str, added: i64, budget: i64) -> Result<()> {
    let estimate: Option<i64> = tx
        .query_row(
            "UPDATE devices SET mirror_bytes = mirror_bytes + ?2 WHERE id = ?1 RETURNING mirror_bytes",
            params![machine_id, added],
            |r| r.get(0),
        )
        .optional()?;
    if estimate.is_some_and(|b| b <= budget) {
        return Ok(());
    }
    let exact: i64 = tx.query_row(
        "SELECT COALESCE(SUM(LENGTH(payload)), 0) FROM events WHERE machine_id = ?1",
        [machine_id],
        |r| r.get(0),
    )?;
    let mut total = exact;
    if exact > budget {
        let target = budget - budget / 10;
        let mut st = tx.prepare("SELECT seq, LENGTH(payload) FROM events WHERE machine_id = ?1 ORDER BY seq ASC")?;
        let mut rows = st.query([machine_id])?;
        let mut cutoff = None;
        while let Some(r) = rows.next()? {
            let (seq, len): (i64, i64) = (r.get(0)?, r.get(1)?);
            total -= len;
            cutoff = Some(seq);
            if total <= target {
                break;
            }
        }
        drop(rows);
        drop(st);
        if let Some(cutoff) = cutoff {
            tx.execute("DELETE FROM events WHERE machine_id = ?1 AND seq <= ?2", params![machine_id, cutoff])?;
            tracing::warn!(device = %machine_id, "mirror over its byte budget: evicted events up to seq {cutoff}");
        }
    }
    tx.execute("UPDATE devices SET mirror_bytes = ?2 WHERE id = ?1", params![machine_id, total])?;
    Ok(())
}

/// Keep a mirrored session's newest MIRROR_MAX_EVENTS_PER_SESSION events.
fn cap_mirror_session_events(tx: &Transaction<'_>, machine_id: &str, session_id: &str) -> Result<()> {
    let cutoff: Option<i64> = tx
        .query_row(
            "SELECT seq FROM events WHERE machine_id = ?1 AND session_id = ?2 ORDER BY seq DESC LIMIT 1 OFFSET ?3",
            params![machine_id, session_id, MIRROR_MAX_EVENTS_PER_SESSION],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(cutoff) = cutoff {
        tx.execute(
            "DELETE FROM events WHERE machine_id = ?1 AND session_id = ?2 AND seq <= ?3",
            params![machine_id, session_id, cutoff],
        )?;
    }
    Ok(())
}

fn query_rows<T, F>(tx: &Transaction<'_>, sql: &str, f: F) -> Result<Vec<T>>
where
    F: FnMut(&Row<'_>) -> rusqlite::Result<T>,
{
    let mut st = tx.prepare(sql)?;
    let rows = st.query_map([], f)?;
    Ok(rows.collect::<rusqlite::Result<Vec<T>>>()?)
}

/// v2 (machine identity): add `machine_id` everywhere and rewrite every stored v1 id to its global
/// form (`machine:local`). Event rowids stay the per-machine seq.
fn migrate_v2(tx: &Transaction<'_>, mid: &str) -> Result<()> {
    tx.execute_batch(SCHEMA_V2)?;
    let glob = global_id_glob();
    let set_machine = |obj: &mut serde_json::Map<String, Value>| {
        obj.insert("machineId".into(), Value::String(mid.to_string()));
    };

    // agents: ids stay local ("codex"); the JSON gains machineId.
    let rows: Vec<(String, String)> = query_rows(tx, "SELECT id, data FROM agents", |r| Ok((r.get(0)?, r.get(1)?)))?;
    for (id, data) in rows {
        let mut v: Value = serde_json::from_str(&data).with_context(|| format!("agent {id}"))?;
        if let Some(o) = v.as_object_mut() {
            set_machine(o);
        }
        tx.execute("UPDATE agents SET machine_id = ?1, data = ?2 WHERE id = ?3", params![mid, v.to_string(), id])?;
    }

    // sessions: id -> machine:id (column + JSON).
    let rows: Vec<(String, String)> = query_rows(tx, "SELECT id, data FROM sessions", |r| Ok((r.get(0)?, r.get(1)?)))?;
    for (old, data) in rows {
        let new = globalize(mid, &old);
        let mut v: Value = serde_json::from_str(&data).with_context(|| format!("session {old}"))?;
        if let Some(o) = v.as_object_mut() {
            o.insert("id".into(), Value::String(new.clone()));
            set_machine(o);
        }
        tx.execute(
            "UPDATE sessions SET id = ?1, machine_id = ?2, data = ?3 WHERE id = ?4",
            params![new, mid, v.to_string(), old],
        )?;
    }

    // pets: one per (machine, agent) with the deterministic id machine:pet-<agent>.
    let rows: Vec<(String, String, String)> =
        query_rows(tx, "SELECT id, agent_id, data FROM pets ORDER BY rowid", |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    let mut pet_ids: Vec<(String, String)> = Vec::with_capacity(rows.len());
    for (old, agent_id, data) in rows {
        let new = global_id(mid, format!("pet-{agent_id}"));
        let mut v: Value = serde_json::from_str(&data).with_context(|| format!("pet {old}"))?;
        if let Some(o) = v.as_object_mut() {
            o.insert("id".into(), Value::String(new.clone()));
            set_machine(o);
        }
        tx.execute(
            "INSERT INTO pets_v2 (id, machine_id, agent_id, data) VALUES (?1, ?2, ?3, ?4)",
            params![new, mid, agent_id, v.to_string()],
        )?;
        pet_ids.push((old, new));
    }
    tx.execute_batch("DROP TABLE pets; ALTER TABLE pets_v2 RENAME TO pets;")?;

    // events: session refs, and request/session ids inside permission payloads.
    tx.execute(
        "UPDATE events SET machine_id = ?1,
             session_id = CASE WHEN session_id GLOB ?2 THEN session_id ELSE ?1 || ':' || session_id END",
        params![mid, glob],
    )?;
    let rows: Vec<(i64, String, String)> = query_rows(
        tx,
        "SELECT id, type, payload FROM events WHERE type IN ('request_permission', 'permission_resolved')",
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    for (seq, kind, payload) in rows {
        let Ok(mut v) = serde_json::from_str::<Value>(&payload) else { continue };
        let Some(o) = v.as_object_mut() else { continue };
        globalize_field(o, "requestId", mid);
        globalize_field(o, "sessionId", mid);
        if kind == "request_permission" {
            set_machine(o); // the payload is a PermissionRequest
        }
        tx.execute("UPDATE events SET payload = ?1 WHERE id = ?2", params![v.to_string(), seq])?;
    }

    // chat: message ids, pet refs (old pet id -> new), session refs.
    for (old, new) in &pet_ids {
        tx.execute("UPDATE chat SET pet_id = ?1 WHERE pet_id = ?2", params![new, old])?;
    }
    tx.execute(
        "UPDATE chat SET machine_id = ?1,
             id = CASE WHEN id GLOB ?2 THEN id ELSE ?1 || ':' || id END,
             pet_id = CASE WHEN pet_id IS NULL OR pet_id GLOB ?2 THEN pet_id ELSE ?1 || ':' || pet_id END,
             session_id = CASE WHEN session_id IS NULL OR session_id GLOB ?2 THEN session_id ELSE ?1 || ':' || session_id END",
        params![mid, glob],
    )?;

    // audit log: request + session refs.
    tx.execute(
        "UPDATE audit_log SET machine_id = ?1,
             request_id = CASE WHEN request_id GLOB ?2 THEN request_id ELSE ?1 || ':' || request_id END,
             session_id = CASE WHEN session_id GLOB ?2 THEN session_id ELSE ?1 || ':' || session_id END",
        params![mid, glob],
    )?;

    tx.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![SETTING_MACHINE_ID, mid],
    )?;
    Ok(())
}

/// The machine id recorded in an existing hub.db (read-only peek; None if absent or unreadable).
pub fn recorded_machine_id(db: &Path) -> Option<MachineId> {
    if !db.exists() {
        return None;
    }
    let conn = Connection::open_with_flags(db, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    let _ = conn.busy_timeout(Duration::from_secs(2));
    let id = stored_machine_id(&conn).ok()??;
    is_valid_machine_id(&id).then_some(id)
}

/// Test helper: a schema-v1 hub.db (pre machine identity) holding `(id, agent_id, name)` pets in
/// that insertion order (oldest first).
#[cfg(test)]
pub(crate) fn create_v1_db_for_tests(path: &Path, pets: &[(&str, &str, &str)]) {
    let c = Connection::open(path).unwrap();
    c.execute_batch(SCHEMA_V1).unwrap();
    c.pragma_update(None, "user_version", 1).unwrap();
    for (id, agent_id, name) in pets {
        let data = serde_json::json!({
            "id": id, "agentId": agent_id, "species": "dog", "skin": "default",
            "name": name, "personality": "eager", "mood": 70, "xp": 0
        });
        c.execute("INSERT INTO pets (id, agent_id, data) VALUES (?1, ?2, ?3)", params![id, agent_id, data.to_string()])
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawl_protocol::{AgentSource, PermissionOutcome, RuleAction, RuleMatch, SessionSource, SessionStatus, Species};
    use serde_json::json;

    const MID: &str = "abcdef012345";

    fn gid(local: &str) -> String {
        global_id(MID, local)
    }

    fn session(id: &str, last: UnixMs) -> Session {
        Session {
            id: gid(id),
            machine_id: MID.into(),
            agent_id: "codex".into(),
            acp_session_id: None,
            external_id: Some("ext".into()),
            cwd: "/r".into(),
            title: None,
            source: SessionSource::CliHook,
            status: SessionStatus::Idle,
            started_at: 1,
            last_event_at: last,
            ended_at: None,
        }
    }

    #[test]
    fn migrations_are_idempotent_and_record_the_machine() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hub.db");
        Store::open(&p, MID).unwrap();
        let s = Store::open(&p, MID).unwrap();
        let v: i64 = s.conn().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        assert_eq!(s.setting(SETTING_MACHINE_ID).unwrap().as_deref(), Some(MID));
        assert_eq!(s.machine_id(), MID);
        drop(s);
        assert_eq!(recorded_machine_id(&p).as_deref(), Some(MID));
        assert_eq!(recorded_machine_id(&dir.path().join("missing.db")), None);
        let err = Store::open(&p, "zzzzzz999999").err().unwrap().to_string();
        assert!(err.contains("belongs to machine"), "{err}");
        assert!(Store::open_in_memory("Not-A-Machine").is_err());
    }

    /// A v1 database (single machine, local ids) is rewritten to global ids for this machine.
    #[test]
    fn v2_migration_rewrites_v1_ids() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hub.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(SCHEMA_V1).unwrap();
            c.pragma_update(None, "user_version", 1).unwrap();
            let agent = json!({"id":"codex","name":"Codex","command":["codex-acp"],"installed":true,"source":"registry"});
            c.execute("INSERT INTO agents (id, data, updated_at) VALUES ('codex', ?1, 1)", [agent.to_string()]).unwrap();
            let sess = json!({"id":"s-1","agentId":"codex","cwd":"/r","source":"owned","status":"done","startedAt":1,"lastEventAt":5});
            c.execute(
                "INSERT INTO sessions (id, agent_id, source, status, cwd, started_at, last_event_at, data)
                 VALUES ('s-1', 'codex', 'owned', 'done', '/r', 1, 5, ?1)",
                [sess.to_string()],
            )
            .unwrap();
            let req = json!({"requestId":"r-1","sessionId":"s-1","agentId":"codex","toolCall":{"toolCallId":"t"},"options":[],"createdAt":2});
            c.execute("INSERT INTO events (session_id, type, payload, ts) VALUES ('s-1', 'request_permission', ?1, 2)", [req.to_string()])
                .unwrap();
            let res = json!({"requestId":"r-1","outcome":{"kind":"cancelled"},"by":"user"});
            c.execute("INSERT INTO events (session_id, type, payload, ts) VALUES ('s-1', 'permission_resolved', ?1, 3)", [res.to_string()])
                .unwrap();
            c.execute("INSERT INTO events (session_id, type, payload, ts) VALUES ('s-1', 'stop', '{\"stopReason\":\"end_turn\"}', 4)", [])
                .unwrap();
            let pet = json!({"id":"pet-legacy","agentId":"codex","species":"dog","skin":"default","name":"Biscuit","personality":"eager","mood":70,"xp":3});
            c.execute("INSERT INTO pets (id, agent_id, data) VALUES ('pet-legacy', 'codex', ?1)", [pet.to_string()]).unwrap();
            c.execute("INSERT INTO chat (id, pet_id, role, text, ts, session_id) VALUES ('c-1', 'pet-legacy', 'user', 'hi', 1, 's-1')", [])
                .unwrap();
            c.execute("INSERT INTO chat (id, pet_id, role, text, ts, session_id) VALUES ('c-2', NULL, 'pet', 'woof', 2, NULL)", []).unwrap();
            c.execute(
                "INSERT INTO audit_log (ts, request_id, session_id, agent_id, decided_by, actor, outcome)
                 VALUES (3, 'r-1', 's-1', 'codex', 'user', 'desktop#1', 'cancelled')",
                [],
            )
            .unwrap();
            c.execute("INSERT INTO settings (key, value) VALUES ('rules_set', '1')", []).unwrap();
        }

        let s = Store::open(&p, MID).unwrap();
        let agents = s.agents().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!((agents[0].id.as_str(), agents[0].machine_id.as_str()), ("codex", MID));

        let sess = s.session(&gid("s-1")).unwrap().expect("session renamed to its global id");
        assert_eq!(sess.machine_id, MID);
        assert!(s.session("s-1").unwrap().is_none());
        assert!(s.session_exists(&gid("s-1")).unwrap());

        let events = s.events(&gid("s-1"), None, 10).unwrap();
        assert_eq!(events.len(), 3);
        for e in &events {
            assert_eq!(e.id, global_id(MID, e.seq));
            assert_eq!(e.machine_id, MID);
        }
        let req: pawl_protocol::PermissionRequest = serde_json::from_value(events[0].payload.clone()).unwrap();
        assert_eq!((req.request_id.as_str(), req.session_id.as_str(), req.machine_id.as_str()), (gid("r-1").as_str(), gid("s-1").as_str(), MID));
        assert_eq!(events[1].payload["requestId"], gid("r-1").as_str());
        assert_eq!(events[2].payload["stopReason"], "end_turn");
        // seq continues after the migrated rows
        let next = s.insert_event(&gid("s-1"), EventType::Stop, &json!({}), 9).unwrap();
        assert_eq!(next.seq, 4);

        let pets = s.pets().unwrap();
        assert_eq!(pets.len(), 1);
        assert_eq!(pets[0].id, gid("pet-codex"));
        assert_eq!((pets[0].machine_id.as_str(), pets[0].name.as_str(), pets[0].xp), (MID, "Biscuit", 3));

        let chat = s.recent_chat(10).unwrap();
        assert_eq!(chat[0].id, gid("c-1"));
        assert_eq!(chat[0].pet_id.as_deref(), Some(gid("pet-codex").as_str()));
        assert_eq!(chat[0].session_id.as_deref(), Some(gid("s-1").as_str()));
        assert_eq!((chat[1].id.as_str(), chat[1].pet_id.as_deref(), chat[1].machine_id.as_str()), (gid("c-2").as_str(), None, MID));

        let audit = s.audit_log(10).unwrap();
        assert_eq!((audit[0].request_id.as_str(), audit[0].session_id.as_str()), (gid("r-1").as_str(), gid("s-1").as_str()));
        assert_eq!(audit[0].machine_id, MID);
        assert_eq!(s.rules().unwrap(), Some(vec![]), "settings survive");
        assert_eq!(s.setting(SETTING_MACHINE_ID).unwrap().as_deref(), Some(MID));
    }

    #[test]
    fn globalize_keeps_global_ids() {
        assert_eq!(globalize(MID, "x"), "abcdef012345:x");
        assert_eq!(globalize(MID, "zzzzzz999999:x"), "zzzzzz999999:x");
        assert_eq!(globalize(MID, "Bad:x"), "abcdef012345:Bad:x");
        let c = Connection::open_in_memory().unwrap();
        let g: bool = c
            .query_row("SELECT 'abcdef012345:s' GLOB ?1 AND NOT 'abcdef01234:s' GLOB ?1 AND NOT 'abcdef012345:' GLOB ?1", [global_id_glob()], |r| r.get(0))
            .unwrap();
        assert!(g);
    }

    #[test]
    fn events_roundtrip_and_paging() {
        let s = Store::open_in_memory(MID).unwrap();
        s.upsert_session(&session("s1", 10)).unwrap();
        let s1 = gid("s1");
        let mut seqs = vec![];
        for i in 0..5 {
            let e = s.insert_event(&s1, EventType::AgentMessageChunk, &json!({"text": format!("t{i}")}), 100 + i).unwrap();
            assert_eq!(e.id, global_id(MID, e.seq));
            assert_eq!(e.machine_id, MID);
            seqs.push(e.seq);
        }
        s.insert_event(&gid("s2"), EventType::Stop, &json!({"stopReason":"end_turn"}), 200).unwrap();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]));
        let last2 = s.events(&s1, None, 2).unwrap();
        assert_eq!(last2.iter().map(|e| e.seq).collect::<Vec<_>>(), seqs[3..].to_vec());
        let after = s.events(&s1, Some(seqs[1]), 10).unwrap();
        assert_eq!(after.len(), 3);
        assert_eq!(after[0].payload["text"], "t2");
        assert_eq!(after[0].kind, EventType::AgentMessageChunk);
        assert_eq!(after[0].id, global_id(MID, seqs[2]));
        // insert_event bumped lastEventAt
        assert_eq!(s.session(&s1).unwrap().unwrap().last_event_at, 104);
        let recent = s.recent_events(10, true).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].kind, EventType::Stop);
        assert_eq!(s.events_since(0, 10).unwrap().len(), 1);
        assert_eq!(s.prune_events_before(102).unwrap(), 2);
    }

    #[test]
    fn sessions_agents_pets_rules_chat() {
        let s = Store::open_in_memory(MID).unwrap();
        let mut a = session("a", 50);
        s.upsert_session(&a).unwrap();
        a.status = SessionStatus::Done;
        a.ended_at = Some(60);
        a.last_event_at = 40; // never moves backwards
        s.upsert_session(&a).unwrap();
        let got = s.session(&gid("a")).unwrap().unwrap();
        assert_eq!(got.status, SessionStatus::Done);
        assert_eq!(got.last_event_at, 50);
        assert_eq!(got.ended_at, Some(60));
        assert_eq!(got.machine_id, MID);
        s.upsert_session(&session("b", 5)).unwrap();
        let active = s.sessions_active_since(45).unwrap();
        assert_eq!(active.iter().map(|x| x.id.clone()).collect::<Vec<_>>(), vec![gid("a"), gid("b")]);
        assert_eq!(s.recent_sessions(1).unwrap()[0].id, gid("a"));
        assert!(!s.session_exists(&gid("nope")).unwrap());

        let agent = Agent {
            id: "codex".into(),
            machine_id: MID.into(),
            name: "Codex".into(),
            command: vec!["codex-acp".into()],
            env: None,
            version: None,
            description: None,
            icon: None,
            installed: true,
            source: AgentSource::Registry,
            capabilities: None,
        };
        s.replace_agents(std::slice::from_ref(&agent)).unwrap();
        assert_eq!(s.agents().unwrap(), vec![agent.clone()]);
        s.upsert_agent(&agent).unwrap();
        assert_eq!(s.agents().unwrap(), vec![agent]);

        let pet = Pet {
            id: gid("pet-codex"),
            machine_id: MID.into(),
            agent_id: "codex".into(),
            species: Species::Dog,
            skin: "default".into(),
            name: "Biscuit".into(),
            personality: "eager".into(),
            mood: 70,
            xp: 0,
        };
        s.upsert_pet(&pet).unwrap();
        assert_eq!(s.pets().unwrap(), vec![pet]);

        assert_eq!(s.rules().unwrap(), None);
        s.set_rules(&[]).unwrap();
        assert_eq!(s.rules().unwrap(), Some(vec![]));
        let r = Rule { id: "r1".into(), matcher: RuleMatch { tool_kind: Some(ToolKind::Read), ..Default::default() }, action: RuleAction::AutoAllow };
        s.set_rules(std::slice::from_ref(&r)).unwrap();
        assert_eq!(s.rules().unwrap(), Some(vec![r]));

        let m = ChatMessage {
            id: gid("c1"),
            machine_id: MID.into(),
            pet_id: Some(gid("pet-codex")),
            role: ChatRole::Pet,
            text: "woof".into(),
            ts: 3,
            session_id: None,
            proposals: vec![pawl_protocol::ChatProposal {
                id: "p1".into(),
                kind: pawl_protocol::ChatProposalKind::Start,
                machine_id: "zyxwvu987654".into(),
                agent_id: "codex".into(),
                cwd: Some("/srv/api".into()),
                repo_name: Some("api".into()),
                session_id: None,
                text: "run the tests".into(),
                summary: "Start Codex on Mac mini in api: run the tests".into(),
            }],
        };
        s.insert_chat(&m).unwrap();
        let plain = ChatMessage { id: gid("c2"), proposals: Vec::new(), ..m.clone() };
        s.insert_chat(&plain).unwrap();
        assert_eq!(s.recent_chat(5).unwrap(), vec![m, plain], "proposals are kept with their message");

        let _ = PermissionOutcome::Cancelled;
        s.set_setting("k", "v").unwrap();
        assert_eq!(s.setting("k").unwrap().as_deref(), Some("v"));
    }

    #[test]
    fn audit_roundtrip() {
        let s = Store::open_in_memory(MID).unwrap();
        let a = AuditEntry {
            id: 0,
            ts: 9,
            machine_id: MID.into(),
            request_id: gid("r"),
            session_id: gid("s"),
            agent_id: "claude-code".into(),
            tool_call_id: Some("t".into()),
            tool_kind: Some(ToolKind::Execute),
            tool_title: Some("rm -rf build".into()),
            locations: vec!["/repo/build".into()],
            decided_by: ResolvedBy::User,
            actor: "desktop#1".into(),
            option_id: Some("reject".into()),
            option_kind: Some(PermissionOptionKind::RejectOnce),
            outcome: AuditOutcome::Rejected,
            detail: None,
            origin: Some(AuditOrigin { machine_id: "zyxwvu987654".into(), name: Some("Mac mini".into()), intent_id: "i-1".into(), client: "desktop".into() }),
            via: Some("voice".into()),
            transcript: Some("approve 4 7".into()),
        };
        let id = s.insert_audit(&a).unwrap();
        let got = s.audit_log(10).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], AuditEntry { id, ..a });
    }

    fn roster_entry(id: &str, name: &str, pk: &str, addr: Option<&str>) -> RosterEntry {
        RosterEntry {
            device: Device {
                id: id.into(),
                kind: DeviceKind::Hub,
                name: name.into(),
                public_key: pk.into(),
                paired_at: 100,
                last_seen: Some(100),
                addr: addr.map(Into::into),
                path: None,
                introduced_by: None,
                os: Some(MachineOs::Macos),
            },
            machine: Machine {
                id: id.into(),
                name: name.into(),
                os: MachineOs::Macos,
                last_seen: 100,
                online: false,
                hub_version: Some("0.1.0".into()),
                user_active_at: None,
            },
        }
    }

    #[test]
    fn revoked_devices_keep_their_key_for_a_bye() {
        const PEER: &str = "zyxwvu987654";
        let s = Store::open_in_memory(MID).unwrap();
        let r: &dyn Roster = &s;
        assert_eq!(r.get_revoked(PEER), None, "unknown");
        let e = roster_entry(PEER, "Mac mini", "pk-1", Some("10.0.0.2:47801"));
        r.upsert(e.clone());
        assert_eq!(r.get_revoked(PEER), None, "still paired");
        r.revoke(PEER);
        assert_eq!(r.get(PEER), None);
        let gone = r.get_revoked(PEER).expect("revoked rows stay readable");
        assert_eq!((gone.device.id.as_str(), gone.device.public_key.as_str()), (PEER, "pk-1"));
        assert!(r.list().is_empty());
        // Pairing again makes it a normal entry once more.
        r.upsert(e.clone());
        assert_eq!(r.get_revoked(PEER), None);
        assert_eq!(r.get(PEER).map(|x| x.device.public_key), Some("pk-1".to_string()));
    }

    #[test]
    fn roster_follows_the_peer_contract() {
        const PEER: &str = "zyxwvu987654";
        let s = Store::open_in_memory(MID).unwrap();
        let r: &dyn Roster = &s;
        assert!(r.list().is_empty());
        assert_eq!((r.last_rx_seq(PEER), r.last_tx_seq(PEER)), (0, 0), "unknown ids read 0");
        r.set_last_rx_seq(PEER, 9); // no row yet: a no-op
        assert_eq!(r.last_rx_seq(PEER), 0);

        let e = roster_entry(PEER, "Mac mini", "pk-1", Some("10.0.0.2:47801"));
        r.upsert(e.clone());
        assert_eq!(r.get(PEER), Some(e.clone()));
        assert_eq!(r.list(), vec![e.clone()]);

        // Counters are stored as given, including values above i64::MAX and going down.
        r.set_last_rx_seq(PEER, u64::MAX - 1);
        r.set_last_tx_seq(PEER, 1_700_000_000_000_000);
        assert_eq!((r.last_rx_seq(PEER), r.last_tx_seq(PEER)), (u64::MAX - 1, 1_700_000_000_000_000));
        r.set_last_rx_seq(PEER, 5);
        assert_eq!(r.last_rx_seq(PEER), 5);

        // update_seen: name/os/version/lastSeen from the frame; addr only when given.
        let mut m = e.machine.clone();
        m.name = "Studio".into();
        m.os = MachineOs::Linux;
        m.hub_version = Some("0.2.0".into());
        r.update_seen(PEER, &m, None, 500);
        let got = r.get(PEER).unwrap();
        assert_eq!(got.device.name, "Studio");
        assert_eq!(got.device.addr.as_deref(), Some("10.0.0.2:47801"), "addr None keeps the stored one");
        assert_eq!(got.device.last_seen, Some(500));
        assert_eq!((got.machine.name.as_str(), got.machine.os, got.machine.last_seen), ("Studio", MachineOs::Linux, 500));
        assert_eq!(got.machine.hub_version.as_deref(), Some("0.2.0"));
        assert!(!got.machine.online, "the roster never says online");
        r.update_seen(PEER, &m, Some("10.0.0.3:47801"), 600);
        assert_eq!(r.get(PEER).unwrap().device.addr.as_deref(), Some("10.0.0.3:47801"));
        r.update_seen("aaaaaa000000", &m, None, 700); // unknown id: no-op
        assert_eq!(r.list().len(), 1);

        // Revoked devices disappear from get/list and are not updated; re-pairing restores them.
        r.revoke(PEER);
        assert_eq!(r.get(PEER), None);
        assert!(r.list().is_empty());
        assert!(!s.revoke_device(PEER, 1).unwrap(), "already revoked");
        r.update_seen(PEER, &m, None, 900);
        let again = roster_entry(PEER, "Mac mini", "pk-2", None);
        r.upsert(again.clone());
        assert_eq!(r.get(PEER), Some(again), "re-pair replaces key, name and addr and clears revoked");
        assert_eq!(r.last_rx_seq(PEER), 0, "a new key restarts the receive counter");
        assert_eq!(r.last_tx_seq(PEER), 1_700_000_000_000_000, "the send counter never goes back on its own");
    }

    #[test]
    fn pairing_audit_roundtrip() {
        let s = Store::open_in_memory(MID).unwrap();
        let a = PairingAuditEntry {
            id: 0,
            ts: 42,
            machine_id: MID.into(),
            kind: PairingAuditKind::RevokedByPeer,
            device_id: Some("zyxwvu987654".into()),
            device_name: Some("Mac mini".into()),
            remote_addr: Some("10.0.0.2:47801".into()),
            actor: "peer".into(),
            detail: None,
        };
        let id = s.insert_pairing_audit(&a).unwrap();
        let rejected = PairingAuditEntry {
            kind: PairingAuditKind::PairRejected,
            device_id: None,
            device_name: None,
            detail: Some("invalid_invite".into()),
            ..a.clone()
        };
        let id2 = s.insert_pairing_audit(&rejected).unwrap();
        assert_eq!(s.pairing_audit(10).unwrap(), vec![PairingAuditEntry { id: id2, ..rejected }, PairingAuditEntry { id, ..a }]);
        assert_eq!(enum_str(&PairingAuditKind::RevokedByPeer), "revoked_by_peer");
    }

    /// A v2 hub.db (step 1) gains the roster and pairing audit tables and keeps its data.
    #[test]
    fn v3_migration_adds_the_roster() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hub.db");
        {
            let mut c = Connection::open(&p).unwrap();
            for (i, step) in MIGRATIONS.iter().take(2).enumerate() {
                let tx = c.transaction().unwrap();
                match step {
                    Migration::Sql(sql) => tx.execute_batch(sql).unwrap(),
                    Migration::Code(f) => f(&tx, MID).unwrap(),
                }
                tx.pragma_update(None, "user_version", (i + 1) as i64).unwrap();
                tx.commit().unwrap();
            }
            c.execute("INSERT INTO settings (key, value) VALUES ('kept', 'yes')", []).unwrap();
        }
        let s = Store::open(&p, MID).unwrap();
        let v: i64 = s.conn().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        assert_eq!(s.setting("kept").unwrap().as_deref(), Some("yes"));
        assert!(s.roster().unwrap().is_empty());
        s.upsert_device(&roster_entry("zyxwvu987654", "Mac mini", "pk", None)).unwrap();
        assert_eq!(s.roster().unwrap().len(), 1);
        assert!(s.pairing_audit(5).unwrap().is_empty());
    }

    /// A v7 hub.db (step 5b) migrates to v8: its paired hubs keep everything and have no introducer.
    #[test]
    fn v8_migration_keeps_the_roster() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hub.db");
        {
            let mut c = Connection::open(&p).unwrap();
            for (i, step) in MIGRATIONS.iter().take(7).enumerate() {
                let tx = c.transaction().unwrap();
                match step {
                    Migration::Sql(sql) => tx.execute_batch(sql).unwrap(),
                    Migration::Code(f) => f(&tx, MID).unwrap(),
                }
                tx.pragma_update(None, "user_version", (i + 1) as i64).unwrap();
                tx.commit().unwrap();
            }
            c.execute(
                "INSERT INTO devices (id, kind, name, os, hub_version, public_key, addr, paired_at, last_seen, last_tx_seq)
                 VALUES ('zyxwvu987654', 'hub', 'Mac mini', 'macos', '0.1.0', 'pk-1', '10.0.0.2:47801', 100, 100, 42)",
                [],
            )
            .unwrap();
        }
        let s = Store::open(&p, MID).unwrap();
        let v: i64 = s.conn().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
        let e = s.roster_entry("zyxwvu987654").unwrap().unwrap();
        assert_eq!((e.device.introduced_by, e.device.os), (None, Some(MachineOs::Macos)));
        assert_eq!(e.device.addr.as_deref(), Some("10.0.0.2:47801"));
        assert_eq!((&s as &dyn Roster).last_tx_seq("zyxwvu987654"), 42);
        assert!(s.pending_roster_intros("zyxwvu987654").unwrap().is_empty());
    }

    fn phone_entry(id: &str, name: &str, pk: &str, os: MachineOs) -> RosterEntry {
        let mut e = roster_entry(id, name, pk, None);
        e.device.kind = DeviceKind::Phone;
        e.device.os = Some(os);
        e.machine.os = os;
        e
    }

    #[test]
    fn introduced_devices_never_replace_anything() {
        const INTRODUCER: &str = "zyxwvu987654";
        let s = Store::open_in_memory(MID).unwrap();
        let phone = phone_entry("pppppp000001", "iPhone", "pk-phone", MachineOs::Ios);
        s.upsert_device(&phone).unwrap();
        assert_eq!(s.roster_entry("pppppp000001").unwrap(), Some(phone.clone()), "kind and os round-trip");

        let mut pixel = phone_entry("pppppp000002", "Pixel", "pk-pixel", MachineOs::Android);
        pixel.device.introduced_by = Some(INTRODUCER.into());
        // An intro may only know the device's os: it is what gets stored.
        pixel.machine.os = MachineOs::Other;
        assert!(s.add_introduced_device(&pixel).unwrap());
        let got = s.roster_entry("pppppp000002").unwrap().unwrap();
        assert_eq!((got.device.introduced_by.as_deref(), got.device.os), (Some(INTRODUCER), Some(MachineOs::Android)));
        assert_eq!((&s as &dyn Roster).last_rx_seq("pppppp000002"), 0);

        // Same id with another key, a known key under another id, a revoked id or key: never added.
        let other_key = phone_entry("pppppp000002", "Pixel", "pk-other", MachineOs::Android);
        assert!(!s.add_introduced_device(&other_key).unwrap());
        assert_eq!(s.roster_entry("pppppp000002").unwrap().unwrap().device.public_key, "pk-pixel");
        assert!(!s.add_introduced_device(&phone_entry("tttttt000001", "Thief", "pk-phone", MachineOs::Ios)).unwrap());
        s.revoke_device("pppppp000001", 5).unwrap();
        assert!(!s.add_introduced_device(&phone).unwrap(), "a revoked id stays revoked");
        assert!(!s.add_introduced_device(&phone_entry("nnnnnn000001", "New", "pk-phone", MachineOs::Ios)).unwrap(), "and its key");
        let all: Vec<(String, bool)> = s.roster_with_revoked().unwrap().into_iter().map(|(e, r)| (e.device.id, r)).collect();
        assert_eq!(all, [("pppppp000001".to_string(), true), ("pppppp000002".to_string(), false)]);

        // Pairing directly with an introduced device makes it a direct pairing.
        let mut direct = pixel.clone();
        direct.device.introduced_by = None;
        s.upsert_device(&direct).unwrap();
        assert_eq!(s.roster_entry("pppppp000002").unwrap().unwrap().device.introduced_by, None);
    }

    /// Only a pairing (`upsert`) sets a device's kind: hellos and presence (`update_seen`) never do,
    /// whatever os they claim.
    #[test]
    fn hellos_never_change_a_devices_kind() {
        let s = Store::open_in_memory(MID).unwrap();
        let r: &dyn Roster = &s;
        let phone = phone_entry("pppppp000001", "iPhone", "pk-phone", MachineOs::Ios);
        r.upsert(phone.clone());
        let mut m = phone.machine.clone();
        m.os = MachineOs::Macos;
        r.update_seen("pppppp000001", &m, Some("192.168.1.9:47801"), 500);
        let got = r.get("pppppp000001").unwrap();
        assert_eq!(got.device.kind, DeviceKind::Phone);
        let hub = roster_entry("zyxwvu987654", "Mac mini", "pk-hub", None);
        r.upsert(hub.clone());
        let mut m = hub.machine.clone();
        m.os = MachineOs::Ios;
        r.update_seen("zyxwvu987654", &m, None, 500);
        assert_eq!(r.get("zyxwvu987654").unwrap().device.kind, DeviceKind::Hub);
    }

    #[test]
    fn owed_intros_are_kept_per_recipient_until_sent_or_revoked() {
        let s = Store::open_in_memory(MID).unwrap();
        let ids = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        s.queue_roster_intros("hhhhhh000001", &ids(&["pppppp000001", "hhhhhh000002", "hhhhhh000001"]), 10).unwrap();
        s.queue_roster_intros("hhhhhh000001", &ids(&["pppppp000001"]), 20).unwrap();
        s.queue_roster_intros("pppppp000001", &ids(&["hhhhhh000001"]), 10).unwrap();
        assert_eq!(s.pending_roster_intros("hhhhhh000001").unwrap(), ["hhhhhh000002", "pppppp000001"], "no repeats, never itself");
        s.clear_roster_intros("hhhhhh000001", &ids(&["hhhhhh000002"])).unwrap();
        assert_eq!(s.pending_roster_intros("hhhhhh000001").unwrap(), ["pppppp000001"]);
        // Revoking a device drops what it is owed and what others are owed about it.
        s.upsert_device(&phone_entry("pppppp000001", "iPhone", "pk", MachineOs::Ios)).unwrap();
        s.revoke_device("pppppp000001", 30).unwrap();
        assert!(s.pending_roster_intros("hhhhhh000001").unwrap().is_empty());
        assert!(s.pending_roster_intros("pppppp000001").unwrap().is_empty());
    }

    const PEER: &str = "zyxwvu987654";

    fn peer_session(local: &str, last: UnixMs) -> Session {
        Session { id: global_id(PEER, local), machine_id: PEER.into(), ..session(local, last) }
    }

    fn peer_event(seq: i64, session_id: &str) -> HubEvent {
        HubEvent {
            id: global_id(PEER, seq),
            seq,
            machine_id: PEER.into(),
            session_id: session_id.into(),
            kind: EventType::ToolCall,
            payload: json!({ "n": seq }),
            ts: 100 + seq,
        }
    }

    #[test]
    fn mirror_rows_keep_their_owner_and_never_touch_ours() {
        let s = Store::open_in_memory(MID).unwrap();
        s.upsert_session(&session("mine", 10)).unwrap();
        let mine = s.insert_event(&gid("mine"), EventType::ToolCall, &json!({}), 10).unwrap();
        assert_eq!((mine.id.clone(), mine.seq), (gid("1"), 1), "local seq = rowid");

        // This machine can never be written through the mirror API.
        assert!(s.mirror_upsert_session(&session("x", 1)).is_err());
        assert!(s.mirror_insert_events(MID, std::slice::from_ref(&mine), false).is_err());
        assert!(s.delete_mirror(MID).is_err());
        let mut own_pet = Pet {
            id: gid("pet-codex"),
            machine_id: MID.into(),
            agent_id: "codex".into(),
            species: Species::Dog,
            skin: "default".into(),
            name: "Mine".into(),
            personality: String::new(),
            mood: 50,
            xp: 0,
        };
        s.upsert_pet(&own_pet).unwrap();
        assert!(s.mirror_upsert_pet(&own_pet).is_err());
        own_pet.machine_id = PEER.into(); // our id under the peer's machine
        assert!(s.mirror_upsert_pet(&own_pet).is_err());
        assert_eq!(s.pets().unwrap().into_iter().find(|p| p.id == gid("pet-codex")).unwrap().machine_id, MID);
        let mut spoof = peer_session("x", 1);
        spoof.id = gid("mine");
        assert!(s.mirror_upsert_session(&spoof).is_err(), "our session id under the peer's machine");
        assert_eq!(s.session(&gid("mine")).unwrap().unwrap().machine_id, MID);

        // Mirrored events keep the owner's seq, even where it collides with a local seq.
        let ps = peer_session("s", 5);
        s.mirror_upsert_session(&ps).unwrap();
        let pe = peer_event(1, &ps.id);
        let new = s.mirror_insert_events(PEER, &[pe.clone(), pe.clone()], false).unwrap();
        assert_eq!(new, vec![pe.clone()], "idempotent by (machine, seq)");
        assert!(s.mirror_insert_events(PEER, std::slice::from_ref(&pe), false).unwrap().is_empty());
        assert_eq!(s.events(&ps.id, None, 10).unwrap(), vec![pe.clone()]);
        assert_eq!(s.events(&gid("mine"), None, 10).unwrap(), vec![mine.clone()]);
        // Crafted events the hub should have dropped are skipped by the store as well.
        let mut into_ours = peer_event(9, &gid("mine"));
        into_ours.session_id = gid("mine");
        let mut bad_id = peer_event(10, &ps.id);
        bad_id.id = global_id(PEER, 11);
        let orphan = peer_event(12, &global_id(PEER, "unknown"));
        assert!(s.mirror_insert_events(PEER, &[into_ours, bad_id, orphan], true).unwrap().is_empty());
        assert_eq!(s.events(&gid("mine"), None, 10).unwrap(), vec![mine.clone()]);

        // The owner side of a sync only ever reads this machine's rows.
        let (local, truncated) = s.local_events_after(0, 10).unwrap();
        assert_eq!((local, truncated), (vec![mine.clone()], false));
        assert_eq!(s.sync_sessions(0, 10).unwrap().iter().map(|x| x.id.clone()).collect::<Vec<_>>(), vec![gid("mine")]);
        assert_eq!(s.sessions_active_since(0).unwrap().len(), 1, "the local cache never loads mirrored sessions");
        assert_eq!(s.mirror_sessions(0).unwrap().iter().map(|x| x.id.clone()).collect::<Vec<_>>(), vec![ps.id.clone()]);
        assert!(s.is_mirrored_session(&ps.id).unwrap() && !s.is_mirrored_session(&gid("mine")).unwrap());

        // Pending requests, then the sync cursor, then revoke.
        let req = PermissionRequest {
            request_id: global_id(PEER, "r1"),
            machine_id: PEER.into(),
            session_id: ps.id.clone(),
            agent_id: "codex".into(),
            tool_call: Default::default(),
            options: vec![],
            created_at: 7,
            voice_code: None,
        };
        s.mirror_put_pending(&req).unwrap();
        assert_eq!(s.mirror_pending().unwrap(), vec![req.clone()]);
        assert!(s.mirror_remove_pending(PEER, &req.request_id).unwrap());
        s.mirror_put_pending(&req).unwrap();
        s.upsert_device(&roster_entry(PEER, "Mac mini", "pk", None)).unwrap();
        assert_eq!(s.mirror_cursor(PEER).unwrap(), 0);
        assert_eq!(s.finish_mirror_sync(PEER).unwrap(), 1, "everything stored so far counts");
        s.mirror_insert_events(PEER, &[peer_event(5, &ps.id)], true).unwrap();
        assert_eq!(s.mirror_cursor(PEER).unwrap(), 5);
        s.delete_mirror(PEER).unwrap();
        assert!(s.mirror_sessions(0).unwrap().is_empty() && s.mirror_pending().unwrap().is_empty());
        assert_eq!(s.mirror_cursor(PEER).unwrap(), 0);
        assert!(s.events(&ps.id, None, 10).unwrap().is_empty());
        assert_eq!(s.events(&gid("mine"), None, 10).unwrap(), vec![mine]);
        assert_eq!(s.pets().unwrap().len(), 1);
    }

    #[test]
    fn mirror_snapshot_replaces_that_machines_set() {
        let s = Store::open_in_memory(MID).unwrap();
        s.upsert_session(&session("mine", 10)).unwrap();
        let (a, b) = (peer_session("a", 5), peer_session("b", 6));
        s.mirror_upsert_session(&a).unwrap();
        s.mirror_upsert_session(&b).unwrap();
        s.mirror_insert_events(PEER, &[peer_event(1, &a.id), peer_event(2, &b.id)], false).unwrap();
        s.mirror_replace(PEER, &[b.clone(), session("evil", 1)], &[], &[]).unwrap();
        assert_eq!(s.mirror_sessions(0).unwrap().iter().map(|x| x.id.clone()).collect::<Vec<_>>(), vec![b.id.clone()]);
        assert!(s.events(&a.id, None, 10).unwrap().is_empty(), "a session left out takes its events with it");
        assert_eq!(s.events(&b.id, None, 10).unwrap().len(), 1);
        assert!(s.session(&gid("evil")).unwrap().is_none(), "items of another machine in a snapshot are skipped");
        assert!(s.session(&gid("mine")).unwrap().is_some());
        assert!(s.mirror_replace(MID, &[], &[], &[]).is_err());
    }

    #[test]
    fn mirror_storage_is_capped_and_pruned() {
        let s = Store::open_in_memory(MID).unwrap();
        s.upsert_session(&session("mine", 1)).unwrap();
        for i in 0..(MIRROR_MAX_SESSIONS + 5) {
            s.mirror_upsert_session(&peer_session(&format!("s{i:04}"), 1000 + i)).unwrap();
        }
        let kept = s.mirror_sessions(0).unwrap();
        assert_eq!(kept.len() as i64, MIRROR_MAX_SESSIONS);
        assert!(kept.iter().all(|x| x.last_event_at >= 1005), "the oldest were evicted");
        assert!(s.session(&gid("mine")).unwrap().is_some());

        let sid = global_id(PEER, format!("s{:04}", MIRROR_MAX_SESSIONS + 4));
        let events: Vec<HubEvent> = (1..=MIRROR_MAX_EVENTS_PER_SESSION + 5).map(|q| peer_event(q, &sid)).collect();
        s.mirror_insert_events(PEER, &events, false).unwrap();
        let stored = s.events(&sid, None, 5000).unwrap();
        assert_eq!(stored.len() as i64, MIRROR_MAX_EVENTS_PER_SESSION);
        assert_eq!(stored[0].seq, 6, "oldest events evicted first");

        for i in 0..MIRROR_MAX_PETS {
            let mut p = Pet {
                id: global_id(PEER, format!("pet-a{i}")),
                machine_id: PEER.into(),
                agent_id: format!("a{i}"),
                species: Species::Dog,
                skin: "default".into(),
                name: format!("P{i}"),
                personality: String::new(),
                mood: 50,
                xp: 0,
            };
            s.mirror_upsert_pet(&p).unwrap();
            if i == 0 {
                p.name = "Renamed".into();
                s.mirror_upsert_pet(&p).unwrap(); // an update is never refused
            }
        }
        let extra = Pet { id: global_id(PEER, "pet-z"), agent_id: "z".into(), ..s.pets().unwrap().pop().unwrap() };
        assert!(s.mirror_upsert_pet(&extra).is_err(), "pets beyond the cap are refused");

        // Ended more than 7 days ago: pruned with its events. Recent or open: kept.
        let now = 100 * 86_400_000;
        let mut old = peer_session("old", now - 9 * 86_400_000);
        old.ended_at = Some(now - 8 * 86_400_000);
        let mut recent = peer_session("recent", now - 86_400_000);
        recent.ended_at = Some(now - 86_400_000);
        s.mirror_replace(PEER, &[old.clone(), recent.clone()], &[], &[]).unwrap();
        s.mirror_insert_events(PEER, &[peer_event(1, &old.id)], false).unwrap();
        assert_eq!(s.prune_mirror(now).unwrap(), 1);
        assert_eq!(s.mirror_sessions(0).unwrap().iter().map(|x| x.id.clone()).collect::<Vec<_>>(), vec![recent.id]);
        assert!(s.events(&old.id, None, 10).unwrap().is_empty());

        // Rows of machines that are no longer paired go; this machine's stay.
        assert!(s.delete_unpaired_mirrors(&[]).unwrap() > 0);
        assert!(s.mirror_sessions(0).unwrap().is_empty());
        assert!(s.pets().unwrap().is_empty());
        assert!(s.session(&gid("mine")).unwrap().is_some());
    }

    #[test]
    fn sync_reads_the_newest_local_events() {
        let s = Store::open_in_memory(MID).unwrap();
        s.upsert_session(&session("mine", 1)).unwrap();
        for i in 0..5 {
            s.insert_event(&gid("mine"), EventType::ToolCall, &json!({ "i": i }), i).unwrap();
        }
        let (v, truncated) = s.local_events_after(0, 3).unwrap();
        assert_eq!(v.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4, 5]);
        assert!(truncated);
        let (v, truncated) = s.local_events_after(2, 10).unwrap();
        assert_eq!(v.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4, 5]);
        assert!(!truncated);
        // Pruning never lowers the seq reported in sync_done (a peer would think we were reset).
        assert_eq!(s.local_max_seq().unwrap(), 5);
        s.prune_events_before(i64::MAX).unwrap();
        assert_eq!(s.local_max_seq().unwrap(), 5);
    }

    /// Review fix 3: only events that are stored (inserted now or already there) move the cursor.
    #[test]
    fn skipped_events_never_move_the_cursor() {
        let s = Store::open_in_memory(MID).unwrap();
        s.upsert_device(&roster_entry(PEER, "Mac mini", "pk", None)).unwrap();
        let ps = peer_session("s", 5);
        s.mirror_upsert_session(&ps).unwrap();
        let orphan = |seq| peer_event(seq, &global_id(PEER, "not-mirrored"));
        s.mirror_insert_events(PEER, &[orphan(100), peer_event(5, &ps.id)], true).unwrap();
        assert_eq!(s.mirror_cursor(PEER).unwrap(), 5);
        s.mirror_insert_events(PEER, &[orphan(200)], true).unwrap();
        let mut bad = peer_event(300, &ps.id);
        bad.id = global_id(PEER, 301);
        s.mirror_insert_events(PEER, &[bad], true).unwrap();
        assert_eq!(s.mirror_cursor(PEER).unwrap(), 5, "skipped events never move it");
        assert!(s.mirror_insert_events(PEER, &[peer_event(5, &ps.id)], true).unwrap().is_empty());
        assert_eq!(s.mirror_cursor(PEER).unwrap(), 5, "an event already stored counts as stored");
        assert_eq!(s.finish_mirror_sync(PEER).unwrap(), 5);
        s.reset_mirror_events(PEER).unwrap();
        assert_eq!(s.mirror_cursor(PEER).unwrap(), 0);
        assert!(s.events(&ps.id, None, 10).unwrap().is_empty());
        assert!(s.is_mirrored_session(&ps.id).unwrap(), "a reset keeps the sessions");
    }

    /// Review fix 4: a peer's mirrored payload bytes stay within the per-peer budget, oldest first out.
    #[test]
    fn mirrored_payload_bytes_stay_within_budget() {
        let s = Store::open_in_memory(MID).unwrap();
        s.set_mirror_byte_budget(10_000);
        s.upsert_device(&roster_entry(PEER, "Mac mini", "pk", None)).unwrap();
        let ps = peer_session("s", 5);
        s.mirror_upsert_session(&ps).unwrap();
        // A local event of the same size is never counted or evicted.
        s.upsert_session(&session("mine", 1)).unwrap();
        s.insert_event(&gid("mine"), EventType::ToolCall, &json!({ "t": "y".repeat(990) }), 1).unwrap();
        for seq in 1..=40 {
            let mut e = peer_event(seq, &ps.id);
            e.payload = json!({ "t": "x".repeat(990) });
            s.mirror_insert_events(PEER, &[e], false).unwrap();
        }
        let total: i64 = s
            .conn()
            .query_row("SELECT SUM(LENGTH(payload)) FROM events WHERE machine_id = ?1", [PEER], |r| r.get(0))
            .unwrap();
        assert!(total <= 10_000, "{total}");
        let kept = s.events(&ps.id, None, 100).unwrap();
        assert!(kept.first().unwrap().seq > 1, "the oldest were evicted");
        assert_eq!(kept.last().unwrap().seq, 40, "the newest stay");
        assert_eq!(s.events(&gid("mine"), None, 10).unwrap().len(), 1);
    }

    /// Review fix M4: the intent log keeps 30 days and at most `max_rows` rows.
    #[test]
    fn the_intent_log_is_pruned() {
        let s = Store::open_in_memory(MID).unwrap();
        let entry = |ts: UnixMs, n: usize| IntentLogEntry {
            id: 0,
            ts,
            role: IntentRole::Owner,
            intent_id: format!("i{n}"),
            kind: "deny".into(),
            peer_machine_id: "zyxwvu987654".into(),
            peer_name: Some("Mac mini".into()),
            client: "desktop".into(),
            target: None,
            option_id: None,
            status: "rejected".into(),
            reason: Some("not_found".into()),
            repeats: 3,
            via: None,
            transcript: None,
        };
        let now = 100 * 86_400_000;
        s.insert_intent_log(&entry(now - INTENT_LOG_RETENTION_MS - 1, 0)).unwrap();
        for n in 1..=10 {
            s.insert_intent_log(&entry(now, n)).unwrap();
        }
        assert_eq!(s.prune_intent_log(now, 4).unwrap(), 7, "1 too old + 6 over the cap");
        let left = s.intent_log(100).unwrap();
        assert_eq!(left.iter().map(|e| e.intent_id.as_str()).collect::<Vec<_>>(), vec!["i10", "i9", "i8", "i7"]);
        assert_eq!(left[0].repeats, 3);
    }

    /// Review fix L10: names other machines report are cleaned before they are stored.
    #[test]
    fn peer_names_are_sanitized_at_the_roster() {
        assert_eq!(sanitize_machine_name("  Mac\u{1b}[2J mini\n "), "Mac[2J mini");
        assert_eq!(sanitize_machine_name(&"x".repeat(60)), "x".repeat(40));
        assert_eq!(sanitize_machine_name(" \u{7}\t "), "Unnamed machine");
        let s = Store::open_in_memory(MID).unwrap();
        let r: &dyn Roster = &s;
        let mut e = roster_entry(PEER, "Evil\u{1b}]0;title\u{7}Name", "pk", None);
        e.machine.name = e.device.name.clone();
        r.upsert(e);
        let got = r.get(PEER).unwrap();
        assert_eq!((got.device.name.as_str(), got.machine.name.as_str()), ("Evil]0;titleName", "Evil]0;titleName"));
        let mut m = got.machine.clone();
        m.name = "   ".into();
        r.update_seen(PEER, &m, None, 5);
        assert_eq!(r.get(PEER).unwrap().device.name, "Unnamed machine");
    }

    #[tokio::test]
    async fn run_moves_work_off_runtime() {
        let s = Store::open_in_memory(MID).unwrap();
        let n = s.run(|s| Ok(s.recent_events(5, false)?.len())).await.unwrap();
        assert_eq!(n, 0);
    }
}
