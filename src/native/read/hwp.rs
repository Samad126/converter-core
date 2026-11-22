//! `.hwp` (Hancom Word Processor 5.x binary: an OLE compound file of compressed record streams) → `Doc`.
//! Reads text with character styles, headings and lists (paragraph shapes, outline styles), tables (spans),
//! pictures and text boxes, and the page setup. Password-protected and distribution-only files are refused.
use super::hwpx::heading_from_style;
use super::{bad, mime_for};
use crate::native::model::*;
use crate::{Config, Error, ErrorCode, Result};
use std::io::{Read, Seek};

const TAG_BIN_DATA: u16 = 0x12;
const TAG_CHAR_SHAPE: u16 = 0x15;
const TAG_PARA_SHAPE: u16 = 0x19;
const TAG_STYLE: u16 = 0x1A;
const TAG_PARA_HEADER: u16 = 0x42;
const TAG_PARA_TEXT: u16 = 0x43;
const TAG_PARA_CHAR_SHAPE: u16 = 0x44;
const TAG_CTRL_HEADER: u16 = 0x47;
const TAG_LIST_HEADER: u16 = 0x48;
const TAG_PAGE_DEF: u16 = 0x49;
const TAG_TABLE: u16 = 0x4D;
const TAG_PICTURE: u16 = 0x55;

struct Rec { tag: u16, level: u16, data: Vec<u8> }

struct Node { rec: Rec, children: Vec<Node> }

fn u16le(d: &[u8], o: usize) -> u16 { d.get(o..o + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]])) }
fn u32le(d: &[u8], o: usize) -> u32 { d.get(o..o + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) }

fn records(data: &[u8]) -> Result<Vec<Rec>> {
    let (mut out, mut i) = (vec![], 0usize);
    while i + 4 <= data.len() {
        let h = u32le(data, i);
        i += 4;
        let (tag, level, mut size) = ((h & 0x3FF) as u16, ((h >> 10) & 0x3FF) as u16, (h >> 20) as usize);
        if size == 0xFFF { size = u32le(data, i) as usize; i += 4; }
        let end = i.checked_add(size).filter(|e| *e <= data.len()).ok_or_else(|| bad("The HWP file is damaged (a record runs past the end of its stream)."))?;
        out.push(Rec { tag, level, data: data[i..end].to_vec() });
        i = end;
    }
    Ok(out)
}

/// Builds the record tree: a record's children are the records that follow it at a deeper level.
fn tree(recs: Vec<Rec>) -> Vec<Node> {
    let mut roots: Vec<Node> = vec![];
    let mut stack: Vec<Node> = vec![];
    fn attach(stack: &mut Vec<Node>, roots: &mut Vec<Node>, n: Node) { match stack.last_mut() { Some(p) => p.children.push(n), None => roots.push(n) } }
    for rec in recs {
        while stack.last().is_some_and(|top| top.rec.level >= rec.level) {
            let done = stack.pop().unwrap();
            attach(&mut stack, &mut roots, done);
        }
        stack.push(Node { rec, children: vec![] });
    }
    while let Some(done) = stack.pop() { attach(&mut stack, &mut roots, done); }
    roots
}

#[derive(Clone, Default)]
struct CharShape { size: f32, bold: bool, italic: bool, underline: bool, color: Option<[u8; 3]> }

#[derive(Clone, Default)]
struct ParaShape { align: Align, kind: u8, level: u8 }

#[derive(Default)]
struct Info { chars: Vec<CharShape>, paras: Vec<ParaShape>, styles: Vec<String>, bins: Vec<(u16, String)> }

fn utf16(d: &[u8], o: usize) -> (String, usize) {
    let n = u16le(d, o) as usize;
    let end = (o + 2 + n * 2).min(d.len());
    let units: Vec<u16> = d[(o + 2).min(end)..end].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    (String::from_utf16_lossy(&units), end)
}

