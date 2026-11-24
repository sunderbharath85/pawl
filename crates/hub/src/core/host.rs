//! Thin seam over `krypto_acp::AcpHost` so the core's message handling can be unit-tested with a fake.

use std::path::PathBuf;

use anyhow::Result;
use krypto_acp::{AcpHost, HostEvent};
use krypto_protocol::{Agent, HistorySession, MachineId, Session, SessionId};
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
            Host::Fake(f) => f.record(format!("close {id}")),
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
    use krypto_protocol::{SessionSource, SessionStatus};
    use std::sync::{Arc, Mutex};

    /// Records every call; sessions start instantly in `idle`.
    #[derive(Clone, Default)]
    pub(crate) struct FakeHost {
        pub calls: Arc<Mutex<Vec<String>>>,
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
            let now = krypto_protocol::now_ms();
            // Hub session ids are global; the owning machine is their prefix.
            let machine_id = krypto_protocol::parse_global_id(&id).map(|(m, _)| m.to_string()).unwrap_or_default();
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
            })
        }
    }
}
