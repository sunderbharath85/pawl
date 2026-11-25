//! The localhost A2A server (PLAN.md "Pawl as an A2A agent", phase A2): transport and security.
//!
//! - Binds 127.0.0.1 only. Off unless the hub starts it (`[a2a] server = true`).
//! - JSON-RPC at `POST /` ([`rpc`]) and HTTP+JSON (REST) under `/` ([`rest`]), A2A 1.0 (spec sections
//!   9 and 11), streaming over SSE, no push notifications (every push method answers
//!   `PushNotificationNotSupported`). Pawl's own dispatcher over the `a2a-lf` types: the SDK's server
//!   crate is not used (its protobuf build pulled in a vendored protoc and tonic/prost).
//! - A JSON-RPC request without an `A2A-Version` header (or with `0.3`) is an A2A 0.3 request (spec
//!   3.6.2): [`crate::compat`] converts it to 1.0 for the same handler and the answer back. REST is
//!   1.0; other versions get `VersionNotSupported`.
//! - Every request: the `Host` header must name this loopback server (`127.0.0.1`, `localhost` or
//!   `[::1]` with the bound port): a DNS-rebinding page reaches the socket with its own host name and
//!   is refused. An `Origin` header must be this server's own origin (browsers send one; other
//!   clients do not). `OPTIONS` (CORS preflight) is refused, and no CORS headers are ever sent.
//! - Everything but the public Agent Card needs `Authorization: Bearer <caller token>`, compared in
//!   constant time with each caller's token. Failed attempts are rate limited for everyone (a
//!   guessing local process), calls per caller too. The caller's id reaches the handler in
//!   [`CALLER_HEADER`], which the guard always strips from the incoming request first; the
//!   `Authorization` header is removed before the handler sees the request. Tokens are never logged.
//! - Bodies over [`BODY_MAX_BYTES`] are refused. A request is authenticated and rate-limited on its
//!   headers before any body byte is read; the body must arrive within [`BODY_READ_TIMEOUT`] and the
//!   headers within [`HEADER_READ_TIMEOUT`]; at most [`MAX_CONNECTIONS`] connections are open.
//! - Every refusal (host, origin, size, token, throttling) is reported to the hub's audit hook
//!   ([`ServerState::with_audit`]).
//! - A caller whose token is rotated or removed is *revoked*: [`ServerState::revocation`] tells the
//!   handler to close that caller's open streams and waits.

pub mod rest;
pub mod rpc;

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use a2a::{
    A2AError, AgentCard, CancelTaskRequest, GetExtendedAgentCardRequest, GetTaskRequest, ListTasksRequest, ListTasksResponse, SendMessageRequest,
    SendMessageResponse, StreamResponse, SubscribeToTaskRequest, Task,
};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures::stream::BoxStream;
use futures::StreamExt;
use subtle::ConstantTimeEq;

use crate::card;

/// Request headers as the handler sees them (lower-case names; the guard has removed
/// `Authorization` and set [`CALLER_HEADER`]).
pub type ServiceParams = HashMap<String, Vec<String>>;

/// A task's updates as a stream (`SendStreamingMessage`, `SubscribeToTask`).
pub type EventStream = BoxStream<'static, Result<StreamResponse, A2AError>>;

/// The A2A operations Pawl serves (spec 3.1), binding-independent. Push notification configuration
/// is not among them: the server answers those methods itself (`PushNotificationNotSupported`,
/// spec 3.3.4).
#[async_trait::async_trait]
pub trait A2aHandler: Send + Sync + 'static {
    async fn send_message(&self, params: &ServiceParams, req: SendMessageRequest) -> Result<SendMessageResponse, A2AError>;
    async fn send_streaming_message(&self, params: &ServiceParams, req: SendMessageRequest) -> Result<EventStream, A2AError>;
    async fn get_task(&self, params: &ServiceParams, req: GetTaskRequest) -> Result<Task, A2AError>;
    async fn list_tasks(&self, params: &ServiceParams, req: ListTasksRequest) -> Result<ListTasksResponse, A2AError>;
    async fn cancel_task(&self, params: &ServiceParams, req: CancelTaskRequest) -> Result<Task, A2AError>;
    async fn subscribe_to_task(&self, params: &ServiceParams, req: SubscribeToTaskRequest) -> Result<EventStream, A2AError>;
    async fn get_extended_agent_card(&self, params: &ServiceParams, req: GetExtendedAgentCardRequest) -> Result<AgentCard, A2AError>;
}

