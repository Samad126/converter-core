//! Archives without 7-Zip: ZIP/CBZ, TAR, TAR.GZ and single-file GZ/XZ/LZMA/LZ4 streams in, ZIP/CBZ/TAR/TAR.GZ out.
//! Unpacking untrusted bytes is the hazard, so the same checks as the 7-Zip route run BEFORE anything is kept:
//! entry count, declared and actual size, encryption, symlinks and path traversal.
use crate::pipelines::archive::{validate_entries, Entry};
use crate::{Config, Error, ErrorCode, Result};
use std::io::{Read, Write};

fn fail(msg: impl Into<String>) -> Error { Error::new(ErrorCode::EConvertFailed, msg) }

#[derive(Debug)]
pub struct Unpacked { pub files: Vec<(String, Vec<u8>)> }

fn take_all(mut r: impl Read, limit: u64) -> Result<Vec<u8>> {
    let mut out = vec![];
    (&mut r).take(limit + 1).read_to_end(&mut out).map_err(|e| fail(format!("cannot read the archive: {e}")))?;
    if out.len() as u64 > limit { return Err(Error::new(ErrorCode::ETooLarge, "The archive expands past the size limit.")); }
    Ok(out)
}

fn sniff_tar(d: &[u8]) -> bool { d.get(257..262) == Some(b"ustar") }

fn from_tar(data: &[u8], cfg: &Config) -> Result<Unpacked> {
    let mut ar = tar::Archive::new(data);
    let (mut entries, mut files) = (vec![], vec![]);
    let mut total = 0u64;
    for e in ar.entries().map_err(|e| fail(format!("not a readable TAR archive: {e}")))? {
        let mut e = e.map_err(|e| fail(format!("damaged TAR entry: {e}")))?;
        let ty = e.header().entry_type();
        let path = e.path().map_err(|e| fail(e.to_string()))?.to_string_lossy().into_owned();
        let is_link = ty.is_symlink() || ty.is_hard_link();
        entries.push(Entry { path: path.clone(), is_folder: ty.is_dir(), size: e.size(), attributes: if is_link { "l".into() } else { String::new() }, encrypted: false });
        if entries.len() > cfg.max_archive_entries { break; }
        if ty.is_file() {
            total = total.saturating_add(e.size());
            if total > cfg.max_archive_uncompressed_bytes { break; }
            let mut buf = vec![];
            e.read_to_end(&mut buf).map_err(|e| fail(e.to_string()))?;
            files.push((path, buf));
        }
    }
    validate_entries(&entries, cfg)?;
    Ok(Unpacked { files })
}

fn from_zip(data: &[u8], cfg: &Config) -> Result<Unpacked> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|e| fail(format!("not a readable ZIP archive: {e}")))?;
    let mut entries = vec![];
    for i in 0..z.len().min(cfg.max_archive_entries + 1) {
        let f = z.by_index_raw(i).map_err(|e| fail(format!("damaged ZIP entry: {e}")))?;
        let unix_mode = f.unix_mode().unwrap_or(0);
        entries.push(Entry { path: f.name().to_string(), is_folder: f.is_dir(), size: f.size(), attributes: if unix_mode & 0o170000 == 0o120000 { "l".into() } else { String::new() }, encrypted: f.encrypted() });
    }
    if z.len() > cfg.max_archive_entries { entries.push(Entry::default()); }
    validate_entries(&entries, cfg)?;
    let mut files = vec![];
    for i in 0..z.len() {
        let f = z.by_index(i).map_err(|e| fail(format!("cannot unpack a ZIP entry (only stored and deflated entries are supported without 7-Zip): {e}")))?;
        if f.is_dir() { continue; }
        let name = f.name().to_string();
        files.push((name, take_all(f, cfg.max_archive_uncompressed_bytes)?));
    }
    Ok(Unpacked { files })
}

/// Unpacks by source extension. A single compressed stream (`.gz`, `.xz`, ...) is one file, or a TAR when it holds one.
pub fn unpack(ext: &str, name_stem: &str, data: &[u8], cfg: &Config) -> Result<Unpacked> {
    let limit = cfg.max_archive_uncompressed_bytes;
    let stream = match ext {
        ".zip" | ".cbz" | ".zipx" => return from_zip(data, cfg),
        ".tar" => return from_tar(data, cfg),
        ".gz" | ".tgz" => take_all(flate2::read::MultiGzDecoder::new(data), limit)?,
        ".xz" | ".txz" => take_all(xz2::read::XzDecoder::new(data), limit)?,
        ".lzma" => take_all(xz2::read::XzDecoder::new_stream(data, xz2::stream::Stream::new_lzma_decoder(u64::MAX).map_err(|e| fail(e.to_string()))?), limit)?,
        ".lz4" => take_all(lz4_flex::frame::FrameDecoder::new(data), limit)?,
        other => return Err(fail(format!("\"{other}\" archives need 7-Zip."))),
    };
    if sniff_tar(&stream) { return from_tar(&stream, cfg); }
    Ok(Unpacked { files: vec![(name_stem.to_string(), stream)] })
}

