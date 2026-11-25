//! `pawl mcp`: a stdio MCP server that is a thin client of Pawl's localhost A2A server, so coding
//! agents (which speak MCP, not A2A) can ask Pawl what is going on and hand work to other agents.
//!
//! MCP is hand-written (newline-delimited JSON-RPC 2.0 on stdin / stdout, logs on stderr): five
//! methods do not justify the official SDK's dependencies. It serves the MCP `2026-07-28` revision
//! (per-request `_meta` protocol version, `server/discover`, `resultType` / `ttlMs` / `cacheScope`,
//! `-32022` for an unknown version) and the legacy `initialize` era (`2025-11-25` and older).
//! Spec: <https://modelcontextprotocol.io/specification/2026-07-28>.
//!
//! It talks only to 127.0.0.1 / ::1 / localhost over plain HTTP without a proxy, as the A2A caller
//! whose token it was given. Every action goes through Pawl's normal approval cards; there is no tool
//! to answer an approval.

use std::time::Duration;

use a2a::jsonrpc::methods as m;
use a2a::{GetExtendedAgentCardRequest, ListTasksRequest, Message, Part, Role, SendMessageConfiguration, SendMessageRequest, SendMessageResponse, Task, TaskState};
use reqwest::Url;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::client::{ClientError, Endpoint};
use crate::ext::DelegateParams;
use crate::server::rpc::Wire;

/// Env vars `pawl mcp` reads (the entry Connect writes sets both).
pub const URL_ENV: &str = "PAWL_A2A_URL";
pub const TOKEN_ENV: &str = "PAWL_A2A_TOKEN";

/// MCP revisions served statelessly (per-request `_meta`), newest first.
pub const MODERN_VERSIONS: &[&str] = &["2026-07-28"];
/// Revisions served after `initialize`, newest first.
pub const LEGACY_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// The request `_meta` key carrying the protocol version (2026-07-28).
pub const VERSION_META: &str = "io.modelcontextprotocol/protocolVersion";
const SERVER_INFO_META: &str = "io.modelcontextprotocol/serverInfo";

/// JSON-RPC / MCP error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const UNSUPPORTED_VERSION: i64 = -32022;

/// A longer line is refused (and skipped).
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
/// How long tool lists may be cached by the client.
const TTL_MS: u64 = 3_600_000;
/// Text of task status in results is cut to this many characters.
const TEXT_CHARS: usize = 2000;
/// Tasks `pawl_status` shows.
const STATUS_TASKS: i32 = 20;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

const INSTRUCTIONS: &str = "Pawl runs the coding agents on this machine. pawl_agents lists the agents Pawl can start; pawl_start starts one in a folder Pawl works in; pawl_status and pawl_task show how the work goes; pawl_prompt sends a follow-up; pawl_cancel stops a task. Tool calls of the agents Pawl starts ask the user on this machine: you cannot approve them.";

/// Where `pawl mcp` connects: Pawl's localhost A2A server and the caller's token.
#[derive(Clone)]
pub struct BridgeConfig {
    url: Url,
    token: String,
}

impl std::fmt::Debug for BridgeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BridgeConfig").field("url", &self.url.as_str()).field("token", &"<redacted>").finish()
    }
}

