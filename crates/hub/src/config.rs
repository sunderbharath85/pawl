//! Hub configuration: data-dir layout, the bearer token, and `config.toml`.

use std::fmt;
use std::io::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use krypto_discovery::{CustomAgent, DiscoveryConfig};
use krypto_protocol::{Rule, HUB_DEFAULT_PORT, HUB_HOST, HUB_TOKEN_FILE, PEER_DEFAULT_PORT};
use serde::{Deserialize, Serialize};

/// Env var: overrides the token (hub and clients).
pub const ENV_TOKEN: &str = "KRYPTO_HUB_TOKEN";
/// Env var: hub URL for clients (`http://127.0.0.1:47800`, `ws://127.0.0.1:47800/ws`, ...).
pub const ENV_URL: &str = "KRYPTO_HUB_URL";
/// Env var: alternative data dir (tests, portable installs). Clients must then also get the token via env.
pub const ENV_DATA_DIR: &str = "KRYPTO_HUB_DATA_DIR";
/// Env var: Anthropic key for the pet brain (used when `brain.anthropic_api_key` is not set).
pub const ENV_ANTHROPIC_KEY: &str = "ANTHROPIC_API_KEY";

pub const CONFIG_FILE: &str = "config.toml";
pub const DB_FILE: &str = "hub.db";
pub const LOG_FILE: &str = "hub.log";
pub const REGISTRY_CACHE_FILE: &str = "registry-cache.json";
/// X25519 device secret for hub-to-hub links (64 hex chars). Never logged, printed or sent.
pub const DEVICE_KEY_FILE: &str = "device.key";
/// `{port, pid, startedAt}` of the running hub (written once it serves, removed on a clean exit).
pub const RUNTIME_FILE: &str = "runtime.json";

/// Files inside the hub data dir.
#[derive(Debug, Clone)]
pub struct Paths {
    pub data_dir: PathBuf,
    pub db: PathBuf,
    pub token: PathBuf,
    pub config: PathBuf,
    /// `machine.toml`: this machine's id + name (see [`crate::machine`]).
    pub machine: PathBuf,
    /// `device.key`: this hub's peer-link secret key (see `krypto_peer::DeviceKeys`).
    pub device_key: PathBuf,
    pub log: PathBuf,
    pub registry_cache: PathBuf,
    /// `runtime.json`: where the running hub listens (see [`RuntimeInfo`]).
    pub runtime: PathBuf,
}

impl Paths {
    pub fn in_dir(dir: impl Into<PathBuf>) -> Self {
        let data_dir: PathBuf = dir.into();
        Self {
            db: data_dir.join(DB_FILE),
            token: data_dir.join(HUB_TOKEN_FILE),
            config: data_dir.join(CONFIG_FILE),
            machine: data_dir.join(crate::machine::MACHINE_FILE),
            device_key: data_dir.join(DEVICE_KEY_FILE),
            log: data_dir.join(LOG_FILE),
            registry_cache: data_dir.join(REGISTRY_CACHE_FILE),
            runtime: data_dir.join(RUNTIME_FILE),
            data_dir,
        }
    }

    /// Explicit dir > `KRYPTO_HUB_DATA_DIR` > `krypto_protocol::data_dir()`.
    pub fn resolve(explicit: Option<PathBuf>) -> Result<Self> {
        let dir = match explicit {
            Some(d) => d,
            None => match std::env::var_os(ENV_DATA_DIR).filter(|v| !v.is_empty()) {
                Some(d) => PathBuf::from(d),
                None => krypto_protocol::data_dir().context("could not determine the OS data directory")?,
            },
        };
        Ok(Self::in_dir(dir))
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.data_dir)
            .with_context(|| format!("create data dir {}", self.data_dir.display()))
    }
}

// ---------------------------------------------------------------- config.toml

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct BrainSection {
    /// Prefer the `ANTHROPIC_API_KEY` env var over writing the key here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_api_key: Option<String>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ollama_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ollama_model: Option<String>,
}

impl Default for BrainSection {
    fn default() -> Self {
        Self { anthropic_api_key: None, model: "claude-haiku-5-5".into(), ollama_url: None, ollama_model: None }
    }
}

impl fmt::Debug for BrainSection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrainSection")
            .field("anthropic_api_key", &self.anthropic_api_key.as_ref().map(|_| "<redacted>"))
            .field("model", &self.model)
            .field("ollama_url", &self.ollama_url)
            .field("ollama_model", &self.ollama_model)
            .finish()
    }
}

