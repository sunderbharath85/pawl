//! Pawl's own Agent Card (spec section 8): the orchestrator as an A2A agent.
//!
//! The public card lists orchestrator skills (status, delegate a task to an agent in a repo, prompt
//! a session), never raw ACP methods and never this machine's agents, repos or folders. The extended
//! card (spec 3.1.11, behind the bearer token) adds a skill per installed agent with the repo names
//! it can be sent to.

use std::collections::HashMap;

use a2a::{
    AgentCapabilities, AgentCard, AgentExtension, AgentInterface, AgentProvider, AgentSkill, HttpAuthSecurityScheme, SecurityScheme,
    TRANSPORT_PROTOCOL_HTTP_JSON, TRANSPORT_PROTOCOL_JSONRPC,
};
use serde_json::{json, Value};

use crate::ext::{DELEGATE_EXT, STRUCTURED_INPUT_EXT};
use crate::structured_input::JSONSCHEMA_DRAFT;

/// Name of the bearer security scheme in the card.
pub const BEARER_SCHEME: &str = "bearer";

const HOME: &str = "https://sunderbharath85.github.io/pawl/";

/// What the hub knows when it builds its card.
#[derive(Debug, Clone, Default)]
pub struct CardInfo {
    /// Pawl's version.
    pub version: String,
    /// `http://127.0.0.1:<port>` (no trailing slash).
    pub base_url: String,
    /// Installed agents (id, display name): extended card only.
    pub agents: Vec<(String, String)>,
    /// Repo names of this machine's repo index: extended card only.
    pub repos: Vec<String>,
}

fn skill(id: &str, name: &str, description: &str, tags: &[&str], examples: &[&str]) -> AgentSkill {
    AgentSkill {
        id: id.into(),
        name: name.into(),
        description: description.into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        examples: (!examples.is_empty()).then(|| examples.iter().map(|e| e.to_string()).collect()),
        input_modes: None,
        output_modes: None,
        security_requirements: None,
    }
}

fn base_skills() -> Vec<AgentSkill> {
    vec![
        skill(
            "status",
            "Status",
            "What the coding agents on this machine are doing: sessions, delegations and what waits for the user (ListTasks / GetTask).",
            &["status", "orchestrator"],
            &["ListTasks with no filter"],
        ),
        skill(
            "delegate",
            "Delegate a task",
            "Start a coding agent in a folder this machine already works in, with a first prompt. SendMessage without a contextId, with message metadata under the delegate extension: {agentId, cwd}. Tool calls the agent wants to make ask the user on this machine.",
            &["delegate", "coding", "orchestrator"],
            &["run the tests (metadata: {agentId: \"<agent>\", cwd: \"<folder>\"})"],
        ),
        skill(
            "prompt",
            "Prompt a session",
            "Send a follow-up to a session this caller started: SendMessage with its contextId. Each prompt is a new task.",
            &["prompt", "coding"],
            &["now fix the failing test"],
        ),
    ]
}

fn extensions() -> Vec<AgentExtension> {
    vec![
        AgentExtension {
            uri: STRUCTURED_INPUT_EXT.into(),
            description: Some("Tool-call confirmations are INPUT_REQUIRED with a JSON Schema whose enum is the agent's permission option ids.".into()),
            required: Some(false),
            params: Some(HashMap::from([("jsonschema_draft".to_string(), json!(JSONSCHEMA_DRAFT))])),
        },
        AgentExtension {
            uri: DELEGATE_EXT.into(),
            description: Some("message.metadata[uri] = {agentId, cwd}: start a session of that agent in that folder.".into()),
            required: Some(false),
            params: None,
        },
    ]
}

