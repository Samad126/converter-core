//! Archive → archive via `7z`. Port of `archive.engine.ts` + `archive.pipeline.ts`.
//! Unpacking untrusted bytes is the hazard here, so: list first and refuse on entry count,
//! declared size, encryption, symlinks and path traversal BEFORE extracting anything.
use super::Ctx;
use crate::{formats::Resolved, process, Config, Error, ErrorCode, Result};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Clone)]
pub struct Entry {
    pub path: String,
    pub is_folder: bool,
    pub size: u64,
    pub attributes: String,
    pub encrypted: bool,
}

fn s(p: &Path) -> String { p.to_string_lossy().into_owned() }
fn fail(msg: String) -> Error { Error::new(ErrorCode::EConvertFailed, msg) }

/// Parses `7z l -slt -ba` output (blank-line separated `Key = Value` blocks).
pub fn parse_listing(out: &str) -> Vec<Entry> {
    let (mut entries, mut cur, mut has_path) = (vec![], Entry::default(), false);
    for line in out.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if has_path { entries.push(std::mem::take(&mut cur)); }
            cur = Entry::default(); has_path = false; continue;
        }
        let Some((k, v)) = line.split_once(" = ") else { continue };
        match k {
            "Path" => { cur.path = v.to_string(); has_path = true; }
            "Folder" => cur.is_folder = v == "+",
            "Size" => cur.size = v.parse().unwrap_or(0),
            "Attributes" => cur.attributes = v.to_string(),
            "Encrypted" => cur.encrypted = v == "+",
            _ => {}
        }
    }
    entries
}

pub fn validate_entries(entries: &[Entry], cfg: &Config) -> Result<()> {
    if entries.len() > cfg.max_archive_entries {
        return Err(fail(format!("archive has {} entries, which is over the {} limit",
            entries.len(), cfg.max_archive_entries)));
    }
    let mut total: u64 = 0;
    for e in entries {
        if e.encrypted {
            return Err(Error::new(ErrorCode::EEncrypted, "This file is password-protected, so it cannot be converted."));
        }
        if e.attributes.trim_start().starts_with('l') {
            return Err(fail(format!("archive entry \"{}\" is a symlink, which is refused", e.path)));
        }
        let norm = e.path.replace('\\', "/");
        if norm.starts_with('/') || norm.split('/').any(|seg| seg == "." || seg == "..") {
            return Err(fail(format!("archive entry \"{}\" has an unsafe path", e.path)));
        }
        if !e.is_folder { total = total.saturating_add(e.size); }
    }
    if total > cfg.max_archive_uncompressed_bytes {
        return Err(fail(format!("archive declares {total} bytes of content, which is over the {}-byte limit",
            cfg.max_archive_uncompressed_bytes)));
    }
    Ok(())
}

async fn sz(ctx: &Ctx<'_>, args: Vec<String>, label: &str) -> Result<process::Outcome> {
    process::run(&ctx.bin("7zip")?, &args, &ctx.workspace, ctx.timeout).await?.require_ok(label)
}

async fn list_and_validate(ctx: &Ctx<'_>, archive: &Path) -> Result<()> {
    let out = sz(ctx, vec!["l".into(), "-slt".into(), "-ba".into(), "--".into(), s(archive)], "7z list").await?;
    validate_entries(&parse_listing(&out.stdout), ctx.cfg)
}

async fn extract(ctx: &Ctx<'_>, archive: &Path, out_dir: &Path) -> Result<()> {
    tokio::fs::create_dir_all(out_dir).await?;
    sz(ctx, vec!["x".into(), format!("-o{}", s(out_dir)), "-y".into(), "--".into(), s(archive)], "7z extract").await?;
    Ok(())
}

/// Unpacks into `out_dir`; a lone inner `.tar` (from .tar.gz etc.) is unpacked one level further.
async fn extract_tree(ctx: &Ctx<'_>, archive: &Path, out_dir: &Path) -> Result<PathBuf> {
    list_and_validate(ctx, archive).await?;
    extract(ctx, archive, out_dir).await?;
    let mut rd = tokio::fs::read_dir(out_dir).await?;
    let mut items = vec![];
    while let Some(e) = rd.next_entry().await? { items.push(e); }
    if items.len() == 1 {
        let e = &items[0];
        if e.file_type().await?.is_file() && e.file_name().to_string_lossy().ends_with(".tar") {
            let inner = e.path();
            let inner_out = ctx.workspace.join("archive-inner");
            list_and_validate(ctx, &inner).await?;
            extract(ctx, &inner, &inner_out).await?;
            return Ok(inner_out);
        }
    }
    Ok(out_dir.to_path_buf())
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> std::io::Result<()> {
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let p = e.path();
        let ft = e.file_type()?;
        if ft.is_dir() { walk(root, &p, out)?; }
        else if ft.is_file() {
            let name = p.strip_prefix(root).unwrap().components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
            out.push((name, p));
        }
    }
    Ok(())
}

