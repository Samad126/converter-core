//! Routes run by our Python scripts: PDF → docx/pptx/xlsx/markdown (`pdf_engine.py`) and fonts.
use super::Ctx;
use crate::{formats::{Engine, Resolved}, process, Error, ErrorCode, Result};
use std::path::{Path, PathBuf};

const NO_TABLES_EXIT: i32 = 2;

/// Where the scripts folder is, given the running executable: next to it, one or two folders up (an install tree), or in a
/// macOS bundle's `Contents/Resources`. `exists` is injected so the search order can be tested without a filesystem.
pub fn find_scripts_for(exe: &Path, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    for dir in exe.ancestors().skip(1).take(3) {
        for rel in ["scripts", "Resources/scripts"] {
            let cand = dir.join(rel);
            if exists(&cand.join("pdf_engine.py")) { return Some(cand); }
        }
    }
    None
}

/// Where `scripts/` lives: env override, next to the executable (or inside a macOS bundle), or the source tree (dev builds).
pub fn scripts_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("CONVERTER_SCRIPTS_DIR") { return p.into(); }
    if let Some(found) = std::env::current_exe().ok().and_then(|exe| find_scripts_for(&exe, |p| p.is_file())) { return found; }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts")
}

fn s(p: &Path) -> String { p.to_string_lossy().into_owned() }

/// `ModuleNotFoundError: No module named 'x'` → an install prompt instead of a stack trace.
fn missing_module(stderr: &str) -> Option<Error> {
    let m = stderr.lines().find(|l| l.contains("ModuleNotFoundError") || l.contains("No module named '"))?;
    let name = m.split('\'').nth(1).unwrap_or("a Python package");
    let mut e = Error::new(ErrorCode::EEngineMissing,
        format!("This conversion needs the Python package \"{name}\", which is not installed. Install the Python tools to add it."));
    // The bundled Python (with all packages) is one download; the UI turns this into an Install button.
    e.engine = Some("python".into());
    Some(e)
}

pub async fn run(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let py = ctx.bin("python")?;
    let out = ctx.out_dir.join(format!("converted{}", conv.target.extension));
    let scripts = scripts_dir();
    let (script, args): (PathBuf, Vec<String>) = match conv.engine {
        Engine::Font => (scripts.join("font_engine.py"), vec![s(&ctx.input), s(&out)]),
        _ => (scripts.join("pdf_engine.py"),
              vec![conv.target.id.clone(), s(&ctx.input), s(&out), ctx.ocr.to_string()]),
    };
    let mut full = vec![s(&script)];
    full.extend(args);
    // pip's user-site packages are found through the real HOME, not the workspace.
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| s(&ctx.workspace));
    let o = process::run_env(&py, &full, &ctx.workspace, ctx.timeout, &[("HOME", home)]).await?;
    if o.code == Some(NO_TABLES_EXIT) && conv.engine == Engine::PdfEngine {
        return Err(Error::new(ErrorCode::ENoTables, "This document does not contain any tables."));
    }
    if let Some(e) = missing_module(&o.stderr) { return Err(e); }
    o.require_ok(if conv.engine == Engine::Font { "font_engine.py" } else { "pdf_engine.py" })?;
    if !out.metadata().is_ok_and(|m| m.len() > 0) {
        return Err(Error::new(ErrorCode::EConvertFailed, format!("The Python engine produced no {} file.", conv.target.extension)));
    }
    Ok(vec![out])
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn module_error_becomes_engine_missing() {
        let e = missing_module("Traceback...\nModuleNotFoundError: No module named 'pdf2docx'\n").unwrap();
        assert_eq!(e.code, ErrorCode::EEngineMissing);
        assert!(e.message.contains("pdf2docx"));
        let e = missing_module("docx failed: No module named 'pdf2docx'").unwrap();
        assert!(e.message.contains("pdf2docx"));
        assert!(missing_module("some other failure").is_none());
    }
    #[test] fn scripts_are_found_beside_the_binary_and_in_a_mac_bundle() {
        let has = |set: &'static [&'static str]| move |p: &Path| set.iter().any(|x| Path::new(x) == p);
        assert_eq!(find_scripts_for(Path::new("/opt/conv/converter-desktop"), has(&["/opt/conv/scripts/pdf_engine.py"])), Some("/opt/conv/scripts".into()));
        assert_eq!(find_scripts_for(Path::new("/Applications/Converter.app/Contents/MacOS/converter-desktop"),
            has(&["/Applications/Converter.app/Contents/Resources/scripts/pdf_engine.py"])), Some("/Applications/Converter.app/Contents/Resources/scripts".into()));
        assert_eq!(find_scripts_for(Path::new("/opt/conv/bin/converter-desktop"), has(&["/opt/conv/scripts/pdf_engine.py"])), Some("/opt/conv/scripts".into()), "one level up");
        assert_eq!(find_scripts_for(Path::new("/usr/bin/converter-desktop"), has(&[])), None);
    }
    #[test] fn scripts_found_in_dev_tree() { assert!(scripts_dir().join("pdf_engine.py").is_file()); }
}

/// Runs one of our scripts with the resolved Python, outside a conversion `Ctx`
/// (used by the standalone PDF operations). Missing packages become `E_ENGINE_MISSING`.
pub async fn run_script(cfg: &crate::Config, script: &str, args: Vec<String>, workspace: &Path, timeout: std::time::Duration) -> Result<process::Outcome> {
    let py = crate::engines::resolve(crate::engines::tool("python").unwrap(), cfg, None).ok_or_else(|| Error::engine_missing("python"))?;
    let mut full = vec![s(&scripts_dir().join(script))];
    full.extend(args);
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| s(workspace));
    let o = process::run_env(&py, &full, workspace, timeout, &[("HOME", home)]).await?;
    if o.code != Some(0) {
        if let Some(e) = missing_module(&o.stderr) { return Err(e); }
        let low = o.stderr.to_lowercase();
        if low.contains("tesseract") && (low.contains("not found") || low.contains("no such file") || low.contains("tesseractnotfound")) {
            return Err(Error::engine_missing("tesseract"));
        }
    }
    Ok(o)
}