fn parse_doc_info(recs: &[Rec]) -> Info {
    let mut info = Info::default();
    for r in recs {
        let d = &r.data;
        match r.tag {
            TAG_CHAR_SHAPE => {
                let attr = u32le(d, 46);
                let c = u32le(d, 52);
                let color = [(c & 0xFF) as u8, ((c >> 8) & 0xFF) as u8, ((c >> 16) & 0xFF) as u8];
                info.chars.push(CharShape { size: u32le(d, 42) as i32 as f32 / 100.0, italic: attr & 1 != 0, bold: attr & 2 != 0, underline: (attr >> 2) & 3 != 0, color: (color != [0, 0, 0]).then_some(color) });
            }
            TAG_PARA_SHAPE => {
                let a = u32le(d, 0);
                let align = match (a >> 2) & 7 { 0 => Align::Justify, 2 => Align::Right, 3 => Align::Center, 4 | 5 => Align::Justify, _ => Align::Left };
                info.paras.push(ParaShape { align, kind: ((a >> 23) & 3) as u8, level: ((a >> 25) & 7) as u8 });
            }
            TAG_STYLE => { let (name, _) = utf16(d, 0); info.styles.push(name); }
            TAG_BIN_DATA => {
                // Property (2): low nibble is the kind (0 link, 1 embedded, 2 storage); embedded files then carry an id and extension.
                let kind = u16le(d, 0) & 0xF;
                if kind == 1 || kind == 2 { let id = u16le(d, 2); let (ext, _) = utf16(d, 4); info.bins.push((id, ext)); } else { info.bins.push((0, String::new())); }
            }
            _ => {}
        }
    }
    info
}

/// Decodes PARA_TEXT into `(wchar index, char)` pairs plus whether each extended control was seen (their count).
fn decode_text(d: &[u8]) -> Vec<(usize, char)> {
    let units: Vec<u16> = d.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let mut out = vec![];
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        match u {
            0 | 13 => i += 1,
            10 => { out.push((i, '\n')); i += 1; }
            24 => { out.push((i, '-')); i += 1; }
            30 | 31 => { out.push((i, ' ')); i += 1; }
            25..=29 => i += 1,
            9 => { out.push((i, '\t')); i += 8; }
            1..=8 | 11 | 12 | 14..=23 => i += 8,
            0xD800..=0xDBFF if i + 1 < units.len() && (0xDC00..=0xDFFF).contains(&units[i + 1]) => {
                let c = 0x10000 + (((u as u32) - 0xD800) << 10) + ((units[i + 1] as u32) - 0xDC00);
                if let Some(ch) = char::from_u32(c) { out.push((i, ch)); }
                i += 2;
            }
            _ => { if let Some(ch) = char::from_u32(u as u32) { out.push((i, ch)); } i += 1; }
        }
    }
    out
}

struct Ctx<'a> {
    info: &'a Info,
    bins: &'a dyn Fn(u16) -> Option<Image>,
    sizes: Vec<f32>,
    page: Option<PageBox>,
    cells: usize,
    max_cells: usize,
}

fn ctrl_id(d: &[u8]) -> String { d.get(0..4).map_or(String::new(), |b| [b[3], b[2], b[1], b[0]].iter().map(|c| *c as char).collect()) }

