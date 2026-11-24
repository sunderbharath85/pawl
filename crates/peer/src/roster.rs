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
///   device that is not stored yet they may be a no-op. Unknown ids read as 0. Revoked rows keep (and
///   update) their counters.
/// - `get_revoked` returns the entry of a *revoked* device (its last key), so the node can answer that
///   device's hello with `bye{revoked}` when the device missed the revoke. The default (None) only
///   loses that answer.
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
    /// The stored entry of a revoked device; None for a paired or unknown id.
    fn get_revoked(&self, device_id: &str) -> Option<RosterEntry> {
        let _ = device_id;
        None
    }
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

    fn get_revoked(&self, device_id: &str) -> Option<RosterEntry> {
        self.rows.lock().unwrap().get(device_id).filter(|r| r.revoked).map(|r| r.entry.clone())
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

#[cfg(test)]
mod tests {
    use super::*;
    use krypto_protocol::{DeviceKind, MachineOs};

    fn entry(id: &str) -> RosterEntry {
        RosterEntry {
            device: Device {
                id: id.into(),
                kind: DeviceKind::Hub,
                name: "x".into(),
                public_key: "pk".into(),
                paired_at: 1,
                last_seen: None,
                addr: None,
                path: None,
                introduced_by: None,
                os: None,
            },
            machine: Machine {
                id: id.into(),
                name: "x".into(),
                os: MachineOs::current(),
                last_seen: 0,
                online: false,
                hub_version: None,
                user_active_at: None,
            },
        }
    }

    #[test]
    fn get_revoked_returns_revoked_rows_only() {
        let r = MemoryRoster::new();
        r.upsert(entry("aaaaaaaaaaaa"));
        assert!(r.get_revoked("aaaaaaaaaaaa").is_none(), "still paired");
        r.set_last_rx_seq("aaaaaaaaaaaa", 7);
        r.revoke("aaaaaaaaaaaa");
        assert!(r.get("aaaaaaaaaaaa").is_none());
        assert_eq!(r.get_revoked("aaaaaaaaaaaa").unwrap().device.public_key, "pk");
        assert_eq!(r.last_rx_seq("aaaaaaaaaaaa"), 7, "counters survive the revoke");
        assert!(r.get_revoked("bbbbbbbbbbbb").is_none(), "unknown");
        r.upsert(entry("aaaaaaaaaaaa"));
        assert!(r.get_revoked("aaaaaaaaaaaa").is_none(), "paired again");
    }

    /// A roster written before `get_revoked` existed (the default answers None).
    struct Legacy(MemoryRoster);

    impl Roster for Legacy {
        fn get(&self, id: &str) -> Option<RosterEntry> {
            self.0.get(id)
        }
        fn list(&self) -> Vec<RosterEntry> {
            self.0.list()
        }
        fn upsert(&self, e: RosterEntry) {
            self.0.upsert(e)
        }
        fn update_seen(&self, id: &str, m: &Machine, addr: Option<&str>, ts: UnixMs) {
            self.0.update_seen(id, m, addr, ts)
        }
        fn revoke(&self, id: &str) {
            self.0.revoke(id)
        }
        fn last_rx_seq(&self, id: &str) -> u64 {
            self.0.last_rx_seq(id)
        }
        fn set_last_rx_seq(&self, id: &str, seq: u64) {
            self.0.set_last_rx_seq(id, seq)
        }
        fn last_tx_seq(&self, id: &str) -> u64 {
            self.0.last_tx_seq(id)
        }
        fn set_last_tx_seq(&self, id: &str, seq: u64) {
            self.0.set_last_tx_seq(id, seq)
        }
    }

    #[test]
    fn get_revoked_defaults_to_none() {
        let r = Legacy(MemoryRoster::new());
        r.upsert(entry("aaaaaaaaaaaa"));
        r.revoke("aaaaaaaaaaaa");
        assert!(r.get_revoked("aaaaaaaaaaaa").is_none());
    }
}
