//! Conversion routes. Each returns the files it produced, on disk in `ctx.out_dir`.
mod archive;
mod data;
mod email;
mod extract;
mod image;
mod office;
mod simple;

use crate::{engines, formats::{Engine, Resolved}, Config, Error, ErrorCode, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

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

pub async fn run(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    tokio::fs::create_dir_all(&ctx.out_dir).await?;
    match conv.engine {
        Engine::Soffice if conv.target.mode == "raster" => office::raster(ctx, conv).await,
        Engine::Soffice => office::direct(ctx, conv).await,
        Engine::Archive => archive::run(ctx, conv).await,
        Engine::Email => email::run(ctx, conv).await,
        Engine::Extract => extract::run(ctx, conv).await,
        Engine::Data => data::run(ctx, conv).await,
        Engine::Pandoc => simple::pandoc(ctx, conv).await,
        Engine::Ffmpeg if image::can_handle(conv) => image::run(ctx, conv).await,
        Engine::Ffmpeg => simple::ffmpeg(ctx, conv).await,
        Engine::Heif => simple::heif(ctx, conv).await,
        Engine::Assimp => simple::assimp(ctx, conv).await,
        Engine::Ebook => simple::ebook(ctx, conv).await,
        other => Err(Error::new(
            ErrorCode::EUnsupported,
            format!("{other:?} conversions are not available in this build yet."),
        )),
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
