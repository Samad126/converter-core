//! Built-in readers: open a file's own bytes and produce the neutral model.
pub mod docx;
pub mod html;
pub mod mht;
pub mod odt;
pub mod hwp;
pub mod hwpx;
pub mod pdf_text;
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

/// Arranges cells given by address `(row, col, row_span, col_span, cell)` into rows of the model's table: positions
/// covered by a cell that spans rows from above get an empty placeholder so columns stay aligned.
pub fn grid_to_rows(mut cells: Vec<(usize, usize, usize, usize, crate::native::model::Cell)>) -> Vec<Vec<crate::native::model::Cell>> {
    use crate::native::model::Cell;
    cells.sort_by_key(|c| (c.0, c.1));
    let n_rows = cells.iter().map(|c| c.0 + 1).max().unwrap_or(0).min(10_000);
    let mut covered: std::collections::HashSet<(usize, usize)> = Default::default();
    let mut rows: Vec<Vec<Cell>> = vec![vec![]; n_rows];
    let mut next_col = vec![0usize; n_rows];
    for (r, c, rs, cs, mut cell) in cells {
        if r >= n_rows || c > 4096 { continue; }
        // Fill any gap left by an overhead row span (or a missing cell) with empty cells.
        while next_col[r] < c {
            let col = next_col[r];
            rows[r].push(Cell { blocks: vec![], col_span: 1 });
            next_col[r] = col + 1;
        }
        cell.col_span = cs.max(1);
        for dr in 1..rs.max(1) { for dc in 0..cs.max(1) { covered.insert((r + dr, c + dc)); } }
        next_col[r] = c + cs.max(1);
        rows[r].push(cell);
    }
    // Row spans: a spanned-over position in a lower row is an empty cell in the right place.
    for r in 0..n_rows {
        let mut placed: Vec<(usize, Cell)> = vec![];
        let mut col = 0usize;
        for cell in std::mem::take(&mut rows[r]) {
            while covered.contains(&(r, col)) { placed.push((col, Cell { blocks: vec![], col_span: 1 })); col += 1; }
            let span = cell.col_span.max(1);
            placed.push((col, cell));
            col += span;
        }
        while covered.contains(&(r, col)) { placed.push((col, Cell { blocks: vec![], col_span: 1 })); col += 1; }
        rows[r] = placed.into_iter().map(|(_, c)| c).collect();
    }
    rows
}
