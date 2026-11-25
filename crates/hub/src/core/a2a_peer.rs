//! Machine to machine A2A (PLAN.md "Between machines", phase A4, protocol v9).
//!
//! Orchestrators delegate to each other with A2A JSON-RPC (1.0) carried inside the end-to-end
//! encrypted peer link: a custom binding (spec 5.8 / 12, `A2A_PEER_BINDING`), no LAN port, no TLS
//! trust. One call is `PeerBody::A2a { call_id, frame }`: the origin sends a `request` frame (the
//! JSON-RPC request, the local client that asked, whether the user confirmed); the owner answers
//! with one `response` frame (or `event` frames and `end` for a stream).
//!
//! Between v9 hubs (`Presence.protocolVersion >= 9`) start / prompt / cancel go this way instead of
//! intents; approve / deny stay intents (the step-4 confirmed / allow-once rules). A v8 peer keeps
//! getting intents. The owner applies exactly the intent rules: `[peer] accept_intents`, the per-peer
//! rate limit, dedupe by call id, confirmed starts only in known folders with installed agents,
//! remote-driven sessions (rules never auto-approve), and every call is in the intent log on both
//! sides. The owner's refusals carry the intent reason (`pawlReason` in the JSON-RPC error's
//! ErrorInfo metadata), so clients see the same ack codes as before.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::MutexGuard;
#[cfg(not(test))]
use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use pawl_a2a::server::rpc::{self, RpcReply, Wire};
use pawl_a2a::server::{A2aHandler, EventStream, ServiceParams};
use pawl_a2a::spec::{
    error_code, A2AError, AgentCard, CancelTaskRequest, GetExtendedAgentCardRequest, GetTaskRequest, ListTasksRequest, ListTasksResponse, Message, Part,
    Role, SendMessageConfiguration, SendMessageRequest, SendMessageResponse, SubscribeToTaskRequest, Task, TypedDetail,
};
use pawl_a2a::{Caller, DelegateParams, DELEGATE_EXT};
use pawl_peer::SendError;
use pawl_protocol::{A2aPeerFrame, ClientKind, Intent, IntentRejectReason, PeerBody, A2A_PEER_MIN_PROTOCOL};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use super::a2a::{message_text, Answers};
use super::a2a_server::HubHandler;
use super::intents::{checked_result, kind_name, reason_code, reason_name, reason_of, target_of, validate_prompt};
use super::{client_label, AckCode, Actor, Hub};
use crate::store::{IntentLogEntry, IntentRole};

#[cfg(not(test))]
const CALL_TIMEOUT: Duration = Duration::from_millis(pawl_protocol::INTENT_TIMEOUT_MS as u64);
#[cfg(test)]
const CALL_TIMEOUT: std::time::Duration = super::intents::INTENT_TIMEOUT;

/// Replies up to this size are kept for repeated call ids; larger ones only as a marker.
pub(crate) const ANSWER_KEEP_MAX_BYTES: usize = 4 * 1024;
/// Remembered call ids per peer (oldest dropped first), and their bytes.
pub(crate) const ANSWERS_PER_PEER: usize = 256;
pub(crate) const ANSWER_BYTES_PER_PEER: usize = 256 * 1024;
/// Open streams one peer may have here at once.
pub(crate) const STREAMS_PER_PEER: usize = 4;

/// The ErrorInfo metadata key with the intent reason of a refusal.
pub const PAWL_REASON: &str = "pawlReason";

#[derive(Default)]
pub(crate) struct A2aPeerState {
    /// Origin: calls waiting for their answer: call id -> (owner, answer).
    waiting: HashMap<String, (String, oneshot::Sender<Value>)>,
    /// Owner: what each peer's calls were answered (per peer, so one peer cannot evict another's).
    answers: HashMap<String, PeerAnswers>,
    /// Owner: calls being handled.
    in_flight: HashSet<(String, String)>,
    /// Owner: open streams per peer.
    streams: HashMap<String, usize>,
}