impl BridgeConfig {
    /// Only `http://127.0.0.1:<port>`, `http://[::1]:<port>` or `http://localhost:<port>` (a `/`
    /// path at most; no user info, query or fragment), and a non-empty token.
    pub fn new(url: &str, token: &str) -> Result<Self, String> {
        let refuse = || format!("{URL_ENV} must be Pawl's local A2A address, http://127.0.0.1:<port>; pawl mcp talks to this machine only");
        let u = Url::parse(url.trim()).map_err(|_| refuse())?;
        let host_ok = match u.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
            None => false,
        };
        let plain = u.scheme() == "http" && u.username().is_empty() && u.password().is_none() && u.query().is_none() && u.fragment().is_none();
        if !host_ok || !plain || u.port().is_none() || !matches!(u.path(), "" | "/") {
            return Err(refuse());
        }
        let token = token.trim();
        if token.is_empty() {
            return Err(format!("{TOKEN_ENV} is not set: connect this agent again in Pawl's Settings > A2A"));
        }
        let mut url = u;
        if url.host() == Some(url::Host::Domain("localhost")) {
            // Never ask a resolver: localhost is 127.0.0.1 here.
            let _ = url.set_host(Some("127.0.0.1"));
        }
        url.set_path("/");
        Ok(Self { url, token: token.to_string() })
    }

    pub fn from_env() -> Result<Self, String> {
        let url = std::env::var(URL_ENV).map_err(|_| format!("{URL_ENV} is not set: connect this agent again in Pawl's Settings > A2A"))?;
        let token = std::env::var(TOKEN_ENV).unwrap_or_default();
        Self::new(&url, &token)
    }

    pub fn url(&self) -> &str {
        self.url.as_str()
    }
}

/// One MCP connection (one `pawl mcp` process).
pub struct Bridge {
    endpoint: Endpoint,
    http: reqwest::Client,
    url: String,
    /// The version `initialize` agreed on (legacy era).
    legacy: Option<String>,
}

