//! In-process event bus: every `ServerMsg` broadcast to connected clients goes through here.
//!
//! The bus can also feed a *tap*: an unbounded channel that receives a copy of every message that
//! may need mirroring to paired hubs (session / event / pet / permission / notification). The tap
//! never lags, unlike broadcast subscribers, so peers never silently miss an update.

use std::sync::{Arc, Mutex};

use pawl_protocol::ServerMsg;
use tokio::sync::{broadcast, mpsc};

/// Buffered messages per subscriber before it counts as lagging.
pub const BUS_CAPACITY: usize = 4096;

#[derive(Clone)]
pub struct Bus {
    tx: broadcast::Sender<ServerMsg>,
    tap: Arc<Mutex<Option<mpsc::UnboundedSender<ServerMsg>>>>,
}

impl Default for Bus {
    fn default() -> Self {
        Self::new(BUS_CAPACITY)
    }
}

/// Message types that describe an item paired hubs may mirror.
pub fn is_mirrorable(msg: &ServerMsg) -> bool {
    matches!(
        msg,
        ServerMsg::Session { .. }
            | ServerMsg::Event { .. }
            | ServerMsg::Pet { .. }
            | ServerMsg::PermissionRequest { .. }
            | ServerMsg::PermissionResolved { .. }
            | ServerMsg::Notification { .. }
    )
}

impl Bus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(16));
        Self { tx, tap: Arc::new(Mutex::new(None)) }
    }

    /// Fire-and-forget to local clients (no subscribers is fine), plus a copy to the tap when the
    /// message is mirrorable.
    pub fn send(&self, msg: ServerMsg) {
        if is_mirrorable(&msg) {
            let tap = self.tap.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(t) = tap.as_ref() {
                let _ = t.send(msg.clone());
            }
        }
        let _ = self.tx.send(msg);
    }

    /// Local clients only: never offered to the tap (hub-local notices such as pairing results).
    pub fn send_local(&self, msg: ServerMsg) {
        let _ = self.tx.send(msg);
    }

    /// Install the tap; returns its receiver. Replaces any previous tap.
    pub fn tap(&self) -> mpsc::UnboundedReceiver<ServerMsg> {
        let (tx, rx) = mpsc::unbounded_channel();
        *self.tap.lock().unwrap_or_else(|p| p.into_inner()) = Some(tx);
        rx
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerMsg> {
        self.tx.subscribe()
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pawl_protocol::Agent;

    #[test]
    fn tap_sees_only_mirrorable_messages() {
        let bus = Bus::default();
        let mut tap = bus.tap();
        let mut sub = bus.subscribe();
        bus.send(ServerMsg::Agents { agents: Vec::<Agent>::new() });
        bus.send(ServerMsg::MachineRemoved { machine_id: "abcdef012345".into() });
        let n = ServerMsg::Notification {
            notification: pawl_protocol::HubNotification {
                id: "abcdef012345:n".into(),
                machine_id: "abcdef012345".into(),
                level: pawl_protocol::NotificationLevel::Info,
                title: "t".into(),
                body: "b".into(),
                session_id: None,
                agent_id: None,
                ts: 0,
            },
        };
        bus.send_local(n.clone());
        bus.send(n.clone());
        assert_eq!(tap.try_recv().unwrap(), n);
        assert!(tap.try_recv().is_err(), "agents, machine_removed and send_local never reach the tap");
        assert_eq!(std::iter::from_fn(|| sub.try_recv().ok()).count(), 4, "local clients get everything");
    }
}
