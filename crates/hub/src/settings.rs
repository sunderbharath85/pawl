//! Settings in the UI (PLAN step 5b): the `HubSettings` view of config.toml, patch validation, and
//! in-place config.toml edits that keep comments and unknown keys (`toml_edit`).
//!
//! Everything here is pure (no hub state); `core/settings.rs` applies the result at runtime.
//!
//! Field names in errors and in the changed-key lists are the camelCase wire paths of
//! `HubSettingsPatch` ("hub.port", "peer.startRoots[2]", "customAgents[1].command"). Errors read
//! `invalid:<field>: <reason>`. Secret values (the Anthropic key, the relay access key) never
//! appear in a `HubSettings`, an error, a log line or an audit row: only whether they are set.

use std::collections::{BTreeMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use krypto_discovery::CustomAgent;
use krypto_protocol::{
    CustomAgentSetting, HubSettings, HubSettingsBot, HubSettingsHub, HubSettingsPatch, HubSettingsPeer, HubSettingsRelay,
    HubSettingsRepos, HubSettingsWatchdog,
};
use toml_edit::{DocumentMut, Item, TableLike, Value as TVal};

use crate::config::{parse_peer_listen, HubConfig, CONFIG_TEMPLATE};
use crate::repos;

// ---------------------------------------------------------------- keys

pub const HUB_PORT: &str = "hub.port";
pub const HUB_OFFLINE: &str = "hub.offline";
pub const HUB_HOOK_STALE: &str = "hub.hookSessionStaleHours";
pub const HUB_OWNED_IDLE: &str = "hub.ownedSessionIdleHours";
pub const HUB_RETENTION: &str = "hub.eventRetentionDays";
pub const HUB_DAILY_REPORT: &str = "hub.dailyReportTime";
pub const BOT_MODEL: &str = "bot.model";
pub const BOT_KEY: &str = "bot.anthropicApiKey";
pub const BOT_OLLAMA_URL: &str = "bot.ollamaUrl";
pub const BOT_OLLAMA_MODEL: &str = "bot.ollamaModel";
pub const WD_NUDGE: &str = "watchdog.waitingNudgeAfterMs";
pub const WD_REPEAT: &str = "watchdog.repeatEveryMs";
pub const WD_TICK: &str = "watchdog.tickSecs";
pub const PEER_LISTEN: &str = "peer.listen";
pub const PEER_ACCEPT: &str = "peer.acceptIntents";
pub const PEER_ROOTS: &str = "peer.startRoots";
pub const REPOS_PATHS: &str = "repos.paths";
pub const RELAY_URL: &str = "relay.url";
pub const RELAY_KEY: &str = "relay.accessKey";
pub const CUSTOM_AGENTS: &str = "customAgents";

/// Every settable key, in the order changes are reported.
pub const ALL_KEYS: &[&str] = &[
    HUB_PORT,
    HUB_OFFLINE,
    HUB_HOOK_STALE,
    HUB_OWNED_IDLE,
    HUB_RETENTION,
    HUB_DAILY_REPORT,
    BOT_MODEL,
    BOT_KEY,
    BOT_OLLAMA_URL,
    BOT_OLLAMA_MODEL,
    WD_NUDGE,
    WD_REPEAT,
    WD_TICK,
    PEER_LISTEN,
    PEER_ACCEPT,
    PEER_ROOTS,
    REPOS_PATHS,
    RELAY_URL,
    RELAY_KEY,
    CUSTOM_AGENTS,
];

/// Keys holding secrets: reported as changed, never with a value.
pub const SECRET_KEYS: &[&str] = &[BOT_KEY, RELAY_KEY];

/// Keys the running hub cannot apply: they take effect at the next start.
pub const RESTART_KEYS: &[&str] = &[HUB_PORT];

// ---------------------------------------------------------------- limits

pub const MAX_CUSTOM_AGENTS: usize = 16;
pub const MAX_START_ROOTS: usize = 64;
pub const MAX_REPO_PATHS: usize = 200;
pub const MAX_PATH_CHARS: usize = 1024;
pub const MAX_URL_CHARS: usize = 512;
pub const MAX_SECRET_CHARS: usize = 512;
pub const MAX_MODEL_CHARS: usize = 100;
pub const MAX_AGENT_NAME_CHARS: usize = 80;
pub const MAX_COMMAND_ARGS: usize = 64;
pub const MAX_COMMAND_ARG_CHARS: usize = 4096;
/// Hours settings (0 = off) go up to a year; event retention (0 = keep forever) up to 10 years.
pub const MAX_HOURS: u32 = 8760;
pub const MAX_RETENTION_DAYS: u32 = 3650;
/// Watchdog: nudge after / repeat every 10 s .. 24 h; tick every 5 s .. 1 h.
pub const WATCHDOG_MS: std::ops::RangeInclusive<i64> = 10_000..=86_400_000;
pub const WATCHDOG_TICK_SECS: std::ops::RangeInclusive<u64> = 5..=3600;

// ---------------------------------------------------------------- errors

/// A patch value that failed validation. Displays as the ack error `invalid:<field>: <reason>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid:{field}: {reason}")]
pub struct InvalidSetting {
    /// camelCase wire path, with list indices (`peer.startRoots[2]`).
    pub field: String,
    pub reason: String,
}

fn invalid(field: impl Into<String>, reason: impl Into<String>) -> InvalidSetting {
    InvalidSetting { field: field.into(), reason: reason.into() }
}

// ---------------------------------------------------------------- the view

/// Where the running hub's values came from besides config.toml (`overriddenBy`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    /// key -> flag or env var, e.g. "hub.port" -> "--port".
    pub keys: BTreeMap<String, String>,
    /// `ANTHROPIC_API_KEY` is set: it is the bot's key while config.toml has none.
    pub env_anthropic_key: bool,
}

