//! Approvals by voice (PLAN.md "Next release (v0.2)" item 3).
//!
//! A chat message sent with `via: "voice"` is a speech transcript, and the microphone also hears a TV,
//! a call or a replay. So approvals follow stricter rules than typed chat:
//!
//! - Before approving, the pet reads the request back (the tool, the command or file, the machine),
//!   unless the transcript already says the request's code.
//! - Reads, searches, thinking and edits on this machine: "approve" (or "yes, approve") after the
//!   read-back is enough. A bare "yes" never is.
//! - Everything else (execute, delete, fetch, move, mode switches, other and unknown kinds) and every
//!   request of another machine: the transcript must say the two-digit code the request's card shows
//!   ("approve 4 7", "approve forty seven"). It proves the speaker can see the screen. A right code on
//!   another machine's request is the confirmation on this device (the owner's rules still apply).
//! - "Always allow" is never available by voice.
//! - Deny is the safe direction: it acts at once, with no read-back and no code (a code only picks the
//!   request).
//! - Requests of hook sessions (answered in the terminal) are not answered by voice.
//! - Codes are local secrets: nothing sent to a model ever contains one, and a transcript that says
//!   a pending request's code is never sent to a model.

use serde_json::Value;

use super::*;
use crate::util::one_line;

/// True for a voice code as the owning hub assigns it: exactly two ASCII digits ("47").
pub fn is_voice_code(code: &str) -> bool {
    code.len() == 2 && code.bytes().all(|b| b.is_ascii_digit())
}

/// "47" -> "4 7", the way the card and the pet show it.
pub fn spell_code(code: &str) -> String {
    code.chars().map(String::from).collect::<Vec<_>>().join(" ")
}

/// True if the transcript says `code` (see [`spoken_numbers`]). False for anything but a voice code.
pub fn says_code(text: &str, code: &str) -> bool {
    is_voice_code(code) && spoken_numbers(text).iter().any(|n| n == code)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Num {
    /// Digits as written ("47", "4").
    Numeral(String),
    /// zero .. nine.
    Unit(u8),
    /// ten .. nineteen (10 + n).
    Teen(u8),
    /// twenty .. ninety (n * 10).
    Tens(u8),
}

/// A number word or numeral, and whether it is weak: a speech-to-text homophone ("for", "to", "won",
/// "ate", "oh") or "one" ("approve that one"), which count only next to a spelled number word.
fn num_word(w: &str) -> Option<(Num, bool)> {
    if !w.is_empty() && w.bytes().all(|b| b.is_ascii_digit()) {
        return Some((Num::Numeral(w.to_string()), false));
    }
    let unit = |n: u8, weak: bool| Some((Num::Unit(n), weak));
    match w {
        "zero" | "nought" => unit(0, false),
        "oh" => unit(0, true),
        "one" | "won" => unit(1, true),
        "two" => unit(2, false),
        "to" | "too" => unit(2, true),
        "three" => unit(3, false),
        "four" => unit(4, false),
        "for" | "fore" => unit(4, true),
        "five" => unit(5, false),
        "six" => unit(6, false),
        "seven" => unit(7, false),
        "eight" => unit(8, false),
        "ate" => unit(8, true),
        "nine" => unit(9, false),
        "ten" => Some((Num::Teen(0), false)),
        "eleven" => Some((Num::Teen(1), false)),
        "twelve" => Some((Num::Teen(2), false)),
        "thirteen" => Some((Num::Teen(3), false)),
        "fourteen" => Some((Num::Teen(4), false)),
        "fifteen" => Some((Num::Teen(5), false)),
        "sixteen" => Some((Num::Teen(6), false)),
        "seventeen" => Some((Num::Teen(7), false)),
        "eighteen" => Some((Num::Teen(8), false)),
        "nineteen" => Some((Num::Teen(9), false)),
        "twenty" => Some((Num::Tens(2), false)),
        "thirty" => Some((Num::Tens(3), false)),
        "forty" | "fourty" => Some((Num::Tens(4), false)),
        "fifty" => Some((Num::Tens(5), false)),
        "sixty" => Some((Num::Tens(6), false)),
        "seventy" => Some((Num::Tens(7), false)),
        "eighty" => Some((Num::Tens(8), false)),
        "ninety" => Some((Num::Tens(9), false)),
        _ => None,
    }
}

fn spelled(n: &Num) -> bool {
    !matches!(n, Num::Numeral(_))
}

/// The digits of one run of number words: "forty seven" and "four seven" are "47", "4 7" is "47".
fn digits(run: &[Num]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i < run.len() {
        match &run[i] {
            Num::Numeral(s) => out.push_str(s),
            Num::Unit(u) => out.push(char::from(b'0' + u)),
            Num::Teen(t) => {
                out.push('1');
                out.push(char::from(b'0' + t));
            }
            Num::Tens(t) => {
                out.push(char::from(b'0' + t));
                match run.get(i + 1) {
                    Some(Num::Unit(u)) if *u > 0 => {
                        out.push(char::from(b'0' + u));
                        i += 1;
                    }
                    _ => out.push('0'),
                }
            }
        }
        i += 1;
    }
    out
}

/// Every number the transcript says, in order, as digit strings: "approve 4 7", "approve 47.",
/// "approve 4-7", "approve forty seven", "approve forty-seven", "approve four seven" and "approve for
/// seven" all say "47". Digits said one by one join up; homophones and "one" count only next to a
/// spelled number word ("approve this one", "approve 47 for codex" say nothing more than "47").
/// Words mixing letters and digits ("api2", "2nd") are not numbers.
pub fn spoken_numbers(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let mut run: Vec<(Num, bool)> = Vec::new();
        while let Some(n) = words.get(i).and_then(|w| num_word(w)) {
            run.push(n);
            i += 1;
        }
        if run.is_empty() {
            i += 1;
            continue;
        }
        // A weak word stays only with a spelled number word next to it.
        let kept: Vec<Num> = run
            .iter()
            .enumerate()
            .filter(|(j, (_, weak))| {
                !weak
                    || (*j > 0 && spelled(&run[j - 1].0))
                    || run.get(j + 1).is_some_and(|(n, w)| spelled(n) && !w)
            })
            .map(|(_, (n, _))| n.clone())
            .collect();
        let strong = run.iter().any(|(_, weak)| !weak);
        if strong && !kept.is_empty() {
            out.push(digits(&kept));
        }
    }
    out
}

