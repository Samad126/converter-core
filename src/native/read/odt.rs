//! OpenDocument text (`.odt`, `.ott`, and flat `.fodt`) → `Doc`: headings, paragraphs and spans with their styles,
//! links, lists (bullet or numbered per list style), tables (column and row spans), pictures, page breaks, page size.
use super::{attr, bad, mime_for, read_part, resolve_target};
use crate::native::model::*;
use crate::{Config, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::{Read, Seek};

#[derive(Clone, Default)]
struct Look { bold: Option<bool>, italic: Option<bool>, underline: Option<bool>, mono: Option<bool>, size: Option<f32>, color: Option<[u8; 3]> }

#[derive(Clone, Default)]
struct PStyle { look: Look, align: Option<Align>, break_before: bool, parent: Option<String> }

#[derive(Default)]
struct Styles {
    text: HashMap<String, (Look, Option<String>)>,
    para: HashMap<String, PStyle>,
    /// list style name → per level (1-based index 0..) "is numbered"
    lists: HashMap<String, Vec<bool>>,
    page: Option<PageBox>,
}

fn length_pt(v: &str) -> Option<f32> {
    let v = v.trim();
    let split = v.find(|c: char| c.is_alphabetic() || c == '%').unwrap_or(v.len());
    let n: f32 = v[..split].parse().ok()?;
    Some(match &v[split..] { "cm" => n * 28.3465, "mm" => n * 2.83465, "in" => n * 72.0, "pt" => n, "pc" => n * 12.0, "px" => n * 0.75, "" => n, _ => return None })
}

fn hex(v: &str) -> Option<[u8; 3]> {
    let h = v.strip_prefix('#')?;
    if h.len() != 6 { return None; }
    let n = u32::from_str_radix(h, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

fn look_from(e: &quick_xml::events::BytesStart) -> Look {
    let g = |k: &str| attr(e.attributes(), k);
    Look {
        bold: g("font-weight").map(|w| w == "bold" || w.parse::<u32>().is_ok_and(|n| n >= 600)),
        italic: g("font-style").map(|s| s == "italic" || s == "oblique"),
        underline: g("text-underline-style").map(|u| u != "none"),
        mono: g("font-name").map(|f| { let f = f.to_lowercase(); f.contains("mono") || f.contains("courier") || f.contains("consolas") }),
        size: g("font-size").and_then(|s| length_pt(&s)),
        color: g("color").and_then(|c| hex(&c)),
    }
}

fn merge(base: &Look, over: &Look) -> Look {
    Look { bold: over.bold.or(base.bold), italic: over.italic.or(base.italic), underline: over.underline.or(base.underline), mono: over.mono.or(base.mono), size: over.size.or(base.size), color: over.color.or(base.color) }
}

impl Styles {
    fn text_look(&self, name: &str) -> Look {
        let mut chain = vec![];
        let mut cur = Some(name.to_string());
        for _ in 0..6 { let Some(n) = cur.take() else { break }; let Some((l, parent)) = self.text.get(&n) else { break }; chain.push(l.clone()); cur = parent.clone(); }
        chain.iter().rev().fold(Look::default(), |acc, l| merge(&acc, l))
    }
    /// The style's own name and its ancestors' (`Heading_20_1` becomes `Heading 1`).
    fn para_names(&self, name: &str) -> Vec<String> {
        let (mut out, mut cur) = (vec![], Some(name.to_string()));
        for _ in 0..8 { let Some(n) = cur.take() else { break }; out.push(n.replace("_20_", " ")); cur = self.para.get(&n).and_then(|p| p.parent.clone()); }
        out
    }
    fn para_style(&self, name: &str) -> PStyle {
        let mut chain = vec![];
        let mut cur = Some(name.to_string());
        for _ in 0..8 { let Some(n) = cur.take() else { break }; let Some(p) = self.para.get(&n) else { break }; chain.push(p.clone()); cur = p.parent.clone(); }
        chain.iter().rev().fold(PStyle::default(), |acc, p| PStyle { look: merge(&acc.look, &p.look), align: p.align.or(acc.align), break_before: p.break_before || acc.break_before, parent: None })
    }
}

/// Collects style definitions (automatic and named) from one XML part.
fn parse_styles(xml: &[u8], st: &mut Styles) {
    let mut r = Reader::from_reader(xml);
    let mut buf = vec![];
    let (mut cur, mut cur_family, mut cur_parent): (Option<String>, String, Option<String>) = (None, String::new(), None);
    let mut cur_list: Option<(String, Vec<bool>)> = None;
    let mut cur_para = PStyle::default();
    let mut cur_text = Look::default();
    let mut in_page_layout = false;
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                match e.local_name().as_ref() {
                    b"style" if attr(e.attributes(), "family").is_some() => {
                        cur = attr(e.attributes(), "name");
                        cur_family = attr(e.attributes(), "family").unwrap_or_default();
                        cur_parent = attr(e.attributes(), "parent-style-name");
                        cur_para = PStyle { parent: cur_parent.clone(), ..Default::default() };
                        cur_text = Look::default();
                        if attr(e.attributes(), "master-page-name").is_some() { cur_para.break_before = true; }
                    }
                    b"text-properties" if cur.is_some() => { cur_text = look_from(&e); cur_para.look = cur_text.clone(); }
                    b"paragraph-properties" if cur.is_some() => {
                        cur_para.align = attr(e.attributes(), "text-align").map(|a| match a.as_str() { "center" => Align::Center, "end" | "right" => Align::Right, "justify" => Align::Justify, _ => Align::Left });
                        if attr(e.attributes(), "break-before").as_deref() == Some("page") { cur_para.break_before = true; }
                    }
                    b"list-style" => cur_list = attr(e.attributes(), "name").map(|n| (n, vec![])),
                    b"list-level-style-number" => if let Some((_, v)) = &mut cur_list { v.push(true); },
                    b"list-level-style-bullet" | b"list-level-style-image" => if let Some((_, v)) = &mut cur_list { v.push(false); },
                    b"page-layout" => in_page_layout = true,
                    b"page-layout-properties" if in_page_layout && st.page.is_none() => {
                        let g = |k: &str| attr(e.attributes(), k).and_then(|v| length_pt(&v));
                        if let (Some(w), Some(h)) = (g("page-width"), g("page-height")) {
                            let m = |k: &str| g(k).unwrap_or(56.7).clamp(0.0, 200.0);
                            st.page = Some(PageBox { width_pt: w, height_pt: h, margins: [m("margin-top"), m("margin-right"), m("margin-bottom"), m("margin-left")] });
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => match e.local_name().as_ref() {
                b"style" => if let Some(n) = cur.take() {
                    match cur_family.as_str() {
                        "text" => { st.text.insert(n, (cur_text.clone(), cur_parent.take())); }
                        "paragraph" => { st.para.insert(n, std::mem::take(&mut cur_para)); }
                        _ => {}
                    }
                },
                b"list-style" => if let Some((n, v)) = cur_list.take() { st.lists.insert(n, v); },
                b"page-layout" => in_page_layout = false,
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
}

struct P { para: Para, style: PStyle, images: Vec<Image>, in_list: Option<ListInfo> }

fn push_text(p: &mut P, look: &Look, link: &Option<String>, text: &str) {
    if text.is_empty() { return; }
    let run = Run { text: text.to_string(), bold: look.bold.unwrap_or(false), italic: look.italic.unwrap_or(false), underline: look.underline.unwrap_or(false) || link.is_some(), mono: look.mono.unwrap_or(false), size_pt: look.size, color: look.color.filter(|c| *c != [0, 0, 0]), link: link.clone() };
    if let Some(l) = p.para.runs.last_mut() {
        if l.bold == run.bold && l.italic == run.italic && l.underline == run.underline && l.mono == run.mono && l.size_pt == run.size_pt && l.color == run.color && l.link == run.link { l.text.push_str(&run.text); return; }
    }
    p.para.runs.push(run);
}

fn parse_body<R: Read + Seek>(xml: &[u8], st: &Styles, zip: Option<&mut zip::ZipArchive<R>>, cfg: &Config, doc: &mut Doc) -> Result<()> {
    let mut zip = zip;
    let mut containers: Vec<Vec<Block>> = vec![vec![]];
    struct T { cells: Vec<(usize, usize, usize, usize, Cell)>, row: usize, col: usize, covered: std::collections::HashSet<(usize, usize)>, span: (usize, usize) }
    let mut tables: Vec<T> = vec![];
    let mut paras: Vec<P> = vec![];
    let mut lists: Vec<Option<String>> = vec![]; // list style names (None = inherit)
    let mut list_item_pending: Vec<bool> = vec![];
    let mut spans: Vec<Look> = vec![Look::default()];
    let mut links: Vec<Option<String>> = vec![None];
    let (mut in_body, mut skip) = (false, 0usize);
    let mut pending_break = false;
    let mut image_budget: u64 = 256 * 1024 * 1024;
    let mut frame: (Option<f32>, Option<f32>) = (None, None);
    let mut cells = 0usize;

    let mut r = Reader::from_reader(xml);
    let mut buf = vec![];
    loop {
        let ev = r.read_event_into(&mut buf).map_err(|e| bad(format!("malformed content.xml: {e}")))?;
        let is_start = matches!(ev, Event::Start(_));
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                let ln = e.local_name();
                let name = ln.as_ref();
                if !in_body { if name == b"text" { in_body = true; } buf.clear(); continue; }
                if skip > 0 { if is_start { skip += 1; } buf.clear(); continue; }
                match name {
                    b"note" | b"annotation" | b"tracked-changes" | b"sequence-decls" | b"user-field-decls" | b"variable-decls" | b"table-of-content-source" if is_start => skip = 1,
                    b"p" | b"h" if is_start => {
                        let sname = attr(e.attributes(), "style-name").unwrap_or_default();
                        let style = st.para_style(&sname);
                        let mut para = Para { align: style.align.unwrap_or(Align::Left), ..Default::default() };
                        if name == b"p" { para.heading = st.para_names(&sname).iter().find_map(|n| super::hwpx::heading_from_style(n)); }
                        if name == b"h" { para.heading = Some(attr(e.attributes(), "outline-level").and_then(|v| v.parse::<u8>().ok()).unwrap_or(1).clamp(1, 6)); }
                        let in_list = if name == b"p" && !lists.is_empty() && list_item_pending.last().copied().unwrap_or(false) {
                            let level = lists.len() - 1;
                            let style_name = lists.iter().rev().find_map(|l| l.clone());
                            let ordered = style_name.and_then(|n| st.lists.get(&n).and_then(|v| v.get(level).or(v.first()).copied())).unwrap_or(false);
                            if let Some(f) = list_item_pending.last_mut() { *f = false; }
                            Some(ListInfo { ordered, level: level.min(6) as u8 })
                        } else { None };
                        para.list = in_list.clone();
                        spans.push(style.look.clone());
                        paras.push(P { para, style, images: vec![], in_list });
                    }
                    b"span" if is_start => { let l = st.text_look(&attr(e.attributes(), "style-name").unwrap_or_default()); let base = spans.last().cloned().unwrap_or_default(); spans.push(merge(&base, &l)); }
                    b"a" if is_start => { links.push(attr(e.attributes(), "href").filter(|h| !h.starts_with('#'))); }
                    b"tab" => if let Some(p) = paras.last_mut() { let l = spans.last().cloned().unwrap_or_default(); push_text(p, &l, links.last().unwrap_or(&None), "\t"); },
                    b"line-break" => if let Some(p) = paras.last_mut() { let l = spans.last().cloned().unwrap_or_default(); push_text(p, &l, links.last().unwrap_or(&None), "\n"); },
                    b"s" => if let Some(p) = paras.last_mut() {
                        let n = attr(e.attributes(), "c").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).min(200);
                        let l = spans.last().cloned().unwrap_or_default();
                        push_text(p, &l, links.last().unwrap_or(&None), &" ".repeat(n));
                    },
                    b"list" if is_start => lists.push(attr(e.attributes(), "style-name")),
                    b"list-item" | b"list-header" if is_start => list_item_pending.push(true),
                    b"table" if is_start => tables.push(T { cells: vec![], row: 0, col: 0, covered: Default::default(), span: (1, 1) }),
                    b"table-row" if is_start => if let Some(t) = tables.last_mut() { t.col = 0; },
                    b"table-cell" | b"covered-table-cell" => {
                        let covered = name == b"covered-table-cell";
                        if let Some(t) = tables.last_mut() {
                            while t.covered.contains(&(t.row, t.col)) { t.col += 1; }
                            let cs = attr(e.attributes(), "number-columns-spanned").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 1024);
                            let rs = attr(e.attributes(), "number-rows-spanned").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 1024);
                            t.span = (rs, cs);
                            if covered { t.col += 1; }
                        }
                        if is_start && !covered { containers.push(vec![]); }
                        if !is_start && !covered {
                            // An empty cell (self-closing).
                            if let Some(t) = tables.last_mut() {
                                let (rs, cs, row, col) = (t.span.0, t.span.1, t.row, t.col);
                                t.cells.push((row, col, rs, cs, Cell { blocks: vec![Block::Para(Para::default())], col_span: cs }));
                                for dr in 1..rs { for dc in 0..cs { t.covered.insert((row + dr, col + dc)); } }
                                t.col += cs;
                            }
                        }
                    }
                    b"frame" if is_start => { frame = (attr(e.attributes(), "width").and_then(|v| length_pt(&v)), attr(e.attributes(), "height").and_then(|v| length_pt(&v))); }
                    b"image" => if let (Some(href), Some(z)) = (attr(e.attributes(), "href"), zip.as_deref_mut()) {
                        let path = resolve_target("", &href);
                        if let Ok(Some(data)) = read_part(z, &path, 64 * 1024 * 1024) {
                            if let Some(left) = image_budget.checked_sub(data.len() as u64) {
                                image_budget = left;
                                let img = Image { mime: mime_for(&path).to_string(), data, width_pt: frame.0, height_pt: frame.1 };
                                match paras.last_mut() { Some(p) => p.images.push(img), None => containers.last_mut().expect("container").push(Block::Image(img)) }
                            }
                        }
                    },
                    _ => {}
                }
            }
            Event::End(e) => {
                let ln = e.local_name();
                let name = ln.as_ref();
                if !in_body { buf.clear(); continue; }
                if skip > 0 { skip -= 1; buf.clear(); continue; }
                match name {
                    b"text" if paras.is_empty() && tables.is_empty() => in_body = false,
                    b"p" | b"h" => if let Some(mut p) = paras.pop() {
                        spans.pop();
                        if p.style.break_before && containers.len() == 1 { pending_break = true; }
                        if let Some(h) = p.para.heading { p.para.list = None; p.para.heading = Some(h); }
                        let keep = !p.para.runs.is_empty() || !p.images.is_empty() || containers.len() == 1;
                        let top = containers.len() == 1;
                        let c = containers.last_mut().expect("container");
                        if pending_break && top { c.push(Block::PageBreak); pending_break = false; }
                        if keep { c.push(Block::Para(p.para)); }
                        for img in p.images { containers.last_mut().expect("container").push(Block::Image(img)); }
                    },
                    b"span" => { if spans.len() > 1 { spans.pop(); } }
                    b"a" => { if links.len() > 1 { links.pop(); } }
                    b"list" => { lists.pop(); }
                    b"list-item" | b"list-header" => { list_item_pending.pop(); }
                    b"table-cell" => if let Some(blocks) = (containers.len() > 1).then(|| containers.pop()).flatten() {
                        cells += 1;
                        if cells > cfg.max_table_cells { return Err(crate::Error::new(crate::ErrorCode::ETooLarge, format!("document holds more than {} table cells", cfg.max_table_cells))); }
                        if let Some(t) = tables.last_mut() {
                            let (rs, cs, row, col) = (t.span.0, t.span.1, t.row, t.col);
                            t.cells.push((row, col, rs, cs, Cell { blocks: if blocks.is_empty() { vec![Block::Para(Para::default())] } else { blocks }, col_span: cs }));
                            for dr in 1..rs { for dc in 0..cs { t.covered.insert((row + dr, col + dc)); } }
                            t.col += cs;
                        }
                    },
                    b"table-row" => if let Some(t) = tables.last_mut() { t.row += 1; },
                    b"table" => if let Some(t) = tables.pop() {
                        let rows = super::grid_to_rows(t.cells);
                        if !rows.is_empty() { containers.last_mut().expect("container").push(Block::Table(Table { rows })); }
                    },
                    b"frame" => frame = (None, None),
                    _ => {}
                }
            }
            Event::Text(t) if skip == 0 && in_body => if let Some(p) = paras.last_mut() {
                let s = t.decode().map_err(|e| bad(e.to_string()))?;
                let l = spans.last().cloned().unwrap_or_default();
                push_text(p, &l, links.last().unwrap_or(&None), &s);
            },
            Event::GeneralRef(rf) if skip == 0 && in_body => if let Some(p) = paras.last_mut() {
                let ch = rf.resolve_char_ref().ok().flatten().or_else(|| match rf.decode().ok()?.as_ref() { "amp" => Some('&'), "lt" => Some('<'), "gt" => Some('>'), "quot" => Some('"'), "apos" => Some('\''), _ => None });
                if let Some(c) = ch { let l = spans.last().cloned().unwrap_or_default(); push_text(p, &l, links.last().unwrap_or(&None), &c.to_string()); }
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    doc.blocks.extend(containers.into_iter().next().unwrap_or_default());
    Ok(())
}

fn finish(mut doc: Doc, st: &Styles) -> Doc {
    while matches!(doc.blocks.last(), Some(Block::Para(p)) if p.is_blank() && p.heading.is_none()) { doc.blocks.pop(); }
    doc.page = st.page;
    let mut sizes: Vec<f32> = doc.blocks.iter().filter_map(|b| if let Block::Para(p) = b { p.runs.iter().find_map(|r| r.size_pt) } else { None }).collect();
    if !sizes.is_empty() { sizes.sort_by(|a, b| a.partial_cmp(b).unwrap()); doc.base_size_pt = Some(sizes[sizes.len() / 2].clamp(8.0, 14.0)); }
    doc
}

pub fn read<R: Read + Seek>(reader: R, cfg: &Config) -> Result<Doc> {
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| bad(format!("not a readable OpenDocument file: {e}")))?;
    let max = cfg.max_document_xml_bytes;
    let mut st = Styles::default();
    if let Some(s) = read_part(&mut zip, "styles.xml", max)? { parse_styles(&s, &mut st); }
    let content = read_part(&mut zip, "content.xml", max)?.ok_or_else(|| bad("The file has no content.xml."))?;
    parse_styles(&content, &mut st);
    let mut doc = Doc::default();
    parse_body(&content, &st, Some(&mut zip), cfg, &mut doc)?;
    Ok(finish(doc, &st))
}

/// Flat OpenDocument (`.fodt`): one XML file holding styles and content (pictures are inline base64 and are skipped).
pub fn read_flat(bytes: &[u8], cfg: &Config) -> Result<Doc> {
    let mut st = Styles::default();
    parse_styles(bytes, &mut st);
    let mut doc = Doc::default();
    parse_body::<std::io::Cursor<Vec<u8>>>(bytes, &st, None, cfg, &mut doc)?;
    Ok(finish(doc, &st))
}