/// Interval of SSE keep-alive comments.
pub const SSE_KEEP_ALIVE: Duration = Duration::from_secs(15);
/// A request's headers must arrive within this.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// A request's body must arrive within this (after it was authenticated).
pub const BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Open connections at most (more are closed at once).
pub const MAX_CONNECTIONS: usize = 64;

/// Where refusals go (the hub's A2A log).
type AuditHook = Box<dyn Fn(Refusal) + Send + Sync>;

/// A request the guard refused, for the hub's audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The authenticated caller, when the refusal came after authentication (rate limit, body).
    pub caller: Option<String>,
    /// "forbidden_host", "forbidden_origin", "too_large", "unauthorized", "throttled", "rate_limited",
    /// "body_timeout".
    pub status: &'static str,
    /// `METHOD /path` (the path only; never a query or a body).
    pub request: String,
}

/// The caller id the guard hands to the handler (never trusted from the client).
pub const CALLER_HEADER: &str = "x-pawl-a2a-caller";
/// Largest request body.
pub const BODY_MAX_BYTES: usize = 1024 * 1024;
/// Calls per caller per second (token bucket; bursts up to twice that).
pub const CALLER_RATE_PER_SEC: f64 = 20.0;
/// Requests for the public card per second, all clients together.
pub const PUBLIC_RATE_PER_SEC: f64 = 10.0;
/// Failed authentications per minute, all clients together, before every attempt gets 429.
pub const AUTH_FAILURES_PER_MIN: u32 = 30;
/// `/.well-known/agent-card.json` (spec 8.2).
pub const WELL_KNOWN_CARD: &str = "/.well-known/agent-card.json";

/// One outside caller the server accepts.
#[derive(Clone)]
pub struct CallerAuth {
    pub id: String,
    pub token: String,
}

impl std::fmt::Debug for CallerAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CallerAuth").field("id", &self.id).field("token", &"<redacted>").finish()
    }
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn new(burst: f64) -> Self {
        Self { tokens: burst, last: Instant::now() }
    }

    fn take(&mut self, rate: f64, burst: f64) -> bool {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * rate).min(burst);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

struct Limits {
    callers: HashMap<String, Bucket>,
    public: Bucket,
    failures: (Instant, u32),
}

/// Live server state: the bound port (for the Host check), the callers (replaced at runtime when the
/// settings change) and the rate limits.
pub struct ServerState {
    port: AtomicU16,
    callers: RwLock<Vec<CallerAuth>>,
    limits: Mutex<Limits>,
    card: Box<dyn Fn() -> AgentCard + Send + Sync>,
    audit: RwLock<Option<AuditHook>>,
    /// Per caller: a counter bumped when its token changes or it is removed.
    revocations: Mutex<HashMap<String, tokio::sync::watch::Sender<u64>>>,
}

/// Why a request was not authenticated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthFail {
    Missing,
    Wrong,
    Throttled,
}