/// True if the transcript says the code of a pending request (such a transcript never goes to a model).
pub(super) fn says_pending_code(text: &str, ctx: &BrainContext) -> bool {
    let numbers = spoken_numbers(text);
    !numbers.is_empty() && ctx.pending.iter().filter_map(|r| r.voice_code.as_deref()).any(|c| numbers.iter().any(|n| n == c))
}

/// Requests of hook sessions are answered in the terminal, never by voice.
fn on_hook_session(ctx: &BrainContext, r: &PermissionRequest) -> bool {
    ctx.sessions.iter().any(|s| s.id == r.session_id && matches!(s.source, SessionSource::VscodeHook | SessionSource::CliHook))
}

/// True unless "approve" after the read-back is enough: a read, search, think or edit on this machine.
pub fn needs_code(local: bool, kind: Option<ToolKind>) -> bool {
    !local || !matches!(kind, Some(ToolKind::Read | ToolKind::Search | ToolKind::Think | ToolKind::Edit))
}

/// What the request does, for the read-back: its kind and the command, the file or its title.
fn what(r: &PermissionRequest) -> String {
    let tc = &r.tool_call;
    // Control characters out, one line; long enough that the end of a command is not hidden.
    let clean = |s: &str| {
        let s: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
        truncate(&one_line(&s).replace('`', "'"), 300)
    };
    let command = || tc.raw_input.as_ref().and_then(|v| v.get("command")).and_then(Value::as_str).map(clean).filter(|c| !c.is_empty());
    let file = || tc.locations.as_ref().and_then(|l| l.first()).map(|l| clean(&l.path)).filter(|p| !p.is_empty());
    let title = || tc.title.as_deref().map(clean).filter(|t| !t.is_empty());
    let kind = summary::kind_label(tc.kind);
    let detail = match tc.kind {
        Some(ToolKind::Execute) => command().or_else(title).map(|c| format!("`{c}`")),
        Some(ToolKind::Read | ToolKind::Edit | ToolKind::Delete | ToolKind::Move) => file().or_else(title),
        _ => title().or_else(file),
    };
    match detail {
        Some(d) => format!("{kind}: {d}"),
        None => kind.to_string(),
    }
}

/// "Codex on Desk wants to run a command: `bun test`." (always names the machine).
fn read_back(ctx: &BrainContext, r: &PermissionRequest) -> String {
    let everyone = remote::all_agents(ctx);
    format!("{} on {} wants to {}.", agent_name(&r.agent_id, &everyone), remote::machine_name(ctx, &r.machine_id), what(r))
}

fn with_read_back(text: String, r: &PermissionRequest) -> BrainReply {
    let mut reply = say(text);
    reply.read_back = Some(r.request_id.clone());
    reply
}

const LOCKED: &str = "Too many wrong codes, so approving by voice is paused for a few minutes. Approve it on the screen.";

