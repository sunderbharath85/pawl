//! Browser tasks (v0.2 item 5). With `[browser] enabled`, a web task the user gives the default pet
//! ("check if CI passed on my PR", "open the staging page") becomes a `browse` proposal: the user
//! confirms it in a client, which sends `start_browser_task { task, confirmed: true }`. The brain never
//! starts one itself (a proposal only, like every command the brain routed), and with the setting off it
//! never proposes one.
//!
//! The deterministic test ([`is_web_task`]): an explicit browser phrase ("in the browser", "in Chrome",
//! "browse …"); a URL or host name with a looking verb; or a looking verb (check, see, look, view, visit,
//! show, read) with a web noun (page, site, tab, link, dashboard, CI, PR, GitHub). "Open" and "go to" count
//! with a page, site, tab, link, dashboard or URL only, so "open a PR for the fix" stays a coding task.

use pawl_protocol::{ChatProposal, ChatProposalKind};

use super::intent::Intent;
use super::{remote, BrainContext, BrainReply};
use crate::util::truncate;

/// The agent that runs browser tasks (Claude Code with the Chrome DevTools MCP server).
pub const BROWSER_AGENT: &str = "claude-code";
/// Longest task text a proposal carries.
pub const MAX_TASK_CHARS: usize = 1_000;

const EXPLICIT: &[&str] = &["in the browser", "in my browser", "in a browser", "in chrome", "with the browser", "use the browser", "using the browser"];
const LOOK_VERBS: &[&str] = &["check", "see", "look", "view", "visit", "browse", "show", "read", "verify", "confirm"];
const OPEN_VERBS: &[&str] = &["open", "go", "navigate", "visit", "browse"];
const PAGE_NOUNS: &[&str] = &["page", "pages", "site", "website", "webpage", "tab", "url", "link", "dashboard", "browser", "chrome"];
const WEB_NOUNS: &[&str] = &["ci", "pr", "prs", "github", "gitlab", "pipeline", "actions"];
/// Last labels that are file extensions, not top-level domains ("main.rs" is a file).
const FILE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "md", "py", "json", "toml", "txt", "yml", "yaml", "lock", "sh", "ps1", "go", "java",
    "kt", "swift", "html", "htm", "css", "scss", "png", "jpg", "jpeg", "gif", "svg", "log", "c", "h", "cpp", "hpp", "cs", "rb", "php",
    "vue", "sql", "csv", "xml", "env", "ini", "cfg", "conf", "bat", "exe", "dll", "zip", "gz", "tar", "pdf", "lua", "dart", "proto",
];

/// The message's words, lowercased (URL and host tokens kept whole).
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| c.is_whitespace() || ",;!?()[]{}\"“”'‘’<>".contains(c))
        .map(|w| w.trim_matches(|c: char| c == '.' || c == ':').to_ascii_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// `https://…` / `http://…`, or a host name such as `staging.example.com` (not a file name).
fn is_url_or_host(w: &str) -> bool {
    if w.starts_with("http://") || w.starts_with("https://") {
        return true;
    }
    let host = w.split('/').next().unwrap_or_default();
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty() || !l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')) {
        return false;
    }
    let tld = labels[labels.len() - 1];
    (2..=24).contains(&tld.len()) && tld.chars().all(|c| c.is_ascii_alphabetic()) && !FILE_EXTS.contains(&tld)
}

/// Does `text` ask for something done in a web browser? See the module docs.
pub fn is_web_task(text: &str) -> bool {
    let lower = text.to_lowercase();
    if EXPLICIT.iter().any(|p| lower.contains(p)) {
        return true;
    }
    let w = words(text);
    let has = |list: &[&str]| w.iter().any(|x| list.contains(&x.as_str()));
    let url = w.iter().any(|x| is_url_or_host(x));
    let pull_request = lower.contains("pull request");
    let look = has(LOOK_VERBS);
    let open = has(OPEN_VERBS);
    if url && (look || open) {
        return true;
    }
    if open && has(PAGE_NOUNS) {
        return true;
    }
    look && (has(PAGE_NOUNS) || has(WEB_NOUNS) || pull_request)
}

/// True when this message is a browser task for the default pet: browser use on, no pet attached, and the
/// parser found no other intent (small talk) or a command with no agent named (the pet would pick one).
pub(crate) fn wanted(ctx: &BrainContext, intent: &Intent, text: &str) -> bool {
    let open_intent = match intent {
        Intent::SmallTalk => true,
        Intent::Command(c) => c.agent.is_none(),
        _ => false,
    };
    ctx.browser && ctx.pet.is_none() && open_intent && is_web_task(text)
}

/// The reply: what will happen, and the `browse` proposal to confirm.
pub(crate) fn reply(ctx: &BrainContext, text: &str) -> BrainReply {
    let task = truncate(text.trim(), MAX_TASK_CHARS);
    let proposal = ChatProposal {
        id: uuid::Uuid::new_v4().to_string(),
        kind: ChatProposalKind::Browse,
        machine_id: ctx.machine.id.clone(),
        agent_id: BROWSER_AGENT.into(),
        cwd: None,
        repo_name: None,
        session_id: None,
        summary: remote::summary(ChatProposalKind::Browse, "Claude Code", None, "", &task),
        text: task,
    };
    BrainReply {
        text: "I can do that in your Chrome with Claude Code. Chrome asks you to allow the connection, and every step \
               except opening an allowed site waits for your OK. Confirm below to start."
            .into(),
        proposals: vec![proposal],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_tasks_are_told_from_coding_tasks() {
        for yes in [
            "check if CI passed on my PR",
            "open the staging page",
            "Did the build pass? check the GitHub actions page",
            "go to https://staging.example.com/login and see if it loads",
            "look at staging.example.com",
            "see if the pull request has reviews",
            "find the release notes in the browser",
            "open the dashboard tab",
        ] {
            assert!(is_web_task(yes), "{yes}");
        }
        for no in [
            "fix the failing test in repo api",
            "open a PR for the fix",
            "run the tests",
            "read main.rs and explain it",
            "what's Claude doing?",
            "look at the logs",
            "refactor config.toml loading",
            "check the types in src/lib.rs",
        ] {
            assert!(!is_web_task(no), "{no}");
        }
        assert!(is_url_or_host("github.com") && is_url_or_host("ci.example.co.uk/x") && !is_url_or_host("e.g") && !is_url_or_host("main.rs"));
    }
}