/// The card. `extended`: the authenticated extended card (per-agent skills).
pub fn pawl_card(info: &CardInfo, extended: bool) -> AgentCard {
    let mut skills = base_skills();
    if extended {
        let repos = if info.repos.is_empty() {
            "No repos are indexed yet.".to_string()
        } else {
            let mut names = info.repos.iter().take(50).cloned().collect::<Vec<_>>().join(", ");
            if info.repos.len() > 50 {
                names.push_str(", ...");
            }
            format!("Repos: {names}.")
        };
        for (id, name) in &info.agents {
            skills.push(skill(
                &format!("delegate:{id}"),
                &format!("Delegate to {name}"),
                &format!("Start {name} (agentId \"{id}\") in a folder of this machine. {repos}"),
                &["delegate", "agent"],
                &[],
            ));
        }
    }
    let url = info.base_url.trim_end_matches('/').to_string();
    let jsonrpc = format!("{url}/");
    let interface = |u: &str, binding: &str, version: &str| AgentInterface { url: u.into(), protocol_binding: binding.into(), protocol_version: version.into(), tenant: None };
    AgentCard {
        name: "Pawl".into(),
        description: "Pawl, the orchestrator pet: delegates coding tasks to the agents on this machine (Claude Code, Codex, Copilot, ...) and reports their status. Every call needs a bearer token from Pawl's Settings.".into(),
        version: info.version.clone(),
        supported_interfaces: vec![
            interface(&jsonrpc, TRANSPORT_PROTOCOL_JSONRPC, crate::A2A_VERSION),
            interface(&url, TRANSPORT_PROTOCOL_HTTP_JSON, crate::A2A_VERSION),
            interface(&jsonrpc, TRANSPORT_PROTOCOL_JSONRPC, crate::A2A_LEGACY_VERSION),
        ],
        capabilities: AgentCapabilities {
            streaming: Some(true),
            push_notifications: Some(false),
            extensions: Some(extensions()),
            extended_agent_card: Some(true),
        },
        default_input_modes: vec!["text/plain".into(), "application/json".into()],
        default_output_modes: vec!["text/plain".into(), "application/json".into()],
        skills,
        provider: Some(AgentProvider { organization: "Pawl".into(), url: HOME.into() }),
        documentation_url: Some(HOME.into()),
        icon_url: None,
        security_schemes: Some(HashMap::from([(
            BEARER_SCHEME.to_string(),
            SecurityScheme::HttpAuth(HttpAuthSecurityScheme {
                scheme: "bearer".into(),
                description: Some("A caller token from Pawl's Settings (A2A server).".into()),
                bearer_format: None,
            }),
        )])),
        security_requirements: Some(vec![HashMap::from([(BEARER_SCHEME.to_string(), Vec::new())])]),
        signatures: None,
    }
}

/// The card as JSON for a reader of `version` (`None` = the request named no version): a 1.x reader
/// gets the strict 1.0 card, anyone else the dual card both 1.0 and 0.3 readers parse.
pub fn card_json(card: &AgentCard, version: Option<&str>) -> Value {
    let v = serde_json::to_value(card).unwrap_or(Value::Null);
    match version.and_then(major) {
        Some(1) => v,
        _ => crate::compat::card_dual(v),
    }
}

/// The major version of an `A2A-Version` value ("1.0" -> 1, "0.3" -> 0).
pub fn major(v: &str) -> Option<u32> {
    v.trim().split('.').next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> CardInfo {
        CardInfo {
            version: "0.1.1".into(),
            base_url: "http://127.0.0.1:47802".into(),
            agents: vec![("codex".into(), "Codex".into())],
            repos: vec!["api".into(), "web".into()],
        }
    }

    #[test]
    fn the_public_card_lists_orchestrator_skills_only() {
        let c = pawl_card(&info(), false);
        let ids: Vec<&str> = c.skills.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["status", "delegate", "prompt"]);
        let text = serde_json::to_string(&c).unwrap();
        assert!(!text.contains("codex") && !text.contains("\"api\""), "no agents or repos in public: {text}");
        assert_eq!(c.capabilities.streaming, Some(true));
        assert_eq!(c.capabilities.push_notifications, Some(false));
        assert_eq!(c.capabilities.extended_agent_card, Some(true));
        assert_eq!(c.supported_interfaces[0].url, "http://127.0.0.1:47802/");
        assert_eq!(c.supported_interfaces[1].protocol_binding, "HTTP+JSON");
        assert_eq!(c.supported_interfaces[2].protocol_version, "0.3");
        // Round-trips through the SDK.
        let back: AgentCard = serde_json::from_str(&text).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn the_extended_card_adds_a_skill_per_agent() {
        let c = pawl_card(&info(), true);
        let s = c.skills.iter().find(|s| s.id == "delegate:codex").unwrap();
        assert!(s.description.contains("api, web"));
    }

    #[test]
    fn readers_get_the_card_shape_of_their_version() {
        let c = pawl_card(&info(), false);
        let strict = card_json(&c, Some("1.0"));
        assert!(strict.get("securitySchemes").is_some());
        assert!(strict.get("url").is_none());
        let dual = card_json(&c, None);
        assert!(dual.get("securitySchemes").is_none());
        assert_eq!(dual["url"], "http://127.0.0.1:47802/");
        assert!(serde_json::from_value::<AgentCard>(dual).is_ok());
        assert_eq!(card_json(&c, Some("0.3"))["protocolVersion"], "0.3.0");
        assert_eq!(major(" 1.0 "), Some(1));
        assert_eq!(major("x"), None);
    }
}