/// approve / deny from a speech transcript (see the module docs). `machine`: a machine the user named.
#[allow(clippy::too_many_arguments)]
pub(super) fn resolve(
    ctx: &BrainContext,
    voice: &Voice,
    seed: u64,
    text: &str,
    approve: bool,
    always: bool,
    agent: Option<AgentRef>,
    pick: Option<Pick>,
    explicit: bool,
    machine: Option<&str>,
) -> BrainReply {
    if approve && always {
        return say(format!(
            "{} I can't “always allow” by voice. Say “approve” to allow it once, or choose “Always allow” on the screen.",
            voice.alert(seed)
        ));
    }
    let mut all: Vec<&PermissionRequest> = ctx.pending.iter().filter(|r| !on_hook_session(ctx, r)).collect();
    all.sort_by(|a, b| a.created_at.cmp(&b.created_at).then_with(|| a.request_id.cmp(&b.request_id)));
    let agent_fits = |r: &PermissionRequest| match &agent {
        Some(AgentRef::Known(id)) => &r.agent_id == id,
        Some(AgentRef::Unknown(_)) => false,
        None => true,
    };
    let machine_fits = |r: &PermissionRequest| machine.is_none_or(|m| remote::same_machine(ctx, &r.machine_id, m));

    // A spoken number is a code: it names the request (any machine's) and stands for the read-back.
    let numbers = spoken_numbers(text);
    if !numbers.is_empty() {
        if approve && ctx.voice_locked {
            return say(format!("{} {LOCKED}", voice.alert(seed)));
        }
        let hit = all.iter().copied().find(|r| r.voice_code.as_deref().is_some_and(|c| is_voice_code(c) && numbers.iter().any(|n| n == c)));
        return match hit {
            Some(r) if agent_fits(r) && machine_fits(r) => decide(ctx, voice, seed, r, approve, always),
            _ => {
                let mut reply = say(format!("{} That code doesn't match, so nothing happened. The code is on the request's card.", voice.alert(seed)));
                reply.code_miss = true;
                reply
            }
        };
    }

    // No code: which request, as in typed chat (a named machine's; else this machine's; else other
    // machines'), then the agent named, an ordinal, the one just read back, or the only one.
    let pending: Vec<&PermissionRequest> = match machine {
        Some(_) => all.into_iter().filter(|r| machine_fits(r)).collect(),
        None => {
            let (here, other): (Vec<&PermissionRequest>, Vec<&PermissionRequest>) = all.into_iter().partition(|r| is_local(ctx, &r.machine_id));
            if here.is_empty() {
                other
            } else {
                here
            }
        }
    };
    if pending.is_empty() {
        return say(format!("{} Nothing is waiting for a yes or no right now.", voice.sound(seed)));
    }
    let which = || {
        format!(
            "{} More than one request is waiting. Which one?{}\nSay “{}” and the code on its card, or name the agent.",
            voice.alert(seed),
            pending_list(&pending, ctx),
            if approve { "approve" } else { "deny" }
        )
    };
    let chosen: &PermissionRequest = if let Some(pick) = pick {
        let idx = match pick {
            Pick::Nth(n) => n - 1,
            Pick::Last => pending.len() - 1,
        };
        match pending.get(idx) {
            Some(r) => r,
            None => return say(format!("{} There's no request #{}. Waiting:{}", voice.thinking(seed), idx + 1, pending_list(&pending, ctx))),
        }
    } else {
        let fits: Vec<&PermissionRequest> = pending.iter().copied().filter(|r| agent_fits(r)).collect();
        let heard = fits.iter().copied().find(|r| ctx.read_back.as_deref() == Some(r.request_id.as_str()));
        match (fits.len(), heard) {
            (1, _) => fits[0],
            (0, _) => {
                let who = match &agent {
                    Some(AgentRef::Known(id)) => agent_name(id, &ctx.agents),
                    Some(AgentRef::Unknown(n)) => n.clone(),
                    None => "Nobody".into(),
                };
                return say(format!("{} {who} isn't waiting on anything. Waiting:{}", voice.thinking(seed), pending_list(&pending, ctx)));
            }
            (_, Some(r)) => r,
            _ => return say(which()),
        }
    };

    let heard_back = ctx.read_back.as_deref() == Some(chosen.request_id.as_str());
    let back = read_back(ctx, chosen);
    let code = chosen.voice_code.as_deref().filter(|c| is_voice_code(c));
    if !approve {
        return decide(ctx, voice, seed, chosen, false, always);
    }
    if needs_code(is_local(ctx, &chosen.machine_id), chosen.tool_call.kind) {
        let tail = match code {
            _ if ctx.voice_locked => LOCKED.to_string(),
            Some(c) => format!("Say: “approve {}” to allow it once, or “deny”.", spell_code(c)),
            None => "It has no voice code, so approve it on the screen (or say “deny”).".to_string(),
        };
        return with_read_back(format!("{} {back}\n{tail}", voice.alert(seed)), chosen);
    }
    if !heard_back {
        return with_read_back(format!("{} {back}\nSay “approve” to allow it once, or “deny”.", voice.alert(seed)), chosen);
    }
    if !explicit {
        return with_read_back(format!("{} A plain “yes” isn't enough by voice: say “approve” (or “deny”).", voice.alert(seed)), chosen);
    }
    decide(ctx, voice, seed, chosen, true, false)
}