/// What a call was answered.
#[derive(Clone)]
enum Answer {
    /// The reply itself (small).
    Reply(Value),
    /// Answered, the reply was too large to keep; or (`stream`) a stream was started.
    Marker(&'static str),
}

#[derive(Default)]
struct PeerAnswers {
    map: HashMap<String, (Answer, usize)>,
    order: VecDeque<String>,
    bytes: usize,
}

impl PeerAnswers {
    fn remember(&mut self, call_id: &str, answer: Answer) {
        let size = match &answer {
            Answer::Reply(v) => serde_json::to_vec(v).map(|b| b.len()).unwrap_or(0) + call_id.len(),
            Answer::Marker(_) => 16 + call_id.len(),
        };
        if let Some((_, old)) = self.map.insert(call_id.to_string(), (answer, size)) {
            self.bytes -= old;
        } else {
            self.order.push_back(call_id.to_string());
        }
        self.bytes += size;
        while self.order.len() > ANSWERS_PER_PEER || self.bytes > ANSWER_BYTES_PER_PEER {
            let Some(old) = self.order.pop_front() else { break };
            if let Some((_, n)) = self.map.remove(&old) {
                self.bytes -= n;
            }
        }
    }
}

/// Drops the origin's waiter for a call however `peer_call` ends (also when its future is dropped).
struct Waiter<'a> {
    hub: &'a Hub,
    call_id: &'a str,
}

impl Drop for Waiter<'_> {
    fn drop(&mut self) {
        self.hub.a2a_peer().waiting.remove(self.call_id);
    }
}

/// An A2A error that carries an intent reason.
fn refusal(reason: IntentRejectReason, message: impl Into<String>) -> A2AError {
    let code = match reason {
        IntentRejectReason::NotFound => error_code::TASK_NOT_FOUND,
        IntentRejectReason::Invalid => error_code::INVALID_PARAMS,
        _ => error_code::UNSUPPORTED_OPERATION,
    };
    let name = reason_name(reason);
    A2AError::new(code, message).with_details(vec![TypedDetail::error_info(name.clone(), "pawl", Some(HashMap::from([(PAWL_REASON.to_string(), name)])))])
}

fn refusal_of(e: &anyhow::Error) -> A2AError {
    refusal(reason_of(e), format!("{e:#}"))
}

/// The intent reason of a JSON-RPC error object (Pawl's `pawlReason`, else by code).
fn reason_in(err: &Value) -> IntentRejectReason {
    let tagged = err.get("data").and_then(Value::as_array).into_iter().flatten().find_map(|d| d.get("metadata")?.get(PAWL_REASON)?.as_str().map(str::to_string));
    match tagged.and_then(|r| serde_json::from_value::<IntentRejectReason>(json!(r)).ok()) {
        Some(r) => r,
        None => match err.get("code").and_then(Value::as_i64).map(|c| c as i32) {
            Some(error_code::TASK_NOT_FOUND) => IntentRejectReason::NotFound,
            _ => IntentRejectReason::Invalid,
        },
    }
}

impl Hub {
    fn a2a_peer(&self) -> MutexGuard<'_, A2aPeerState> {
        self.inner.a2a_peer.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Does paired hub `machine_id` speak A2A over the link (its latest presence says v9+)?
    pub(super) fn peer_speaks_a2a(&self, machine_id: &str) -> bool {
        self.peer_protocol(machine_id).is_some_and(|v| v >= A2A_PEER_MIN_PROTOCOL)
    }

    // ------------------------------------------------------------ origin

