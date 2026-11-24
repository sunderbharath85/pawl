//! The repo index (PLAN step 5): the folders this hub runs agents in, shared with paired hubs in its
//! presence and used to resolve "in repo X" and to allow remote starts.
//!
//! Built from the canonical cwds of this machine's own sessions (owned and hook-tracked, with the
//! time each was last used) plus config.toml `[repos] paths`. Folders that no longer exist are
//! dropped, names are made unique per machine ("api (work)"), and the newest REPO_INDEX_MAX are kept.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf, Prefix};

use krypto_protocol::{RepoRef, UnixMs, REPO_INDEX_MAX};

/// Peer-supplied repo fields beyond these are refused.
pub const PEER_REPO_NAME_MAX: usize = 80;
pub const PEER_REPO_PATH_MAX: usize = 1024;

/// `p` as a plain path: the Windows verbatim prefix of a drive path is dropped (`\\?\C:\x` -> `C:\x`).
/// None for network shares (`\\server\share`, `\\?\UNC\...`) and device paths (`\\.\...`, other `\\?\`).
pub fn plain_path(p: &Path) -> Option<PathBuf> {
    let mut parts = p.components();
    match parts.next() {
        Some(Component::Prefix(pre)) => match pre.kind() {
            Prefix::Disk(_) => Some(p.to_path_buf()),
            Prefix::VerbatimDisk(d) => {
                let mut out = PathBuf::from(format!("{}:\\", d as char));
                for c in parts {
                    if let Component::Normal(n) = c {
                        out.push(n);
                    }
                }
                Some(out)
            }
            _ => None,
        },
        _ => Some(p.to_path_buf()),
    }
}

/// An existing directory, canonicalized as a plain local path (None otherwise, or for a share/device path).
pub fn canonical_dir(p: &Path) -> Option<PathBuf> {
    let c = plain_path(&std::fs::canonicalize(p).ok()?)?;
    c.is_dir().then_some(c)
}

/// Path components for comparison (case-insensitive on Windows).
pub fn path_key(p: &Path) -> Vec<String> {
    p.components()
        .map(|c| {
            let s = c.as_os_str().to_string_lossy().into_owned();
            if cfg!(windows) {
                s.to_lowercase()
            } else {
                s
            }
        })
        .collect()
}

/// `child` is `root` or inside it.
pub fn is_within(child: &Path, root: &Path) -> bool {
    let (c, r) = (path_key(child), path_key(root));
    c.len() >= r.len() && c[..r.len()] == r[..]
}

/// The repo index from candidate folders `(path, last used)`: canonical existing folders only,
/// one entry per folder (latest use wins), newest first, at most REPO_INDEX_MAX, unique names.
pub fn build_index(candidates: impl IntoIterator<Item = (String, Option<UnixMs>)>) -> Vec<RepoRef> {
    let mut by_key: HashMap<Vec<String>, (PathBuf, Option<UnixMs>)> = HashMap::new();
    for (raw, used) in candidates {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let Some(dir) = canonical_dir(Path::new(raw)) else { continue };
        let e = by_key.entry(path_key(&dir)).or_insert((dir, None));
        e.1 = e.1.max(used);
    }
    let mut folders: Vec<(PathBuf, Option<UnixMs>)> = by_key.into_values().collect();
    // Newest first; folders never used (config paths) after them, by path.
    folders.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    folders.truncate(REPO_INDEX_MAX);
    let names = unique_names(&folders.iter().map(|f| f.0.clone()).collect::<Vec<_>>());
    folders
        .into_iter()
        .zip(names)
        .map(|((path, used), name)| RepoRef { name, path: path.to_string_lossy().into_owned(), last_used_at: used })
        .collect()
}

