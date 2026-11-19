//! Routes that are one external tool, one output file `converted<ext>`.
use super::{collect_produced, Ctx};
use crate::{formats::Resolved, process, Error, ErrorCode, Result};
use std::path::{Path, PathBuf};

fn s(p: &Path) -> String { p.to_string_lossy().into_owned() }

fn output(ctx: &Ctx<'_>, conv: &Resolved) -> PathBuf {
    ctx.out_dir.join(format!("converted{}", conv.target.extension))
}

fn require_output(ctx: &Ctx<'_>, conv: &Resolved, label: &str) -> Result<Vec<PathBuf>> {
    let want = output(ctx, conv);
    if want.metadata().is_ok_and(|m| m.len() > 0) { return Ok(vec![want]); }
    collect_produced(&ctx.out_dir, &conv.target.extension).into_iter().next()
        .map(|p| vec![p])
        .ok_or_else(|| Error::new(ErrorCode::EConvertFailed, format!("{label} produced no {} file.", conv.target.extension)))
}

async fn ffmpeg_still(ctx: &Ctx<'_>, input: &Path, out: &Path) -> Result<()> {
    // `-frames:v 1 -update 1` is mandatory: without it GIF/WEBP sources trip image2's
    // "Cannot write more than one file with the same name".
    let args = ["-y", "-i"].map(String::from).into_iter()
        .chain([s(input), "-frames:v".into(), "1".into(), "-update".into(), "1".into(), s(out)]).collect::<Vec<_>>();
    process::run(&ctx.bin("ffmpeg")?, &args, &ctx.workspace, ctx.timeout).await?.require_ok("ffmpeg")?;
    Ok(())
}

pub async fn ffmpeg(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    ffmpeg_still(ctx, &ctx.input, &output(ctx, conv)).await?;
    require_output(ctx, conv, "ffmpeg")
}

pub async fn pandoc(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let bin = ctx.bin("pandoc")?;
    let id = conv.target.id.as_str();
    if id == "pdf" {
        // markup → docx (pandoc) → pdf (LibreOffice)
        let docx = ctx.workspace.join("markdown-intermediate.docx");
        let args = ["--standalone", "-t", "docx", "-o"].map(String::from).into_iter().chain([s(&docx), s(&ctx.input)]).collect::<Vec<_>>();
        process::run(&bin, &args, &ctx.workspace, ctx.timeout).await?.require_ok("pandoc -t docx")?;
        let soffice = Ctx { input: docx, ..clone_ctx(ctx) };
        let pdf = crate::formats::resolve_conversion(".docx", "pdf").expect("docx->pdf exists");
        return super::office::direct(&soffice, &pdf).await;
    }
    let writer = match id {
        "docx" => "docx", "html" => "html", "odt" => "odt", "rtf" => "rtf", "txt" => "plain", "markdown" => "gfm",
        other => return Err(Error::new(ErrorCode::EConvertFailed, format!("no pandoc writer for \"{other}\""))),
    };
    let out = output(ctx, conv);
    let args = ["--standalone", "-t", writer, "-o"].map(String::from).into_iter().chain([s(&out), s(&ctx.input)]).collect::<Vec<_>>();
    process::run(&bin, &args, &ctx.workspace, ctx.timeout).await?.require_ok(&format!("pandoc -t {writer}"))?;
    require_output(ctx, conv, "pandoc")
}

fn clone_ctx<'a>(c: &Ctx<'a>) -> Ctx<'a> {
    Ctx { cfg: c.cfg, workspace: c.workspace.clone(), input: c.input.clone(), out_dir: c.out_dir.clone(),
          profile_dir: c.profile_dir.clone(), timeout: c.timeout, ocr: c.ocr }
}

pub async fn heif(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let src_heif = matches!(conv.source.extension.as_str(), ".heic" | ".heif");
    let tgt_heif = matches!(conv.target.id.as_str(), "heic" | "heif");
    let out = output(ctx, conv);
    let ws = &ctx.workspace;
    let decode = |i: PathBuf, o: PathBuf| async move {
        process::run(&ctx.bin("heif")?, &[s(&i), s(&o)], ws, ctx.timeout).await?.require_ok("heif-convert").map(|_| ())
    };
    let encode = |i: PathBuf, o: PathBuf| async move {
        let enc = ctx.bin("heif")?.with_file_name(if cfg!(windows) { "heif-enc.exe" } else { "heif-enc" });
        process::run(&enc, &["-o".into(), s(&o), s(&i)], ws, ctx.timeout).await?.require_ok("heif-enc").map(|_| ())
    };
    let mid = ws.join("heif-intermediate.png");
    if src_heif && !tgt_heif && matches!(conv.target.extension.as_str(), ".jpg" | ".jpeg" | ".png" | ".tif" | ".tiff") {
        decode(ctx.input.clone(), out).await?;
    } else if src_heif {
        decode(ctx.input.clone(), mid.clone()).await?;
        if tgt_heif { encode(mid, out).await?; } else { ffmpeg_still(ctx, &mid, &out).await?; }
    } else {
        let direct = matches!(conv.source.extension.as_str(), ".png" | ".jpg" | ".jpeg");
        let enc_in = if direct { ctx.input.clone() } else { ffmpeg_still(ctx, &ctx.input, &mid).await?; mid };
        encode(enc_in, out).await?;
    }
    require_output(ctx, conv, "heif")
}

pub async fn assimp(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let out = output(ctx, conv);
    process::run(&ctx.bin("assimp")?, &["export".into(), s(&ctx.input), s(&out)], &ctx.workspace, ctx.timeout)
        .await?.require_ok("assimp")?;
    require_output(ctx, conv, "assimp")
}

/// A missing system library shows up as a Python/Qt traceback; say what to install instead.
pub fn explain_calibre_failure(stderr: &str) -> Option<&'static str> {
    let low = stderr.to_lowercase();
    (low.contains("libegl") || low.contains("libopengl") || low.contains("libxcb-cursor") || low.contains("cannot open shared object file"))
        .then_some("Calibre needs a few system libraries that are missing on this computer. On Debian/Ubuntu: sudo apt install libegl1 libopengl0 libxcb-cursor0 libxkbcommon0")
}

pub async fn ebook(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let out = output(ctx, conv);
    let o = process::run(&ctx.bin("calibre")?, &[s(&ctx.input), s(&out)], &ctx.workspace, ctx.timeout).await?;
    if !o.ok() {
        if let Some(hint) = explain_calibre_failure(&o.stderr) { return Err(Error::new(ErrorCode::EConvertFailed, hint)); }
    }
    o.require_ok("ebook-convert")?;
    require_output(ctx, conv, "ebook-convert")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn missing_qt_libraries_get_an_install_hint() {
        let tb = "Failed to import PyQt module: PyQt6.QtGui with error: libEGL.so.1: cannot open shared object file: No such file or directory";
        assert!(explain_calibre_failure(tb).unwrap().contains("apt install libegl1"));
        assert!(explain_calibre_failure("Conversion error: bad epub").is_none(), "other failures keep their own message");
    }
}