    /// One A2A call to the owner `target`: its JSON-RPC response, or why there is none
    /// (`offline`, `timeout`, `invalid` when the frame could not be sent).
    pub(super) async fn peer_call(&self, target: &str, call_id: &str, client: ClientKind, confirmed: bool, method: &str, params: Value) -> std::result::Result<Value, AckCode> {
        let rpc = json!({ "jsonrpc": "2.0", "id": call_id, "method": method, "params": params });
        let (tx, rx) = oneshot::channel();
        self.a2a_peer().waiting.insert(call_id.to_string(), (target.to_string(), tx));
        let _waiter = Waiter { hub: self, call_id };
        let body = PeerBody::A2a { call_id: call_id.to_string(), frame: A2aPeerFrame::Request { rpc, client, confirmed } };
        let outcome = match self.inner.peer.send(target, body) {
            Err(SendError::NotLinked) | Err(SendError::Backpressure) => Err(AckCode::Offline),
            Err(e) => {
                tracing::warn!(device = %target, "A2A call not sent: {e}");
                Err(AckCode::Invalid)
            }
            Ok(()) => match tokio::time::timeout(CALL_TIMEOUT, rx).await {
                Ok(Ok(v)) => Ok(v),
                _ => Err(AckCode::Timeout),
            },
        };
        outcome
    }

    /// The result of a JSON-RPC response, or the owner's refusal as an intent reason.
    fn rpc_result(v: Value) -> std::result::Result<Value, IntentRejectReason> {
        if let Some(e) = v.get("error") {
            return Err(reason_in(e));
        }
        v.get("result").cloned().ok_or(IntentRejectReason::Invalid)
    }

