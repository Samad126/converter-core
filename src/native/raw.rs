//! Camera RAW files (DNG, CR2, NEF, ARW, ORF, RW2, PEF, SRW, ...): the full-size JPEG preview every one of them
//! embeds. Decoding the sensor data itself needs a RAW developer (LibreRaw/dcraw); the preview is what most people
//! want from a conversion, and it needs nothing but a TIFF walker.
use crate::{Error, ErrorCode, Result};

pub const EXTENSIONS: &[&str] = &[".dng", ".cr2", ".nef", ".arw", ".orf", ".rw2", ".pef", ".srw", ".raf", ".mrw", ".erf", ".3fr", ".kdc", ".mef", ".nrw", ".sr2"];

fn bad(msg: impl Into<String>) -> Error { Error::new(ErrorCode::EConvertFailed, msg) }

struct Tiff<'a> { d: &'a [u8], le: bool }

impl Tiff<'_> {
    fn u16(&self, o: usize) -> Option<u16> { self.d.get(o..o + 2).map(|b| if self.le { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) }) }
    fn u32(&self, o: usize) -> Option<u32> { self.d.get(o..o + 4).map(|b| if self.le { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) }) }
}

#[derive(Default, Clone, Copy)]
struct Entry { tag: u16, ty: u16, count: u32, value_at: usize }

fn type_size(ty: u16) -> usize { match ty { 1 | 2 | 6 | 7 => 1, 3 | 8 => 2, 4 | 9 | 11 | 13 => 4, 5 | 10 | 12 => 8, _ => 1 } }

fn entries(t: &Tiff, ifd: usize) -> (Vec<Entry>, Option<usize>) {
    let Some(n) = t.u16(ifd) else { return (vec![], None) };
    let n = (n as usize).min(512);
    let mut out = vec![];
    for i in 0..n {
        let e = ifd + 2 + i * 12;
        let (Some(tag), Some(ty), Some(count)) = (t.u16(e), t.u16(e + 2), t.u32(e + 4)) else { break };
        let size = type_size(ty).saturating_mul(count as usize);
        let value_at = if size <= 4 { e + 8 } else { t.u32(e + 8).unwrap_or(0) as usize };
        out.push(Entry { tag, ty, count, value_at });
    }
    let next = t.u32(ifd + 2 + n * 12).filter(|o| *o != 0).map(|o| o as usize);
    (out, next)
}

fn value(t: &Tiff, e: &Entry) -> Option<u32> { if type_size(e.ty) == 2 { t.u16(e.value_at).map(u32::from) } else { t.u32(e.value_at) } }

fn collect(t: &Tiff, ifd: usize, depth: u32, seen: &mut Vec<usize>, out: &mut Vec<(usize, usize)>) {
    if depth > 4 || seen.contains(&ifd) || seen.len() > 64 { return; }
    seen.push(ifd);
    let (es, next) = entries(t, ifd);
    let get = |tag: u16| es.iter().find(|e| e.tag == tag).copied();
    // JPEGInterchangeFormat + Length (thumbnail/preview IFDs, CR2, NEF, ARW, ORF, PEF).
    if let (Some(o), Some(l)) = (get(0x201).and_then(|e| value(t, &e)), get(0x202).and_then(|e| value(t, &e))) { out.push((o as usize, l as usize)); }
    // Compression 6/7 with one strip: DNG/NEF preview IFDs.
    if matches!(get(0x103).and_then(|e| value(t, &e)), Some(6) | Some(7)) {
        if let (Some(o), Some(l)) = (get(0x111), get(0x117)) {
            if o.count == 1 { if let (Some(o), Some(l)) = (value(t, &o), value(t, &l)) { out.push((o as usize, l as usize)); } }
        }
    }
    // Panasonic RW2: JpgFromRaw (0x2E) holds the JPEG in place.
    if let Some(e) = get(0x2E) { if e.ty == 7 { out.push((e.value_at, e.count as usize)); } }
    // SubIFDs (0x14A) and Exif (0x8769) can hold previews too.
    for tag in [0x14Au16, 0x8769] {
        if let Some(e) = get(tag) {
            let n = if e.ty == 4 { e.count.min(8) as usize } else { 1 };
            for k in 0..n { if let Some(o) = t.u32(e.value_at + 4 * k) { collect(t, o as usize, depth + 1, seen, out); } }
        }
    }
    if let Some(n) = next { collect(t, n, depth + 1, seen, out); }
}

