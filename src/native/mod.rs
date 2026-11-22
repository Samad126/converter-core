//! Conversions that run in pure Rust, with no external program.
//!
//! Two roles: pairs only this module can do (`Engine::Native`), and a fallback for pairs an engine would normally
//! do (LibreOffice, pandoc, ...) when that engine is not installed. The engine wins when it is installed, because
//! it is the higher-fidelity route.
pub mod archive;
pub mod font;
pub mod fonts;
pub mod layout;
pub mod model;
pub mod render;
pub mod subs;
pub mod read;
pub mod util;
pub mod write;

use crate::{formats::Resolved, pipelines::Ctx, Error, ErrorCode, Result};
use model::Content;
use std::path::{Path, PathBuf};

const WORD: &[&str] = &[".docx", ".docm", ".dotx", ".dotm"];
const SLIDES: &[&str] = &[".pptx", ".pptm", ".ppsx", ".potx"];
const ODT: &[&str] = &[".odt", ".ott"];
const FODT: &[&str] = &[".fodt"];
const HTML: &[&str] = &[".html", ".htm"];
const SUBS: &[&str] = subs::EXTENSIONS;
const DATA: &[&str] = &[".ndjson", ".avro"];
const FONTS: &[&str] = &[".ttf", ".otf", ".woff", ".eot"];
const ARCHIVES: &[&str] = &[".zip", ".cbz", ".tar", ".tgz", ".gz", ".xz", ".txz", ".lzma", ".lz4", ".zipx"];
const HWP: &[&str] = &[".hwp"];
const HWPX: &[&str] = &[".hwpx"];
const MHT: &[&str] = &[".mht", ".mhtml"];
const SHEET: &[&str] = &[".xlsx", ".xlsm", ".xls", ".ods", ".xlsb", ".xltx", ".xlt", ".ots"];

