//! `.docx` (and macro/template variants) → `Doc`. Reads document.xml, styles, numbering and relationships.
use super::{attr, bad, mime_for, read_part, resolve_target};
use crate::native::model::*;
use crate::{Config, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::{Read, Seek};

const EMU_PER_PT: f32 = 12_700.0;

#[derive(Default)]
struct Styles { by_id: HashMap<String, (String, Option<String>)>, base_size: Option<f32> }

impl Styles {
    fn heading_level(&self, id: &str) -> Option<u8> {
        let mut cur = id.to_string();
        for _ in 0..8 {
            let (name, based) = self.by_id.get(&cur)?;
            let n = name.to_lowercase();
            if n == "title" { return Some(1); }
            if let Some(rest) = n.strip_prefix("heading ") { return rest.trim().parse::<u8>().ok().map(|l| l.clamp(1, 6)); }
            cur = based.clone()?;
        }
        None
    }
    fn list_kind(&self, id: &str) -> Option<bool> {
        let (name, _) = self.by_id.get(id)?;
        let n = name.to_lowercase();
        if n.starts_with("list bullet") { Some(false) } else if n.starts_with("list number") { Some(true) } else { None }
    }
}

fn parse_styles(xml: &[u8]) -> Styles {
    let mut st = Styles::default();
    let mut in_defaults = false;
    let mut r = Reader::from_reader(xml);
    let mut cur: Option<String> = None;
    let mut buf = vec![];
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"docDefaults" => in_defaults = true,
                b"sz" if in_defaults => st.base_size = attr(e.attributes(), "val").and_then(|v| v.parse::<f32>().ok()).map(|h| h / 2.0),
                b"style" => cur = attr(e.attributes(), "styleId"),
                b"name" => if let Some(id) = &cur { let n = attr(e.attributes(), "val").unwrap_or_default(); st.by_id.entry(id.clone()).or_default().0 = n; },
                b"basedOn" => if let Some(id) = &cur { st.by_id.entry(id.clone()).or_default().1 = attr(e.attributes(), "val"); },
                _ => {}
            },
            Ok(Event::End(e)) if e.local_name().as_ref() == b"style" => cur = None,
            Ok(Event::End(e)) if e.local_name().as_ref() == b"docDefaults" => in_defaults = false,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    st
}

/// `(numId, level)` → is the list numbered?
fn parse_numbering(xml: &[u8]) -> HashMap<(String, u8), bool> {
    let mut abstract_fmt: HashMap<(String, u8), bool> = HashMap::new();
    let mut num_to_abs: HashMap<String, String> = HashMap::new();
    let (mut cur_abs, mut cur_lvl, mut cur_num): (Option<String>, u8, Option<String>) = (None, 0, None);
    let mut r = Reader::from_reader(xml);
    let mut buf = vec![];
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                b"abstractNum" => cur_abs = attr(e.attributes(), "abstractNumId"),
                b"lvl" => cur_lvl = attr(e.attributes(), "ilvl").and_then(|v| v.parse().ok()).unwrap_or(0),
                b"numFmt" => if let Some(a) = &cur_abs { abstract_fmt.insert((a.clone(), cur_lvl), attr(e.attributes(), "val").is_some_and(|v| v != "bullet" && v != "none")); },
                b"num" => cur_num = attr(e.attributes(), "numId"),
                b"abstractNumId" => if let Some(n) = &cur_num { if let Some(v) = attr(e.attributes(), "val") { num_to_abs.insert(n.clone(), v); } },
                _ => {}
            },
            Ok(Event::End(e)) => match e.local_name().as_ref() { b"abstractNum" => cur_abs = None, b"num" => cur_num = None, _ => {} },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    let mut out = HashMap::new();
    for (num, abs) in num_to_abs {
        for lvl in 0..9u8 { if let Some(v) = abstract_fmt.get(&(abs.clone(), lvl)) { out.insert((num.clone(), lvl), *v); } }
    }
    out
}

