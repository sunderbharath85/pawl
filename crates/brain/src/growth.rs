//! Pet growth: default pets and mood/xp changes driven by session events.
//!
//! Mood is clamped to 0..=100, xp only grows (saturating). Finished turns and plans make
//! the pet happy and earn xp; errors, refusals and failed tools make it droop.

use pet_protocol::{
    Agent, EventType, HookPayload, PermissionResolvedPayload, Pet, ResolvedBy, Species, StopPayload,
    StopReason, ToolCallStatus,
};
use serde_json::Value;

use crate::util::fnv1a;

pub const DEFAULT_MOOD: u8 = 70;
pub const MAX_MOOD: u8 = 100;
pub const DEFAULT_SKIN: &str = "default";
pub const DEFAULT_PERSONALITY: &str =
    "loyal, upbeat and a little goofy; loves fetching results and wags at every finished build";
/// XP needed per level.
pub const XP_PER_LEVEL: u32 = 100;

const DOG_NAMES: &[&str] = &[
    "Biscuit", "Pixel", "Rusty", "Mochi", "Nugget", "Waffles", "Ziggy", "Pepper", "Scout", "Bento",
    "Cookie", "Tofu", "Patch", "Dash", "Kernel", "Byte",
];

/// Stable pet id for an agent (one pet per agent).
pub fn pet_id_for(agent_id: &str) -> String {
    format!("pet-{agent_id}")
}

/// Fun default name, seeded by the agent id so it is stable across restarts.
pub fn default_name(agent_id: &str) -> String {
    DOG_NAMES[(fnv1a(agent_id) % DOG_NAMES.len() as u64) as usize].to_string()
}

/// species Dog, fun default name, mood 70, xp 0.
pub fn default_pet(agent: &Agent) -> Pet {
    Pet {
        id: pet_id_for(&agent.id),
        agent_id: agent.id.clone(),
        species: Species::Dog,
        skin: DEFAULT_SKIN.to_string(),
        name: default_name(&agent.id),
        personality: DEFAULT_PERSONALITY.to_string(),
        mood: DEFAULT_MOOD,
        xp: 0,
    }
}

/// Level 1 at 0 xp, +1 every [`XP_PER_LEVEL`].
pub fn level(xp: u32) -> u32 {
    xp / XP_PER_LEVEL + 1
}

pub fn mood_label(mood: u8) -> &'static str {
    match mood {
        85..=u8::MAX => "ecstatic",
        65..=84 => "happy",
        45..=64 => "okay",
        25..=44 => "down",
        _ => "miserable",
    }
}

/// (mood delta, xp delta) for one event.
fn deltas(kind: EventType, payload: &Value) -> (i32, u32) {
    match kind {
        EventType::SessionStart => (2, 1),
        EventType::UserMessage => (1, 0),
        EventType::ToolCall => (0, 1),
        EventType::ToolCallUpdate => {
            let status = payload
                .get("status")
                .and_then(|s| serde_json::from_value::<ToolCallStatus>(s.clone()).ok());
            match status {
                Some(ToolCallStatus::Completed) => (0, 1),
                Some(ToolCallStatus::Failed) => (-2, 0),
                _ => (0, 0),
            }
        }
        EventType::Plan => (0, 2),
        EventType::PermissionResolved => {
            match serde_json::from_value::<PermissionResolvedPayload>(payload.clone()) {
                Ok(p) if p.by == ResolvedBy::User => (2, 0),
                _ => (0, 0),
            }
        }
        EventType::Stop => match serde_json::from_value::<StopPayload>(payload.clone()).map(|p| p.stop_reason) {
            Ok(StopReason::EndTurn) => (5, 10),
            Ok(StopReason::Refusal) => (-6, 0),
            Ok(StopReason::MaxTokens | StopReason::MaxTurnRequests) => (-2, 2),
            Ok(StopReason::Cancelled) => (-1, 0),
            Err(_) => (0, 0),
        },
        EventType::Error => (-8, 0),
        EventType::Hook => {
            let event = serde_json::from_value::<HookPayload>(payload.clone())
                .map(|h| h.hook_event)
                .unwrap_or_default()
                .to_lowercase()
                .replace(['-', '_', ' '], "");
            match event.as_str() {
                "stop" | "agentturncomplete" | "turncomplete" => (3, 5),
                "sessionstart" => (2, 1),
                "userpromptsubmit" | "userpromptsubmitted" => (1, 0),
                "posttooluse" => (0, 1),
                "erroroccurred" | "error" => (-5, 0),
                _ => (0, 0),
            }
        }
        EventType::SessionEnd
        | EventType::AgentThoughtChunk
        | EventType::AgentMessageChunk
        | EventType::RequestPermission
        | EventType::Notification => (0, 0),
    }
}

