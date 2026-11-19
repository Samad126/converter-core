//! Installs engines on demand (Filee-style): pinned URL + SHA-256, downloaded into the app's
//! own data folder, unpacked into a staging dir and swapped in atomically, no admin rights.
//! A failed or tampered install never touches an existing working copy.
use crate::progress::{Progress, ProgressFn};
use crate::{engines, Config, Error, ErrorCode, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
pub struct PlatformEntry {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// `zip` | `tar.gz` | `tar.xz` | `file`
    pub kind: String,
    /// For `kind = file`: the name to save the executable as.
    #[serde(default)]
    pub file_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EngineEntry { pub version: String, pub platforms: BTreeMap<String, PlatformEntry> }

#[derive(Debug, Deserialize)]
pub struct Manifest { pub engines: BTreeMap<String, EngineEntry> }

static MANIFEST: OnceLock<Manifest> = OnceLock::new();

pub fn manifest() -> &'static Manifest {
    MANIFEST.get_or_init(|| serde_json::from_str(include_str!("../../data/engines.json")).expect("data/engines.json is valid"))
}

/// `linux-x64`, `windows-x64`, `macos-arm64`, …
pub fn current_platform() -> String {
    let os = match std::env::consts::OS { "macos" => "macos", "windows" => "windows", _ => "linux" };
    let arch = match std::env::consts::ARCH { "aarch64" => "arm64", _ => "x64" };
    format!("{os}-{arch}")
}

fn err(msg: impl Into<String>) -> Error { Error::new(ErrorCode::EConvertFailed, msg) }

#[derive(Debug, Clone)]
pub struct CatalogItem {
    pub name: &'static str,
    pub used_for: &'static str,
    pub installed_path: Option<PathBuf>,
    /// Version we would download on this platform (None = no portable download here; use `hint`).
    pub downloadable: Option<String>,
    pub size: Option<u64>,
    pub hint: &'static str,
}

/// Every known tool with whether it is present and whether we can download it on this platform.
pub fn catalog(cfg: &Config) -> Vec<CatalogItem> {
    let plat = current_platform();
    engines::TOOLS.iter().map(|t| {
        let entry = manifest().engines.get(t.name);
        let p = entry.and_then(|e| e.platforms.get(&plat).map(|p| (e.version.clone(), p.size)));
        CatalogItem { name: t.name, used_for: t.used_for, installed_path: engines::resolve(t, cfg, None),
            downloadable: p.as_ref().map(|x| x.0.clone()), size: p.map(|x| x.1), hint: t.hint() }
    }).collect()
}

pub async fn install(cfg: &Config, name: &str, progress: ProgressFn) -> Result<()> {
    let entry = manifest().engines.get(name).ok_or_else(|| err(format!("\"{name}\" has no portable download.")))?;
    let plat = current_platform();
    let p = entry.platforms.get(&plat).ok_or_else(|| err(format!("\"{name}\" has no portable download for {plat}. Install it with: {}",
        engines::tool(name).map(|t| t.hint()).unwrap_or("your package manager"))))?;
    install_entry(cfg, name, p, progress).await
}

pub async fn remove(cfg: &Config, name: &str) -> Result<()> {
    if engines::tool(name).is_none() { return Err(err(format!("Unknown engine \"{name}\"."))); }
    let dir = cfg.engines_dir.join(name);
    if dir.exists() { tokio::fs::remove_dir_all(&dir).await?; }
    Ok(())
}

pub async fn install_entry(cfg: &Config, name: &str, p: &PlatformEntry, progress: ProgressFn) -> Result<()> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;
    let tool = engines::tool(name).ok_or_else(|| err(format!("Unknown engine \"{name}\".")))?;
    tokio::fs::create_dir_all(&cfg.engines_dir).await?;
    let part = cfg.engines_dir.join(format!(".dl-{name}.part"));
    let staging = cfg.engines_dir.join(format!(".staging-{name}"));
    let _ = tokio::fs::remove_dir_all(&staging).await;
    let cleanup = |part: &Path, staging: &Path| { let _ = std::fs::remove_file(part); let _ = std::fs::remove_dir_all(staging); };

    progress(Progress::Started);
    let resp = reqwest::Client::builder().user_agent("converter-core").build().map_err(|e| err(e.to_string()))?
        .get(&p.url).send().await.and_then(|r| r.error_for_status())
        .map_err(|e| err(format!("Download failed: {e}")))?;
    let mut hasher = Sha256::new();
    let (mut done, mut file) = (0u64, tokio::fs::File::create(&part).await?);
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| { cleanup(&part, &staging); err(format!("Download interrupted: {e}")) })?;
        done += chunk.len() as u64;
        if done > p.size { cleanup(&part, &staging); return Err(err("The download is larger than expected; refusing it.")); }
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
        progress(Progress::Fraction((done as f32 / p.size.max(1) as f32).min(1.0)));
    }
    file.flush().await?;
    drop(file);

    progress(Progress::Stage("Verifying"));
    let digest = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();
    if done != p.size || !digest.eq_ignore_ascii_case(&p.sha256) {
        cleanup(&part, &staging);
        return Err(err(format!("The download did not match its published checksum ({} bytes, sha256 {digest}); it was discarded.", done)));
    }

    progress(Progress::Stage("Unpacking"));
    let (part2, staging2, kind, file_name) = (part.clone(), staging.clone(), p.kind.clone(), p.file_name.clone());
    let unpack = tokio::task::spawn_blocking(move || unpack_archive(&part2, &staging2, &kind, file_name.as_deref())).await
        .map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))?;
    let _ = std::fs::remove_file(&part);
    if let Err(e) = unpack { let _ = std::fs::remove_dir_all(&staging); return Err(e); }

    // Prove the tool is really in there before replacing anything.
    if !tool.bins.iter().any(|b| find(&staging, b, 5)) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err(format!("The downloaded package for {name} does not contain the expected program.")));
    }
    let final_dir = cfg.engines_dir.join(name);
    let old = cfg.engines_dir.join(format!(".old-{name}"));
    let _ = std::fs::remove_dir_all(&old);
    if final_dir.exists() { std::fs::rename(&final_dir, &old)?; }
    if let Err(e) = std::fs::rename(&staging, &final_dir) {
        if old.exists() { let _ = std::fs::rename(&old, &final_dir); } // put the working copy back
        return Err(e.into());
    }
    let _ = std::fs::remove_dir_all(&old);
    progress(Progress::Finished);
    Ok(())
}

