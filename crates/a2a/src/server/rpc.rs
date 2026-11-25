//! The JSON-RPC binding (spec section 9) for A2A 1.0 and 0.3, over [`A2aHandler`].
//!
//! [`call`] turns one JSON-RPC request into one JSON-RPC response (or a stream of them, for
//! `SendStreamingMessage` / `SubscribeToTask`); the HTTP route sends that as JSON or SSE. The same
//! function serves other carriers of A2A JSON-RPC (the encrypted peer link, phase A4).
//!
//! 0.3 requests (`message/send`, `kind` shapes, ...) are converted to 1.0 by [`crate::compat`] on the
//! way in and the results back on the way out, so the handler only ever sees 1.0 types.

use std::sync::Arc;

use a2a::jsonrpc::methods as m;
use a2a::{
    A2AError, CancelTaskRequest, GetExtendedAgentCardRequest, GetTaskRequest, JsonRpcId, JsonRpcResponse, ListTasksRequest, SendMessageRequest,
    SubscribeToTaskRequest,
};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures::stream::BoxStream;
use futures::StreamExt;
use serde_json::{json, Value};

use super::{requested_version, service_params, sse, A2aHandler, ServiceParams};
use crate::{card, compat};

/// Which protocol version a request speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    V1,
    V03,
}

impl Wire {
    /// The wire of an `A2A-Version` value (None = not given = 0.3, spec 3.6.2); Err = unsupported.
    pub fn of(version: Option<&str>) -> Result<Wire, A2AError> {
        match version.map(str::trim) {
            None => Ok(Wire::V03),
            Some(v) if v == "0.3" || v.starts_with("0.3.") => Ok(Wire::V03),
            Some(v) if card::major(v) == Some(1) => Ok(Wire::V1),
            Some(v) => Err(A2AError::version_not_supported(v)),
        }
    }
}

/// One JSON-RPC answer: a response object, or a stream of response objects.
pub enum RpcReply {
    Unary(Value),
    Stream(BoxStream<'static, Value>),
}

fn error(id: &JsonRpcId, e: A2AError) -> Value {
    serde_json::to_value(JsonRpcResponse::error(id.clone(), e.to_jsonrpc_error())).unwrap_or(Value::Null)
}

fn success(id: &JsonRpcId, v: Value) -> Value {
    serde_json::to_value(JsonRpcResponse::success(id.clone(), v)).unwrap_or(Value::Null)
}

fn typed<T: serde::de::DeserializeOwned>(v: Value) -> Result<T, A2AError> {
    serde_json::from_value(v).map_err(|e| A2AError::invalid_params(format!("invalid params: {e}")))
}

fn as_value<T: serde::Serialize>(v: &T) -> Result<Value, A2AError> {
    serde_json::to_value(v).map_err(|e| A2AError::internal(format!("serialize: {e}")))
}

/// Is `method` a method of `wire`, and which 1.0 method is it?
fn method_of(method: &str, wire: Wire) -> Option<&'static str> {
    match wire {
        Wire::V03 => compat::method_from_v03(method),
        Wire::V1 => [
            m::SEND_MESSAGE,
            m::SEND_STREAMING_MESSAGE,
            m::GET_TASK,
            m::LIST_TASKS,
            m::CANCEL_TASK,
            m::SUBSCRIBE_TO_TASK,
            m::CREATE_PUSH_CONFIG,
            m::GET_PUSH_CONFIG,
            m::LIST_PUSH_CONFIGS,
            m::DELETE_PUSH_CONFIG,
            m::GET_EXTENDED_AGENT_CARD,
        ]
        .into_iter()
        .find(|x| *x == method),
    }
}

/// The request's 1.0 method and params, or the JSON-RPC error to answer.
fn parse(body: &[u8], wire: Wire) -> Result<(JsonRpcId, &'static str, Value), (JsonRpcId, A2AError)> {
    let raw: Value = serde_json::from_slice(body).map_err(|e| (JsonRpcId::Null, A2AError::parse_error(format!("invalid JSON payload: {e}"))))?;
    let Value::Object(obj) = raw else {
        return Err((JsonRpcId::Null, A2AError::invalid_request("the request must be a JSON object")));
    };
    let id = match obj.get("id") {
        Some(v) => serde_json::from_value::<JsonRpcId>(v.clone()).map_err(|_| (JsonRpcId::Null, A2AError::invalid_request("id must be a string, an integer or null")))?,
        None => return Err((JsonRpcId::Null, A2AError::invalid_request("id is required"))),
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err((id, A2AError::invalid_request("jsonrpc must be \"2.0\"")));
    }
    let Some(name) = obj.get("method").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
        return Err((id, A2AError::invalid_request("method is required")));
    };
    let Some(method) = method_of(name, wire) else {
        return Err((id, A2AError::method_not_found(name)));
    };
    let params = match obj.get("params") {
        None | Some(Value::Null) => json!({}),
        Some(p @ Value::Object(_)) => p.clone(),
        Some(_) => return Err((id, A2AError::invalid_params("params must be an object"))),
    };
    let params = match wire {
        Wire::V03 => compat::params_from_v03(method, params),
        Wire::V1 => params,
    };
    Ok((id, method, params))
}

/// Answer one JSON-RPC request (`body`) of `wire` with `handler`.
pub async fn call<H: A2aHandler + ?Sized>(handler: &H, params: &ServiceParams, body: &[u8], wire: Wire) -> RpcReply {
    let (id, method, p) = match parse(body, wire) {
        Ok(x) => x,
        Err((id, e)) => return RpcReply::Unary(error(&id, e)),
    };
    if m::is_streaming(method) {
        let stream = match method {
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
            Ok(s) => RpcReply::Stream(
                s.map(move |item| match item.and_then(|ev| as_value(&ev)) {
                    Ok(v) => success(&id, if wire == Wire::V03 { compat::stream_event_to_v03(v) } else { v }),
                    Err(e) => error(&id, e),
                })
                .boxed(),
            ),
            Err(e) => RpcReply::Unary(error(&id, e)),
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
        // Spec 3.3.4: push notifications are not a capability of this agent.
        _ => Err(A2AError::push_notification_not_supported()),
    };
    RpcReply::Unary(match result {
        Ok(v) => success(&id, if wire == Wire::V03 { compat::result_to_v03(method, v) } else { v }),
        Err(e) => error(&id, e),
    })
}

/// `POST /`.
pub fn router<H: A2aHandler>(handler: Arc<H>) -> Router {
    Router::new().route("/", post(handle::<H>)).with_state(handler)
}

async fn handle<H: A2aHandler>(State(h): State<Arc<H>>, headers: HeaderMap, uri: Uri, body: Bytes) -> Response {
    let version = requested_version(&headers, uri.query());
    let wire = match Wire::of(version.as_deref()) {
        Ok(w) => w,
        Err(e) => return Json(error(&JsonRpcId::Null, e)).into_response(),
    };
    match call(&*h, &service_params(&headers), &body, wire).await {
        RpcReply::Unary(v) => Json(v).into_response(),
        RpcReply::Stream(s) => sse(s),
    }
}
