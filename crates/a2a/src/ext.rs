//! A2A extensions Pawl uses, and helpers for extension metadata.
//!
//! - Structured Input (experimental, Google): <https://github.com/a2aproject/experimental-ext-structured-input>
//!   (v1 spec: `v1/spec.md`). Used for tool-call confirmations (ACP `request_permission`).
//! - Pawl delegate: how a caller asks the orchestrator to start a session of a given agent in a
//!   folder (`message.metadata[DELEGATE_EXT]`). A message with a `contextId` (an existing session)
//!   needs no delegate metadata.

use std::collections::HashMap;

use a2a::Message;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Structured Input extension URI (the only URI the extension accepts, spec section 2).
pub const STRUCTURED_INPUT_EXT: &str = "https://github.com/a2aproject/experimental-ext-structured-input/tree/main/v1";

/// Pawl's delegate extension URI (message metadata key; declared in Pawl's Agent Card).
pub const DELEGATE_EXT: &str = "https://sunderbharath85.github.io/pawl/a2a/delegate/v1";

/// `message.metadata[DELEGATE_EXT]`: start a new session (no `contextId`) of `agent_id` in `cwd`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegateParams {
    /// Agent to start ("claude-code", "codex", ...). Required to start a session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// Absolute folder the session runs in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The machine that should run it (default: the machine that receives the message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
    /// Set by a paired hub that showed the user a confirmation for this start (step-4 rule; a
    /// statement by a paired hub, never honoured from an outside caller).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmed: Option<bool>,
}

impl DelegateParams {
    /// The delegate parameters of `msg`, if any (a malformed object counts as none).
    pub fn of(msg: &Message) -> Option<DelegateParams> {
        let v = msg.metadata.as_ref()?.get(DELEGATE_EXT)?;
        serde_json::from_value(v.clone()).ok()
    }

    /// Write these parameters into `msg.metadata`.
    pub fn attach(&self, msg: &mut Message) {
        let v = serde_json::to_value(self).unwrap_or(Value::Null);
        msg.metadata.get_or_insert_with(HashMap::new).insert(DELEGATE_EXT.to_string(), v);
    }
}

/// The extension metadata object of a part (`part.metadata[uri]`), if it is an object.
pub fn part_ext<'a>(metadata: &'a Option<HashMap<String, Value>>, uri: &str) -> Option<&'a serde_json::Map<String, Value>> {
    metadata.as_ref()?.get(uri)?.as_object()
}

/// `{ uri: obj }` as part metadata.
pub fn ext_metadata(uri: &str, obj: Value) -> HashMap<String, Value> {
    HashMap::from([(uri.to_string(), obj)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use a2a::{Part, Role};

    #[test]
    fn delegate_params_round_trip_through_message_metadata() {
        let mut m = Message::new(Role::User, vec![Part::text("run the tests")]);
        assert_eq!(DelegateParams::of(&m), None);
        let p = DelegateParams { agent_id: Some("codex".into()), cwd: Some("/repo".into()), ..Default::default() };
        p.attach(&mut m);
        assert_eq!(DelegateParams::of(&m), Some(p));
        let wire = serde_json::to_value(&m).unwrap();
        assert_eq!(wire["metadata"][DELEGATE_EXT]["agentId"], "codex");
        assert!(wire["metadata"][DELEGATE_EXT].get("machineId").is_none());
    }

    #[test]
    fn malformed_delegate_metadata_counts_as_none() {
        let mut m = Message::new(Role::User, vec![Part::text("x")]);
        m.metadata = Some(ext_metadata(DELEGATE_EXT, serde_json::json!({ "agentId": 7 })));
        assert_eq!(DelegateParams::of(&m), None);
    }
}