impl BrainSection {
    /// The configured model, or the default when blank.
    pub fn effective_model(&self) -> String {
        if self.model.trim().is_empty() { BrainSection::default().model } else { self.model.trim().to_string() }
    }

    /// Config key, else `ANTHROPIC_API_KEY`.
    pub fn to_brain_config(&self) -> krypto_brain::chat::BrainConfig {
        let key = self
            .anthropic_api_key
            .clone()
            .filter(|k| !k.trim().is_empty())
            .or_else(|| std::env::var(ENV_ANTHROPIC_KEY).ok().filter(|k| !k.trim().is_empty()));
        krypto_brain::chat::BrainConfig {
            anthropic_api_key: key,
            model: self.effective_model(),
            ollama_url: self.ollama_url.clone().filter(|s| !s.trim().is_empty()),
            ollama_model: self.ollama_model.clone().filter(|s| !s.trim().is_empty()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WatchdogSection {
    /// Nudge when a session has waited on you this long.
    pub waiting_nudge_after_ms: i64,
    /// Repeat the nudge this often while it keeps waiting.
    pub repeat_every_ms: i64,
    /// Watchdog tick interval.
    pub tick_secs: u64,
}

impl Default for WatchdogSection {
    fn default() -> Self {
        Self { waiting_nudge_after_ms: 5 * 60_000, repeat_every_ms: 10 * 60_000, tick_secs: 30 }
    }
}

impl WatchdogSection {
    pub fn to_watchdog_config(&self) -> krypto_brain::watchdog::WatchdogConfig {
        krypto_brain::watchdog::WatchdogConfig {
            waiting_nudge_after_ms: self.waiting_nudge_after_ms.max(1_000),
            repeat_every_ms: self.repeat_every_ms.max(10_000),
        }
    }
}

/// `[peer]`: hub-to-hub links (pairing + presence).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PeerSection {
    /// Peer listener address "ip[:port]" (default port 47801). Unset = no listener (the default):
    /// this hub can still join other hubs' invites and dial them, but cannot invite.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen: Option<String>,
    /// Let paired hubs act on this machine's sessions (approve once, deny, prompt, start, cancel).
    /// false = every intent is answered `forbidden`; the mirror still works.
    pub accept_intents: bool,
    /// Folders (and everything under them) where paired hubs may start agents, besides the folders
    /// of this machine's own previous sessions.
    pub start_roots: Vec<String>,
}

impl Default for PeerSection {
    fn default() -> Self {
        Self { listen: None, accept_intents: true, start_roots: Vec::new() }
    }
}

impl PeerSection {
    /// The listener address, if one is configured.
    pub fn listen_addr(&self) -> Result<Option<SocketAddr>> {
        match self.listen.as_deref().map(str::trim) {
            None | Some("") => Ok(None),
            Some(s) => parse_peer_listen(s).map(Some),
        }
    }
}

/// "ip[:port]" -> socket address; the port defaults to 47801. IPv6 with a port needs brackets
/// ("[::1]:47801"). Host names are refused: the listener binds an interface, not a name.
pub fn parse_peer_listen(s: &str) -> Result<SocketAddr> {
    let s = s.trim();
    if let Ok(a) = s.parse::<SocketAddr>() {
        return Ok(a);
    }
    let bare = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')).unwrap_or(s);
    match bare.parse::<IpAddr>() {
        Ok(ip) => Ok(SocketAddr::new(ip, PEER_DEFAULT_PORT)),
        Err(_) => bail!("peer listen address must be \"ip\" or \"ip:port\" (e.g. 192.168.1.20:47801), got {s:?}"),
    }
}

/// `[repos]`: folders to list in this machine's repo index besides those its sessions used.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ReposSection {
    pub paths: Vec<String>,
}

/// `[relay]`: the relay hubs and phones connect to outbound (PLAN step 6). Stored and shown now;
/// wired into the link layer in step 6 phase B.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RelaySection {
    /// e.g. "wss://relay.example.com". Unset = no relay.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Secret the relay asks for (write-only in the UI; never logged or sent to clients).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access_key: Option<String>,
}

impl fmt::Debug for RelaySection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RelaySection")
            .field("url", &self.url)
            .field("access_key", &self.access_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HubConfig {
    pub port: u16,
    /// Skip the network registry fetch (cache / built-in list only).
    pub offline: bool,
    pub custom_agents: Vec<CustomAgent>,
    /// Seed permission rules. Once a client calls `set_rules`, the rules stored in hub.db win.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<Rule>>,
    pub brain: BrainSection,
    pub watchdog: WatchdogSection,
    /// Local time "HH:MM" for the end-of-day report notification; "" or "off" disables it.
    pub daily_report_time: String,
    /// Hook-tracked sessions without any hook for this long are marked ended.
    pub hook_session_stale_hours: u32,
    /// Owned (ACP) sessions idle this long are closed to free their agent process (0 = never).
    pub owned_session_idle_hours: u32,
    /// Events older than this are pruned at startup (0 = keep forever).
    pub event_retention_days: u32,
    pub peer: PeerSection,
    pub repos: ReposSection,
    pub relay: RelaySection,
}

impl Default for HubConfig {
    fn default() -> Self {
        Self {
            port: HUB_DEFAULT_PORT,
            offline: false,
            custom_agents: Vec::new(),
            rules: None,
            brain: BrainSection::default(),
            watchdog: WatchdogSection::default(),
            daily_report_time: "18:00".into(),
            hook_session_stale_hours: 12,
            owned_session_idle_hours: 12,
            event_retention_days: 30,
            peer: PeerSection::default(),
            repos: ReposSection::default(),
            relay: RelaySection::default(),
        }
    }
}

/// Written when config.toml is missing. The desktop app's Settings change these keys in place
/// (comments and unknown keys are kept).
pub(crate) const CONFIG_TEMPLATE: &str = r#"# Krypto hub configuration. Every key is optional; defaults shown.
# Every setting can also be changed in the desktop app (Settings).
# port = 47800                 # the hub only ever binds 127.0.0.1
# offline = false              # true = never fetch the ACP registry
# daily_report_time = "18:00"  # local time, "off" to disable
# hook_session_stale_hours = 12
# owned_session_idle_hours = 12   # close idle ACP sessions (frees the agent process), 0 = never
# event_retention_days = 30
#
# [brain]
# model = "claude-haiku-5-5"   # key comes from the ANTHROPIC_API_KEY env var
# ollama_url = "http://127.0.0.1:11434"
# ollama_model = "llama3.2"
#
# [peer]
# listen = "192.168.1.20:47801"   # hub-to-hub listener (off by default; needed to create invites)
# accept_intents = true           # false = paired machines can watch but never act here
# start_roots = ["E:/work"]       # paired machines may also start agents under these folders
#
# [repos]
# paths = ["E:/work/api"]          # also offered as repos ("in repo api"), besides folders sessions used
#
# [relay]
# url = "wss://relay.example.com"  # reach paired machines off the LAN (step 6)
#
# [watchdog]
# waiting_nudge_after_ms = 300000
# repeat_every_ms = 600000
# tick_secs = 30
#
# [[custom_agents]]
# id = "my-agent"
# name = "My Agent"
# command = ["my-agent", "--acp"]
#
# Seed rules (first match wins; no match = ask). Clients can replace them at runtime.
# [[rules]]
# id = "read-anything"
# action = "auto_allow"
# match = { toolKind = "read" }
"#;

impl HubConfig {
    /// Load `config.toml`; writes a commented template if missing.
    pub fn load_or_init(paths: &Paths) -> Result<Self> {
        match std::fs::read_to_string(&paths.config) {
            Ok(text) => Self::parse(&text).with_context(|| format!("parse {}", paths.config.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Err(e) = std::fs::write(&paths.config, CONFIG_TEMPLATE) {
                    tracing::warn!("could not write default {}: {e}", paths.config.display());
                }
                Ok(Self::default())
            }
            Err(e) => Err(e).with_context(|| format!("read {}", paths.config.display())),
        }
    }

    /// Load `config.toml` without creating anything (client commands). Missing file = defaults.
    pub fn load(paths: &Paths) -> Result<Self> {
        match std::fs::read_to_string(&paths.config) {
            Ok(text) => Self::parse(&text).with_context(|| format!("parse {}", paths.config.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| format!("read {}", paths.config.display())),
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        let cfg: HubConfig = toml::from_str(text)?;
        cfg.daily_report_minutes_checked()?; // validate early
        cfg.peer.listen_addr().context("[peer] listen")?;
        Ok(cfg)
    }

    /// Daily report time as minutes after local midnight; None = disabled.
    pub fn daily_report_minutes(&self) -> Option<u32> {
        self.daily_report_minutes_checked().ok().flatten()
    }

    fn daily_report_minutes_checked(&self) -> Result<Option<u32>> {
        let t = self.daily_report_time.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("off") || t.eq_ignore_ascii_case("none") {
            return Ok(None);
        }
        let Some((h, m)) = t.split_once(':') else { bail!("daily_report_time must be \"HH:MM\", got {t:?}") };
        let (h, m): (u32, u32) = (h.trim().parse()?, m.trim().parse()?);
        if h > 23 || m > 59 {
            bail!("daily_report_time out of range: {t:?}");
        }
        Ok(Some(h * 60 + m))
    }

    pub fn discovery_config(&self, paths: &Paths, machine_id: &str) -> DiscoveryConfig {
        DiscoveryConfig {
            machine_id: machine_id.to_string(),
            cache_dir: paths.data_dir.clone(),
            custom_agents: self.custom_agents.clone(),
            offline: self.offline,
        }
    }
}

// ---------------------------------------------------------------- runtime.json

/// The running hub: clients find its port here first, so a port saved in config.toml but not yet
/// applied (it needs a restart) never sends hook events to the wrong port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    pub port: u16,
    pub pid: u32,
    pub started_at: krypto_protocol::UnixMs,
}

impl RuntimeInfo {
    /// This process, serving on `port` now.
    pub fn now(port: u16) -> Self {
        Self { port, pid: std::process::id(), started_at: krypto_protocol::now_ms() }
    }

    /// Write it atomically (the hub, once its port is bound).
    pub fn write(&self, paths: &Paths) -> Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        crate::settings::write_atomic(&paths.runtime, &text)
    }

    /// None when missing or unreadable (no hub, or an old one that never wrote it).
    pub fn read(paths: &Paths) -> Option<Self> {
        let text = std::fs::read_to_string(&paths.runtime).ok()?;
        serde_json::from_str::<RuntimeInfo>(&text).ok().filter(|r| r.port != 0)
    }

    /// Remove runtime.json if it is still this process's (a replacement may have written its own).
    pub fn remove_if_ours(paths: &Paths) {
        if Self::read(paths).is_some_and(|r| r.pid == std::process::id()) {
            if let Err(e) = std::fs::remove_file(&paths.runtime) {
                tracing::warn!("remove {}: {e}", paths.runtime.display());
            }
        }
    }
}

// ---------------------------------------------------------------- token

fn valid_token(t: &str) -> bool {
    t.len() >= 32 && t.chars().all(|c| c.is_ascii_hexdigit())
}

/// `KRYPTO_HUB_TOKEN` if set, else the token file (None if missing/invalid). Never creates.
pub fn read_token(paths: &Paths) -> Option<String> {
    if let Ok(t) = std::env::var(ENV_TOKEN) {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    let t = std::fs::read_to_string(&paths.token).ok()?;
    let t = t.trim().to_string();
    valid_token(&t).then_some(t)
}

pub fn generate_token() -> Result<String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).map_err(|e| anyhow::anyhow!("OS random source failed: {e}"))?;
    Ok(hex::encode(buf))
}

/// Env override, else read the token file, else create it (32 random bytes, hex) with user-only perms.
pub fn load_or_create_token(paths: &Paths) -> Result<String> {
    if let Some(t) = read_token(paths) {
        return Ok(t);
    }
    paths.ensure()?;
    let token = generate_token()?;
    // A missing or corrupt file is replaced. create_new first so two racing processes agree on one token.
    if paths.token.exists() {
        std::fs::remove_file(&paths.token).with_context(|| format!("replace invalid {}", paths.token.display()))?;
    }
    match open_private_new(&paths.token) {
        Ok(mut f) => {
            f.write_all(token.as_bytes())?;
            f.sync_all().ok();
            Ok(token)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            read_token(paths).context("token file created concurrently but unreadable")
        }
        Err(e) => Err(e).with_context(|| format!("create {}", paths.token.display())),
    }
}

#[cfg(unix)]
fn open_private_new(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)
}

/// Windows: the data dir lives under %APPDATA%, whose inherited ACL already limits access to the
/// user, SYSTEM and Administrators.
#[cfg(not(unix))]
fn open_private_new(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new().write(true).create_new(true).open(path)
}

// ---------------------------------------------------------------- hub URL

/// Where a client finds the hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HubEndpoint {
    /// e.g. `http://127.0.0.1:47800`
    pub http_base: String,
    /// e.g. `ws://127.0.0.1:47800/ws`
    pub ws_url: String,
    pub port: u16,
}