pub fn pack(target: &str, files: &[(String, Vec<u8>)], out: &std::path::Path) -> Result<()> {
    let zerr = |e: zip::result::ZipError| fail(e.to_string());
    match target {
        "zip" | "cbz" => {
            let mut z = zip::ZipWriter::new(std::fs::File::create(out)?);
            let mut sorted: Vec<&(String, Vec<u8>)> = files.iter().collect();
            if target == "cbz" { sorted.sort_by(|a, b| a.0.cmp(&b.0)); }
            for (name, data) in sorted {
                z.start_file(name.replace('\\', "/"), zip::write::SimpleFileOptions::default()).map_err(zerr)?;
                z.write_all(data)?;
            }
            z.finish().map_err(zerr)?;
        }
        "tar" | "tar.gz" => {
            let file = std::fs::File::create(out)?;
            let sink: Box<dyn Write> = if target == "tar.gz" { Box::new(flate2::write::GzEncoder::new(file, flate2::Compression::default())) } else { Box::new(file) };
            let mut b = tar::Builder::new(sink);
            for (name, data) in files {
                let mut h = tar::Header::new_gnu();
                h.set_size(data.len() as u64); h.set_mode(0o644); h.set_mtime(0);
                b.append_data(&mut h, name.replace('\\', "/"), data.as_slice()).map_err(|e| fail(e.to_string()))?;
            }
            b.into_inner().map_err(|e| fail(e.to_string()))?.flush()?;
        }
        other => return Err(fail(format!("\"{other}\" output needs 7-Zip."))),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn files() -> Vec<(String, Vec<u8>)> { vec![("a.txt".into(), b"hello".to_vec()), ("dir/b.bin".into(), (0..=255u8).collect())] }
    fn tmp(n: &str) -> std::path::PathBuf { std::env::temp_dir().join(format!("arch-{}-{n}", std::process::id())) }
    #[test] fn every_output_reads_back_through_every_input() {
        let cfg = Config::default();
        for target in ["zip", "tar", "tar.gz"] {
            let p = tmp(target); pack(target, &files(), &p).unwrap();
            let ext = match target { "tar.gz" => ".gz", t => if t == "zip" { ".zip" } else { ".tar" } };
            let back = unpack(ext, "x", &std::fs::read(&p).unwrap(), &cfg).unwrap();
            assert_eq!(back.files, files(), "{target}");
        }
    }
    #[test] fn single_streams_become_one_file() {
        let cfg = Config::default();
        let mut gz = flate2::write::GzEncoder::new(vec![], flate2::Compression::default()); gz.write_all(b"payload").unwrap();
        assert_eq!(unpack(".gz", "notes.txt", &gz.finish().unwrap(), &cfg).unwrap().files, vec![("notes.txt".to_string(), b"payload".to_vec())]);
        let mut lz = lz4_flex::frame::FrameEncoder::new(vec![]); lz.write_all(b"payload").unwrap();
        assert_eq!(unpack(".lz4", "n", &lz.finish().unwrap(), &cfg).unwrap().files[0].1, b"payload");
        let mut xz = xz2::write::XzEncoder::new(vec![], 3); xz.write_all(b"payload").unwrap();
        assert_eq!(unpack(".xz", "n", &xz.finish().unwrap(), &cfg).unwrap().files[0].1, b"payload");
    }
    #[test] fn traversal_symlinks_and_bombs_are_refused() {
        let cfg = Config::default();
        let p = tmp("evil"); pack("zip", &[("../evil.txt".to_string(), b"x".to_vec())], &p).unwrap();
        assert!(unpack(".zip", "x", &std::fs::read(&p).unwrap(), &cfg).unwrap_err().message.contains("unsafe path"));
        let small = Config { max_archive_uncompressed_bytes: 100, ..Config::default() };
        let p = tmp("big"); pack("zip", &[("a".to_string(), vec![0u8; 5000])], &p).unwrap();
        assert!(unpack(".zip", "x", &std::fs::read(&p).unwrap(), &small).is_err());
        let mut b = tar::Builder::new(vec![]);
        let mut h = tar::Header::new_gnu(); h.set_entry_type(tar::EntryType::Symlink); h.set_size(0); h.set_link_name("/etc/passwd").unwrap(); h.set_cksum();
        b.append_data(&mut h, "link", std::io::empty()).unwrap();
        assert!(unpack(".tar", "x", &b.into_inner().unwrap(), &cfg).unwrap_err().message.contains("symlink"));
    }
}
