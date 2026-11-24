//! SQLite persistence (rusqlite, bundled). All methods here are blocking; async code goes through
//! [`Store::run`], which moves the work onto tokio's blocking pool.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::{Context, Result};
use pet_protocol::{
    Agent, ChatMessage, ChatRole, EventType, HubEvent, PermissionOptionKind, Pet, ResolvedBy, Rule, Session, SessionId,
    ToolKind, UnixMs,
};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{de::DeserializeOwned, Serialize};

/// Ordered schema migrations; `PRAGMA user_version` = number applied.
const MIGRATIONS: &[&str] = &[
    // v1
    r#"
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
"#,
];

const SETTING_RULES_SET: &str = "rules_set";

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

fn row_event(r: &Row<'_>) -> rusqlite::Result<HubEvent> {
    let kind: String = r.get(2)?;
    let payload: String = r.get(3)?;
    Ok(HubEvent {
        id: r.get(0)?,
        session_id: r.get(1)?,
        kind: enum_from(&kind).unwrap_or(EventType::Hook),
        payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
        ts: r.get(4)?,
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
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open {}", path.display()))?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self> {
        migrate(&mut conn)?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
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
        let now = pet_protocol::now_ms();
        for a in agents {
            tx.execute("INSERT INTO agents (id, data, updated_at) VALUES (?1, ?2, ?3)", params![a.id, json(a)?, now])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_agent(&self, a: &Agent) -> Result<()> {
        self.conn().execute(
            "INSERT INTO agents (id, data, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data, updated_at = excluded.updated_at",
            params![a.id, json(a)?, pet_protocol::now_ms()],
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
                                   started_at, last_event_at, ended_at, data)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(id) DO UPDATE SET
                agent_id = excluded.agent_id, source = excluded.source, status = excluded.status,
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
                json(s)?
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

    /// Insert an event (rowid = HubEvent.id) and bump the session's `lastEventAt`.
    pub fn insert_event(&self, session_id: &str, kind: EventType, payload: &serde_json::Value, ts: UnixMs) -> Result<HubEvent> {
        let mut c = self.conn();
        let tx = c.transaction()?;
        tx.execute(
            "INSERT INTO events (session_id, type, payload, ts) VALUES (?1, ?2, ?3, ?4)",
            params![session_id, kind.as_str(), payload.to_string(), ts],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "UPDATE sessions SET last_event_at = max(last_event_at, ?2) WHERE id = ?1",
            params![session_id, ts],
        )?;
        tx.commit()?;
        Ok(HubEvent { id, session_id: session_id.to_string(), kind, payload: payload.clone(), ts })
    }

    /// Events of one session, ascending. `since_id` = strictly after that id (oldest first);
    /// otherwise the latest `limit` events.
    pub fn events(&self, session_id: &str, since_id: Option<i64>, limit: u32) -> Result<Vec<HubEvent>> {
        let c = self.conn();
        let mut out: Vec<HubEvent> = match since_id {
            Some(since) => {
                let mut st = c.prepare(
                    "SELECT id, session_id, type, payload, ts FROM events WHERE session_id = ?1 AND id > ?2 ORDER BY id ASC LIMIT ?3",
                )?;
                let rows = st.query_map(params![session_id, since, limit], row_event)?;
                rows.filter_map(|r| r.ok()).collect()
            }
            None => {
                let mut st = c.prepare(
                    "SELECT id, session_id, type, payload, ts FROM events WHERE session_id = ?1 ORDER BY id DESC LIMIT ?2",
                )?;
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
            "SELECT id, session_id, type, payload, ts FROM events
             WHERE type NOT IN ('agent_thought_chunk', 'agent_message_chunk') ORDER BY id DESC LIMIT ?1"
        } else {
            "SELECT id, session_id, type, payload, ts FROM events ORDER BY id DESC LIMIT ?1"
        };
        let mut st = c.prepare(sql)?;
        let rows = st.query_map([limit], row_event)?;
        let mut v: Vec<HubEvent> = rows.filter_map(|r| r.ok()).collect();
        v.reverse();
        Ok(v)
    }

    /// Events since `ts` (ascending), skipping streaming chunks.
    pub fn events_since(&self, ts: UnixMs, limit: u32) -> Result<Vec<HubEvent>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT id, session_id, type, payload, ts FROM events
             WHERE ts >= ?1 AND type NOT IN ('agent_thought_chunk', 'agent_message_chunk') ORDER BY id ASC LIMIT ?2",
        )?;
        let rows = st.query_map(params![ts, limit], row_event)?;
        Ok(rows.filter_map(|r| r.ok()).collect())
    }

    pub fn prune_events_before(&self, ts: UnixMs) -> Result<usize> {
        Ok(self.conn().execute("DELETE FROM events WHERE ts < ?1", [ts])?)
    }

    // ------------------------------------------------------------ pets

    pub fn upsert_pet(&self, p: &Pet) -> Result<()> {
        self.conn().execute(
            "INSERT INTO pets (id, agent_id, data) VALUES (?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET agent_id = excluded.agent_id, data = excluded.data",
            params![p.id, p.agent_id, json(p)?],
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
            "INSERT INTO chat (id, pet_id, role, text, ts, session_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![m.id, m.pet_id, enum_str(&m.role), m.text, m.ts, m.session_id],
        )?;
        Ok(())
    }

    /// Latest chat messages (ascending).
    pub fn recent_chat(&self, limit: u32) -> Result<Vec<ChatMessage>> {
        let c = self.conn();
        let mut st = c.prepare("SELECT id, pet_id, role, text, ts, session_id FROM chat ORDER BY seq DESC LIMIT ?1")?;
        let rows = st.query_map([limit], |r| {
            let role: String = r.get(2)?;
            Ok(ChatMessage {
                id: r.get(0)?,
                pet_id: r.get(1)?,
                role: enum_from(&role).unwrap_or(ChatRole::User),
                text: r.get(3)?,
                ts: r.get(4)?,
                session_id: r.get(5)?,
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
                                    decided_by, actor, option_id, option_kind, outcome, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
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
                a.detail
            ],
        )?;
        Ok(c.last_insert_rowid())
    }

    /// Latest audit entries (newest first).
    pub fn audit_log(&self, limit: u32) -> Result<Vec<AuditEntry>> {
        let c = self.conn();
        let mut st = c.prepare(
            "SELECT id, ts, request_id, session_id, agent_id, tool_call_id, tool_kind, tool_title, locations,
                    decided_by, actor, option_id, option_kind, outcome, detail
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

fn migrate(conn: &mut Connection) -> Result<()> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let target = MIGRATIONS.len() as i64;
    if current > target {
        anyhow::bail!("hub.db schema v{current} is newer than this pet-hub (v{target}); upgrade pet-hub");
    }
    for (i, sql) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql).with_context(|| format!("migration v{}", i + 1))?;
        tx.pragma_update(None, "user_version", (i + 1) as i64)?;
        tx.commit()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pet_protocol::{AgentSource, PermissionOutcome, RuleAction, RuleMatch, SessionSource, SessionStatus, Species};
    use serde_json::json;

    fn session(id: &str, last: UnixMs) -> Session {
        Session {
            id: id.into(),
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
    fn migrations_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("hub.db");
        Store::open(&p).unwrap();
        let s = Store::open(&p).unwrap();
        let v: i64 = s.conn().query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, MIGRATIONS.len() as i64);
    }

    #[test]
    fn events_roundtrip_and_paging() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_session(&session("s1", 10)).unwrap();
        let mut ids = vec![];
        for i in 0..5 {
            let e = s.insert_event("s1", EventType::AgentMessageChunk, &json!({"text": format!("t{i}")}), 100 + i).unwrap();
            ids.push(e.id);
        }
        s.insert_event("s2", EventType::Stop, &json!({"stopReason":"end_turn"}), 200).unwrap();
        assert!(ids.windows(2).all(|w| w[0] < w[1]));
        let last2 = s.events("s1", None, 2).unwrap();
        assert_eq!(last2.iter().map(|e| e.id).collect::<Vec<_>>(), ids[3..].to_vec());
        let after = s.events("s1", Some(ids[1]), 10).unwrap();
        assert_eq!(after.len(), 3);
        assert_eq!(after[0].payload["text"], "t2");
        assert_eq!(after[0].kind, EventType::AgentMessageChunk);
        // insert_event bumped lastEventAt
        assert_eq!(s.session("s1").unwrap().unwrap().last_event_at, 104);
        let recent = s.recent_events(10, true).unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].kind, EventType::Stop);
        assert_eq!(s.prune_events_before(102).unwrap(), 2);
    }

    #[test]
    fn sessions_agents_pets_rules_chat() {
        let s = Store::open_in_memory().unwrap();
        let mut a = session("a", 50);
        s.upsert_session(&a).unwrap();
        a.status = SessionStatus::Done;
        a.ended_at = Some(60);
        a.last_event_at = 40; // never moves backwards
        s.upsert_session(&a).unwrap();
        let got = s.session("a").unwrap().unwrap();
        assert_eq!(got.status, SessionStatus::Done);
        assert_eq!(got.last_event_at, 50);
        assert_eq!(got.ended_at, Some(60));
        s.upsert_session(&session("b", 5)).unwrap();
        let active = s.sessions_active_since(45).unwrap();
        assert_eq!(active.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(s.recent_sessions(1).unwrap()[0].id, "a");

        let agent = Agent {
            id: "codex".into(),
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
        assert_eq!(s.agents().unwrap(), vec![agent]);

        let pet = Pet {
            id: "pet-codex".into(),
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

        let m = ChatMessage { id: "c1".into(), pet_id: Some("pet-codex".into()), role: ChatRole::Pet, text: "woof".into(), ts: 3, session_id: None };
        s.insert_chat(&m).unwrap();
        assert_eq!(s.recent_chat(5).unwrap(), vec![m]);

        let _ = PermissionOutcome::Cancelled;
        s.set_setting("k", "v").unwrap();
        assert_eq!(s.setting("k").unwrap().as_deref(), Some("v"));
    }

    #[test]
    fn audit_roundtrip() {
        let s = Store::open_in_memory().unwrap();
        let a = AuditEntry {
            id: 0,
            ts: 9,
            request_id: "r".into(),
            session_id: "s".into(),
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
        let s = Store::open_in_memory().unwrap();
        let n = s.run(|s| Ok(s.recent_events(5, false)?.len())).await.unwrap();
        assert_eq!(n, 0);
    }
}