fn nonblank(o: &Option<String>) -> Option<String> {
    o.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// "HH:MM", or "" when the daily report is off.
fn daily_report_text(cfg: &HubConfig) -> String {
    cfg.daily_report_minutes().map(|m| format!("{:02}:{:02}", m / 60, m % 60)).unwrap_or_default()
}

/// The `HubSettings` clients see for `cfg` (the running hub's effective config).
pub fn to_settings(cfg: &HubConfig, overrides: &Overrides) -> HubSettings {
    let key_in_config = nonblank(&cfg.brain.anthropic_api_key).is_some();
    let mut overridden_by = overrides.keys.clone();
    if !key_in_config && overrides.env_anthropic_key {
        overridden_by.insert(BOT_KEY.to_string(), crate::config::ENV_ANTHROPIC_KEY.to_string());
    }
    HubSettings {
        hub: HubSettingsHub {
            port: cfg.port,
            offline: cfg.offline,
            hook_session_stale_hours: cfg.hook_session_stale_hours,
            owned_session_idle_hours: cfg.owned_session_idle_hours,
            event_retention_days: cfg.event_retention_days,
            daily_report_time: daily_report_text(cfg),
        },
        bot: HubSettingsBot {
            model: cfg.brain.effective_model(),
            anthropic_api_key_set: key_in_config || overrides.env_anthropic_key,
            ollama_url: nonblank(&cfg.brain.ollama_url),
            ollama_model: nonblank(&cfg.brain.ollama_model),
        },
        watchdog: HubSettingsWatchdog {
            waiting_nudge_after_ms: cfg.watchdog.waiting_nudge_after_ms,
            repeat_every_ms: cfg.watchdog.repeat_every_ms,
            tick_secs: cfg.watchdog.tick_secs,
        },
        peer: HubSettingsPeer {
            listen: cfg.peer.listen_addr().ok().flatten().map(|a| a.to_string()),
            accept_intents: cfg.peer.accept_intents,
            start_roots: cfg.peer.start_roots.clone(),
        },
        repos: HubSettingsRepos { paths: cfg.repos.paths.clone() },
        relay: HubSettingsRelay { url: nonblank(&cfg.relay.url), access_key_set: nonblank(&cfg.relay.access_key).is_some() },
        custom_agents: cfg
            .custom_agents
            .iter()
            .map(|a| CustomAgentSetting { id: a.id.clone(), name: a.name.clone(), command: a.command.clone() })
            .collect(),
        overridden_by,
    }
}

/// The keys whose value differs between `a` and `b` (secrets compared by value, never shown).
pub fn changed_keys(a: &HubConfig, b: &HubConfig) -> Vec<&'static str> {
    let none = Overrides::default();
    let (x, y) = (to_settings(a, &none), to_settings(b, &none));
    let differs = |key: &str| -> bool {
        match key {
            HUB_PORT => x.hub.port != y.hub.port,
            HUB_OFFLINE => x.hub.offline != y.hub.offline,
            HUB_HOOK_STALE => x.hub.hook_session_stale_hours != y.hub.hook_session_stale_hours,
            HUB_OWNED_IDLE => x.hub.owned_session_idle_hours != y.hub.owned_session_idle_hours,
            HUB_RETENTION => x.hub.event_retention_days != y.hub.event_retention_days,
            HUB_DAILY_REPORT => x.hub.daily_report_time != y.hub.daily_report_time,
            BOT_MODEL => x.bot.model != y.bot.model,
            BOT_KEY => nonblank(&a.brain.anthropic_api_key) != nonblank(&b.brain.anthropic_api_key),
            BOT_OLLAMA_URL => x.bot.ollama_url != y.bot.ollama_url,
            BOT_OLLAMA_MODEL => x.bot.ollama_model != y.bot.ollama_model,
            WD_NUDGE => x.watchdog.waiting_nudge_after_ms != y.watchdog.waiting_nudge_after_ms,
            WD_REPEAT => x.watchdog.repeat_every_ms != y.watchdog.repeat_every_ms,
            WD_TICK => x.watchdog.tick_secs != y.watchdog.tick_secs,
            PEER_LISTEN => x.peer.listen != y.peer.listen,
            PEER_ACCEPT => x.peer.accept_intents != y.peer.accept_intents,
            PEER_ROOTS => x.peer.start_roots != y.peer.start_roots,
            REPOS_PATHS => x.repos.paths != y.repos.paths,
            RELAY_URL => x.relay.url != y.relay.url,
            RELAY_KEY => nonblank(&a.relay.access_key) != nonblank(&b.relay.access_key),
            CUSTOM_AGENTS => x.custom_agents != y.custom_agents,
            _ => false,
        }
    };
    ALL_KEYS.iter().copied().filter(|k| differs(k)).collect()
}

