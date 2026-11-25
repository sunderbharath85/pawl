//! The localhost A2A server (PLAN.md "Pawl as an A2A agent", phase A2): transport and security.
//!
//! - Binds 127.0.0.1 only. Off unless the hub starts it (`[a2a] server = true`).
//! - JSON-RPC at `POST /` and HTTP+JSON (REST) under `/` (the SDK's routers, A2A 1.0), streaming
//!   over SSE, no push notifications. A JSON-RPC request without an `A2A-Version` header (or with
//!   `0.3`) is an A2A 0.3 request (spec 3.6.2) and goes through [`crate::compat`] to the same
//!   handler; other versions get `VersionNotSupported`.
//! - Every request: the `Host` header must name this loopback server (`127.0.0.1`, `localhost` or
//!   `[::1]` with the bound port): a DNS-rebinding page reaches the socket with its own host name and
//!   is refused. An `Origin` header must be this server's own origin (browsers send one; other
//!   clients do not). `OPTIONS` (CORS preflight) is refused, and no CORS headers are ever sent.
//! - Everything but the public Agent Card needs `Authorization: Bearer <caller token>`, compared in
//!   constant time with each caller's token. Failed attempts are rate limited for everyone (a
//!   guessing local process), calls per caller too. The caller's id reaches the handler in
//!   [`CALLER_HEADER`], which the guard always strips from the incoming request first; the
//!   `Authorization` header is removed before the handler sees the request. Tokens are never logged.
//! - Bodies over [`BODY_MAX_BYTES`] are refused.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use a2a::jsonrpc::methods as m;
use a2a::{
    A2AError, AgentCard, CancelTaskRequest, GetExtendedAgentCardRequest, GetTaskRequest, JsonRpcId, JsonRpcRequest, JsonRpcResponse,
    ListTasksRequest, SendMessageRequest, SubscribeToTaskRequest,
};
use a2a_server::{RequestHandler, ServiceParams};
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tower::ServiceExt;

use crate::card;
use crate::compat;

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
        })
    }

    /// Replace the accepted callers (a removed or rotated token stops working at once).
    pub fn set_callers(&self, callers: Vec<CallerAuth>) {
        *self.callers.write().unwrap_or_else(|p| p.into_inner()) = callers;
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
        return plain(StatusCode::FORBIDDEN, "host not allowed");
    }
    if !origin_ok(req.headers(), port) || req.method() == Method::OPTIONS {
        return plain(StatusCode::FORBIDDEN, "origin not allowed");
    }
    // Only the guard says who is calling.
    req.headers_mut().remove(CALLER_HEADER);
    // The body cap holds for every route (the SDK routers set a larger one of their own).
    let too_big = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|n| n > BODY_MAX_BYTES);
    if too_big {
        return plain(StatusCode::PAYLOAD_TOO_LARGE, "request too large");
    }
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, BODY_MAX_BYTES).await {
        Ok(b) => b,
        Err(_) => return plain(StatusCode::PAYLOAD_TOO_LARGE, "request too large"),
    };
    let mut req = Request::from_parts(parts, axum::body::Body::from(bytes));
    if req.method() == Method::GET && req.uri().path() == WELL_KNOWN_CARD {
        if !st.public_ok() {
            return plain(StatusCode::TOO_MANY_REQUESTS, "slow down");
        }
        req.headers_mut().remove(header::AUTHORIZATION);
        return next.run(req).await;
    }
    match st.authenticate(req.headers()) {
        Ok(id) => {
            if !st.caller_ok(&id) {
                return plain(StatusCode::TOO_MANY_REQUESTS, "slow down");
            }
            req.headers_mut().remove(header::AUTHORIZATION);
            match HeaderValue::from_str(&id) {
                Ok(v) => {
                    req.headers_mut().insert(CALLER_HEADER, v);
                }
                Err(_) => return plain(StatusCode::UNAUTHORIZED, "unauthorized"),
            }
            next.run(req).await
        }
        Err(AuthFail::Throttled) => plain(StatusCode::TOO_MANY_REQUESTS, "too many failed attempts; wait a minute"),
        Err(AuthFail::Missing | AuthFail::Wrong) => {
            let mut r = plain(StatusCode::UNAUTHORIZED, "unauthorized");
            r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer realm=\"pawl\""));
            r
        }
    }
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

