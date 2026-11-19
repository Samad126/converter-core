//! protect / unlock / repair / compress through `qpdf` (pdf-lib-style libraries cannot encrypt).
use crate::{engines, process, workspace::Workspace, Config, Error, ErrorCode, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressLevel { Low, Medium, High }

pub fn compress_args(level: CompressLevel) -> Vec<&'static str> {
    let base = ["--compress-streams=y", "--object-streams=generate"];
    let image = |w: &'static str, h: &'static str, a: &'static str| vec!["--recompress-flate", "--compression-level=9", "--optimize-images", w, h, a];
    let mut v = base.to_vec();
    match level {
        CompressLevel::Low => {}
        CompressLevel::Medium => v.extend(image("--oi-min-width=200", "--oi-min-height=200", "--oi-min-area=40000")),
        CompressLevel::High => v.extend(image("--oi-min-width=0", "--oi-min-height=0", "--oi-min-area=0")),
    }
    v
}

#[derive(Debug, Clone)]
pub enum Op { Protect(String), Unlock(String), Repair, Compress(CompressLevel) }

pub fn args_for(op: &Op, input: &str, output: &str) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    match op {
        Op::Protect(p) => vec!["--encrypt".into(), p.clone(), p.clone(), "256".into(), "--".into(), input.into(), output.into()],
        Op::Unlock(p) => vec![format!("--password={p}"), "--decrypt".into(), "--".into(), input.into(), output.into()],
        Op::Repair => vec![input.into(), output.into()],
        Op::Compress(l) => { let mut a = s(&compress_args(*l)); a.extend(["--".into(), input.into(), output.into()]); a }
    }
}

pub async fn run(cfg: &Config, op: Op, bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.is_empty() { return Err(Error::new(ErrorCode::EBadRequest, "PDF input is empty.")); }
    if matches!(&op, Op::Protect(p) | Op::Unlock(p) if p.is_empty()) {
        return Err(Error::new(ErrorCode::EBadRequest, "password is required."));
    }
    let bin = engines::resolve(engines::tool("qpdf").unwrap(), cfg, None).ok_or_else(|| Error::engine_missing("qpdf"))?;
    let ws = Workspace::create(cfg).await?;
    let (inp, out) = (ws.path().join("input.pdf"), ws.path().join("output.pdf"));
    tokio::fs::write(&inp, bytes).await?;
    let args = args_for(&op, &inp.to_string_lossy(), &out.to_string_lossy());
    let o = process::run(&bin, &args, ws.path(), cfg.media_timeout).await?;
    // qpdf exits 3 for "succeeded with warnings", which is the normal outcome of a repair.
    let ok = o.code == Some(0) || (matches!(op, Op::Repair) && o.code == Some(3));
    if !ok {
        if matches!(op, Op::Unlock(_)) && o.stderr.to_lowercase().contains("invalid password") {
            return Err(Error::new(ErrorCode::EWrongPassword, "That password does not unlock this PDF."));
        }
        return Err(Error::new(ErrorCode::EConvertFailed, format!("qpdf failed (exit {:?}): {}", o.code, o.stderr.trim())));
    }
    let data = tokio::fs::read(&out).await?;
    if data.is_empty() { return Err(Error::new(ErrorCode::EConvertFailed, "qpdf produced an empty file")); }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn protect_and_unlock_args() {
        assert_eq!(args_for(&Op::Protect("pw".into()), "i", "o"), ["--encrypt", "pw", "pw", "256", "--", "i", "o"]);
        assert_eq!(args_for(&Op::Unlock("pw".into()), "i", "o"), ["--password=pw", "--decrypt", "--", "i", "o"]);
    }
    #[test] fn compress_levels_escalate() {
        assert_eq!(compress_args(CompressLevel::Low).len(), 2);
        assert!(compress_args(CompressLevel::Medium).contains(&"--oi-min-width=200"));
        assert!(compress_args(CompressLevel::High).contains(&"--oi-min-area=0"));
    }
    #[test] fn password_starting_with_dash_stays_a_value() {
        // passed as `--password=<pw>` / positional-after-`--`-free slot, never as its own option
        assert!(args_for(&Op::Unlock("-x".into()), "i", "o")[0].starts_with("--password="));
    }
}
