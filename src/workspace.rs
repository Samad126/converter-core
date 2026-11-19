//! Per-job temp directories with a stale sweep.
use crate::{config::Config, Result};
use std::path::PathBuf;
use std::time::SystemTime;

pub struct Workspace(pub PathBuf);

impl Workspace {
    pub async fn create(cfg: &Config) -> Result<Self> {
        let name = format!("{}-{}", std::process::id(), SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        let dir = cfg.temp_root.join(name);
        tokio::fs::create_dir_all(&dir).await?;
        Ok(Self(dir))
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
