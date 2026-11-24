//! krypto-peer: device keys, invites, NaCl-box envelopes and the hub-to-hub peer link.
//! API contract: docs/CONTRACTS.md "Step 2 — Pairing + LAN presence".
//!
//! Security model, in short:
//! - Every frame is a `PeerEnvelope` whose `box` is a NaCl box from the sender's long-term X25519
//!   key to the receiver's. The nonce binds `seq` and the direction (see [`envelope`]).
//! - A peer is either in the roster (its key is known) or it is pairing with a live one-time invite
//!   code. Anything else is closed without a reply.
//! - Secrets (the device secret key, invite codes and URIs) are never logged.

pub mod b64;
mod envelope;
mod invite;
mod io;
mod keys;
mod limits;
mod node;
mod roster;

pub use b64::B64Error;
pub use envelope::{open, seal, OpenError};
pub use invite::{Invite, InviteError};
pub use keys::DeviceKeys;
pub use node::{JoinError, PeerEvent, PeerNode, PeerTimings};
pub use roster::{MemoryRoster, Roster, RosterEntry};
