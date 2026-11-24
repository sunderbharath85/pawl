//! This hub's repo index and paired hubs' repos (PLAN step 5). See [`crate::repos`] for how the
//! index is built; this keeps it current, shares it in presence, answers `list_repos`, and decides
//! which folders a remote start may use.

use std::path::{Path, PathBuf};

use krypto_protocol::{DeviceKind, RepoRef};

use super::*;
use crate::repos;

/// The index is rebuilt at least this often (folders may have gone away).
const REPO_REBUILD_EVERY: Duration = Duration::from_secs(3600);

impl Hub {
    /// This machine's repo index (newest first).
    pub fn repos(&self) -> Vec<RepoRef> {
        self.inner.repos.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Rebuild the index from this machine's session folders and `[repos] paths`; a changed index
    /// is pushed to paired hubs with the next presence.
    pub(super) async fn rebuild_repo_index(&self) {
        let paths = self.inner.config.repos.paths.clone();
        let built = self
            .inner
            .store
            .run(move |s| {
                let mut c: Vec<(String, Option<UnixMs>)> =
                    s.local_session_folders()?.into_iter().map(|(p, t)| (p, Some(t))).collect();
                c.extend(paths.into_iter().map(|p| (p, None)));
                Ok(repos::build_index(c))
            })
            .await;
        let index = match built {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("build the repo index: {e:#}");
                return;
            }
        };
        let changed = {
            let mut cur = self.inner.repos.lock().unwrap_or_else(|p| p.into_inner());
            let changed = *cur != index;
            *cur = index;
            changed
        };
        if changed {
            self.refresh_presence();
        }
    }

    /// A session of this machine uses `cwd`: rebuild the index the first time a folder shows up.
    pub(super) fn note_session_folder(&self, cwd: &str) {
        if cwd.trim().is_empty() {
            return;
        }
        let new = self.inner.seen_cwds.lock().unwrap_or_else(|p| p.into_inner()).insert(cwd.to_string());
        if new {
            let hub = self.clone();
            tokio::spawn(async move { hub.rebuild_repo_index().await });
        }
    }

    pub(super) async fn repo_index_loop(self) {
        let mut shutdown = self.shutdown_rx();
        let mut iv = tokio::time::interval_at(tokio::time::Instant::now() + REPO_REBUILD_EVERY, REPO_REBUILD_EVERY);
        loop {
            tokio::select! {
                _ = iv.tick() => self.rebuild_repo_index().await,
                _ = shutdown.changed() => break,
            }
        }
    }

    /// The step-4 "known folder" rule, one function: `dir` (canonical) is a folder of the repo index
    /// or inside one, or inside a `[peer] start_roots` entry. A folder the cached index does not
    /// have yet triggers one rebuild before the answer is no.
    pub(super) async fn is_known_folder(&self, dir: &Path) -> bool {
        let roots = self.inner.config.peer.start_roots.clone();
        let roots: Vec<PathBuf> = tokio::task::spawn_blocking(move || {
            roots.iter().filter_map(|r| repos::canonical_dir(Path::new(r.trim()))).collect()
        })
        .await
        .unwrap_or_default();
        let known = |hub: &Hub| {
            hub.repos().iter().any(|r| repos::is_within(dir, Path::new(&r.path))) || roots.iter().any(|r| repos::is_within(dir, r))
        };
        if known(self) {
            return true;
        }
        self.rebuild_repo_index().await;
        known(self)
    }

    /// `list_repos{machineId}`: this machine's index, or a paired hub's (latest presence, else the
    /// last one it reported). Unknown machine: `not_found`.
    pub(super) async fn repos_of(&self, machine_id: Option<&str>) -> Result<Vec<RepoRef>> {
        let Some(m) = machine_id.filter(|m| *m != self.inner.machine_id) else { return Ok(self.repos()) };
        if !self.devices().iter().any(|d| d.id == m && d.kind == DeviceKind::Hub) {
            return Err(AckCode::NotFound.into());
        }
        if let Some(r) = self.presence_repos(m) {
            return Ok(r);
        }
        let id = m.to_string();
        Ok(self.inner.store.run(move |s| s.device_repos(&id)).await?.unwrap_or_default())
    }
}
