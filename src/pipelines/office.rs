use super::{collect_produced, Ctx};
use crate::{formats::{pdf_filter, Resolved}, process, Error, ErrorCode, Result};
use std::path::{Path, PathBuf};

/// `file:///…` URL for `-env:UserInstallation`.
fn file_url(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = if s.starts_with('/') { s } else { format!("/{s}") };
    let enc: String = s.chars().map(|c| match c {
        ' ' => "%20".into(), '#' => "%23".into(), '%' => "%25".into(), '?' => "%3F".into(), c => c.to_string(),
    }).collect();
    format!("file://{enc}")
}

async fn soffice(ctx: &Ctx<'_>, input: &Path, convert_to: &str) -> Result<process::Outcome> {
    let bin = ctx.bin("libreoffice")?;
    tokio::fs::create_dir_all(&ctx.profile_dir).await?;
    let args: Vec<String> = [
        "--headless", "--norestore", "--invisible", "--nolockcheck", "--nodefault", "--nofirststartwizard",
    ].iter().map(|s| s.to_string())
    .chain([
        format!("-env:UserInstallation={}", file_url(&ctx.profile_dir)),
        "--convert-to".into(), convert_to.into(),
        "--outdir".into(), ctx.out_dir.to_string_lossy().into_owned(),
        input.to_string_lossy().into_owned(),
    ]).collect();
    process::run(&bin, &args, &ctx.workspace, ctx.timeout).await
}

pub async fn direct(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let out = soffice(ctx, &ctx.input, &conv.convert_to).await?;
    let files = collect_produced(&ctx.out_dir, &conv.target.extension);
    if files.is_empty() {
        return Err(Error::new(ErrorCode::EConvertFailed, format!(
            "LibreOffice produced no {} file (exit {:?}): {}", conv.target.extension, out.code, out.stderr.trim())));
    }
    Ok(files)
}

/// Source → PDF (soffice) → one image per page (pdftoppm).
pub async fn raster(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let pdf = if conv.source.extension == ".pdf" {
        ctx.input.clone()
    } else {
        let filter = pdf_filter(conv.source.family.as_deref()).ok_or_else(|| {
            Error::new(ErrorCode::EInternal, "no PDF export filter for this source")
        })?;
        let out = soffice(ctx, &ctx.input, &format!("pdf:{filter}")).await?;
        collect_produced(&ctx.out_dir, ".pdf").into_iter().next().ok_or_else(|| {
            Error::new(ErrorCode::EConvertFailed, format!("LibreOffice produced no intermediate PDF: {}", out.stderr.trim()))
        })?
    };

    let raster_dir = ctx.workspace.join("raster");
    tokio::fs::create_dir_all(&raster_dir).await?;
    let png = conv.target.id == "png";
    let mut args = vec![(if png { "-png" } else { "-jpeg" }).to_string(), "-r".into(), ctx.cfg.raster_dpi.to_string()];
    if !png { args.extend(["-jpegopt".into(), format!("quality={}", ctx.cfg.raster_jpeg_quality)]); }
    args.push(pdf.to_string_lossy().into_owned());
    args.push(raster_dir.join("slide").to_string_lossy().into_owned());
    process::run(&ctx.bin("poppler")?, &args, &ctx.workspace, ctx.timeout).await?.require_ok("pdftoppm")?;

    let pages = collect_produced(&raster_dir, &conv.target.extension);
    if pages.is_empty() {
        return Err(Error::new(ErrorCode::EConvertFailed, "The rasteriser produced no images."));
    }
    if pages.len() > ctx.cfg.max_raster_pages {
        return Err(Error::new(ErrorCode::ETooLarge, format!(
            "This document has {} pages; the limit is {}.", pages.len(), ctx.cfg.max_raster_pages)));
    }
    Ok(pages)
}