/// The folder's own name (the path itself for a drive or filesystem root).
fn base_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// The `depth` parent folder names of `p`, nearest last ("work/api" style), or None if it has fewer.
fn parents(p: &Path, depth: usize) -> Option<String> {
    let names: Vec<String> = p
        .parent()?
        .components()
        .filter_map(|c| match c {
            Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    (names.len() >= depth).then(|| names[names.len() - depth..].join("/"))
}

/// Names unique per machine (case-insensitive): a clashing "api" becomes "api (work)", then
/// "api (code/work)", ... and as a last resort "api (2)".
pub fn unique_names(paths: &[PathBuf]) -> Vec<String> {
    let key = |s: &str| s.to_lowercase();
    let mut names: Vec<String> = paths.iter().map(|p| base_name(p)).collect();
    let mut depth = 1;
    loop {
        let mut count: HashMap<String, usize> = HashMap::new();
        for n in &names {
            *count.entry(key(n)).or_default() += 1;
        }
        let clashing: Vec<usize> = (0..names.len()).filter(|i| count[&key(&names[*i])] > 1).collect();
        if clashing.is_empty() {
            return names;
        }
        let mut progressed = false;
        for i in clashing {
            if let Some(par) = parents(&paths[i], depth) {
                names[i] = format!("{} ({par})", base_name(&paths[i]));
                progressed = true;
            }
        }
        if !progressed {
            break;
        }
        depth += 1;
    }
    // Still clashing (same names all the way up): number them in order.
    let mut seen: HashMap<String, usize> = HashMap::new();
    for n in names.iter_mut() {
        let c = seen.entry(key(n)).or_default();
        *c += 1;
        if *c > 1 {
            *n = format!("{n} ({c})");
        }
    }
    names
}

/// A paired hub sends at most this many agents in its presence (more are dropped).
pub const PEER_AGENTS_MAX: usize = 64;
const PEER_AGENT_ID_MAX: usize = 64;
const PEER_AGENT_NAME_MAX: usize = 80;

/// A paired hub's agents as they arrive in its presence: an agent whose id is not 1-64 characters of
/// `[A-Za-z0-9._-]`, or whose name is empty, over 80 characters or has control characters, is
/// dropped; at most PEER_AGENTS_MAX are kept. They feed the brain ("tell the Mac mini's codex")
/// and the agent pickers.
pub fn validate_peer_agents(agents: Vec<krypto_protocol::Agent>) -> Vec<krypto_protocol::Agent> {
    let id_ok = |id: &str| {
        !id.is_empty()
            && id.chars().count() <= PEER_AGENT_ID_MAX
            && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    };
    let name_ok = |n: &str| !n.trim().is_empty() && n.chars().count() <= PEER_AGENT_NAME_MAX && !n.chars().any(char::is_control);
    agents.into_iter().filter(|a| id_ok(&a.id) && name_ok(&a.name)).take(PEER_AGENTS_MAX).collect()
}

/// A paired hub's repo index as it arrives in its presence: items with an empty or over-long name
/// or path, or control characters, are dropped; at most REPO_INDEX_MAX are kept.
pub fn validate_peer_repos(repos: Vec<RepoRef>) -> Vec<RepoRef> {
    let ok = |s: &str, max: usize| !s.trim().is_empty() && s.chars().count() <= max && !s.chars().any(char::is_control);
    repos
        .into_iter()
        .filter(|r| ok(&r.name, PEER_REPO_NAME_MAX) && ok(&r.path, PEER_REPO_PATH_MAX))
        .take(REPO_INDEX_MAX)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(root: &Path, rel: &str) -> PathBuf {
        let p = root.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn the_index_is_canonical_deduped_named_and_newest_first() {
        let t = tempfile::tempdir().unwrap();
        let api_work = dir(t.path(), "work/api");
        let api_home = dir(t.path(), "home/api");
        let web = dir(t.path(), "work/web");
        let cfg_only = dir(t.path(), "notes");
        let idx = build_index(vec![
            (s(&api_work), Some(10)),
            // the same folder through `..` and (Windows) another case: one entry, latest use wins
            (s(&api_work.join("..").join("api")), Some(30)),
            (s(&web), Some(20)),
            (s(&api_home), Some(5)),
            (s(&t.path().join("gone")), Some(99)), // no longer exists
            (s(&cfg_only), None),                  // a config path never used
            ("   ".into(), Some(1)),
        ]);
        let names: Vec<&str> = idx.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["api (work)", "web", "api (home)", "notes"]);
        assert_eq!(idx[0].last_used_at, Some(30));
        assert_eq!(idx[3].last_used_at, None);
        let canon = canonical_dir(&api_work).unwrap();
        assert_eq!(idx[0].path, s(&canon));
        assert!(!idx[0].path.starts_with(r"\\?\"), "plain paths: {}", idx[0].path);
        if cfg!(windows) {
            let upper = build_index(vec![(s(&api_work).to_uppercase(), Some(1)), (s(&api_work), Some(2))]);
            assert_eq!(upper.len(), 1, "case-insensitive on Windows");
        }
    }

    #[test]
    fn the_index_is_capped() {
        let t = tempfile::tempdir().unwrap();
        let cands: Vec<(String, Option<UnixMs>)> =
            (0..(REPO_INDEX_MAX + 20)).map(|i| (s(&dir(t.path(), &format!("r{i:03}"))), Some(i as UnixMs))).collect();
        let idx = build_index(cands);
        assert_eq!(idx.len(), REPO_INDEX_MAX);
        assert_eq!(idx[0].name, format!("r{:03}", REPO_INDEX_MAX + 19), "newest first");
        assert!(idx.iter().all(|r| r.name != "r000"), "the oldest are dropped");
    }

    #[test]
    fn clashing_names_get_more_of_their_path() {
        let p = |x: &str| PathBuf::from(x);
        let n = unique_names(&[p("/a/x/api"), p("/b/x/api"), p("/c/web"), p("/d/API")]);
        assert_eq!(n, vec!["api (a/x)", "api (b/x)", "web", "API (d)"]);
        let n = unique_names(&[p("/x/api"), p("/x/api")]);
        assert_eq!(n, vec!["api (x)", "api (x) (2)"], "identical paths still get unique names");
    }

    #[test]
    fn peer_repos_are_validated() {
        let r = |name: &str, path: &str| RepoRef { name: name.into(), path: path.into(), last_used_at: None };
        let long_name = "n".repeat(PEER_REPO_NAME_MAX + 1);
        let long_path = "/".repeat(PEER_REPO_PATH_MAX + 1);
        let got = validate_peer_repos(vec![
            r("api", "/srv/api"),
            r("", "/srv/x"),
            r("x", "  "),
            r(&long_name, "/srv/y"),
            r("y", &long_path),
            r("bad\u{1b}[2J", "/srv/z"),
            r("z", "/srv/z\n"),
            r("web", "C:\\work\\web"),
        ]);
        assert_eq!(got.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), vec!["api", "web"]);
        let many: Vec<RepoRef> = (0..300).map(|i| r(&format!("r{i}"), &format!("/r/{i}"))).collect();
        assert_eq!(validate_peer_repos(many).len(), REPO_INDEX_MAX);
    }

    #[test]
    fn peer_agents_are_validated() {
        let a = |id: &str, name: &str| krypto_protocol::Agent {
            id: id.into(),
            machine_id: "zyxwvu987654".into(),
            name: name.into(),
            command: vec!["x".into()],
            env: None,
            version: None,
            description: None,
            icon: None,
            installed: true,
            source: krypto_protocol::AgentSource::Registry,
            capabilities: None,
        };
        let got = validate_peer_agents(vec![
            a("codex", "Codex"),
            a("claude-code", "Claude Code"),
            a("opencode.v2_x", "OpenCode"),
            a("", "Empty id"),
            a("bad id", "Space in id"),
            a("../x", "Slash in id"),
            a(&"x".repeat(65), "Long id"),
            a("evil", "Name\u{1b}[2J"),
            a("long", &"n".repeat(81)),
            a("blank", "   "),
        ]);
        assert_eq!(got.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec!["codex", "claude-code", "opencode.v2_x"]);
        let many: Vec<_> = (0..100).map(|i| a(&format!("a{i}"), "A")).collect();
        assert_eq!(validate_peer_agents(many).len(), PEER_AGENTS_MAX);
    }

    #[test]
    fn plain_paths_and_containment() {
        if cfg!(windows) {
            let w = |x: &str| plain_path(Path::new(x)).map(|v| s(&v));
            assert_eq!(w(r"\\?\C:\Users\me\repo").as_deref(), Some(r"C:\Users\me\repo"));
            assert_eq!(w(r"\\?\UNC\server\share"), None);
            assert_eq!(w(r"\\server\share\x"), None);
            assert_eq!(w(r"\\.\PhysicalDrive0"), None);
            assert!(is_within(Path::new(r"c:\USERS\me\repo\x"), Path::new(r"C:\Users\me\repo")));
        } else {
            assert!(is_within(Path::new("/home/me/repo/x"), Path::new("/home/me/repo")));
            assert!(!is_within(Path::new("/home/me/REPO"), Path::new("/home/me/repo")));
        }
        assert!(!is_within(Path::new("/home/me/repo2"), Path::new("/home/me/repo")));
    }
}
