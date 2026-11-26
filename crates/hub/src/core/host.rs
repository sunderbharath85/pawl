//! Thin seam over `pawl_acp::AcpHost` so the core's message handling can be unit-tested with a fake.

use std::path::PathBuf;

use anyhow::Result;
use pawl_acp::{AcpHost, HostEvent};
use pawl_protocol::{Agent, ChatImage, HistorySession, MachineId, Session, SessionId};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Clone)]
pub(crate) enum Host {
    Acp(AcpHost),
    #[cfg(test)]
    Fake(fake::FakeHost),
}

impl Host {
    pub(crate) fn acp(machine_id: MachineId, events: UnboundedSender<HostEvent>) -> Self {
        Host::Acp(AcpHost::new(machine_id, events))
    }

    pub(crate) async fn start_session(&self, agent: &Agent, cwd: PathBuf, id: SessionId, prompt: Option<String>) -> Result<Session> {
        match self {
            Host::Acp(h) => h.start_session(agent, cwd, id, prompt).await,
            #[cfg(test)]
            Host::Fake(f) => f.start_session(agent, cwd, id, prompt),
        }
    }

    /// `start_session` with ACP start options (`session/new` `_meta`, a clean agent environment).
    pub(crate) async fn start_session_with(
        &self,
        agent: &Agent,
        cwd: PathBuf,
        id: SessionId,
        prompt: Option<String>,
        opts: pawl_acp::StartOptions,
    ) -> Result<Session> {
        match self {
            Host::Acp(h) => h.start_session_with(agent, cwd, id, prompt, opts).await,
            #[cfg(test)]
            Host::Fake(f) => {
                f.options.lock().unwrap().push(opts);
                // Tests: a slow start (a settings change can land meanwhile).
                let delay = f.start_delay_ms.load(std::sync::atomic::Ordering::SeqCst);
                if delay > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                }
                f.start_session(agent, cwd, id, prompt)
            }
        }
    }

    pub(crate) async fn prompt(&self, id: &str, text: String) -> Result<()> {
        match self {
            Host::Acp(h) => h.prompt(id, text).await,
            #[cfg(test)]
            Host::Fake(f) => {
                f.record(format!("prompt {id} {text}"))?;
                // Tests: the agent refuses this prompt.
                if text == "fail" {
                    anyhow::bail!("the agent refused the prompt");
                }
                Ok(())
            }
        }
    }

    /// A prompt with pictures (ACP image blocks); the host refuses it unless the agent takes images.
    pub(crate) async fn prompt_with_images(&self, id: &str, text: String, images: Vec<ChatImage>) -> Result<()> {
        match self {
            Host::Acp(h) => h.prompt_with_images(id, text, images).await,
            #[cfg(test)]
            Host::Fake(f) => {
                if !f.images_ok.load(std::sync::atomic::Ordering::SeqCst) {
                    anyhow::bail!("this agent does not accept pictures (it does not advertise promptCapabilities.image)");
                }
                // The calls log never holds a picture, only how many came and their type.
                let kinds: Vec<&str> = images.iter().map(|i| i.media_type.as_str()).collect();
                f.record(format!("prompt {id} {text} [images: {}]", kinds.join(",")))?;
                f.image_bytes.lock().unwrap().extend(images.iter().map(|i| i.data.len()));
                Ok(())
            }
        }
    }

    /// Page element `uid` of the latest snapshot this session's tools returned (untrusted page text).
    pub(crate) fn element_description(&self, id: &str, uid: &str) -> Option<String> {
        match self {
            Host::Acp(h) => h.element_description(id, uid),
            #[cfg(test)]
            Host::Fake(f) => f.elements.lock().unwrap().get(&(id.to_string(), uid.to_string())).cloned(),
        }
    }

    /// True when the agent of this open session advertised `promptCapabilities.image`.
    pub(crate) fn accepts_images(&self, id: &str) -> bool {
        match self {
            Host::Acp(h) => h.accepts_images(id),
            #[cfg(test)]
            Host::Fake(f) => f.images_ok.load(std::sync::atomic::Ordering::SeqCst),
        }
    }

    pub(crate) async fn cancel(&self, id: &str) -> Result<()> {
        match self {
            Host::Acp(h) => h.cancel(id).await,
            #[cfg(test)]
            Host::Fake(f) => f.record(format!("cancel {id}")),
        }
    }

    pub(crate) async fn resolve_permission(&self, request_id: &str, option_id: Option<String>) -> Result<()> {
        match self {
            Host::Acp(h) => h.resolve_permission(request_id, option_id).await,
            #[cfg(test)]
            Host::Fake(f) => {
                f.record(format!("resolve {request_id} {}", option_id.as_deref().unwrap_or("<cancelled>")))?;
                // Tests: a slow answer the agent then refuses (the request goes back to pending).
                if option_id.as_deref() == Some("slow-fail") {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    anyhow::bail!("the agent refused the answer");
                }
                Ok(())
            }
        }
    }

    pub(crate) async fn list_history(&self, agent: &Agent, cwd: Option<PathBuf>) -> Result<Vec<HistorySession>> {
        match self {
            Host::Acp(h) => h.list_history(agent, cwd).await,
            #[cfg(test)]
            Host::Fake(_) => Ok(vec![]),
        }
    }

    pub(crate) async fn resume_session(&self, agent: &Agent, acp_session_id: String, cwd: PathBuf, id: SessionId) -> Result<Session> {
        match self {
            Host::Acp(h) => h.resume_session(agent, acp_session_id, cwd, id).await,
            #[cfg(test)]
            Host::Fake(f) => {
                let mut s = f.start_session(agent, cwd, id, None)?;
                s.acp_session_id = Some(acp_session_id);
                Ok(s)
            }
        }
    }

    pub(crate) async fn close_session(&self, id: &str) -> Result<()> {
        match self {
            Host::Acp(h) => h.close_session(id).await,
            #[cfg(test)]
            Host::Fake(f) => {
                f.record(format!("close {id}"))?;
                // Tests: the agent does not let go of the session.
                if f.close_fails.load(std::sync::atomic::Ordering::SeqCst) {
                    anyhow::bail!("the agent did not close the session");
                }
                Ok(())
            }
        }
    }

    pub(crate) async fn shutdown(&self) {
        match self {
            Host::Acp(h) => h.shutdown().await,
            #[cfg(test)]
            Host::Fake(f) => {
                let _ = f.record("shutdown".into());
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use pawl_protocol::{SessionSource, SessionStatus};
    use std::sync::{Arc, Mutex};

    /// Records every call; sessions start instantly in `idle`.
    #[derive(Clone, Default)]
    pub(crate) struct FakeHost {
        pub calls: Arc<Mutex<Vec<String>>>,
        /// Every agent a session was started with (env included).
        pub started: Arc<Mutex<Vec<Agent>>>,
        /// The start options of every `start_session_with`.
        pub options: Arc<Mutex<Vec<pawl_acp::StartOptions>>>,
        /// The agent advertises `promptCapabilities.image` (tests turn it on).
        pub images_ok: Arc<std::sync::atomic::AtomicBool>,
        /// The base64 length of every picture a prompt carried (never the picture).
        pub image_bytes: Arc<Mutex<Vec<usize>>>,
        /// (session, uid) -> element description, as the latest snapshot would give it.
        pub elements: Arc<Mutex<std::collections::HashMap<(String, String), String>>>,
        /// `start_session_with` waits this long first (tests of what happens meanwhile).
        pub start_delay_ms: Arc<std::sync::atomic::AtomicU64>,
        /// `close_session` fails (tests).
        pub close_fails: Arc<std::sync::atomic::AtomicBool>,
    }

    impl FakeHost {
        pub(crate) fn record(&self, call: String) -> Result<()> {
            self.calls.lock().unwrap().push(call);
            Ok(())
        }

        pub(crate) fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }

        pub(crate) fn start_session(&self, agent: &Agent, cwd: PathBuf, id: SessionId, prompt: Option<String>) -> Result<Session> {
            self.record(format!("start {} {}", agent.id, prompt.unwrap_or_default()))?;
            self.started.lock().unwrap().push(agent.clone());
            let now = pawl_protocol::now_ms();
            // Hub session ids are global; the owning machine is their prefix.
            let machine_id = pawl_protocol::parse_global_id(&id).map(|(m, _)| m.to_string()).unwrap_or_default();
            Ok(Session {
                id,
                machine_id,
                agent_id: agent.id.clone(),
                acp_session_id: Some(format!("acp-{}", uuid::Uuid::new_v4())),
                external_id: None,
                cwd: cwd.to_string_lossy().into_owned(),
                title: None,
                source: SessionSource::Owned,
                status: SessionStatus::Idle,
                started_at: now,
                last_event_at: now,
                ended_at: None,
                purpose: None,
            })
        }
    }
}
