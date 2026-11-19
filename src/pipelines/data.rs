use super::{python, Ctx};
use crate::{data, formats::Resolved, Error, ErrorCode, Result};
use serde_json::Value;
use std::path::PathBuf;

const ARROW_EXTS: [&str; 3] = [".parquet", ".orc", ".feather"];
const ARROW_TARGETS: [&str; 3] = ["parquet", "orc", "feather"];

fn join_err(e: tokio::task::JoinError) -> Error { Error::new(ErrorCode::EInternal, e.to_string()) }

/// Columnar files have no pure-Rust reader here: `arrow_engine.py` (pyarrow) turns them into a JSON array of rows.
async fn arrow_read(ctx: &Ctx<'_>) -> Result<Value> {
    let json = ctx.workspace.join("arrow-intermediate.json");
    let args = vec!["read".into(), ctx.input.to_string_lossy().into_owned(), json.to_string_lossy().into_owned()];
    python::run_script(ctx.cfg, "arrow_engine.py", args, &ctx.workspace, ctx.timeout).await?.require_ok("arrow_engine.py read")?;
    let text = tokio::fs::read_to_string(&json).await?;
    let mut v: Value = serde_json::from_str(&text).map_err(|e| Error::new(ErrorCode::EConvertFailed, format!("unreadable table: {e}")))?;
    data::normalize_numbers(&mut v);
    Ok(v)
}

async fn arrow_write(ctx: &Ctx<'_>, v: &Value, out: &std::path::Path, label: &str) -> Result<()> {
    let json = ctx.workspace.join("arrow-intermediate.json");
    tokio::fs::write(&json, serde_json::to_vec(v).unwrap()).await?;
    let args = vec!["write".into(), json.to_string_lossy().into_owned(), out.to_string_lossy().into_owned()];
    let o = python::run_script(ctx.cfg, "arrow_engine.py", args, &ctx.workspace, ctx.timeout).await?;
    // The script exits 1 *without a traceback* when the JSON is not a flat list of records.
    if o.code == Some(1) && !o.stderr.contains("Traceback") {
        return Err(Error::new(ErrorCode::ENotTabular, format!("This document is not a flat table of records (a top-level list of flat objects), so it cannot become {}.", label.to_uppercase())));
    }
    o.require_ok("arrow_engine.py write")?;
    Ok(())
}

pub async fn run(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let (src, tgt) = (conv.source.extension.clone(), conv.target.id.clone());
    let out = ctx.out_dir.join(format!("converted{}", conv.target.extension));
    let value = if ARROW_EXTS.contains(&src.as_str()) {
        arrow_read(ctx).await?
    } else {
        let input = ctx.input.clone();
        tokio::task::spawn_blocking(move || data::parse_source(&src, &input)).await.map_err(join_err)??
    };
    if ARROW_TARGETS.contains(&tgt.as_str()) {
        arrow_write(ctx, &value, &out, &tgt).await?;
    } else {
        let out2 = out.clone();
        tokio::task::spawn_blocking(move || data::serialize_target(&tgt, &value, &out2)).await.map_err(join_err)??;
    }
    Ok(vec![out])
}