fn find(dir: &Path, bin: &str, depth: u32) -> bool {
    std::fs::read_dir(dir).into_iter().flatten().flatten().any(|e| {
        let p = e.path();
        if p.is_dir() { depth > 0 && find(&p, bin, depth - 1) } else { p.file_name().is_some_and(|n| n == bin) }
    })
}

/// Rejects anything that could write outside `dest` (absolute paths, `..`, drive prefixes).
fn safe_relative(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c { Component::Normal(x) => out.push(x), Component::CurDir => {}, _ => return None }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// A link is allowed only if it is relative and still points inside the package.
fn link_stays_inside(rel: &Path, target: &Path) -> bool {
    let resolved = rel.parent().unwrap_or(Path::new("")).join(target);
    let mut depth = 0i32;
    for c in resolved.components() {
        match c { Component::Normal(_) => depth += 1, Component::ParentDir => depth -= 1, Component::CurDir => {}, _ => return false }
        if depth < 0 { return false; }
    }
    true
}

#[cfg(unix)]
fn set_mode(p: &Path, mode: u32) { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)); }
#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) {}

pub fn unpack_archive(archive: &Path, dest: &Path, kind: &str, file_name: Option<&str>) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    match kind {
        "file" => {
            let name = file_name.and_then(|n| safe_relative(Path::new(n))).ok_or_else(|| err("manifest entry has no valid file_name"))?;
            std::fs::copy(archive, dest.join(name.clone()))?;
            set_mode(&dest.join(name), 0o755);
            Ok(())
        }
        "zip" => {
            let mut z = zip::ZipArchive::new(std::fs::File::open(archive)?).map_err(|e| err(format!("Bad zip: {e}")))?;
            for i in 0..z.len() {
                let mut e = z.by_index(i).map_err(|e| err(format!("Bad zip: {e}")))?;
                let rel = e.enclosed_name().and_then(|p| safe_relative(&p)).ok_or_else(|| err("The package contains an unsafe path; refusing it."))?;
                let out = dest.join(&rel);
                if e.is_symlink() {
                    let mut target = String::new();
                    std::io::Read::read_to_string(&mut e, &mut target)?;
                    if !link_stays_inside(&rel, Path::new(&target)) { return Err(err("The package has a link outside itself; refusing it.")); }
                    if let Some(parent) = out.parent() { std::fs::create_dir_all(parent)?; }
                    #[cfg(unix)] std::os::unix::fs::symlink(&target, &out)?;
                    continue;
                }
                if e.is_dir() { std::fs::create_dir_all(&out)?; continue; }
                if let Some(parent) = out.parent() { std::fs::create_dir_all(parent)?; }
                std::io::copy(&mut e, &mut std::fs::File::create(&out)?)?;
                if let Some(m) = e.unix_mode() { set_mode(&out, m & 0o777); }
            }
            Ok(())
        }
        "tar.gz" | "tar.xz" => {
            let f = std::fs::File::open(archive)?;
            let reader: Box<dyn std::io::Read> = if kind == "tar.gz" { Box::new(flate2::read::GzDecoder::new(f)) } else { Box::new(xz2::read::XzDecoder::new(f)) };
            let mut ar = tar::Archive::new(reader);
            for e in ar.entries().map_err(|e| err(format!("Bad archive: {e}")))? {
                let mut e = e.map_err(|e| err(format!("Bad archive: {e}")))?;
                let t = e.header().entry_type();
                let rel = safe_relative(&e.path().map_err(|e| err(e.to_string()))?).ok_or_else(|| err("The package contains an unsafe path; refusing it."))?;
                if t.is_symlink() || t.is_hard_link() {
                    let target = e.link_name().ok().flatten().ok_or_else(|| err("bad link"))?;
                    if !link_stays_inside(&rel, &target) { return Err(err("The package has a link outside itself; refusing it.")); }
                }
                if !(t.is_file() || t.is_dir() || t.is_symlink() || t.is_hard_link()) { continue; }
                let out = dest.join(&rel);
                if let Some(parent) = out.parent() { std::fs::create_dir_all(parent)?; }
                e.unpack(&out).map_err(|e| err(format!("Could not unpack: {e}")))?;
            }
            Ok(())
        }
        other => Err(err(format!("Unknown package kind \"{other}\"."))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test] fn manifest_is_complete_and_wellformed() {
        for (name, e) in &manifest().engines {
            assert!(engines::tool(name).is_some(), "{name} is not a known tool");
            for (plat, p) in &e.platforms {
                assert!(["linux-x64", "linux-arm64", "windows-x64", "macos-x64", "macos-arm64"].contains(&plat.as_str()), "{plat}");
                assert!(p.url.starts_with("https://"), "{name}/{plat}: https only");
                assert_eq!(p.sha256.len(), 64, "{name}/{plat}");
                assert!(p.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
                assert!(p.size > 0 && ["zip", "tar.gz", "tar.xz", "file"].contains(&p.kind.as_str()));
                assert!(p.kind != "file" || p.file_name.is_some(), "{name}/{plat}");
            }
        }
        assert!(manifest().engines.contains_key("pandoc"));
    }
    #[test] fn platform_name_shape() { let p = current_platform(); assert!(p.contains('-')); }

    #[test] fn safe_relative_rejects_escapes() {
        assert!(safe_relative(Path::new("a/b")).is_some());
        for bad in ["../x", "/abs", "a/../../x", ""] { assert!(safe_relative(Path::new(bad)).is_none(), "{bad}"); }
    }

    fn tmpdir(n: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("mgr-{}-{n}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }

    fn make_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let mut z = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (n, d) in entries { z.start_file(*n, zip::write::SimpleFileOptions::default().unix_permissions(0o755)).unwrap(); z.write_all(d).unwrap(); }
        z.finish().unwrap();
    }
    #[test] fn zip_unpacks_and_sets_exec_bit() {
        let d = tmpdir("zip"); let a = d.join("a.zip");
        make_zip(&a, &[("pkg/bin/tool", b"#!/bin/sh\necho hi\n")]);
        unpack_archive(&a, &d.join("out"), "zip", None).unwrap();
        let t = d.join("out/pkg/bin/tool");
        assert!(t.is_file());
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; assert_eq!(t.metadata().unwrap().permissions().mode() & 0o111, 0o111); }
    }
    #[test] fn zip_slip_is_refused() {
        let d = tmpdir("slip"); let a = d.join("evil.zip");
        make_zip(&a, &[("../escape.txt", b"x")]);
        assert!(unpack_archive(&a, &d.join("out"), "zip", None).is_err());
        assert!(!d.join("escape.txt").exists());
    }
    #[test] fn tar_gz_with_outside_symlink_is_refused() {
        let d = tmpdir("tar"); let a = d.join("evil.tar.gz");
        let gz = flate2::write::GzEncoder::new(std::fs::File::create(&a).unwrap(), flate2::Compression::fast());
        let mut b = tar::Builder::new(gz);
        let mut h = tar::Header::new_gnu(); h.set_entry_type(tar::EntryType::Symlink); h.set_size(0);
        b.append_link(&mut h, "link", "../../etc/passwd").unwrap();
        b.into_inner().unwrap().finish().unwrap();
        assert!(unpack_archive(&a, &d.join("out"), "tar.gz", None).unwrap_err().message.contains("outside"));
    }
    #[cfg(unix)]
    #[test] fn zip_symlinks_inside_are_kept_and_outside_refused() {
        let d = tmpdir("zlink");
        let mk = |name: &str, target: &str| { let a = d.join(name); let mut z = zip::ZipWriter::new(std::fs::File::create(&a).unwrap());
            z.start_file("lib/real.so", zip::write::SimpleFileOptions::default()).unwrap(); z.write_all(b"x").unwrap();
            z.add_symlink("lib/link.so", target, zip::write::SimpleFileOptions::default()).unwrap(); z.finish().unwrap(); a };
        unpack_archive(&mk("ok.zip", "real.so"), &d.join("ok"), "zip", None).unwrap();
        assert_eq!(std::fs::read_link(d.join("ok/lib/link.so")).unwrap(), Path::new("real.so"));
        assert!(unpack_archive(&mk("bad.zip", "../../../etc/passwd"), &d.join("bad"), "zip", None).is_err());
        assert!(unpack_archive(&mk("abs.zip", "/etc/passwd"), &d.join("abs"), "zip", None).is_err());
    }
    #[test] fn tar_xz_roundtrip() {
        let d = tmpdir("xz"); let a = d.join("a.tar.xz");
        let xz = xz2::write::XzEncoder::new(std::fs::File::create(&a).unwrap(), 1);
        let mut b = tar::Builder::new(xz);
        let data = b"hello"; let mut h = tar::Header::new_gnu(); h.set_size(data.len() as u64); h.set_mode(0o755); h.set_cksum();
        b.append_data(&mut h, "dir/tool", &data[..]).unwrap();
        b.into_inner().unwrap().finish().unwrap();
        unpack_archive(&a, &d.join("out"), "tar.xz", None).unwrap();
        assert_eq!(std::fs::read(d.join("out/dir/tool")).unwrap(), b"hello");
    }

    /// Serves `body` once per connection on 127.0.0.1; returns the URL.
    fn serve(body: Vec<u8>) -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || for s in l.incoming() {
            let Ok(mut s) = s else { break };
            let mut buf = [0u8; 2048]; let _ = std::io::Read::read(&mut s, &mut buf);
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
            let _ = s.write_all(&body);
        });
        format!("http://127.0.0.1:{port}/pkg")
    }
    fn sha(b: &[u8]) -> String { Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect() }
    fn cfg_in(d: &Path) -> Config { Config { engines_dir: d.join("engines"), ..Config::default() } }
    fn entry(url: String, body: &[u8]) -> PlatformEntry { PlatformEntry { url, sha256: sha(body), size: body.len() as u64, kind: "zip".into(), file_name: None } }

    fn pandoc_zip(d: &Path, content: &[u8]) -> Vec<u8> { let a = d.join("p.zip"); make_zip(&a, &[("pandoc-9/bin/pandoc", content)]); std::fs::read(a).unwrap() }

    #[tokio::test] async fn installs_verified_package_and_resolver_finds_it() {
        let d = tmpdir("inst"); let body = pandoc_zip(&d, b"v1");
        let cfg = cfg_in(&d);
        let seen = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
        let s2 = seen.clone();
        install_entry(&cfg, "pandoc", &entry(serve(body.clone()), &body), std::sync::Arc::new(move |p| s2.lock().unwrap().push(p))).await.unwrap();
        let found = engines::resolve(engines::tool("pandoc").unwrap(), &cfg, None).unwrap();
        assert!(found.starts_with(cfg.engines_dir.join("pandoc")), "{found:?}");
        let events = seen.lock().unwrap();
        assert!(events.contains(&Progress::Stage("Verifying")) && events.contains(&Progress::Finished));
        assert!(std::fs::read_dir(&cfg.engines_dir).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with('.')), "no staging leftovers");
    }
    #[tokio::test] async fn tampered_download_is_rejected_and_old_install_survives() {
        let d = tmpdir("tamper"); let good = pandoc_zip(&d, b"GOOD"); let cfg = cfg_in(&d);
        install_entry(&cfg, "pandoc", &entry(serve(good.clone()), &good), crate::progress::noop()).await.unwrap();
        let evil = pandoc_zip(&d, b"EVIL");
        // same size as what we pin, different bytes → checksum must catch it
        let mut pinned = entry(serve(evil.clone()), &good); pinned.size = evil.len() as u64;
        let e = install_entry(&cfg, "pandoc", &pinned, crate::progress::noop()).await.unwrap_err();
        assert!(e.message.contains("checksum"), "{}", e.message);
        let bin = engines::resolve(engines::tool("pandoc").unwrap(), &cfg, None).unwrap();
        assert_eq!(std::fs::read(bin).unwrap(), b"GOOD", "working copy must be untouched");
        assert!(std::fs::read_dir(&cfg.engines_dir).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with('.')));
    }
    #[tokio::test] async fn oversized_download_is_refused() {
        let d = tmpdir("big"); let body = pandoc_zip(&d, b"x"); let cfg = cfg_in(&d);
        let mut e = entry(serve(body.clone()), &body); e.size = 10;
        assert!(install_entry(&cfg, "pandoc", &e, crate::progress::noop()).await.unwrap_err().message.contains("larger"));
    }
    #[tokio::test] async fn package_without_the_program_is_refused() {
        let d = tmpdir("wrong"); let a = d.join("w.zip"); make_zip(&a, &[("readme.txt", b"hi")]); let body = std::fs::read(a).unwrap();
        let cfg = cfg_in(&d);
        let e = install_entry(&cfg, "pandoc", &entry(serve(body.clone()), &body), crate::progress::noop()).await.unwrap_err();
        assert!(e.message.contains("expected program"));
        assert!(!cfg.engines_dir.join("pandoc").exists());
    }
    #[tokio::test] async fn remove_deletes_the_install() {
        let d = tmpdir("rm"); let body = pandoc_zip(&d, b"v"); let cfg = cfg_in(&d);
        install_entry(&cfg, "pandoc", &entry(serve(body.clone()), &body), crate::progress::noop()).await.unwrap();
        remove(&cfg, "pandoc").await.unwrap();
        assert!(!cfg.engines_dir.join("pandoc").exists());
        assert!(remove(&cfg, "nope").await.is_err());
    }
}