fn para(node: &Node, cx: &mut Ctx) -> Result<Vec<Block>> {
    let h = &node.rec.data;
    let (shape_id, style_id, break_type) = (u16le(h, 8) as usize, *h.get(10).unwrap_or(&0) as usize, *h.get(11).unwrap_or(&0));
    let mut text: Vec<(usize, char)> = vec![];
    let mut shapes: Vec<(u32, u32)> = vec![];
    let mut controls: Vec<&Node> = vec![];
    for c in &node.children {
        match c.rec.tag {
            TAG_PARA_TEXT => text = decode_text(&c.rec.data),
            TAG_PARA_CHAR_SHAPE => shapes = c.rec.data.chunks_exact(8).map(|b| (u32le(b, 0), u32le(b, 4))).collect(),
            TAG_CTRL_HEADER => controls.push(c),
            _ => {}
        }
    }
    let mut p = Para::default();
    let shape_at = |idx: usize| -> u32 { shapes.iter().take_while(|(pos, _)| *pos as usize <= idx).last().map_or(0, |s| s.1) };
    let mut cur: Option<(u32, String)> = None;
    let flush = |cur: &mut Option<(u32, String)>, p: &mut Para, info: &Info, sizes: &mut Vec<f32>| {
        if let Some((id, t)) = cur.take() {
            let cs = info.chars.get(id as usize).cloned().unwrap_or_default();
            if cs.size > 0.0 { sizes.push(cs.size); }
            p.runs.push(Run { text: t, bold: cs.bold, italic: cs.italic, underline: cs.underline, size_pt: (cs.size > 0.0).then_some(cs.size), color: cs.color, ..Default::default() });
        }
    };
    for (idx, ch) in &text {
        let sid = shape_at(*idx);
        match &mut cur { Some((id, t)) if *id == sid => t.push(*ch), _ => { flush(&mut cur, &mut p, cx.info, &mut cx.sizes); cur = Some((sid, ch.to_string())); } }
    }
    flush(&mut cur, &mut p, cx.info, &mut cx.sizes);

    if let Some(ps) = cx.info.paras.get(shape_id) {
        p.align = ps.align;
        match ps.kind {
            1 => p.heading = Some((ps.level + 1).clamp(1, 6)),
            2 => p.list = Some(ListInfo { ordered: true, level: ps.level.min(6) }),
            3 => p.list = Some(ListInfo { ordered: false, level: ps.level.min(6) }),
            _ => {}
        }
    }
    if let Some(name) = cx.info.styles.get(style_id) { if let Some(hl) = heading_from_style(name) { p.heading = Some(hl); p.list = None; } }

    let mut blocks = vec![];
    if break_type & 4 != 0 || break_type & 1 != 0 && false { blocks.push(Block::PageBreak); }
    if !p.runs.is_empty() { blocks.push(Block::Para(p)); }
    for c in controls { blocks.extend(control(c, cx)?); }
    if blocks.is_empty() { blocks.push(Block::Para(Para::default())); }
    Ok(blocks)
}

/// Paragraphs among a node's children, descending into containers (text boxes keep their text).
fn paragraphs_in(children: &[Node], cx: &mut Ctx) -> Result<Vec<Block>> {
    let mut out = vec![];
    for c in children {
        match c.rec.tag {
            TAG_PARA_HEADER => out.extend(para(c, cx)?),
            _ if !c.children.is_empty() => out.extend(paragraphs_in(&c.children, cx)?),
            _ => {}
        }
    }
    Ok(out)
}

fn find_picture(children: &[Node]) -> Option<u16> {
    for c in children {
        if c.rec.tag == TAG_PICTURE { let id = u16le(&c.rec.data, 71); if id > 0 { return Some(id); } }
        if let Some(id) = find_picture(&c.children) { return Some(id); }
    }
    None
}

fn control(node: &Node, cx: &mut Ctx) -> Result<Vec<Block>> {
    let id = ctrl_id(&node.rec.data);
    match id.as_str() {
        "tbl " => table(node, cx).map(|t| t.map(Block::Table).into_iter().collect()),
        "gso " => {
            let d = &node.rec.data;
            let (w, h) = (u32le(d, 16) as f32 / 100.0, u32le(d, 20) as f32 / 100.0);
            let mut out = vec![];
            if let Some(bin) = find_picture(&node.children) {
                if let Some(mut img) = (cx.bins)(bin) { if w > 0.0 && h > 0.0 { img.width_pt = Some(w); img.height_pt = Some(h); } out.push(Block::Image(img)); }
            }
            out.extend(paragraphs_in(&node.children, cx)?.into_iter().filter(|b| !matches!(b, Block::Para(p) if p.is_blank())));
            Ok(out)
        }
        "secd" => {
            for c in &node.children {
                if c.rec.tag == TAG_PAGE_DEF {
                    let d = &c.rec.data;
                    let g = |o: usize| u32le(d, o) as f32 / 100.0;
                    let (w, h) = (g(0), g(4));
                    if w > 100.0 && h > 100.0 && cx.page.is_none() { cx.page = Some(PageBox { width_pt: w, height_pt: h, margins: [g(16), g(12), g(20), g(8)].map(|m| m.clamp(0.0, 200.0)) }); }
                }
            }
            Ok(vec![])
        }
        _ => Ok(vec![]),
    }
}

