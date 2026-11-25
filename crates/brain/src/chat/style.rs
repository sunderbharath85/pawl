//! Reply style (`bot.style`). PLAIN (default): friendly, short, normal sentences; no stage directions in
//! asterisks, no barks or pet sounds, at most ONE emoji, first, from a fixed set chosen from the situation
//! ([`Icon`]), never inside the text. PLAYFUL: the pet's voice as before (sounds and actions).
//!
//! [`plain_text`] is applied to every plain reply, deterministic or model-written, so a model that slips
//! into role-play is cleaned up too.

/// The one emoji a plain reply may start with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Icon {
    /// 🐾 greeting / general.
    Paw,
    /// 👀 status overview.
    Eyes,
    /// ⚠️ something needs the user.
    Warn,
    /// ✅ done / finished.
    Done,
    /// 🤔 not sure / asking which.
    Think,
    /// 💤 all quiet.
    Quiet,
    /// 🛠️ a proposal (or a command) to start or prompt an agent.
    Tool,
}

impl Icon {
    pub(crate) fn emoji(self) -> &'static str {
        match self {
            Icon::Paw => "\u{1F43E}",
            Icon::Eyes => "\u{1F440}",
            Icon::Warn => "\u{26A0}\u{FE0F}",
            Icon::Done => "\u{2705}",
            Icon::Think => "\u{1F914}",
            Icon::Quiet => "\u{1F4A4}",
            Icon::Tool => "\u{1F6E0}\u{FE0F}",
        }
    }
}

/// Pet sounds that open a playful reply ("Woof!", "Beep boop.").
const INTERJECTIONS: &[&str] = &[
    "woof", "arf", "ruff", "bark", "yip", "meow", "mrrp", "mrow", "purr", "tweet", "chirp", "squawk", "beep", "boop", "bzzt",
    "bleep", "grr", "awoo",
];

/// True for emoji and pictographs (and their joiners / variation selectors).
fn is_emoji(c: char) -> bool {
    matches!(c as u32,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B00..=0x2BFF | 0xFE0F | 0x200D | 0x20E3)
}

/// `*wags tail*`-style stage directions out (1-60 chars, on one line, starting with a letter).
pub(crate) fn strip_actions(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('*') {
        let after = &rest[start + 1..];
        let end = after.find(['*', '\n']);
        match end {
            Some(e) if after[e..].starts_with('*') && (1..=60).contains(&after[..e].chars().count())
                && after.chars().next().is_some_and(char::is_alphabetic) =>
            {
                out.push_str(&rest[..start]);
                rest = &after[e + 1..];
            }
            _ => {
                out.push_str(&rest[..=start]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Leading pet sounds out of each line ("Woof! Arf. Big question" -> "Big question").
fn strip_interjections(line: &str) -> String {
    let is_sound = |w: &str| INTERJECTIONS.contains(&w.to_lowercase().as_str());
    let mut t = line.trim_start();
    loop {
        let end = t.find(|c: char| !c.is_alphabetic()).unwrap_or(t.len());
        if end == 0 || !is_sound(&t[..end]) {
            break;
        }
        let after = &t[end..];
        let rest = after.trim_start_matches(['!', '.', ',', '~']);
        let next: String = after.trim_start().chars().take_while(|c| c.is_alphabetic()).collect();
        // A sound on its own ("Woof!", "Beep boop."), not a word of a sentence ("Bark at the moon").
        if rest.len() < after.len() || after.is_empty() || is_sound(&next) {
            t = rest.trim_start();
        } else {
            break;
        }
    }
    t.to_string()
}

/// Spaces collapsed inside lines, lines trimmed at the end, the text trimmed.
fn tidy(s: &str) -> String {
    let lines: Vec<String> = s
        .lines()
        .map(|l| {
            let mut out = String::with_capacity(l.len());
            let mut space = false;
            for c in l.chars() {
                if c == ' ' {
                    if !space {
                        out.push(' ');
                    }
                    space = true;
                } else {
                    space = false;
                    out.push(c);
                }
            }
            out.trim_end().replace(" .", ".").replace(" ,", ",").replace(" !", "!")
        })
        .collect();
    lines.join("\n").trim().to_string()
}

/// The text of a plain reply: stage directions, pet sounds and emoji out, tidied (no icon).
pub(crate) fn plain(s: &str) -> String {
    let no_actions = strip_actions(s);
    let no_emoji: String = no_actions.chars().filter(|c| !is_emoji(*c)).collect();
    let lines: Vec<String> = no_emoji.lines().map(strip_interjections).collect();
    tidy(&lines.join("\n"))
}

/// A plain reply: [`plain`] text after the one emoji of `icon`.
pub(crate) fn plain_text(icon: Icon, s: &str) -> String {
    let text = plain(s);
    if text.is_empty() {
        icon.emoji().to_string()
    } else {
        format!("{} {text}", icon.emoji())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_has_no_actions_sounds_or_extra_emoji() {
        let raw = "Woof! Big question! *wags tail* For me, life is belly rubs 🐶 and *snores softly* naps.";
        assert_eq!(plain(raw), "Big question! For me, life is belly rubs and naps.");
        assert_eq!(plain("*perks ears* Approved! Claude Code can go ahead: cargo test. *tail going wild*"), "Approved! Claude Code can go ahead: cargo test.");
        assert_eq!(plain("Beep boop. All quiet."), "All quiet.");
        assert_eq!(plain("Mrrp.\n• Codex · api: working"), "• Codex · api: working");
        // Real text with asterisks or sound-like words stays.
        assert_eq!(plain("Run `ls *.rs` and 2*3*4"), "Run `ls *.rs` and 2*3*4");
        assert_eq!(plain("Bark at the moon: done"), "Bark at the moon: done");
        assert_eq!(plain("  Hi!   Biscuit here.  2 sessions open ."), "Hi! Biscuit here. 2 sessions open.");
        // One emoji, first, from the set; none inside.
        let t = plain_text(Icon::Eyes, "✅ 2 sessions open 🚀, 1 busy");
        assert_eq!(t, "\u{1F440} 2 sessions open, 1 busy");
        assert_eq!(t.chars().filter(|c| is_emoji(*c)).count(), 1);
        assert_eq!(plain_text(Icon::Warn, "x"), "\u{26A0}\u{FE0F} x");
    }
}