fn zip_tree(tree: &Path, out: &Path) -> Result<()> {
    let mut files = vec![];
    walk(tree, tree, &mut files)?;
    files.sort();
    let mut zw = zip::ZipWriter::new(std::fs::File::create(out)?);
    let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, path) in files {
        zw.start_file(name, opt).map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))?;
        std::io::copy(&mut std::fs::File::open(path)?, &mut zw)?;
    }
    zw.finish().map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))?;
    Ok(())
}

pub async fn run(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let writer = conv.target.archive_writer.clone()
        .ok_or_else(|| fail(format!("no archiveWriter registered for target \"{}\"", conv.target.id)))?;

    let archive = if conv.source.extension == ".zst" {
        let dec = ctx.workspace.join("archive-zst-decompressed");
        process::run(&ctx.bin("zstd")?, &["-d".into(), "-f".into(), "-o".into(), s(&dec), s(&ctx.input)],
            &ctx.workspace, ctx.timeout).await?.require_ok("zstd")?;
        dec
    } else { ctx.input.clone() };

    let tree = extract_tree(ctx, &archive, &ctx.workspace.join("archive-extracted")).await?;
    let out = ctx.out_dir.join(format!("converted{}", conv.target.extension));

    if writer == "zip" {
        let (t, o) = (tree.clone(), out.clone());
        tokio::task::spawn_blocking(move || zip_tree(&t, &o))
            .await.map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))??;
        return Ok(vec![out]);
    }

    let src = format!("{}/.", s(&tree));
    match writer.as_str() {
        "tar" => { sz(ctx, vec!["a".into(), "-ttar".into(), s(&out), src], "7z create").await?; }
        "7z" => { sz(ctx, vec!["a".into(), "-t7z".into(), s(&out), src], "7z create").await?; }
        w => {
            let tar = ctx.workspace.join("archive-intermediate.tar");
            sz(ctx, vec!["a".into(), "-ttar".into(), s(&tar), src], "7z create").await?;
            if w == "tar.zst" {
                process::run(&ctx.bin("zstd")?, &["-f".into(), "-o".into(), s(&out), s(&tar)], &ctx.workspace, ctx.timeout)
                    .await?.require_ok("zstd")?;
            } else {
                let fmt = if w == "tar.gz" { "gzip" } else { "bzip2" };
                sz(ctx, vec!["a".into(), format!("-t{fmt}"), s(&out), s(&tar)], "7z compress").await?;
            }
        }
    }
    if !out.metadata().is_ok_and(|m| m.len() > 0) {
        return Err(fail(format!("7z produced no {} file for archive target \"{}\"", conv.target.extension, conv.target.id)));
    }
    let _ = std::io::stdout().flush();
    Ok(vec![out])
}

#[cfg(test)]
mod tests {
    use super::*;
    fn e(path: &str) -> Entry { Entry { path: path.into(), size: 10, ..Default::default() } }
    fn cfg() -> Config { Config::default() }

    #[test]
    fn listing_parses_blocks() {
        let out = "Path = a.txt\nSize = 5\nFolder = -\nAttributes = -rw-r--r--\n\nPath = d\nFolder = +\nSize = 0\n";
        let v = parse_listing(out);
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].size, v[0].is_folder, v[1].is_folder), (5, false, true));
    }
    #[test] fn rejects_traversal() {
        for p in ["../x", "a/../b", "/abs", "a\\..\\b"] {
            assert!(validate_entries(&[e(p)], &cfg()).is_err(), "{p}");
        }
        assert!(validate_entries(&[e("a/b.txt"), e("..hidden")], &cfg()).is_ok());
    }
    #[test] fn rejects_symlink_and_encrypted() {
        assert!(validate_entries(&[Entry { attributes: "lrwxrwxrwx".into(), ..e("l") }], &cfg()).is_err());
        let enc = validate_entries(&[Entry { encrypted: true, ..e("x") }], &cfg()).unwrap_err();
        assert_eq!(enc.code, ErrorCode::EEncrypted);
    }
    #[test] fn rejects_bombs() {
        let mut c = cfg(); c.max_archive_entries = 2;
        assert!(validate_entries(&[e("a"), e("b"), e("c")], &c).is_err());
        let mut c = cfg(); c.max_archive_uncompressed_bytes = 15;
        assert!(validate_entries(&[e("a"), e("b")], &c).is_err());
    }
}
