//! The HTTP+JSON (REST) binding (spec section 11), A2A 1.0.
//!
//! | Route                                              | Operation                         |
//! |----------------------------------------------------|-----------------------------------|
//! | `POST /message:send`                               | SendMessage                       |
//! | `POST /message:stream`                             | SendStreamingMessage (SSE)        |
//! | `GET /tasks/{id}?historyLength=`                   | GetTask                           |
//! | `GET /tasks?contextId=&status=&pageSize=&...`      | ListTasks                         |
//! | `POST /tasks/{id}:cancel`                          | CancelTask                        |
//! | `POST /tasks/{id}:subscribe`                       | SubscribeToTask (SSE)             |
//! | `/tasks/{id}/pushNotificationConfigs[/{configId}]` | PushNotificationNotSupported      |
//! | `GET /extendedAgentCard`                           | GetExtendedAgentCard              |
//!
//! Bodies and answers are `application/a2a+json`. Errors are the `google.rpc.Status` JSON of spec
//! 11.6: `{"error": {"code": <HTTP status>, "status": "NOT_FOUND", "message", "details": [ErrorInfo]}}`.
//! Task ids are Pawl global ids (`machine:uuid`), so `:cancel` / `:subscribe` are taken off the end of
//! the id segment. A request that names an `A2A-Version` other than 1.x gets `VersionNotSupported`.

use std::collections::HashMap;
use std::sync::Arc;

use a2a::{error_code, A2AError, CancelTaskRequest, GetExtendedAgentCardRequest, GetTaskRequest, ListTasksRequest, SendMessageRequest, SubscribeToTaskRequest, TaskState};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use futures::StreamExt;
use serde_json::{json, Value};

use super::{requested_version, service_params, sse, A2aHandler, EventStream};
use crate::card;

/// `application/a2a+json` (spec 14.1).
pub const A2A_JSON: &str = "application/a2a+json";

/// The gRPC status name of an A2A error (spec 5.4).
pub fn status_name(code: i32) -> &'static str {
    match code {
        error_code::TASK_NOT_FOUND => "NOT_FOUND",
        error_code::TASK_NOT_CANCELABLE
        | error_code::PUSH_NOTIFICATION_NOT_SUPPORTED
        | error_code::UNSUPPORTED_OPERATION
        | error_code::EXTENDED_CARD_NOT_CONFIGURED
        | error_code::EXTENSION_SUPPORT_REQUIRED
        | error_code::VERSION_NOT_SUPPORTED => "FAILED_PRECONDITION",
        error_code::CONTENT_TYPE_NOT_SUPPORTED | error_code::PARSE_ERROR | error_code::INVALID_REQUEST | error_code::INVALID_PARAMS => "INVALID_ARGUMENT",
        error_code::METHOD_NOT_FOUND => "UNIMPLEMENTED",
        _ => "INTERNAL",
    }
}

/// The `google.rpc.Status` body of `e` (spec 11.6).
pub fn error_body(e: &A2AError) -> Value {
    let details = e.to_jsonrpc_error().data.unwrap_or_else(|| json!([]));
    json!({ "error": { "code": e.http_status_code(), "status": status_name(e.code), "message": e.message, "details": details } })
}

fn reply(status: StatusCode, v: &Value) -> Response {
    (status, [(header::CONTENT_TYPE, A2A_JSON)], serde_json::to_vec(v).unwrap_or_default()).into_response()
}