impl Bridge {
    /// Fails closed: without the client it asked for (no proxy, no redirects) it does not run.
    pub fn new(cfg: BridgeConfig) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .user_agent(concat!("pawl-mcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("could not set up the HTTP client: {e}"))?;
        let url = cfg.url.as_str().trim_end_matches('/').to_string();
        Ok(Self { endpoint: Endpoint::new(cfg.url, Wire::V1, Some(cfg.token)), http, url, legacy: None })
    }

    /// The version `initialize` agreed on, if any.
    pub fn legacy_version(&self) -> Option<&str> {
        self.legacy.as_deref()
    }

    /// Answer one JSON-RPC message; None for notifications (and responses, which we never ask for).
    pub async fn handle(&mut self, msg: Value) -> Option<Value> {
        if msg.is_array() {
            return Some(error(Value::Null, INVALID_REQUEST, "batches are not supported", None));
        }
        let id = msg.get("id").cloned();
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            return id.map(|id| error(id, INVALID_REQUEST, "not a JSON-RPC request", None));
        };
        // A notification: nothing to answer.
        let id = id?;
        if msg.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(error(id, INVALID_REQUEST, "not a JSON-RPC 2.0 request", None));
        }
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        let modern = match params.get("_meta").and_then(|m| m.get(VERSION_META)) {
            None => None,
            Some(Value::String(v)) if MODERN_VERSIONS.contains(&v.as_str()) || LEGACY_VERSIONS.contains(&v.as_str()) => Some(v.clone()),
            Some(v) => {
                let all: Vec<&str> = MODERN_VERSIONS.iter().chain(LEGACY_VERSIONS).copied().collect();
                return Some(error(id, UNSUPPORTED_VERSION, "Unsupported protocol version", Some(json!({ "supported": all, "requested": v }))));
            }
        };
        let result = match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "server/discover" => match modern {
                Some(_) => Ok(discover()),
                None => Err((INVALID_PARAMS, format!("server/discover needs _meta[\"{VERSION_META}\"]"))),
            },
            "tools/list" => Ok(json!({ "tools": tools(), "ttlMs": TTL_MS, "cacheScope": "private" })),
            "tools/call" => self.call_tool(&params).await,
            other => Err((METHOD_NOT_FOUND, format!("Method not found: {}", other.chars().take(100).collect::<String>()))),
        };
        Some(match result {
            Ok(mut r) => {
                if let Some(obj) = r.as_object_mut() {
                    obj.entry("resultType").or_insert(json!("complete"));
                    if modern.is_some() {
                        obj.entry("_meta").or_insert_with(|| json!({ SERVER_INFO_META: server_info() }));
                    }
                }
                json!({ "jsonrpc": "2.0", "id": id, "result": r })
            }
            Err((code, message)) => error(id, code, &message, None),
        })
    }

    fn initialize(&mut self, params: &Value) -> Value {
        let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
        let version = if LEGACY_VERSIONS.contains(&asked) { asked } else { LEGACY_VERSIONS[0] };
        self.legacy = Some(version.to_string());
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": server_info(),
            "instructions": INSTRUCTIONS,
        })
    }

    async fn call_tool(&self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params.get("name").and_then(Value::as_str).ok_or((INVALID_PARAMS, "tools/call needs a tool name".to_string()))?;
        let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
        if !args.is_object() {
            return Ok(tool_error("arguments must be an object"));
        }
        let arg = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        let need = |k: &str| arg(k).ok_or_else(|| format!("{k} is required"));
        let out = match name {
            "pawl_status" => self.status().await,
            "pawl_agents" => self.agents().await,
            "pawl_start" => match (need("agent"), need("folder"), need("prompt")) {
                (Ok(a), Ok(f), Ok(p)) => self.start(&a, &f, &p).await,
                (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return Ok(tool_error(&e)),
            },
            "pawl_prompt" => match (need("session"), need("prompt")) {
                (Ok(s), Ok(p)) => self.prompt(&s, &p).await,
                (Err(e), _) | (_, Err(e)) => return Ok(tool_error(&e)),
            },
            "pawl_task" => match need("task") {
                Ok(t) => self.endpoint.get_task(&self.http, &t).await.map(|t| task_result("", &t)),
                Err(e) => return Ok(tool_error(&e)),
            },
            "pawl_cancel" => match need("task") {
                Ok(t) => self.endpoint.cancel_task(&self.http, &t).await.map(|t| task_result("Asked Pawl to stop it. ", &t)),
                Err(e) => return Ok(tool_error(&e)),
            },
            other => return Err((INVALID_PARAMS, format!("Unknown tool: {}", other.chars().take(100).collect::<String>()))),
        };
        Ok(out.unwrap_or_else(|e| tool_error(&self.explain(&e))))
    }

    async fn status(&self) -> Result<Value, ClientError> {
        let req = ListTasksRequest {
            context_id: None,
            status: None,
            page_size: Some(STATUS_TASKS),
            page_token: None,
            history_length: Some(0),
            status_timestamp_after: None,
            include_artifacts: Some(false),
            tenant: None,
        };
        let v = self.endpoint.call(&self.http, m::LIST_TASKS, serde_json::to_value(req).unwrap_or_default()).await?;
        let tasks: Vec<Task> = v.get("tasks").cloned().and_then(|t| serde_json::from_value(t).ok()).unwrap_or_default();
        let rows: Vec<Value> = tasks.iter().map(task_row).collect();
        let mut text = if rows.is_empty() { "Nothing started through Pawl by this agent yet.".to_string() } else { "Work started through Pawl, newest first:".to_string() };
        for r in &rows {
            text.push_str(&format!("\n- {}", row_line(r)));
        }
        Ok(ok(text, json!({ "tasks": rows })))
    }

    async fn agents(&self) -> Result<Value, ClientError> {
        let v = self.endpoint.call(&self.http, m::GET_EXTENDED_AGENT_CARD, serde_json::to_value(GetExtendedAgentCardRequest { tenant: None }).unwrap_or_default()).await?;
        let skills = v.get("skills").and_then(Value::as_array).cloned().unwrap_or_default();
        let agents: Vec<Value> = skills
            .iter()
            .filter_map(|s| {
                let id = s.get("id")?.as_str()?.strip_prefix("delegate:")?;
                let name = s.get("name").and_then(Value::as_str).unwrap_or(id);
                let name = name.strip_prefix("Delegate to ").unwrap_or(name);
                Some(json!({ "agent": id, "name": name, "description": s.get("description").cloned().unwrap_or(Value::Null) }))
            })
            .collect();
        let mut text = if agents.is_empty() { "Pawl has no coding agents to start on this machine.".to_string() } else { "Agents Pawl can start (use the agent id with pawl_start):".to_string() };
        for a in &agents {
            text.push_str(&format!("\n- {} ({}): {}", a["agent"].as_str().unwrap_or(""), a["name"].as_str().unwrap_or(""), a["description"].as_str().unwrap_or("")));
        }
        Ok(ok(text, json!({ "agents": agents })))
    }

    async fn start(&self, agent: &str, folder: &str, prompt: &str) -> Result<Value, ClientError> {
        let mut message = Message::new(Role::User, vec![Part::text(prompt)]);
        DelegateParams { agent_id: Some(agent.to_string()), cwd: Some(folder.to_string()), ..Default::default() }.attach(&mut message);
        self.send(message, "Started. ").await
    }

    async fn prompt(&self, session: &str, prompt: &str) -> Result<Value, ClientError> {
        let mut message = Message::new(Role::User, vec![Part::text(prompt)]);
        message.context_id = Some(session.to_string());
        self.send(message, "Sent. ").await
    }

    async fn send(&self, message: Message, lead: &str) -> Result<Value, ClientError> {
        let configuration = SendMessageConfiguration { accepted_output_modes: None, task_push_notification_config: None, history_length: Some(0), return_immediately: Some(true) };
        let req = SendMessageRequest { message, configuration: Some(configuration), metadata: None, tenant: None };
        Ok(match self.endpoint.send_message(&self.http, &req).await? {
            SendMessageResponse::Task(t) => task_result(lead, &t),
            SendMessageResponse::Message(msg) => {
                let text = msg.text().unwrap_or("").chars().take(TEXT_CHARS).collect::<String>();
                ok(format!("Pawl answered: {text}"), json!({ "message": text }))
            }
        })
    }

    /// A short sentence for the model; never the token.
    fn explain(&self, e: &ClientError) -> String {
        match e {
            ClientError::Agent { message, .. } => format!("Pawl said: {message}"),
            ClientError::Status(401) => "Pawl refused this agent's token: connect it again in Pawl's Settings > A2A".into(),
            ClientError::Status(403) => "Pawl refused the request (403)".into(),
            ClientError::Status(429) => "Pawl is busy with too many requests from this agent; try again in a minute".into(),
            ClientError::Unreachable(_) | ClientError::Timeout => {
                format!("Pawl's A2A server is not answering at {}: is Pawl running, with Settings > A2A > server on?", self.url)
            }
            other => format!("Pawl's answer could not be used: {other}"),
        }
    }
}