    /// Start / prompt / cancel on v9 owner `target` as A2A (instead of the intent). Same checks
    /// before sending, same ack results and codes, same origin log entry.
    pub(super) async fn forward_a2a(&self, target: &str, actor: &Actor, confirmed: bool, intent: &Intent) -> Result<Value> {
        let client = actor.kind.unwrap_or(ClientKind::Cli);
        let (kind, (tgt, option)) = (kind_name(intent), target_of(intent));
        let call_id = uuid::Uuid::new_v4().to_string();
        let immediate = || Some(SendMessageConfiguration { accepted_output_modes: None, task_push_notification_config: None, history_length: Some(0), return_immediately: Some(true) });
        let outcome: std::result::Result<std::result::Result<Value, IntentRejectReason>, AckCode> = match intent {
            Intent::Start { agent_id, cwd, prompt } => {
                let parts = prompt.as_deref().filter(|p| !p.trim().is_empty()).map(|p| vec![Part::text(p)]).unwrap_or_default();
                let mut m = Message::new(Role::User, parts);
                DelegateParams { agent_id: Some(agent_id.clone()), cwd: Some(cwd.clone()), machine_id: Some(target.to_string()), confirmed: Some(confirmed) }.attach(&mut m);
                let req = SendMessageRequest { message: m, configuration: immediate(), metadata: None, tenant: None };
                let res = self.peer_call(target, &call_id, client, confirmed, "SendMessage", serde_json::to_value(&req).unwrap_or(Value::Null)).await;
                res.map(|v| {
                    Self::rpc_result(v).and_then(|r| {
                        // The owner's session (and warning) ride in the answer's delegate metadata.
                        let meta = r.get("task").or_else(|| r.get("message")).and_then(|x| x.get("metadata")).and_then(|m| m.get(DELEGATE_EXT)).cloned();
                        let meta = meta.ok_or(IntentRejectReason::Invalid)?;
                        let mut out = json!({ "session": meta.get("session").cloned().unwrap_or(Value::Null) });
                        if let Some(w) = meta.get("warning").filter(|w| w.is_string()) {
                            out["warning"] = w.clone();
                        }
                        Ok(out)
                    })
                })
            }
            Intent::Prompt { session_id, text } => {
                let mut m = Message::new(Role::User, vec![Part::text(text.clone())]);
                m.context_id = Some(session_id.clone());
                let req = SendMessageRequest { message: m, configuration: immediate(), metadata: None, tenant: None };
                let res = self.peer_call(target, &call_id, client, confirmed, "SendMessage", serde_json::to_value(&req).unwrap_or(Value::Null)).await;
                res.map(|v| Self::rpc_result(v).map(|_| json!({})))
            }
            Intent::Cancel { session_id } => {
                // The running task of the session, then CancelTask; a session with no running task has
                // nothing to cancel.
                let list = ListTasksRequest {
                    context_id: Some(session_id.clone()),
                    status: None,
                    page_size: Some(1),
                    page_token: None,
                    history_length: Some(0),
                    status_timestamp_after: None,
                    include_artifacts: Some(false),
                    tenant: None,
                };
                match self.peer_call(target, &call_id, client, confirmed, "ListTasks", serde_json::to_value(&list).unwrap_or(Value::Null)).await {
                    Err(code) => Err(code),
                    Ok(v) => match Self::rpc_result(v) {
                        Err(r) => Ok(Err(r)),
                        Ok(r) => {
                            let running = r.get("tasks").and_then(Value::as_array).and_then(|t| t.first()).filter(|t| {
                                let s = t.get("status").and_then(|s| s.get("state")).and_then(Value::as_str).unwrap_or("");
                                !matches!(s, "TASK_STATE_COMPLETED" | "TASK_STATE_FAILED" | "TASK_STATE_CANCELED" | "TASK_STATE_REJECTED")
                            });
                            match running.and_then(|t| t.get("id")).and_then(Value::as_str) {
                                None => Ok(Ok(json!({}))),
                                Some(id) => {
                                    let cancel = CancelTaskRequest { id: id.to_string(), metadata: None, tenant: None };
                                    let call2 = uuid::Uuid::new_v4().to_string();
                                    self.peer_call(target, &call2, client, confirmed, "CancelTask", serde_json::to_value(&cancel).unwrap_or(Value::Null)).await.map(|v| {
                                        match v.get("error").and_then(|e| e.get("code")).and_then(Value::as_i64).map(|c| c as i32) {
                                            // Finished meanwhile: nothing left to cancel.
                                            Some(error_code::TASK_NOT_CANCELABLE) => Ok(json!({})),
                                            _ => Self::rpc_result(v).map(|_| json!({})),
                                        }
                                    })
                                }
                            }
                        }
                    },
                }
            }
            Intent::Approve { .. } | Intent::Deny { .. } => unreachable!("approvals stay intents"),
        };
        let (status, reason) = match &outcome {
            Ok(Ok(_)) => ("applied".to_string(), None),
            Ok(Err(r)) => ("rejected".to_string(), Some(reason_name(*r))),
            Err(code) => (code.to_string(), None),
        };
        tracing::info!(call = %call_id, kind, device = %target, status = %status, reason = ?reason, "A2A call sent");
        self.log_intent(IntentLogEntry {
            id: 0,
            ts: pawl_protocol::now_ms(),
            role: IntentRole::Origin,
            intent_id: call_id,
            kind: kind.to_string(),
            peer_machine_id: target.to_string(),
            peer_name: self.peer_name(target),
            client: client_label(client).to_string(),
            target: Some(tgt),
            option_id: option,
            status,
            reason,
            repeats: 0,
        })
        .await;
        match outcome {
            Ok(Ok(v)) => checked_result(kind, target, Some(v)).map_err(Into::into),
            Ok(Err(r)) => Err(reason_code(r).into()),
            Err(code) => Err(code.into()),
        }
    }

    // ------------------------------------------------------------ both

    /// A frame of an A2A call from paired hub `from` (already authenticated as linked and paired).
    pub(super) fn on_a2a_frame(&self, from: &str, call_id: String, frame: A2aPeerFrame) {
        match frame {
            A2aPeerFrame::Request { rpc, client, confirmed } => {
                #[cfg(test)]
                if self.intents().drop_all {
                    return;
                }
                let hub = self.clone();
                let from = from.to_string();
                tokio::spawn(async move { hub.answer_a2a(&from, &call_id, rpc, client, confirmed).await });
            }
            A2aPeerFrame::Response { rpc } => {
                let waiter = {
                    let mut s = self.a2a_peer();
                    match s.waiting.get(&call_id) {
                        Some((t, _)) if t == from => s.waiting.remove(&call_id),
                        _ => None,
                    }
                };
                match waiter {
                    Some((_, tx)) => {
                        let _ = tx.send(rpc);
                    }
                    None => tracing::debug!(device = %from, "ignoring an A2A response nobody is waiting for"),
                }
            }
            // This hub only makes unary calls.
            A2aPeerFrame::Event { .. } | A2aPeerFrame::End => tracing::debug!(device = %from, "ignoring an A2A stream frame"),
        }
    }