// ---------------------------------------------------------------- validation

fn check_hours(field: &str, v: u32, max: u32) -> Result<u32, InvalidSetting> {
    if v > max {
        return Err(invalid(field, format!("must be 0-{max} (0 = off)")));
    }
    Ok(v)
}

/// "HH:MM" (24 h, two digits each) -> stored as is; "" -> "off".
fn check_daily_report(v: &str) -> Result<String, InvalidSetting> {
    let t = v.trim();
    if t.is_empty() {
        return Ok("off".into());
    }
    let bad = || invalid(HUB_DAILY_REPORT, "must be \"HH:MM\" (00:00-23:59) or \"\" for off");
    let (h, m) = t.split_once(':').ok_or_else(bad)?;
    let digits = |s: &str| s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(h) || !digits(m) {
        return Err(bad());
    }
    let (h, m): (u32, u32) = (h.parse().map_err(|_| bad())?, m.parse().map_err(|_| bad())?);
    if h > 23 || m > 59 {
        return Err(bad());
    }
    Ok(format!("{h:02}:{m:02}"))
}

/// A model id: 1-100 visible characters, no spaces.
fn check_model(field: &str, v: &str) -> Result<String, InvalidSetting> {
    let v = v.trim();
    if v.is_empty() {
        return Err(invalid(field, "must not be empty"));
    }
    if v.chars().count() > MAX_MODEL_CHARS {
        return Err(invalid(field, format!("is longer than {MAX_MODEL_CHARS} characters")));
    }
    if v.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid(field, "must not contain spaces or control characters"));
    }
    Ok(v.to_string())
}

/// A secret value: 1-512 visible characters, no spaces. The reason never quotes the value.
fn check_secret(field: &str, v: &str) -> Result<String, InvalidSetting> {
    let v = v.trim();
    if v.is_empty() {
        return Err(invalid(field, "is empty; send null to clear it"));
    }
    if v.chars().count() > MAX_SECRET_CHARS {
        return Err(invalid(field, format!("is longer than {MAX_SECRET_CHARS} characters")));
    }
    if v.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid(field, "must not contain spaces or control characters"));
    }
    Ok(v.to_string())
}

/// `scheme://host[:port][/path]` with one of `schemes`; no user info, spaces or control
/// characters. Returned trimmed, scheme lowercased, without a trailing '/'.
pub fn check_url(field: &str, v: &str, schemes: &[&str]) -> Result<String, InvalidSetting> {
    let v = v.trim();
    let want = schemes.iter().map(|s| format!("{s}://")).collect::<Vec<_>>().join(" or ");
    if v.chars().count() > MAX_URL_CHARS {
        return Err(invalid(field, format!("is longer than {MAX_URL_CHARS} characters")));
    }
    if v.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid(field, "must not contain spaces or control characters"));
    }
    let Some((scheme, rest)) = v.split_once("://") else { return Err(invalid(field, format!("must start with {want}"))) };
    let scheme = scheme.to_ascii_lowercase();
    if !schemes.contains(&scheme.as_str()) {
        return Err(invalid(field, format!("must start with {want}")));
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err(invalid(field, "must not contain a user name or password"));
    }
    let host = match authority.rsplit_once(':') {
        Some((h, p)) if !p.contains(']') => {
            if p.parse::<u16>().ok().filter(|p| *p > 0).is_none() {
                return Err(invalid(field, "has an invalid port"));
            }
            h
        }
        _ => authority,
    };
    let bare = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    if bare.is_empty() || !bare.chars().all(|c| c.is_ascii_alphanumeric() || ".-_:".contains(c)) {
        return Err(invalid(field, "has no valid host"));
    }
    let out = format!("{scheme}://{rest}");
    Ok(out.trim_end_matches('/').to_string())
}

