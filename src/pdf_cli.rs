//! `converter pdf <op> …` — thin file-in/file-out wrapper over `pdf::*` for the CLI and tests.
use crate::pdf::{pages, qpdf, ranges};
use crate::{convert::unique_path, Config, Error, ErrorCode, Result};
use std::path::{Path, PathBuf};

fn bad(m: impl Into<String>) -> Error { Error::new(ErrorCode::EBadRequest, m) }

async fn read(p: &Path) -> Result<Vec<u8>> { Ok(tokio::fs::read(p).await?) }

async fn write(dir: &Path, stem: &str, suffix: &str, data: &[u8]) -> Result<PathBuf> {
    tokio::fs::create_dir_all(dir).await?;
    let dest = unique_path(dir, &format!("{stem}{suffix}"), ".pdf");
    tokio::fs::write(&dest, data).await?;
    Ok(dest)
}

/// `flag` values are the CLI's `--name value` pairs.
pub async fn run(cfg: &Config, op: &str, files: &[PathBuf], flag: &dyn Fn(&str) -> Option<String>, out: Option<PathBuf>) -> Result<Vec<PathBuf>> {
    let first = files.first().ok_or_else(|| bad("Give at least one PDF."))?;
    let dir = out.unwrap_or_else(|| first.parent().map(Path::to_path_buf).unwrap_or_default());
    let stem = first.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "output".into());
    let num = |n: &str| flag(n).map(|v| v.parse::<f64>().map_err(|_| bad(format!("--{n} must be a number")))).transpose();
    let need = |n: &str| flag(n).ok_or_else(|| bad(format!("--{n} is required")));
    let data = read(first).await?;
    let count = || pages::page_count(&data);
    let sel = |name: &str| -> Result<Vec<usize>> { ranges::parse_selection(&need(name)?, count()?) };
    let opt_sel = |name: &str| -> Result<Option<Vec<usize>>> { flag(name).map(|s| ranges::parse_selection(&s, count()?)).transpose() };

    let one = |suffix: &'static str, bytes: Vec<u8>| { let dir = dir.clone(); let stem = stem.clone(); async move { write(&dir, &stem, suffix, &bytes).await.map(|p| vec![p]) } };
    match op {
        "merge" => {
            if files.len() < 2 { return Err(Error::new(ErrorCode::ETooFewFiles, "Merging needs at least two PDFs.")); }
            if files.len() > cfg.max_page_operation_files { return Err(bad(format!("At most {} files can be merged at once.", cfg.max_page_operation_files))); }
            let mut all = vec![]; for f in files { all.push(read(f).await?); }
            one("-merged", pages::merge(&all)?).await
        }
        "split" => {
            let every = num("every")?.unwrap_or(1.0) as usize;
            let mut out = vec![];
            for (i, part) in pages::split(&data, every)?.iter().enumerate() { out.push(write(&dir, &stem, &format!("-part{}", i + 1), part).await?); }
            Ok(out)
        }
        "extract" => one("-extracted", pages::select_pages(&data, &sel("pages")?)?).await,
        "remove" => one("-removed", pages::remove_pages(&data, &sel("pages")?)?).await,
        "organize" => {
            let order = sel("order")?;
            if !ranges::is_permutation(&order, count()?) {
                return Err(Error::new(ErrorCode::EBadPageRange, format!("The order must name every page exactly once (1-{}).", count()?)));
            }
            one("-organized", pages::select_pages(&data, &order)?).await
        }
        "rotate" => {
            let d = num("degrees")?.ok_or_else(|| bad("--degrees is required"))? as i64;
            if ![90, 180, 270, -90, -180, -270].contains(&d) { return Err(bad("degrees must be one of 90, 180, 270, -90, -180, -270.")); }
            one("-rotated", pages::rotate(&data, d, opt_sel("pages")?.as_deref())?).await
        }
        "crop" => {
            let m = pages::CropMargins { left: num("left")?.unwrap_or(0.0), right: num("right")?.unwrap_or(0.0), top: num("top")?.unwrap_or(0.0), bottom: num("bottom")?.unwrap_or(0.0) };
            one("-cropped", pages::crop(&data, m, opt_sel("pages")?.as_deref())?).await
        }
        "protect" => one("-protected", qpdf::run(cfg, qpdf::Op::Protect(need("password")?), &data).await?).await,
        "unlock" => one("-unlocked", qpdf::run(cfg, qpdf::Op::Unlock(need("password")?), &data).await?).await,
        "repair" => one("-repaired", qpdf::run(cfg, qpdf::Op::Repair, &data).await?).await,
        "compress" => {
            let level = match flag("level").as_deref() { None | Some("medium") => qpdf::CompressLevel::Medium, Some("low") => qpdf::CompressLevel::Low,
                Some("high") => qpdf::CompressLevel::High, Some(_) => return Err(bad("--level must be low, medium or high")) };
            one("-compressed", qpdf::run(cfg, qpdf::Op::Compress(level), &data).await?).await
        }
        other => Err(bad(format!("Unknown PDF operation \"{other}\"."))),
    }
}