    fn send_frame(&self, to: &str, call_id: &str, frame: A2aPeerFrame) -> std::result::Result<(), SendError> {
        let r = self.inner.peer.send(to, PeerBody::A2a { call_id: call_id.to_string(), frame });
        if let Err(e) = &r {
            tracing::warn!(device = %to, call = %call_id, "A2A answer not sent: {e}");
        }
        r
    }

    fn remember_answer(&self, from: &str, call_id: &str, answer: Answer) {
        self.a2a_peer().answers.entry(from.to_string()).or_default().remember(call_id, answer);
    }

    // ------------------------------------------------------------ owner

    /// Answer one A2A call: dedupe, `forbidden`, rate limit, the dispatcher with the peer policy, log.
    pub(super) async fn answer_a2a(&self, from: &str, call_id: &str, rpc: Value, client: ClientKind, confirmed: bool) {
        let key = (from.to_string(), call_id.to_string());
        let id = rpc.get("id").cloned().unwrap_or(Value::Null);
        let error_reply = |e: A2AError| json!({ "jsonrpc": "2.0", "id": id, "error": e.to_jsonrpc_error() });
        {
            let mut s = self.a2a_peer();
            let seen = s.answers.get(from).and_then(|a| a.map.get(call_id)).map(|(a, _)| a.clone());
            match seen {
                Some(Answer::Reply(v)) => {
                    drop(s);
                    let _ = self.send_frame(from, call_id, A2aPeerFrame::Response { rpc: v });
                    return;
                }
                Some(Answer::Marker(what)) => {
                    drop(s);
                    let e = refusal(IntentRejectReason::Invalid, format!("this call was already {what}"));
                    let _ = self.send_frame(from, call_id, A2aPeerFrame::Response { rpc: error_reply(e) });
                    return;
                }
                None => {}
            }
            if !s.in_flight.insert(key.clone()) {
                return; // a repeat of a call still being handled
            }
        }
        let method = rpc.get("method").and_then(Value::as_str).unwrap_or_default().to_string();
        let streaming = pawl_a2a::spec::jsonrpc::methods::is_streaming(&method);
        let refused = if !self.cfg().peer.accept_intents {
            Some(refusal(IntentRejectReason::Forbidden, "this machine does not accept remote control"))
        } else if !self.intent_rate_ok(from) {
            Some(refusal(IntentRejectReason::RateLimited, "too many requests"))
        } else if streaming && self.a2a_peer().streams.get(from).copied().unwrap_or(0) >= STREAMS_PER_PEER {
            Some(refusal(IntentRejectReason::RateLimited, format!("at most {STREAMS_PER_PEER} open streams per machine")))
        } else {
            None
        };
        let kind = Self::call_kind(&method, &rpc);
        let reply = match refused {
            Some(e) => RpcReply::Unary(error_reply(e)),
            None => {
                let handler = PeerHandler { hub: self.clone(), from: from.to_string(), client, confirmed };
                let body = serde_json::to_vec(&rpc).unwrap_or_default();
                rpc::call(&handler, &ServiceParams::new(), &body, Wire::V1).await
            }
        };
        let (status, reason) = match &reply {
            RpcReply::Unary(v) => match v.get("error") {
                Some(e) => ("rejected".to_string(), Some(reason_name(reason_in(e)))),
                None => ("applied".to_string(), None),
            },
            RpcReply::Stream(_) => ("applied".to_string(), None),
        };
        tracing::info!(call = %call_id, kind = %kind, device = %from, status = %status, reason = ?reason, "A2A call handled");
        let mut stream = None;
        match reply {
            RpcReply::Unary(v) => {
                // An answer the link cannot carry becomes a small error, so the origin is not left waiting.
                let size = serde_json::to_vec(&v).map(|b| b.len()).unwrap_or(0);
                let v = if size > pawl_protocol::MIRROR_FRAME_MAX_BYTES {
                    error_reply(refusal(IntentRejectReason::Invalid, "the answer is too large for the link"))
                } else {
                    v
                };
                self.a2a_peer().in_flight.remove(&key);
                // A rate-limited call may be retried with the same id; small replies are kept, larger
                // ones only as "answered".
                if reason.as_deref() != Some("rate_limited") {
                    let answer = if size <= ANSWER_KEEP_MAX_BYTES { Answer::Reply(v.clone()) } else { Answer::Marker("answered") };
                    self.remember_answer(from, call_id, answer);
                }
                let _ = self.send_frame(from, call_id, A2aPeerFrame::Response { rpc: v });
            }
            RpcReply::Stream(st) => {
                {
                    let mut s = self.a2a_peer();
                    s.in_flight.remove(&key);
                    *s.streams.entry(from.to_string()).or_default() += 1;
                }
                // A repeat of this call id must not start it again.
                self.remember_answer(from, call_id, Answer::Marker("accepted"));
                stream = Some(st);
            }
        }
        let target = rpc.get("params").and_then(|p| {
            p.get("message").and_then(|m| m.get("contextId").or_else(|| m.get("metadata").and_then(|x| x.get(DELEGATE_EXT)).and_then(|d| d.get("agentId")))).or_else(|| p.get("id")).or_else(|| p.get("contextId"))
        });
        self.log_intent(IntentLogEntry {
            id: 0,
            ts: pawl_protocol::now_ms(),
            role: IntentRole::Owner,
            intent_id: call_id.to_string(),
            kind,
            peer_machine_id: from.to_string(),
            peer_name: self.peer_name(from),
            client: client_label(client).to_string(),
            target: target.and_then(Value::as_str).map(str::to_string),
            option_id: None,
            status,
            reason,
            repeats: 0,
        })
        .await;
        if let Some(mut st) = stream {
            // Forward until the stream ends or the link is gone.
            let mut linked = true;
            while let Some(v) = st.next().await {
                if let Err(SendError::NotLinked | SendError::Backpressure) = self.send_frame(from, call_id, A2aPeerFrame::Event { rpc: v }) {
                    linked = false;
                    break;
                }
            }
            if linked {
                let _ = self.send_frame(from, call_id, A2aPeerFrame::End);
            }
            let mut s = self.a2a_peer();
            if let Some(n) = s.streams.get_mut(from) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    s.streams.remove(from);
                }
            }
        }
    }

    /// The intent-log kind of a call: start / prompt / cancel, else the method.
    fn call_kind(method: &str, rpc: &Value) -> String {
        let msg = rpc.get("params").and_then(|p| p.get("message"));
        match method {
            "SendMessage" | "SendStreamingMessage" if msg.and_then(|m| m.get("contextId")).is_some() => "prompt".into(),
            "SendMessage" | "SendStreamingMessage" => "start".into(),
            "CancelTask" => "cancel".into(),
            other => other.chars().take(40).collect(),
        }
    }
}

