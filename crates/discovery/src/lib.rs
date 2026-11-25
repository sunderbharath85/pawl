//! pawl-discovery: finds ACP agents (ACP registry + built-ins + custom) and handles agent hooks
//! (normalize, forward to the hub, install into agent configs).
//!
//! `discover()` order: built-ins (`claude-code`, `codex`, `gemini`, `copilot`, stable ids,
//! enriched by their registry entries), then every other registry agent in registry order,
//! then custom agents (a custom agent with an existing id replaces that entry).

use pawl_protocol::{Agent, AgentSource, MachineId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub mod detect;
pub mod hooks;
pub mod mcp_config;
pub mod registry;

use detect::{package_basename, package_bins, package_name, HostEnv};
use registry::{PackageDist, RegistryAgent};

/// A user-configured agent (hub `config.toml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomAgent {
    pub id: String,
    pub name: String,
    pub command: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DiscoveryConfig {
    /// This hub's machine; stamped on every discovered `Agent`.
    pub machine_id: MachineId,
    /// Holds `registry-cache.json` and `agents/<registry-id>/<version>/` binary installs.
    pub cache_dir: PathBuf,
    pub custom_agents: Vec<CustomAgent>,
    /// Never touch the network (use the cache regardless of age, else built-ins).
    pub offline: bool,
}

/// Fetch the ACP registry (24h cache in `cache_dir/registry-cache.json`, fall back to cache,
/// then to a built-in list), detect installed agents, merge custom agents.
pub async fn discover(cfg: &DiscoveryConfig) -> anyhow::Result<Vec<Agent>> {
    let url = std::env::var("PAWL_HUB_REGISTRY_URL").unwrap_or_else(|_| registry::REGISTRY_URL.to_string());
    let (doc, origin) = registry::load(&cfg.cache_dir, &url, cfg.offline, pawl_protocol::now_ms()).await;
    tracing::debug!(?origin, "ACP registry loaded");
    let reg = doc.as_ref().map(registry::parse_registry).unwrap_or_default();
    let cfg = cfg.clone();
    // PATH/npm probing is blocking filesystem work.
    let agents = tokio::task::spawn_blocking(move || {
        let env = HostEnv::from_system();
        build_agents(&reg, &cfg, &env)
    })
    .await?;
    Ok(agents)
}

/// Directory a registry binary distribution is (or would be) unpacked into.
pub fn binary_install_dir(cache_dir: &Path, registry_id: &str, version: Option<&str>) -> PathBuf {
    cache_dir.join("agents").join(sanitize(registry_id)).join(sanitize(version.unwrap_or("latest")))
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || "._-".contains(c) { c } else { '_' }).collect()
}

// ---------- built-ins ----------

/// An agent we always offer, with a stable hub id.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinAgent {
    pub id: &'static str,
    pub name: &'static str,
    /// Id of the same agent in the ACP registry.
    pub registry_id: &'static str,
    pub description: &'static str,
    /// npm package (unpinned) used when the registry is unavailable.
    pub npx_package: &'static str,
    pub args: &'static [&'static str],
    /// Executables that speak ACP directly when installed globally.
    pub acp_bins: &'static [&'static str],
    /// Vendor CLI executables / home dirs whose presence means the user has this agent set up.
    pub vendor_bins: &'static [&'static str],
    pub vendor_dirs: &'static [&'static str],
}

pub const BUILTIN_AGENTS: &[BuiltinAgent] = &[
    BuiltinAgent {
        id: "claude-code",
        name: "Claude Code",
        registry_id: "claude-acp",
        description: "Claude Code over ACP via @agentclientprotocol/claude-agent-acp",
        npx_package: "@agentclientprotocol/claude-agent-acp",
        args: &[],
        acp_bins: &["claude-agent-acp"],
        vendor_bins: &["claude"],
        vendor_dirs: &[".claude"],
    },
    BuiltinAgent {
        id: "codex",
        name: "Codex",
        registry_id: "codex-acp",
        description: "OpenAI Codex over ACP via @agentclientprotocol/codex-acp",
        npx_package: "@agentclientprotocol/codex-acp",
        args: &[],
        acp_bins: &["codex-acp"],
        vendor_bins: &["codex"],
        vendor_dirs: &[".codex"],
    },
    BuiltinAgent {
        id: "gemini",
        name: "Gemini CLI",
        registry_id: "gemini",
        description: "Google Gemini CLI (native ACP via --acp)",
        npx_package: "@google/gemini-cli",
        args: &["--acp"],
        acp_bins: &["gemini"],
        vendor_bins: &["gemini"],
        vendor_dirs: &[".gemini"],
    },
    BuiltinAgent {
        id: "copilot",
        name: "GitHub Copilot",
        registry_id: "github-copilot-cli",
        description: "GitHub Copilot CLI (native ACP via --acp)",
        npx_package: "@github/copilot",
        args: &["--acp"],
        acp_bins: &["copilot"],
        vendor_bins: &["copilot"],
        vendor_dirs: &[".copilot"],
    },
];