struct Dispatch<H: RequestHandler> {
    handler: Arc<H>,
    jsonrpc: Router,
}

impl<H: RequestHandler> Clone for Dispatch<H> {
    fn clone(&self) -> Self {
        Self { handler: self.handler.clone(), jsonrpc: self.jsonrpc.clone() }
    }
}

fn service_params(headers: &HeaderMap) -> ServiceParams {
    let mut p = ServiceParams::new();
    for (name, value) in headers {
        if let Ok(v) = value.to_str() {
            p.entry(name.as_str().to_owned()).or_default().push(v.to_owned());
        }
    }
    p
}

fn rpc_error(id: JsonRpcId, e: A2AError) -> Response {
    (StatusCode::OK, Json(JsonRpcResponse::error(id, e.to_jsonrpc_error()))).into_response()
}

/// `POST /`: 1.x -> the SDK's JSON-RPC router; none or 0.3 -> the 0.3 path; else VersionNotSupported.
async fn jsonrpc_dispatch<H: RequestHandler>(State(d): State<Dispatch<H>>, mut req: Request) -> Response {
    let version = requested_version(req.headers(), req.uri().query());
    match version.as_deref() {
        None => {}
        Some(v) if v.trim() == "0.3" || v.trim().starts_with("0.3.") => {}
        Some(v) if card::major(v) == Some(1) => {
            if !req.headers().contains_key("a2a-version") {
                if let Ok(hv) = HeaderValue::from_str(v) {
                    req.headers_mut().insert("a2a-version", hv);
                }
            }
            return match d.jsonrpc.clone().oneshot(req).await {
                Ok(r) => r,
                Err(never) => match never {},
            };
        }
        Some(v) => return rpc_error(JsonRpcId::Null, A2AError::version_not_supported(v)),
    }
    let params = service_params(req.headers());
    let body = match axum::body::to_bytes(req.into_body(), BODY_MAX_BYTES).await {
        Ok(b) => b,
        Err(_) => return plain(StatusCode::PAYLOAD_TOO_LARGE, "request too large"),
    };
    legacy(&*d.handler, &params, &body).await
}

fn typed<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, A2AError> {
    serde_json::from_value(v).map_err(|e| A2AError::invalid_params(format!("invalid params: {e}")))
}

fn as_value<T: serde::Serialize>(v: &T) -> Result<Value, A2AError> {
    serde_json::to_value(v).map_err(|e| A2AError::internal(format!("serialize: {e}")))
}

