//! Session titles from user prompts (owned and hook sessions).
//!
//! Agent harnesses put text of their own into user turns: `<task-notification>` and
//! `<system-reminder>` blocks, `<hc from=…>` messages from another terminal, pasted-content wrappers,
//! `[Image #N]` placeholders. None of that is a title. [`from_prompt`] strips it; a prompt that is only
//! injected text gives no title, and the session keeps looking at later prompts. Until a real one
//! arrives the title is [`fallback`]: "<agent name> in <repo folder>".

/// Longest title kept (chars; cut on a char boundary with an ellipsis).
pub const MAX_TITLE_CHARS: usize = 80;
/// The title of a prompt that was only pasted text.
pub const PASTED: &str = "Pasted text";

/// Blocks injected into user turns, by tag name (matched case-insensitively). Their content is never a title.
const INJECTED_TAGS: &[&str] = &[
    "task-notification",
    "system-reminder",
    "hc",
    "pasted_content",
    "local-command-stdout",
    "local-command-stderr",
    "command-name",
    "command-message",
    "command-args",
    "user-prompt-submit-hook",
];
/// Tags whose content was pasted by the user: "Pasted text" when nothing else is left.
const PASTED_TAGS: &[&str] = &["pasted_content"];

/// What a prompt gives as a title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Candidate {
    /// Real text the user typed (cleaned).
    Real(String),
    /// Only pasted content: [`PASTED`].
    Pasted,
    /// Only injected text (or nothing).
    Nothing,
}

/// The title a prompt (or a raw stored title) gives: injected blocks and placeholders removed, control
/// characters and whitespace runs collapsed to single spaces, at most [`MAX_TITLE_CHARS`].
pub fn from_prompt(raw: &str) -> Candidate {
    let (text, pasted) = strip_injected(raw);
    let flat: String = text.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if !flat.is_empty() {
        Candidate::Real(cap(&flat))
    } else if pasted {
        Candidate::Pasted
    } else {
        Candidate::Nothing
    }
}

/// "<agent name> in <repo folder>" (just the agent name while the folder is unknown).
pub fn fallback(agent_name: &str, cwd: &str) -> String {
    let folder = cwd.trim().trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or("").trim();
    let agent = agent_name.trim();
    let title = if folder.is_empty() { agent.to_string() } else { format!("{agent} in {folder}") };
    match from_prompt(&title) {
        Candidate::Real(t) => t,
        _ => "Session".to_string(),
    }
}

/// True when `title` is not a real title yet: none, a fallback, "Pasted text", or text that cleaning
/// would still change (an injected block stored before this cleanup).
pub fn is_placeholder(title: Option<&str>, agent_name: &str, cwd: &str) -> bool {
    match title {
        None => true,
        Some(t) => {
            t == PASTED
                || t == fallback(agent_name, cwd)
                || t == fallback(agent_name, "")
                || !matches!(from_prompt(t), Candidate::Real(ref c) if c == t)
        }
    }
}

/// The title after a new candidate arrives. `overwrite`: a real title replaces a real one (hook
/// sessions follow the latest prompt; owned sessions follow the agent's title). Pasted text only fills a
/// placeholder; nothing keeps the current title (or the fallback while there is none).
pub fn next(current: Option<&str>, candidate: Candidate, overwrite: bool, agent_name: &str, cwd: &str) -> String {
    let placeholder = is_placeholder(current, agent_name, cwd);
    match candidate {
        Candidate::Real(t) if overwrite || placeholder => t,
        Candidate::Pasted if placeholder && current != Some(PASTED) => PASTED.to_string(),
        _ if placeholder && current != Some(PASTED) => fallback(agent_name, cwd),
        _ => current.unwrap_or_default().to_string(),
    }
}

/// A stored title worth cleaning at hub start: starts like an injected block or image placeholder.
pub fn needs_retitle(title: &str) -> bool {
    let t = title.trim_start();
    t.starts_with('<') || t.starts_with("[Image")
}

fn cap(s: &str) -> String {
    if s.chars().count() <= MAX_TITLE_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(MAX_TITLE_CHARS - 1).collect();
    while out.ends_with(char::is_whitespace) {
        out.pop();
    }
    out.push('…');
    out
}

