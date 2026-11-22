//! Conversion routes. Each returns the files it produced, on disk in `ctx.out_dir`.
mod archive;
mod data;
mod email;
mod extract;
mod image;
mod office;
pub mod python;
mod simple;

use crate::{engines, formats::{Engine, Resolved}, Config, Error, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The external tool a conversion really needs, or `None` when it runs in pure Rust
/// (the native image path never touches ffmpeg, so a PNG -> WebP must not ask for it).
pub fn required_tool_for(conv: &Resolved) -> Option<&'static str> {
    if conv.engine == Engine::Ffmpeg && image::can_handle(conv) { return None; }
    // A PDF's pages are drawn by poppler when present, otherwise by the built-in renderer: nothing to install.
    if conv.engine == Engine::Soffice && conv.target.mode == "raster" && conv.source.extension == ".pdf" { return None; }
    engines::required_tool(conv.engine)
}

pub struct Ctx<'a> {
    pub cfg: &'a Config,
    pub workspace: PathBuf,
    pub input: PathBuf,
    pub out_dir: PathBuf,
    pub profile_dir: PathBuf,
    pub timeout: Duration,
    pub ocr: bool,
}

impl Ctx<'_> {
    /// Resolved path to an external tool, or `E_ENGINE_MISSING` so the UI can offer to install it.
    pub fn bin(&self, tool: &str) -> Result<PathBuf> {
        let t = engines::tool(tool).expect("known tool");
        engines::resolve(t, self.cfg, None).ok_or_else(|| Error::engine_missing(tool))
    }
}

/// The tool to offer installing: set only when it is missing AND the built-in converters cannot do the pair.
pub fn missing_tool(cfg: &Config, conv: &Resolved) -> Option<&'static str> {
    let name = required_tool_for(conv)?;
    let tool = engines::tool(name)?;
    if engines::resolve(tool, cfg, None).is_some() || crate::native::supports(&conv.source.extension, &conv.target.id) { return None; }
    Some(tool.name)
}

/// The installed engine is preferred (better fidelity); the built-in route only steps in when it is absent.
fn native_fallback(ctx: &Ctx<'_>, conv: &Resolved) -> bool {
    if conv.engine == Engine::Native { return true; }
    let Some(name) = required_tool_for(conv) else { return false };
    let Some(tool) = engines::tool(name) else { return false };
    crate::native::supports(&conv.source.extension, &conv.target.id) && engines::resolve(tool, ctx.cfg, None).is_none()
}

pub async fn run(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    tokio::fs::create_dir_all(&ctx.out_dir).await?;
    if native_fallback(ctx, conv) { return crate::native::run(ctx, conv).await; }
    match conv.engine {
        Engine::Native => crate::native::run(ctx, conv).await,
        Engine::Soffice if conv.target.mode == "raster" => office::raster(ctx, conv).await,
        Engine::Soffice => office::direct(ctx, conv).await,
        Engine::Archive => archive::run(ctx, conv).await,
        Engine::Email => email::run(ctx, conv).await,
        Engine::Extract => extract::run(ctx, conv).await,
        Engine::PdfEngine | Engine::Font => python::run(ctx, conv).await,
        Engine::Data => data::run(ctx, conv).await,
        Engine::Pandoc => simple::pandoc(ctx, conv).await,
        Engine::Ffmpeg if image::can_handle(conv) => image::run(ctx, conv).await,
        Engine::Ffmpeg => simple::ffmpeg(ctx, conv).await,
        Engine::Heif => simple::heif(ctx, conv).await,
        Engine::Assimp => simple::assimp(ctx, conv).await,
        Engine::Ebook => simple::ebook(ctx, conv).await,
    }
}

/// Files in `dir` ending with `ext`, non-empty, ordered by page number (slide-2 before slide-10).
pub fn collect_produced(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let ext = ext.to_lowercase();
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file()
            && p.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase().ends_with(&ext))
            && std::fs::metadata(p).is_ok_and(|m| m.len() > 0))
        .collect();
    v.sort_by_key(|p| {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let digits: String = name.chars().rev().skip(ext.len()).take_while(|c| c.is_ascii_digit()).collect::<Vec<_>>().into_iter().rev().collect();
        (digits.parse::<u64>().unwrap_or(0), name)
    });
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formats::resolve_conversion;
    #[test] fn native_images_need_no_external_tool() {
        assert_eq!(required_tool_for(&resolve_conversion(".png", "webp").unwrap()), None);
        assert_eq!(required_tool_for(&resolve_conversion(".jpg", "bmp").unwrap()), None);
    }
    #[test] fn non_native_routes_still_name_their_tool() {
        assert_eq!(required_tool_for(&resolve_conversion(".png", "avif").unwrap()), Some("ffmpeg"));
        assert_eq!(required_tool_for(&resolve_conversion(".docx", "pdf").unwrap()), Some("libreoffice"));
        assert_eq!(required_tool_for(&resolve_conversion(".csv", "json").unwrap()), None);
    }
    #[test] fn pdf_pages_need_no_tool_because_the_built_in_renderer_replaces_poppler() {
        assert_eq!(required_tool_for(&resolve_conversion(".pdf", "png").unwrap()), None);
        assert_eq!(required_tool_for(&resolve_conversion(".pdf", "jpg").unwrap()), None);
        assert_eq!(required_tool_for(&resolve_conversion(".pptx", "png").unwrap()), Some("libreoffice"));
    }
    #[test] fn built_in_converters_remove_the_install_prompt() {
        let cfg = Config { engines_dir: std::env::temp_dir().join("no-engines-here"), ..Config::default() };
        // Only meaningful when the machine has no LibreOffice: the check is "missing AND no built-in route".
        let conv = resolve_conversion(".docx", "markdown").unwrap();
        assert_eq!(conv.engine, Engine::Native);
        assert_eq!(missing_tool(&cfg, &conv), None);
    }
}
