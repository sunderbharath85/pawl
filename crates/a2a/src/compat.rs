//! A2A 0.3 <-> 1.0, at the JSON level.
//!
//! Spec 1.0.1 section 3.6: a request without an `A2A-Version` header is a 0.3 request, and agents
//! "MUST interpret empty value as 0.3 version"; Appendix A lists what changed. The official Rust SDK
//! speaks 1.0 only, so Pawl converts 0.3 JSON-RPC traffic here (server: 0.3 clients; client: 0.3
//! agents) and keeps one 1.0 code path behind it.
//!
//! What differs (spec 0.3.0 vs 1.0.1, Appendix A):
//! - JSON-RPC methods: `message/send` -> `SendMessage`, `message/stream` -> `SendStreamingMessage`,
//!   `tasks/get` -> `GetTask`, `tasks/cancel` -> `CancelTask`, `tasks/resubscribe` -> `SubscribeToTask`,
//!   `tasks/pushNotificationConfig/{set,get,list,delete}`, `agent/getAuthenticatedExtendedCard`.
//! - `kind` discriminators (`"task"`, `"message"`, `"status-update"`, `"artifact-update"`, part kinds
//!   `"text"` / `"file"` / `"data"`) became member names (`{"task": ..}`, `{"statusUpdate": ..}`,
//!   parts `{text}` / `{raw|url, filename, mediaType}` / `{data}`).
//! - Enums: roles `user` / `agent` -> `ROLE_USER` / `ROLE_AGENT`; states `input-required` ->
//!   `TASK_STATE_INPUT_REQUIRED` and so on (`unknown` <-> `TASK_STATE_UNSPECIFIED`).
//! - `SendMessage` results: 0.3 returns the Task or Message itself; 1.0 wraps it (`{"task": ..}`).
//! - Configuration: 0.3 `blocking` (default true in the reference SDKs) <-> 1.0 `returnImmediately`;
//!   `pushNotificationConfig` <-> `taskPushNotificationConfig`.
//! - Status updates: 0.3 has `final: true` on the last one.
//! - Agent Card: 0.3 `url` + `preferredTransport` + `additionalInterfaces` + `protocolVersion` <-> 1.0
//!   `supportedInterfaces`; `supportsAuthenticatedExtendedCard` <-> `capabilities.extendedAgentCard`;
//!   OpenAPI-style `securitySchemes` / `security` <-> 1.0 tagged schemes / `securityRequirements`.

use a2a::jsonrpc::methods as m;
use serde_json::{json, Map, Value};

/// A 0.3 JSON-RPC method name -> the 1.0 method.
pub fn method_from_v03(method: &str) -> Option<&'static str> {
    Some(match method {
        "message/send" => m::SEND_MESSAGE,
        "message/stream" => m::SEND_STREAMING_MESSAGE,
        "tasks/get" => m::GET_TASK,
        "tasks/list" => m::LIST_TASKS,
        "tasks/cancel" => m::CANCEL_TASK,
        "tasks/resubscribe" => m::SUBSCRIBE_TO_TASK,
        "tasks/pushNotificationConfig/set" => m::CREATE_PUSH_CONFIG,
        "tasks/pushNotificationConfig/get" => m::GET_PUSH_CONFIG,
        "tasks/pushNotificationConfig/list" => m::LIST_PUSH_CONFIGS,
        "tasks/pushNotificationConfig/delete" => m::DELETE_PUSH_CONFIG,
        "agent/getAuthenticatedExtendedCard" => m::GET_EXTENDED_AGENT_CARD,
        _ => return None,
    })
}

/// A 1.0 method -> its 0.3 JSON-RPC name.
pub fn method_to_v03(method: &str) -> Option<&'static str> {
    Some(match method {
        m::SEND_MESSAGE => "message/send",
        m::SEND_STREAMING_MESSAGE => "message/stream",
        m::GET_TASK => "tasks/get",
        m::LIST_TASKS => "tasks/list",
        m::CANCEL_TASK => "tasks/cancel",
        m::SUBSCRIBE_TO_TASK => "tasks/resubscribe",
        m::CREATE_PUSH_CONFIG => "tasks/pushNotificationConfig/set",
        m::GET_PUSH_CONFIG => "tasks/pushNotificationConfig/get",
        m::LIST_PUSH_CONFIGS => "tasks/pushNotificationConfig/list",
        m::DELETE_PUSH_CONFIG => "tasks/pushNotificationConfig/delete",
        m::GET_EXTENDED_AGENT_CARD => "agent/getAuthenticatedExtendedCard",
        _ => return None,
    })
}

