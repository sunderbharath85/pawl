//! ACP registry: tolerant model of
//! <https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json>, plus the
//! 24h on-disk cache (`cache_dir/registry-cache.json`).
//!
//! Real shape (registry `version: "1.0.0"`, checked 2026-11-23):
//! ```json
//! { "version": "1.0.0", "extensions": [],
//!   "agents": [ { "id": "gemini", "name": "Gemini CLI", "version": "0.70.0", "description": "...",
//!                 "icon": "https://...svg", "repository": "...", "website": "...", "authors": ["..."],
//!                 "license": "...", "license_url": "...",
//!                 "distribution": {
//!                   "npx":    { "package": "@google/gemini-cli@0.70.0", "args": ["--acp"], "env": {} },
//!                   "uvx":    { "package": "fast-agent-acp==0.10.65", "args": ["-x"], "env": {} },
//!                   "binary": { "windows-x86_64": { "archive": "https://...zip", "cmd": "./x.exe",
//!                                                    "args": ["acp"], "env": {}, "sha256": "..." } } } } ] }
//! ```
//! Binary platform keys: `{darwin|linux|windows}-{x86_64|aarch64}` and `linux-*-musl`.
//! Every field except `id` is treated as optional and malformed entries are skipped, so a
//! schema addition upstream never breaks discovery.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const REGISTRY_URL: &str = "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";
pub const CACHE_FILE: &str = "registry-cache.json";
pub const CACHE_TTL_MS: i64 = 24 * 60 * 60 * 1000;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RegistryAgent {
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub distribution: Distribution,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Distribution {
    pub npx: Option<PackageDist>,
    pub uvx: Option<PackageDist>,
    /// platform key (e.g. `windows-x86_64`) -> target
    pub binary: BTreeMap<String, BinaryTarget>,
}

impl Distribution {
    pub fn is_empty(&self) -> bool {
        self.npx.is_none() && self.uvx.is_none() && self.binary.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PackageDist {
    pub package: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BinaryTarget {
    pub archive: Option<String>,
    pub cmd: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub sha256: Option<String>,
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    match v.get(key)? {
        Value::String(s) => {
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        }
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn string_list(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| match i {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                Value::Bool(b) => Some(b.to_string()),
                _ => None,
            })
            .collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn string_map(v: Option<&Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(Value::Object(m)) = v {
        for (k, val) in m {
            let s = match val {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => continue,
            };
            out.insert(k.clone(), s);
        }
    }
    out
}

fn parse_package(v: Option<&Value>) -> Option<PackageDist> {
    let v = v?;
    let package = match v {
        Value::String(s) => s.trim().to_string(),
        _ => str_field(v, "package")?,
    };
    if package.is_empty() {
        return None;
    }
    Some(PackageDist { package, args: string_list(v.get("args")), env: string_map(v.get("env")) })
}

fn parse_binary_target(v: &Value) -> Option<BinaryTarget> {
    let cmd = str_field(v, "cmd").or_else(|| str_field(v, "command"))?;
    Some(BinaryTarget {
        archive: str_field(v, "archive").or_else(|| str_field(v, "url")),
        cmd,
        args: string_list(v.get("args")),
        env: string_map(v.get("env")),
        sha256: str_field(v, "sha256"),
    })
}

fn parse_distribution(v: Option<&Value>) -> Distribution {
    let mut d = Distribution::default();
    let Some(Value::Object(m)) = v else { return d };
    d.npx = parse_package(m.get("npx"));
    d.uvx = parse_package(m.get("uvx"));
    if let Some(Value::Object(bins)) = m.get("binary") {
        for (platform, target) in bins {
            if let Some(t) = parse_binary_target(target) {
                d.binary.insert(platform.clone(), t);
            }
        }
    }
    d
}

fn parse_agent(v: &Value) -> Option<RegistryAgent> {
    let id = str_field(v, "id")?;
    let name = str_field(v, "name").unwrap_or_else(|| id.clone());
    Some(RegistryAgent {
        name,
        version: str_field(v, "version"),
        description: str_field(v, "description"),
        icon: str_field(v, "icon"),
        distribution: parse_distribution(v.get("distribution")),
        id,
    })
}

/// Parse a registry document. Accepts `{agents:[...]}` or a bare array; skips malformed entries
/// and duplicate ids (first wins).
pub fn parse_registry(doc: &Value) -> Vec<RegistryAgent> {
    let list = match doc {
        Value::Array(a) => a.as_slice(),
        Value::Object(m) => match m.get("agents") {
            Some(Value::Array(a)) => a.as_slice(),
            _ => &[],
        },
        _ => &[],
    };
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(list.len());
    for item in list {
        match parse_agent(item) {
            Some(a) if seen.insert(a.id.clone()) => out.push(a),
            Some(a) => tracing::debug!(id = %a.id, "duplicate registry agent id skipped"),
            None => tracing::debug!("malformed registry agent entry skipped"),
        }
    }
    out
}

fn looks_like_registry(doc: &Value) -> bool {
    matches!(doc.get("agents"), Some(Value::Array(_))) || doc.is_array()
}

// ---------- cache ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistryCache {
    pub fetched_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    pub registry: Value,
}

pub fn cache_path(cache_dir: &Path) -> PathBuf {
    cache_dir.join(CACHE_FILE)
}

pub fn read_cache(cache_dir: &Path) -> Option<RegistryCache> {
    let bytes = std::fs::read(cache_path(cache_dir)).ok()?;
    let cache: RegistryCache = serde_json::from_slice(&bytes).ok()?;
    looks_like_registry(&cache.registry).then_some(cache)
}

pub fn write_cache(cache_dir: &Path, cache: &RegistryCache) -> anyhow::Result<()> {
    std::fs::create_dir_all(cache_dir)?;
    let path = cache_path(cache_dir);
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec(cache)?)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Where the registry data came from (for logs / reports).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryOrigin {
    FreshCache,
    Network,
    NotModified,
    StaleCache,
    None,
}

enum FetchOutcome {
    Fresh { doc: Value, etag: Option<String> },
    NotModified,
}

async fn fetch(url: &str, etag: Option<&str>) -> anyhow::Result<FetchOutcome> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .connect_timeout(Duration::from_secs(5))
        .user_agent(concat!("ai-pet-hub/", env!("CARGO_PKG_VERSION"), " (pet-discovery)"))
        .build()?;
    let mut req = client.get(url).header(reqwest::header::ACCEPT, "application/json");
    if let Some(tag) = etag {
        req = req.header(reqwest::header::IF_NONE_MATCH, tag);
    }
    let resp = req.send().await?;
    if resp.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(FetchOutcome::NotModified);
    }
    let resp = resp.error_for_status()?;
    let etag = resp
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let doc: Value = resp.json().await?;
    anyhow::ensure!(looks_like_registry(&doc), "registry response has no `agents` array");
    Ok(FetchOutcome::Fresh { doc, etag })
}

/// Cache-first registry load. Never fails: returns `(None, RegistryOrigin::None)` when neither
/// the network nor the cache has data (the caller then uses the built-in list).
pub async fn load(cache_dir: &Path, url: &str, offline: bool, now: i64) -> (Option<Value>, RegistryOrigin) {
    let cached = read_cache(cache_dir);
    if let Some(c) = &cached {
        let age = now - c.fetched_at;
        if (0..CACHE_TTL_MS).contains(&age) {
            return (Some(c.registry.clone()), RegistryOrigin::FreshCache);
        }
    }
    if offline {
        return match cached {
            Some(c) => (Some(c.registry), RegistryOrigin::StaleCache),
            None => (None, RegistryOrigin::None),
        };
    }
    match fetch(url, cached.as_ref().and_then(|c| c.etag.as_deref())).await {
        Ok(FetchOutcome::Fresh { doc, etag }) => {
            let cache = RegistryCache { fetched_at: now, etag, source_url: Some(url.to_string()), registry: doc };
            if let Err(e) = write_cache(cache_dir, &cache) {
                tracing::warn!(error = %e, "could not write registry cache");
            }
            (Some(cache.registry), RegistryOrigin::Network)
        }
        Ok(FetchOutcome::NotModified) => match cached {
            Some(mut c) => {
                c.fetched_at = now;
                if let Err(e) = write_cache(cache_dir, &c) {
                    tracing::warn!(error = %e, "could not refresh registry cache timestamp");
                }
                (Some(c.registry), RegistryOrigin::NotModified)
            }
            None => (None, RegistryOrigin::None),
        },
        Err(e) => {
            tracing::warn!(error = %e, "ACP registry fetch failed; falling back to cache/built-ins");
            match cached {
                Some(c) => (Some(c.registry), RegistryOrigin::StaleCache),
                None => (None, RegistryOrigin::None),
            }
        }
    }
}

/// Binary-distribution platform keys for the running host, most preferred first.
pub fn platform_keys() -> Vec<String> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = std::env::consts::ARCH;
    let mut keys = Vec::new();
    if cfg!(target_env = "musl") {
        keys.push(format!("{os}-{arch}-musl"));
    }
    keys.push(format!("{os}-{arch}"));
    // Emulated fallbacks: Windows-on-ARM runs x64, Apple Silicon runs x64 under Rosetta.
    if arch == "aarch64" && (os == "windows" || os == "darwin") {
        keys.push(format!("{os}-x86_64"));
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub(crate) fn sample() -> Value {
        json!({
            "version": "1.0.0",
            "extensions": [],
            "agents": [
                { "id": "codex-acp", "name": "Codex", "version": "2.10.0", "description": "ACP Codex",
                  "icon": "https://cdn.example/codex.svg", "authors": ["OpenAI"], "license": "Apache-2.0",
                  "distribution": { "npx": { "package": "@agentclientprotocol/codex-acp@2.10.0" } } },
                { "id": "goose", "name": "goose", "version": "1.60.2", "description": "Block goose",
                  "distribution": { "binary": {
                      "windows-x86_64": { "archive": "https://x/goose.zip", "cmd": "./goose.exe", "args": ["acp"], "sha256": "ab" },
                      "linux-x86_64": { "archive": "https://x/goose.tar.bz2", "cmd": "./goose", "args": ["acp"] },
                      "darwin-aarch64": { "archive": "https://x/goose-mac.tar.bz2", "cmd": "./goose", "args": ["acp"] },
                      "broken": { "archive": "https://x/nothing" } } } },
                { "id": "fast-agent", "name": "fast-agent", "version": "0.10.65",
                  "distribution": { "uvx": { "package": "fast-agent-acp==0.10.65", "args": ["-x"],
                                              "env": { "FAST_AGENT_MODEL": "codexplan", "N": 3 } } } },
                { "name": "no id -> skipped" },
                "garbage",
                { "id": "goose", "name": "duplicate" },
                { "id": "future", "name": "Future", "distribution": { "docker": { "image": "x" } }, "newField": {"a": 1} }
            ]
        })
    }

    #[test]
    fn parses_real_shape_tolerantly() {
        let agents = parse_registry(&sample());
        let ids: Vec<_> = agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["codex-acp", "goose", "fast-agent", "future"]);
        let codex = &agents[0];
        assert_eq!(codex.distribution.npx.as_ref().unwrap().package, "@agentclientprotocol/codex-acp@2.10.0");
        assert_eq!(codex.icon.as_deref(), Some("https://cdn.example/codex.svg"));
        let goose = &agents[1];
        assert_eq!(goose.distribution.binary.len(), 3, "entry without cmd dropped");
        assert_eq!(goose.distribution.binary["windows-x86_64"].args, ["acp"]);
        let fa = &agents[2];
        let uvx = fa.distribution.uvx.as_ref().unwrap();
        assert_eq!(uvx.env["N"], "3");
        assert!(agents[3].distribution.is_empty());
    }

    #[test]
    fn cache_roundtrip_and_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        // Nothing cached + offline -> None.
        let (doc, origin) = rt.block_on(load(dir.path(), "http://127.0.0.1:9/none.json", true, 1_000));
        assert!(doc.is_none());
        assert_eq!(origin, RegistryOrigin::None);

        write_cache(dir.path(), &RegistryCache { fetched_at: 1_000, etag: None, source_url: None, registry: sample() }).unwrap();
        // Fresh cache is used without touching the (unreachable) network.
        let (doc, origin) = rt.block_on(load(dir.path(), "http://127.0.0.1:9/none.json", false, 2_000));
        assert_eq!(origin, RegistryOrigin::FreshCache);
        assert_eq!(parse_registry(&doc.unwrap()).len(), 4);
        // Stale cache + network failure -> stale cache.
        let later = 1_000 + CACHE_TTL_MS + 1;
        let (doc, origin) = rt.block_on(load(dir.path(), "http://127.0.0.1:9/none.json", false, later));
        assert_eq!(origin, RegistryOrigin::StaleCache);
        assert!(doc.is_some());
        // Corrupt cache is ignored.
        std::fs::write(cache_path(dir.path()), b"{not json").unwrap();
        assert!(read_cache(dir.path()).is_none());
    }

    #[test]
    fn platform_keys_match_registry_naming() {
        let keys = platform_keys();
        assert!(!keys.is_empty());
        let re_ok = keys.iter().all(|k| {
            let parts: Vec<_> = k.split('-').collect();
            ["darwin", "linux", "windows"].contains(&parts[0]) || parts[0] == std::env::consts::OS
        });
        assert!(re_ok, "{keys:?}");
    }
}
