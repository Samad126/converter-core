//! A small page-layout engine: paragraphs wrap and paginate, tables size their columns and split across pages,
//! images scale to fit. It produces drawing commands (`Page::cmds`) that `write::pdf` turns into a PDF and that a
//! renderer could rasterise. Everything is in points with the origin at the page's top-left.
use super::fonts::FontSet;
use super::model::*;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSetup { pub width: f32, pub height: f32, /// top, right, bottom, left
    pub margin: [f32; 4] }

impl PageSetup {
    pub const A4: PageSetup = PageSetup { width: 595.28, height: 841.89, margin: [72.0, 72.0, 72.0, 72.0] };
    pub fn landscape(self) -> PageSetup { PageSetup { width: self.height, height: self.width, margin: [self.margin[3], self.margin[0], self.margin[1], self.margin[2]] } }
    pub fn inner_width(&self) -> f32 { self.width - self.margin[1] - self.margin[3] }
    pub fn top(&self) -> f32 { self.margin[0] }
    pub fn bottom(&self) -> f32 { self.height - self.margin[2] }
    pub fn left(&self) -> f32 { self.margin[3] }
}

#[derive(Debug, Clone)]
pub struct Piece { pub x: f32, pub text: String, pub face: usize, pub size: f32, pub color: [u8; 3], pub underline: bool, pub width: f32, pub link: Option<Arc<str>> }

#[derive(Debug, Clone)]
pub struct ImgItem { pub x: f32, pub w: f32, pub h: f32, pub img: usize }

#[derive(Debug, Clone)]
pub enum Item { Text(Piece), Img(ImgItem) }

#[derive(Debug, Clone, Default)]
pub struct Line { pub h: f32, pub baseline: f32, pub items: Vec<Item>, pub space_before: f32, pub space_after: f32, pub keep_next: bool }

#[derive(Debug, Clone)]
pub enum Cmd {
    Text(Piece, f32 /* baseline y */),
    Rect { x: f32, y: f32, w: f32, h: f32, fill: Option<[u8; 3]>, stroke: Option<([u8; 3], f32)> },
    Image { x: f32, y: f32, w: f32, h: f32, img: usize },
}

#[derive(Debug, Clone)]
pub struct Page { pub width: f32, pub height: f32, pub cmds: Vec<Cmd> }

pub struct Layout { pub pages: Vec<Page>, pub images: Vec<Image> }

struct Cell { x: f32, w: f32, lines: Vec<Line> }
struct Row { cells: Vec<Cell> }
pub struct TableL { rows: Vec<Row> }

pub enum Flow { Lines(Vec<Line>), Table(TableL), PageBreak }

#[derive(Clone)]
struct Style { bold: bool, italic: bool, mono: bool, size: f32, color: [u8; 3], underline: bool, link: Option<Arc<str>> }

struct Seg { face: usize, text: String, width: f32, style: usize }

enum Tok { Word(Vec<(usize, String, f32)>, f32, usize), Space(f32, usize), Break }

pub struct Engine<'a> {
    pub fonts: &'a mut FontSet,
    pub setup: PageSetup,
    pub base: f32,
    /// Stretch tables to the full text width (documents) or keep their natural width (spreadsheets).
    pub stretch_tables: bool,
    pub pages: Vec<Page>,
    pub images: Vec<Image>,
    y: f32,
    styles: Vec<Style>,
}

const PAD: f32 = 3.0;
const LINK: [u8; 3] = [0x1a, 0x55, 0xc4];
const BORDER: [u8; 3] = [0x9a, 0x9a, 0x9a];
const LIST_STEP: f32 = 18.0;

impl<'a> Engine<'a> {
    pub fn new(fonts: &'a mut FontSet, setup: PageSetup, base: f32) -> Self {
        Engine { fonts, setup, base, stretch_tables: true, pages: vec![], images: vec![], y: 0.0, styles: vec![] }
    }

    pub fn finish(self) -> Layout { Layout { pages: self.pages, images: self.images } }

    pub fn new_page(&mut self) {
        self.pages.push(Page { width: self.setup.width, height: self.setup.height, cmds: vec![] });
        self.y = self.setup.top();
    }