fn error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut e = json!({ "code": code, "message": message });
    if let Some(d) = data {
        e["data"] = d;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": e })
}

fn server_info() -> Value {
    json!({ "name": "pawl", "title": "Pawl", "version": env!("CARGO_PKG_VERSION") })
}

fn discover() -> Value {
    json!({
        "supportedVersions": MODERN_VERSIONS,
        "capabilities": { "tools": {} },
        "instructions": INSTRUCTIONS,
        "ttlMs": TTL_MS,
        "cacheScope": "private",
    })
}

fn ok(text: String, structured: Value) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "structuredContent": structured, "isError": false })
}

fn tool_error(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

fn state_word(s: &TaskState) -> &'static str {
    crate::mapping::state_name(s)
}

fn task_row(t: &Task) -> Value {
    let text: Option<String> = t.status.message.as_ref().and_then(|m| m.text()).map(|s| s.chars().take(TEXT_CHARS).collect());
    let mut row = json!({ "task": t.id, "session": t.context_id, "state": state_word(&t.status.state) });
    if let Some(text) = text {
        row["text"] = json!(text);
    }
    if t.status.state == TaskState::InputRequired {
        row["waitsForUser"] = json!(true);
    }
    row
}

fn row_line(r: &Value) -> String {
    let mut s = format!("task {} (session {}): {}", r["task"].as_str().unwrap_or(""), r["session"].as_str().unwrap_or(""), r["state"].as_str().unwrap_or(""));
    if r.get("waitsForUser").is_some() {
        s.push_str(", waiting for the user on this machine");
    }
    if let Some(t) = r.get("text").and_then(Value::as_str) {
        s.push_str(&format!(": {t}"));
    }
    s
}