/// A start / prompt answer without the task's history and artifacts: the origin needs the ids and
/// the state, and link frames are size-capped (a long prompt would not fit twice).
fn lean(r: SendMessageResponse) -> SendMessageResponse {
    match r {
        SendMessageResponse::Task(mut t) => {
            t.history = None;
            t.artifacts = None;
            SendMessageResponse::Task(t)
        }
        other => other,
    }
}

/// The A2A operations a paired hub may call (as `Caller::Peer`): the intent rules.
struct PeerHandler {
    hub: Hub,
    from: String,
    client: ClientKind,
    confirmed: bool,
}

impl PeerHandler {
    fn caller(&self) -> Caller {
        Caller::Peer(self.from.clone())
    }

    fn actor(&self) -> Actor {
        let who = self.hub.peer_name(&self.from).unwrap_or_else(|| self.from.clone());
        Actor { label: format!("{}@{who}", client_label(self.client)), kind: Some(self.client) }
    }

    async fn send(&self, req: SendMessageRequest) -> Result<SendMessageResponse, A2AError> {
        let m = &req.message;
        if m.task_id.is_some() {
            return Err(refusal(IntentRejectReason::Forbidden, "approvals between machines go through approve / deny"));
        }
        let text = message_text(m);
        if let Some(session_id) = m.context_id.clone() {
            // The step-4 prompt rules: this machine's open owned session, prompt limits.
            self.hub.check_owned(&session_id).map_err(|e| refusal_of(&e))?;
            validate_prompt(&text).map_err(|e| refusal_of(&e))?;
            self.hub.owned_open_session(&session_id).map_err(|e| refusal_of(&e))?;
            let d = self.hub.a2a_send(req, &self.caller(), &self.actor(), Answers::Never).await.map_err(|e| refusal_of(&e))?;
            return Ok(lean(d.response));
        }
        // The step-4 start rules: confirmed on the origin's device, an installed agent, a known folder.
        let params = DelegateParams::of(m).ok_or_else(|| refusal(IntentRejectReason::Invalid, "a start needs agentId and cwd"))?;
        if !self.confirmed {
            return Err(refusal(IntentRejectReason::Unconfirmed, "a remote start needs the user's confirmation"));
        }
        let agent_id = params.agent_id.unwrap_or_default();
        if !self.hub.st().agents.iter().any(|a| a.id == agent_id && a.installed) {
            return Err(refusal(IntentRejectReason::Invalid, format!("{agent_id} is not installed here")));
        }
        let dir = self.hub.remote_start_dir(params.cwd.as_deref().unwrap_or_default()).await.map_err(|r| refusal(r, "the folder is not one this machine starts agents in"))?;
        let prompt = Some(text).filter(|t| !t.trim().is_empty());
        if let Some(t) = prompt.as_deref() {
            validate_prompt(t).map_err(|e| refusal_of(&e))?;
        }
        let d = self.hub.a2a_start(&agent_id, &dir.to_string_lossy(), prompt, &self.caller()).await.map_err(|e| {
            tracing::warn!(device = %self.from, "remote start of {agent_id} failed: {e:#}");
            refusal(IntentRejectReason::Invalid, "the agent did not start")
        })?;
        // The origin's client gets the session (and a warning) like an intent ack.
        let mut meta = json!({ "session": d.session });
        if let Some(w) = &d.warning {
            meta["warning"] = json!(w);
        }
        let mut response = lean(d.response);
        let metadata = match &mut response {
            SendMessageResponse::Task(t) => &mut t.metadata,
            SendMessageResponse::Message(m) => &mut m.metadata,
        };
        metadata.get_or_insert_with(HashMap::new).insert(DELEGATE_EXT.to_string(), meta);
        Ok(response)
    }
}

