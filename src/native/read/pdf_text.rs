//! Text, headings, lists and tables out of a PDF (no OCR): hayro's interpreter supplies each glyph's Unicode and
//! position, and this module rebuilds lines, paragraphs and tables from where the glyphs sit on the page.
use super::bad;
use crate::native::model::*;
use crate::{Error, ErrorCode, Result};
use hayro::hayro_interpret::kurbo::{BezPath, Point, Shape};
use hayro::hayro_interpret::util::TransformExt;
use hayro::hayro_interpret::{
    interpret_page, BlendMode, ClipPath, Context, Device, DrawMode, DrawProps, Image as PdfImage, ImageDrawProps,
    InterpreterCache, InterpreterSettings, SoftMask,
};
use hayro::hayro_interpret::font::{Glyph, GlyphRun};
use hayro::hayro_syntax::Pdf;

const PLACEHOLDER: char = '\u{fffc}';

#[derive(Debug, Clone)]
pub struct G { pub text: String, pub x0: f32, pub x1: f32, pub y: f32, pub size: f32 }

/// A vertical ruling line (table cell border): x, y-top, y-bottom.
pub type VRule = (f32, f32, f32);

pub struct PageGlyphs { pub glyphs: Vec<G>, pub rules: Vec<VRule>, pub width: f32, pub height: f32, pub has_images: bool }

// ---- glyph collection -----------------------------------------------------------------------------------------

struct Collector { glyphs: Vec<G>, rules: Vec<VRule>, images: bool }