fn fail(e: A2AError) -> Response {
    let status = StatusCode::from_u16(e.http_status_code()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    reply(status, &error_body(&e))
}

fn ok<T: serde::Serialize>(v: &T) -> Response {
    match serde_json::to_value(v) {
        Ok(v) => reply(StatusCode::OK, &v),
        Err(e) => fail(A2AError::internal(format!("serialize: {e}"))),
    }
}

fn stream(s: EventStream) -> Response {
    sse(s
        .map(|item| match item {
            Ok(ev) => serde_json::to_value(&ev).unwrap_or(Value::Null),
            Err(e) => error_body(&e),
        })
        .boxed())
}

/// REST is 1.0: a request may name no version or 1.x; else the error response.
fn version_error(headers: &HeaderMap, uri: &Uri) -> Option<Response> {
    match requested_version(headers, uri.query()) {
        None => None,
        Some(v) if card::major(&v) == Some(1) => None,
        Some(v) => Some(fail(A2AError::version_not_supported(&v))),
    }
}

fn body_of<T: serde::de::DeserializeOwned>(body: &Bytes) -> Result<T, A2AError> {
    let v: Value = if body.is_empty() { json!({}) } else { serde_json::from_slice(body).map_err(|e| A2AError::parse_error(format!("invalid JSON payload: {e}")))? };
    serde_json::from_value(v).map_err(|e| A2AError::invalid_params(format!("invalid body: {e}")))
}

fn int(q: &HashMap<String, String>, k: &str) -> Result<Option<i32>, A2AError> {
    q.get(k).filter(|v| !v.is_empty()).map(|v| v.parse::<i32>().map_err(|_| A2AError::invalid_params(format!("{k} must be an integer, got {v:?}")))).transpose()
}

/// ListTasks from query parameters (spec 11.5: camelCase names, enum names, ISO 8601 times).
fn list_request(q: &HashMap<String, String>) -> Result<ListTasksRequest, A2AError> {
    let status = match q.get("status").filter(|v| !v.is_empty()) {
        Some(s) => Some(serde_json::from_value::<TaskState>(json!(s)).map_err(|_| A2AError::invalid_params(format!("status: invalid value {s:?}")))?),
        None => None,
    };
    let after = match q.get("statusTimestampAfter").filter(|v| !v.is_empty()) {
        Some(t) => Some(
            chrono::DateTime::parse_from_rfc3339(t)
                .map_err(|_| A2AError::invalid_params(format!("statusTimestampAfter must be an ISO 8601 time, got {t:?}")))?
                .with_timezone(&chrono::Utc),
        ),
        None => None,
    };
    let include = match q.get("includeArtifacts").map(String::as_str) {
        None | Some("") => None,
        Some("true") => Some(true),
        Some("false") => Some(false),
        Some(v) => return Err(A2AError::invalid_params(format!("includeArtifacts must be true or false, got {v:?}"))),
    };
    Ok(ListTasksRequest {
        context_id: q.get("contextId").filter(|v| !v.is_empty()).cloned(),
        status,
        page_size: int(q, "pageSize")?,
        page_token: q.get("pageToken").filter(|v| !v.is_empty()).cloned(),
        history_length: int(q, "historyLength")?,
        status_timestamp_after: after,
        include_artifacts: include,
        tenant: None,
    })
}

async fn send<H: A2aHandler>(State(h): State<Arc<H>>, headers: HeaderMap, uri: Uri, body: Bytes) -> Response {
    if let Some(r) = version_error(&headers, &uri) {
        return r;
    }
    match body_of::<SendMessageRequest>(&body) {
        Ok(req) => match h.send_message(&service_params(&headers), req).await {
            Ok(v) => ok(&v),
            Err(e) => fail(e),
        },
        Err(e) => fail(e),
    }
}

async fn send_stream<H: A2aHandler>(State(h): State<Arc<H>>, headers: HeaderMap, uri: Uri, body: Bytes) -> Response {
    if let Some(r) = version_error(&headers, &uri) {
        return r;
    }
    match body_of::<SendMessageRequest>(&body) {
        Ok(req) => match h.send_streaming_message(&service_params(&headers), req).await {
            Ok(s) => stream(s),
            Err(e) => fail(e),
        },
        Err(e) => fail(e),
    }
}

async fn get_task<H: A2aHandler>(State(h): State<Arc<H>>, Path(id): Path<String>, Query(q): Query<HashMap<String, String>>, headers: HeaderMap, uri: Uri) -> Response {
    if let Some(r) = version_error(&headers, &uri) {
        return r;
    }
    let history_length = match int(&q, "historyLength") {
        Ok(n) => n,
        Err(e) => return fail(e),
    };
    match h.get_task(&service_params(&headers), GetTaskRequest { id, history_length, tenant: None }).await {
        Ok(t) => ok(&t),
        Err(e) => fail(e),
    }
}

async fn list<H: A2aHandler>(State(h): State<Arc<H>>, Query(q): Query<HashMap<String, String>>, headers: HeaderMap, uri: Uri) -> Response {
    if let Some(r) = version_error(&headers, &uri) {
        return r;
    }
    match list_request(&q) {
        Ok(req) => match h.list_tasks(&service_params(&headers), req).await {
            Ok(v) => ok(&v),
            Err(e) => fail(e),
        },
        Err(e) => fail(e),
    }
}

/// `POST /tasks/{id}:cancel` and `POST /tasks/{id}:subscribe`.
async fn task_action<H: A2aHandler>(State(h): State<Arc<H>>, Path(seg): Path<String>, headers: HeaderMap, uri: Uri, body: Bytes) -> Response {
    if let Some(r) = version_error(&headers, &uri) {
        return r;
    }
    let params = service_params(&headers);
    if let Some(id) = seg.strip_suffix(":cancel") {
        let metadata = match body_of::<Value>(&body) {
            Ok(v) => v.get("metadata").cloned().and_then(|m| serde_json::from_value(m).ok()),
            Err(e) => return fail(e),
        };
        return match h.cancel_task(&params, CancelTaskRequest { id: id.to_string(), metadata, tenant: None }).await {
            Ok(t) => ok(&t),
            Err(e) => fail(e),
        };
    }
    if let Some(id) = seg.strip_suffix(":subscribe") {
        return match h.subscribe_to_task(&params, SubscribeToTaskRequest { id: id.to_string(), tenant: None }).await {
            Ok(s) => stream(s),
            Err(e) => fail(e),
        };
    }
    fail(A2AError::method_not_found(&format!("POST /tasks/{seg}")))
}

async fn push(headers: HeaderMap, uri: Uri) -> Response {
    if let Some(r) = version_error(&headers, &uri) {
        return r;
    }
    fail(A2AError::push_notification_not_supported())
}

async fn extended_card<H: A2aHandler>(State(h): State<Arc<H>>, headers: HeaderMap, uri: Uri) -> Response {
    if let Some(r) = version_error(&headers, &uri) {
        return r;
    }
    match h.get_extended_agent_card(&service_params(&headers), GetExtendedAgentCardRequest { tenant: None }).await {
        Ok(c) => ok(&c),
        Err(e) => fail(e),
    }
}

/// The REST routes.
pub fn router<H: A2aHandler>(handler: Arc<H>) -> Router {
    Router::new()
        .route("/message:send", post(send::<H>))
        .route("/message:stream", post(send_stream::<H>))
        .route("/tasks", get(list::<H>))
        .route("/tasks/{id}", get(get_task::<H>).post(task_action::<H>))
        .route("/tasks/{id}/pushNotificationConfigs", get(push).post(push))
        .route("/tasks/{id}/pushNotificationConfigs/{config}", get(push).delete(push))
        .route("/extendedAgentCard", get(extended_card::<H>))
        .with_state(handler)
}