fn task_result(lead: &str, t: &Task) -> Value {
    let row = task_row(t);
    ok(format!("{lead}{}", row_line(&row)), row)
}

fn schema(props: &[(&str, &str)]) -> Value {
    let mut p = serde_json::Map::new();
    for (k, d) in props {
        p.insert(k.to_string(), json!({ "type": "string", "description": d }));
    }
    let required: Vec<&str> = props.iter().map(|(k, _)| *k).collect();
    json!({ "type": "object", "properties": p, "required": required, "additionalProperties": false })
}

fn tool(name: &str, title: &str, description: &str, input: Value, read_only: bool, destructive: bool, idempotent: bool) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": input,
        "annotations": { "title": title, "readOnlyHint": read_only, "destructiveHint": destructive, "idempotentHint": idempotent, "openWorldHint": false },
    })
}

/// The tools `pawl mcp` offers.
pub fn tools() -> Vec<Value> {
    vec![
        tool("pawl_status", "Pawl status", "What the work you started through Pawl is doing, newest first, and what waits for the user.", schema(&[]), true, false, true),
        tool("pawl_agents", "Pawl agents", "The coding agents Pawl can start on this machine, with the repos it knows.", schema(&[]), true, false, true),
        tool(
            "pawl_start",
            "Start an agent",
            "Start a coding agent in a folder Pawl works in, with a first prompt. Its tool calls ask the user on this machine.",
            schema(&[("agent", "Agent id from pawl_agents, e.g. codex"), ("folder", "Absolute path of a folder Pawl knows"), ("prompt", "What the agent should do")]),
            false,
            false,
            false,
        ),
        tool(
            "pawl_prompt",
            "Prompt a session",
            "Send a follow-up to a session you started with pawl_start.",
            schema(&[("session", "The session id pawl_start returned"), ("prompt", "The follow-up")]),
            false,
            false,
            false,
        ),
        tool("pawl_task", "Pawl task", "The state and latest text of one task.", schema(&[("task", "The task id")]), true, false, true),
        tool("pawl_cancel", "Stop a task", "Stop a task you started.", schema(&[("task", "The task id")]), false, false, true),
    ]
}

/// Serve MCP on `input` / `output` until `input` ends.
pub async fn serve<R, W>(cfg: BridgeConfig, mut input: R, mut output: W) -> anyhow::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut bridge = Bridge::new(cfg).map_err(anyhow::Error::msg)?;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = (&mut input).take(MAX_LINE_BYTES as u64 + 1).read_until(b'\n', &mut buf).await?;
        if n == 0 {
            return Ok(());
        }
        let reply = if buf.len() > MAX_LINE_BYTES && buf.last() != Some(&b'\n') {
            // Skip the rest of the line.
            let mut rest = Vec::new();
            loop {
                rest.clear();
                let k = (&mut input).take(64 * 1024).read_until(b'\n', &mut rest).await?;
                if k == 0 || rest.last() == Some(&b'\n') {
                    break;
                }
            }
            Some(error(Value::Null, INVALID_REQUEST, "message too large", None))
        } else {
            let text = String::from_utf8_lossy(&buf);
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(text) {
                Ok(msg) => bridge.handle(msg).await,
                Err(_) => Some(error(Value::Null, PARSE_ERROR, "Parse error", None)),
            }
        };
        if let Some(r) = reply {
            let mut line = serde_json::to_vec(&r)?;
            line.push(b'\n');
            output.write_all(&line).await?;
            output.flush().await?;
        }
    }
}

/// `pawl mcp`: serve on this process's stdin / stdout.
pub async fn run_stdio(cfg: BridgeConfig) -> anyhow::Result<()> {
    serve(cfg, tokio::io::BufReader::new(tokio::io::stdin()), tokio::io::stdout()).await
}

#[cfg(test)]
mod tests;
