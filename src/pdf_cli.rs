//! `converter pdf <op> …` — thin file-in/file-out wrapper over `pdf::*` for the CLI and tests.
use crate::pdf::{draw::ImageFormat, overlay, pages, qpdf, ranges};
use crate::{convert::output_path, Config, Error, ErrorCode, Result};
use std::path::{Path, PathBuf};

fn bad(m: impl Into<String>) -> Error { Error::new(ErrorCode::EBadRequest, m) }

async fn read(p: &Path) -> Result<Vec<u8>> { Ok(tokio::fs::read(p).await?) }

async fn write(cfg: &Config, dir: &Path, stem: &str, suffix: &str, data: &[u8]) -> Result<PathBuf> {
    tokio::fs::create_dir_all(dir).await?;
    let dest = output_path(cfg, dir, &format!("{stem}{suffix}"), ".pdf");
    tokio::fs::write(&dest, data).await?;
    Ok(dest)
}

/// `flag` values are the CLI's `--name value` pairs.
pub async fn run(cfg: &Config, op: &str, files: &[PathBuf], flag: &(dyn Fn(&str) -> Option<String> + Sync), out: Option<PathBuf>) -> Result<Vec<PathBuf>> {
    let first = files.first().ok_or_else(|| bad("Give at least one PDF."))?;
    let dir = out.unwrap_or_else(|| first.parent().map(Path::to_path_buf).unwrap_or_default());
    let stem = first.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "output".into());
    let num = |n: &str| flag(n).map(|v| v.parse::<f64>().map_err(|_| bad(format!("--{n} must be a number")))).transpose();
    let need = |n: &str| flag(n).ok_or_else(|| bad(format!("--{n} is required")));
    let data = read(first).await?;
    let count = || pages::page_count(&data);
    let sel = |name: &str| -> Result<Vec<usize>> { ranges::parse_selection(&need(name)?, count()?) };
    let opt_sel = |name: &str| -> Result<Option<Vec<usize>>> { flag(name).map(|s| ranges::parse_selection(&s, count()?)).transpose() };

    let one = |suffix: &'static str, bytes: Vec<u8>| { let dir = dir.clone(); let stem = stem.clone(); async move { write(cfg, &dir, &stem, suffix, &bytes).await.map(|p| vec![p]) } };
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
            for (i, part) in pages::split(&data, every)?.iter().enumerate() { out.push(write(cfg, &dir, &stem, &format!("-part{}", i + 1), part).await?); }
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
        "ocr" => one("-ocr", crate::pdf::python_ops::ocr(cfg, &data, flag("force").as_deref() == Some("true")).await?).await,
        "redact" => {
            let json = tokio::fs::read_to_string(need("areas")?).await?;
            let areas: Vec<crate::pdf::python_ops::Area> = serde_json::from_str(&json).map_err(|e| bad(format!("The areas file is not valid: {e}")))?;
            one("-redacted", crate::pdf::python_ops::redact(cfg, &data, &areas).await?).await
        }
        "compare" => {
            if files.len() != 2 { return Err(bad("compare needs exactly two PDFs.")); }
            let report = crate::pdf::python_ops::compare(cfg, &data, &read(&files[1]).await?).await?;
            println!("{}", serde_json::to_string_pretty(&report).unwrap());
            Ok(vec![])
        }
        "form-fields" => { println!("{}", serde_json::to_string_pretty(&crate::pdf::forms::list_fields(&data)?).unwrap()); Ok(vec![]) }
        "fill-form" => {
            let json = tokio::fs::read_to_string(need("values")?).await?;
            let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&json).map_err(|e| bad(format!("The values file is not valid: {e}")))?;
            let mut values = vec![];
            for (k, v) in map {
                let fv = match v { serde_json::Value::Bool(b) => crate::pdf::forms::FieldValue::Bool(b), serde_json::Value::String(s) => crate::pdf::forms::FieldValue::Text(s),
                    other => return Err(Error::new(ErrorCode::EInvalidField, format!("The value given for field \"{k}\" must be text or true/false, not {other}."))) };
                values.push((k, fv));
            }
            one("-filled", crate::pdf::forms::fill(&data, &values, flag("flatten").as_deref() == Some("true"))?).await
        }
        "watermark" => one("-watermarked", overlay::watermark(&data, &need("text")?, opt_sel("pages")?.as_deref())?).await,
        "page-numbers" => {
            let pos = match flag("position").as_deref() { None | Some("bottom-center") => overlay::NumberPosition::BottomCenter,
                Some("bottom-right") => overlay::NumberPosition::BottomRight, Some("bottom-left") => overlay::NumberPosition::BottomLeft,
                Some(_) => return Err(bad("--position must be bottom-center, bottom-right or bottom-left")) };
            one("-numbered", overlay::page_numbers(&data, pos, num("start")?.unwrap_or(1.0) as i64)?).await
        }
        "scan-to-pdf" => {
            let mut imgs = vec![];
            for f in files {
                let fmt = match crate::formats::extension_of(f).as_str() { ".png" => ImageFormat::Png, ".jpg" | ".jpeg" => ImageFormat::Jpg,
                    e => return Err(bad(format!("\"{e}\" is not a PNG or JPG image."))) };
                imgs.push((read(f).await?, fmt));
            }
            one("-scan", overlay::scan_to_pdf(&imgs)?).await
        }
        "edit" | "sign" => {
            // --elements <json file>; extra files after the PDF are the images the elements point at.
            let json = tokio::fs::read_to_string(need("elements")?).await?;
            let mut imgs = vec![];
            for f in &files[1..] {
                let fmt = if crate::formats::extension_of(f) == ".png" { ImageFormat::Png } else { ImageFormat::Jpg };
                imgs.push((read(f).await?, fmt));
            }
            let parse_err = |e: serde_json::Error| bad(format!("The elements file is not valid: {e}"));
            if op == "edit" { one("-edited", overlay::edit(&data, &serde_json::from_str::<Vec<overlay::EditElement>>(&json).map_err(parse_err)?, &imgs)?).await }
            else { one("-signed", overlay::sign(&data, &serde_json::from_str::<Vec<overlay::SignElement>>(&json).map_err(parse_err)?, &imgs)?).await }
        }
        other => Err(bad(format!("Unknown PDF operation \"{other}\"."))),
    }
}