/// Apply an event to the pet. Returns true if mood or xp changed.
pub fn on_event(pet: &mut Pet, kind: EventType, payload: &serde_json::Value) -> bool {
    let (dm, dxp) = deltas(kind, payload);
    let mood = (i32::from(pet.mood) + dm).clamp(0, i32::from(MAX_MOOD)) as u8;
    let xp = pet.xp.saturating_add(dxp);
    let changed = mood != pet.mood || xp != pet.xp;
    pet.mood = mood;
    pet.xp = xp;
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use pet_protocol::{AgentSource, HookSource, PermissionOutcome};
    use serde_json::json;

    fn agent(id: &str) -> Agent {
        Agent {
            id: id.into(),
            name: id.into(),
            command: vec![],
            env: None,
            version: None,
            description: None,
            icon: None,
            installed: true,
            source: AgentSource::Registry,
            capabilities: None,
        }
    }

    #[test]
    fn default_pet_is_a_dog_with_stable_name() {
        let p = default_pet(&agent("codex"));
        assert_eq!(p.species, Species::Dog);
        assert_eq!(p.mood, 70);
        assert_eq!(p.xp, 0);
        assert_eq!(p.agent_id, "codex");
        assert_eq!(p.id, "pet-codex");
        assert_eq!(p.skin, "default");
        assert!(DOG_NAMES.contains(&p.name.as_str()));
        assert_eq!(p.name, default_pet(&agent("codex")).name);
        // Different agents usually get different names (seeded by id).
        let names: std::collections::BTreeSet<String> = ["codex", "claude-code", "gemini", "copilot", "goose"]
            .iter()
            .map(|id| default_name(id))
            .collect();
        assert!(names.len() >= 3);
    }

    #[test]
    fn events_change_mood_and_xp() {
        let mut p = default_pet(&agent("codex"));
        assert!(on_event(&mut p, EventType::Stop, &json!({"stopReason": "end_turn"})));
        assert_eq!((p.mood, p.xp), (75, 10));
        assert!(on_event(&mut p, EventType::Error, &json!({"message": "boom"})));
        assert_eq!(p.mood, 67);
        assert!(on_event(&mut p, EventType::ToolCall, &json!({"toolCallId": "t"})));
        assert_eq!(p.xp, 11);
        assert!(!on_event(&mut p, EventType::AgentMessageChunk, &json!({"text": "hi"})));
        assert!(on_event(&mut p, EventType::ToolCallUpdate, &json!({"toolCallId": "t", "status": "failed"})));
        assert_eq!(p.mood, 65);
        let resolved = serde_json::to_value(PermissionResolvedPayload {
            request_id: "r".into(),
            outcome: PermissionOutcome::Cancelled,
            by: ResolvedBy::User,
        })
        .unwrap();
        assert!(on_event(&mut p, EventType::PermissionResolved, &resolved));
        assert_eq!(p.mood, 67);
        let hook = serde_json::to_value(HookPayload {
            source: HookSource::Codex,
            hook_event: "agent-turn-complete".into(),
            message: None,
            raw: json!({}),
        })
        .unwrap();
        assert!(on_event(&mut p, EventType::Hook, &hook));
        assert_eq!((p.mood, p.xp), (70, 16));
        // Malformed payloads are ignored.
        assert!(!on_event(&mut p, EventType::Stop, &json!("nope")));
    }

    #[test]
    fn mood_and_xp_are_clamped() {
        let mut p = default_pet(&agent("codex"));
        for _ in 0..50 {
            on_event(&mut p, EventType::Stop, &json!({"stopReason": "end_turn"}));
        }
        assert_eq!(p.mood, 100);
        assert!(!on_event(&mut p, EventType::UserMessage, &json!({"text": "x"})));
        for _ in 0..50 {
            on_event(&mut p, EventType::Error, &json!({"message": "x"}));
        }
        assert_eq!(p.mood, 0);
        assert!(!on_event(&mut p, EventType::Error, &json!({"message": "x"})));
        p.xp = u32::MAX - 1;
        on_event(&mut p, EventType::Stop, &json!({"stopReason": "end_turn"}));
        assert_eq!(p.xp, u32::MAX);
        assert_eq!(level(0), 1);
        assert_eq!(level(250), 3);
        assert_eq!(mood_label(70), "happy");
    }
}
