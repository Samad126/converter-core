//! `.hwpx` (Hancom's OWPML, a zip of XML) → `Doc`: paragraphs with character styles, headings and lists from the
//! paragraph styles in header.xml, tables (spans), pictures, page size and margins. Headers, footers and notes are
//! left out of the body.
use super::{attr, bad, mime_for, read_part, resolve_target};
use crate::native::model::*;
use crate::{Config, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::{Read, Seek};

#[derive(Clone, Default)]
struct CharPr { size: Option<f32>, bold: bool, italic: bool, underline: bool, color: Option<[u8; 3]> }

#[derive(Clone, Default)]
struct ParaPr { align: Align, heading: Option<(String, u8)> }

#[derive(Default)]
struct Header { chars: HashMap<String, CharPr>, paras: HashMap<String, ParaPr>, styles: HashMap<String, (String, String)> }

fn hex_color(v: &str) -> Option<[u8; 3]> {
    let v = v.strip_prefix('#')?;
    if v.len() != 6 { return None; }
    let n = u32::from_str_radix(v, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

fn parse_header(xml: &[u8]) -> Header {
    let mut h = Header::default();
    let (mut cur_char, mut cur_para): (Option<String>, Option<String>) = (None, None);
    let mut r = Reader::from_reader(xml);
    let mut buf = vec![];
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let a = || e.attributes();
                match e.local_name().as_ref() {
                    b"charPr" => {
                        cur_char = attr(a(), "id");
                        if let Some(id) = &cur_char {
                            let c = h.chars.entry(id.clone()).or_default();
                            c.size = attr(a(), "height").and_then(|v| v.parse::<f32>().ok()).map(|v| v / 100.0);
                            c.color = attr(a(), "textColor").and_then(|v| hex_color(&v));
                        }
                    }
                    b"bold" => if let Some(c) = cur_char.as_ref().and_then(|id| h.chars.get_mut(id)) { c.bold = true; },
                    b"italic" => if let Some(c) = cur_char.as_ref().and_then(|id| h.chars.get_mut(id)) { c.italic = true; },
                    b"underline" => if let Some(c) = cur_char.as_ref().and_then(|id| h.chars.get_mut(id)) { c.underline = attr(a(), "type").is_some_and(|t| t != "NONE"); },
                    b"paraPr" => { cur_para = attr(a(), "id"); if let Some(id) = &cur_para { h.paras.entry(id.clone()).or_default(); } }
                    b"align" => if let Some(p) = cur_para.as_ref().and_then(|id| h.paras.get_mut(id)) {
                        p.align = match attr(a(), "horizontal").as_deref() { Some("CENTER") => Align::Center, Some("RIGHT") => Align::Right, Some("JUSTIFY") | Some("DISTRIBUTE") => Align::Justify, _ => Align::Left };
                    },
                    b"heading" => if let Some(p) = cur_para.as_ref().and_then(|id| h.paras.get_mut(id)) {
                        let ty = attr(a(), "type").unwrap_or_default();
                        if ty != "NONE" && !ty.is_empty() { p.heading = Some((ty, attr(a(), "level").and_then(|v| v.parse().ok()).unwrap_or(0))); }
                    },
                    b"style" => if let (Some(id), Some(name)) = (attr(a(), "id"), attr(a(), "name")) {
                        h.styles.insert(id, (name, attr(a(), "paraPrIDRef").unwrap_or_default()));
                    },
                    _ => {}
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() { b"charPr" => cur_char = None, b"paraPr" => cur_para = None, _ => {} },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    h
}

/// Style names Hancom uses for headings: 개요 N (outline), 제목 (title), Heading N.
pub fn heading_from_style(name: &str) -> Option<u8> {
    let n = name.trim().trim_start_matches('@');
    let lower = n.to_lowercase();
    let digit = n.chars().rev().find(|c| c.is_ascii_digit()).and_then(|c| c.to_digit(10)).map(|d| d as u8);
    if n.starts_with("개요") || lower.starts_with("outline") || lower.starts_with("heading") { return Some(digit.unwrap_or(1).clamp(1, 6)); }
    if n.starts_with("제목") || lower == "title" { return Some(1); }
    None
}

/// `(id → target path)` from content.hpf's manifest.
fn parse_manifest(xml: &[u8]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let mut r = Reader::from_reader(xml);
    let mut buf = vec![];
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.local_name().as_ref() == b"item" => {
                if let (Some(id), Some(href)) = (attr(e.attributes(), "id"), attr(e.attributes(), "href")) { m.insert(id, href); }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    m
}

struct PState { para: Para, para_pr: String, style: String, images: Vec<Image>, page_break: bool }
struct TBuilder { cells: Vec<(usize, usize, usize, usize, Cell)>, addr: (usize, usize), row_span: usize, col_span: usize }

pub fn read<R: Read + Seek>(reader: R, cfg: &Config) -> Result<Doc> {
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| bad(format!("not a readable HWPX file: {e}")))?;
    let max = cfg.max_document_xml_bytes;
    let header = read_part(&mut zip, "Contents/header.xml", max)?.map(|x| parse_header(&x)).unwrap_or_default();
    let manifest = read_part(&mut zip, "Contents/content.hpf", max)?.map(|x| parse_manifest(&x)).unwrap_or_default();
    let mut sections: Vec<String> = (0..).map(|i| format!("Contents/section{i}.xml")).take_while(|n| zip.by_name(n).is_ok()).collect();
    if sections.is_empty() { return Err(bad("The HWPX file has no body sections.")); }
    sections.truncate(500);

    let mut doc = Doc::default();
    let mut image_budget: u64 = 256 * 1024 * 1024;
    let (mut cells_seen, max_cells) = (0usize, cfg.max_table_cells);
    let mut sizes: Vec<f32> = vec![];

    for (si, name) in sections.iter().enumerate() {
        let xml = read_part(&mut zip, name, max.max(128 * 1024 * 1024))?.ok_or_else(|| bad("missing section"))?;
        if si > 0 { doc.blocks.push(Block::PageBreak); }
        let mut containers: Vec<Vec<Block>> = vec![vec![]];
        let mut tables: Vec<TBuilder> = vec![];
        let mut spans: Vec<usize> = vec![];
        let mut paras: Vec<PState> = vec![];
        let mut cur_char = CharPr::default();
        let (mut in_t, mut skip, mut in_pic) = (false, 0usize, false);
        let mut pic_size: (Option<f32>, Option<f32>) = (None, None);

        let mut r = Reader::from_reader(xml.as_slice());
        let mut buf = vec![];
        loop {
            let ev = r.read_event_into(&mut buf).map_err(|e| bad(format!("malformed {name}: {e}")))?;
            let is_start = matches!(ev, Event::Start(_));
            match &ev {
                Event::Start(e) | Event::Empty(e) => {
                    let ln = e.local_name();
                    let a = || e.attributes();
                    // Headers, footers, notes and equations are not body text: skip their whole subtree.
                    if skip > 0 { if is_start { skip += 1; } buf.clear(); continue; }
                    match ln.as_ref() {
                        b"header" | b"footer" | b"footNote" | b"endNote" | b"equation" | b"memo" => { if is_start { skip = 1; } }
                        b"pagePr" => {
                            let (w, h) = (attr(a(), "width").and_then(|v| v.parse::<f32>().ok()), attr(a(), "height").and_then(|v| v.parse::<f32>().ok()));
                            if let (Some(w), Some(h)) = (w, h) { if doc.page.is_none() { doc.page = Some(PageBox { width_pt: w / 100.0, height_pt: h / 100.0, margins: [72.0; 4] }); } }
                        }
                        b"margin" if doc.page.is_some_and(|p| p.margins == [72.0; 4]) && attr(a(), "left").is_some() && attr(a(), "top").is_some() => {
                            let g = |k: &str| attr(a(), k).and_then(|v| v.parse::<f32>().ok()).map(|v| (v / 100.0).clamp(0.0, 200.0));
                            if let (Some(t), Some(rt), Some(b), Some(l), Some(p)) = (g("top"), g("right"), g("bottom"), g("left"), doc.page.as_mut()) { p.margins = [t, rt, b, l]; }
                        }
                        b"p" if is_start => {
                            let para_pr = attr(a(), "paraPrIDRef").unwrap_or_default();
                            let style = attr(a(), "styleIDRef").unwrap_or_default();
                            paras.push(PState { para: Para::default(), para_pr, style, images: vec![], page_break: attr(a(), "pageBreak").as_deref() == Some("1") });
                        }
                        b"run" => cur_char = header.chars.get(&attr(a(), "charPrIDRef").unwrap_or_default()).cloned().unwrap_or_default(),
                        b"t" if is_start => in_t = true,
                        b"tab" if in_t => push(&mut paras, &cur_char, "\t"),
                        b"lineBreak" | b"br" if in_t => push(&mut paras, &cur_char, "\n"),
                        b"fwSpace" | b"nbSpace" if in_t => push(&mut paras, &cur_char, " "),
                        b"hyphen" if in_t => push(&mut paras, &cur_char, "-"),
                        b"pic" if is_start => { in_pic = true; pic_size = (None, None); }
                        b"curSz" | b"sz" if in_pic => {
                            let (w, h) = (attr(a(), "width").and_then(|v| v.parse::<f32>().ok()), attr(a(), "height").and_then(|v| v.parse::<f32>().ok()));
                            if w.is_some_and(|w| w > 0.0) { pic_size = (w.map(|w| w / 100.0), h.map(|h| h / 100.0)); }
                        }
                        b"img" => if let (Some(id), Some(ps)) = (attr(a(), "binaryItemIDRef"), paras.last_mut()) {
                            if let Some(href) = manifest.get(&id) {
                                let path = resolve_target("", href);
                                if let Ok(Some(data)) = read_part(&mut zip, &path, 64 * 1024 * 1024) {
                                    if let Some(left) = image_budget.checked_sub(data.len() as u64) {
                                        image_budget = left;
                                        ps.images.push(Image { mime: mime_for(&path).to_string(), data, width_pt: pic_size.0, height_pt: pic_size.1 });
                                    }
                                }
                            }
                        },
                        b"tbl" if is_start => tables.push(TBuilder { cells: vec![], addr: (0, 0), row_span: 1, col_span: 1 }),
                                                b"tc" if is_start => { containers.push(vec![]); spans.push(1); }
                        b"cellAddr" => if let Some(t) = tables.last_mut() {
                            t.addr = (attr(a(), "rowAddr").and_then(|v| v.parse().ok()).unwrap_or(0), attr(a(), "colAddr").and_then(|v| v.parse().ok()).unwrap_or(0));
                        },
                        b"cellSpan" => if let Some(t) = tables.last_mut() {
                            t.col_span = attr(a(), "colSpan").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 1024);
                            t.row_span = attr(a(), "rowSpan").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 1024);
                        },
                        _ => {}
                    }
                }
                Event::End(e) => {
                    let ln = e.local_name();
                    if skip > 0 { skip -= 1; buf.clear(); continue; }
                    match ln.as_ref() {
                        b"t" => in_t = false,
                        b"p" => if let Some(ps) = paras.pop() {
                            let mut p = ps.para;
                            if let Some(pp) = header.paras.get(&ps.para_pr) {
                                p.align = pp.align;
                                match &pp.heading {
                                    Some((ty, lvl)) if ty == "OUTLINE" => p.heading = Some((lvl + 1).clamp(1, 6)),
                                    Some((ty, lvl)) if ty == "NUMBER" => p.list = Some(ListInfo { ordered: true, level: (*lvl).min(6) }),
                                    Some((ty, lvl)) if ty == "BULLET" => p.list = Some(ListInfo { ordered: false, level: (*lvl).min(6) }),
                                    _ => {}
                                }
                            }
                            if let Some((sname, _)) = header.styles.get(&ps.style) { if let Some(h) = heading_from_style(sname) { p.heading = Some(h); p.list = None; } }
                            for r in &p.runs { if let Some(s) = r.size_pt { sizes.push(s); } }
                            let keep = !p.runs.is_empty() || !ps.images.is_empty() || containers.len() == 1;
                            let top_level = containers.len() == 1;
                            let c = containers.last_mut().expect("container");
                            if ps.page_break && top_level { c.push(Block::PageBreak); }
                            if keep { c.push(Block::Para(p)); }
                            let c = containers.last_mut().expect("container");
                            for img in ps.images { c.push(Block::Image(img)); }
                        },
                        b"tc" => if let (Some(blocks), Some(span)) = (containers.pop(), spans.pop()) {
                            cells_seen += 1;
                            if cells_seen > max_cells { return Err(crate::Error::new(crate::ErrorCode::ETooLarge, format!("document holds more than {max_cells} table cells"))); }
                            let _ = span;
                            if let Some(t) = tables.last_mut() {
                                let (r, c, rs, cs) = (t.addr.0, t.addr.1, t.row_span, t.col_span);
                                t.cells.push((r, c, rs, cs, Cell { blocks, col_span: cs }));
                                t.row_span = 1; t.col_span = 1; t.addr = (r, c + cs);
                            }
                        },
                        b"tbl" => if let Some(t) = tables.pop() { containers.last_mut().expect("container").push(Block::Table(Table { rows: super::grid_to_rows(t.cells) })); },
                        b"pic" => { in_pic = false; pic_size = (None, None); }
                        _ => {}
                    }
                }
                Event::Text(t) if in_t && skip == 0 => { if let Ok(s) = t.decode() { push(&mut paras, &cur_char, &s); } }
                Event::GeneralRef(rf) if in_t && skip == 0 => {
                    let ch = rf.resolve_char_ref().ok().flatten().or_else(|| match rf.decode().ok()?.as_ref() { "amp" => Some('&'), "lt" => Some('<'), "gt" => Some('>'), "quot" => Some('"'), "apos" => Some('\''), _ => None });
                    if let Some(c) = ch { push(&mut paras, &cur_char, &c.to_string()); }
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        doc.blocks.extend(containers.into_iter().next().unwrap_or_default());
    }
    while matches!(doc.blocks.last(), Some(Block::Para(p)) if p.is_blank() && p.heading.is_none()) { doc.blocks.pop(); }
    if !sizes.is_empty() {
        sizes.sort_by(|a, b| a.partial_cmp(b).unwrap());
        doc.base_size_pt = Some(sizes[sizes.len() / 2].clamp(8.0, 14.0));
    }
    Ok(doc)
}

fn push(paras: &mut [PState], c: &CharPr, text: &str) {
    let Some(ps) = paras.last_mut() else { return };
    if text.is_empty() { return; }
    let run = Run { text: text.to_string(), bold: c.bold, italic: c.italic, underline: c.underline, size_pt: c.size, color: c.color.filter(|c| *c != [0, 0, 0]), ..Default::default() };
    if let Some(last) = ps.para.runs.last_mut() {
        if last.bold == run.bold && last.italic == run.italic && last.underline == run.underline && last.size_pt == run.size_pt && last.color == run.color { last.text.push_str(&run.text); return; }
    }
    ps.para.runs.push(run);
}