fn table(node: &Node, cx: &mut Ctx) -> Result<Option<Table>> {
    let mut cells: Vec<(usize, usize, usize, usize, Cell)> = vec![];
    let mut current: Option<(usize, usize, usize, usize, Vec<Block>)> = None;
    let mut done = |cur: &mut Option<(usize, usize, usize, usize, Vec<Block>)>, cells: &mut Vec<_>| {
        if let Some((r, c, rs, cs, blocks)) = cur.take() { cells.push((r, c, rs, cs, Cell { blocks, col_span: cs })); }
    };
    for c in &node.children {
        match c.rec.tag {
            TAG_TABLE => {}
            TAG_LIST_HEADER => {
                done(&mut current, &mut cells);
                let d = &c.rec.data;
                cx.cells += 1;
                if cx.cells > cx.max_cells { return Err(Error::new(ErrorCode::ETooLarge, format!("document holds more than {} table cells", cx.max_cells))); }
                // LIST_HEADER: paragraph count (2), unknown (2), flags (4), then column, row, column span, row span.
                current = Some((u16le(d, 10) as usize, u16le(d, 8) as usize, (u16le(d, 14) as usize).clamp(1, 1024), (u16le(d, 12) as usize).clamp(1, 1024), vec![]));
            }
            TAG_PARA_HEADER => {
                let blocks = para(c, cx)?;
                if let Some((.., b)) = &mut current { b.extend(blocks.into_iter().filter(|b| !matches!(b, Block::PageBreak))); }
            }
            _ => {}
        }
    }
    done(&mut current, &mut cells);
    if cells.is_empty() { return Ok(None); }
    // A cell whose paragraphs are all blank keeps a single empty paragraph so writers have something to draw.
    for (_, _, _, _, cell) in &mut cells { if cell.blocks.is_empty() { cell.blocks.push(Block::Para(Para::default())); } }
    Ok(Some(Table { rows: super::grid_to_rows(cells) }))
}

fn inflate(data: &[u8], limit: u64) -> Result<Vec<u8>> {
    let mut out = vec![];
    flate2::read::DeflateDecoder::new(data).take(limit + 1).read_to_end(&mut out).map_err(|e| bad(format!("cannot decompress an HWP stream: {e}")))?;
    if out.len() as u64 > limit { return Err(Error::new(ErrorCode::ETooLarge, "An HWP stream inflates past the size limit.")); }
    Ok(out)
}

