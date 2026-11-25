//! Pawl's A2A (Agent2Agent, <https://a2a-protocol.org>) layer.
//!
//! PLAN.md "A2A orchestration": the orchestrator (the default pet) talks to every sub-agent the A2A
//! way. ACP agents (Claude Code, Codex, Copilot ...) keep speaking ACP to the hub; this crate gives
//! each ACP session an A2A view:
//!
//! - one A2A `contextId` per session (the session's global id),
//! - one [`a2a::Task`] per delegation (prompt turn), with a global id `${machineId}:${uuid}` so the
//!   owning machine can be read off it like every other Pawl id,
//! - ACP updates become task status / artifact updates ([`mapping`]),
//! - an ACP `request_permission` becomes `TASK_STATE_INPUT_REQUIRED` with a Structured Input
//!   schema whose enum is the ACP option ids ([`structured_input`]),
//! - stop reasons become terminal states: end_turn -> COMPLETED, cancelled -> CANCELED,
//!   refusal -> REJECTED, errors -> FAILED.
//!
//! The SDK crates are pre-1.0 and pinned exactly; the rest of Pawl uses the types re-exported here
//! ([`spec`]) and the functions of this crate, so an SDK upgrade stays local.

pub mod ext;
pub mod mapping;
pub mod structured_input;
pub mod tasks;

/// The A2A v1 wire types (crate `a2a-lf`, pinned).
pub use a2a as spec;

pub use ext::{DelegateParams, DELEGATE_EXT, STRUCTURED_INPUT_EXT};
pub use tasks::{Caller, TaskRegistry, TaskUpdate};

/// The A2A protocol version Pawl speaks natively (`A2A-Version` header value).
pub const A2A_VERSION: &str = a2a::VERSION;

/// The legacy protocol version assumed for requests without an `A2A-Version` header (spec 3.6.2).
pub const A2A_LEGACY_VERSION: &str = "0.3";