/// The relay URL: `wss://` / `https://`, or `ws://` / `http://` to this machine only (local relay
/// tests, `wrangler dev`), as the link layer accepts it.
pub(crate) fn check_relay_url(v: &str) -> Result<String, InvalidSetting> {
    let url = check_url(RELAY_URL, v, &["wss", "https", "ws", "http"])?;
    let (scheme, rest) = url.split_once("://").unwrap_or_default();
    if matches!(scheme, "ws" | "http") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let host = match authority.rsplit_once(':') {
            Some((h, p)) if !p.contains(']') => h,
            _ => authority,
        };
        let host = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
        let loopback = host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
        if !loopback {
            return Err(invalid(RELAY_URL, "must use wss:// (ws:// only to this machine)"));
        }
    }
    if rest.contains(['?', '#', '%']) {
        return Err(invalid(RELAY_URL, "must not have a query, fragment or %-escapes"));
    }
    Ok(url)
}

/// The relay access key: 1-256 characters of A-Z, a-z, 0-9, `-` and `_` (base64url). Never quoted.
pub(crate) fn check_relay_key(v: &str) -> Result<String, InvalidSetting> {
    let v = v.trim();
    if v.is_empty() {
        return Err(invalid(RELAY_KEY, "is empty; send null to clear it"));
    }
    if v.len() > 256 || !v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(invalid(RELAY_KEY, "must be 1-256 characters of A-Z, a-z, 0-9, - and _"));
    }
    Ok(v.to_string())
}

/// An absolute path to an existing local folder (no network share or device path, not a whole
/// drive or the filesystem root). Returned canonicalized (links resolved, no `..`; on Windows
/// without the `\\?\` prefix).
fn check_dir(field: &str, v: &str) -> Result<String, InvalidSetting> {
    let v = v.trim();
    if v.is_empty() {
        return Err(invalid(field, "must not be empty"));
    }
    if v.chars().count() > MAX_PATH_CHARS || v.chars().any(char::is_control) {
        return Err(invalid(field, format!("must be at most {MAX_PATH_CHARS} characters without control characters")));
    }
    let p = Path::new(v);
    if !p.is_absolute() {
        return Err(invalid(field, "must be an absolute path"));
    }
    if repos::plain_path(p).is_none() {
        return Err(invalid(field, "network shares and device paths are not allowed"));
    }
    if !p.is_dir() {
        return Err(invalid(field, "is not an existing folder"));
    }
    let canon = repos::canonical_dir(p).ok_or_else(|| invalid(field, "resolves to a network share or device path"))?;
    if canon.parent().is_none() {
        return Err(invalid(field, "is a whole drive or the filesystem root; pick a folder inside it"));
    }
    let out = canon.to_string_lossy().into_owned();
    if out.chars().count() > MAX_PATH_CHARS {
        return Err(invalid(field, format!("is longer than {MAX_PATH_CHARS} characters once resolved")));
    }
    Ok(out)
}

fn check_dirs(field: &str, list: &[String], max: usize) -> Result<Vec<String>, InvalidSetting> {
    if list.len() > max {
        return Err(invalid(field, format!("has more than {max} entries")));
    }
    let mut out: Vec<String> = Vec::with_capacity(list.len());
    let mut seen: Vec<Vec<String>> = Vec::with_capacity(list.len());
    for (i, v) in list.iter().enumerate() {
        let f = format!("{field}[{i}]");
        let dir = check_dir(&f, v)?;
        let key = repos::path_key(Path::new(&dir));
        if let Some(j) = seen.iter().position(|k| *k == key) {
            return Err(invalid(f, format!("is the same folder as {field}[{j}]")));
        }
        seen.push(key);
        out.push(dir);
    }
    Ok(out)
}