/// `s` without injected blocks and placeholders; true if pasted content was among them.
fn strip_injected(s: &str) -> (String, bool) {
    let mut out = String::with_capacity(s.len());
    let mut pasted = false;
    let mut i = 0;
    while i < s.len() {
        let rest = &s[i..];
        if rest.starts_with('<') {
            if let Some((name, open_end)) = open_tag(rest) {
                pasted |= PASTED_TAGS.contains(&name);
                i += match close_tag(&rest[open_end..], name) {
                    Some(close_end) => open_end + close_end,
                    // Never closed (e.g. cut off by a length limit): everything after it is the block.
                    None => rest.len(),
                };
                out.push(' ');
                continue;
            }
        }
        if rest.starts_with('[') {
            if let Some((len, was_paste)) = placeholder(rest) {
                pasted |= was_paste;
                i += len;
                out.push(' ');
                continue;
            }
        }
        let c = rest.chars().next().unwrap_or(' ');
        out.push(c);
        i += c.len_utf8();
    }
    (out, pasted)
}

/// An opening tag of an injected block at the start of `s` (`<hc from="t13">`, `<\pasted_content id=…>`):
/// its name and the byte length up to and including `>` (or the whole rest when `>` is missing).
fn open_tag(s: &str) -> Option<(&'static str, usize)> {
    let after = s.strip_prefix('<')?;
    let after = after.strip_prefix('\\').unwrap_or(after);
    let lower = after.get(..after.len().min(40)).unwrap_or(after).to_ascii_lowercase();
    let name = INJECTED_TAGS.iter().copied().find(|n| {
        lower.starts_with(n) && lower[n.len()..].chars().next().is_none_or(|c| c == '>' || c == '/' || c.is_whitespace())
    })?;
    Some((name, s.find('>').map_or(s.len(), |i| i + 1)))
}

/// The end (byte offset just past `>`) of the first closing tag of `name` in `s`: `</name…>`, with an
/// optional `\` before or after the `/` (escaped forms).
fn close_tag(s: &str, name: &str) -> Option<usize> {
    let lower = s.to_ascii_lowercase();
    let mut from = 0;
    while let Some(at) = lower[from..].find('<').map(|k| from + k) {
        let mut j = at + 1;
        let b = lower.as_bytes();
        if b.get(j) == Some(&b'\\') {
            j += 1;
        }
        if b.get(j) == Some(&b'/') {
            j += 1;
            if b.get(j) == Some(&b'\\') {
                j += 1;
            }
            if lower[j..].starts_with(name) {
                let end = lower[j..].find('>').map_or(lower.len(), |k| j + k + 1);
                return Some(end);
            }
        }
        from = at + 1;
    }
    None
}