const STATES: &[(&str, &str)] = &[
    ("submitted", "TASK_STATE_SUBMITTED"),
    ("working", "TASK_STATE_WORKING"),
    ("input-required", "TASK_STATE_INPUT_REQUIRED"),
    ("completed", "TASK_STATE_COMPLETED"),
    ("canceled", "TASK_STATE_CANCELED"),
    ("failed", "TASK_STATE_FAILED"),
    ("rejected", "TASK_STATE_REJECTED"),
    ("auth-required", "TASK_STATE_AUTH_REQUIRED"),
    ("unknown", "TASK_STATE_UNSPECIFIED"),
];

pub fn state_from_v03(s: &str) -> &'static str {
    STATES.iter().find(|(old, _)| *old == s).map(|(_, new)| *new).unwrap_or("TASK_STATE_UNSPECIFIED")
}

pub fn state_to_v03(s: &str) -> &'static str {
    STATES.iter().find(|(_, new)| *new == s).map(|(old, _)| *old).unwrap_or("unknown")
}

fn role_from_v03(r: &str) -> &'static str {
    match r {
        "user" => "ROLE_USER",
        "agent" => "ROLE_AGENT",
        _ => "ROLE_UNSPECIFIED",
    }
}

fn role_to_v03(r: &str) -> &'static str {
    match r {
        "ROLE_AGENT" => "agent",
        _ => "user",
    }
}

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(o) => o,
        _ => Map::new(),
    }
}

/// Copy `keys` from `from` to `to` when present.
fn copy(from: &Map<String, Value>, to: &mut Map<String, Value>, keys: &[&str]) {
    for k in keys {
        if let Some(v) = from.get(*k) {
            to.insert((*k).to_string(), v.clone());
        }
    }
}

fn map_array(v: Option<&Value>, f: fn(Value) -> Value) -> Option<Value> {
    v.and_then(Value::as_array).map(|a| Value::Array(a.iter().cloned().map(f).collect()))
}

pub fn part_from_v03(v: Value) -> Value {
    let p = obj(v);
    let mut out = Map::new();
    match p.get("kind").and_then(Value::as_str) {
        Some("file") => {
            let f = p.get("file").cloned().map(obj).unwrap_or_default();
            if let Some(b) = f.get("bytes") {
                out.insert("raw".into(), b.clone());
            } else if let Some(u) = f.get("uri") {
                out.insert("url".into(), u.clone());
            }
            if let Some(n) = f.get("name") {
                out.insert("filename".into(), n.clone());
            }
            if let Some(mt) = f.get("mimeType") {
                out.insert("mediaType".into(), mt.clone());
            }
        }
        Some("data") => copy(&p, &mut out, &["data"]),
        _ => copy(&p, &mut out, &["text"]),
    }
    copy(&p, &mut out, &["metadata"]);
    Value::Object(out)
}

pub fn part_to_v03(v: Value) -> Value {
    let p = obj(v);
    let mut out = Map::new();
    if p.contains_key("raw") || p.contains_key("url") {
        out.insert("kind".into(), json!("file"));
        let mut f = Map::new();
        if let Some(b) = p.get("raw") {
            f.insert("bytes".into(), b.clone());
        }
        if let Some(u) = p.get("url") {
            f.insert("uri".into(), u.clone());
        }
        if let Some(n) = p.get("filename") {
            f.insert("name".into(), n.clone());
        }
        if let Some(mt) = p.get("mediaType") {
            f.insert("mimeType".into(), mt.clone());
        }
        out.insert("file".into(), Value::Object(f));
    } else if p.contains_key("data") {
        out.insert("kind".into(), json!("data"));
        copy(&p, &mut out, &["data"]);
    } else {
        out.insert("kind".into(), json!("text"));
        out.insert("text".into(), p.get("text").cloned().unwrap_or(json!("")));
    }
    copy(&p, &mut out, &["metadata"]);
    Value::Object(out)
}

const MESSAGE_KEYS: &[&str] = &["messageId", "contextId", "taskId", "metadata", "extensions", "referenceTaskIds"];