    /// Starts a fresh page with a different page size (used per spreadsheet).
    pub fn begin_section(&mut self, setup: PageSetup, base: f32) {
        self.setup = setup; self.base = base;
        if self.pages.last().is_some_and(|p| p.cmds.is_empty()) { self.pages.pop(); }
        self.new_page();
    }

    fn page(&mut self) -> &mut Page { if self.pages.is_empty() { self.new_page(); } self.pages.last_mut().expect("page") }

    // ---- inline layout -------------------------------------------------------------------------------------------

    fn heading_size(&self, level: u8) -> f32 {
        let s = [20.0, 16.0, 14.0, 12.5, 11.5, 11.0][(level.clamp(1, 6) - 1) as usize];
        s * self.base / 11.0
    }

    fn style_id(&mut self, s: Style) -> usize { self.styles.push(s); self.styles.len() - 1 }

    fn tokenize(&mut self, p: &Para) -> Vec<Tok> {
        let mut toks = vec![];
        let heading_size = p.heading.map(|h| self.heading_size(h));
        for r in &p.runs {
            let sid = self.style_id(Style {
                bold: r.bold || p.heading.is_some(), italic: r.italic, mono: r.mono,
                size: r.size_pt.filter(|s| *s > 0.0).or(heading_size).unwrap_or(self.base),
                color: r.color.unwrap_or(if r.link.is_some() { LINK } else { [0, 0, 0] }),
                underline: r.underline || r.link.is_some(), link: r.link.as_deref().map(Arc::from),
            });
            let st = self.styles[sid].clone();
            let mut word = String::new();
            let flush = |this: &mut Self, word: &mut String, toks: &mut Vec<Tok>| {
                if word.is_empty() { return; }
                let segs = this.fonts.segments(word, st.bold, st.italic, st.mono);
                let w = segs.iter().map(|s| s.2).sum::<f32>() * st.size;
                toks.push(Tok::Word(segs, w, sid));
                word.clear();
            };
            for c in r.text.chars() {
                match c {
                    '\n' => { flush(self, &mut word, &mut toks); toks.push(Tok::Break); }
                    c if c.is_whitespace() && c != '\u{a0}' => {
                        flush(self, &mut word, &mut toks);
                        let n = if c == '\t' { 4 } else { 1 };
                        let w = self.fonts.width(" ", st.size, st.bold, st.italic, st.mono) * n as f32;
                        toks.push(Tok::Space(w, sid));
                    }
                    c => word.push(c),
                }
            }
            flush(self, &mut word, &mut toks);
        }
        toks
    }

    fn finish_line(&mut self, cur: &mut Vec<Seg>, x0: f32, width: f32, align: Align, lines: &mut Vec<Line>) {
        // Drop trailing spaces so right/centre alignment is exact.
        while cur.last().is_some_and(|s| s.text.trim().is_empty()) { cur.pop(); }
        if let Some(l) = cur.last_mut() { let t = l.text.trim_end().len(); if t < l.text.len() { let drop = l.text.len() - t; l.width *= t as f32 / (t + drop) as f32; l.text.truncate(t); } }
        let total: f32 = cur.iter().map(|s| s.width).sum();
        let max_size = cur.iter().map(|s| self.styles[s.style].size).fold(0.0f32, f32::max);
        let max_size = if max_size == 0.0 { self.base } else { max_size };
        let h = max_size * 1.2;
        let offset = match align { Align::Center => ((width - total) / 2.0).max(0.0), Align::Right => (width - total).max(0.0), _ => 0.0 };
        let mut x = x0 + offset;
        let mut items: Vec<Item> = vec![];
        for s in cur.drain(..) {
            let st = &self.styles[s.style];
            let can_merge = matches!(items.last(), Some(Item::Text(p)) if p.face == s.face && p.size == st.size && p.color == st.color && p.underline == st.underline && p.link == st.link);
            if can_merge {
                if let Some(Item::Text(p)) = items.last_mut() { p.text.push_str(&s.text); p.width += s.width; }
            } else {
                items.push(Item::Text(Piece { x, text: s.text, face: s.face, size: st.size, color: st.color, underline: st.underline, width: s.width, link: st.link.clone() }));
            }
            x += s.width;
        }
        lines.push(Line { h, baseline: (h - max_size * 1.117) / 2.0 + max_size * 0.905, items, ..Default::default() });
    }

