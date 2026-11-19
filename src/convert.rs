//! Public entry point: convert one file on disk.
use crate::{formats, pipelines, workspace::Workspace, Config, Error, ErrorCode, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Default, Clone)]
pub struct ConvertOptions {
    /// Where to write; defaults to the input's own folder.
    pub out_dir: Option<PathBuf>,
    pub ocr: Option<bool>,
}

/// `report.docx` → `report.pdf`, or `report (1).pdf` when that exists.
pub fn unique_path(dir: &Path, stem: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{stem}{ext}"));
    if !first.exists() { return first; }
    (1..).map(|n| dir.join(format!("{stem} ({n}){ext}"))).find(|p| !p.exists()).unwrap()
}

/// Where a result goes: `name.ext`, then `name (1).ext`... unless the config says to replace.
pub fn output_path(cfg: &Config, dir: &Path, stem: &str, ext: &str) -> PathBuf {
    if cfg.overwrite_existing { dir.join(format!("{stem}{ext}")) } else { unique_path(dir, stem, ext) }
}

/// Returns the written files (one, or one `.zip` when the route produced several).
pub async fn convert_file(cfg: &Config, input: &Path, target: &str, opts: ConvertOptions) -> Result<Vec<PathBuf>> {
    let ext = formats::extension_of(input);
    if !formats::is_target_id(target) {
        return Err(Error::new(ErrorCode::EUnknownTarget, format!("Unknown target \"{target}\".")));
    }
    if !formats::is_allowed_extension(&ext) {
        return Err(Error::new(ErrorCode::EUnsupported, format!("Files of type \"{ext}\" are not supported.")));
    }
    let conv = formats::resolve_conversion(&ext, target).ok_or_else(|| Error::new(
        ErrorCode::EUnsupportedTarget,
        format!("{ext} cannot become {target}. It can become: {}", formats::targets_for(&ext).join(", ")),
    ))?;
    if tokio::fs::metadata(input).await?.len() == 0 {
        return Err(Error::new(ErrorCode::EBadRequest, "The file is empty."));
    }

    let ws = Workspace::create(cfg).await?;
    let work_input = ws.path().join(format!("input{ext}"));
    tokio::fs::copy(input, &work_input).await?;
    let ctx = pipelines::Ctx {
        cfg,
        workspace: ws.path().to_path_buf(),
        input: work_input,
        out_dir: ws.path().join("out"),
        profile_dir: ws.path().join("lo-profile"),
        timeout: cfg.convert_timeout,
        ocr: opts.ocr.unwrap_or(true),
    };
    let produced = pipelines::run(&ctx, &conv).await?;

    let dest_dir = opts.out_dir.unwrap_or_else(|| input.parent().map(Path::to_path_buf).unwrap_or_default());
    tokio::fs::create_dir_all(&dest_dir).await?;
    let mut stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "converted".into());
    // `foo.tar.gz` has stem `foo.tar`; the result should be `foo.<new ext>`.
    if let Some(s) = stem.strip_suffix(".tar") { stem = s.to_string(); }

    let written = if produced.len() == 1 {
        let dest = output_path(cfg, &dest_dir, &stem, &conv.target.extension);
        tokio::fs::copy(&produced[0], &dest).await?;
        dest
    } else {
        let dest = output_path(cfg, &dest_dir, &stem, ".zip");
        let file = std::fs::File::create(&dest)?;
        let mut zip = zip::ZipWriter::new(file);
        let opt = zip::write::SimpleFileOptions::default();
        for p in &produced {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            zip.start_file(name, opt).map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))?;
            zip.write_all(&std::fs::read(p)?)?;
        }
        zip.finish().map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))?;
        dest
    };
    Ok(vec![written])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn output_path_numbers_or_replaces() {
        let d = std::env::temp_dir().join(format!("outpath-{}", std::process::id())); std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("a.pdf"), "x").unwrap();
        let mut cfg = Config::default();
        assert_eq!(output_path(&cfg, &d, "a", ".pdf").file_name().unwrap(), "a (1).pdf");
        cfg.overwrite_existing = true;
        assert_eq!(output_path(&cfg, &d, "a", ".pdf").file_name().unwrap(), "a.pdf");
        assert_eq!(output_path(&cfg, &d, "b", ".pdf").file_name().unwrap(), "b.pdf");
    }
}
