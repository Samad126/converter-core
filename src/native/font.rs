//! Web-font containers without Python: WOFF 1.0 ↔ SFNT (TrueType/OpenType) and EOT → SFNT. WOFF2 needs Brotli and
//! TrueType ↔ OpenType needs outline conversion, so those still go through the Python font engine.
use crate::{Error, ErrorCode, Result};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use std::io::{Read, Write};

fn bad(msg: impl Into<String>) -> Error { Error::new(ErrorCode::EConvertFailed, msg) }
fn u16be(d: &[u8], o: usize) -> Result<u16> { d.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]])).ok_or_else(|| bad("The font file is truncated.")) }
fn u32be(d: &[u8], o: usize) -> Result<u32> { d.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]])).ok_or_else(|| bad("The font file is truncated.")) }
fn pad4(n: usize) -> usize { (4 - n % 4) % 4 }

const MAX_TABLE: usize = 64 * 1024 * 1024;

/// Which outline format an SFNT holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor { TrueType, Cff }

pub fn flavor_of(sfnt: &[u8]) -> Result<Flavor> {
    match sfnt.get(0..4) {
        Some([0, 1, 0, 0]) | Some(b"true") => Ok(Flavor::TrueType),
        Some(b"OTTO") => Ok(Flavor::Cff),
        _ => Err(bad("This is not a TrueType or OpenType font.")),
    }
}

struct Table { tag: [u8; 4], checksum: u32, data: Vec<u8> }

fn sfnt_header(flavor: [u8; 4], n: u16) -> Vec<u8> {
    let mut entry_selector = 0u16;
    while (1u32 << (entry_selector + 1)) <= n as u32 { entry_selector += 1; }
    let search_range = (1u16 << entry_selector) * 16;
    let mut h = flavor.to_vec();
    for v in [n, search_range, entry_selector, n * 16 - search_range] { h.extend(v.to_be_bytes()); }
    h
}

fn build_sfnt(flavor: [u8; 4], tables: &[Table]) -> Vec<u8> {
    let mut out = sfnt_header(flavor, tables.len() as u16);
    let mut offset = 12 + 16 * tables.len();
    for t in tables {
        out.extend(t.tag);
        out.extend(t.checksum.to_be_bytes());
        out.extend((offset as u32).to_be_bytes());
        out.extend((t.data.len() as u32).to_be_bytes());
        offset += t.data.len() + pad4(t.data.len());
    }
    for t in tables { out.extend(&t.data); out.extend(vec![0u8; pad4(t.data.len())]); }
    out
}

fn checksum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    for c in data.chunks(4) { let mut b = [0u8; 4]; b[..c.len()].copy_from_slice(c); sum = sum.wrapping_add(u32::from_be_bytes(b)); }
    sum
}

pub fn woff_to_sfnt(woff: &[u8]) -> Result<Vec<u8>> {
    if woff.get(0..4) != Some(b"wOFF") { return Err(bad("This is not a WOFF font.")); }
    let flavor = [woff[4], woff[5], woff[6], woff[7]];
    let n = u16be(woff, 12)? as usize;
    let mut tables = vec![];
    for i in 0..n {
        let e = 44 + i * 20;
        let tag = [*woff.get(e).ok_or_else(|| bad("The font file is truncated."))?, woff[e + 1], woff[e + 2], woff[e + 3]];
        let (off, comp, orig, sum) = (u32be(woff, e + 4)? as usize, u32be(woff, e + 8)? as usize, u32be(woff, e + 12)? as usize, u32be(woff, e + 16)?);
        if orig > MAX_TABLE { return Err(Error::new(ErrorCode::ETooLarge, "A font table is too large.")); }
        let raw = woff.get(off..off.checked_add(comp).ok_or_else(|| bad("The font file is damaged."))?).ok_or_else(|| bad("The font file is truncated."))?;
        let data = if comp < orig {
            let mut d = Vec::with_capacity(orig);
            ZlibDecoder::new(raw).take(orig as u64 + 1).read_to_end(&mut d).map_err(|e| bad(format!("A font table cannot be decompressed: {e}")))?;
            if d.len() != orig { return Err(bad("A font table has the wrong size after decompression.")); }
            d
        } else { raw.to_vec() };
        tables.push(Table { tag, checksum: sum, data });
    }
    tables.sort_by_key(|t| t.tag);
    Ok(build_sfnt(flavor, &tables))
}

pub fn sfnt_to_woff(sfnt: &[u8]) -> Result<Vec<u8>> {
    flavor_of(sfnt)?;
    let n = u16be(sfnt, 4)? as usize;
    let mut tables: Vec<(Table, Vec<u8>)> = vec![];
    for i in 0..n {
        let e = 12 + i * 16;
        let tag = [*sfnt.get(e).ok_or_else(|| bad("The font file is truncated."))?, sfnt[e + 1], sfnt[e + 2], sfnt[e + 3]];
        let (sum, off, len) = (u32be(sfnt, e + 4)?, u32be(sfnt, e + 8)? as usize, u32be(sfnt, e + 12)? as usize);
        let data = sfnt.get(off..off.checked_add(len).ok_or_else(|| bad("The font file is damaged."))?).ok_or_else(|| bad("The font file is truncated."))?.to_vec();
        let mut z = ZlibEncoder::new(vec![], Compression::best());
        z.write_all(&data)?;
        let packed = z.finish()?;
        tables.push((Table { tag, checksum: sum, data }, packed));
    }
    tables.sort_by_key(|(t, _)| t.tag);
    let total_sfnt = 12 + 16 * n + tables.iter().map(|(t, _)| t.data.len() + pad4(t.data.len())).sum::<usize>();
    let mut dir = vec![];
    let mut body = vec![];
    let mut offset = 44 + 20 * n;
    for (t, packed) in &tables {
        let stored: &[u8] = if packed.len() < t.data.len() { packed } else { &t.data };
        dir.extend(t.tag);
        for v in [offset as u32, stored.len() as u32, t.data.len() as u32, t.checksum] { dir.extend(v.to_be_bytes()); }
        body.extend(stored);
        body.extend(vec![0u8; pad4(stored.len())]);
        offset += stored.len() + pad4(stored.len());
    }
    let mut out = b"wOFF".to_vec();
    out.extend(&sfnt[0..4]);
    out.extend((offset as u32).to_be_bytes());
    out.extend((n as u16).to_be_bytes());
    out.extend(0u16.to_be_bytes());
    out.extend((total_sfnt as u32).to_be_bytes());
    out.extend([0, 1, 0, 0]);
    out.extend([0u8; 20]); // metadata and private data: none
    out.extend(dir);
    out.extend(body);
    Ok(out)
}