fn builtin_as_registry(b: &BuiltinAgent) -> RegistryAgent {
    RegistryAgent {
        id: b.registry_id.to_string(),
        name: b.name.to_string(),
        version: None,
        description: Some(b.description.to_string()),
        icon: None,
        distribution: registry::Distribution {
            npx: Some(PackageDist {
                package: b.npx_package.to_string(),
                args: b.args.iter().map(|s| s.to_string()).collect(),
                env: BTreeMap::new(),
            }),
            ..Default::default()
        },
    }
}

// ---------- resolution ----------

/// Pure assembly step of `discover()` (no network), exposed for tests and offline callers.
pub fn build_agents(reg: &[RegistryAgent], cfg: &DiscoveryConfig, env: &HostEnv) -> Vec<Agent> {
    let platforms = registry::platform_keys();
    let mut out: Vec<Agent> = Vec::new();
    let mut used_registry_ids = std::collections::HashSet::new();

    for b in BUILTIN_AGENTS {
        let entry = reg
            .iter()
            .find(|a| a.id == b.registry_id || a.id == b.id)
            .filter(|a| !a.distribution.is_empty());
        let entry = match entry {
            Some(e) => {
                used_registry_ids.insert(e.id.clone());
                let mut e = e.clone();
                if e.description.is_none() {
                    e.description = Some(b.description.to_string());
                }
                e
            }
            None => builtin_as_registry(b),
        };
        let mut agent = resolve(&entry, Some(b), cfg, env, &platforms);
        agent.id = b.id.to_string();
        agent.name = b.name.to_string();
        out.push(agent);
    }

    for e in reg {
        if used_registry_ids.contains(&e.id) || out.iter().any(|a| a.id == e.id) {
            continue;
        }
        if e.distribution.is_empty() {
            tracing::debug!(id = %e.id, "registry agent has no usable distribution; skipped");
            continue;
        }
        out.push(resolve(e, None, cfg, env, &platforms));
    }

    for c in &cfg.custom_agents {
        if c.id.trim().is_empty() || c.command.is_empty() {
            tracing::warn!(id = %c.id, "custom agent without id/command ignored");
            continue;
        }
        let agent = custom_agent(c, &cfg.machine_id, env);
        match out.iter_mut().find(|a| a.id == c.id) {
            Some(slot) => *slot = agent,
            None => out.push(agent),
        }
    }
    out
}

