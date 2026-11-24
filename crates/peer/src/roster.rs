//! Pairing roster storage. The hub implements `Roster` on SQLite; `MemoryRoster` is the reference
//! implementation (used by tests, usable by the hub's tests too).

use krypto_protocol::{Device, Machine, UnixMs};
use std::collections::HashMap;
use std::sync::Mutex;

/// Roster storage the hub provides (SQLite). Sync, quick calls.
///
/// Notes for implementers:
/// - `get`/`list` return non-revoked entries only.
/// - `upsert` is a (re-)pairing: replace the key, name, addr and machine, clear `revoked`. It need not
///   touch the seq counters; the node sets both right after.
/// - `update_seen`: set name/os/hub_version/last_seen from `machine`, `last_seen = ts`, and the addr
///   only when `addr` is `Some` (None keeps the stored one). No-op for unknown ids.
/// - `set_last_rx_seq`/`set_last_tx_seq` store the value as given (it may go down on a re-pair). For a
///   device that is not stored yet they may be a no-op. Unknown ids read as 0.
pub trait Roster: Send + Sync + 'static {
    fn get(&self, device_id: &str) -> Option<RosterEntry>;
    fn list(&self) -> Vec<RosterEntry>;
    fn upsert(&self, entry: RosterEntry);
    fn update_seen(&self, device_id: &str, machine: &Machine, addr: Option<&str>, ts: UnixMs);
    fn revoke(&self, device_id: &str);
    fn last_rx_seq(&self, device_id: &str) -> u64;
    fn set_last_rx_seq(&self, device_id: &str, seq: u64);
    fn last_tx_seq(&self, device_id: &str) -> u64;
    fn set_last_tx_seq(&self, device_id: &str, seq: u64);
}

#[derive(Debug, Clone, PartialEq)]
pub struct RosterEntry {
    pub device: Device,
    /// Last known machine info; `online` is always false here (live state comes from `PeerNode::online`).
    pub machine: Machine,
}

#[derive(Debug, Clone)]
struct Row {
    entry: RosterEntry,
    revoked: bool,
    last_rx: u64,
    last_tx: u64,
}

/// In-memory `Roster`.
#[derive(Debug, Default)]
pub struct MemoryRoster {
    rows: Mutex<HashMap<String, Row>>,
}

impl MemoryRoster {
    pub fn new() -> Self {
        Self::default()
    }

    /// True if the id is stored and revoked.
    pub fn is_revoked(&self, device_id: &str) -> bool {
        self.rows.lock().unwrap().get(device_id).is_some_and(|r| r.revoked)
    }
}

impl Roster for MemoryRoster {
    fn get(&self, device_id: &str) -> Option<RosterEntry> {
        self.rows.lock().unwrap().get(device_id).filter(|r| !r.revoked).map(|r| r.entry.clone())
    }

    fn list(&self) -> Vec<RosterEntry> {
        let mut v: Vec<RosterEntry> =
            self.rows.lock().unwrap().values().filter(|r| !r.revoked).map(|r| r.entry.clone()).collect();
        v.sort_by(|a, b| a.device.id.cmp(&b.device.id));
        v
    }

    fn upsert(&self, entry: RosterEntry) {
        let mut rows = self.rows.lock().unwrap();
        let mut entry = entry;
        entry.machine.online = false;
        match rows.get_mut(&entry.device.id) {
            Some(row) => {
                row.entry = entry;
                row.revoked = false;
            }
            None => {
                rows.insert(entry.device.id.clone(), Row { entry, revoked: false, last_rx: 0, last_tx: 0 });
            }
        }
    }

    fn update_seen(&self, device_id: &str, machine: &Machine, addr: Option<&str>, ts: UnixMs) {
        let mut rows = self.rows.lock().unwrap();
        let Some(row) = rows.get_mut(device_id) else { return };
        let e = &mut row.entry;
        e.device.name = machine.name.clone();
        e.device.last_seen = Some(ts);
        if let Some(a) = addr {
            e.device.addr = Some(a.to_string());
        }
        e.machine = Machine { last_seen: ts, online: false, ..machine.clone() };
    }

    fn revoke(&self, device_id: &str) {
        if let Some(row) = self.rows.lock().unwrap().get_mut(device_id) {
            row.revoked = true;
        }
    }

    fn last_rx_seq(&self, device_id: &str) -> u64 {
        self.rows.lock().unwrap().get(device_id).map_or(0, |r| r.last_rx)
    }

    fn set_last_rx_seq(&self, device_id: &str, seq: u64) {
        if let Some(row) = self.rows.lock().unwrap().get_mut(device_id) {
            row.last_rx = seq;
        }
    }

    fn last_tx_seq(&self, device_id: &str) -> u64 {
        self.rows.lock().unwrap().get(device_id).map_or(0, |r| r.last_tx)
    }

    fn set_last_tx_seq(&self, device_id: &str, seq: u64) {
        if let Some(row) = self.rows.lock().unwrap().get_mut(device_id) {
            row.last_tx = seq;
        }
    }
}
