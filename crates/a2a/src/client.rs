//! Calling outside A2A agents (PLAN.md "Outside A2A agents", phase A3).
//!
//! The user adds an agent by URL. Pawl fetches its Agent Card (spec 8.2: `/.well-known/agent-card.json`
//! under the URL, or the URL itself when it names a `.json` file), keeps its ETag, and talks JSON-RPC
//! to the card's JSON-RPC interface: A2A 1.0 when the card offers it, else 0.3 through
//! [`crate::compat`]. One code path for both versions; the SDK's client speaks 1.0 only.
//!
//! Safety:
//! - Only `https://` URLs, or `http://` to this machine (loopback). The card's endpoint must be on the
//!   same origin as the URL the user added, so a card cannot send the bearer token elsewhere.
//!   Redirects are never followed (same reason).
//! - Size caps: the card [`CARD_MAX_BYTES`], a unary answer [`RESPONSE_MAX_BYTES`], one stream event
//!   [`EVENT_MAX_BYTES`], a whole stream [`STREAM_MAX_BYTES`]. Timeouts: connect [`CONNECT_TIMEOUT`],
//!   unary calls [`CALL_TIMEOUT`], a stream that sends nothing for [`STREAM_IDLE`].
//! - The bearer token is never logged (`Endpoint`'s Debug hides it).
//! - Everything the agent sends is data: callers treat its text as untrusted.

use std::time::Duration;

use a2a::jsonrpc::methods as m;
use a2a::{AgentCard, CancelTaskRequest, GetTaskRequest, SendMessageRequest, SendMessageResponse, StreamResponse, Task};
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, ETAG, IF_NONE_MATCH};
use reqwest::{StatusCode, Url};
use serde_json::{json, Value};

use crate::compat;
use crate::server::rpc::Wire;

pub const CARD_MAX_BYTES: usize = 256 * 1024;
pub const RESPONSE_MAX_BYTES: usize = 4 * 1024 * 1024;
pub const EVENT_MAX_BYTES: usize = 1024 * 1024;
pub const STREAM_MAX_BYTES: usize = 64 * 1024 * 1024;
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const CALL_TIMEOUT: Duration = Duration::from_secs(60);
pub const STREAM_IDLE: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ClientError {
    #[error("{0}")]
    BadUrl(String),
    #[error("could not reach the agent: {0}")]
    Unreachable(String),
    #[error("the agent answered HTTP {0}")]
    Status(u16),
    #[error("the agent sent more than Pawl accepts")]
    TooLarge,
    #[error("the agent did not answer in time")]
    Timeout,
    #[error("the agent sent something that is not A2A: {0}")]
    Invalid(String),
    /// A JSON-RPC error from the agent (its code and message).
    #[error("the agent said: {message}")]
    Agent { code: i32, message: String },
    #[error("the agent card's endpoint {0} is not on the host the agent was added by")]
    OtherHost(String),
    #[error("the agent card offers no JSON-RPC interface Pawl speaks (1.0 or 0.3)")]
    NoInterface,
}

impl From<reqwest::Error> for ClientError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            ClientError::Timeout
        } else {
            // Without the URL: it may carry nothing secret, but keep messages short.
            ClientError::Unreachable(e.without_url().to_string())
        }
    }
}

/// The HTTP client type (reqwest's).
pub type HttpClient = reqwest::Client;

/// The HTTP client for outside agents: no redirects, connect timeout, Pawl's user agent.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(CONNECT_TIMEOUT)
        .user_agent(concat!("pawl/", env!("CARGO_PKG_VERSION")))
        .build()
        .unwrap_or_default()
}