#[pawl_a2a::async_trait]
impl A2aHandler for PeerHandler {
    async fn send_message(&self, _p: &ServiceParams, req: SendMessageRequest) -> Result<SendMessageResponse, A2AError> {
        self.send(req).await
    }

    async fn send_streaming_message(&self, _p: &ServiceParams, req: SendMessageRequest) -> Result<EventStream, A2AError> {
        match self.send(req).await? {
            SendMessageResponse::Message(m) => Ok(futures::stream::iter(vec![Ok(pawl_a2a::spec::StreamResponse::Message(m))]).boxed()),
            SendMessageResponse::Task(t) => Ok(HubHandler::follow(self.hub.clone(), self.caller(), t.id, None, None)),
        }
    }

    async fn get_task(&self, _p: &ServiceParams, req: GetTaskRequest) -> Result<Task, A2AError> {
        self.hub.a2a_get(&req.id, req.history_length, &self.caller()).map_err(|_| refusal(IntentRejectReason::NotFound, "no such task"))
    }

    async fn list_tasks(&self, _p: &ServiceParams, req: ListTasksRequest) -> Result<ListTasksResponse, A2AError> {
        // A session that is not this machine's (or unknown) is `not_found`, as the cancel intent said.
        if let Some(ctx) = &req.context_id {
            self.hub.check_owned(ctx).map_err(|e| refusal_of(&e))?;
            if self.hub.session(ctx).is_none() {
                return Err(refusal(IntentRejectReason::NotFound, "no such session"));
            }
        }
        self.hub.a2a_list(&req, &self.caller()).map_err(|e| refusal(IntentRejectReason::Invalid, format!("{e:#}")))
    }

