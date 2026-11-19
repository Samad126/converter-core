use super::Ctx;
use crate::{email, formats::Resolved, Error, ErrorCode, Result};
use std::path::PathBuf;

pub async fn run(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let bytes = tokio::fs::read(&ctx.input).await?;
    let text = if conv.target.id == "html" { email::render_html(&bytes) } else { email::render_text(&bytes) }
        .map_err(|e| Error::new(ErrorCode::EConvertFailed, e.message))?;
    let out = ctx.out_dir.join(format!("converted{}", conv.target.extension));
    tokio::fs::write(&out, text).await?;
    Ok(vec![out])
}