impl<'a> Device<'a> for Collector {
    fn draw_path(&mut self, path: &BezPath, props: DrawProps<'a>, _: &DrawMode) {
        // Thin tall shapes are vertical borders; a stroked or filled rectangle contributes its left and right edges.
        let bb = (props.transform * path.clone()).bounding_box();
        let (w, h) = (bb.width() as f32, bb.height() as f32);
        if w < 2.0 && h > 6.0 { self.rules.push((((bb.x0 + bb.x1) / 2.0) as f32, bb.y0 as f32, bb.y1 as f32)); }
        else if w >= 2.0 && h >= 6.0 && w < 800.0 && path.elements().len() <= 6 && h < 400.0 {
            // A cell outline drawn as a rectangle path (stroked): both vertical edges.
            self.rules.push((bb.x0 as f32, bb.y0 as f32, bb.y1 as f32));
            self.rules.push((bb.x1 as f32, bb.y0 as f32, bb.y1 as f32));
        }
    }
    fn push_clip_path(&mut self, _: &ClipPath) {}
    fn push_transparency_group(&mut self, _: f32, _: Option<SoftMask<'a>>, _: BlendMode) {}
    fn draw_glyph_run(&mut self, run: &GlyphRun<'_, 'a>, props: DrawProps<'a>, _: &DrawMode) {
        for g in run.glyphs() {
            let outline_bb = if let Glyph::Outline(og) = &**g { Some(og.outline().bounding_box()) } else { None };
            // A glyph with no Unicode value: an empty outline is a space; otherwise it is usually a bullet from a symbol
            // font, so keep a placeholder for the line builder (a bullet at the start of a line, nothing elsewhere).
            let text = match g.as_unicode() {
                Some(hayro::hayro_interpret::hayro_cmap::BfString::Char(c)) => c.to_string(),
                Some(hayro::hayro_interpret::hayro_cmap::BfString::String(s)) => s,
                None if outline_bb.is_some_and(|b| b.width() <= 0.0) => " ".to_string(),
                None => PLACEHOLDER.to_string(),
            };
            if text.is_empty() { continue; }
            let m = props.transform * g.transform();
            let (o, u) = (m * Point::new(0.0, 0.0), m * Point::new(1000.0, 0.0));
            let size = ((u.x - o.x).powi(2) + (u.y - o.y).powi(2)).sqrt() as f32;
            if !(size.is_finite()) || size < 0.5 { continue; }
            let scale = size / 1000.0;
            let (mut x0, mut x1) = (o.x as f32, o.x as f32 + 0.5 * size);
            if let Some(bb) = outline_bb {
                if bb.width() > 0.0 { x0 = o.x as f32 + bb.x0 as f32 * scale; x1 = o.x as f32 + bb.x1 as f32 * scale; }
            }
            // CJK glyphs advance a full em; their ink is narrower, which would read as a word gap.
            if text.chars().next().is_some_and(is_cjk) { x0 = o.x as f32; x1 = o.x as f32 + size; }
            self.glyphs.push(G { text, x0, x1, y: o.y as f32, size });
        }
    }
    fn draw_image(&mut self, _: PdfImage<'a, '_>, _: ImageDrawProps<'a>) { self.images = true; }
    fn pop_clip(&mut self) {}
    fn pop_transparency_group(&mut self) {}
}

pub fn extract_glyphs(bytes: Vec<u8>, max_pages: usize) -> Result<Vec<PageGlyphs>> {
    let pdf = Pdf::new(bytes).map_err(|e| match format!("{e:?}") {
        s if s.to_lowercase().contains("encrypt") => Error::new(ErrorCode::EEncrypted, "This PDF is password protected."),
        s => bad(format!("not a readable PDF: {s}")),
    })?;
    let pages = pdf.pages();
    if pages.len() > max_pages.max(500) {
        return Err(Error::new(ErrorCode::ETooLarge, format!("This document has {} pages; the limit is {}.", pages.len(), max_pages.max(500))));
    }
    let cache = InterpreterCache::new();
    let mut out = vec![];
    for page in pages.iter() {
        let (w, h) = page.render_dimensions();
        let transform = page.initial_transform(true).to_kurbo();
        let bbox = hayro::hayro_interpret::kurbo::Rect::new(0.0, 0.0, w as f64, h as f64);
        let mut ctx = Context::new(transform, bbox, &cache, page.xref(), InterpreterSettings::default());
        let mut dev = Collector { glyphs: vec![], rules: vec![], images: false };
        interpret_page(page, &mut ctx, &mut dev);
        out.push(PageGlyphs { glyphs: dev.glyphs, rules: dev.rules, width: w, height: h, has_images: dev.images });
    }
    Ok(out)
}

// ---- layout analysis ------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Seg { x0: f32, x1: f32, text: String }

#[derive(Debug, Clone)]
struct Line { y: f32, size: f32, segs: Vec<Seg> }

impl Line {
    fn x0(&self) -> f32 { self.segs.first().map_or(0.0, |s| s.x0) }
    fn x1(&self) -> f32 { self.segs.last().map_or(0.0, |s| s.x1) }
    fn text(&self) -> String { self.segs.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join(" ") }
}

fn is_space(s: &str) -> bool { s.chars().all(|c| c.is_whitespace()) }
fn is_cjk(c: char) -> bool { matches!(c as u32, 0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff | 0xff00..=0xffef) }

fn build_lines(mut glyphs: Vec<G>, rules: &[VRule]) -> Vec<Line> {
    // Fake bold / drop shadows draw the same glyph twice at (almost) the same place.
    glyphs.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap().then(a.x0.partial_cmp(&b.x0).unwrap()));
    let mut clusters: Vec<Vec<G>> = vec![];
    let mut cur_y = f32::NEG_INFINITY;
    for g in glyphs {
        let tol = 0.5 * g.size;
        match clusters.last_mut() {
            Some(c) if (g.y - cur_y).abs() <= tol => { let n = c.len() as f32; cur_y = (cur_y * n + g.y) / (n + 1.0); c.push(g); }
            _ => { cur_y = g.y; clusters.push(vec![g]); }
        }
    }
    let mut lines = vec![];
    for mut c in clusters {
        c.sort_by(|a, b| a.x0.partial_cmp(&b.x0).unwrap());
        let mut segs: Vec<Seg> = vec![];
        let (mut prev_x1, mut pending_space, mut prev_text): (Option<f32>, bool, Option<(String, f32)>) = (None, false, None);
        let mut size_chars: Vec<(f32, usize)> = vec![];
        let line_y = c.iter().map(|g| g.y).sum::<f32>() / c.len() as f32;
        // Real space glyphs are honoured; otherwise an ink gap wider than a quarter em is a word break (letters of
        // loosely spaced fonts reach ~0.2em, a space plus side bearings is at least ~0.3em).
        let space_gap = 0.25;
        for (gi, g) in c.iter().enumerate() {
            if is_space(&g.text) { pending_space = true; continue; }
            // Symbol-font bullets arrive as private-use or unmapped glyphs: at the start of a line they are a bullet.
            let is_symbol = g.text.chars().all(|ch| ch == PLACEHOLDER || ('\u{e000}'..='\u{f8ff}').contains(&ch));
            if is_symbol {
                if gi == 0 { segs.push(Seg { x0: g.x0, x1: g.x1, text: "•".into() }); prev_x1 = Some(g.x1); pending_space = true; prev_text = Some((g.text.clone(), g.x0)); }
                continue;
            }
            if let Some((t, x)) = &prev_text { if *t == g.text && (g.x0 - x).abs() < 0.25 * g.size { continue; } }
            let gap = prev_x1.map(|p| g.x0 - p);
            let ruled = prev_x1.is_some_and(|p| rules.iter().any(|r| r.0 > p - 0.5 && r.0 < g.x0 + 0.5 && line_y >= r.1 - 3.0 && line_y <= r.2 + 3.0));
            let new_seg = gap.is_some_and(|d| d > 1.5 * g.size) || ruled || segs.is_empty();
            if new_seg { segs.push(Seg { x0: g.x0, x1: g.x1, text: String::new() }); }
            else if pending_space || gap.is_some_and(|d| d > space_gap * g.size) {
                let seg = segs.last_mut().unwrap();
                // No space between two CJK characters.
                let both_cjk = seg.text.chars().last().is_some_and(is_cjk) && g.text.chars().next().is_some_and(is_cjk);
                if !both_cjk && !pending_space_is_redundant(&seg.text) { seg.text.push(' '); }
            }
            let seg = segs.last_mut().unwrap();
            seg.text.push_str(&g.text);
            seg.x1 = seg.x1.max(g.x1);
            prev_x1 = Some(g.x1); pending_space = false; prev_text = Some((g.text.clone(), g.x0));
            let r = (g.size * 2.0).round() / 2.0;
            match size_chars.iter_mut().find(|(s, _)| *s == r) { Some(e) => e.1 += g.text.chars().count(), None => size_chars.push((r, g.text.chars().count())) }
        }
        segs.retain(|s| !s.text.trim().is_empty());
        if segs.is_empty() { continue; }
        let size = size_chars.iter().max_by_key(|(_, n)| *n).map(|(s, _)| *s).unwrap_or(10.0);
        let y = c.iter().map(|g| g.y).sum::<f32>() / c.len() as f32;
        lines.push(Line { y, size, segs });
    }
    lines
}

fn pending_space_is_redundant(s: &str) -> bool { s.ends_with(' ') }

fn body_size(pages: &[Vec<Line>]) -> f32 {
    let mut hist: Vec<(f32, usize)> = vec![];
    for l in pages.iter().flatten() {
        let n = l.text().chars().count();
        match hist.iter_mut().find(|(s, _)| *s == l.size) { Some(e) => e.1 += n, None => hist.push((l.size, n)) }
    }
    hist.into_iter().max_by_key(|(_, n)| *n).map(|(s, _)| s).unwrap_or(11.0)
}

/// Lines that repeat at the top/bottom of most pages (running headers, footers, page numbers).
fn drop_running_lines(pages: &mut [(Vec<Line>, f32)]) {
    if pages.len() < 3 { return; }
    let norm = |t: &str| t.chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect::<String>();
    let mut seen: std::collections::HashMap<String, usize> = Default::default();
    for (lines, h) in pages.iter() {
        let mut keys = std::collections::HashSet::new();
        for l in lines { if l.y < 0.09 * h || l.y > 0.91 * h { keys.insert(norm(&l.text())); } }
        for k in keys { *seen.entry(k).or_default() += 1; }
    }
    let need = (pages.len() as f32 * 0.5).ceil() as usize;
    for (lines, h) in pages.iter_mut() {
        let h = *h;
        lines.retain(|l| !((l.y < 0.09 * h || l.y > 0.91 * h) && seen.get(&norm(&l.text())).is_some_and(|n| *n >= need.max(3))));
    }
}

/// `Some((ordered, marker byte length))` when the text starts with a list marker.
fn list_marker(t: &str) -> Option<(bool, usize)> {
    let mut it = t.char_indices();
    let (_, c0) = it.next()?;
    if matches!(c0, '•' | '◦' | '▪' | '●' | '○' | '■' | '·' | '➢' | '–' | '—' | '-' | '*' | '▶' | '►') {
        let rest = &t[c0.len_utf8()..];
        if rest.starts_with(' ') && rest.trim().len() > 0 { return Some((false, c0.len_utf8() + 1)); }
        return None;
    }
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    let (label_len, ok) = if (1..=3).contains(&digits) { (digits, true) }
        else if t.chars().next().is_some_and(|c| c.is_ascii_lowercase()) && t.chars().nth(1).is_some_and(|c| c == '.' || c == ')') { (1, true) } else { (0, false) };
    if ok {
        let after: Vec<char> = t.chars().skip(label_len).take(2).collect();
        if after.len() == 2 && (after[0] == '.' || after[0] == ')') && after[1] == ' ' { return Some((true, label_len + 2)); }
    }
    None
}

struct Acc { text: String, last_y: f32, last_size: f32, last_x1: f32, left: f32, list: Option<ListInfo>, base_left: f32 }

fn join_line(acc: &mut String, next: &str) {
    if acc.ends_with('-') && acc.chars().rev().nth(1).is_some_and(|c| c.is_alphabetic()) && next.chars().next().is_some_and(|c| c.is_lowercase()) {
        acc.pop();
        acc.push_str(next);
        return;
    }
    let (a, b) = (acc.chars().last(), next.chars().next());
    if a.is_some_and(is_cjk) && b.is_some_and(is_cjk) { acc.push_str(next); } else { acc.push(' '); acc.push_str(next); }
}

fn table_from_rows(rows: &[&Line]) -> Option<Table> {
    // Column intervals: project every segment onto the x axis and merge overlaps.
    let mut spans: Vec<(f32, f32)> = rows.iter().flat_map(|l| l.segs.iter().map(|s| (s.x0, s.x1))).collect();
    spans.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    let mut cols: Vec<(f32, f32)> = vec![];
    for (a, b) in spans { match cols.last_mut() { Some(c) if a <= c.1 => c.1 = c.1.max(b), _ => cols.push((a, b)) } }
    if cols.len() < 2 { return None; }
    let seg_chars: Vec<usize> = rows.iter().flat_map(|l| l.segs.iter().map(|s| s.text.chars().count())).collect();
    if seg_chars.iter().sum::<usize>() as f32 / seg_chars.len() as f32 > 50.0 { return None; } // columns of prose, not a table
    let mut out = vec![];
    for l in rows {
        let mut cells = vec![String::new(); cols.len()];
        for s in &l.segs {
            let mid = (s.x0 + s.x1) / 2.0;
            let ci = cols.iter().position(|c| mid >= c.0 && mid <= c.1).unwrap_or(0);
            if !cells[ci].is_empty() { cells[ci].push(' '); }
            cells[ci].push_str(&s.text);
        }
        out.push(cells);
    }
    // Every column must carry something in at least two rows, or it is just stray text.
    for ci in 0..cols.len() { if out.iter().filter(|r| !r[ci].is_empty()).count() < 2 { return None; } }
    Some(Table::from_text(&out))
}

struct Band { y0: f32, y1: f32, xs: Vec<f32> }

/// Tables drawn with cell borders: vertical rules are grouped into row bands, contiguous bands form a table, and
/// every glyph whose centre falls in a cell belongs to that cell. Returns the glyphs left over and `(top y, table)`.
fn extract_ruled(glyphs: Vec<G>, rules: &[VRule]) -> (Vec<G>, Vec<(f32, Table)>) {
    // Borders are often stroked twice (both sides of a hairline): merge near-identical rules.
    let mut rs: Vec<VRule> = vec![];
    for &(x, a, b) in rules {
        let (a, b) = (a.min(b), a.max(b));
        if b - a < 6.0 { continue; }
        if let Some(r) = rs.iter_mut().find(|r| (r.0 - x).abs() < 1.5 && (r.1 - a).abs() < 2.0 && (r.2 - b).abs() < 2.0) { r.0 = (r.0 + x) / 2.0; continue; }
        rs.push((x, a, b));
    }
    rs.sort_by(|p, q| p.1.partial_cmp(&q.1).unwrap().then(p.0.partial_cmp(&q.0).unwrap()));
    let mut bands: Vec<Band> = vec![];
    for (x, a, b) in rs {
        match bands.iter_mut().find(|bd| (bd.y0 - a).abs() < 2.0 && (bd.y1 - b).abs() < 2.0) {
            Some(bd) => bd.xs.push(x),
            None => bands.push(Band { y0: a, y1: b, xs: vec![x] }),
        }
    }
    bands.retain(|b| b.xs.len() >= 3);
    bands.sort_by(|a, b| a.y0.partial_cmp(&b.y0).unwrap());
    // Contiguous bands are one table.
    let mut groups: Vec<Vec<Band>> = vec![];
    for b in bands {
        match groups.last_mut() { Some(g) if b.y0 <= g.last().unwrap().y1 + 3.0 => g.push(b), _ => groups.push(vec![b]) }
    }
    let mut rest = glyphs;
    let mut tables = vec![];
    for g in groups {
        let (top, bottom) = (g[0].y0, g.iter().map(|b| b.y1).fold(f32::MIN, f32::max));
        let mut cols: Vec<f32> = g.iter().flat_map(|b| b.xs.iter().copied()).collect();
        cols.sort_by(|a, b| a.partial_cmp(b).unwrap());
        cols.dedup_by(|a, b| (*a - *b).abs() < 2.0);
        let (left, right) = (cols[0] - 2.0, cols[cols.len() - 1] + 2.0);
        let (inside, outside): (Vec<G>, Vec<G>) = rest.into_iter().partition(|gl| {
            let cx = (gl.x0 + gl.x1) / 2.0;
            gl.y >= top - 1.0 && gl.y <= bottom + 2.0 && cx >= left && cx <= right
        });
        rest = outside;
        let mut rows: Vec<Vec<Cell>> = vec![];
        for band in &g {
            let mut xs = band.xs.clone();
            xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            xs.dedup_by(|a, b| (*a - *b).abs() < 2.0);
            let mut row = vec![];
            for w in xs.windows(2) {
                let in_cell: Vec<G> = inside.iter().filter(|gl| {
                    let cx = (gl.x0 + gl.x1) / 2.0;
                    gl.y >= band.y0 - 1.0 && gl.y <= band.y1 + 2.0 && cx >= w[0] - 1.0 && cx < w[1] + 1.0
                }).cloned().collect();
                let span = cols.iter().filter(|c| **c > w[0] + 2.0 && **c < w[1] - 2.0).count() + 1;
                let lines = build_lines(in_cell, &[]);
                let mut paras: Vec<Block> = vec![];
                let mut cur: Option<(String, f32, f32)> = None;
                for l in &lines {
                    let t = l.text();
                    match &mut cur {
                        Some((txt, y, size)) if l.y - *y <= 1.6 * (*size).max(l.size) => { join_line(txt, t.trim()); *y = l.y; *size = l.size; }
                        _ => {
                            if let Some((txt, _, _)) = cur.take() { paras.push(Block::Para(Para { runs: vec![Run::plain(txt)], ..Default::default() })); }
                            cur = Some((t.trim().to_string(), l.y, l.size));
                        }
                    }
                }
                if let Some((txt, _, _)) = cur { paras.push(Block::Para(Para { runs: vec![Run::plain(txt)], ..Default::default() })); }
                if paras.is_empty() { paras.push(Block::Para(Para::default())); }
                row.push(Cell { blocks: paras, col_span: span });
            }
            if !row.is_empty() { rows.push(row); }
        }
        if !rows.is_empty() { tables.push((top, Table { rows })); }
    }
    (rest, tables)
}

fn analyse(pages: Vec<PageGlyphs>) -> Doc {
    let mut ruled_per_page: Vec<Vec<(f32, Table)>> = vec![];
    let mut per_page: Vec<(Vec<Line>, f32)> = pages.into_iter().map(|p| {
        let (rest, tables) = extract_ruled(p.glyphs, &p.rules);
        ruled_per_page.push(tables);
        (build_lines(rest, &p.rules), p.height)
    }).collect();
    drop_running_lines(&mut per_page);
    let all: Vec<Vec<Line>> = per_page.iter().map(|(l, _)| l.clone()).collect();
    let body = body_size(&all);
    // Heading sizes, largest first.
    let mut hs: Vec<f32> = all.iter().flatten().filter(|l| l.size >= body * 1.15 && l.segs.len() == 1 && l.text().chars().count() <= 150).map(|l| l.size).collect();
    hs.sort_by(|a, b| b.partial_cmp(a).unwrap()); hs.dedup();
    let level_of = |size: f32| hs.iter().position(|s| *s == size).map(|i| (i as u8 + 1).min(4));

    let mut doc = Doc { base_size_pt: Some(body.clamp(8.0, 14.0)), ..Default::default() };
    for (pi, lines) in all.iter().enumerate() {
        let mut ruled = std::mem::take(&mut ruled_per_page[pi]).into_iter().peekable();
        if pi > 0 && (!lines.is_empty() || ruled.peek().is_some()) { doc.blocks.push(Block::PageBreak); }
        let base_left = lines.iter().map(|l| l.x0()).fold(f32::MAX, f32::min);
        let mut x1s: Vec<f32> = lines.iter().filter(|l| (l.size - body).abs() < 0.9).map(|l| l.x1()).collect();
        x1s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let right_edge = x1s.get(x1s.len().saturating_sub(1) * 9 / 10).copied().unwrap_or(0.0);

        let mut acc: Option<Acc> = None;
        let mut last_heading: Option<(u8, f32)> = None;
        let flush = |acc: &mut Option<Acc>, doc: &mut Doc| if let Some(a) = acc.take() {
            doc.blocks.push(Block::Para(Para { runs: vec![Run::plain(a.text)], list: a.list, ..Default::default() }));
        };
        let mut i = 0;
        while i < lines.len() {
            let l = &lines[i];
            while ruled.peek().is_some_and(|(ty, _)| *ty < l.y) {
                flush(&mut acc, &mut doc);
                if let Some((_, t)) = ruled.next() { doc.blocks.push(Block::Table(t)); }
            }
            // Table: consecutive lines that each split into several segments.
            if l.segs.len() >= 2 {
                let mut j = i;
                while j + 1 < lines.len() && lines[j + 1].segs.len() >= 2 && lines[j + 1].y - lines[j].y <= 4.0 * lines[j].size.max(body) { j += 1; }
                if j > i {
                    let rows: Vec<&Line> = lines[i..=j].iter().collect();
                    if let Some(t) = table_from_rows(&rows) { flush(&mut acc, &mut doc); doc.blocks.push(Block::Table(t)); i = j + 1; continue; }
                }
            }
            let text = l.text();
            if l.segs.len() == 1 && l.size >= body * 1.15 && text.chars().count() <= 150 {
                if let Some(h) = level_of(l.size) {
                    // Consecutive lines of one heading size are one (wrapped) heading.
                    if let (Some((lh, ly)), Some(Block::Para(p))) = (last_heading, doc.blocks.last_mut()) {
                        if lh == h && acc.is_none() && l.y - ly <= 1.6 * l.size && p.heading == Some(h) {
                            let t = format!("{} {}", p.text(), text.trim());
                            p.runs = vec![Run::plain(t)];
                            last_heading = Some((h, l.y));
                            i += 1; continue;
                        }
                    }
                    flush(&mut acc, &mut doc);
                    doc.blocks.push(Block::Para(Para { runs: vec![Run::plain(text.trim().to_string())], heading: Some(h), ..Default::default() }));
                    last_heading = Some((h, l.y));
                    i += 1; continue;
                }
            }
            let marker = list_marker(&text);
            let starts_new = match &acc {
                None => true,
                Some(a) => {
                    let pitch = l.y - a.last_y;
                    let short_prev = a.last_x1 < right_edge - 3.0 * a.last_size;
                    let ends_sentence = a.text.ends_with(['.', '!', '?', ':', '。', '！', '？']);
                    marker.is_some() || pitch > 1.6 * a.last_size.max(l.size) || (l.size - a.last_size).abs() > 1.0
                        || (short_prev && ends_sentence) || l.x0() < a.left - 2.0 * l.size
                        || (a.list.is_none() && l.x0() > a.left + 1.5 * l.size && ends_sentence)
                }
            };
            if starts_new {
                flush(&mut acc, &mut doc);
                let (list, body_text) = match marker {
                    Some((ordered, n)) => {
                        let level = (((l.x0() - base_left) / (1.8 * l.size)).round() as i32).clamp(0, 3) as u8;
                        (Some(ListInfo { ordered, level }), text[n..].trim().to_string())
                    }
                    None => (None, text.trim().to_string()),
                };
                acc = Some(Acc { text: body_text, last_y: l.y, last_size: l.size, last_x1: l.x1(), left: l.x0(), list, base_left });
            } else if let Some(a) = &mut acc {
                join_line(&mut a.text, text.trim());
                a.last_y = l.y; a.last_size = l.size; a.last_x1 = l.x1();
            }
            i += 1;
        }
        flush(&mut acc, &mut doc);
        for (_, t) in ruled { doc.blocks.push(Block::Table(t)); }
    }
    merge_split_tables(&mut doc);
    doc
}

/// A table that runs over a page break is one table: join `[Table, PageBreak, Table]` when the column counts agree.
fn merge_split_tables(doc: &mut Doc) {
    let mut out: Vec<Block> = Vec::with_capacity(doc.blocks.len());
    let mut iter = std::mem::take(&mut doc.blocks).into_iter().peekable();
    while let Some(b) = iter.next() {
        if let Block::Table(mut a) = b {
            while matches!(iter.peek(), Some(Block::PageBreak)) {
                let mut look = iter.clone();
                look.next();
                match look.peek() {
                    Some(Block::Table(next)) if next.columns() == a.columns() => {
                        iter.next();
                        if let Some(Block::Table(mut n)) = iter.next() {
                            // A row split by the page break resumes with only some cells filled: fold it back.
                            let cont = n.rows.first().is_some_and(|r| r.len() == a.rows.last().map_or(0, |p| p.len()) && r.iter().any(|c| c.text().is_empty()) && r.iter().any(|c| !c.text().is_empty()));
                            if cont {
                                let first = n.rows.remove(0);
                                if let Some(prev) = a.rows.last_mut() {
                                    for (pc, c) in prev.iter_mut().zip(first) {
                                        let add = c.text();
                                        if add.is_empty() { continue; }
                                        match pc.blocks.iter_mut().rev().find_map(|b| if let Block::Para(p) = b { Some(p) } else { None }) {
                                            Some(p) => { let t = format!("{} {}", p.text(), add); p.runs = vec![Run::plain(t)]; }
                                            None => pc.blocks.push(Block::Para(Para { runs: vec![Run::plain(add)], ..Default::default() })),
                                        }
                                    }
                                }
                            }
                            a.rows.extend(n.rows);
                        }
                    }
                    _ => break,
                }
            }
            out.push(Block::Table(a));
        } else { out.push(b); }
    }
    doc.blocks = out;
}

/// PDF bytes → document model. Errors when the file has no selectable text (scanned pages need OCR).
pub fn read(bytes: Vec<u8>, max_pages: usize) -> Result<Doc> {
    let pages = extract_glyphs(bytes, max_pages)?;
    let total: usize = pages.iter().map(|p| p.glyphs.len()).sum();
    if total == 0 {
        let msg = if pages.iter().any(|p| p.has_images) {
            "This PDF has no selectable text (it looks scanned). Reading it needs OCR, which needs Tesseract."
        } else { "This PDF has no text." };
        return Err(Error::new(ErrorCode::EConvertFailed, msg));
    }
    Ok(analyse(pages))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(x: f32, y: f32, size: f32, s: &str) -> Vec<G> {
        // 0.5em per character, no kerning: x1 of the last char is the word's right edge.
        s.chars().enumerate().map(|(i, c)| G { text: c.to_string(), x0: x + i as f32 * 0.5 * size, x1: x + (i as f32 + 1.0) * 0.5 * size, y, size }).collect()
    }
    fn line(x: f32, y: f32, size: f32, words: &[&str]) -> Vec<G> {
        let mut out = vec![]; let mut cx = x;
        for w in words { let g = word(cx, y, size, w); cx = g.last().unwrap().x1 + 0.3 * size; out.extend(g); }
        out
    }
    fn page(glyphs: Vec<G>) -> PageGlyphs { PageGlyphs { glyphs, rules: vec![], width: 595.0, height: 842.0, has_images: false } }
    fn texts(d: &Doc) -> Vec<String> { d.blocks.iter().filter_map(|b| if let Block::Para(p) = b { Some(p.text()) } else { None }).collect() }

    #[test] fn words_and_wrapped_lines_become_one_paragraph() {
        let mut g = line(72.0, 100.0, 11.0, &["Hello", "wor-"]);
        g.extend(line(72.0, 113.0, 11.0, &["ld", "again", "here."]));
        g.extend(line(72.0, 140.0, 11.0, &["Next", "paragraph."]));
        let d = analyse(vec![page(g)]);
        assert_eq!(texts(&d), vec!["Hello world again here.", "Next paragraph."]);
    }
    #[test] fn big_text_is_a_heading_and_bullets_are_a_list() {
        let mut g = line(72.0, 80.0, 22.0, &["Chapter", "One"]);
        for (i, t) in ["First", "Second"].iter().enumerate() { g.extend(line(72.0, 120.0 + i as f32 * 14.0, 11.0, &["•", t, "item"])); }
        g.extend(line(72.0, 180.0, 11.0, &["Plain", "closing", "sentence", "of", "body", "text", "here."]));
        let d = analyse(vec![page(g)]);
        let Block::Para(h) = &d.blocks[0] else { panic!() };
        assert_eq!((h.heading, h.text().as_str()), (Some(1), "Chapter One"));
        let Block::Para(li) = &d.blocks[1] else { panic!() };
        assert_eq!((li.list.is_some(), li.text().as_str()), (true, "First item"));
    }
    #[test] fn aligned_columns_become_a_table_but_prose_columns_do_not() {
        let mut g = vec![];
        for (i, (a, b, c)) in [("Name", "Qty", "Price"), ("Apples", "12", "1.50"), ("Pears", "7", "2.25")].iter().enumerate() {
            let y = 100.0 + i as f32 * 16.0;
            g.extend(word(72.0, y, 11.0, a)); g.extend(word(200.0, y, 11.0, b)); g.extend(word(300.0, y, 11.0, c));
        }
        let d = analyse(vec![page(g)]);
        let Block::Table(t) = &d.blocks[0] else { panic!("{:?}", d.blocks) };
        assert_eq!(t.to_text(), vec![vec!["Name", "Qty", "Price"], vec!["Apples", "12", "1.50"], vec!["Pears", "7", "2.25"]]);
    }
    #[test] fn repeated_page_numbers_and_headers_are_dropped() {
        let pages: Vec<PageGlyphs> = (0..4).map(|i| {
            let mut g = line(72.0, 40.0, 9.0, &["Company", "Report"]);
            g.extend(line(72.0, 300.0, 11.0, &["Body", "text", "of", "page", &format!("number{i}.")]));
            g.extend(word(300.0, 810.0, 9.0, &format!("{}", i + 1)));
            page(g)
        }).collect();
        let d = analyse(pages);
        let all = texts(&d).join("|");
        assert!(!all.contains("Company") && !all.contains("|1") && all.contains("number2."), "{all}");
    }
    #[test] fn list_markers() {
        assert_eq!(list_marker("• item"), Some((false, 4))); assert_eq!(list_marker("12. item"), Some((true, 4)));
        assert_eq!(list_marker("a) item"), Some((true, 3)));
        for s in ["1.5 million", "2024 was good", "Text", "- ", "A. Smith"] { assert_eq!(list_marker(s), None, "{s}"); }
    }
}
