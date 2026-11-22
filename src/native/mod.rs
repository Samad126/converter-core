//! Conversions that run in pure Rust, with no external program.
//!
//! Two roles: pairs only this module can do (`Engine::Native`), and a fallback for pairs an engine would normally
//! do (LibreOffice, pandoc, ...) when that engine is not installed. The engine wins when it is installed, because
//! it is the higher-fidelity route.
pub mod fonts;
pub mod layout;
pub mod model;
pub mod render;
pub mod read;
pub mod util;
pub mod write;

use crate::{formats::Resolved, pipelines::Ctx, Error, ErrorCode, Result};
use model::Content;
use std::path::{Path, PathBuf};

const WORD: &[&str] = &[".docx", ".docm", ".dotx"];
const SLIDES: &[&str] = &[".pptx", ".pptm", ".ppsx", ".potx"];
const SHEET: &[&str] = &[".xlsx", ".xlsm", ".xls", ".ods"];

/// `(source extensions, target ids)` this module can convert. A source never lists itself as a target.
const PAIRS: &[(&[&str], &[&str])] = &[
    (WORD, &["txt", "html", "markdown", "epub", "pdf", "png", "jpg"]),
    (SLIDES, &["pdf", "txt", "html", "markdown", "png", "jpg"]),
    (&[".pdf"], &["png", "jpg", "tiff", "cbz", "pptx"]),
    (SHEET, &["csv", "tsv", "json", "html", "xlsx", "ods", "pdf", "png", "jpg"]),
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
    if SLIDES.contains(&ext) {
        let f = std::fs::File::open(&ctx.input)?;
        return read::pptx::read(std::io::BufReader::new(f), ctx.cfg).map(Content::Deck);
    }
    if SHEET.contains(&ext) { return read::sheet::read(&ctx.input, ctx.cfg).map(Content::Book); }
    Err(Error::new(ErrorCode::EUnsupported, format!("No built-in reader for \"{ext}\".")))
}

fn pdf_bytes(content: Content) -> Result<Vec<u8>> {
    match content {
        Content::Doc(d) => write::pdf::doc_to_pdf(&d, "Document"),
        Content::Book(b) => write::pdf::book_to_pdf(&b, "Workbook"),
        Content::Deck(k) => write::pdf::deck_to_pdf(&k, "Presentation"),
    }
}

/// Rasterises PDF bytes with the built-in renderer into files in `out_dir`.
/// png/jpg/tiff: one file per page (`page-001.png`, ...); cbz and pptx: a single package.
pub fn raster_pdf(cfg: &crate::Config, pdf: Vec<u8>, target_id: &str, out_dir: &Path) -> Result<Vec<PathBuf>> {
    use std::io::Write;
    let pages = render::render_pdf(pdf, cfg.raster_dpi as f32, cfg.max_raster_pages)?;
    let q = cfg.raster_jpeg_quality as u8;
    let zerr = |e: zip::result::ZipError| Error::new(ErrorCode::EInternal, e.to_string());
    match target_id {
        "png" | "jpg" | "tiff" => {
            let ext = if target_id == "jpg" { "jpg".to_string() } else { target_id.to_string() };
            pages.iter().enumerate().map(|(i, p)| {
                let path = out_dir.join(format!("page-{:03}.{ext}", i + 1));
                std::fs::write(&path, render::encode(&p.image, target_id, q)?)?;
                Ok(path)
            }).collect()
        }
        "cbz" => {
            let path = out_dir.join("converted.cbz");
            let mut z = zip::ZipWriter::new(std::fs::File::create(&path)?);
            let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for (i, p) in pages.iter().enumerate() {
                z.start_file(format!("{:03}.jpg", i + 1), opt).map_err(zerr)?;
                z.write_all(&render::encode(&p.image, "jpg", q)?)?;
            }
            z.finish().map_err(zerr)?;
            Ok(vec![path])
        }
        "pptx" => {
            let path = out_dir.join("converted.pptx");
            let slides = pages.iter().map(|p| Ok(write::pptx::SlideImage { width_pt: p.width_pt, height_pt: p.height_pt, png: render::encode(&p.image, "png", q)? })).collect::<Result<Vec<_>>>()?;
            write::pptx::write(&slides, &path)?;
            Ok(vec![path])
        }
        other => Err(Error::new(ErrorCode::EUnsupported, format!("No built-in page renderer output for \"{other}\"."))),
    }
}

fn is_page_target(id: &str) -> bool { matches!(id, "png" | "jpg" | "tiff" | "cbz" | "pptx") }

fn write(content: Content, target: &str, out: &Path) -> Result<()> {
    use write::*;
    match target {
        "txt" => std::fs::write(out, text::to_txt(&content.into_doc()))?,
        "markdown" => std::fs::write(out, text::to_markdown(&content.into_doc()))?,
        "html" => std::fs::write(out, html::to_html(&content.into_doc(), "Document"))?,
        "pdf" => std::fs::write(out, pdf_bytes(content)?)?,
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
    if ext == ".pdf" && is_page_target(&target.id) || matches!(target.id.as_str(), "png" | "jpg") {
        let (cfg, input, out_dir, id) = (ctx.cfg.clone(), ctx.input.clone(), ctx.out_dir.clone(), target.id.clone());
        return tokio::task::spawn_blocking(move || -> Result<Vec<PathBuf>> {
            let pdf = if ext == ".pdf" { std::fs::read(&input)? } else {
                let c = Ctx { cfg: &cfg, workspace: PathBuf::new(), input, out_dir: PathBuf::new(), profile_dir: PathBuf::new(), timeout: Default::default(), ocr: false };
                pdf_bytes(read(&c, &ext)?)?
            };
            raster_pdf(&cfg, pdf, &id, &out_dir)
        }).await.map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))?;
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