fn valid_agent_id(id: &str) -> bool {
    (1..=40).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Custom agents: <= 16; id `[a-z0-9-]{1,40}`, unique and not a built-in or registry id (`reserved`);
/// name 1-80 characters; command a non-empty argv. `env` of an agent that keeps its id is kept.
fn check_custom_agents(list: &[CustomAgentSetting], cur: &[CustomAgent], reserved: &[String]) -> Result<Vec<CustomAgent>, InvalidSetting> {
    if list.len() > MAX_CUSTOM_AGENTS {
        return Err(invalid(CUSTOM_AGENTS, format!("has more than {MAX_CUSTOM_AGENTS} agents")));
    }
    let mut ids = HashSet::new();
    let mut out = Vec::with_capacity(list.len());
    for (i, a) in list.iter().enumerate() {
        let f = |k: &str| format!("{CUSTOM_AGENTS}[{i}].{k}");
        let id = a.id.trim();
        if !valid_agent_id(id) {
            return Err(invalid(f("id"), "must be 1-40 characters of a-z, 0-9 and -"));
        }
        if reserved.iter().any(|r| r == id) {
            return Err(invalid(f("id"), format!("{id:?} is a built-in agent id")));
        }
        if !ids.insert(id.to_string()) {
            return Err(invalid(f("id"), format!("{id:?} is used twice")));
        }
        let name = a.name.trim();
        if name.is_empty() || name.chars().count() > MAX_AGENT_NAME_CHARS || name.chars().any(char::is_control) {
            return Err(invalid(f("name"), format!("must be 1-{MAX_AGENT_NAME_CHARS} characters without control characters")));
        }
        if a.command.is_empty() || a.command[0].trim().is_empty() {
            return Err(invalid(f("command"), "must name a program to run"));
        }
        if a.command.len() > MAX_COMMAND_ARGS {
            return Err(invalid(f("command"), format!("has more than {MAX_COMMAND_ARGS} arguments")));
        }
        if a.command.iter().any(|arg| arg.chars().count() > MAX_COMMAND_ARG_CHARS || arg.contains('\0')) {
            return Err(invalid(f("command"), format!("arguments must be at most {MAX_COMMAND_ARG_CHARS} characters without NUL")));
        }
        let mut command = a.command.clone();
        command[0] = command[0].trim().to_string();
        let env = cur.iter().find(|c| c.id == id).and_then(|c| c.env.clone());
        out.push(CustomAgent { id: id.to_string(), name: name.to_string(), command, env });
    }
    Ok(out)
}

/// The peer listener address: an IP literal with an optional port (default 47801), port
/// 1024-65535, never the hub's own API port on loopback / every interface.
fn check_listen(v: &str, hub_port: u16) -> Result<Option<String>, InvalidSetting> {
    let v = v.trim();
    if v.is_empty() {
        return Ok(None);
    }
    let addr = parse_peer_listen(v)
        .map_err(|_| invalid(PEER_LISTEN, "must be an IP address with an optional port, e.g. 192.168.1.20:47801"))?;
    if addr.port() < 1024 {
        return Err(invalid(PEER_LISTEN, "port must be 1024-65535"));
    }
    if addr.port() == hub_port && (addr.ip().is_loopback() || addr.ip().is_unspecified()) {
        return Err(invalid(PEER_LISTEN, "uses the hub's own port"));
    }
    Ok(Some(addr.to_string()))
}

/// Validate `patch` against the running config `cur`. Ok = the next config and the keys that
/// changed (none: nothing to write). `reserved` = agent ids custom agents may not take.
/// Any error leaves everything as it was: nothing has been written or applied yet.
pub fn apply_patch(cur: &HubConfig, patch: &HubSettingsPatch, reserved: &[String]) -> Result<(HubConfig, Vec<&'static str>), InvalidSetting> {
    let mut next = cur.clone();
    if let Some(h) = &patch.hub {
        if let Some(p) = h.port {
            if p < 1024 {
                return Err(invalid(HUB_PORT, "must be 1024-65535"));
            }
            next.port = p;
        }
        if let Some(o) = h.offline {
            next.offline = o;
        }
        if let Some(v) = h.hook_session_stale_hours {
            next.hook_session_stale_hours = check_hours(HUB_HOOK_STALE, v, MAX_HOURS)?;
        }
        if let Some(v) = h.owned_session_idle_hours {
            next.owned_session_idle_hours = check_hours(HUB_OWNED_IDLE, v, MAX_HOURS)?;
        }
        if let Some(v) = h.event_retention_days {
            next.event_retention_days = check_hours(HUB_RETENTION, v, MAX_RETENTION_DAYS)?;
        }
        if let Some(t) = &h.daily_report_time {
            next.daily_report_time = check_daily_report(t)?;
        }
    }
    if let Some(b) = &patch.bot {
        if let Some(m) = &b.model {
            next.brain.model = check_model(BOT_MODEL, m)?;
        }
        match &b.anthropic_api_key {
            Some(Some(k)) => next.brain.anthropic_api_key = Some(check_secret(BOT_KEY, k)?),
            Some(None) => next.brain.anthropic_api_key = None,
            None => {}
        }
        match &b.ollama_url {
            Some(Some(u)) if !u.trim().is_empty() => next.brain.ollama_url = Some(check_url(BOT_OLLAMA_URL, u, &["http", "https"])?),
            Some(_) => next.brain.ollama_url = None,
            None => {}
        }
        match &b.ollama_model {
            Some(Some(m)) if !m.trim().is_empty() => next.brain.ollama_model = Some(check_model(BOT_OLLAMA_MODEL, m)?),
            Some(_) => next.brain.ollama_model = None,
            None => {}
        }
    }
    if let Some(w) = &patch.watchdog {
        let ms = |field: &str, v: i64| {
            if WATCHDOG_MS.contains(&v) {
                Ok(v)
            } else {
                Err(invalid(field, format!("must be {}-{} ms", WATCHDOG_MS.start(), WATCHDOG_MS.end())))
            }
        };
        if let Some(v) = w.waiting_nudge_after_ms {
            next.watchdog.waiting_nudge_after_ms = ms(WD_NUDGE, v)?;
        }
        if let Some(v) = w.repeat_every_ms {
            next.watchdog.repeat_every_ms = ms(WD_REPEAT, v)?;
        }
        if let Some(v) = w.tick_secs {
            if !WATCHDOG_TICK_SECS.contains(&v) {
                return Err(invalid(WD_TICK, format!("must be {}-{} s", WATCHDOG_TICK_SECS.start(), WATCHDOG_TICK_SECS.end())));
            }
            next.watchdog.tick_secs = v;
        }
    }
    if let Some(p) = &patch.peer {
        match &p.listen {
            Some(Some(l)) => next.peer.listen = check_listen(l, next.port)?,
            Some(None) => next.peer.listen = None,
            None => {}
        }
        if let Some(a) = p.accept_intents {
            next.peer.accept_intents = a;
        }
        if let Some(roots) = &p.start_roots {
            next.peer.start_roots = check_dirs(PEER_ROOTS, roots, MAX_START_ROOTS)?;
        }
    }
    if let Some(r) = &patch.repos {
        if let Some(paths) = &r.paths {
            next.repos.paths = check_dirs(REPOS_PATHS, paths, MAX_REPO_PATHS)?;
        }
    }
    if let Some(r) = &patch.relay {
        match &r.url {
            Some(Some(u)) if !u.trim().is_empty() => next.relay.url = Some(check_relay_url(u)?),
            Some(_) => next.relay.url = None,
            None => {}
        }
        match &r.access_key {
            Some(Some(k)) => next.relay.access_key = Some(check_relay_key(k)?),
            Some(None) => next.relay.access_key = None,
            None => {}
        }
    }
    if let Some(list) = &patch.custom_agents {
        next.custom_agents = check_custom_agents(list, &cur.custom_agents, reserved)?;
    }
    // The API port and the peer listener can never share a port on loopback / every interface
    // (check_listen covers a listener change; this covers a port change).
    if patch.hub.as_ref().is_some_and(|h| h.port.is_some()) {
        if let Ok(Some(addr)) = next.peer.listen_addr() {
            if addr.port() == next.port && (addr.ip().is_loopback() || addr.ip().is_unspecified()) {
                return Err(invalid(HUB_PORT, "is the peer listener's port"));
            }
        }
    }
    let changed = changed_keys(cur, &next);
    Ok((next, changed))
}

// ---------------------------------------------------------------- config.toml edits

/// Replace (or add) `key` = `v` in `t`, keeping the comment lines above the key and the
/// comment after the value.
fn set_value(t: &mut dyn TableLike, key: &str, v: impl Into<TVal>) {
    let mut v: TVal = v.into();
    match t.get_mut(key) {
        Some(item) => {
            if let Some(old) = item.as_value() {
                *v.decor_mut() = old.decor().clone();
            }
            *item = Item::Value(v);
        }
        None => {
            t.insert(key, Item::Value(v));
        }
    }
}

/// Remove `key` from `t`; comment lines above it move to the key that follows (if any).
fn remove_key(t: &mut dyn TableLike, key: &str) {
    let prefix = t.key(key).and_then(|k| k.leaf_decor().prefix()).and_then(|p| p.as_str()).map(str::to_string);
    let next_key = {
        let mut keys = t.iter().map(|(k, _)| k.to_string());
        keys.by_ref().find(|k| k == key);
        keys.next()
    };
    t.remove(key);
    let Some(prefix) = prefix.filter(|p| p.contains('#')) else { return };
    if let Some(next) = next_key {
        if let Some(mut k) = t.key_mut(&next) {
            let rest = k.leaf_decor().prefix().and_then(|p| p.as_str()).unwrap_or("").to_string();
            k.leaf_decor_mut().set_prefix(format!("{prefix}{rest}"));
        }
    }
}

fn set_opt(t: &mut dyn TableLike, key: &str, v: Option<&str>) {
    match v {
        Some(v) => set_value(t, key, v),
        None => remove_key(t, key),
    }
}

fn string_array(items: &[String]) -> toml_edit::Array {
    let mut a = toml_edit::Array::new();
    for s in items {
        a.push(s.as_str());
    }
    a
}

/// `[name]` (created at the end when missing; an inline table is fine too).
fn section<'a>(doc: &'a mut DocumentMut, name: &str) -> Result<&'a mut dyn TableLike> {
    let item = doc.as_table_mut().entry(name).or_insert_with(|| Item::Table(toml_edit::Table::new()));
    item.as_table_like_mut().with_context(|| format!("`{name}` in config.toml is not a table"))
}

