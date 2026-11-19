use super::Ctx;
use crate::{data, formats::Resolved, Error, ErrorCode, Result};
use std::path::PathBuf;

pub async fn run(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let (src, tgt) = (conv.source.extension.clone(), conv.target.id.clone());
    if matches!(src.as_str(), ".parquet" | ".orc" | ".feather") || matches!(tgt.as_str(), "parquet" | "orc" | "feather") {
        return Err(Error::new(ErrorCode::EUnsupported, "Columnar formats (Parquet/ORC/Feather) are not available in this build yet."));
    }
    let input = ctx.input.clone();
    let out = ctx.out_dir.join(format!("converted{}", conv.target.extension));
    let out2 = out.clone();
    tokio::task::spawn_blocking(move || {
        let v = data::parse_source(&src, &input)?;
        data::serialize_target(&tgt, &v, &out2)
    }).await.map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))??;
    Ok(vec![out])
}