fn loopback_host(u: &Url) -> bool {
    match u.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// An agent URL Pawl accepts: absolute `https://`, or `http://` to this machine; no user info, no
/// fragment.
pub fn check_agent_url(url: &str) -> Result<Url, ClientError> {
    let u = Url::parse(url.trim()).map_err(|_| ClientError::BadUrl("must be an absolute http(s) URL".into()))?;
    if !u.username().is_empty() || u.password().is_some() {
        return Err(ClientError::BadUrl("must not contain a user name or password".into()));
    }
    if u.fragment().is_some() {
        return Err(ClientError::BadUrl("must not have a #fragment".into()));
    }
    match u.scheme() {
        "https" => Ok(u),
        "http" if loopback_host(&u) => Ok(u),
        "http" => Err(ClientError::BadUrl("must be https (http only to this machine)".into())),
        _ => Err(ClientError::BadUrl("must be an http(s) URL".into())),
    }
}

/// Where the card is: the URL itself when it names a `.json` file, else `/.well-known/agent-card.json`
/// under it.
pub fn card_url(url: &str) -> Result<Url, ClientError> {
    let u = check_agent_url(url)?;
    if u.path().ends_with(".json") {
        return Ok(u);
    }
    let base = u.as_str().trim_end_matches('/').to_string();
    Url::parse(&format!("{base}/.well-known/agent-card.json")).map_err(|_| ClientError::BadUrl("bad URL".into()))
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme() && a.host_str().map(str::to_ascii_lowercase) == b.host_str().map(str::to_ascii_lowercase) && a.port_or_known_default() == b.port_or_known_default()
}

/// A fetched card.
#[derive(Debug, Clone)]
pub enum CardFetch {
    /// The card (normalized to the 1.0 shape), its ETag, and whether it carries signatures.
    Fresh { card: Box<AgentCard>, etag: Option<String>, signed: bool },
    /// `If-None-Match` matched: the cached card is current.
    NotModified,
}

/// Read at most `max` bytes of a response body.
async fn body_capped(mut resp: reqwest::Response, max: usize) -> Result<Vec<u8>, ClientError> {
    if resp.content_length().is_some_and(|n| n as usize > max) {
        return Err(ClientError::TooLarge);
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if out.len() + chunk.len() > max {
            return Err(ClientError::TooLarge);
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

/// Fetch the agent's card from `url` (see [`card_url`]); `etag` = the cached card's.
pub async fn fetch_card(http: &reqwest::Client, url: &str, etag: Option<&str>, token: Option<&str>) -> Result<CardFetch, ClientError> {
    let at = card_url(url)?;
    let mut req = http.get(at).timeout(CALL_TIMEOUT).header(ACCEPT, "application/json");
    if let Some(e) = etag {
        req = req.header(IF_NONE_MATCH, e);
    }
    if let Some(t) = token {
        // Some agents only serve their card to authenticated callers; the card is on the same origin.
        req = req.header(AUTHORIZATION, format!("Bearer {t}"));
    }
    let resp = req.send().await?;
    if resp.status() == StatusCode::NOT_MODIFIED {
        return Ok(CardFetch::NotModified);
    }
    if !resp.status().is_success() {
        return Err(ClientError::Status(resp.status().as_u16()));
    }
    let etag = resp.headers().get(ETAG).and_then(|v| v.to_str().ok()).map(str::to_string);
    let bytes = body_capped(resp, CARD_MAX_BYTES).await?;
    let raw: Value = serde_json::from_slice(&bytes).map_err(|e| ClientError::Invalid(format!("the card is not JSON: {e}")))?;
    let signed = raw.get("signatures").and_then(Value::as_array).is_some_and(|a| !a.is_empty());
    let card: AgentCard = serde_json::from_value(compat::card_from_v03(raw)).map_err(|e| ClientError::Invalid(format!("the card is not an Agent Card: {e}")))?;
    Ok(CardFetch::Fresh { card: Box::new(card), etag, signed })
}

/// Where and how to call an agent.
#[derive(Clone)]
pub struct Endpoint {
    pub url: Url,
    pub wire: Wire,
    token: Option<String>,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint").field("url", &self.url.as_str()).field("wire", &self.wire).field("token", &self.token.as_ref().map(|_| "<redacted>")).finish()
    }
}

impl Endpoint {
    /// An endpoint whose URL was checked elsewhere (tests, a cached card).
    pub fn new(url: Url, wire: Wire, token: Option<String>) -> Self {
        Self { url, wire, token }
    }
}

/// The card's JSON-RPC interface on the origin the agent was added by: 1.0 if offered, else 0.3.
pub fn endpoint(card: &AgentCard, added_url: &str, token: Option<String>) -> Result<Endpoint, ClientError> {
    let added = check_agent_url(added_url)?;
    let jsonrpc: Vec<_> = card.supported_interfaces.iter().filter(|i| i.protocol_binding.eq_ignore_ascii_case("JSONRPC")).collect();
    let pick = |major: &str| jsonrpc.iter().find(|i| i.protocol_version.trim().starts_with(major));
    let (iface, wire) = match (pick("1."), pick("0.3")) {
        (Some(i), _) => (i, Wire::V1),
        (None, Some(i)) => (i, Wire::V03),
        _ => return Err(ClientError::NoInterface),
    };
    let url = Url::parse(&iface.url).map_err(|_| ClientError::Invalid("the card's interface URL is not a URL".into()))?;
    if !same_origin(&url, &added) {
        return Err(ClientError::OtherHost(url.origin().ascii_serialization()));
    }
    Ok(Endpoint { url, wire, token })
}

fn rpc_error(v: &Value) -> Option<ClientError> {
    let e = v.get("error")?;
    let code = e.get("code").and_then(Value::as_i64).unwrap_or(-32603) as i32;
    let message = e.get("message").and_then(Value::as_str).unwrap_or("error").chars().take(500).collect::<String>();
    Some(ClientError::Agent { code, message })
}

impl Endpoint {
    fn request(&self, http: &reqwest::Client, method: &str, params: Value) -> Result<reqwest::RequestBuilder, ClientError> {
        let (name, params) = match self.wire {
            Wire::V1 => (method.to_string(), params),
            Wire::V03 => (compat::method_to_v03(method).ok_or_else(|| ClientError::Invalid(format!("{method} has no 0.3 form")))?.to_string(), compat::params_to_v03(method, params)),
        };
        let body = json!({ "jsonrpc": "2.0", "id": uuid::Uuid::new_v4().to_string(), "method": name, "params": params });
        let mut req = http.post(self.url.clone()).header(CONTENT_TYPE, "application/json").json(&body);
        if self.wire == Wire::V1 {
            req = req.header("A2A-Version", crate::A2A_VERSION);
        }
        if let Some(t) = &self.token {
            req = req.header(AUTHORIZATION, format!("Bearer {t}"));
        }
        Ok(req)
    }

    /// One unary JSON-RPC call; the result in its 1.0 shape.
    pub async fn call(&self, http: &reqwest::Client, method: &str, params: Value) -> Result<Value, ClientError> {
        let resp = self.request(http, method, params)?.timeout(CALL_TIMEOUT).header(ACCEPT, "application/json").send().await?;
        if !resp.status().is_success() {
            return Err(ClientError::Status(resp.status().as_u16()));
        }
        let bytes = body_capped(resp, RESPONSE_MAX_BYTES).await?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|e| ClientError::Invalid(format!("not JSON: {e}")))?;
        if let Some(e) = rpc_error(&v) {
            return Err(e);
        }
        let result = v.get("result").cloned().ok_or_else(|| ClientError::Invalid("no result".into()))?;
        Ok(match self.wire {
            Wire::V1 => result,
            Wire::V03 => compat::result_from_v03(method, result),
        })
    }

    pub async fn send_message(&self, http: &reqwest::Client, req: &SendMessageRequest) -> Result<SendMessageResponse, ClientError> {
        let v = self.call(http, m::SEND_MESSAGE, serde_json::to_value(req).unwrap_or(Value::Null)).await?;
        serde_json::from_value(v).map_err(|e| ClientError::Invalid(format!("not a SendMessage answer: {e}")))
    }

    pub async fn get_task(&self, http: &reqwest::Client, id: &str) -> Result<Task, ClientError> {
        let req = GetTaskRequest { id: id.to_string(), history_length: Some(0), tenant: None };
        let v = self.call(http, m::GET_TASK, serde_json::to_value(req).unwrap_or(Value::Null)).await?;
        serde_json::from_value(v).map_err(|e| ClientError::Invalid(format!("not a task: {e}")))
    }

    pub async fn cancel_task(&self, http: &reqwest::Client, id: &str) -> Result<Task, ClientError> {
        let req = CancelTaskRequest { id: id.to_string(), metadata: None, tenant: None };
        let v = self.call(http, m::CANCEL_TASK, serde_json::to_value(req).unwrap_or(Value::Null)).await?;
        serde_json::from_value(v).map_err(|e| ClientError::Invalid(format!("not a task: {e}")))
    }

    /// `SendStreamingMessage`: the agent's events as they come.
    pub async fn send_streaming(&self, http: &reqwest::Client, req: &SendMessageRequest) -> Result<BoxStream<'static, Result<StreamResponse, ClientError>>, ClientError> {
        let resp = self
            .request(http, m::SEND_STREAMING_MESSAGE, serde_json::to_value(req).unwrap_or(Value::Null))?
            .header(ACCEPT, "text/event-stream")
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(ClientError::Status(resp.status().as_u16()));
        }
        let is_sse = resp.headers().get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).is_some_and(|c| c.starts_with("text/event-stream"));
        if !is_sse {
            // An error answer comes as plain JSON-RPC.
            let bytes = body_capped(resp, RESPONSE_MAX_BYTES).await?;
            let v: Value = serde_json::from_slice(&bytes).map_err(|e| ClientError::Invalid(format!("not JSON: {e}")))?;
            return Err(rpc_error(&v).unwrap_or_else(|| ClientError::Invalid("expected an event stream".into())));
        }
        Ok(sse_events(resp, self.wire))
    }
}