    /// Wraps tokens into lines. `first_x`/`rest_x` are the left offsets of the first and following lines.
    fn wrap(&mut self, toks: Vec<Tok>, width: f32, first_x: f32, rest_x: f32, align: Align) -> Vec<Line> {
        let mut lines: Vec<Line> = vec![];
        let mut cur: Vec<Seg> = vec![];
        let mut cur_w = 0.0f32;
        let avail = |lines: &Vec<Line>| (width - if lines.is_empty() { first_x } else { rest_x }).max(20.0);
        let x_of = |lines: &Vec<Line>| if lines.is_empty() { first_x } else { rest_x };
        for t in toks {
            match t {
                Tok::Break => { let (x, a) = (x_of(&lines), avail(&lines)); self.finish_line(&mut cur, x, a, align, &mut lines); cur_w = 0.0; }
                Tok::Space(w, sid) => {
                    if cur.is_empty() { continue; }
                    cur.push(Seg { face: 0, text: " ".into(), width: w, style: sid });
                    cur_w += w;
                    // A space is drawn with the primary face of its style; width is exact either way.
                    let st = self.styles[sid].clone();
                    if let Some(l) = cur.last_mut() { l.face = self.fonts.pick(' ', st.bold, st.italic, st.mono); }
                }
                Tok::Word(segs, w, sid) => {
                    let a = avail(&lines);
                    if cur_w + w > a && !cur.is_empty() {
                        let x = x_of(&lines); self.finish_line(&mut cur, x, a, align, &mut lines); cur_w = 0.0;
                    }
                    let a = avail(&lines);
                    let size = self.styles[sid].size;
                    if w <= a { for (f, t, em) in segs { cur.push(Seg { face: f, text: t, width: em * size, style: sid }); } cur_w += w; continue; }
                    // A single word wider than the line: break it by character.
                    for (f, t, _) in segs {
                        for ch in t.chars() {
                            let cw = self.fonts.faces[f].advance(ch).unwrap_or(0.5) * size;
                            let a = avail(&lines);
                            if cur_w + cw > a && !cur.is_empty() { let x = x_of(&lines); self.finish_line(&mut cur, x, a, align, &mut lines); cur_w = 0.0; }
                            match cur.last_mut() { Some(l) if l.face == f && l.style == sid => { l.text.push(ch); l.width += cw; } _ => cur.push(Seg { face: f, text: ch.to_string(), width: cw, style: sid }) }
                            cur_w += cw;
                        }
                    }
                }
            }
        }
        let (x, a) = (x_of(&lines), avail(&lines));
        if !cur.is_empty() || lines.is_empty() { self.finish_line(&mut cur, x, a, align, &mut lines); }
        lines
    }

    pub fn image_size(&self, idx: usize) -> (f32, f32) {
        let img = &self.images[idx];
        image::ImageReader::new(std::io::Cursor::new(&img.data)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok())
            .map(|(w, h)| (w as f32 * 0.75, h as f32 * 0.75)).unwrap_or((100.0, 100.0))
    }

    fn image_line(&mut self, img: &Image, width: f32, max_h: f32, x: f32) -> Line {
        self.images.push(img.clone());
        let idx = self.images.len() - 1;
        let (nw, nh) = self.image_size(idx);
        let (mut w, mut h) = match (img.width_pt, img.height_pt) { (Some(w), Some(h)) if w > 0.0 && h > 0.0 => (w, h), _ => (nw, nh) };
        let k = (width / w).min(max_h / h).min(1.0);
        if k < 1.0 { w *= k; h *= k; }
        Line { h, baseline: h, items: vec![Item::Img(ImgItem { x, w, h, img: idx })], space_after: 4.0, ..Default::default() }
    }