/// `[Image #3]` / `[Pasted text #1 +20 lines]` at the start of `s`: its byte length and whether it was a paste.
fn placeholder(s: &str) -> Option<(usize, bool)> {
    let end = s.find(']')? + 1;
    let inner = s[1..end - 1].trim().to_ascii_lowercase();
    if inner.chars().count() > 60 {
        return None;
    }
    if let Some(n) = inner.strip_prefix("image") {
        let n = n.trim_start().trim_start_matches(['#', ':']).trim();
        return (n.is_empty() || n.chars().all(|c| c.is_ascii_digit())).then_some((end, false));
    }
    if inner.starts_with("pasted text") {
        return Some((end, true));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn real(s: &str) -> Candidate {
        Candidate::Real(s.into())
    }

    #[test]
    fn injected_blocks_are_never_titles() {
        // Each kind the user saw in the sidebar, alone: no title.
        for raw in [
            "[Image #40]",
            "<task-notification> <task-id>bt7xilh3z</task-id> <status>completed</status> </task-notification>",
            "<system-reminder>\nCalled the Read tool\n</system-reminder>",
            "<hc from=\"t13\">t13: accepted, thanks. Idle.</hc>",
            "   \n\t ",
            "",
        ] {
            assert_eq!(from_prompt(raw), Candidate::Nothing, "{raw:?}");
        }
        // With real text around them, only the text is left.
        assert_eq!(from_prompt("[Image #40] fix   the\nlayout"), real("fix the layout"));
        assert_eq!(from_prompt("<system-reminder>be terse</system-reminder>\nRename foo to bar"), real("Rename foo to bar"));
        assert_eq!(from_prompt("<hc from=\"t13\">ping</hc> and also run lint"), real("and also run lint"));
        assert_eq!(from_prompt("<TASK-NOTIFICATION>x</Task-Notification>deploy it"), real("deploy it"), "tag names ignore case");
        // Cut off by a length limit before the closing tag: the rest is the block.
        assert_eq!(from_prompt("<task-notification> <task-id>bt7…"), Candidate::Nothing);
        assert_eq!(from_prompt("Look at this <system-reminder>never closed"), real("Look at this"));
        // Other tags and brackets are ordinary text.
        assert_eq!(from_prompt("<div> centering is hard"), real("<div> centering is hard"));
        assert_eq!(from_prompt("[WIP] fix the build"), real("[WIP] fix the build"));
        assert_eq!(from_prompt("handle <hcx> tags"), real("handle <hcx> tags"));
    }

    #[test]
    fn pasted_content_is_pasted_text_only_when_nothing_else_is_left() {
        let paste = "<pasted_content id=\"66d5\">\n<hc from=\"t13\">do the task</hc>\n</pasted_content id=\"66d5\">";
        assert_eq!(from_prompt(paste), Candidate::Pasted);
        // The escaped form some transports show.
        assert_eq!(from_prompt("<\\pasted_content id=x>secret stuff<\\/pasted_content>"), Candidate::Pasted);
        assert_eq!(from_prompt(&format!("{paste}\nsummarize this")), real("summarize this"));
        assert_eq!(from_prompt("[Pasted text #1 +20 lines]"), Candidate::Pasted);
        assert_eq!(from_prompt("[Pasted text #1 +20 lines] what is wrong here?"), real("what is wrong here?"));
        assert_eq!(from_prompt("[Image #1] [Pasted text #2 +3 lines]"), Candidate::Pasted);
    }

    #[test]
    fn titles_are_bounded_on_char_boundaries_without_control_characters() {
        let long = "ü".repeat(500);
        let Candidate::Real(t) = from_prompt(&long) else { panic!() };
        assert_eq!(t.chars().count(), MAX_TITLE_CHARS);
        assert!(t.ends_with('…'));
        assert_eq!(from_prompt("a\u{7}b\u{1b}[2Jc"), real("a b [2Jc"));
    }

    #[test]
    fn fallback_and_next() {
        assert_eq!(fallback("Claude Code", "E:\\work\\tiny-whisper\\"), "Claude Code in tiny-whisper");
        assert_eq!(fallback("Codex", "/home/me/api"), "Codex in api");
        assert_eq!(fallback("Codex", ""), "Codex");
        let (agent, cwd) = ("Claude Code", "/w/ai-pet");
        let fb = "Claude Code in ai-pet";
        // Only injected text so far: the fallback.
        assert_eq!(next(None, Candidate::Nothing, true, agent, cwd), fb);
        // A later real prompt replaces the fallback (and a stored injected title).
        assert_eq!(next(Some(fb), real("fix the build"), false, agent, cwd), "fix the build");
        assert_eq!(next(Some("Codex"), real("fix the build"), false, "Codex", "/w/x"), "fix the build", "fallback from before the cwd was known");
        assert_eq!(next(Some("[Image #4]"), Candidate::Nothing, false, agent, cwd), fb);
        assert_eq!(next(Some("<task-notification> <task-id>bt7…"), real("ship it"), false, agent, cwd), "ship it");
        // A real title stays unless the caller follows new titles; injected-only prompts never replace it.
        assert_eq!(next(Some("fix the build"), real("now lint"), false, agent, cwd), "fix the build");
        assert_eq!(next(Some("fix the build"), real("now lint"), true, agent, cwd), "now lint");
        assert_eq!(next(Some("fix the build"), Candidate::Nothing, true, agent, cwd), "fix the build");
        assert_eq!(next(Some("fix the build"), Candidate::Pasted, true, agent, cwd), "fix the build");
        // Pasted text fills a placeholder and stays one: a later real prompt replaces it.
        assert_eq!(next(Some(fb), Candidate::Pasted, false, agent, cwd), PASTED);
        assert_eq!(next(Some(PASTED), Candidate::Nothing, false, agent, cwd), PASTED);
        assert_eq!(next(Some(PASTED), real("explain it"), false, agent, cwd), "explain it");
        // The folder became known: the fallback follows it.
        assert_eq!(next(Some("Claude Code"), Candidate::Nothing, false, agent, cwd), fb);
    }

    #[test]
    fn retitle_candidates() {
        assert!(needs_retitle("<task-notification> <task-id>bt7…") && needs_retitle("[Image #40]") && needs_retitle("  <hc from"));
        assert!(!needs_retitle("fix the build") && !needs_retitle("Claude Code in ai-pet"));
    }
}