impl HubEndpoint {
    pub fn local(port: u16) -> Self {
        Self {
            http_base: format!("http://{HUB_HOST}:{port}"),
            ws_url: format!("ws://{HUB_HOST}:{port}/ws"),
            port,
        }
    }

    /// Accepts `http(s)://host[:port][/...]` or `ws(s)://host[:port][/ws]`.
    pub fn parse(url: &str) -> Result<Self> {
        let url = url.trim();
        let (scheme, rest) = url.split_once("://").context("hub URL needs a scheme (http:// or ws://)")?;
        let (http_scheme, ws_scheme, default_port) = match scheme.to_ascii_lowercase().as_str() {
            "http" | "ws" => ("http", "ws", 80),
            "https" | "wss" => ("https", "wss", 443),
            other => bail!("unsupported hub URL scheme {other:?}"),
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        if authority.is_empty() {
            bail!("hub URL has no host");
        }
        let port = match authority.rsplit_once(':') {
            Some((_, p)) if !p.contains(']') => p.parse().with_context(|| format!("bad port in hub URL {url:?}"))?,
            _ => default_port,
        };
        Ok(Self {
            http_base: format!("{http_scheme}://{authority}"),
            ws_url: format!("{ws_scheme}://{authority}/ws"),
            port,
        })
    }

    /// `--port` > `KRYPTO_HUB_URL` > config port.
    pub fn resolve(port_flag: Option<u16>, config_port: u16) -> Self {
        if let Some(p) = port_flag {
            return Self::local(p);
        }
        if let Ok(url) = std::env::var(ENV_URL) {
            if !url.trim().is_empty() {
                match Self::parse(&url) {
                    Ok(ep) => return ep,
                    Err(e) => tracing::warn!("ignoring {ENV_URL}: {e:#}"),
                }
            }
        }
        Self::local(config_port)
    }
}

/// Port the daemon binds: `--port` > port of `KRYPTO_HUB_URL` > config.
pub fn serve_port(port_flag: Option<u16>, cfg: &HubConfig) -> u16 {
    HubEndpoint::resolve(port_flag, cfg.port).port
}

impl HubEndpoint {
    /// Where a client (`krypto hook`, every other client command) finds the running hub:
    /// `--port` > `KRYPTO_HUB_URL` > runtime.json (once verified, see [`runtime_candidate`] and
    /// `cli::client_endpoint`) > config.toml. This is the answer without runtime.json.
    pub fn for_client_fallback(port_flag: Option<u16>, config_port: u16) -> Self {
        Self::resolve(port_flag, config_port)
    }
}

/// The port runtime.json offers, when it would change the answer: no `--port`, no
/// `KRYPTO_HUB_URL`, and a port other than config.toml's. The caller must check that the hub there
/// is this data dir's (`/health` machineId == machine.toml) before using it: runtime.json is a
/// plain file, and anything listening on that port could otherwise collect hook events.
pub fn runtime_candidate(port_flag: Option<u16>, paths: &Paths, config_port: u16) -> Option<u16> {
    if port_flag.is_some() || std::env::var(ENV_URL).is_ok_and(|u| !u.trim().is_empty()) {
        return None;
    }
    RuntimeInfo::read(paths).map(|r| r.port).filter(|p| *p != config_port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_parse() {
        let cfg = HubConfig::parse("").unwrap();
        assert_eq!(cfg.port, 47800);
        assert_eq!(cfg.daily_report_minutes(), Some(18 * 60));
        let cfg = HubConfig::parse(
            r#"
port = 48000
daily_report_time = "off"
[brain]
model = "x"
[[custom_agents]]
id = "a"
name = "A"
command = ["a", "--acp"]
[[rules]]
id = "r"
action = "auto_allow"
match = { toolKind = "read" }
"#,
        )
        .unwrap();
        assert_eq!(cfg.port, 48000);
        assert_eq!(cfg.daily_report_minutes(), None);
        assert_eq!(cfg.custom_agents.len(), 1);
        assert_eq!(cfg.rules.as_ref().unwrap()[0].id, "r");
        assert!(HubConfig::parse("daily_report_time = \"25:00\"").is_err());
        // the shipped template must parse to the defaults
        let t = HubConfig::parse(CONFIG_TEMPLATE).unwrap();
        assert_eq!(t.port, HUB_DEFAULT_PORT);
    }

    #[test]
    fn peer_listen_addresses() {
        assert_eq!(parse_peer_listen("192.168.1.20").unwrap(), "192.168.1.20:47801".parse().unwrap());
        assert_eq!(parse_peer_listen(" 127.0.0.1:47911 ").unwrap(), "127.0.0.1:47911".parse().unwrap());
        assert_eq!(parse_peer_listen("0.0.0.0").unwrap(), "0.0.0.0:47801".parse().unwrap());
        assert_eq!(parse_peer_listen("::1").unwrap(), "[::1]:47801".parse().unwrap());
        assert_eq!(parse_peer_listen("[::1]").unwrap(), "[::1]:47801".parse().unwrap());
        assert_eq!(parse_peer_listen("[::1]:5000").unwrap(), "[::1]:5000".parse().unwrap());
        for bad in ["localhost", "my-pc:47801", "1.2.3.4:99999", "", "1.2.3.4:"] {
            assert!(parse_peer_listen(bad).is_err(), "{bad:?}");
        }
        let cfg = HubConfig::parse("[peer]\nlisten = \"10.0.0.5\"").unwrap();
        assert_eq!(cfg.peer.listen_addr().unwrap(), Some("10.0.0.5:47801".parse().unwrap()));
        assert_eq!(HubConfig::parse("").unwrap().peer.listen_addr().unwrap(), None);
        assert_eq!(HubConfig::parse("[peer]\nlisten = \" \"").unwrap().peer.listen_addr().unwrap(), None);
        let err = format!("{:#}", HubConfig::parse("[peer]\nlisten = \"my-pc\"").unwrap_err());
        assert!(err.contains("[peer] listen"), "{err}");
    }

    #[test]
    fn endpoint_parse() {
        let e = HubEndpoint::parse("ws://127.0.0.1:48001/ws").unwrap();
        assert_eq!(e.http_base, "http://127.0.0.1:48001");
        assert_eq!(e.ws_url, "ws://127.0.0.1:48001/ws");
        assert_eq!(e.port, 48001);
        let e = HubEndpoint::parse("http://localhost:47800/").unwrap();
        assert_eq!(e.ws_url, "ws://localhost:47800/ws");
        assert!(HubEndpoint::parse("ftp://x").is_err());
    }

    #[test]
    fn clients_prefer_the_running_port() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path());
        if std::env::var(ENV_URL).is_ok() {
            return; // KRYPTO_HUB_URL set in this environment: it wins by design
        }
        assert_eq!(runtime_candidate(None, &paths, 48001), None, "no runtime.json: config.toml");
        RuntimeInfo::now(47810).write(&paths).unwrap();
        let r = RuntimeInfo::read(&paths).unwrap();
        assert_eq!((r.port, r.pid), (47810, std::process::id()));
        assert_eq!(runtime_candidate(None, &paths, 48001), Some(47810), "the running port is offered (to be verified)");
        assert_eq!(runtime_candidate(None, &paths, 47810), None, "same as config.toml: nothing to verify");
        assert_eq!(runtime_candidate(Some(47999), &paths, 48001), None, "--port beats everything");
        assert_eq!(HubEndpoint::for_client_fallback(Some(47999), 48001).port, 47999);
        let text = std::fs::read_to_string(&paths.runtime).unwrap();
        assert!(text.contains("\"startedAt\""), "{text}");
        // Only the process that wrote it removes it.
        std::fs::write(&paths.runtime, r#"{"port":47811,"pid":1,"startedAt":0}"#).unwrap();
        RuntimeInfo::remove_if_ours(&paths);
        assert!(paths.runtime.exists());
        RuntimeInfo::now(47812).write(&paths).unwrap();
        RuntimeInfo::remove_if_ours(&paths);
        assert!(!paths.runtime.exists());
        std::fs::write(&paths.runtime, "garbage").unwrap();
        assert_eq!(runtime_candidate(None, &paths, 48001), None, "unreadable runtime.json is ignored");
    }

    #[test]
    fn token_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path());
        if std::env::var(ENV_TOKEN).is_ok() {
            return; // env override active in this environment; file path not exercised
        }
        let t1 = load_or_create_token(&paths).unwrap();
        assert_eq!(t1.len(), 64);
        let t2 = load_or_create_token(&paths).unwrap();
        assert_eq!(t1, t2);
        std::fs::write(&paths.token, "garbage").unwrap();
        let t3 = load_or_create_token(&paths).unwrap();
        assert_ne!(t3, t1);
        assert_eq!(read_token(&paths).unwrap(), t3);
    }
}