/// One A2A 0.3 JSON-RPC request, answered in 0.3 shapes by the 1.0 handler.
async fn legacy<H: RequestHandler>(handler: &H, params: &ServiceParams, body: &Bytes) -> Response {
    let raw: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => return rpc_error(JsonRpcId::Null, A2AError::parse_error(format!("parse error: {e}"))),
    };
    let id: JsonRpcId = raw.get("id").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or(JsonRpcId::Null);
    let request: JsonRpcRequest = match serde_json::from_value(raw) {
        Ok(r) => r,
        Err(e) => return rpc_error(id, A2AError::invalid_request(format!("invalid request: {e}"))),
    };
    if request.jsonrpc != "2.0" {
        return rpc_error(id, A2AError::invalid_request("invalid jsonrpc version"));
    }
    let Some(method) = compat::method_from_v03(&request.method) else {
        return rpc_error(id, A2AError::method_not_found(&request.method));
    };
    let p = compat::params_from_v03(method, request.params.unwrap_or(Value::Null));
    let p = if p.is_null() { json!({}) } else { p };
    if m::is_streaming(method) {
        let stream: Result<BoxStream<'static, Result<a2a::StreamResponse, A2AError>>, A2AError> = match method {
            m::SEND_STREAMING_MESSAGE => match typed::<SendMessageRequest>(p) {
                Ok(r) => handler.send_streaming_message(params, r).await,
                Err(e) => Err(e),
            },
            _ => match typed::<SubscribeToTaskRequest>(p) {
                Ok(r) => handler.subscribe_to_task(params, r).await,
                Err(e) => Err(e),
            },
        };
        return match stream {
            Ok(s) => {
                let s = s.map(|item| item.and_then(|ev| as_value(&ev)).map(compat::stream_event_to_v03)).boxed();
                a2a_server::sse::sse_jsonrpc_stream(id, s).into_response()
            }
            Err(e) => rpc_error(id, e),
        };
    }
    let result: Result<Value, A2AError> = match method {
        m::SEND_MESSAGE => match typed::<SendMessageRequest>(p) {
            Ok(r) => handler.send_message(params, r).await.and_then(|v| as_value(&v)),
            Err(e) => Err(e),
        },
        m::GET_TASK => match typed::<GetTaskRequest>(p) {
            Ok(r) => handler.get_task(params, r).await.and_then(|v| as_value(&v)),
            Err(e) => Err(e),
        },
        m::LIST_TASKS => match typed::<ListTasksRequest>(p) {
            Ok(r) => handler.list_tasks(params, r).await.and_then(|v| as_value(&v)),
            Err(e) => Err(e),
        },
        m::CANCEL_TASK => match typed::<CancelTaskRequest>(p) {
            Ok(r) => handler.cancel_task(params, r).await.and_then(|v| as_value(&v)),
            Err(e) => Err(e),
        },
        m::GET_EXTENDED_AGENT_CARD => match typed::<GetExtendedAgentCardRequest>(p) {
            Ok(r) => handler.get_extended_agent_card(params, r).await.and_then(|v| as_value(&v)),
            Err(e) => Err(e),
        },
        _ => Err(A2AError::push_notification_not_supported()),
    };
    match result {
        Ok(v) => Json(JsonRpcResponse::success(id, compat::result_to_v03(method, v))).into_response(),
        Err(e) => rpc_error(id, e),
    }
}

/// The whole server: card, JSON-RPC (1.0 + 0.3), REST (1.0), behind the guard.
pub fn router<H: RequestHandler>(handler: Arc<H>, state: Arc<ServerState>) -> Router {
    let dispatch = Dispatch { handler: handler.clone(), jsonrpc: a2a_server::jsonrpc::jsonrpc_router(handler.clone()) };
    let rpc = Router::new().route("/", post(jsonrpc_dispatch::<H>)).with_state(dispatch);
    let card = Router::new().route(WELL_KNOWN_CARD, get(card_handler)).with_state(state.clone());
    Router::new()
        .merge(card)
        .merge(rpc)
        .merge(a2a_server::rest::rest_router(handler))
        .layer(axum::extract::DefaultBodyLimit::max(BODY_MAX_BYTES))
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
pub async fn serve<H: RequestHandler>(handler: Arc<H>, state: Arc<ServerState>, port: u16) -> std::io::Result<RunningServer> {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let addr = listener.local_addr()?;
    state.port.store(addr.port(), Ordering::SeqCst);
    let app = router(handler, state);
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let task = tokio::spawn(async move {
        let shutdown = async move {
            let _ = rx.await;
        };
        if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown).await {
            tracing::warn!("A2A server stopped: {e}");
        }
    });
    Ok(RunningServer { addr, shutdown: Some(tx), task })
}

/// Tests: a router whose Host / Origin checks expect `port` (no socket).
#[doc(hidden)]
pub fn router_for_port<H: RequestHandler>(handler: Arc<H>, state: Arc<ServerState>, port: u16) -> Router {
    state.port.store(port, Ordering::SeqCst);
    router(handler, state)
}

#[cfg(test)]
mod tests;
