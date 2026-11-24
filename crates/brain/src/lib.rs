//! krypto-brain: permission rules, chat bot, watchdog and pet growth for Krypto.
//!
//! The public API is fixed by `docs/CONTRACTS.md` (section `krypto-brain`).
//!
//! Safety invariant: actions ([`chat::BrainAction`]) only ever come from the
//! deterministic intent parser. LLM output (Anthropic or Ollama) is used for
//! wording only and can never trigger an action.

pub mod chat;
pub mod growth;
pub mod rules;
pub mod watchdog;

mod util;