/// `(id → (target, is_external))`
fn parse_rels(xml: &[u8]) -> HashMap<String, (String, bool)> {
    let mut m = HashMap::new();
    let mut r = Reader::from_reader(xml);
    let mut buf = vec![];
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.local_name().as_ref() == b"Relationship" => {
                let (id, target) = (attr(e.attributes(), "Id"), attr(e.attributes(), "Target"));
                let external = attr(e.attributes(), "TargetMode").is_some_and(|v| v == "External");
                if let (Some(id), Some(t)) = (id, target) { m.insert(id, (t, external)); }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    m
}

#[derive(Clone, Default)]
struct RunProps { bold: bool, italic: bool, underline: bool, mono: bool, size: Option<f32>, color: Option<[u8; 3]> }

#[derive(Default)]
struct ParaState {
    para: Para,
    style: Option<String>,
    outline: Option<u8>,
    num: Option<(String, u8)>,
    images: Vec<Image>,
    page_break_after: bool,
}

struct TableBuilder { rows: Vec<Vec<Cell>>, row: Vec<Cell> }

fn parse_hex(v: &str) -> Option<[u8; 3]> {
    if v.len() != 6 { return None; }
    let n = u32::from_str_radix(v, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

fn push_text(ps: &mut ParaState, props: &RunProps, link: &Option<String>, text: &str) {
    if text.is_empty() { return; }
    let run = Run { text: text.to_string(), bold: props.bold, italic: props.italic, underline: props.underline, mono: props.mono,
        size_pt: props.size, color: props.color, link: link.clone() };
    if let Some(last) = ps.para.runs.last_mut() {
        if last.bold == run.bold && last.italic == run.italic && last.underline == run.underline && last.mono == run.mono
            && last.size_pt == run.size_pt && last.color == run.color && last.link == run.link {
            last.text.push_str(&run.text);
            return;
        }
    }
    ps.para.runs.push(run);
}

pub fn read<R: Read + Seek>(reader: R, cfg: &Config) -> Result<Doc> {
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| bad(format!("not a readable document: {e}")))?;
    let max = cfg.max_document_xml_bytes;
    let document = read_part(&mut zip, "word/document.xml", max)?.ok_or_else(|| bad("the document has no readable word/document.xml"))?;
    let styles = read_part(&mut zip, "word/styles.xml", max)?.map(|x| parse_styles(&x)).unwrap_or_default();
    let numbering = read_part(&mut zip, "word/numbering.xml", max)?.map(|x| parse_numbering(&x)).unwrap_or_default();
    let rels = read_part(&mut zip, "word/_rels/document.xml.rels", max)?.map(|x| parse_rels(&x)).unwrap_or_default();

    let mut image_budget: u64 = 256 * 1024 * 1024;
    let mut load_image = |zip: &mut zip::ZipArchive<R>, rid: &str, size: (Option<f32>, Option<f32>)| -> Option<Image> {
        let (target, external) = rels.get(rid)?;
        if *external { return None; }
        let path = resolve_target("word", target);
        let data = read_part(zip, &path, 64 * 1024 * 1024).ok()??;
        image_budget = image_budget.checked_sub(data.len() as u64)?;
        Some(Image { data, mime: mime_for(&path).to_string(), width_pt: size.0, height_pt: size.1 })
    };

    let mut containers: Vec<Vec<Block>> = vec![vec![]];
    let mut tables: Vec<TableBuilder> = vec![];
    let mut spans: Vec<usize> = vec![];
    let mut paras: Vec<ParaState> = vec![];
    let (mut in_ppr, mut in_rpr, mut in_t, mut in_ppr_numpr) = (false, false, false, false);
    let mut props = RunProps::default();
    let mut link: Option<String> = None;
    let mut extent: (Option<f32>, Option<f32>) = (None, None);
    let mut cells_seen = 0usize;
    let (mut page_w, mut page_h, mut margins): (Option<f32>, Option<f32>, [f32; 4]) = (None, None, [72.0; 4]);

    let mut r = Reader::from_reader(document.as_slice());
    let mut buf = vec![];
    loop {
        let ev = r.read_event_into(&mut buf).map_err(|e| bad(format!("malformed document.xml: {e}")))?;
        let (is_start, is_empty) = (matches!(ev, Event::Start(_)), matches!(ev, Event::Empty(_)));
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                match e.local_name().as_ref() {
                    b"p" if !is_empty => paras.push(ParaState::default()),
                    b"p" => {}
                    b"pPr" if is_start => in_ppr = true,
                    b"numPr" if is_start => in_ppr_numpr = true,
                    b"rPr" if is_start => in_rpr = true,
                    b"pStyle" if in_ppr => if let Some(ps) = paras.last_mut() { ps.style = attr(e.attributes(), "val"); },
                    b"jc" if in_ppr => if let Some(ps) = paras.last_mut() {
                        ps.para.align = match attr(e.attributes(), "val").as_deref() { Some("center") => Align::Center, Some("right") | Some("end") => Align::Right, Some("both") | Some("distribute") => Align::Justify, _ => Align::Left };
                    },
                    b"outlineLvl" if in_ppr => if let Some(ps) = paras.last_mut() { ps.outline = attr(e.attributes(), "val").and_then(|v| v.parse::<u8>().ok()).filter(|l| *l < 9).map(|l| l + 1); },
                    b"ilvl" if in_ppr_numpr => if let Some(ps) = paras.last_mut() {
                        let l = attr(e.attributes(), "val").and_then(|v| v.parse().ok()).unwrap_or(0);
                        ps.num = Some((ps.num.take().map(|n| n.0).unwrap_or_default(), l));
                    },
                    b"numId" if in_ppr_numpr => if let Some(ps) = paras.last_mut() {
                        let id = attr(e.attributes(), "val").unwrap_or_default();
                        ps.num = Some((id, ps.num.take().map(|n| n.1).unwrap_or(0)));
                    },
                    b"r" if is_start => props = RunProps::default(),
                    b"b" if in_rpr => props.bold = !matches!(attr(e.attributes(), "val").as_deref(), Some("0") | Some("false")),
                    b"i" if in_rpr => props.italic = !matches!(attr(e.attributes(), "val").as_deref(), Some("0") | Some("false")),
                    b"u" if in_rpr => props.underline = !matches!(attr(e.attributes(), "val").as_deref(), Some("none") | Some("0")),
                    b"sz" if in_rpr => props.size = attr(e.attributes(), "val").and_then(|v| v.parse::<f32>().ok()).map(|h| h / 2.0),
                    b"color" if in_rpr => props.color = attr(e.attributes(), "val").and_then(|v| parse_hex(&v)),
                    b"rFonts" if in_rpr => {
                        let f = attr(e.attributes(), "ascii").unwrap_or_default().to_lowercase();
                        props.mono = f.contains("courier") || f.contains("consolas") || f.contains("mono");
                    }
                    b"t" if is_start => in_t = true,
                    b"tab" => if let Some(ps) = paras.last_mut() { if !in_ppr { push_text(ps, &props, &link, "\t"); } },
                    b"br" => if let Some(ps) = paras.last_mut() {
                        if attr(e.attributes(), "type").as_deref() == Some("page") { ps.page_break_after = true; } else { push_text(ps, &props, &link, "\n"); }
                    },
                    b"cr" => if let Some(ps) = paras.last_mut() { push_text(ps, &props, &link, "\n"); },
                    b"hyperlink" if is_start => link = attr(e.attributes(), "id").and_then(|id| rels.get(&id)).filter(|(_, ext)| *ext).map(|(t, _)| t.clone()),
                    b"extent" => extent = (attr(e.attributes(), "cx").and_then(|v| v.parse::<f32>().ok()).map(|v| v / EMU_PER_PT),
                                           attr(e.attributes(), "cy").and_then(|v| v.parse::<f32>().ok()).map(|v| v / EMU_PER_PT)),
                    b"blip" => if let (Some(rid), Some(ps)) = (attr(e.attributes(), "embed"), paras.last_mut()) {
                        if let Some(img) = load_image(&mut zip, &rid, extent) { ps.images.push(img); }
                    },
                    b"pgSz" => {
                        page_w = attr(e.attributes(), "w").and_then(|v| v.parse::<f32>().ok()).map(|t| t / 20.0);
                        page_h = attr(e.attributes(), "h").and_then(|v| v.parse::<f32>().ok()).map(|t| t / 20.0);
                    }
                    b"pgMar" => for (i, k) in ["top", "right", "bottom", "left"].iter().enumerate() {
                        if let Some(v) = attr(e.attributes(), k).and_then(|v| v.parse::<f32>().ok()) { margins[i] = (v / 20.0).clamp(0.0, 200.0); }
                    },
                    b"tbl" if is_start => tables.push(TableBuilder { rows: vec![], row: vec![] }),
                    b"tr" if is_start => if let Some(t) = tables.last_mut() { t.row = vec![]; },
                    b"tc" if is_start => { containers.push(vec![]); spans.push(1); }
                    b"gridSpan" => if let Some(s) = spans.last_mut() { *s = attr(e.attributes(), "val").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 1024); },
                    _ => {}
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                b"pPr" => in_ppr = false,
                b"numPr" => in_ppr_numpr = false,
                b"rPr" => in_rpr = false,
                b"t" => in_t = false,
                b"hyperlink" => link = None,
                b"drawing" | b"pict" => extent = (None, None),
                b"p" => if let Some(ps) = paras.pop() {
                    let mut p = ps.para;
                    let style = ps.style.as_deref();
                    p.heading = style.and_then(|s| styles.heading_level(s)).or(ps.outline);
                    if p.heading.is_none() {
                        let ordered = match &ps.num {
                            Some((id, lvl)) if id != "0" => Some((numbering.get(&(id.clone(), *lvl)).copied().unwrap_or(false), *lvl)),
                            _ => style.and_then(|s| styles.list_kind(s)).map(|o| (o, 0)),
                        };
                        p.list = ordered.map(|(ordered, level)| ListInfo { ordered, level });
                    }
                    let keep = !p.runs.is_empty() || !ps.images.is_empty() || ps.page_break_after || containers.len() == 1;
                    let c = containers.last_mut().expect("body container");
                    if keep { c.push(Block::Para(p)); }
                    let c = containers.last_mut().expect("body container");
                    for img in ps.images { c.push(Block::Image(img)); }
                    if ps.page_break_after { c.push(Block::PageBreak); }
                },
                b"tc" => if let (Some(blocks), Some(span)) = (containers.pop(), spans.pop()) {
                    cells_seen += 1;
                    if cells_seen > cfg.max_table_cells { return Err(crate::Error::new(crate::ErrorCode::ETooLarge, format!("document holds more than {} table cells", cfg.max_table_cells))); }
                    if let Some(t) = tables.last_mut() { t.row.push(Cell { blocks, col_span: span }); }
                },
                b"tr" => if let Some(t) = tables.last_mut() { let row = std::mem::take(&mut t.row); t.rows.push(row); },
                b"tbl" => if let Some(t) = tables.pop() {
                    containers.last_mut().expect("container").push(Block::Table(Table { rows: t.rows }));
                },
                _ => {}
            },
            Event::Text(t) if in_t => if let Some(ps) = paras.last_mut() {
                let s = t.decode().map_err(|e| bad(e.to_string()))?;
                push_text(ps, &props, &link, &s);
            },
            Event::GeneralRef(rf) if in_t => if let Some(ps) = paras.last_mut() {
                let ch = match rf.resolve_char_ref().map_err(|e| bad(e.to_string()))? {
                    Some(c) => Some(c),
                    None => match rf.decode().map_err(|e| bad(e.to_string()))?.as_ref() {
                        "amp" => Some('&'), "lt" => Some('<'), "gt" => Some('>'), "quot" => Some('"'), "apos" => Some('\''), _ => None,
                    },
                };
                if let Some(c) = ch { push_text(ps, &props, &link, &c.to_string()); }
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    let mut blocks = containers.into_iter().next().unwrap_or_default();
    // Trailing empty paragraphs are noise.
    while matches!(blocks.last(), Some(Block::Para(p)) if p.is_blank() && p.heading.is_none()) { blocks.pop(); }
    let page = match (page_w, page_h) { (Some(w), Some(h)) if w > 100.0 && h > 100.0 => Some(PageBox { width_pt: w, height_pt: h, margins }), _ => None };
    Ok(Doc { blocks, page, base_size_pt: styles.base_size.filter(|s| (6.0..=40.0).contains(s)) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    pub fn make_docx(body: &str) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(vec![]);
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let o = zip::write::SimpleFileOptions::default();
            z.start_file("word/document.xml", o).unwrap();
            write!(z, "<?xml version=\"1.0\"?><w:document xmlns:w=\"w\" xmlns:r=\"r\"><w:body>{body}</w:body></w:document>").unwrap();
            z.start_file("word/styles.xml", o).unwrap();
            z.write_all(br#"<w:styles xmlns:w="w"><w:style w:styleId="H1"><w:name w:val="heading 1"/></w:style></w:styles>"#).unwrap();
            z.start_file("word/numbering.xml", o).unwrap();
            z.write_all(br#"<w:numbering xmlns:w="w"><w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum><w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum><w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num><w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num></w:numbering>"#).unwrap();
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test] fn reads_headings_runs_lists_and_tables() {
        let bytes = make_docx(concat!(
            r#"<w:p><w:pPr><w:pStyle w:val="H1"/></w:pPr><w:r><w:t>Title &amp; more</w:t></w:r></w:p>"#,
            r#"<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>bold</w:t></w:r><w:r><w:t xml:space="preserve"> plain</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/></w:numPr></w:pPr><w:r><w:t>one</w:t></w:r></w:p>"#,
            r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="2"/></w:numPr></w:pPr><w:r><w:t>dot</w:t></w:r></w:p>"#,
            r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>a</w:t></w:r></w:p></w:tc><w:tc><w:tcPr><w:gridSpan w:val="2"/></w:tcPr><w:p><w:r><w:t>b</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#,
        ));
        let doc = read(std::io::Cursor::new(bytes), &Config::default()).unwrap();
        let Block::Para(h) = &doc.blocks[0] else { panic!() };
        assert_eq!((h.heading, h.text().as_str()), (Some(1), "Title & more"));
        let Block::Para(p) = &doc.blocks[1] else { panic!() };
        assert!(p.runs[0].bold && !p.runs[1].bold);
        assert_eq!(p.text(), "bold plain");
        let Block::Para(l1) = &doc.blocks[2] else { panic!() };
        let Block::Para(l2) = &doc.blocks[3] else { panic!() };
        assert_eq!(l1.list.as_ref().map(|l| l.ordered), Some(true));
        assert_eq!(l2.list.as_ref().map(|l| l.ordered), Some(false));
        let Block::Table(t) = &doc.blocks[4] else { panic!() };
        assert_eq!(t.rows[0][1].col_span, 2);
        assert_eq!(t.to_text(), vec![vec!["a", "b", ""]]);
    }
}