    /// Lines for a run of blocks inside a box of `width` (used for table cells; nested tables become text).
    fn blocks_to_lines(&mut self, blocks: &[Block], width: f32, max_h: f32) -> Vec<Line> {
        let mut out = vec![];
        let mut counters: Vec<(bool, u32)> = vec![];
        for b in blocks {
            match b {
                Block::Para(p) => { let l = self.para_lines(p, width, &mut counters); out.extend(l); }
                Block::Image(i) => out.push(self.image_line(i, width, max_h, 0.0)),
                Block::Table(t) => {
                    for row in t.to_text() {
                        let p = Para { runs: vec![Run::plain(row.join("   "))], ..Default::default() };
                        let l = self.para_lines(&p, width, &mut counters); out.extend(l);
                    }
                }
                Block::PageBreak => {}
            }
        }
        out
    }

    pub fn para_lines(&mut self, p: &Para, width: f32, counters: &mut Vec<(bool, u32)>) -> Vec<Line> {
        let toks = self.tokenize(p);
        let (mut first_x, mut rest_x, mut marker) = (0.0, 0.0, None);
        if p.heading.is_none() {
            match &p.list {
                Some(l) => {
                    let lvl = l.level as usize;
                    counters.truncate(lvl + 1);
                    counters.resize(lvl + 1, (l.ordered, 0));
                    if counters[lvl].0 != l.ordered { counters[lvl] = (l.ordered, 0); }
                    counters[lvl].1 += 1;
                    let indent = (lvl as f32 + 1.0) * LIST_STEP;
                    first_x = indent; rest_x = indent;
                    marker = Some((if l.ordered { format!("{}.", counters[lvl].1) } else { "\u{2022}".to_string() }, indent));
                }
                None => counters.clear(),
            }
        } else { counters.clear(); }
        let mut lines = self.wrap(toks, width, first_x, rest_x, p.align);
        if let Some((m, indent)) = marker {
            let (face, size) = (self.fonts.pick(m.chars().next().unwrap_or('•'), false, false, false), self.base);
            let w = self.fonts.width(&m, size, false, false, false);
            if let Some(first) = lines.first_mut() {
                first.items.insert(0, Item::Text(Piece { x: (indent - w - 4.0).max(0.0), text: m, face, size, color: [0, 0, 0], underline: false, width: w, link: None }));
            }
        }
        match p.heading {
            Some(h) => {
                let size = self.heading_size(h);
                if let Some(f) = lines.first_mut() { f.space_before = size * 0.7; }
                if let Some(l) = lines.last_mut() { l.space_after = size * 0.3; l.keep_next = true; }
                for l in lines.iter_mut().rev().skip(1).take(0) { l.keep_next = true; }
            }
            None => if let Some(l) = lines.last_mut() { l.space_after = if p.list.is_some() { 2.0 } else { 6.0 }; },
        }
        lines
    }

    // ---- tables ------------------------------------------------------------------------------------------------

    /// `(natural width, longest-word width)` per column, with padding included.
    fn column_metrics(&mut self, t: &Table) -> (Vec<f32>, Vec<f32>) {
        let n = t.columns();
        let (mut nat, mut min) = (vec![PAD * 2.0; n], vec![PAD * 2.0; n]);
        let mut spans: Vec<(usize, usize, f32, f32)> = vec![];
        for row in &t.rows {
            let mut c = 0;
            for cell in row {
                let span = cell.col_span.max(1);
                let (mut nw, mut mw) = (0.0f32, 0.0f32);
                for b in &cell.blocks {
                    let paras: Vec<Para> = match b {
                        Block::Para(p) => vec![p.clone()],
                        Block::Table(t) => t.to_text().into_iter().map(|r| Para { runs: vec![Run::plain(r.join("   "))], ..Default::default() }).collect(),
                        _ => vec![],
                    };
                    for p in paras {
                        let mut line = 0.0f32;
                        for tok in self.tokenize(&p) {
                            match tok { Tok::Word(_, w, _) => { line += w; mw = mw.max(w); } Tok::Space(w, _) => line += w, Tok::Break => { nw = nw.max(line); line = 0.0; } }
                        }
                        nw = nw.max(line);
                    }
                    if let Block::Image(i) = b { nw = nw.max(i.width_pt.unwrap_or(120.0)); mw = mw.max(i.width_pt.unwrap_or(120.0).min(120.0)); }
                }
                if span == 1 { if c < n { nat[c] = nat[c].max(nw + PAD * 2.0); min[c] = min[c].max(mw + PAD * 2.0); } }
                else { spans.push((c, span, nw + PAD * 2.0, mw + PAD * 2.0)); }
                c += span;
            }
        }
        for (c, span, nw, mw) in spans {
            let end = (c + span).min(n);
            if end <= c { continue; }
            let k = (end - c) as f32;
            if nat[c..end].iter().sum::<f32>() < nw { for x in &mut nat[c..end] { *x = nw / k; } }
            if min[c..end].iter().sum::<f32>() < mw { for x in &mut min[c..end] { *x = mw / k; } }
        }
        (nat, min)
    }

