//! HTTP/WebSocket server (axum 0.8), bound to 127.0.0.1 only.
//!
//! - `GET  /health`          → `{"ok":true,"version":"x","machineId":"..."}` (no auth; clients use
//!   machineId to check that a port from runtime.json is this data dir's hub)
//! - `GET  /ws`              → WebSocket; first frame must be `hello{token}` within 5 s
//! - `POST /hooks/{source}`  → `Authorization: Bearer <token>`, raw hook JSON body → `{"ok":true}`

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::ws::{close_code, CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use pawl_protocol::{ClientMsg, HookSource, ServerMsg, HUB_HOST};
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tokio::sync::{broadcast, mpsc};

use crate::core::{Actor, Hub};

pub const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const PING_EVERY: Duration = Duration::from_secs(30);
const MAX_WS_MESSAGE: usize = 8 * 1024 * 1024;
const MAX_HOOK_BODY: usize = 4 * 1024 * 1024;
const OUTBOX: usize = 1024;

/// Browser origins allowed to open `/ws` (non-browser clients send no Origin).
pub const ALLOWED_ORIGINS: &[&str] =
    &["tauri://localhost", "http://tauri.localhost", "https://tauri.localhost", "http://localhost:1420"];
pub const ALLOWED_ORIGIN_PREFIXES: &[&str] = &["vscode-webview://"];

static CONN_IDS: AtomicU64 = AtomicU64::new(1);

/// Absent or empty Origin is allowed; anything else must be on the allowlist. `null` is rejected.
pub fn origin_allowed(origin: Option<&HeaderValue>) -> bool {
    let Some(v) = origin else { return true };
    let Ok(s) = v.to_str() else { return false };
    let s = s.trim();
    if s.is_empty() {
        return true;
    }
    let lower = s.to_ascii_lowercase();
    ALLOWED_ORIGINS.contains(&lower.as_str())
        || ALLOWED_ORIGIN_PREFIXES.iter().any(|p| lower.starts_with(p) && lower.len() > p.len())
}

/// Constant-time token comparison.
pub fn token_matches(expected: &str, candidate: &str) -> bool {
    !expected.is_empty() && bool::from(expected.as_bytes().ct_eq(candidate.as_bytes()))
}

pub fn router(hub: Hub) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ws", get(ws_upgrade))
        .route("/hooks/{source}", post(hook))
        .layer(DefaultBodyLimit::max(MAX_HOOK_BODY))
        .with_state(hub)
}

/// Bind 127.0.0.1:`port` and serve until `shutdown` resolves.
pub async fn serve(hub: Hub, port: u16, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> Result<()> {
    let listener = bind(port, Duration::ZERO).await?;
    serve_on(hub, listener, shutdown).await
}

/// Bind 127.0.0.1:`port`, retrying for up to `retry_for` while it is still taken (a hub that
/// replaces itself with `restart_hub` waits here for the old process to let go of the port).
pub async fn bind(port: u16, retry_for: Duration) -> Result<tokio::net::TcpListener> {
    let addr: SocketAddr = format!("{HUB_HOST}:{port}").parse().context("hub address")?;
    let deadline = tokio::time::Instant::now() + retry_for;
    loop {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(l) => return Ok(l),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse && tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(e) => {
                return Err(anyhow::Error::new(e).context(format!("could not listen on {addr} (is another pawl already running?)")))
            }
        }
    }
}

/// Serve on a bound listener until `shutdown` resolves.
pub async fn serve_on(hub: Hub, listener: tokio::net::TcpListener, shutdown: impl std::future::Future<Output = ()> + Send + 'static) -> Result<()> {
    let addr = listener.local_addr().context("hub address")?;
    tracing::info!("pawl {} listening on http://{addr}", crate::HUB_VERSION);
    axum::serve(listener, router(hub)).with_graceful_shutdown(shutdown).await.context("server error")
}

async fn health(State(hub): State<Hub>) -> Json<Value> {
    Json(json!({ "ok": true, "version": crate::HUB_VERSION, "machineId": hub.machine_id() }))
}

// ---------------------------------------------------------------- hooks

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, token) = v.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

fn json_error(status: StatusCode, error: &str) -> Response {
    (status, Json(json!({ "ok": false, "error": error }))).into_response()
}