pub fn read<R: Read + Seek>(reader: R, cfg: &Config) -> Result<Doc> {
    let mut cf = cfb::CompoundFile::open(reader).map_err(|e| bad(format!("not an HWP 5 document (HWP 3.0 and older are not supported): {e}")))?;
    let limit = cfg.max_document_xml_bytes.max(64 * 1024 * 1024) * 2;
    let mut stream = |cf: &mut cfb::CompoundFile<R>, path: &str| -> Result<Option<Vec<u8>>> {
        if !cf.exists(path) { return Ok(None); }
        let mut s = cf.open_stream(path).map_err(|e| bad(e.to_string()))?;
        let mut buf = vec![];
        (&mut s).take(limit + 1).read_to_end(&mut buf).map_err(|e| bad(e.to_string()))?;
        if buf.len() as u64 > limit { return Err(Error::new(ErrorCode::ETooLarge, "An HWP stream is too large.")); }
        Ok(Some(buf))
    };
    let header = stream(&mut cf, "FileHeader")?.ok_or_else(|| bad("The file has no HWP header."))?;
    if !header.starts_with(b"HWP Document File") { return Err(bad("The file is not an HWP document.")); }
    let flags = u32le(&header, 36);
    let compressed = flags & 1 != 0;
    if flags & 2 != 0 { return Err(Error::new(ErrorCode::EEncrypted, "This HWP document is password protected.")); }
    if flags & 4 != 0 { return Err(bad("This is a distribution-only (read-only) HWP document, which is stored encrypted and cannot be converted.")); }
    let unpack = |raw: Vec<u8>| -> Result<Vec<u8>> { if compressed { inflate(&raw, limit) } else { Ok(raw) } };

    let doc_info = unpack(stream(&mut cf, "DocInfo")?.ok_or_else(|| bad("The HWP file has no DocInfo stream."))?)?;
    let info = parse_doc_info(&records(&doc_info)?);

    // Pictures live in BinData/BIN%04X.<ext>; load lazily and keep a budget.
    let mut bin_cache: std::collections::HashMap<u16, Option<Image>> = Default::default();
    let mut budget: u64 = 256 * 1024 * 1024;
    let mut section_blocks: Vec<Vec<Block>> = vec![];
    let mut sizes = vec![];
    let mut page = None;
    let mut cells = 0usize;
    for si in 0..2000 {
        let path = format!("BodyText/Section{si}");
        let Some(raw) = stream(&mut cf, &path)? else { break };
        let body = unpack(raw)?;
        let roots = tree(records(&body)?);
        // Resolve the pictures this section refers to first (the closure given to the walker cannot borrow `cf`).
        let mut wanted: Vec<u16> = vec![];
        fn collect(n: &[Node], out: &mut Vec<u16>) { for c in n { if c.rec.tag == TAG_PICTURE { let id = u16le(&c.rec.data, 71); if id > 0 { out.push(id); } } collect(&c.children, out); } }
        collect(&roots, &mut wanted);
        for id in wanted {
            if bin_cache.contains_key(&id) { continue; }
            let loaded = info.bins.get(id as usize - 1).filter(|(bid, _)| *bid > 0).and_then(|(bid, ext)| {
                let name = format!("BinData/BIN{bid:04X}.{ext}");
                let raw = stream(&mut cf, &name).ok().flatten()?;
                let data = if compressed { inflate(&raw, limit).unwrap_or(raw) } else { raw };
                budget = budget.checked_sub(data.len() as u64)?;
                Some(Image { mime: mime_for(&name).to_string(), data, width_pt: None, height_pt: None })
            });
            bin_cache.insert(id, loaded);
        }
        let lookup = |id: u16| bin_cache.get(&id).cloned().flatten();
        let mut cx = Ctx { info: &info, bins: &lookup, sizes: vec![], page, cells, max_cells: cfg.max_table_cells };
        let mut blocks = vec![];
        for n in &roots { if n.rec.tag == TAG_PARA_HEADER { blocks.extend(para(n, &mut cx)?); } }
        sizes.extend(std::mem::take(&mut cx.sizes));
        page = cx.page; cells = cx.cells;
        section_blocks.push(blocks);
    }
    if section_blocks.is_empty() { return Err(bad("The HWP file has no body text.")); }
    let mut doc = Doc { page, ..Default::default() };
    for (i, b) in section_blocks.into_iter().enumerate() {
        if i > 0 { doc.blocks.push(Block::PageBreak); }
        doc.blocks.extend(b);
    }
    while matches!(doc.blocks.last(), Some(Block::Para(p)) if p.is_blank() && p.heading.is_none()) { doc.blocks.pop(); }
    if !sizes.is_empty() { sizes.sort_by(|a, b| a.partial_cmp(b).unwrap()); doc.base_size_pt = Some(sizes[sizes.len() / 2].clamp(8.0, 14.0)); }
    Ok(doc)
}
