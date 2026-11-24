//! krypto-peer: device keys, invites, NaCl-box envelopes and the hub-to-hub peer link.
//! API contract: docs/CONTRACTS.md "Step 2 — Pairing + LAN presence" and "Step 6 — Relay".
//!
//! Security model, in short:
//! - Every frame is a `PeerEnvelope` whose `box` is a NaCl box from the sender's long-term X25519
//!   key to the receiver's. The nonce binds `seq` and the direction (see [`envelope`]).
//! - A peer is either in the roster (its key is known) or it is pairing with a live one-time invite
//!   code. Anything else is closed without a reply.
//! - Frames go over a direct LAN WebSocket or, when there is none, through the relay (see
//!   [`PeerNode::set_relay`]). The relay sees public keys and ciphertext only; a relayed frame must come
//!   from the roster key of the device it claims to be from.
//! - Secrets (the device secret key, invite codes and URIs) are never logged.

pub mod b64;
mod envelope;
mod invite;
mod io;
mod keys;
mod limits;
mod node;
mod relay;
mod roster;

pub use b64::B64Error;
pub use envelope::{open, seal, OpenError};
pub use invite::{Invite, InviteError};
pub use keys::DeviceKeys;
pub use node::{JoinError, Joined, PeerEvent, PeerNode, PeerTimings, SendError, MAX_SEND_BODY_BYTES};
pub use relay::{RelayConfig, RelayConfigError};
// The wire types the hub reports these with.
pub use krypto_protocol::{LinkPath, RelayStatusInfo};
pub use roster::{MemoryRoster, Roster, RosterEntry};