/// Embedded OpenType: a header followed by the font; only the uncompressed (optionally XOR-obfuscated) kind.
pub fn eot_to_sfnt(eot: &[u8]) -> Result<Vec<u8>> {
    let le = |o: usize| eot.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize).ok_or_else(|| bad("The EOT file is truncated."));
    let (size, data_size, flags) = (le(0)?, le(4)?, le(12)?);
    if size != eot.len() || data_size == 0 || data_size > size { return Err(bad("This is not a valid EOT font.")); }
    if flags & 0x4 != 0 { return Err(bad("This EOT font uses MicroType Express compression, which is not supported.")); }
    let mut data = eot[size - data_size..].to_vec();
    if flags & 0x1000_0000 != 0 { for b in &mut data { *b ^= 0x50; } }
    flavor_of(&data)?;
    Ok(data)
}

pub fn convert(source_ext: &str, target: &str, input: &[u8]) -> Result<Vec<u8>> {
    let sfnt = match source_ext {
        ".woff" => woff_to_sfnt(input)?,
        ".eot" => eot_to_sfnt(input)?,
        ".ttf" | ".otf" => { flavor_of(input)?; input.to_vec() }
        other => return Err(bad(format!("\"{other}\" is not a font this converter reads without Python."))),
    };
    let flavor = flavor_of(&sfnt)?;
    match target {
        "woff" => sfnt_to_woff(&sfnt),
        "ttf" if flavor == Flavor::TrueType => Ok(sfnt),
        "otf" if flavor == Flavor::Cff => Ok(sfnt),
        "ttf" => Err(bad("This font has CFF (PostScript) outlines; it can become OTF or WOFF without conversion, but TTF needs an outline conversion that the built-in converter does not do.")),
        "otf" => Err(bad("This font has TrueType outlines; it can become TTF or WOFF without conversion, but OTF needs an outline conversion that the built-in converter does not do.")),
        other => Err(bad(format!("No built-in font conversion to \"{other}\"."))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn dejavu() -> Vec<u8> { include_bytes!("../../assets/fonts/LiberationMono-Regular.ttf").to_vec() }
    fn tables(sfnt: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        let n = u16be(sfnt, 4).unwrap() as usize;
        (0..n).map(|i| { let e = 12 + i * 16; let (off, len) = (u32be(sfnt, e + 8).unwrap() as usize, u32be(sfnt, e + 12).unwrap() as usize); ([sfnt[e], sfnt[e + 1], sfnt[e + 2], sfnt[e + 3]], sfnt[off..off + len].to_vec()) }).collect()
    }
    #[test] fn ttf_woff_ttf_keeps_every_table_and_the_font_still_parses() {
        let ttf = dejavu();
        let woff = sfnt_to_woff(&ttf).unwrap();
        assert!(woff.starts_with(b"wOFF") && woff.len() < ttf.len(), "compressed: {} < {}", woff.len(), ttf.len());
        let back = woff_to_sfnt(&woff).unwrap();
        assert_eq!(tables(&back), tables(&ttf));
        let face = ttf_parser::Face::parse(&back, 0).expect("the rebuilt font parses");
        assert!(face.glyph_index('A').is_some());
        // The directory is valid: every table's checksum matches its data (head excluded, as it holds the file checksum).
        for (tag, data) in tables(&back) { if &tag != b"head" { let n = u16be(&back, 4).unwrap() as usize; let e = (0..n).map(|i| 12 + i * 16).find(|e| back[*e..*e + 4] == tag).unwrap(); assert_eq!(checksum(&data), u32be(&back, e + 4).unwrap(), "{}", String::from_utf8_lossy(&tag)); } }
    }
    #[test] fn eot_with_obfuscation_unwraps_to_the_font() {
        let ttf = dejavu();
        let mut eot = vec![0u8; 82];
        eot.extend(ttf.iter().map(|b| b ^ 0x50));
        let size = eot.len() as u32;
        eot[0..4].copy_from_slice(&size.to_le_bytes());
        eot[4..8].copy_from_slice(&(ttf.len() as u32).to_le_bytes());
        eot[12..16].copy_from_slice(&0x1000_0000u32.to_le_bytes());
        assert_eq!(eot_to_sfnt(&eot).unwrap(), ttf);
        eot[12..16].copy_from_slice(&0x4u32.to_le_bytes());
        assert!(eot_to_sfnt(&eot).unwrap_err().message.contains("MicroType"));
    }
    #[test] fn flavor_mismatches_explain_themselves() {
        assert!(convert(".ttf", "otf", &dejavu()).unwrap_err().message.contains("TrueType outlines"));
        assert!(convert(".ttf", "woff", &dejavu()).is_ok());
        assert!(convert(".ttf", "woff", b"nope").is_err());
    }
}