    pub fn natural_table_width(&mut self, t: &Table) -> f32 { self.column_metrics(t).0.iter().sum() }

    fn layout_table(&mut self, t: &Table, avail: f32) -> TableL {
        let n = t.columns();
        let (nat, min) = self.column_metrics(t);
        let (sn, sm): (f32, f32) = (nat.iter().sum(), min.iter().sum());
        let widths: Vec<f32> = if sn <= avail {
            if self.stretch_tables && sn > 0.0 { nat.iter().map(|w| w * avail / sn).collect() } else { nat.clone() }
        } else if sm >= avail {
            min.iter().map(|w| w * avail / sm).collect()
        } else {
            let extra = avail - sm;
            let room: f32 = nat.iter().zip(&min).map(|(a, b)| a - b).sum::<f32>().max(0.001);
            min.iter().zip(&nat).map(|(m, a)| m + (a - m) * extra / room).collect()
        };
        let max_h = (self.setup.bottom() - self.setup.top()) * 0.8;
        let mut rows = vec![];
        for row in &t.rows {
            let (mut c, mut x) = (0usize, 0.0f32);
            let mut cells = vec![];
            for cell in row {
                let span = cell.col_span.max(1);
                let end = (c + span).min(n);
                let w: f32 = widths[c.min(n)..end].iter().sum();
                let lines = self.blocks_to_lines(&cell.blocks, (w - PAD * 2.0).max(10.0), max_h);
                cells.push(Cell { x, w, lines });
                x += w; c += span;
            }
            rows.push(Row { cells });
        }
        TableL { rows }
    }

    // ---- absolutely positioned content (slides) --------------------------------------------------------------

    /// Starts a page of exactly `w` x `h` points with an optional solid background.
    pub fn slide_page(&mut self, w: f32, h: f32, background: Option<[u8; 3]>) {
        self.setup = PageSetup { width: w, height: h, margin: [0.0; 4] };
        self.pages.push(Page { width: w, height: h, cmds: vec![] });
        if let Some(bg) = background { self.put_rect(0.0, 0.0, w, h, Some(bg), None); }
    }

    pub fn put_rect(&mut self, x: f32, y: f32, w: f32, h: f32, fill: Option<[u8; 3]>, stroke: Option<([u8; 3], f32)>) {
        self.page().cmds.push(Cmd::Rect { x, y, w, h, fill, stroke });
    }

    pub fn put_picture(&mut self, img: &Image, x: f32, y: f32, w: f32, h: f32) {
        self.images.push(img.clone());
        let idx = self.images.len() - 1;
        self.page().cmds.push(Cmd::Image { x, y, w, h, img: idx });
    }

    /// Text inside a box; overflow is allowed (it is drawn past the box rather than dropped).
    pub fn put_text_box(&mut self, paras: &[Para], x: f32, y: f32, w: f32, h: f32, middle: bool) {
        const INSET_X: f32 = 7.2;
        const INSET_Y: f32 = 3.6;
        let mut counters = vec![];
        let mut lines: Vec<Line> = vec![];
        for p in paras { let l = self.para_lines(p, (w - 2.0 * INSET_X).max(10.0), &mut counters); lines.extend(l); }
        let total: f32 = lines.iter().map(|l| l.h + l.space_after).sum();
        let top = if middle { y + ((h - total) / 2.0).max(INSET_Y) } else { y + INSET_Y };
        self.y = top;
        for l in &lines { self.put_line(l, x + INSET_X); self.y += l.space_after; }
    }