async fn hook(State(hub): State<Hub>, Path(source): Path<String>, headers: HeaderMap, body: Bytes) -> Response {
    if !origin_allowed(headers.get(header::ORIGIN)) {
        return json_error(StatusCode::FORBIDDEN, "origin not allowed");
    }
    if !bearer(&headers).is_some_and(|t| token_matches(hub.token(), t)) {
        return json_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let Ok(source) = serde_json::from_value::<HookSource>(Value::String(source.to_ascii_lowercase())) else {
        return json_error(StatusCode::NOT_FOUND, "unknown hook source");
    };
    let raw: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return json_error(StatusCode::BAD_REQUEST, &format!("body is not JSON: {e}")),
    };
    match hub.ingest_hook(source, raw).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => {
            tracing::warn!("hook ingest failed: {e:#}");
            json_error(StatusCode::INTERNAL_SERVER_ERROR, "hook could not be processed")
        }
    }
}

// ---------------------------------------------------------------- websocket

async fn ws_upgrade(State(hub): State<Hub>, headers: HeaderMap, ws: WebSocketUpgrade) -> Response {
    if !origin_allowed(headers.get(header::ORIGIN)) {
        tracing::warn!("rejected /ws from origin {:?}", headers.get(header::ORIGIN));
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    ws.max_message_size(MAX_WS_MESSAGE).on_upgrade(move |socket| ws_connection(hub, socket))
}

fn text(msg: &ServerMsg) -> Message {
    Message::Text(serde_json::to_string(msg).unwrap_or_else(|_| "{}".into()).into())
}

fn close(code: u16, reason: &str) -> Message {
    Message::Close(Some(CloseFrame { code, reason: reason.into() }))
}

/// Best-effort `id` from a frame that failed to parse, so the client still gets its ack.
fn salvage_id(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v.get("id").and_then(|i| i.as_str().map(str::to_string)))
        .unwrap_or_default()
}

