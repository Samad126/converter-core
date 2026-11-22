//! Conversions that run in pure Rust, with no external program.
//!
//! Two roles: pairs only this module can do (`Engine::Native`), and a fallback for pairs an engine would normally
//! do (LibreOffice, pandoc, ...) when that engine is not installed. The engine wins when it is installed, because
//! it is the higher-fidelity route.
pub mod model;
pub mod read;
pub mod util;
pub mod write;

use crate::{formats::Resolved, pipelines::Ctx, Error, ErrorCode, Result};
use model::Content;
use std::path::{Path, PathBuf};

const WORD: &[&str] = &[".docx", ".docm", ".dotx"];
const SHEET: &[&str] = &[".xlsx", ".xlsm", ".xls", ".ods"];

/// `(source extensions, target ids)` this module can convert. A source never lists itself as a target.
const PAIRS: &[(&[&str], &[&str])] = &[
    (WORD, &["txt", "html", "markdown", "epub"]),
    (SHEET, &["csv", "tsv", "json", "html", "xlsx", "ods"]),
];

pub fn supports(ext: &str, target: &str) -> bool {
    let same = |t: &str| format!(".{t}") == ext;
    PAIRS.iter().any(|(exts, targets)| exts.contains(&ext) && targets.contains(&target) && !same(target))
}

/// Every pair `supports` accepts (used to extend the format matrix).
pub fn all_pairs() -> Vec<(&'static str, &'static str)> {
    PAIRS.iter().flat_map(|(exts, targets)| exts.iter().flat_map(move |e| targets.iter().map(move |t| (*e, *t))))
        .filter(|(e, t)| supports(e, t)).collect()
}

fn read(ctx: &Ctx<'_>, ext: &str) -> Result<Content> {
    if WORD.contains(&ext) {
        let f = std::fs::File::open(&ctx.input)?;
        return read::docx::read(std::io::BufReader::new(f), ctx.cfg).map(Content::Doc);
    }
    if SHEET.contains(&ext) { return read::sheet::read(&ctx.input, ctx.cfg).map(Content::Book); }
    Err(Error::new(ErrorCode::EUnsupported, format!("No built-in reader for \"{ext}\".")))
}

fn write(content: Content, target: &str, out: &Path) -> Result<()> {
    use write::*;
    match target {
        "txt" => std::fs::write(out, text::to_txt(&content.into_doc()))?,
        "markdown" => std::fs::write(out, text::to_markdown(&content.into_doc()))?,
        "html" => std::fs::write(out, html::to_html(&content.into_doc(), "Document"))?,
        "epub" => epub::to_epub(&content.into_doc(), "Document", out)?,
        "csv" => sheet::delimited(&content.into_book(), ',', out)?,
        "tsv" => sheet::delimited(&content.into_book(), '\t', out)?,
        "json" => sheet::json(&content.into_book(), out)?,
        "xlsx" => sheet::xlsx(&content.into_book(), out)?,
        "ods" => sheet::ods(&content.into_book(), out)?,
        other => return Err(Error::new(ErrorCode::EUnsupported, format!("No built-in writer for \"{other}\"."))),
    }
    Ok(())
}

pub async fn run(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let (ext, target) = (conv.source.extension.clone(), conv.target.clone());
    if !supports(&ext, &target.id) {
        return Err(Error::new(ErrorCode::EUnsupported, format!("{ext} cannot become {} without an external engine.", target.id)));
    }
    let out = ctx.out_dir.join(format!("converted{}", target.extension));
    let (cfg_ctx_input, out2) = (ctx.input.clone(), out.clone());
    let cfg = ctx.cfg.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let c = Ctx { cfg: &cfg, workspace: PathBuf::new(), input: cfg_ctx_input, out_dir: PathBuf::new(), profile_dir: PathBuf::new(), timeout: Default::default(), ocr: false };
        write(read(&c, &ext)?, &target.id, &out2)
    }).await.map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))??;
    Ok(vec![out])
}
