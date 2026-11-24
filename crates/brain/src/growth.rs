//! Pet growth: default pets and mood/xp changes driven by session events.
//!
//! Mood is clamped to 0..=100, xp only grows (saturating). Finished turns and plans make
//! the pet happy and earn xp; errors, refusals and failed tools make it droop.

use krypto_protocol::{
    global_id, Agent, EventType, HookPayload, PermissionResolvedPayload, Pet, ResolvedBy, Species, StopPayload,
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

pub const DOG_NAMES: &[&str] = &[
    "Biscuit", "Pixel", "Rusty", "Mochi", "Nugget", "Waffles", "Ziggy", "Pepper", "Scout", "Bento",
    "Cookie", "Tofu", "Patch", "Dash", "Kernel", "Byte", "Pickles", "Noodle", "Maple", "Juniper",
    "Pretzel", "Clover", "Sprocket", "Toffee", "Bagel", "Widget", "Ramen", "Hazel", "Gizmo", "Sushi",
    "Taco", "Ollie",
];

/// Stable global pet id for an agent on a machine (one pet per agent per machine):
/// `${machine_id}:pet-${agent_id}`.
pub fn pet_id_for(machine_id: &str, agent_id: &str) -> String {
    global_id(machine_id, format!("pet-{agent_id}"))
}

/// Fun default name, seeded by `machine_id + "/" + agent_id` so it is stable across restarts and
/// differs between machines. Names in `taken` (case-insensitive, surrounding whitespace ignored)
/// are skipped by walking the list from the seeded slot; once every name is taken, the walk
/// repeats with numbered variants ("Biscuit II", "Biscuit III", ...).
pub fn default_name(machine_id: &str, agent_id: &str, taken: &[String]) -> String {
    let taken: std::collections::HashSet<String> = taken.iter().map(|n| name_key(n)).collect();
    let n = DOG_NAMES.len();
    let start = (fnv1a(&format!("{machine_id}/{agent_id}")) % n as u64) as usize;
    // Each tier offers `n` fresh names and `taken` is finite, so tier `taken.len() / n + 1` at the
    // latest has a free one; the loop bound is just that guarantee.
    let mut tier = 1;
    loop {
        for i in 0..n {
            let base = DOG_NAMES[(start + i) % n];
            let name = if tier == 1 { base.to_string() } else { format!("{base} {}", roman(tier)) };
            if !taken.contains(&name_key(&name)) {
                return name;
            }
        }
        tier += 1;
    }
}

fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Upper-case Roman numeral for `n >= 1` (name tiers: 2 -> "II").
fn roman(mut n: usize) -> String {
    const TABLE: &[(usize, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for &(v, s) in TABLE {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

/// species Dog, fun default name, mood 70, xp 0. Id `${machine_id}:pet-${agent_id}`.
/// `taken_names`: names of every known pet on every machine, so paired machines never show two
/// pets with the same name.
pub fn default_pet(agent: &Agent, taken_names: &[String]) -> Pet {
    Pet {
        id: pet_id_for(&agent.machine_id, &agent.id),
        machine_id: agent.machine_id.clone(),
        agent_id: agent.id.clone(),
        species: Species::Dog,
        skin: DEFAULT_SKIN.to_string(),
        name: default_name(&agent.machine_id, &agent.id, taken_names),
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
    use krypto_protocol::{AgentSource, HookSource, PermissionOutcome};
    use serde_json::json;

    const M1: &str = "abcdef012345";
    const M2: &str = "zz9988776655";
    const AGENTS: [&str; 6] = ["codex", "claude-code", "gemini", "copilot", "goose", "auggie"];

    fn agent(id: &str) -> Agent {
        agent_on(M1, id)
    }

    fn agent_on(machine_id: &str, id: &str) -> Agent {
        Agent {
            id: id.into(),
            machine_id: machine_id.into(),
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
        let p = default_pet(&agent("codex"), &[]);
        assert_eq!(p.species, Species::Dog);
        assert_eq!(p.mood, 70);
        assert_eq!(p.xp, 0);
        assert_eq!(p.agent_id, "codex");
        assert_eq!(p.machine_id, M1);
        assert_eq!(p.id, "abcdef012345:pet-codex");
        assert_eq!(krypto_protocol::parse_global_id(&p.id), Some((M1, "pet-codex")));
        assert_eq!(p.skin, "default");
        assert!(DOG_NAMES.contains(&p.name.as_str()));
        assert_eq!(p, default_pet(&agent("codex"), &[]), "deterministic");
        // Different agents usually get different names (seeded by machine + id).
        let names: std::collections::BTreeSet<String> = AGENTS.iter().map(|id| default_name(M1, id, &[])).collect();
        assert!(names.len() >= 3);
    }

    #[test]
    fn same_agent_on_two_machines_gets_different_names() {
        // The machine id alone changes the seed for most agents...
        let differ = AGENTS.iter().filter(|id| default_name(M1, id, &[]) != default_name(M2, id, &[])).count();
        assert!(differ >= 4, "machine id should change the seed ({differ}/6 differ)");
        // ...and with the roster of known names (what the hub passes) they never collide.
        for id in AGENTS {
            let first = default_pet(&agent_on(M1, id), &[]);
            let second = default_pet(&agent_on(M2, id), std::slice::from_ref(&first.name));
            assert_ne!(first.name, second.name, "{id}");
            assert_ne!(first.id, second.id, "{id}");
            assert_eq!(second.machine_id, M2);
            assert_eq!(second.id, format!("{M2}:pet-{id}"));
        }
        // A whole roster across both machines stays unique.
        let mut taken: Vec<String> = Vec::new();
        for m in [M1, M2] {
            for id in AGENTS {
                let p = default_pet(&agent_on(m, id), &taken);
                assert!(!taken.iter().any(|t| t.eq_ignore_ascii_case(&p.name)), "{} reused", p.name);
                taken.push(p.name);
            }
        }
    }

    #[test]
    fn taken_names_are_skipped_case_insensitively() {
        let seeded = default_name(M1, "codex", &[]);
        let next = default_name(M1, "codex", &[seeded.to_uppercase()]);
        assert_ne!(next, seeded);
        // Walks the list in order from the seeded slot.
        let i = DOG_NAMES.iter().position(|n| *n == seeded).unwrap();
        assert_eq!(next, DOG_NAMES[(i + 1) % DOG_NAMES.len()]);
        assert_eq!(default_name(M1, "codex", &[format!("  {}  ", seeded.to_lowercase())]), next);
        // Unrelated taken names do not move the pick.
        assert_eq!(default_name(M1, "codex", &["Nobody".into()]), seeded);
    }

    #[test]
    fn exhausted_list_falls_back_to_numbered_names() {
        let mut taken: Vec<String> = DOG_NAMES.iter().map(|n| n.to_string()).collect();
        let seeded = default_name(M1, "codex", &[]);
        let second = default_name(M1, "codex", &taken);
        assert_eq!(second, format!("{seeded} II"));
        taken.push(second.to_lowercase());
        let i = DOG_NAMES.iter().position(|n| *n == seeded).unwrap();
        assert_eq!(default_name(M1, "codex", &taken), format!("{} II", DOG_NAMES[(i + 1) % DOG_NAMES.len()]));
        taken.extend(DOG_NAMES.iter().map(|n| format!("{n} II")));
        assert_eq!(default_name(M1, "codex", &taken), format!("{seeded} III"));
        assert_eq!(roman(4), "IV");
        assert_eq!(roman(14), "XIV");
    }

    #[test]
    fn events_change_mood_and_xp() {
        let mut p = default_pet(&agent("codex"), &[]);
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
            by_machine_id: None,
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
        let mut p = default_pet(&agent("codex"), &[]);
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