/// `(source extensions, target ids)` this module can convert. A source never lists itself as a target.
const PAIRS: &[(&[&str], &[&str])] = &[
    (WORD, &["txt", "html", "markdown", "epub", "pdf", "png", "jpg", "hwpx"]),
    (SLIDES, &["pdf", "txt", "html", "markdown", "png", "jpg", "hwpx"]),
    (&[".pdf"], &["png", "jpg", "tiff", "cbz", "pptx", "txt", "markdown", "html", "docx", "xlsx", "hwpx"]),
    (SHEET, &["csv", "tsv", "json", "html", "xlsx", "ods", "pdf", "png", "jpg", "hwpx"]),
    (SUBS, &["srt", "vtt", "sbv", "lrc", "ass", "ssa"]),
    (DATA, &["csv", "tsv", "json", "jsonl", "yaml", "xml", "toml", "ini", "sqlite"]),
    (&[".ttf", ".otf"], &["woff"]),
    (&[".woff"], &["ttf", "otf"]),
    (&[".eot"], &["ttf", "otf", "woff"]),
    (ARCHIVES, &["zip", "tar", "tar.gz", "cbz"]),
    (HWP, &["pdf", "txt", "html", "markdown", "epub", "docx", "hwpx", "xlsx", "png", "jpg"]),
    (HWPX, &["pdf", "txt", "html", "markdown", "epub", "docx", "xlsx", "png", "jpg"]),
    (FODT, &["pdf", "txt", "html", "markdown", "epub", "docx", "hwpx", "png", "jpg"]),
    (ODT, &["pdf", "txt", "html", "markdown", "epub", "docx", "hwpx", "png", "jpg"]),
    (HTML, &["pdf", "txt", "markdown", "epub", "docx", "hwpx", "png", "jpg"]),
    (MHT, &["pdf", "txt", "html", "markdown", "epub", "docx", "hwpx", "xlsx", "png", "jpg"]),
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
    if ODT.contains(&ext) {
        let f = std::fs::File::open(&ctx.input)?;
        return read::odt::read(std::io::BufReader::new(f), ctx.cfg).map(Content::Doc);
    }
    if HTML.contains(&ext) {
        let bytes = std::fs::read(&ctx.input)?;
        let text = String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
        return Ok(Content::Doc(read::html::parse(&text, &|_| None)));
    }
    if MHT.contains(&ext) { return read::mht::read(&std::fs::read(&ctx.input)?).map(Content::Doc); }
    if HWP.contains(&ext) {
        let f = std::fs::File::open(&ctx.input)?;
        return read::hwp::read(std::io::BufReader::new(f), ctx.cfg).map(Content::Doc);
    }
    if HWPX.contains(&ext) {
        let f = std::fs::File::open(&ctx.input)?;
        return read::hwpx::read(std::io::BufReader::new(f), ctx.cfg).map(Content::Doc);
    }
    if ext == ".pdf" { return read::pdf_text::read(std::fs::read(&ctx.input)?, ctx.cfg.max_raster_pages).map(Content::Doc); }
    if FODT.contains(&ext) { return read::odt::read_flat(&std::fs::read(&ctx.input)?, ctx.cfg).map(Content::Doc); }
    if SHEET.contains(&ext) {
        // calamine picks its reader by file extension; templates are the same formats under another name.
        let as_ext = match ext { ".xltx" => Some("xlsx"), ".xlt" => Some("xls"), ".ots" => Some("ods"), _ => None };
        let path = match as_ext { Some(e) => { let p = ctx.input.with_extension(e); std::fs::copy(&ctx.input, &p)?; p } None => ctx.input.clone() };
        return read::sheet::read(&path, ctx.cfg).map(Content::Book);
    }
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

/// The spreadsheet view of a result; a document with no tables has nothing to put in one.
fn book_of(content: Content) -> Result<model::Workbook> {
    if let Content::Doc(d) = &content {
        if d.tables().is_empty() { return Err(Error::new(ErrorCode::ENoTables, "This document does not contain any tables.")); }
    }
    Ok(content.into_book())
}

fn write(content: Content, target: &str, out: &Path) -> Result<()> {
    use write::*;
    match target {
        "txt" => std::fs::write(out, text::to_txt(&content.into_doc()))?,
        "markdown" => std::fs::write(out, text::to_markdown(&content.into_doc()))?,
        "html" => std::fs::write(out, html::to_html(&content.into_doc(), "Document"))?,
        "pdf" => std::fs::write(out, pdf_bytes(content)?)?,
        "docx" => docx::write(&content.into_doc(), out)?,
        "hwpx" => hwpx::write(&content.into_doc(), "Document", out)?,
        "epub" => epub::to_epub(&content.into_doc(), "Document", out)?,
        "csv" => sheet::delimited(&book_of(content)?, ',', out)?,
        "tsv" => sheet::delimited(&book_of(content)?, '\t', out)?,
        "json" => sheet::json(&book_of(content)?, out)?,
        "xlsx" => sheet::xlsx(&book_of(content)?, out)?,
        "ods" => sheet::ods(&book_of(content)?, out)?,
        other => return Err(Error::new(ErrorCode::EUnsupported, format!("No built-in writer for \"{other}\"."))),
    }
    Ok(())
}

pub async fn run(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let (ext, target) = (conv.source.extension.clone(), conv.target.clone());
    if !supports(&ext, &target.id) {
        return Err(Error::new(ErrorCode::EUnsupported, format!("{ext} cannot become {} without an external engine.", target.id)));
    }
    if SUBS.contains(&ext.as_str()) {
        let (input, out) = (ctx.input.clone(), ctx.out_dir.join(format!("converted{}", target.extension)));
        let id = target.id.clone();
        let out2 = out.clone();
        tokio::task::spawn_blocking(move || -> Result<()> { let cues = subs::parse(&ext, &std::fs::read(&input)?)?; std::fs::write(&out2, subs::write(&id, &cues)?)?; Ok(()) })
            .await.map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))??;
        return Ok(vec![out]);
    }
    if ARCHIVES.contains(&ext.as_str()) {
        let (cfg, input, name) = (ctx.cfg.clone(), ctx.input.clone(), ctx.name.clone());
        let (out, id) = (ctx.out_dir.join(format!("converted{}", target.extension)), target.id.clone());
        let out2 = out.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            // A single compressed stream keeps its name without the compression suffix ("notes.txt.gz" -> "notes.txt").
            let stem = name.rsplit_once('.').map_or(name.as_str(), |(s, _)| s);
            let stem = if stem.is_empty() { "file" } else { stem };
            let un = archive::unpack(&ext, stem, &std::fs::read(&input)?, &cfg)?;
            archive::pack(&id, &un.files, &out2)
        }).await.map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))??;
        return Ok(vec![out]);
    }
    if FONTS.contains(&ext.as_str()) {
        let (input, out) = (ctx.input.clone(), ctx.out_dir.join(format!("converted{}", target.extension)));
        let id = target.id.clone();
        let out2 = out.clone();
        tokio::task::spawn_blocking(move || -> Result<()> { std::fs::write(&out2, font::convert(&ext, &id, &std::fs::read(&input)?)?)?; Ok(()) })
            .await.map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))??;
        return Ok(vec![out]);
    }
    if DATA.contains(&ext.as_str()) {
        let (input, out) = (ctx.input.clone(), ctx.out_dir.join(format!("converted{}", target.extension)));
        let id = target.id.clone();
        let out2 = out.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let value = if ext == ".avro" { read::avro::read(&std::fs::read(&input)?)? } else { crate::data::parse_source(".jsonl", &input)? };
            crate::data::serialize_target(&id, &value, &out2)
        }).await.map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))??;
        return Ok(vec![out]);
    }
    if ext == ".pdf" && is_page_target(&target.id) || matches!(target.id.as_str(), "png" | "jpg") {
        let (cfg, input, out_dir, id) = (ctx.cfg.clone(), ctx.input.clone(), ctx.out_dir.clone(), target.id.clone());
        return tokio::task::spawn_blocking(move || -> Result<Vec<PathBuf>> {
            let pdf = if ext == ".pdf" { std::fs::read(&input)? } else {
                let c = Ctx { cfg: &cfg, name: String::new(), workspace: PathBuf::new(), input, out_dir: PathBuf::new(), profile_dir: PathBuf::new(), timeout: Default::default(), ocr: false };
                pdf_bytes(read(&c, &ext)?)?
            };
            raster_pdf(&cfg, pdf, &id, &out_dir)
        }).await.map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))?;
    }
    let out = ctx.out_dir.join(format!("converted{}", target.extension));
    let (cfg_ctx_input, out2) = (ctx.input.clone(), out.clone());
    let cfg = ctx.cfg.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let c = Ctx { cfg: &cfg, name: String::new(), workspace: PathBuf::new(), input: cfg_ctx_input, out_dir: PathBuf::new(), profile_dir: PathBuf::new(), timeout: Default::default(), ocr: false };
        write(read(&c, &ext)?, &target.id, &out2)
    }).await.map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))??;
    Ok(vec![out])
}
