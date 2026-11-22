//! Format-neutral content the built-in readers produce and the built-in writers consume.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub mono: bool,
    pub size_pt: Option<f32>,
    pub color: Option<[u8; 3]>,
    pub link: Option<String>,
}

impl Run {
    pub fn plain(text: impl Into<String>) -> Self { Run { text: text.into(), ..Default::default() } }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Align { #[default] Left, Center, Right, Justify }

#[derive(Debug, Clone, PartialEq)]
pub struct ListInfo { pub ordered: bool, pub level: u8 }

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Para {
    pub runs: Vec<Run>,
    pub align: Align,
    /// 1..=6 for headings.
    pub heading: Option<u8>,
    pub list: Option<ListInfo>,
}

impl Para {
    pub fn text(&self) -> String { self.runs.iter().map(|r| r.text.as_str()).collect() }
    pub fn is_blank(&self) -> bool { self.runs.iter().all(|r| r.text.trim().is_empty()) }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub data: Vec<u8>,
    /// `image/png`, `image/jpeg`, ...
    pub mime: String,
    pub width_pt: Option<f32>,
    pub height_pt: Option<f32>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cell { pub blocks: Vec<Block>, pub col_span: usize }

impl Cell {
    pub fn text(&self) -> String {
        let mut parts = vec![];
        for b in &self.blocks {
            match b {
                Block::Para(p) => parts.push(p.text()),
                Block::Table(t) => parts.push(t.rows.iter().map(|r| r.iter().map(Cell::text).collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join(" ")),
                _ => {}
            }
        }
        parts.join("\n")
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table { pub rows: Vec<Vec<Cell>> }

impl Table {
    pub fn from_text(rows: &[Vec<String>]) -> Table {
        Table { rows: rows.iter().map(|r| r.iter().map(|t| Cell {
            blocks: vec![Block::Para(Para { runs: vec![Run::plain(t.clone())], ..Default::default() })], col_span: 1,
        }).collect()).collect() }
    }
    pub fn to_text(&self) -> Vec<Vec<String>> {
        self.rows.iter().map(|r| {
            let mut out = vec![];
            for c in r {
                out.push(c.text());
                for _ in 1..c.col_span.max(1) { out.push(String::new()); }
            }
            out
        }).collect()
    }
    pub fn columns(&self) -> usize { self.rows.iter().map(|r| r.iter().map(|c| c.col_span.max(1)).sum::<usize>()).max().unwrap_or(0) }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block { Para(Para), Table(Table), Image(Image), PageBreak }

/// Page size and margins a file declares for itself (points; margins are top, right, bottom, left).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageBox { pub width_pt: f32, pub height_pt: f32, pub margins: [f32; 4] }

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Doc {
    pub blocks: Vec<Block>,
    pub page: Option<PageBox>,
    /// The document's default text size.
    pub base_size_pt: Option<f32>,
}

impl Doc {
    pub fn para(&mut self, text: impl Into<String>) { self.blocks.push(Block::Para(Para { runs: vec![Run::plain(text)], ..Default::default() })); }
    pub fn heading(&mut self, level: u8, text: impl Into<String>) {
        self.blocks.push(Block::Para(Para { runs: vec![Run::plain(text)], heading: Some(level), ..Default::default() }));
    }
    /// Every table in reading order, nested ones included.
    pub fn tables(&self) -> Vec<&Table> {
        fn walk<'a>(blocks: &'a [Block], out: &mut Vec<&'a Table>) {
            for b in blocks { if let Block::Table(t) = b {
                out.push(t);
                for r in &t.rows { for c in r { walk(&c.blocks, out); } }
            } }
        }
        let mut v = vec![]; walk(&self.blocks, &mut v); v
    }
    pub fn first_heading(&self) -> Option<String> {
        self.blocks.iter().find_map(|b| match b { Block::Para(p) if p.heading.is_some() && !p.is_blank() => Some(p.text()), _ => None })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Sheet { pub name: String, pub rows: Vec<Vec<String>> }

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Workbook { pub sheets: Vec<Sheet> }

impl Workbook {
    /// A document with one heading and one table per sheet (how a workbook is laid out for PDF/HTML).
    pub fn to_doc(&self) -> Doc {
        let mut d = Doc::default();
        for (i, s) in self.sheets.iter().enumerate() {
            if self.sheets.len() > 1 {
                if i > 0 { d.blocks.push(Block::PageBreak); }
                d.heading(2, s.name.clone());
            }
            let width = s.rows.iter().map(Vec::len).max().unwrap_or(0);
            let rows: Vec<Vec<String>> = s.rows.iter().map(|r| { let mut r = r.clone(); r.resize(width, String::new()); r }).collect();
            if !rows.is_empty() { d.blocks.push(Block::Table(Table::from_text(&rows))); }
        }
        d
    }
}


/// What a reader hands to a writer.
#[derive(Debug, Clone, PartialEq)]
pub enum Content { Doc(Doc), Book(Workbook), Deck(Deck) }

impl Content {
    pub fn into_doc(self) -> Doc { match self { Content::Doc(d) => d, Content::Book(b) => b.to_doc(), Content::Deck(k) => k.to_doc() } }
    /// Spreadsheet view: a workbook as is, a document as one sheet per table.
    pub fn into_book(self) -> Workbook {
        match self {
            Content::Book(b) => b,
            Content::Deck(k) => Content::Doc(k.to_doc()).into_book(),
            Content::Doc(d) => Workbook { sheets: d.tables().iter().enumerate()
                .map(|(i, t)| Sheet { name: format!("Table_{}", i + 1), rows: t.to_text() }).collect() },
        }
    }
}

/// A positioned object on a slide (points, origin top-left).
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Text { x: f32, y: f32, w: f32, h: f32, fill: Option<[u8; 3]>, line: Option<[u8; 3]>, paras: Vec<Para>, middle: bool },
    Picture { x: f32, y: f32, w: f32, h: f32, image: Image },
    Table { x: f32, y: f32, w: f32, table: Table },
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Slide { pub background: Option<[u8; 3]>, pub shapes: Vec<Shape> }

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Deck { pub width_pt: f32, pub height_pt: f32, pub slides: Vec<Slide> }

impl Deck {
    /// Text of every slide as a document (a heading per slide), for text-like outputs.
    pub fn to_doc(&self) -> Doc {
        let mut d = Doc::default();
        for (i, s) in self.slides.iter().enumerate() {
            d.heading(2, format!("Slide {}", i + 1));
            for sh in &s.shapes {
                match sh {
                    Shape::Text { paras, .. } => for p in paras { if !p.is_blank() { d.blocks.push(Block::Para(p.clone())); } },
                    Shape::Table { table, .. } => d.blocks.push(Block::Table(table.clone())),
                    Shape::Picture { image, .. } => d.blocks.push(Block::Image(image.clone())),
                }
            }
        }
        d
    }
}