impl ServerState {
    /// `card` builds the public Agent Card on every request (it follows the hub's state).
    pub fn new(callers: Vec<CallerAuth>, card: impl Fn() -> AgentCard + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            port: AtomicU16::new(0),
            callers: RwLock::new(callers),
            limits: Mutex::new(Limits { callers: HashMap::new(), public: Bucket::new(PUBLIC_RATE_PER_SEC * 2.0), failures: (Instant::now(), 0) }),
            card: Box::new(card),
            audit: RwLock::new(None),
            revocations: Mutex::new(HashMap::new()),
        })
    }

    /// Report every refusal to `audit` (the hub writes it to its A2A log).
    pub fn with_audit(self: Arc<Self>, audit: impl Fn(Refusal) + Send + Sync + 'static) -> Arc<Self> {
        *self.audit.write().unwrap_or_else(|p| p.into_inner()) = Some(Box::new(audit));
        self
    }

    fn refuse(&self, caller: Option<&str>, status: &'static str, req: &Request) -> Response {
        if let Some(f) = self.audit.read().unwrap_or_else(|p| p.into_inner()).as_ref() {
            let path: String = req.uri().path().chars().take(120).collect();
            f(Refusal { caller: caller.map(str::to_string), status, request: format!("{} {path}", req.method()) });
        }
        let (code, text) = match status {
            "forbidden_host" => (StatusCode::FORBIDDEN, "host not allowed"),
            "forbidden_origin" => (StatusCode::FORBIDDEN, "origin not allowed"),
            "too_large" => (StatusCode::PAYLOAD_TOO_LARGE, "request too large"),
            "throttled" => (StatusCode::TOO_MANY_REQUESTS, "too many failed attempts; wait a minute"),
            "rate_limited" => (StatusCode::TOO_MANY_REQUESTS, "slow down"),
            "body_timeout" => (StatusCode::REQUEST_TIMEOUT, "the request body did not arrive in time"),
            _ => (StatusCode::UNAUTHORIZED, "unauthorized"),
        };
        let mut r = plain(code, text);
        if code == StatusCode::UNAUTHORIZED {
            r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer realm=\"pawl\""));
        }
        r
    }

    /// Replace the accepted callers. A removed or rotated token stops working at once, and that
    /// caller is revoked (its open streams and waits end, see [`Self::revocation`]).
    pub fn set_callers(&self, callers: Vec<CallerAuth>) {
        let old = std::mem::replace(&mut *self.callers.write().unwrap_or_else(|p| p.into_inner()), callers.clone());
        let revs = self.revocations.lock().unwrap_or_else(|p| p.into_inner());
        for o in &old {
            let same = callers.iter().any(|c| c.id == o.id && bool::from(c.token.as_bytes().ct_eq(o.token.as_bytes())));
            if !same {
                if let Some(tx) = revs.get(&o.id) {
                    tx.send_modify(|n| *n += 1);
                }
            }
        }
    }

    /// Changes when caller `id` is revoked (token rotated or caller removed) from now on.
    pub fn revocation(&self, id: &str) -> tokio::sync::watch::Receiver<u64> {
        let mut revs = self.revocations.lock().unwrap_or_else(|p| p.into_inner());
        revs.entry(id.to_string()).or_insert_with(|| tokio::sync::watch::channel(0).0).subscribe()
    }

    pub fn port(&self) -> u16 {
        self.port.load(Ordering::SeqCst)
    }

    fn limits(&self) -> std::sync::MutexGuard<'_, Limits> {
        self.limits.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn public_ok(&self) -> bool {
        self.limits().public.take(PUBLIC_RATE_PER_SEC, PUBLIC_RATE_PER_SEC * 2.0)
    }

    fn caller_ok(&self, id: &str) -> bool {
        let burst = CALLER_RATE_PER_SEC * 2.0;
        self.limits().callers.entry(id.to_string()).or_insert_with(|| Bucket::new(burst)).take(CALLER_RATE_PER_SEC, burst)
    }

    fn throttled(&self) -> bool {
        let mut l = self.limits();
        if l.failures.0.elapsed() >= Duration::from_secs(60) {
            l.failures = (Instant::now(), 0);
        }
        l.failures.1 >= AUTH_FAILURES_PER_MIN
    }

    fn failed(&self) {
        let mut l = self.limits();
        l.failures.1 = l.failures.1.saturating_add(1);
    }

    /// The caller whose token `headers` carry. Every token is compared (constant time each).
    fn authenticate(&self, headers: &HeaderMap) -> Result<String, AuthFail> {
        if self.throttled() {
            return Err(AuthFail::Throttled);
        }
        let given = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
            .map(str::trim)
            .filter(|t| !t.is_empty());
        let Some(given) = given else {
            self.failed();
            return Err(AuthFail::Missing);
        };
        let callers = self.callers.read().unwrap_or_else(|p| p.into_inner());
        let mut found = None;
        for c in callers.iter().filter(|c| !c.token.is_empty()) {
            if bool::from(c.token.as_bytes().ct_eq(given.as_bytes())) && found.is_none() {
                found = Some(c.id.clone());
            }
        }
        drop(callers);
        found.ok_or_else(|| {
            self.failed();
            AuthFail::Wrong
        })
    }
}

/// The caller id the guard put in the request (handlers only).
pub fn caller_of(params: &ServiceParams) -> Option<&str> {
    params.get(CALLER_HEADER).and_then(|v| v.first()).map(String::as_str)
}

/// The `A2A-Version` a request asked for: the header, else the `A2A-Version` query parameter
/// (spec 3.6.1). None = not given (0.3).
pub fn requested_version(headers: &HeaderMap, query: Option<&str>) -> Option<String> {
    if let Some(v) = headers.get("a2a-version").and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty()) {
        return Some(v.to_string());
    }
    query?.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k.eq_ignore_ascii_case("A2A-Version") && !v.is_empty()).then(|| v.to_string())
    })
}