/// The decision itself: approve = allow once (never "always" by voice); deny = reject (once, or always
/// when asked), else cancel.
fn decide(ctx: &BrainContext, voice: &Voice, seed: u64, r: &PermissionRequest, approve: bool, always: bool) -> BrainReply {
    let everyone = remote::all_agents(ctx);
    let name = format!("{} on {}", agent_name(&r.agent_id, &everyone), remote::machine_name(ctx, &r.machine_id));
    let request_id = r.request_id.clone();
    if approve {
        return match find_option(r, PermissionOptionKind::AllowOnce) {
            Some(option_id) => act(
                format!("{} Approved once: {name} can {}.", voice.sound(seed), what(r)),
                BrainAction::ResolvePermission { request_id, option_id: Some(option_id) },
            ),
            None if find_option(r, PermissionOptionKind::AllowAlways).is_some() => say(format!(
                "{} {name}'s request only offers “always allow”, and that isn't available by voice. Answer it on the screen.",
                voice.sad(seed)
            )),
            None => say(format!("{} {name}'s request has no allow option, so I can't approve it.", voice.sad(seed))),
        };
    }
    let option = if always {
        find_option(r, PermissionOptionKind::RejectAlways).or_else(|| find_option(r, PermissionOptionKind::RejectOnce))
    } else {
        find_option(r, PermissionOptionKind::RejectOnce)
    };
    let text = match &option {
        Some(_) => format!("{} Denied: {name} won't {}.", voice.alert(seed), what(r)),
        None => format!("{} Denied. {name}'s request had no reject option, so I cancelled it.", voice.alert(seed)),
    };
    act(text, BrainAction::ResolvePermission { request_id, option_id: option })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_heard_however_they_are_said() {
        for t in [
            "approve 4 7",
            "Approve 47.",
            "approve 4-7",
            "approve four seven",
            "approve forty seven",
            "Approve forty-seven please",
            "approve for seven",
            "yes, approve 4, 7",
            "approve 4 7 for codex",
            "approve code number 47",
            "APPROVE FOUR SEVEN",
        ] {
            assert_eq!(spoken_numbers(t), vec!["47".to_string()], "{t}");
            assert!(says_code(t, "47"), "{t}");
        }
        assert_eq!(spoken_numbers("approve forty"), vec!["40"]);
        assert_eq!(spoken_numbers("approve four oh"), vec!["40"]);
        assert_eq!(spoken_numbers("approve twelve"), vec!["12"]);
        assert_eq!(spoken_numbers("approve ten"), vec!["10"]);
        assert_eq!(spoken_numbers("approve forty one"), vec!["41"]);
        assert_eq!(spoken_numbers("approve one nine"), vec!["19"]);
        assert_eq!(spoken_numbers("approve nine ate"), vec!["98"]);
        assert_eq!(spoken_numbers("approve 2"), vec!["2"], "a lone number is a (wrong) code, not a pick");
        assert_eq!(spoken_numbers("approve 4 7 2"), vec!["472"]);
        assert_eq!(spoken_numbers("deny 12 and 47"), vec!["12", "47"]);
        // Words that only sound like numbers, alone, say nothing.
        for t in ["approve this one", "approve it for me", "go to codex", "approve the second one", "approve api2 on the 2nd try", "approve", ""] {
            assert!(spoken_numbers(t).is_empty(), "{t}: {:?}", spoken_numbers(t));
        }
        assert!(!says_code("approve 4 7", "74"));
        assert!(!says_code("approve 4", "47"));
        assert!(!says_code("approve 47", "4"), "not a voice code");
        assert!(is_voice_code("07") && !is_voice_code("7") && !is_voice_code("470") && !is_voice_code("4a"));
        assert_eq!(spell_code("47"), "4 7");
    }

    #[test]
    fn only_local_reads_searches_thinking_and_edits_go_without_a_code() {
        for k in [ToolKind::Read, ToolKind::Search, ToolKind::Think, ToolKind::Edit] {
            assert!(!needs_code(true, Some(k)), "{k:?}");
            assert!(needs_code(false, Some(k)), "remote {k:?}");
        }
        for k in [ToolKind::Execute, ToolKind::Delete, ToolKind::Fetch, ToolKind::Move, ToolKind::SwitchMode, ToolKind::Other] {
            assert!(needs_code(true, Some(k)), "{k:?}");
        }
        assert!(needs_code(true, None), "unknown kind");
    }
}