/// `[[custom_agents]]` = `agents`, in order. A table whose id stays keeps its comments and other
/// keys (e.g. `env`).
fn write_custom_agents(doc: &mut DocumentMut, agents: &[CustomAgent]) -> Result<()> {
    let root = doc.as_table_mut();
    let mut old: Vec<toml_edit::Table> = match root.get("custom_agents") {
        Some(Item::ArrayOfTables(a)) => a.iter().cloned().collect(),
        Some(Item::Value(TVal::Array(a))) => a.iter().filter_map(|v| v.as_inline_table()).map(|t| t.clone().into_table()).collect(),
        Some(_) => bail!("`custom_agents` in config.toml is not a list of tables"),
        None => Vec::new(),
    };
    if agents.is_empty() {
        remove_key(root, "custom_agents");
        return Ok(());
    }
    let position = old.iter().filter_map(|t| t.position()).min();
    let mut aot = toml_edit::ArrayOfTables::new();
    for a in agents {
        let found = old.iter().position(|t| t.get("id").and_then(Item::as_str) == Some(a.id.as_str()));
        let mut t = match found {
            Some(i) => old.remove(i),
            None => toml_edit::Table::new(),
        };
        t.set_position(position);
        set_value(&mut t, "id", a.id.as_str());
        set_value(&mut t, "name", a.name.as_str());
        set_value(&mut t, "command", string_array(&a.command));
        aot.push(t);
    }
    match root.get_mut("custom_agents") {
        Some(item) => *item = Item::ArrayOfTables(aot),
        None => {
            root.insert("custom_agents", Item::ArrayOfTables(aot));
        }
    }
    Ok(())
}