    async fn cancel_task(&self, _p: &ServiceParams, req: CancelTaskRequest) -> Result<Task, A2AError> {
        match self.hub.a2a_cancel(&req.id, &self.caller(), &self.actor()).await {
            Ok(t) => Ok(t),
            Err(e) => Err(match super::a2a::A2aError::find(&e) {
                Some(super::a2a::A2aError::TaskNotCancelable) => A2AError::task_not_cancelable(&req.id),
                Some(super::a2a::A2aError::TaskNotFound) => refusal(IntentRejectReason::NotFound, "no such task"),
                _ => refusal_of(&e),
            }),
        }
    }

    async fn subscribe_to_task(&self, _p: &ServiceParams, req: SubscribeToTaskRequest) -> Result<EventStream, A2AError> {
        let t = self.hub.a2a_get(&req.id, None, &self.caller()).map_err(|_| refusal(IntentRejectReason::NotFound, "no such task"))?;
        if t.status.state.is_terminal() {
            return Err(A2AError::unsupported_operation("the task has finished"));
        }
        Ok(HubHandler::follow(self.hub.clone(), self.caller(), t.id, None, None))
    }

    async fn get_extended_agent_card(&self, _p: &ServiceParams, _r: GetExtendedAgentCardRequest) -> Result<AgentCard, A2AError> {
        let mut card = pawl_a2a::card::pawl_card(&self.hub.card_info(0, true), true);
        // Over the link there is no URL: the binding is the encrypted peer link.
        for i in card.supported_interfaces.iter_mut() {
            i.url = format!("pawl-peer://{}", self.hub.machine_id());
            i.protocol_binding = pawl_protocol::A2A_PEER_BINDING.to_string();
            i.protocol_version = pawl_a2a::A2A_VERSION.to_string();
        }
        card.supported_interfaces.truncate(1);
        Ok(card)
    }
}

#[cfg(test)]
impl Hub {
    /// Tests: calls waiting for an answer (origin).
    pub(super) fn a2a_waiting(&self) -> usize {
        self.a2a_peer().waiting.len()
    }

    /// Tests: open streams of `peer` (owner); `set` replaces the count.
    pub(super) fn a2a_peer_streams(&self, peer: &str, set: Option<usize>) -> usize {
        let mut s = self.a2a_peer();
        if let Some(n) = set {
            s.streams.insert(peer.to_string(), n);
        }
        s.streams.get(peer).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembered_answers_are_bounded_per_peer_by_count_and_bytes() {
        let mut a = PeerAnswers::default();
        for i in 0..(ANSWERS_PER_PEER + 10) {
            a.remember(&format!("c{i}"), Answer::Marker("answered"));
        }
        assert_eq!(a.map.len(), ANSWERS_PER_PEER);
        assert!(!a.map.contains_key("c0"), "oldest out first");
        let mut b = PeerAnswers::default();
        let big = json!({ "result": "x".repeat(ANSWER_KEEP_MAX_BYTES - 100) });
        for i in 0..100 {
            b.remember(&format!("c{i}"), Answer::Reply(big.clone()));
        }
        assert!(b.bytes <= ANSWER_BYTES_PER_PEER);
        assert!(b.map.len() < 100);
        // Re-remembering a call replaces it without double counting.
        let mut c = PeerAnswers::default();
        c.remember("x", Answer::Reply(json!(1)));
        let once = c.bytes;
        c.remember("x", Answer::Reply(json!(1)));
        assert_eq!((c.bytes, c.order.len()), (once, 1));
    }
}