/// Split SSE text into events' `data` (joined lines), per the SSE format: events end with a blank line.
struct SseParser {
    buf: Vec<u8>,
}

impl SseParser {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, ClientError> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            let end = find_blank_line(&self.buf);
            let Some((at, len)) = end else { break };
            let block: Vec<u8> = self.buf.drain(..at + len).collect();
            let text = String::from_utf8_lossy(&block[..at]).into_owned();
            let data: Vec<&str> = text.lines().filter_map(|l| l.strip_prefix("data:")).map(|d| d.strip_prefix(' ').unwrap_or(d)).collect();
            if !data.is_empty() {
                out.push(data.join("\n"));
            }
        }
        if self.buf.len() > EVENT_MAX_BYTES {
            return Err(ClientError::TooLarge);
        }
        Ok(out)
    }
}

fn find_blank_line(b: &[u8]) -> Option<(usize, usize)> {
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'\n' && b[i + 1] == b'\n' {
            return Some((i, 2));
        }
        if i + 3 < b.len() && &b[i..i + 4] == b"\r\n\r\n" {
            return Some((i, 4));
        }
        i += 1;
    }
    None
}

fn event_of(data: &str, wire: Wire) -> Result<StreamResponse, ClientError> {
    let v: Value = serde_json::from_str(data).map_err(|e| ClientError::Invalid(format!("an event is not JSON: {e}")))?;
    if let Some(e) = rpc_error(&v) {
        return Err(e);
    }
    let result = v.get("result").cloned().ok_or_else(|| ClientError::Invalid("an event has no result".into()))?;
    let result = match wire {
        Wire::V1 => result,
        Wire::V03 => compat::stream_event_from_v03(result),
    };
    serde_json::from_value(result).map_err(|e| ClientError::Invalid(format!("not a stream event: {e}")))
}

