//! Subprocess runner. Port of `runProcess` in `backend/src/engines/soffice.engine.ts`.
//! Cancellation = dropping the future: `kill_on_drop` plus a process group on
//! unix so soffice's child `soffice.bin` dies with it.
use crate::{Error, ErrorCode, Result};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

#[derive(Debug)]
pub struct Outcome {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl Outcome {
    pub fn ok(&self) -> bool { self.code == Some(0) }

    /// Fails with `E_CONVERT_FAILED` unless the tool exited 0.
    pub fn require_ok(self, label: &str) -> Result<Self> {
        if self.ok() { return Ok(self); }
        Err(Error::new(
            ErrorCode::EConvertFailed,
            format!("{label} failed (exit {:?}): {}", self.code, self.stderr.trim()),
        ))
    }
}

fn clip(mut s: String, max: usize) -> String {
    if s.len() > max { s.truncate(max); }
    s
}

pub async fn run(bin: &Path, args: &[String], workspace: &Path, timeout: Duration) -> Result<Outcome> {
    run_env(bin, args, workspace, timeout, &[]).await
}

/// Like `run`, with extra environment variables (e.g. a real `HOME` for pip's user site).
pub async fn run_env(bin: &Path, args: &[String], workspace: &Path, timeout: Duration, env: &[(&str, String)]) -> Result<Outcome> {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env("HOME", workspace)
        .env("TMPDIR", workspace)
        .env("LANG", "C.UTF-8")
        .env("LC_ALL", "C.UTF-8")
        .env("JAVA_TOOL_OPTIONS", "");
    for (k, v) in env { cmd.env(k, v); }
    #[cfg(target_os = "linux")]
    cmd.env("SAL_USE_VCLPLUGIN", "svp");
    #[cfg(unix)]
    cmd.process_group(0);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    let child = cmd.spawn().map_err(|e| {
        Error::new(ErrorCode::EConvertFailed, format!("Could not start {}: {e}", bin.display()))
    })?;
    #[cfg(unix)]
    let pgid = child.id();

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(o)) => Ok(Outcome {
            code: o.status.code(),
            stdout: clip(String::from_utf8_lossy(&o.stdout).into_owned(), 4 << 20),
            stderr: clip(String::from_utf8_lossy(&o.stderr).into_owned(), 8192),
        }),
        Ok(Err(e)) => Err(Error::new(ErrorCode::EConvertFailed, format!("Process error: {e}"))),
        Err(_) => {
            // Future dropped by timeout kills the direct child; take the group too.
            #[cfg(unix)]
            if let Some(pid) = pgid { unsafe { libc_kill(-(pid as i32)); } }
            Err(Error::new(ErrorCode::ETimeout, "The conversion took too long and was stopped."))
        }
    }
}

#[cfg(unix)]
unsafe fn libc_kill(pid: i32) {
    extern "C" { fn kill(pid: i32, sig: i32) -> i32; }
    kill(pid, 9);
}
