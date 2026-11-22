//! Built-in readers: open a file's own bytes and produce the neutral model.
pub mod docx;
pub mod pptx;
pub mod sheet;

use crate::{Error, ErrorCode};
use quick_xml::events::attributes::Attributes;
use std::io::Read;

pub fn bad(msg: impl Into<String>) -> Error { Error::new(ErrorCode::EConvertFailed, msg) }

/// Attribute value by local name (`w:val` and `val` both match `val`).
pub fn attr(attrs: Attributes, name: &str) -> Option<String> {
    for a in attrs.flatten() {
        let key = a.key.local_name();
        if key.as_ref() == name.as_bytes() {
            return a.unescape_value().ok().map(|v| v.into_owned());
        }
    }
    None
}

/// Reads one zip entry with a hard cap on what is inflated (decompression-bomb bound).
pub fn read_part<R: Read + std::io::Seek>(zip: &mut zip::ZipArchive<R>, name: &str, max: u64) -> crate::Result<Option<Vec<u8>>> {
    let entry = match zip.by_name(name) { Ok(e) => e, Err(_) => return Ok(None) };
    if entry.size() > max {
        return Err(Error::new(ErrorCode::ETooLarge, format!("{name} declares {} bytes, over the {max} byte ceiling", entry.size())));
    }
    let mut buf = vec![];
    entry.take(max + 1).read_to_end(&mut buf).map_err(|e| bad(format!("cannot read {name}: {e}")))?;
    if buf.len() as u64 > max {
        return Err(Error::new(ErrorCode::ETooLarge, format!("{name} inflates past the {max} byte ceiling")));
    }
    Ok(Some(buf))
}

pub fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "png" => "image/png", "jpg" | "jpeg" | "jpe" => "image/jpeg", "gif" => "image/gif", "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff", "webp" => "image/webp", "svg" => "image/svg+xml", "emf" => "image/emf", "wmf" => "image/wmf",
        _ => "application/octet-stream",
    }
}

/// Resolves `../media/x.png` style targets against the part's folder.
pub fn resolve_target(base_dir: &str, target: &str) -> String {
    if let Some(t) = target.strip_prefix('/') { return t.to_string(); }
    let mut parts: Vec<&str> = base_dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in target.split('/') {
        match seg { "" | "." => {}, ".." => { parts.pop(); }, s => parts.push(s) }
    }
    parts.join("/")
}