fn custom_agent(c: &CustomAgent, machine_id: &str, env: &HostEnv) -> Agent {
    let mut command = c.command.clone();
    let resolved = env.which(&command[0]);
    let installed = resolved.is_some();
    if let Some(p) = resolved {
        command[0] = p.to_string_lossy().into_owned();
    }
    Agent {
        id: c.id.clone(),
        machine_id: machine_id.to_string(),
        name: if c.name.trim().is_empty() { c.id.clone() } else { c.name.clone() },
        command,
        env: c.env.clone().filter(|e| !e.is_empty()),
        version: None,
        description: None,
        icon: None,
        installed,
        source: AgentSource::Custom,
        capabilities: None,
    }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

fn non_empty(env: BTreeMap<String, String>) -> Option<BTreeMap<String, String>> {
    (!env.is_empty()).then_some(env)
}

/// `./dist-package\\cursor-agent.cmd` -> path under `base`.
fn join_cmd(base: &Path, cmd: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for part in cmd.split(['/', '\\']).filter(|s| !s.is_empty() && *s != ".") {
        p.push(part);
    }
    p
}

fn cmd_stem(cmd: &str) -> String {
    let last = cmd.rsplit(['/', '\\']).next().unwrap_or(cmd);
    let lower = last.to_lowercase();
    for ext in [".exe", ".cmd", ".bat", ".com", ".ps1", ".sh"] {
        if lower.ends_with(ext) {
            return last[..last.len() - ext.len()].to_string();
        }
    }
    last.to_string()
}

/// A previously unpacked binary for `id`: exact version first, else any version dir.
fn cached_binary(cfg: &DiscoveryConfig, id: &str, version: Option<&str>, cmd: &str, env: &HostEnv) -> Option<PathBuf> {
    let exact = join_cmd(&binary_install_dir(&cfg.cache_dir, id, version), cmd);
    if let Some(p) = env.which(&path_str(&exact)) {
        return Some(p);
    }
    let root = cfg.cache_dir.join("agents").join(sanitize(id));
    let mut versions: Vec<PathBuf> = std::fs::read_dir(&root).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    versions.sort();
    versions.into_iter().rev().find_map(|v| env.which(&path_str(&join_cmd(&v, cmd))))
}

fn resolve(e: &RegistryAgent, builtin: Option<&BuiltinAgent>, cfg: &DiscoveryConfig, env: &HostEnv, platforms: &[String]) -> Agent {
    let mut agent = Agent {
        id: e.id.clone(),
        machine_id: cfg.machine_id.clone(),
        name: e.name.clone(),
        command: Vec::new(),
        env: None,
        version: e.version.clone(),
        description: e.description.clone(),
        icon: e.icon.clone(),
        installed: false,
        source: AgentSource::Registry,
        capabilities: None,
    };
    let d = &e.distribution;
    let binary = platforms.iter().find_map(|k| d.binary.get(k));
    let vendor_present = builtin.is_some_and(|b| {
        b.vendor_bins.iter().any(|n| env.which(n).is_some()) || b.vendor_dirs.iter().any(|dir| env.home_has(dir))
    });

    // 1. Registry binary: unpacked in our cache, or the same executable on PATH.
    if let Some(t) = binary {
        let found = cached_binary(cfg, &e.id, e.version.as_deref(), &t.cmd, env).or_else(|| env.which(&cmd_stem(&t.cmd)));
        if let Some(p) = found {
            agent.command = std::iter::once(path_str(&p)).chain(t.args.iter().cloned()).collect();
            agent.env = non_empty(t.env.clone());
            agent.installed = true;
            return agent;
        }
    }

    // 2. npx package: global install (direct bin), else `npx -y <pkg>`.
    if let Some(npx) = &d.npx {
        let name = package_name(&npx.package);
        let mut bins: Vec<String> = builtin.map(|b| b.acp_bins.iter().map(|s| s.to_string()).collect()).unwrap_or_default();
        let global = env.global_package_dir(name);
        if let Some(g) = &global {
            bins.extend(package_bins(g));
        }
        if builtin.is_none() && global.is_none() {
            // Unknown bin name: only trust the package basename if a global install exists.
            bins.clear();
        }
        bins.dedup();
        // A global bin only counts if the package is globally installed (or it is a known ACP bin).
        if let Some(p) = bins.iter().find_map(|b| env.which(b)) {
            if global.is_some() || builtin.is_some_and(|b| b.acp_bins.contains(&cmd_stem(&path_str(&p)).as_str())) {
                agent.command = std::iter::once(path_str(&p)).chain(npx.args.iter().cloned()).collect();
                agent.env = non_empty(npx.env.clone());
                agent.installed = true;
                return agent;
            }
        }
        if let Some(npx_exe) = env.which("npx") {
            agent.command = [path_str(&npx_exe), "-y".to_string(), npx.package.clone()]
                .into_iter()
                .chain(npx.args.iter().cloned())
                .collect();
            agent.env = non_empty(npx.env.clone());
            agent.installed = !env.npx_cached(name).is_empty() || vendor_present;
            return agent;
        }
    }

    // 3. uvx package: tool installed on PATH, else `uvx <pkg>`.
    if let Some(uvx) = &d.uvx {
        let bin = package_basename(&uvx.package).to_string();
        if let Some(p) = env.which(&bin) {
            agent.command = std::iter::once(path_str(&p)).chain(uvx.args.iter().cloned()).collect();
            agent.env = non_empty(uvx.env.clone());
            agent.installed = true;
            return agent;
        }
        if let Some(uvx_exe) = env.which("uvx") {
            agent.command = [path_str(&uvx_exe), uvx.package.clone()].into_iter().chain(uvx.args.iter().cloned()).collect();
            agent.env = non_empty(uvx.env.clone());
            agent.installed = vendor_present;
            return agent;
        }
    }

    // 4. Nothing runnable yet: best-effort command so the UI can show how it would launch.
    if let Some(npx) = &d.npx {
        agent.command = ["npx".to_string(), "-y".to_string(), npx.package.clone()].into_iter().chain(npx.args.iter().cloned()).collect();
        agent.env = non_empty(npx.env.clone());
    } else if let Some(uvx) = &d.uvx {
        agent.command = ["uvx".to_string(), uvx.package.clone()].into_iter().chain(uvx.args.iter().cloned()).collect();
        agent.env = non_empty(uvx.env.clone());
    } else if let Some(t) = binary {
        let p = join_cmd(&binary_install_dir(&cfg.cache_dir, &e.id, e.version.as_deref()), &t.cmd);
        agent.command = std::iter::once(path_str(&p)).chain(t.args.iter().cloned()).collect();
        agent.env = non_empty(t.env.clone());
    } else if let Some((_, t)) = d.binary.iter().next() {
        // No build for this platform: keep the command name for display, never installed.
        agent.command = std::iter::once(cmd_stem(&t.cmd)).chain(t.args.iter().cloned()).collect();
    }
    agent
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fake_exe(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, b"x").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    fn exe(name: &str) -> String {
        if cfg!(windows) { format!("{name}.cmd") } else { name.to_string() }
    }

    fn host(path_dirs: Vec<PathBuf>, home: Option<PathBuf>) -> HostEnv {
        HostEnv::with_paths(cfg!(windows), path_dirs, ".COM;.EXE;.BAT;.CMD", home)
    }

    fn registry_doc() -> serde_json::Value {
        let plat = registry::platform_keys()[0].clone();
        json!({ "version": "1.0.0", "agents": [
            { "id": "claude-acp", "name": "Claude Agent", "version": "0.110.0", "description": "ACP wrapper for Anthropic's Claude",
              "icon": "https://cdn/claude.svg",
              "distribution": { "npx": { "package": "@agentclientprotocol/claude-agent-acp@0.110.0" } } },
            { "id": "codex-acp", "name": "Codex", "version": "2.10.0",
              "distribution": { "npx": { "package": "@agentclientprotocol/codex-acp@2.10.0" } } },
            { "id": "goose", "name": "goose", "version": "1.60.2",
              "distribution": { "binary": { plat.clone(): { "cmd": "./goose", "args": ["acp"], "archive": "https://x" } } } },
            { "id": "amp-acp", "name": "Amp", "version": "0.13.0",
              "distribution": { "binary": { plat: { "cmd": "./bin/amp-acp", "archive": "https://x", "env": {"AMP": "1"} } } } },
            { "id": "auggie", "name": "Auggie CLI", "version": "0.37.0",
              "distribution": { "npx": { "package": "@augmentcode/auggie@0.37.0", "args": ["--acp"], "env": {"AUGMENT_DISABLE_AUTO_UPDATE": "1"} } } },
            { "id": "fast-agent", "name": "fast-agent", "version": "0.10.65",
              "distribution": { "uvx": { "package": "fast-agent-acp==0.10.65", "args": ["-x"] } } },
            { "id": "dockerish", "name": "Future", "distribution": { "docker": {} } }
        ]})
    }

    #[test]
    fn builtins_present_without_registry_or_tools() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = DiscoveryConfig { cache_dir: dir.path().join("cache"), ..Default::default() };
        let agents = build_agents(&[], &cfg, &host(vec![], None));
        let ids: Vec<_> = agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["claude-code", "codex", "gemini", "copilot"]);
        let claude = &agents[0];
        assert_eq!(claude.command, ["npx", "-y", "@agentclientprotocol/claude-agent-acp"]);
        assert!(!claude.installed);
        assert_eq!(agents[2].command, ["npx", "-y", "@google/gemini-cli", "--acp"]);
        assert!(agents.iter().all(|a| a.source == AgentSource::Registry));
    }

    #[test]
    fn registry_mapping_and_detection() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let npx = fake_exe(&bin, &exe("npx"));
        let goose = fake_exe(&bin, if cfg!(windows) { "goose.exe" } else { "goose" });
        fake_exe(&bin, &exe("codex")); // vendor CLI present -> codex counts as installed
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        let cache_dir = dir.path().join("cache");
        let amp_dir = binary_install_dir(&cache_dir, "amp-acp", Some("0.13.0")).join("bin");
        let amp = fake_exe(&amp_dir, if cfg!(windows) { "amp-acp.exe" } else { "amp-acp" });

        let cfg = DiscoveryConfig {
            machine_id: "abcdef012345".into(),
            cache_dir,
            custom_agents: vec![
                CustomAgent { id: "my-agent".into(), name: "Mine".into(), command: vec![path_str(&goose), "serve".into()], env: None },
                CustomAgent { id: "gemini".into(), name: "My Gemini".into(), command: vec!["definitely-missing-xyz".into()], env: Some(BTreeMap::from([("K".into(), "V".into())])) },
            ],
            offline: true,
        };
        let reg = registry::parse_registry(&registry_doc());
        let agents = build_agents(&reg, &cfg, &host(vec![bin.clone()], Some(home)));
        let by = |id: &str| agents.iter().find(|a| a.id == id).unwrap_or_else(|| panic!("{id} missing"));

        let ids: Vec<_> = agents.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["claude-code", "codex", "gemini", "copilot", "goose", "amp-acp", "auggie", "fast-agent", "my-agent"]);
        assert!(agents.iter().all(|a| a.machine_id == "abcdef012345"), "every agent carries cfg.machine_id");
        assert!(!ids.contains(&"claude-acp") && !ids.contains(&"codex-acp"), "aliases folded into built-ins");

        let claude = by("claude-code");
        assert_eq!(claude.name, "Claude Code");
        assert_eq!(claude.command, [path_str(&npx), "-y".into(), "@agentclientprotocol/claude-agent-acp@0.110.0".into()]);
        assert_eq!(claude.version.as_deref(), Some("0.110.0"));
        assert_eq!(claude.icon.as_deref(), Some("https://cdn/claude.svg"));
        assert!(claude.installed, "~/.claude marks Claude as set up");

        assert!(by("codex").installed);
        assert!(!by("copilot").installed);

        let g = by("goose");
        assert_eq!(g.command, [path_str(&goose), "acp".into()]);
        assert!(g.installed);

        let a = by("amp-acp");
        assert_eq!(a.command, [path_str(&amp)]);
        assert_eq!(a.env.as_ref().unwrap()["AMP"], "1");
        assert!(a.installed, "cached binary");

        let au = by("auggie");
        assert_eq!(au.command, [path_str(&npx), "-y".into(), "@augmentcode/auggie@0.37.0".into(), "--acp".into()]);
        assert!(!au.installed, "npx alone does not mean installed");
        assert_eq!(au.env.as_ref().unwrap()["AUGMENT_DISABLE_AUTO_UPDATE"], "1");

        let fa = by("fast-agent");
        assert_eq!(fa.command, ["uvx", "fast-agent-acp==0.10.65", "-x"]);
        assert!(!fa.installed);

        let gem = by("gemini");
        assert_eq!(gem.source, AgentSource::Custom);
        assert_eq!(gem.name, "My Gemini");
        assert!(!gem.installed);
        let mine = by("my-agent");
        assert!(mine.installed);
        assert_eq!(mine.command[1], "serve");
    }

    #[test]
    fn global_acp_bin_preferred_over_npx() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fake_exe(&bin, &exe("npx"));
        let gemini = fake_exe(&bin, &exe("gemini"));
        let cfg = DiscoveryConfig { cache_dir: dir.path().join("c"), ..Default::default() };
        let agents = build_agents(&[], &cfg, &host(vec![bin], None));
        let gem = agents.iter().find(|a| a.id == "gemini").unwrap();
        assert_eq!(gem.command, [path_str(&gemini), "--acp".into()]);
        assert!(gem.installed);
    }

    #[test]
    fn discover_offline_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        registry::write_cache(
            dir.path(),
            &registry::RegistryCache { fetched_at: 0, etag: None, source_url: None, registry: registry_doc() },
        )
        .unwrap();
        let cfg = DiscoveryConfig { machine_id: "m0m0m0m0m0m0".into(), cache_dir: dir.path().to_path_buf(), custom_agents: vec![], offline: true };
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let agents = rt.block_on(discover(&cfg)).unwrap();
        for id in ["claude-code", "codex", "gemini", "copilot", "goose", "auggie"] {
            assert!(agents.iter().any(|a| a.id == id), "{id}");
        }
        assert!(agents.iter().all(|a| !a.command.is_empty()));
        assert!(agents.iter().all(|a| a.machine_id == "m0m0m0m0m0m0"));
    }
}
