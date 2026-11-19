//! Tables out of `word/document.xml`. Straight port of `docx-tables.ts`: a hand-rolled tag
//! scanner (not a DOM) so memory stays bounded, with gridSpan/vMerge handling and hard caps
//! on cells and tables (a 138KB document can otherwise declare millions of grid positions).
use std::collections::HashMap;

const MAX_GRID_SPAN: usize = 16_384;

#[derive(Clone, Copy, PartialEq)]
enum VMerge { Restart, Continue }

struct Cell { paragraphs: Vec<String>, open: Option<String>, grid_span: usize, vmerge: Option<VMerge> }

#[derive(Default)]
struct Frame {
    rows: Vec<Vec<String>>,
    row: Option<Vec<String>>,
    cell: Option<Cell>,
    carry: HashMap<usize, String>,
    column: usize,
}

pub enum Extraction {
    Ok(Vec<Vec<Vec<String>>>),
    TooLarge { cells: bool },
}

fn decode_xml_text(v: &str) -> String {
    if !v.contains('&') { return v.to_string(); }
    let mut out = String::with_capacity(v.len());
    let mut rest = v;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let decoded = tail.find(';').filter(|&e| e > 1 && e - 1 <= 12).and_then(|e| {
            let body = &tail[1..e];
            let r = if let Some(h) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) {
                u32::from_str_radix(h, 16).ok().and_then(legal_char)
            } else if let Some(d) = body.strip_prefix('#') {
                d.parse::<u32>().ok().and_then(legal_char)
            } else {
                match body { "amp" => Some('&'), "lt" => Some('<'), "gt" => Some('>'), "quot" => Some('"'),
                             "apos" => Some('\''), "nbsp" => Some('\u{a0}'), _ => None }
            };
            r.map(|c| (c, e + 1))
        });
        match decoded {
            Some((c, len)) => { out.push(c); rest = &tail[len..]; }
            None => { out.push('&'); rest = &tail[1..]; }
        }
    }
    out.push_str(rest);
    out
}

pub fn is_legal_xml_char(cp: u32) -> bool {
    matches!(cp, 0x09 | 0x0a | 0x0d) || (0x20..=0xd7ff).contains(&cp)
        || (0xe000..=0xfffd).contains(&cp) || (0x10000..=0x10ffff).contains(&cp)
}
fn legal_char(cp: u32) -> Option<char> { if is_legal_xml_char(cp) { char::from_u32(cp) } else { None } }

fn find_tag_end(xml: &[u8], from: usize) -> Option<usize> {
    let mut quote = 0u8;
    for (i, &c) in xml.iter().enumerate().skip(from + 1) {
        if quote != 0 { if c == quote { quote = 0; } continue; }
        if c == b'"' || c == b'\'' { quote = c; continue; }
        if c == b'>' { return Some(i); }
    }
    None
}

/// `w:val="…"` value from an attribute string.
fn w_val(attrs: &str) -> Option<&str> {
    let i = attrs.find("w:val")?;
    if i > 0 && !attrs.as_bytes()[i - 1].is_ascii_whitespace() { return None; }
    let rest = attrs[i + 5..].trim_start().strip_prefix('=')?.trim_start();
    let q = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
    let rest = &rest[1..];
    Some(&rest[..rest.find(q)?])
}