pub fn message_from_v03(v: Value) -> Value {
    let msg = obj(v);
    let mut out = Map::new();
    copy(&msg, &mut out, MESSAGE_KEYS);
    out.insert("role".into(), json!(role_from_v03(msg.get("role").and_then(Value::as_str).unwrap_or(""))));
    out.insert("parts".into(), map_array(msg.get("parts"), part_from_v03).unwrap_or(json!([])));
    Value::Object(out)
}

pub fn message_to_v03(v: Value) -> Value {
    let msg = obj(v);
    let mut out = Map::new();
    out.insert("kind".into(), json!("message"));
    copy(&msg, &mut out, MESSAGE_KEYS);
    out.insert("role".into(), json!(role_to_v03(msg.get("role").and_then(Value::as_str).unwrap_or(""))));
    out.insert("parts".into(), map_array(msg.get("parts"), part_to_v03).unwrap_or(json!([])));
    Value::Object(out)
}

fn status_from_v03(v: Value) -> Value {
    let s = obj(v);
    let mut out = Map::new();
    out.insert("state".into(), json!(state_from_v03(s.get("state").and_then(Value::as_str).unwrap_or(""))));
    if let Some(msg) = s.get("message").filter(|m| !m.is_null()) {
        out.insert("message".into(), message_from_v03(msg.clone()));
    }
    copy(&s, &mut out, &["timestamp"]);
    Value::Object(out)
}

fn status_to_v03(v: Value) -> Value {
    let s = obj(v);
    let mut out = Map::new();
    out.insert("state".into(), json!(state_to_v03(s.get("state").and_then(Value::as_str).unwrap_or(""))));
    if let Some(msg) = s.get("message").filter(|m| !m.is_null()) {
        out.insert("message".into(), message_to_v03(msg.clone()));
    }
    copy(&s, &mut out, &["timestamp"]);
    Value::Object(out)
}

const ARTIFACT_KEYS: &[&str] = &["artifactId", "name", "description", "metadata", "extensions"];

fn artifact_from_v03(v: Value) -> Value {
    let a = obj(v);
    let mut out = Map::new();
    copy(&a, &mut out, ARTIFACT_KEYS);
    out.insert("parts".into(), map_array(a.get("parts"), part_from_v03).unwrap_or(json!([])));
    Value::Object(out)
}

fn artifact_to_v03(v: Value) -> Value {
    let a = obj(v);
    let mut out = Map::new();
    copy(&a, &mut out, ARTIFACT_KEYS);
    out.insert("parts".into(), map_array(a.get("parts"), part_to_v03).unwrap_or(json!([])));
    Value::Object(out)
}

pub fn task_from_v03(v: Value) -> Value {
    let t = obj(v);
    let mut out = Map::new();
    copy(&t, &mut out, &["id", "contextId", "metadata"]);
    out.insert("status".into(), status_from_v03(t.get("status").cloned().unwrap_or(json!({}))));
    if let Some(a) = map_array(t.get("artifacts"), artifact_from_v03) {
        out.insert("artifacts".into(), a);
    }
    if let Some(h) = map_array(t.get("history"), message_from_v03) {
        out.insert("history".into(), h);
    }
    Value::Object(out)
}

pub fn task_to_v03(v: Value) -> Value {
    let t = obj(v);
    let mut out = Map::new();
    out.insert("kind".into(), json!("task"));
    copy(&t, &mut out, &["id", "contextId", "metadata"]);
    out.insert("status".into(), status_to_v03(t.get("status").cloned().unwrap_or(json!({}))));
    if let Some(a) = map_array(t.get("artifacts"), artifact_to_v03) {
        out.insert("artifacts".into(), a);
    }
    if let Some(h) = map_array(t.get("history"), message_to_v03) {
        out.insert("history".into(), h);
    }
    Value::Object(out)
}

/// 0.3 `MessageSendParams` / other params -> 1.0 request params for `method` (a 1.0 name).
pub fn params_from_v03(method: &str, params: Value) -> Value {
    match method {
        m::SEND_MESSAGE | m::SEND_STREAMING_MESSAGE => {
            let p = obj(params);
            let mut out = Map::new();
            if let Some(msg) = p.get("message") {
                out.insert("message".into(), message_from_v03(msg.clone()));
            }
            if let Some(c) = p.get("configuration").filter(|c| c.is_object()) {
                let c = obj(c.clone());
                let mut cfg = Map::new();
                copy(&c, &mut cfg, &["acceptedOutputModes", "historyLength"]);
                if let Some(pn) = c.get("pushNotificationConfig") {
                    cfg.insert("taskPushNotificationConfig".into(), pn.clone());
                }
                // 0.3 `blocking` defaults to true (reference SDKs); 1.0 blocks unless told not to.
                if c.get("blocking").and_then(Value::as_bool) == Some(false) {
                    cfg.insert("returnImmediately".into(), json!(true));
                }
                out.insert("configuration".into(), Value::Object(cfg));
            }
            copy(&p, &mut out, &["metadata"]);
            Value::Object(out)
        }
        _ => params,
    }
}

