//! Host probing: PATH lookup (with Windows `PATHEXT`), npx/uvx availability, npm global
//! installs, the npx cache and per-agent binary cache dirs.

use serde_json::Value;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Snapshot of everything discovery needs from the host. Built once per `discover()` so a run
/// costs one `read_dir` per PATH entry instead of thousands of `stat`s.
#[derive(Debug, Clone, Default)]
pub struct HostEnv {
    pub windows: bool,
    pub path_dirs: Vec<PathBuf>,
    /// Lowercased extensions including the dot, in PATHEXT order (Windows only).
    pub pathext: Vec<String>,
    pub home: Option<PathBuf>,
    /// npm cache root (`.../npm-cache` or `~/.npm`), whose `_npx/*/node_modules` holds npx installs.
    pub npm_cache: Option<PathBuf>,
    /// Global `node_modules` roots (npm prefix, bun global).
    pub global_node_modules: Vec<PathBuf>,
    /// dir -> (lookup key -> real file name); keys are lowercased on Windows.
    index: HashMap<PathBuf, HashMap<String, String>>,
    /// package name -> package dirs found in the npx cache
    npx_cache: HashMap<String, Vec<PathBuf>>,
}

fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var).filter(|v| !v.is_empty()).map(PathBuf::from)
}

impl HostEnv {
    /// Probe the running system.
    pub fn from_system() -> Self {
        let windows = cfg!(windows);
        let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).filter(|d| !d.as_os_str().is_empty()).collect())
            .unwrap_or_default();
        let pathext = if windows {
            parse_pathext(std::env::var_os("PATHEXT"))
        } else {
            Vec::new()
        };
        let home = dirs::home_dir();

        let npm_cache = env_path("npm_config_cache").or_else(|| env_path("NPM_CONFIG_CACHE")).or_else(|| {
            if windows {
                dirs::cache_dir().map(|d| d.join("npm-cache")) // %LOCALAPPDATA%\npm-cache
            } else {
                home.as_ref().map(|h| h.join(".npm"))
            }
        });

        let mut global_node_modules = Vec::new();
        let prefix = env_path("npm_config_prefix").or_else(|| env_path("NPM_CONFIG_PREFIX"));
        if let Some(p) = prefix {
            global_node_modules.push(if windows { p.join("node_modules") } else { p.join("lib").join("node_modules") });
        }
        if windows {
            if let Some(appdata) = env_path("APPDATA") {
                global_node_modules.push(appdata.join("npm").join("node_modules"));
            }
        } else {
            if let Some(h) = &home {
                global_node_modules.push(h.join(".npm-global").join("lib").join("node_modules"));
            }
            global_node_modules.push(PathBuf::from("/usr/local/lib/node_modules"));
            global_node_modules.push(PathBuf::from("/opt/homebrew/lib/node_modules"));
            global_node_modules.push(PathBuf::from("/usr/lib/node_modules"));
        }
        if let Some(h) = &home {
            global_node_modules.push(h.join(".bun").join("install").join("global").join("node_modules"));
        }

        let mut env = HostEnv { windows, path_dirs, pathext, home, npm_cache, global_node_modules, ..Default::default() };
        // npm prefix derived from where `npm` lives (nvm, fnm, volta, custom prefixes).
        if let Some(npm) = env.which("npm") {
            let npm = std::fs::canonicalize(&npm).unwrap_or(npm);
            if let Some(bin_dir) = npm.parent() {
                let root = if windows {
                    bin_dir.join("node_modules")
                } else {
                    bin_dir.parent().map(|p| p.join("lib").join("node_modules")).unwrap_or_else(|| bin_dir.join("node_modules"))
                };
                if !env.global_node_modules.contains(&root) {
                    env.global_node_modules.push(root);
                }
            }
        }
        env.build_index();
        env
    }

    /// Test/explicit constructor.
    pub fn with_paths(windows: bool, path_dirs: Vec<PathBuf>, pathext: &str, home: Option<PathBuf>) -> Self {
        let mut env = HostEnv {
            windows,
            path_dirs,
            pathext: if windows { parse_pathext(Some(OsString::from(pathext))) } else { Vec::new() },
            home,
            ..Default::default()
        };
        env.build_index();
        env
    }

    pub fn set_npm_dirs(&mut self, npm_cache: Option<PathBuf>, global_node_modules: Vec<PathBuf>) {
        self.npm_cache = npm_cache;
        self.global_node_modules = global_node_modules;
        self.build_index();
    }

    fn build_index(&mut self) {
        self.index.clear();
        for dir in &self.path_dirs {
            if self.index.contains_key(dir) {
                continue;
            }
            let mut names = HashMap::new();
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    let key = if self.windows { n.to_lowercase() } else { n.clone() };
                    names.entry(key).or_insert(n);
                }
            }
            self.index.insert(dir.clone(), names);
        }
        self.npx_cache.clear();
        if let Some(cache) = &self.npm_cache {
            if let Ok(rd) = std::fs::read_dir(cache.join("_npx")) {
                for e in rd.flatten() {
                    let nm = e.path().join("node_modules");
                    for (name, dir) in list_packages(&nm) {
                        self.npx_cache.entry(name).or_default().push(dir);
                    }
                }
            }
        }
    }

    /// Resolve an executable like a shell would: bare names are searched on PATH (Windows:
    /// with every `PATHEXT` extension); names with a path separator are checked directly.
    pub fn which(&self, name: &str) -> Option<PathBuf> {
        if name.is_empty() {
            return None;
        }
        let p = Path::new(name);
        if p.components().count() > 1 || p.is_absolute() {
            return self.check_candidate_path(p);
        }
        let lower = name.to_lowercase();
        let has_ext = self.windows && self.pathext.iter().any(|e| lower.ends_with(e.as_str()));
        for dir in &self.path_dirs {
            let Some(names) = self.index.get(dir) else { continue };
            if self.windows {
                if has_ext {
                    if let Some(real) = names.get(&lower) {
                        return Some(dir.join(real));
                    }
                }
                for ext in &self.pathext {
                    if let Some(real) = names.get(&format!("{lower}{ext}")) {
                        return Some(dir.join(real));
                    }
                }
            } else if names.contains_key(name) {
                let full = dir.join(name);
                if is_executable(&full) {
                    return Some(full);
                }
            }
        }
        None
    }

    fn check_candidate_path(&self, p: &Path) -> Option<PathBuf> {
        if p.is_file() && (self.windows || is_executable(p)) {
            return Some(p.to_path_buf());
        }
        if self.windows && p.extension().is_none() {
            for ext in &self.pathext {
                let mut s = p.as_os_str().to_owned();
                s.push(ext);
                let cand = PathBuf::from(s);
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
        None
    }

    /// Package dir of `package` in a global node_modules root, if installed globally.
    pub fn global_package_dir(&self, package: &str) -> Option<PathBuf> {
        self.global_node_modules
            .iter()
            .map(|root| package_dir(root, package))
            .find(|d| d.join("package.json").is_file())
    }

    /// Package dirs of `package` inside the npx cache.
    pub fn npx_cached(&self, package: &str) -> &[PathBuf] {
        self.npx_cache.get(package).map(Vec::as_slice).unwrap_or(&[])
    }

    /// `~/<rel>` exists.
    pub fn home_has(&self, rel: &str) -> bool {
        self.home.as_ref().is_some_and(|h| h.join(rel).exists())
    }
}

fn parse_pathext(v: Option<OsString>) -> Vec<String> {
    let raw = v
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
    let mut out: Vec<String> = Vec::new();
    for e in raw.split(';') {
        let e = e.trim().to_lowercase();
        if e.is_empty() {
            continue;
        }
        let e = if e.starts_with('.') { e } else { format!(".{e}") };
        if !out.contains(&e) {
            out.push(e);
        }
    }
    out
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

fn package_dir(root: &Path, package: &str) -> PathBuf {
    let mut d = root.to_path_buf();
    for part in package.split('/') {
        d.push(part);
    }
    d
}

/// (package name, dir) for every package in a node_modules dir (incl. `@scope/*`).
fn list_packages(node_modules: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(node_modules) else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        if name.starts_with('@') {
            if let Ok(scoped) = std::fs::read_dir(&path) {
                for s in scoped.flatten() {
                    out.push((format!("{name}/{}", s.file_name().to_string_lossy()), s.path()));
                }
            }
        } else {
            out.push((name, path));
        }
    }
    out
}

/// Bin names declared by `<pkg_dir>/package.json` (`bin` string or map).
pub fn package_bins(pkg_dir: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(pkg_dir.join("package.json")) else { return Vec::new() };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else { return Vec::new() };
    match v.get("bin") {
        Some(Value::Object(m)) => m.keys().cloned().collect(),
        Some(Value::String(_)) => v
            .get("name")
            .and_then(Value::as_str)
            .map(|n| vec![n.rsplit('/').next().unwrap_or(n).to_string()])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// `@scope/name@1.2.3` -> `@scope/name`; `name@^1` -> `name`; `pkg==1.0` (uvx) -> `pkg`.
pub fn package_name(spec: &str) -> &str {
    let spec = spec.trim();
    let spec = spec.split("==").next().unwrap_or(spec);
    let spec = spec.split(['<', '>', '~', '!', '[']).next().unwrap_or(spec);
    if let Some(rest) = spec.strip_prefix('@') {
        match rest.find('@') {
            Some(i) => &spec[..i + 1],
            None => spec,
        }
    } else {
        spec.split('@').next().unwrap_or(spec)
    }
}

/// Last path segment of a package name (`@google/gemini-cli` -> `gemini-cli`).
pub fn package_basename(spec: &str) -> &str {
    let n = package_name(spec);
    n.rsplit('/').next().unwrap_or(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_parsing() {
        assert_eq!(package_name("@agentclientprotocol/codex-acp@2.10.0"), "@agentclientprotocol/codex-acp");
        assert_eq!(package_name("@google/gemini-cli"), "@google/gemini-cli");
        assert_eq!(package_name("droid@1.18.0"), "droid");
        assert_eq!(package_name("fast-agent-acp==0.10.65"), "fast-agent-acp");
        assert_eq!(package_name("minion-code@0.1.44"), "minion-code");
        assert_eq!(package_basename("@google/gemini-cli@1"), "gemini-cli");
    }

    #[test]
    fn which_uses_pathext_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("npx.CMD"), b"@echo off").unwrap();
        std::fs::write(dir.path().join("tool.exe"), b"MZ").unwrap();
        std::fs::write(dir.path().join("noext"), b"x").unwrap();
        let env = HostEnv::with_paths(true, vec![dir.path().to_path_buf()], ".COM;.EXE;.BAT;.CMD", None);
        assert_eq!(env.which("npx").unwrap(), dir.path().join("npx.CMD"));
        assert_eq!(env.which("tool").unwrap(), dir.path().join("tool.exe"));
        assert_eq!(env.which("TOOL.EXE").unwrap(), dir.path().join("tool.exe"), "real on-disk name");
        assert!(env.which("noext").is_none(), "Windows cannot spawn extension-less files");
        assert!(env.which("missing").is_none());
        let abs = dir.path().join("tool");
        assert_eq!(env.which(abs.to_str().unwrap()).unwrap(), dir.path().join("tool.exe"));
    }

    #[cfg(unix)]
    #[test]
    fn which_requires_exec_bit_on_unix() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("agent");
        std::fs::write(&p, b"#!/bin/sh").unwrap();
        let env = HostEnv::with_paths(false, vec![dir.path().to_path_buf()], "", None);
        assert!(env.which("agent").is_none());
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        let env = HostEnv::with_paths(false, vec![dir.path().to_path_buf()], "", None);
        assert_eq!(env.which("agent").unwrap(), p);
    }

    #[test]
    fn npx_cache_and_global_detection() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("npm-cache");
        let pkg = cache.join("_npx").join("abc123").join("node_modules").join("@agentclientprotocol").join("codex-acp");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("package.json"), br#"{"name":"@agentclientprotocol/codex-acp","bin":{"codex-acp":"dist/index.js"}}"#).unwrap();
        let global = dir.path().join("global");
        let g = global.join("@google").join("gemini-cli");
        std::fs::create_dir_all(&g).unwrap();
        std::fs::write(g.join("package.json"), br#"{"name":"@google/gemini-cli","bin":{"gemini":"bundle/gemini.js"}}"#).unwrap();
        let mut env = HostEnv::with_paths(true, vec![], ".EXE", None);
        env.set_npm_dirs(Some(cache), vec![global]);
        assert_eq!(env.npx_cached("@agentclientprotocol/codex-acp").len(), 1);
        assert!(env.npx_cached("@google/gemini-cli").is_empty());
        let gd = env.global_package_dir("@google/gemini-cli").unwrap();
        assert_eq!(package_bins(&gd), ["gemini"]);
        assert_eq!(package_bins(&env.npx_cached("@agentclientprotocol/codex-acp")[0]), ["codex-acp"]);
    }
}