enum First {
    Hello(String, pawl_protocol::ClientKind, String),
    Reject(Option<String>, &'static str),
    Gone,
}

async fn first_frame(socket: &mut futures::stream::SplitStream<WebSocket>) -> First {
    loop {
        let Some(Ok(frame)) = socket.next().await else { return First::Gone };
        match frame {
            Message::Text(t) => {
                return match serde_json::from_str::<ClientMsg>(t.as_str()) {
                    Ok(ClientMsg::Hello { id, client, token, .. }) => First::Hello(id, client, token),
                    Ok(other) => First::Reject(Some(other.id().to_string()), "expected hello"),
                    Err(_) => First::Reject(Some(salvage_id(t.as_str())), "expected hello"),
                };
            }
            Message::Binary(_) => return First::Reject(None, "expected hello"),
            Message::Close(_) => return First::Gone,
            Message::Ping(_) | Message::Pong(_) => continue,
        }
    }
}

async fn ws_connection(hub: Hub, socket: WebSocket) {
    let conn_id = CONN_IDS.fetch_add(1, Ordering::Relaxed);
    let (mut sink, mut stream) = socket.split();

    let (hello_id, client) = match tokio::time::timeout(HELLO_TIMEOUT, first_frame(&mut stream)).await {
        Err(_) => {
            let _ = sink.send(close(close_code::POLICY, "hello timeout")).await;
            return;
        }
        Ok(First::Gone) => return,
        Ok(First::Reject(id, why)) => {
            let _ = sink.send(text(&ServerMsg::ack_err(id.unwrap_or_default(), why))).await;
            let _ = sink.send(close(close_code::POLICY, why)).await;
            return;
        }
        Ok(First::Hello(id, client, token)) => {
            if !token_matches(hub.token(), &token) {
                tracing::warn!("ws #{conn_id}: bad token");
                let _ = sink.send(text(&ServerMsg::ack_err(id, "unauthorized"))).await;
                let _ = sink.send(close(close_code::POLICY, "unauthorized")).await;
                return;
            }
            (id, client)
        }
    };
    let actor = Actor::client(client, conn_id);
    tracing::info!("ws #{conn_id}: {} connected", actor.label);
    // Counted while connected (open_panel tells whether a desktop app got the request).
    let _connected = hub.client_connected(client);

    // Subscribe before taking the snapshot so nothing falls between the two.
    let mut bus = hub.subscribe();
    let welcome = match serde_json::to_value(hub.welcome()) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!("serialize welcome: {e}");
            return;
        }
    };
    if sink.send(text(&ServerMsg::ack_ok(hello_id, welcome))).await.is_err() {
        return;
    }
    for notification in hub.startup_notices() {
        if sink.send(text(&ServerMsg::Notification { notification })).await.is_err() {
            return;
        }
    }

    // One writer task owns the sink; everything else queues frames.
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(OUTBOX);
    let writer = tokio::spawn(async move {
        while let Some(m) = out_rx.recv().await {
            let closing = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || closing {
                break;
            }
        }
        let _ = sink.close().await;
    });

    let mut ping = tokio::time::interval(PING_EVERY);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ping.tick().await;

    loop {
        tokio::select! {
            frame = stream.next() => {
                let Some(Ok(frame)) = frame else { break };
                match frame {
                    Message::Text(t) => {
                        let reply_tx = out_tx.clone();
                        match serde_json::from_str::<ClientMsg>(t.as_str()) {
                            Ok(msg) => {
                                let (hub, actor) = (hub.clone(), actor.clone());
                                // Requests run concurrently (start_session can take a while).
                                tokio::spawn(async move {
                                    let ack = hub.handle(msg, &actor).await;
                                    let _ = reply_tx.send(text(&ack)).await;
                                });
                            }
                            Err(e) => {
                                let id = salvage_id(t.as_str());
                                let _ = reply_tx.send(text(&ServerMsg::ack_err(id, format!("invalid message: {e}")))).await;
                            }
                        }
                    }
                    Message::Close(_) => break,
                    Message::Binary(_) => {
                        let _ = out_tx.send(text(&ServerMsg::ack_err("", "binary frames are not supported"))).await;
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                }
            }
            msg = bus.recv() => match msg {
                Ok(m) => {
                    if out_tx.send(text(&m)).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // The client missed state; make it reconnect and resync from a fresh Welcome.
                    tracing::warn!("ws #{conn_id}: lagged by {n} messages; closing so it resyncs");
                    let _ = out_tx.send(close(close_code::AGAIN, "lagged; reconnect")).await;
                    break;
                }
                Err(broadcast::error::RecvError::Closed) => {
                    let _ = out_tx.send(close(close_code::AWAY, "hub shutting down")).await;
                    break;
                }
            },
            _ = ping.tick() => {
                if out_tx.send(Message::Ping(Bytes::from_static(b"pet"))).await.is_err() {
                    break;
                }
            }
        }
    }
    drop(out_tx);
    let _ = writer.await;
    tracing::info!("ws #{conn_id}: {} disconnected", actor.label);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hv(s: &str) -> HeaderValue {
        HeaderValue::from_str(s).unwrap()
    }

    #[test]
    fn origins() {
        assert!(origin_allowed(None));
        assert!(origin_allowed(Some(&hv(""))));
        assert!(origin_allowed(Some(&hv("tauri://localhost"))));
        assert!(origin_allowed(Some(&hv("http://tauri.localhost"))));
        assert!(origin_allowed(Some(&hv("https://tauri.localhost"))));
        assert!(origin_allowed(Some(&hv("http://localhost:1420"))));
        assert!(origin_allowed(Some(&hv("vscode-webview://1abc2def"))));
        assert!(!origin_allowed(Some(&hv("vscode-webview://"))));
        assert!(!origin_allowed(Some(&hv("null"))));
        assert!(!origin_allowed(Some(&hv("http://localhost:3000"))));
        assert!(!origin_allowed(Some(&hv("https://evil.example"))));
        assert!(!origin_allowed(Some(&hv("http://localhost:1420.evil.example"))));
    }

    #[test]
    fn tokens_and_bearer() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abc", "abd"));
        assert!(!token_matches("abc", "abcd"));
        assert!(!token_matches("", ""));
        let mut h = HeaderMap::new();
        h.insert(header::AUTHORIZATION, hv("Bearer  tok "));
        assert_eq!(bearer(&h), Some("tok"));
        h.insert(header::AUTHORIZATION, hv("Basic tok"));
        assert_eq!(bearer(&h), None);
    }

    #[test]
    fn salvage() {
        assert_eq!(salvage_id(r#"{"type":"nope","id":"7"}"#), "7");
        assert_eq!(salvage_id("garbage"), "");
    }
}