/// config.toml text with the `changed` keys of `next` written in place. `old` = the current file
/// (None = missing: start from the commented template). Everything else (comments, unknown keys,
/// formatting) stays as it was. The result is parsed back and must give `next` for every key.
pub fn edit_config(old: Option<&str>, next: &HubConfig, changed: &[&str]) -> Result<String> {
    let text = old.unwrap_or(CONFIG_TEMPLATE);
    let mut doc: DocumentMut = text.parse().context("config.toml is not valid TOML; fix it by hand first")?;
    if doc.as_table().is_empty() {
        // A comment-only file (the template): its comments stay on top, new keys go below them.
        let comments = doc.trailing().as_str().unwrap_or_default().to_string();
        if !comments.trim().is_empty() {
            doc.set_trailing("");
            let sep = if comments.ends_with('\n') { "\n" } else { "\n\n" };
            doc.as_table_mut().decor_mut().set_prefix(format!("{comments}{sep}"));
        }
    }
    for key in changed {
        match *key {
            HUB_PORT => set_value(doc.as_table_mut(), "port", i64::from(next.port)),
            HUB_OFFLINE => set_value(doc.as_table_mut(), "offline", next.offline),
            HUB_HOOK_STALE => set_value(doc.as_table_mut(), "hook_session_stale_hours", i64::from(next.hook_session_stale_hours)),
            HUB_OWNED_IDLE => set_value(doc.as_table_mut(), "owned_session_idle_hours", i64::from(next.owned_session_idle_hours)),
            HUB_RETENTION => set_value(doc.as_table_mut(), "event_retention_days", i64::from(next.event_retention_days)),
            HUB_DAILY_REPORT => set_value(doc.as_table_mut(), "daily_report_time", next.daily_report_time.as_str()),
            BOT_MODEL => set_value(section(&mut doc, "brain")?, "model", next.brain.model.as_str()),
            BOT_KEY => set_opt(section(&mut doc, "brain")?, "anthropic_api_key", next.brain.anthropic_api_key.as_deref()),
            BOT_OLLAMA_URL => set_opt(section(&mut doc, "brain")?, "ollama_url", next.brain.ollama_url.as_deref()),
            BOT_OLLAMA_MODEL => set_opt(section(&mut doc, "brain")?, "ollama_model", next.brain.ollama_model.as_deref()),
            WD_NUDGE => set_value(section(&mut doc, "watchdog")?, "waiting_nudge_after_ms", next.watchdog.waiting_nudge_after_ms),
            WD_REPEAT => set_value(section(&mut doc, "watchdog")?, "repeat_every_ms", next.watchdog.repeat_every_ms),
            WD_TICK => {
                let secs = i64::try_from(next.watchdog.tick_secs).context("watchdog.tickSecs out of range")?;
                set_value(section(&mut doc, "watchdog")?, "tick_secs", secs)
            }
            PEER_LISTEN => set_opt(section(&mut doc, "peer")?, "listen", next.peer.listen.as_deref()),
            PEER_ACCEPT => set_value(section(&mut doc, "peer")?, "accept_intents", next.peer.accept_intents),
            PEER_ROOTS => set_value(section(&mut doc, "peer")?, "start_roots", string_array(&next.peer.start_roots)),
            REPOS_PATHS => set_value(section(&mut doc, "repos")?, "paths", string_array(&next.repos.paths)),
            RELAY_URL => set_opt(section(&mut doc, "relay")?, "url", next.relay.url.as_deref()),
            RELAY_KEY => set_opt(section(&mut doc, "relay")?, "access_key", next.relay.access_key.as_deref()),
            CUSTOM_AGENTS => write_custom_agents(&mut doc, &next.custom_agents)?,
            other => bail!("unknown setting {other:?}"),
        }
    }
    let out = doc.to_string();
    // Defence in depth: what we wrote must read back as `next`.
    let back = HubConfig::parse(&out).context("the edited config.toml does not parse")?;
    let mismatch: Vec<&str> = changed_keys(&back, next).into_iter().filter(|k| changed.contains(k)).collect();
    if !mismatch.is_empty() {
        bail!("the edited config.toml does not read back as written ({})", mismatch.join(", "));
    }
    Ok(out)
}

