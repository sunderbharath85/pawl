//! SQLite persistence (rusqlite, bundled). All methods here are blocking; async code goes through
//! [`Store::run`], which moves the work onto tokio's blocking pool.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use krypto_protocol::{
    global_id, is_valid_machine_id, parse_global_id, Agent, ChatMessage, ChatRole, EventType, HubEvent, MachineId,
    PermissionOptionKind, Pet, ResolvedBy, Rule, Session, SessionId, ToolKind, UnixMs,
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
const MIGRATIONS: &[Migration] = &[Migration::Sql(SCHEMA_V1), Migration::Code(migrate_v2)];

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
}

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    /// This hub's machine; prefix of every global id it stores.
    machine_id: MachineId,
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
const EVENT_COLS: &str = "id, machine_id, session_id, type, payload, ts";

/// The rowid is the per-machine `seq`; `HubEvent.id` is its global form.
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
        Ok(Self { conn: Arc::new(Mutex::new(conn)), machine_id: machine_id.to_string() })
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
        let now = krypto_protocol::now_ms();
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
            params![a.id, a.machine_id, json(a)?, krypto_protocol::now_ms()],
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
        self.conn().execute(
            "INSERT INTO sessions (id, agent_id, source, status, external_id, acp_session_id, cwd, title,
                                   started_at, last_event_at, ended_at, data, machine_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(id) DO UPDATE SET
                machine_id = excluded.machine_id, agent_id = excluded.agent_id, source = excluded.source, status = excluded.status,
                external_id = excluded.external_id, acp_session_id = excluded.acp_session_id, cwd = excluded.cwd,
                title = excluded.title, started_at = excluded.started_at,
                last_event_at = max(sessions.last_event_at, excluded.last_event_at),
                ended_at = excluded.ended_at, data = excluded.data",
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

    pub fn session(&self, id: &str) -> Result<Option<Session>> {
        let c = self.conn();
        Ok(c.query_row(&format!("SELECT {SESSION_COLS} FROM sessions WHERE id = ?1"), [id], row_session).optional()?)
    }

    /// Open sessions plus sessions with activity since `since` (newest first).
    pub fn sessions_active_since(&self, since: UnixMs) -> Result<Vec<Session>> {
        let c = self.conn();
        let mut st = c.prepare(&format!(
            "SELECT {SESSION_COLS} FROM sessions WHERE ended_at IS NULL OR last_event_at >= ?1 ORDER BY last_event_at DESC"
        ))?;
        let rows = st.query_map([since], row_session)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    /// Most recent sessions, ended or not.
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
                    "SELECT {EVENT_COLS} FROM events WHERE session_id = ?1 AND id > ?2 ORDER BY id ASC LIMIT ?3"
                ))?;
                let rows = st.query_map(params![session_id, since, limit], row_event)?;
                rows.filter_map(|r| r.ok()).collect()
            }
            None => {
                let mut st =
                    c.prepare(&format!("SELECT {EVENT_COLS} FROM events WHERE session_id = ?1 ORDER BY id DESC LIMIT ?2"))?;
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
    pub fn events_since(&self, ts: UnixMs, limit: u32) -> Result<Vec<HubEvent>> {
        let c = self.conn();
        let mut st = c.prepare(&format!(
            "SELECT {EVENT_COLS} FROM events
             WHERE ts >= ?1 AND type NOT IN ('agent_thought_chunk', 'agent_message_chunk') ORDER BY id ASC LIMIT ?2"
        ))?;
        let rows = st.query_map(params![ts, limit], row_event)?;
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
            "INSERT INTO chat (id, machine_id, pet_id, role, text, ts, session_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![m.id, m.machine_id, m.pet_id, enum_str(&m.role), m.text, m.ts, m.session_id],
        )?;
        Ok(())
    }

    /// Latest chat messages (ascending).
    pub fn recent_chat(&self, limit: u32) -> Result<Vec<ChatMessage>> {
        let c = self.conn();
        let mut st =
            c.prepare("SELECT id, machine_id, pet_id, role, text, ts, session_id FROM chat ORDER BY seq DESC LIMIT ?1")?;
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
                                    decided_by, actor, option_id, option_kind, outcome, detail, machine_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
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
                a.machine_id
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Latest audit entries (newest first).
    pub fn audit_log(&self, limit: u32) -> Result<Vec<AuditEntry>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT id, ts, request_id, session_id, agent_id, tool_call_id, tool_kind, tool_title, locations,
                    decided_by, actor, option_id, option_kind, outcome, detail, machine_id
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
            })
        })?;
        Ok(rows.filter_map(|r| r.ok()).collect())
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

fn migrate(conn: &mut Connection, machine_id: &str) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let target = MIGRATIONS.len() as i64;
    if current > target {
        bail!("hub.db schema v{current} is newer than this krypto (v{target}); upgrade krypto");
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
    use krypto_protocol::{AgentSource, PermissionOutcome, RuleAction, RuleMatch, SessionSource, SessionStatus, Species};
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
        let req: krypto_protocol::PermissionRequest = serde_json::from_value(events[0].payload.clone()).unwrap();
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
        };
        s.insert_chat(&m).unwrap();
        assert_eq!(s.recent_chat(5).unwrap(), vec![m]);

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
        };
        let id = s.insert_audit(&a).unwrap();
        let got = s.audit_log(10).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], AuditEntry { id, ..a });
    }

    #[tokio::test]
    async fn run_moves_work_off_runtime() {
        let s = Store::open_in_memory(MID).unwrap();
        let n = s.run(|s| Ok(s.recent_events(5, false)?.len())).await.unwrap();
        assert_eq!(n, 0);
    }
}