fn host_parts(v: &str) -> Option<(String, Option<u16>)> {
    let v = v.trim();
    if let Some(rest) = v.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p.parse().ok()?),
            None if after.is_empty() => None,
            None => return None,
        };
        return Some((host.to_ascii_lowercase(), port));
    }
    match v.rsplit_once(':') {
        Some((h, p)) => Some((h.to_ascii_lowercase(), Some(p.parse().ok()?))),
        None => Some((v.to_ascii_lowercase(), None)),
    }
}

fn loopback_name(h: &str) -> bool {
    matches!(h, "127.0.0.1" | "localhost" | "::1")
}

/// DNS-rebinding defence: `Host` must be a loopback name with this server's port.
pub fn host_ok(headers: &HeaderMap, port: u16) -> bool {
    let Some(v) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else { return false };
    matches!(host_parts(v), Some((h, Some(p))) if loopback_name(&h) && p == port)
}

/// An `Origin` header, if present, must be this server's own origin.
pub fn origin_ok(headers: &HeaderMap, port: u16) -> bool {
    let Some(v) = headers.get(header::ORIGIN) else { return true };
    let Ok(v) = v.to_str() else { return false };
    let Some(rest) = v.trim().strip_prefix("http://") else { return false };
    matches!(host_parts(rest), Some((h, Some(p))) if loopback_name(&h) && p == port)
}

fn plain(status: StatusCode, text: &'static str) -> Response {
    (status, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response()
}

async fn guard(State(st): State<Arc<ServerState>>, mut req: Request, next: Next) -> Response {
    let port = st.port();
    if !host_ok(req.headers(), port) {
        return st.refuse(None, "forbidden_host", &req);
    }
    if !origin_ok(req.headers(), port) || req.method() == Method::OPTIONS {
        return st.refuse(None, "forbidden_origin", &req);
    }
    // Only the guard says who is calling.
    req.headers_mut().remove(CALLER_HEADER);
    let too_big = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|n| n > BODY_MAX_BYTES);
    if too_big {
        return st.refuse(None, "too_large", &req);
    }
    if req.method() == Method::GET && req.uri().path() == WELL_KNOWN_CARD {
        if !st.public_ok() {
            return st.refuse(None, "rate_limited", &req);
        }
        req.headers_mut().remove(header::AUTHORIZATION);
        return next.run(req).await;
    }
    // Who is calling, from the headers alone: nothing of the body is read for a stranger.
    let id = match st.authenticate(req.headers()) {
        Ok(id) => id,
        Err(AuthFail::Throttled) => return st.refuse(None, "throttled", &req),
        Err(AuthFail::Missing | AuthFail::Wrong) => return st.refuse(None, "unauthorized", &req),
    };
    if !st.caller_ok(&id) {
        return st.refuse(Some(&id), "rate_limited", &req);
    }
    req.headers_mut().remove(header::AUTHORIZATION);
    let Ok(v) = HeaderValue::from_str(&id) else { return st.refuse(None, "unauthorized", &req) };
    req.headers_mut().insert(CALLER_HEADER, v);
    // The body: capped for every route, and it must arrive in time.
    let (parts, body) = req.into_parts();
    let bytes = match tokio::time::timeout(BODY_READ_TIMEOUT, axum::body::to_bytes(body, BODY_MAX_BYTES)).await {
        Ok(Ok(b)) => b,
        Ok(Err(_)) => return st.refuse(Some(&id), "too_large", &Request::from_parts(parts, axum::body::Body::empty())),
        Err(_) => return st.refuse(Some(&id), "body_timeout", &Request::from_parts(parts, axum::body::Body::empty())),
    };
    next.run(Request::from_parts(parts, axum::body::Body::from(bytes))).await
}

/// FNV-1a of `bytes` as an ETag.
fn etag(bytes: &[u8]) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("\"{h:016x}\"")
}