/// Write `text` to `path` atomically: a temp file in the same folder (user-only permissions on
/// Unix: config.toml can hold secrets), flushed, then renamed over the old file.
pub fn write_atomic(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "config.toml".into());
    let tmp = dir.join(format!(".{name}.{}.{}.tmp", std::process::id(), uuid::Uuid::new_v4().simple()));
    let written = (|| -> Result<()> {
        let mut f = open_private(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path).with_context(|| format!("replace {}", path.display()))
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(unix)]
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
}

/// Windows: the data dir lives under %APPDATA%, whose inherited ACL already limits access.
#[cfg(not(unix))]
fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().write(true).create_new(true).open(path)
}

/// `krypto settings` output: the settings as JSON with secrets shown as set / not set only.
pub fn masked_lines(s: &HubSettings, restart_required: &[String]) -> Vec<String> {
    let opt = |o: &Option<String>| o.clone().unwrap_or_else(|| "(off)".into());
    let set = |b: bool| if b { "set (hidden)" } else { "not set" }.to_string();
    let list = |v: &[String]| if v.is_empty() { "(none)".to_string() } else { v.join(", ") };
    let over = |k: &str| s.overridden_by.get(k).map(|by| format!("   [from {by}]")).unwrap_or_default();
    let restart = |k: &str| if restart_required.iter().any(|r| r == k) { "   [restart the hub to apply]" } else { "" };
    let mut v = vec![
        "[hub]".to_string(),
        format!("port                      {}{}{}", s.hub.port, over(HUB_PORT), restart(HUB_PORT)),
        format!("offline                   {}", s.hub.offline),
        format!("hook session stale hours  {}", s.hub.hook_session_stale_hours),
        format!("owned session idle hours  {}", s.hub.owned_session_idle_hours),
        format!("event retention days      {}", s.hub.event_retention_days),
        format!("daily report time         {}", if s.hub.daily_report_time.is_empty() { "(off)" } else { &s.hub.daily_report_time }),
        "[bot]".to_string(),
        format!("model                     {}", s.bot.model),
        format!("anthropic api key         {}{}", set(s.bot.anthropic_api_key_set), over(BOT_KEY)),
        format!("ollama url                {}", opt(&s.bot.ollama_url)),
        format!("ollama model              {}", opt(&s.bot.ollama_model)),
        "[watchdog]".to_string(),
        format!("waiting nudge after ms    {}", s.watchdog.waiting_nudge_after_ms),
        format!("repeat every ms           {}", s.watchdog.repeat_every_ms),
        format!("tick secs                 {}", s.watchdog.tick_secs),
        "[peer]".to_string(),
        format!("listen                    {}{}", opt(&s.peer.listen), over(PEER_LISTEN)),
        format!("accept intents            {}", s.peer.accept_intents),
        format!("start roots               {}", list(&s.peer.start_roots)),
        "[repos]".to_string(),
        format!("paths                     {}", list(&s.repos.paths)),
        "[relay]".to_string(),
        format!("url                       {}", opt(&s.relay.url)),
        format!("access key                {}", set(s.relay.access_key_set)),
        "[custom agents]".to_string(),
    ];
    if s.custom_agents.is_empty() {
        v.push("(none)".into());
    }
    for a in &s.custom_agents {
        v.push(format!("{:<25} {} ({})", a.id, a.command.join(" "), a.name));
    }
    v
}

#[cfg(test)]
mod tests;