    pub fn put_table(&mut self, t: &Table, x: f32, y: f32, w: f32) {
        if t.rows.is_empty() || w <= 0.0 { return; }
        let saved = (self.setup, self.base);
        let (pw, ph) = (self.setup.width, self.setup.height);
        self.setup = PageSetup { width: pw, height: ph, margin: [y, (pw - x - w).max(0.0), -1.0e6, x] };
        self.base = 12.0;
        self.y = y;
        let tl = self.layout_table(t, w);
        self.place_table(&tl);
        (self.setup, self.base) = saved;
    }

    // ---- flow ------------------------------------------------------------------------------------------------

    pub fn flow_doc(&mut self, doc: &Doc) -> Vec<Flow> {
        let mut flows = vec![];
        let avail = self.setup.inner_width();
        let max_h = (self.setup.bottom() - self.setup.top()) * 0.9;
        let mut counters: Vec<(bool, u32)> = vec![];
        for b in &doc.blocks {
            match b {
                Block::Para(p) => { let l = self.para_lines(p, avail, &mut counters); flows.push(Flow::Lines(l)); }
                Block::Image(i) => { counters.clear(); flows.push(Flow::Lines(vec![self.image_line(i, avail, max_h, 0.0)])); }
                Block::Table(t) => { counters.clear(); if !t.rows.is_empty() { let tl = self.layout_table(t, avail); flows.push(Flow::Table(tl)); } }
                Block::PageBreak => { counters.clear(); flows.push(Flow::PageBreak); }
            }
        }
        flows
    }

    fn put_line(&mut self, l: &Line, x0: f32) {
        let y = self.y;
        let page = self.page();
        for it in &l.items {
            match it {
                Item::Text(p) => { let mut p = p.clone(); p.x += x0; page.cmds.push(Cmd::Text(p, y + l.baseline)); }
                Item::Img(i) => page.cmds.push(Cmd::Image { x: x0 + i.x, y, w: i.w, h: i.h, img: i.img }),
            }
        }
        self.y += l.h;
    }

    pub fn place(&mut self, flows: &[Flow]) {
        if self.pages.is_empty() { self.new_page(); }
        let (left, bottom, top) = (self.setup.left(), self.setup.bottom(), self.setup.top());
        for f in flows {
            match f {
                Flow::PageBreak => if self.y > top + 0.1 { self.new_page(); },
                Flow::Lines(lines) => {
                    for (i, l) in lines.iter().enumerate() {
                        let before = if self.y <= top + 0.1 { 0.0 } else { l.space_before };
                        let need = before + l.h + if l.keep_next { lines.get(i + 1).map_or(self.base * 2.4, |n| n.h) } else { 0.0 };
                        if self.y + need > bottom && self.y > top + 0.1 { self.new_page(); } else { self.y += before; }
                        self.put_line(l, left);
                        self.y += l.space_after;
                    }
                }
                Flow::Table(t) => self.place_table(t),
            }
        }
    }