/// 1.0 request params -> 0.3 params for `method` (a 1.0 name).
pub fn params_to_v03(method: &str, params: Value) -> Value {
    match method {
        m::SEND_MESSAGE | m::SEND_STREAMING_MESSAGE => {
            let p = obj(params);
            let mut out = Map::new();
            if let Some(msg) = p.get("message") {
                out.insert("message".into(), message_to_v03(msg.clone()));
            }
            let c = p.get("configuration").cloned().map(obj).unwrap_or_default();
            let mut cfg = Map::new();
            copy(&c, &mut cfg, &["acceptedOutputModes", "historyLength"]);
            if let Some(pn) = c.get("taskPushNotificationConfig") {
                cfg.insert("pushNotificationConfig".into(), pn.clone());
            }
            let immediate = c.get("returnImmediately").and_then(Value::as_bool).unwrap_or(false);
            cfg.insert("blocking".into(), json!(!immediate));
            out.insert("configuration".into(), Value::Object(cfg));
            copy(&p, &mut out, &["metadata"]);
            Value::Object(out)
        }
        _ => params,
    }
}

/// A 1.0 unary result -> the 0.3 result of `method` (a 1.0 name).
pub fn result_to_v03(method: &str, result: Value) -> Value {
    match method {
        m::SEND_MESSAGE => {
            let r = obj(result);
            if let Some(t) = r.get("task") {
                task_to_v03(t.clone())
            } else if let Some(msg) = r.get("message") {
                message_to_v03(msg.clone())
            } else {
                Value::Object(r)
            }
        }
        m::GET_TASK | m::CANCEL_TASK => task_to_v03(result),
        m::LIST_TASKS => {
            let mut r = obj(result);
            if let Some(tasks) = map_array(r.get("tasks"), task_to_v03) {
                r.insert("tasks".into(), tasks);
            }
            Value::Object(r)
        }
        m::GET_EXTENDED_AGENT_CARD => card_to_v03(result),
        _ => result,
    }
}

/// A 0.3 unary result -> the 1.0 result of `method` (a 1.0 name).
pub fn result_from_v03(method: &str, result: Value) -> Value {
    match method {
        m::SEND_MESSAGE => match result.get("kind").and_then(Value::as_str) {
            Some("message") => json!({ "message": message_from_v03(result) }),
            _ => json!({ "task": task_from_v03(result) }),
        },
        m::GET_TASK | m::CANCEL_TASK => task_from_v03(result),
        m::LIST_TASKS => {
            let mut r = obj(result);
            if let Some(tasks) = map_array(r.get("tasks"), task_from_v03) {
                r.insert("tasks".into(), tasks);
            }
            Value::Object(r)
        }
        m::GET_EXTENDED_AGENT_CARD => card_from_v03(result),
        _ => result,
    }
}

fn is_final_state(state: &str) -> bool {
    matches!(
        state,
        "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED" | "TASK_STATE_REJECTED" | "TASK_STATE_INPUT_REQUIRED" | "TASK_STATE_AUTH_REQUIRED"
    )
}

