//! Per-job temp directories with a stale sweep.
use crate::{config::Config, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

pub struct Workspace(pub PathBuf);

impl Workspace {
    pub async fn create(cfg: &Config) -> Result<Self> {
        // A process-wide counter makes the name unique even when two jobs start within one clock tick
        // (macOS timestamps are only microsecond-accurate); `create_dir` (not `_all`) fails on a clash, so we retry rather than share a folder.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        tokio::fs::create_dir_all(&cfg.temp_root).await?;
        loop {
            let name = format!("{}-{}-{}", std::process::id(), SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0), NEXT.fetch_add(1, Ordering::Relaxed));
            let dir = cfg.temp_root.join(name);
            match tokio::fs::create_dir(&dir).await {
                Ok(()) => return Ok(Self(dir)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }
    pub fn path(&self) -> &std::path::Path { &self.0 }
}

impl Drop for Workspace {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

/// Removes children of `temp_root` older than `stale_workspace` (crash leftovers).
pub async fn sweep_stale(cfg: &Config) -> usize {
    let Ok(mut rd) = tokio::fs::read_dir(&cfg.temp_root).await else { return 0 };
    let mut n = 0;
    while let Ok(Some(e)) = rd.next_entry().await {
        let old = e.metadata().await.ok().and_then(|m| m.modified().ok())
            .and_then(|t| t.elapsed().ok()).is_some_and(|age| age > cfg.stale_workspace);
        if old && tokio::fs::remove_dir_all(e.path()).await.is_ok() { n += 1; }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn concurrent_workspaces_never_share_a_folder() {
        let mut cfg = Config::default();
        cfg.temp_root = std::env::temp_dir().join(format!("ws-unique-{}", std::process::id()));
        let all = futures_unordered(&cfg).await;
        let mut paths: Vec<_> = all.iter().map(|w| w.path().to_path_buf()).collect();
        paths.sort(); paths.dedup();
        assert_eq!(paths.len(), 64);
        drop(all);
        let _ = std::fs::remove_dir_all(&cfg.temp_root);
    }
    async fn futures_unordered(cfg: &Config) -> Vec<Workspace> {
        let hs: Vec<_> = (0..64).map(|_| { let c = cfg.clone(); tokio::spawn(async move { Workspace::create(&c).await.unwrap() }) }).collect();
        let mut v = Vec::new(); for h in hs { v.push(h.await.unwrap()); } v
    }
}