fn sse_events(resp: reqwest::Response, wire: Wire) -> BoxStream<'static, Result<StreamResponse, ClientError>> {
    struct St {
        resp: reqwest::Response,
        parser: SseParser,
        pending: std::collections::VecDeque<String>,
        total: usize,
        done: bool,
    }
    let st = St { resp, parser: SseParser { buf: Vec::new() }, pending: Default::default(), total: 0, done: false };
    futures::stream::unfold(st, move |mut st| async move {
        loop {
            if let Some(d) = st.pending.pop_front() {
                let ev = event_of(&d, wire);
                if ev.is_err() {
                    st.done = true;
                }
                return Some((ev, st));
            }
            if st.done {
                return None;
            }
            match tokio::time::timeout(STREAM_IDLE, st.resp.chunk()).await {
                Err(_) => {
                    st.done = true;
                    return Some((Err(ClientError::Timeout), st));
                }
                Ok(Err(e)) => {
                    st.done = true;
                    return Some((Err(e.into()), st));
                }
                Ok(Ok(None)) => return None,
                Ok(Ok(Some(chunk))) => {
                    st.total += chunk.len();
                    if st.total > STREAM_MAX_BYTES {
                        st.done = true;
                        return Some((Err(ClientError::TooLarge), st));
                    }
                    match st.parser.push(&chunk) {
                        Ok(events) => st.pending.extend(events),
                        Err(e) => {
                            st.done = true;
                            return Some((Err(e), st));
                        }
                    }
                }
            }
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests;