/// A 1.0 `StreamResponse` -> the 0.3 stream event.
pub fn stream_event_to_v03(v: Value) -> Value {
    let e = obj(v);
    if let Some(t) = e.get("task") {
        return task_to_v03(t.clone());
    }
    if let Some(msg) = e.get("message") {
        return message_to_v03(msg.clone());
    }
    if let Some(s) = e.get("statusUpdate") {
        let s = obj(s.clone());
        let state = s.get("status").and_then(|st| st.get("state")).and_then(Value::as_str).unwrap_or("").to_string();
        let mut out = Map::new();
        out.insert("kind".into(), json!("status-update"));
        copy(&s, &mut out, &["taskId", "contextId", "metadata"]);
        out.insert("status".into(), status_to_v03(s.get("status").cloned().unwrap_or(json!({}))));
        out.insert("final".into(), json!(is_final_state(&state)));
        return Value::Object(out);
    }
    if let Some(a) = e.get("artifactUpdate") {
        let a = obj(a.clone());
        let mut out = Map::new();
        out.insert("kind".into(), json!("artifact-update"));
        copy(&a, &mut out, &["taskId", "contextId", "append", "lastChunk", "metadata"]);
        out.insert("artifact".into(), artifact_to_v03(a.get("artifact").cloned().unwrap_or(json!({}))));
        return Value::Object(out);
    }
    Value::Object(e)
}

/// A 0.3 stream event -> the 1.0 `StreamResponse`.
pub fn stream_event_from_v03(v: Value) -> Value {
    match v.get("kind").and_then(Value::as_str) {
        Some("task") => json!({ "task": task_from_v03(v) }),
        Some("message") => json!({ "message": message_from_v03(v) }),
        Some("status-update") => {
            let s = obj(v);
            let mut out = Map::new();
            copy(&s, &mut out, &["taskId", "contextId", "metadata"]);
            out.insert("status".into(), status_from_v03(s.get("status").cloned().unwrap_or(json!({}))));
            json!({ "statusUpdate": Value::Object(out) })
        }
        Some("artifact-update") => {
            let a = obj(v);
            let mut out = Map::new();
            copy(&a, &mut out, &["taskId", "contextId", "append", "lastChunk", "metadata"]);
            out.insert("artifact".into(), artifact_from_v03(a.get("artifact").cloned().unwrap_or(json!({}))));
            json!({ "artifactUpdate": Value::Object(out) })
        }
        _ => v,
    }
}

/// Add the 0.3 card fields to a 1.0 Agent Card (1.0 readers ignore them, 0.3 readers find what they
/// need): `protocolVersion`, `url` + `preferredTransport` (the first 0.3-capable JSON-RPC interface,
/// else the first one), `additionalInterfaces`, `supportsAuthenticatedExtendedCard`, OpenAPI-style
/// `securitySchemes` and `security`.
pub fn card_to_v03(card: Value) -> Value {
    let mut c = obj(card);
    let ifaces: Vec<Map<String, Value>> =
        c.get("supportedInterfaces").and_then(Value::as_array).map(|a| a.iter().cloned().map(obj).collect()).unwrap_or_default();
    let main = ifaces
        .iter()
        .find(|i| i.get("protocolBinding").and_then(Value::as_str) == Some("JSONRPC"))
        .or_else(|| ifaces.first())
        .cloned();
    c.insert("protocolVersion".into(), json!("0.3.0"));
    if let Some(main) = &main {
        c.insert("url".into(), main.get("url").cloned().unwrap_or(json!("")));
        c.insert("preferredTransport".into(), main.get("protocolBinding").cloned().unwrap_or(json!("JSONRPC")));
    }
    let extra: Vec<Value> = ifaces
        .iter()
        .map(|i| json!({ "url": i.get("url").cloned().unwrap_or(json!("")), "transport": i.get("protocolBinding").cloned().unwrap_or(json!("")) }))
        .collect();
    c.insert("additionalInterfaces".into(), Value::Array(extra));
    let ext = c.get("capabilities").and_then(|x| x.get("extendedAgentCard")).and_then(Value::as_bool).unwrap_or(false);
    c.insert("supportsAuthenticatedExtendedCard".into(), json!(ext));
    // Schemes: 1.0 `{name: {httpAuthSecurityScheme: {scheme}}}` -> OpenAPI `{name: {type: "http", scheme}}`.
    if let Some(schemes) = c.get("securitySchemes").and_then(Value::as_object).cloned() {
        let mut legacy = Map::new();
        for (name, s) in schemes {
            if let Some(h) = s.get("httpAuthSecurityScheme") {
                let mut o = Map::new();
                o.insert("type".into(), json!("http"));
                if let Some(v) = h.get("scheme") {
                    o.insert("scheme".into(), v.clone());
                }
                if let Some(v) = h.get("bearerFormat") {
                    o.insert("bearerFormat".into(), v.clone());
                }
                legacy.insert(name, Value::Object(o));
            } else if let Some(k) = s.get("apiKeySecurityScheme") {
                legacy.insert(name, json!({ "type": "apiKey", "in": k.get("location").cloned().unwrap_or(json!("header")), "name": k.get("name").cloned().unwrap_or(json!("")) }));
            }
        }
        // 1.0 readers would choke on OpenAPI-shaped schemes under the same key, so the 0.3 shape goes
        // into `security` requirements only when a 0.3 reader asks (see `card_from_v03`); the scheme
        // table keeps its 1.0 shape and 0.3 readers that need it read `x-a2a-v03-securitySchemes`.
        c.insert("x-a2a-v03-securitySchemes".into(), Value::Object(legacy));
    }
    if let Some(reqs) = c.get("securityRequirements").and_then(Value::as_array).cloned() {
        let security: Vec<Value> = reqs
            .iter()
            .filter_map(|r| r.get("schemes").and_then(Value::as_object))
            .map(|s| {
                Value::Object(
                    s.iter()
                        .map(|(k, v)| (k.clone(), v.get("list").cloned().unwrap_or(json!([]))))
                        .collect::<Map<String, Value>>(),
                )
            })
            .collect();
        c.insert("security".into(), Value::Array(security));
    }
    Value::Object(c)
}