/// The largest embedded JPEG preview.
pub fn preview(data: &[u8]) -> Result<Vec<u8>> {
    // Header: "II"/"MM", magic 42 (RW2 uses 0x55, ORF 0x4F52/0x5352), offset of IFD0. CR2 adds "CR" at 8.
    let le = match data.get(0..2) { Some(b"II") => true, Some(b"MM") => false, _ if data.starts_with(b"FUJIFILMCCD-RAW") => return fuji_preview(data), _ => return Err(bad("This is not a TIFF-based RAW file.")) };
    let t = Tiff { d: data, le };
    let ifd0 = t.u32(4).ok_or_else(|| bad("The RAW file is truncated."))? as usize;
    let (mut found, mut seen) = (vec![], vec![]);
    collect(&t, ifd0, 0, &mut seen, &mut found);
    let best = found.into_iter()
        .filter_map(|(o, l)| data.get(o..o.checked_add(l)?).filter(|b| b.starts_with(&[0xFF, 0xD8, 0xFF])).map(|b| b.to_vec()))
        .max_by_key(|b| b.len());
    best.ok_or_else(|| bad("No embedded JPEG preview was found in this RAW file."))
}

/// Fujifilm RAF: a fixed header points at the embedded JPEG.
fn fuji_preview(data: &[u8]) -> Result<Vec<u8>> {
    let be = |o: usize| data.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let (off, len) = (be(84).ok_or_else(|| bad("The RAF file is truncated."))?, be(88).unwrap_or(0));
    data.get(off..off.checked_add(len).ok_or_else(|| bad("The RAF file is damaged."))?).filter(|b| b.starts_with(&[0xFF, 0xD8])).map(|b| b.to_vec()).ok_or_else(|| bad("No embedded JPEG preview was found in this RAW file."))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tiny_jpeg(w: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(w, w, image::Rgb([200, 100, 50]));
        let mut c = std::io::Cursor::new(vec![]);
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut c, 80).encode_image(&img).unwrap();
        c.into_inner()
    }
    /// A DNG-shaped file: IFD0 with a SubIFD that holds a JPEG-compressed strip, plus a small thumbnail via 0x201/0x202.
    fn fake(le: bool) -> (Vec<u8>, Vec<u8>) {
        let (thumb, big) = (tiny_jpeg(8), tiny_jpeg(64));
        let w16 = |v: u16| if le { v.to_le_bytes() } else { v.to_be_bytes() };
        let w32 = |v: u32| if le { v.to_le_bytes() } else { v.to_be_bytes() };
        let mut d = if le { b"II".to_vec() } else { b"MM".to_vec() };
        d.extend(w16(42)); d.extend(w32(8));
        // IFD0 at 8: 2 entries -> thumbnail offset/length (0x201/0x202)... plus SubIFDs (0x14A). Layout computed below.
        let ifd0_len = 2 + 3 * 12 + 4;
        let sub_at = 8 + ifd0_len;
        let sub_len = 2 + 3 * 12 + 4;
        let thumb_at = sub_at + sub_len;
        let big_at = thumb_at + thumb.len();
        d.extend(w16(3));
        for (tag, ty, val) in [(0x14Au16, 4u16, sub_at as u32), (0x201, 4, thumb_at as u32), (0x202, 4, thumb.len() as u32)] { d.extend(w16(tag)); d.extend(w16(ty)); d.extend(w32(1)); d.extend(w32(val)); }
        d.extend(w32(0));
        d.extend(w16(3));
        for (tag, ty, val) in [(0x103u16, 3u16, 7u32), (0x111, 4, big_at as u32), (0x117, 4, big.len() as u32)] {
            d.extend(w16(tag)); d.extend(w16(ty)); d.extend(w32(1));
            if ty == 3 { d.extend(w16(val as u16)); d.extend([0, 0]); } else { d.extend(w32(val)); }
        }
        d.extend(w32(0));
        d.extend(&thumb); d.extend(&big);
        (d, big)
    }
    #[test] fn the_largest_preview_wins_in_both_byte_orders() {
        for le in [true, false] {
            let (raw, big) = fake(le);
            assert_eq!(preview(&raw).unwrap(), big, "little-endian: {le}");
        }
    }
    #[test] fn non_raw_and_previewless_files_are_refused() {
        assert!(preview(b"hello world, definitely not a raw file").is_err());
        let mut t = b"II*\0\x08\0\0\0".to_vec(); t.extend([0, 0, 0, 0, 0, 0]);
        assert!(preview(&t).unwrap_err().message.contains("No embedded JPEG"));
    }
}