    fn place_table(&mut self, t: &TableL) {
        let (left, bottom, top) = (self.setup.left(), self.setup.bottom(), self.setup.top());
        let min_row = self.base * 1.2 + PAD * 2.0;
        for row in &t.rows {
            let mut pos = vec![0usize; row.cells.len()];
            loop {
                let avail = bottom - self.y - PAD * 2.0;
                let at_top = self.y <= top + 0.1;
                let mut take = vec![0usize; row.cells.len()];
                let mut slice_h = 0.0f32;
                for (ci, cell) in row.cells.iter().enumerate() {
                    let mut cum = 0.0;
                    let mut k = 0;
                    for l in &cell.lines[pos[ci]..] {
                        let h = l.h + l.space_after.min(2.0);
                        if cum + h > avail && !(at_top && k == 0) { break; }
                        cum += h; k += 1;
                    }
                    take[ci] = k;
                    slice_h = slice_h.max(cum);
                }
                let remaining = row.cells.iter().enumerate().any(|(ci, c)| pos[ci] < c.lines.len());
                if remaining && take.iter().all(|k| *k == 0) { self.new_page(); continue; }
                // An entirely empty row keeps a minimum height; it only needs to fit.
                let h = (slice_h + PAD * 2.0).max(min_row);
                if !at_top && self.y + h > bottom && pos.iter().all(|p| *p == 0) { self.new_page(); continue; }
                let y0 = self.y;
                for (ci, cell) in row.cells.iter().enumerate() {
                    let x = left + cell.x;
                    self.page().cmds.push(Cmd::Rect { x, y: y0, w: cell.w, h, fill: None, stroke: Some((BORDER, 0.5)) });
                    self.y = y0 + PAD;
                    for l in &cell.lines[pos[ci]..pos[ci] + take[ci]] {
                        self.put_line(l, x + PAD);
                        self.y += l.space_after.min(2.0);
                    }
                    pos[ci] += take[ci];
                }
                self.y = y0 + h;
                if row.cells.iter().enumerate().all(|(ci, c)| pos[ci] >= c.lines.len()) { break; }
                self.new_page();
            }
        }
        self.y += 8.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(text: &str) -> Block { Block::Para(Para { runs: vec![Run::plain(text)], ..Default::default() }) }

    fn layout(doc: &Doc) -> Layout {
        let mut fonts = FontSet::new();
        let mut e = Engine::new(&mut fonts, PageSetup::A4, 11.0);
        let flows = e.flow_doc(doc);
        e.place(&flows);
        e.finish()
    }

    #[test] fn long_text_wraps_inside_the_margins_and_paginates() {
        let doc = Doc { blocks: (0..80).map(|i| para(&format!("Paragraph {i}: the quick brown fox jumps over the lazy dog, again and again, until the line has to wrap around."))).collect(), ..Default::default() };
        let l = layout(&doc);
        assert!(l.pages.len() >= 2, "{} pages", l.pages.len());
        let s = PageSetup::A4;
        for p in &l.pages { for c in &p.cmds { if let Cmd::Text(piece, y) = c {
            assert!(piece.x >= s.left() - 0.01 && piece.x + piece.width <= s.width - s.margin[1] + 0.5, "x {} w {}", piece.x, piece.width);
            assert!(*y <= s.bottom() + 1.0, "baseline {y} past the bottom");
        } } }
    }

    #[test] fn an_overlong_word_is_broken_not_clipped() {
        let doc = Doc { blocks: vec![para(&"W".repeat(200))], ..Default::default() };
        let l = layout(&doc);
        let lines = l.pages[0].cmds.iter().filter(|c| matches!(c, Cmd::Text(..))).count();
        assert!(lines >= 3, "{lines}");
        let s = PageSetup::A4;
        for c in &l.pages[0].cmds { if let Cmd::Text(p, _) = c { assert!(p.x + p.width <= s.width - s.margin[1] + 0.5); } }
    }

    #[test] fn a_tall_table_row_splits_across_pages_without_losing_text() {
        let words = (0..1500).map(|i| format!("w{i}")).collect::<Vec<_>>().join(" ");
        let t = Table::from_text(&[vec![words.clone(), "side".into()]]);
        let l = layout(&Doc { blocks: vec![Block::Table(t)], ..Default::default() });
        assert!(l.pages.len() >= 2);
        let all: String = l.pages.iter().flat_map(|p| p.cmds.iter()).filter_map(|c| if let Cmd::Text(p, _) = c { Some(p.text.clone()) } else { None }).collect::<Vec<_>>().join(" ");
        assert!(all.contains("w0") && all.contains("w1499") && all.contains("side"));
    }

    #[test] fn headings_do_not_sit_alone_at_the_bottom() {
        let mut blocks: Vec<Block> = (0..46).map(|i| para(&format!("line {i}"))).collect();
        blocks.push(Block::Para(Para { runs: vec![Run::plain("Heading")], heading: Some(1), ..Default::default() }));
        blocks.push(para("after the heading"));
        let l = layout(&Doc { blocks, ..Default::default() });
        let page_of = |needle: &str| l.pages.iter().position(|p| p.cmds.iter().any(|c| matches!(c, Cmd::Text(pc, _) if pc.text.contains(needle)))).unwrap();
        assert_eq!(page_of("Heading"), page_of("after the heading"));
    }
}