/// A card as an outside agent published it (0.3 or 1.0) -> the 1.0 shape. A 1.0 card is returned
/// as is. A 0.3 card's interfaces are marked `protocolVersion: "0.3"` so the client talks 0.3 to it.
pub fn card_from_v03(card: Value) -> Value {
    if card.get("supportedInterfaces").and_then(Value::as_array).is_some_and(|a| !a.is_empty()) {
        return card;
    }
    let mut c = obj(card);
    let version = "0.3";
    let mut ifaces = Vec::new();
    if let Some(url) = c.get("url").and_then(Value::as_str) {
        let binding = c.get("preferredTransport").and_then(Value::as_str).unwrap_or("JSONRPC");
        ifaces.push(json!({ "url": url, "protocolBinding": binding, "protocolVersion": version }));
    }
    for i in c.get("additionalInterfaces").and_then(Value::as_array).cloned().unwrap_or_default() {
        let (Some(url), Some(t)) = (i.get("url").and_then(Value::as_str), i.get("transport").and_then(Value::as_str)) else { continue };
        if !ifaces.iter().any(|x| x["url"] == url && x["protocolBinding"] == t) {
            ifaces.push(json!({ "url": url, "protocolBinding": t, "protocolVersion": version }));
        }
    }
    c.insert("supportedInterfaces".into(), Value::Array(ifaces));
    let ext = c.get("supportsAuthenticatedExtendedCard").and_then(Value::as_bool).unwrap_or(false);
    let caps = c.entry("capabilities").or_insert_with(|| json!({}));
    if let Some(o) = caps.as_object_mut() {
        o.insert("extendedAgentCard".into(), json!(ext));
    }
    // OpenAPI-style schemes -> 1.0 tagged schemes (http and apiKey; others are dropped).
    if let Some(schemes) = c.get("securitySchemes").and_then(Value::as_object).cloned() {
        let mut out = Map::new();
        for (name, s) in schemes {
            match s.get("type").and_then(Value::as_str) {
                Some("http") => {
                    out.insert(name, json!({ "httpAuthSecurityScheme": { "scheme": s.get("scheme").cloned().unwrap_or(json!("bearer")) } }));
                }
                Some("apiKey") => {
                    out.insert(
                        name,
                        json!({ "apiKeySecurityScheme": { "location": s.get("in").cloned().unwrap_or(json!("header")), "name": s.get("name").cloned().unwrap_or(json!("")) } }),
                    );
                }
                _ if s.as_object().is_some_and(|o| o.keys().any(|k| k.ends_with("SecurityScheme"))) => {
                    out.insert(name, s);
                }
                _ => {}
            }
        }
        c.insert("securitySchemes".into(), Value::Object(out));
    }
    if let Some(sec) = c.get("security").and_then(Value::as_array).cloned() {
        c.insert("securityRequirements".into(), Value::Array(sec));
    }
    for k in ["url", "preferredTransport", "additionalInterfaces", "protocolVersion", "supportsAuthenticatedExtendedCard", "security"] {
        c.remove(k);
    }
    c.entry("version").or_insert(json!(""));
    c.entry("capabilities").or_insert(json!({}));
    Value::Object(c)
}

#[cfg(test)]
mod tests;