pub fn extract_tables(xml: &str, max_cells: usize, max_tables: usize) -> Extraction {
    let b = xml.as_bytes();
    let mut frames: Vec<Frame> = vec![];      // every table, in document order
    let mut open: Vec<usize> = vec![];        // indices into `frames` of currently-open tables
    let (mut text_start, mut cells, mut tables) = (None::<usize>, 0usize, 0usize);

    fn close_cell(f: &mut Frame, cells: &mut usize) {
        let Some(cell) = f.cell.take() else { return };
        let mut paragraphs = cell.paragraphs;
        if let Some(o) = cell.open { paragraphs.push(o); }
        let mut value = paragraphs.join("\n").trim().to_string();
        if cell.vmerge == Some(VMerge::Continue) {
            value = f.carry.get(&f.column).cloned().unwrap_or_default();
        } else { f.carry.insert(f.column, value.clone()); }
        let row = f.row.get_or_insert_with(Vec::new);
        for _ in 0..cell.grid_span {
            if row.len() <= f.column { row.resize(f.column + 1, String::new()); }
            row[f.column] = value.clone();
            if cell.vmerge == Some(VMerge::Restart) { f.carry.insert(f.column, value.clone()); }
            f.column += 1;
            *cells += 1;
        }
    }
    fn close_row(f: &mut Frame, cells: &mut usize) {
        if f.cell.is_some() { close_cell(f, cells); }
        if let Some(r) = f.row.take() { f.rows.push(r); *cells += 1; }
    }

    let mut at = 0;
    while at < b.len() {
        if cells > max_cells { return Extraction::TooLarge { cells: true }; }
        if tables > max_tables { return Extraction::TooLarge { cells: false }; }
        let Some(rel) = xml[at..].find('<') else { break };
        let tag_start = at + rel;
        let Some(tag_end) = find_tag_end(b, tag_start) else { break };
        let raw = &xml[tag_start + 1..tag_end];
        at = tag_end + 1;

        let self_closing = raw.ends_with('/');
        let body = if self_closing { &raw[..raw.len() - 1] } else { raw };
        let closing = body.starts_with('/');
        let unclosed = if closing { &body[1..] } else { body };
        let name_end = unclosed.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(unclosed.len());
        let name = &unclosed[..name_end];
        let attrs = if closing { "" } else { &unclosed[name_end..] };

        if name == "w:tbl" {
            if closing {
                if let Some(i) = open.pop() { close_row(&mut frames[i], &mut cells); }
            } else {
                frames.push(Frame::default());
                open.push(frames.len() - 1);
                tables += 1;
                if self_closing { let i = open.pop().unwrap(); close_row(&mut frames[i], &mut cells); }
            }
            continue;
        }
        let Some(&fi) = open.last() else { continue };
        let f = &mut frames[fi];
        match name {
            "w:tr" => if closing { close_row(f, &mut cells) } else {
                f.row = Some(vec![]); f.column = 0; f.cell = None;
                if self_closing { close_row(f, &mut cells); }
            },
            "w:tc" => if closing { close_cell(f, &mut cells) } else {
                f.cell = Some(Cell { paragraphs: vec![], open: None, grid_span: 1, vmerge: None });
                if self_closing { close_cell(f, &mut cells); }
            },
            "w:gridSpan" if !closing => if let Some(c) = &mut f.cell {
                if let Ok(s) = w_val(attrs).unwrap_or("1").trim().parse::<usize>() {
                    if s > 0 { c.grid_span = s.min(MAX_GRID_SPAN); }
                }
            },
            "w:vMerge" if !closing => if let Some(c) = &mut f.cell {
                c.vmerge = Some(if w_val(attrs) == Some("restart") { VMerge::Restart } else { VMerge::Continue });
            },
            "w:p" => if let Some(c) = &mut f.cell {
                if self_closing { c.paragraphs.push(String::new()); }
                else if closing { c.paragraphs.push(c.open.take().unwrap_or_default()); }
                else { c.open = Some(String::new()); }
            },
            "w:t" => if closing {
                if let (Some(s), Some(c)) = (text_start, &mut f.cell) {
                    c.open.get_or_insert_with(String::new).push_str(&decode_xml_text(&xml[s..tag_start]));
                }
                text_start = None;
            } else { text_start = Some(at); },
            _ => {}
        }
    }
    Extraction::Ok(frames.into_iter().filter(|f| !f.rows.is_empty()).map(|f| f.rows).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tc(t: &str) -> String { format!("<w:tc><w:p><w:r><w:t>{t}</w:t></w:r></w:p></w:tc>") }
    fn run(x: &str) -> Vec<Vec<Vec<String>>> {
        match extract_tables(x, 1000, 100) { Extraction::Ok(t) => t, _ => panic!("too large") }
    }
    #[test] fn simple_table() {
        let x = format!("<w:tbl><w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>", tc("a"), tc("b"), tc("c"), tc("d&amp;e"));
        assert_eq!(run(&x), vec![vec![vec!["a", "b"], vec!["c", "d&e"]]]);
    }
    #[test] fn grid_span_repeats_value() {
        let x = "<w:tbl><w:tr><w:tc><w:tcPr><w:gridSpan w:val=\"2\"/></w:tcPr><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc></w:tr></w:tbl>";
        assert_eq!(run(x), vec![vec![vec!["x", "x"]]]);
    }
    #[test] fn vertical_merge_carries_down() {
        let top = "<w:tc><w:tcPr><w:vMerge w:val=\"restart\"/></w:tcPr><w:p><w:r><w:t>m</w:t></w:r></w:p></w:tc>";
        let cont = "<w:tc><w:tcPr><w:vMerge/></w:tcPr><w:p/></w:tc>";
        let x = format!("<w:tbl><w:tr>{top}</w:tr><w:tr>{cont}</w:tr></w:tbl>");
        assert_eq!(run(&x), vec![vec![vec!["m"], vec!["m"]]]);
    }
    #[test] fn empty_tables_are_dropped_and_capped() {
        assert!(run("<w:tbl></w:tbl>").is_empty());
        let many = "<w:tbl></w:tbl>".repeat(5);
        assert!(matches!(extract_tables(&many, 1000, 3), Extraction::TooLarge { cells: false }));
        let big = format!("<w:tbl><w:tr>{}</w:tr></w:tbl>", tc("a").repeat(10));
        assert!(matches!(extract_tables(&big, 5, 100), Extraction::TooLarge { cells: true }));
    }
}