async fn card_handler(State(st): State<Arc<ServerState>>, req: Request) -> Response {
    let version = requested_version(req.headers(), req.uri().query());
    let body = serde_json::to_vec(&card::card_json(&(st.card)(), version.as_deref())).unwrap_or_default();
    let tag = etag(&body);
    let not_modified = req.headers().get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()).is_some_and(|v| v.split(',').any(|t| t.trim() == tag));
    let mut r = if not_modified {
        StatusCode::NOT_MODIFIED.into_response()
    } else {
        (StatusCode::OK, [(header::CONTENT_TYPE, "application/json")], body).into_response()
    };
    let h = r.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=300"));
    h.insert(header::VARY, HeaderValue::from_static("A2A-Version"));
    if let Ok(v) = HeaderValue::from_str(&tag) {
        h.insert(header::ETAG, v);
    }
    r
}

pub(crate) fn service_params(headers: &HeaderMap) -> ServiceParams {
    let mut p = ServiceParams::new();
    for (name, value) in headers {
        if let Ok(v) = value.to_str() {
            p.entry(name.as_str().to_owned()).or_default().push(v.to_owned());
        }
    }
    p
}

/// An SSE response: one `data:` line of JSON per item, numbered ids, keep-alive comments.
pub(crate) fn sse(items: BoxStream<'static, serde_json::Value>) -> Response {
    let mut n = 0u64;
    let events = items.map(move |v| {
        n += 1;
        Ok::<_, std::convert::Infallible>(Event::default().id(n.to_string()).data(serde_json::to_string(&v).unwrap_or_default()))
    });
    Sse::new(events).keep_alive(KeepAlive::new().interval(SSE_KEEP_ALIVE)).into_response()
}

/// The whole server: card, JSON-RPC (1.0 + 0.3), REST (1.0), behind the guard.
pub fn router<H: A2aHandler>(handler: Arc<H>, state: Arc<ServerState>) -> Router {
    let card = Router::new().route(WELL_KNOWN_CARD, get(card_handler)).with_state(state.clone());
    Router::new()
        .merge(card)
        .merge(rpc::router(handler.clone()))
        .merge(rest::router(handler))
        .layer(middleware::from_fn_with_state(state, guard))
}

/// A running server.
pub struct RunningServer {
    pub addr: SocketAddr,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl RunningServer {
    /// Stop accepting, give open requests a moment, then drop what is left (SSE streams).
    pub async fn stop(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if tokio::time::timeout(Duration::from_secs(2), &mut self.task).await.is_err() {
            self.task.abort();
        }
    }
}

/// Bind `127.0.0.1:port` (0 = any free port) and serve `router(handler, state)`.
pub async fn serve<H: A2aHandler>(handler: Arc<H>, state: Arc<ServerState>, port: u16) -> std::io::Result<RunningServer> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let addr = listener.local_addr()?;
    state.port.store(addr.port(), Ordering::SeqCst);
    let app = router(handler, state);
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(accept_loop(listener, app, rx));
    Ok(RunningServer { addr, shutdown: Some(tx), task })
}

/// HTTP/1.1 connections with a header-read timeout, at most [`MAX_CONNECTIONS`] at once. On shutdown
/// open requests get a moment, then every connection (SSE streams too) is dropped.
async fn accept_loop(listener: tokio::net::TcpListener, app: Router, mut shutdown: tokio::sync::oneshot::Receiver<()>) {
    let slots = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    let mut conns = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let Ok(slot) = slots.clone().try_acquire_owned() else {
                    tracing::warn!("A2A server: {MAX_CONNECTIONS} connections open, closing a new one");
                    continue;
                };
                let service = hyper_util::service::TowerToHyperService::new(app.clone());
                conns.spawn(async move {
                    let _slot = slot;
                    let mut http = hyper::server::conn::http1::Builder::new();
                    http.timer(hyper_util::rt::TokioTimer::new()).header_read_timeout(HEADER_READ_TIMEOUT);
                    if let Err(e) = http.serve_connection(hyper_util::rt::TokioIo::new(stream), service).await {
                        tracing::debug!("A2A connection: {e}");
                    }
                });
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(2), async {
        while conns.join_next().await.is_some() {}
    })
    .await;
    conns.abort_all();
}

/// Tests: a router whose Host / Origin checks expect `port` (no socket).
#[doc(hidden)]
pub fn router_for_port<H: A2aHandler>(handler: Arc<H>, state: Arc<ServerState>, port: u16) -> Router {
    state.port.store(port, Ordering::SeqCst);
    router(handler, state)
}

#[cfg(test)]
mod conformance;
#[cfg(test)]
mod tests;
