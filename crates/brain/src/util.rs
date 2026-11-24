//! Small shared helpers: hashing, paths, text and time formatting, agent names.

use pet_protocol::{Agent, UnixMs};

/// Stable 64-bit FNV-1a hash (std's `DefaultHasher` is not stable across releases).
pub(crate) fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Last path segment of a cwd (`E:\work\ai-pet` -> `ai-pet`). Falls back to the input.
pub(crate) fn basename(path: &str) -> &str {
    let t = path.trim().trim_end_matches(['/', '\\']);
    match t.rsplit(['/', '\\']).next() {
        Some(b) if !b.is_empty() => b,
        _ => path.trim(),
    }
}

/// Comparison key for directories: forward slashes, no trailing slash, case-folded on
/// case-insensitive platforms.
pub(crate) fn path_key(p: &str) -> String {
    let mut s = p.trim().replace('\\', "/");
    while s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    if cfg!(any(windows, target_os = "macos")) {
        s = s.to_lowercase();
    }
    s
}

/// Truncate to at most `max` chars, adding an ellipsis when cut.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    while out.ends_with(char::is_whitespace) {
        out.pop();
    }
    out.push('…');
    out
}

/// Collapse all whitespace (including newlines) into single spaces.
pub(crate) fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// Human duration: "under a minute", "7 min", "2 h 5 min", "3 days".
pub(crate) fn fmt_duration(ms: i64) -> String {
    let mins = ms.max(0) / 60_000;
    if mins < 1 {
        return "under a minute".into();
    }
    if mins < 60 {
        return format!("{mins} min");
    }
    let h = mins / 60;
    let m = mins % 60;
    if h >= 48 {
        return format!("{} days", h / 24);
    }
    if m == 0 {
        format!("{h} h")
    } else {
        format!("{h} h {m} min")
    }
}

/// "just now" / "7 min ago" / "2 h ago".
pub(crate) fn ago(ts: UnixMs, now: UnixMs) -> String {
    let d = now - ts;
    if d < 60_000 {
        "just now".into()
    } else {
        format!("{} ago", fmt_duration(d))
    }
}

fn capitalize(w: &str) -> String {
    let mut c = w.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// `claude-code` -> `Claude Code`, `codex` -> `Codex`.
pub(crate) fn prettify_agent_id(id: &str) -> String {
    let id = id.trim();
    let id = id.strip_suffix("-acp").unwrap_or(id);
    let words: Vec<String> = id
        .split(['-', '_', ' '])
        .filter(|w| !w.is_empty())
        .map(capitalize)
        .collect();
    if words.is_empty() {
        "Agent".into()
    } else {
        words.join(" ")
    }
}

/// Display name for an agent id: discovery name if known, else a prettified id.
pub(crate) fn agent_name(id: &str, agents: &[Agent]) -> String {
    agents
        .iter()
        .find(|a| a.id == id)
        .map(|a| a.name.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| prettify_agent_id(id))
}

/// Short lowercase handle users can type for an agent ("claude", "codex").
pub(crate) fn agent_handle(id: &str, agents: &[Agent]) -> String {
    let name = agent_name(id, agents).to_lowercase();
    name.split_whitespace()
        .find(|w| w.len() >= 3 && !matches!(*w, "github" | "openai" | "google" | "the"))
        .map(str::to_string)
        .unwrap_or_else(|| id.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_handles_both_separators() {
        assert_eq!(basename("E:\\workspace\\ai-pet"), "ai-pet");
        assert_eq!(basename("/home/me/ai-pet/"), "ai-pet");
        assert_eq!(basename("ai-pet"), "ai-pet");
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_duration(30_000), "under a minute");
        assert_eq!(fmt_duration(7 * 60_000), "7 min");
        assert_eq!(fmt_duration(125 * 60_000), "2 h 5 min");
        assert_eq!(fmt_duration(72 * 3_600_000), "3 days");
    }

    #[test]
    fn agent_names() {
        assert_eq!(prettify_agent_id("claude-code"), "Claude Code");
        assert_eq!(prettify_agent_id("codex-acp"), "Codex");
        assert_eq!(agent_handle("claude-code", &[]), "claude");
        assert_eq!(truncate("abcdef", 4), "abc…");
        assert_eq!(fnv1a("codex"), fnv1a("codex"));
    }
}
