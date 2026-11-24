//! Machine names for replies. With a single known machine replies never mention it; once a
//! second machine is known (paired hub, mirrored session, remote agent or request), every
//! agent mention names its machine: "Claude Code on Mac mini is working on ...".

use std::collections::{BTreeSet, HashMap};

use pawl_protocol::{Machine, Session};

use super::BrainContext;

#[derive(Debug, Clone, Default)]
pub(crate) struct MachineNames {
    /// machine id -> display name.
    names: HashMap<String, String>,
    /// More than one distinct machine id is known.
    multi: bool,
}

fn display(m: &Machine) -> String {
    let name = m.name.trim();
    if name.is_empty() {
        m.id.clone()
    } else {
        name.to_string()
    }
}

impl MachineNames {
    /// Every machine the context knows: `machine`, `machines`, and the machine ids carried by
    /// its agents, sessions and pending requests. `machine` is authoritative for its own name.
    pub(crate) fn from_ctx(ctx: &BrainContext) -> Self {
        let mut names: HashMap<String, String> = HashMap::new();
        for m in std::iter::once(&ctx.machine).chain(&ctx.machines) {
            if !m.id.is_empty() {
                names.entry(m.id.clone()).or_insert_with(|| display(m));
            }
        }
        let ids: BTreeSet<&str> = names
            .keys()
            .map(String::as_str)
            .chain(ctx.agents.iter().map(|a| a.machine_id.as_str()))
            .chain(ctx.sessions.iter().map(|s| s.machine_id.as_str()))
            .chain(ctx.pending.iter().map(|r| r.machine_id.as_str()))
            .filter(|id| !id.is_empty())
            .collect();
        let multi = ids.len() > 1;
        Self { names, multi }
    }

    /// Only the sessions are known (no names): machines are labelled by id when they differ.
    pub(crate) fn from_sessions(sessions: &[Session]) -> Self {
        let ids: BTreeSet<&str> = sessions.iter().map(|s| s.machine_id.as_str()).filter(|id| !id.is_empty()).collect();
        Self { names: HashMap::new(), multi: ids.len() > 1 }
    }

    pub(crate) fn is_multi(&self) -> bool {
        self.multi
    }

    /// Display name of `machine_id`, or None while only one machine is known.
    pub(crate) fn label(&self, machine_id: &str) -> Option<String> {
        if !self.multi || machine_id.is_empty() {
            return None;
        }
        Some(self.names.get(machine_id).cloned().unwrap_or_else(|| machine_id.to_string()))
    }

    /// " on Mac mini", or "" while only one machine is known.
    pub(crate) fn on(&self, machine_id: &str) -> String {
        self.label(machine_id).map(|n| format!(" on {n}")).unwrap_or_default()
    }
}
