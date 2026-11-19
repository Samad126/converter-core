//! OCR, compare and redact: standalone PDF operations implemented by `pdf_engine.py`.
use crate::pipelines::python::run_script;
use crate::{workspace::Workspace, Config, Error, ErrorCode, Result};
use serde::{Deserialize, Serialize};

/// A region to strip, in points, top-left origin, 1-based page (same contract as `/pdf/redact`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Area { pub page: usize, pub x: f64, pub y: f64, pub width: f64, pub height: f64 }

fn s(p: &std::path::Path) -> String { p.to_string_lossy().into_owned() }

async fn stage(ws: &Workspace, name: &str, bytes: &[u8]) -> Result<std::path::PathBuf> {
    if bytes.is_empty() { return Err(Error::new(ErrorCode::EBadRequest, "PDF input is empty.")); }
    let p = ws.path().join(name);
    tokio::fs::write(&p, bytes).await?;
    Ok(p)
}

async fn read_output(p: &std::path::Path, label: &str, o: crate::process::Outcome) -> Result<Vec<u8>> {
    let o = o.require_ok(label)?;
    let _ = o;
    let data = tokio::fs::read(p).await.unwrap_or_default();
    if data.is_empty() { return Err(Error::new(ErrorCode::EConvertFailed, format!("{label} produced no output."))); }
    Ok(data)
}

/// Makes a scanned PDF searchable. `force` re-runs OCR even when a text layer exists.
pub async fn ocr(cfg: &Config, bytes: &[u8], force: bool) -> Result<Vec<u8>> {
    let ws = Workspace::create(cfg).await?;
    let (inp, out) = (stage(&ws, "input.pdf", bytes).await?, ws.path().join("output.pdf"));
    let o = run_script(cfg, "pdf_engine.py", vec!["ocr".into(), s(&inp), s(&out), force.to_string()], ws.path(), cfg.media_timeout).await?;
    read_output(&out, "pdf_engine.py ocr", o).await
}

/// Per-page text diff of two PDFs (JSON report, 1-based pages).
pub async fn compare(cfg: &Config, a: &[u8], b: &[u8]) -> Result<serde_json::Value> {
    let ws = Workspace::create(cfg).await?;
    let (pa, pb) = (stage(&ws, "a.pdf", a).await?, stage(&ws, "b.pdf", b).await?);
    let out = ws.path().join("compare.json");
    let o = run_script(cfg, "pdf_engine.py", vec!["compare".into(), s(&pa), s(&pb), s(&out)], ws.path(), cfg.convert_timeout).await?;
    let data = read_output(&out, "pdf_engine.py compare", o).await?;
    serde_json::from_slice(&data).map_err(|e| Error::new(ErrorCode::EConvertFailed, format!("unreadable compare report: {e}")))
}

/// Genuinely removes the content under each area (not a black box on top).
pub async fn redact(cfg: &Config, bytes: &[u8], areas: &[Area]) -> Result<Vec<u8>> {
    if areas.is_empty() { return Err(Error::new(ErrorCode::EInvalidField, "Give at least one area to redact.")); }
    let ws = Workspace::create(cfg).await?;
    let inp = stage(&ws, "input.pdf", bytes).await?;
    let areas_path = ws.path().join("redact-areas.json");
    tokio::fs::write(&areas_path, serde_json::to_vec(areas).unwrap()).await?;
    let out = ws.path().join("output.pdf");
    let o = run_script(cfg, "pdf_engine.py", vec!["redact".into(), s(&inp), s(&areas_path), s(&out)], ws.path(), cfg.convert_timeout).await?;
    read_output(&out, "pdf_engine.py redact", o).await
}
