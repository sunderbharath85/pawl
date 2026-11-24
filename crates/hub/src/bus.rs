//! In-process event bus: every `ServerMsg` broadcast to connected clients goes through here.

use pet_protocol::ServerMsg;
use tokio::sync::broadcast;

/// Buffered messages per subscriber before it counts as lagging.
pub const BUS_CAPACITY: usize = 4096;

#[derive(Clone)]
pub struct Bus {
    tx: broadcast::Sender<ServerMsg>,
}

impl Default for Bus {
    fn default() -> Self {
        Self::new(BUS_CAPACITY)
    }
}

impl Bus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(16));
        Self { tx }
    }

    /// Fire-and-forget; no subscribers is fine.
    pub fn send(&self, msg: ServerMsg) {
        let _ = self.tx.send(msg);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ServerMsg> {
        self.tx.subscribe()
    }

    pub fn subscriber_count(&self) -> usize {
        self.tx.receiver_count()
    }
}
