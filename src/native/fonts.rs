//! Fonts for the built-in PDF writer: a small bundled set (so output looks the same on every machine) plus a lazy
//! fall-back to whatever the system has for characters the bundled fonts lack (CJK, symbols, ...).
use std::collections::HashMap;
use std::sync::Arc;

static SANS: [&[u8]; 4] = [
    include_bytes!("../../assets/fonts/LiberationSans-Regular.ttf"),
    include_bytes!("../../assets/fonts/LiberationSans-Bold.ttf"),
    include_bytes!("../../assets/fonts/LiberationSans-Italic.ttf"),
    include_bytes!("../../assets/fonts/LiberationSans-BoldItalic.ttf"),
];
static MONO: [&[u8]; 2] = [
    include_bytes!("../../assets/fonts/LiberationMono-Regular.ttf"),
    include_bytes!("../../assets/fonts/LiberationMono-Bold.ttf"),
];
static SYMBOLS: [&[u8]; 2] = [
    include_bytes!("../../assets/fonts/DejaVuSans.ttf"),
    include_bytes!("../../assets/fonts/DejaVuSans-Bold.ttf"),
];

pub struct Face {
    pub data: Arc<Vec<u8>>,
    pub index: u32,
    pub font: krilla::text::Font,
    upem: f32,
    /// em-fraction advance per char; `None` = the font has no glyph for it.
    adv: HashMap<char, Option<f32>>,
}

impl Face {
    fn load(data: Arc<Vec<u8>>, index: u32) -> Option<Face> {
        let upem = ttf_parser::Face::parse(&data, index).ok()?.units_per_em() as f32;
        let font = krilla::text::Font::new(data.clone().into(), index)?;
        Some(Face { data, index, font, upem, adv: HashMap::new() })
    }

    /// Advance in em, or `None` when the font cannot draw `c`.
    pub fn advance(&mut self, c: char) -> Option<f32> {
        if let Some(v) = self.adv.get(&c) { return *v; }
        let v = ttf_parser::Face::parse(&self.data, self.index).ok().and_then(|f| {
            let g = f.glyph_index(c)?;
            Some(f.glyph_hor_advance(g).unwrap_or(0) as f32 / self.upem)
        });
        self.adv.insert(c, v);
        v
    }
}

pub struct FontSet {
    pub faces: Vec<Face>,
    system: Option<fontdb::Database>,
    system_pick: HashMap<char, Option<usize>>,
}

const SANS_BASE: usize = 0;
const MONO_BASE: usize = 4;
const SYMBOL_BASE: usize = 6;

impl FontSet {
    pub fn new() -> FontSet {
        let faces = SANS.iter().chain(MONO.iter()).chain(SYMBOLS.iter())
            .map(|d| Face::load(Arc::new(d.to_vec()), 0).expect("bundled font parses")).collect();
        FontSet { faces, system: None, system_pick: HashMap::new() }
    }

    fn styled(bold: bool, italic: bool, mono: bool) -> usize {
        if mono { MONO_BASE + bold as usize } else { SANS_BASE + bold as usize + 2 * italic as usize }
    }

    /// Face index able to draw `c` in the requested style (the style's own face first).
    pub fn pick(&mut self, c: char, bold: bool, italic: bool, mono: bool) -> usize {
        let primary = Self::styled(bold, italic, mono);
        if self.faces[primary].advance(c).is_some() { return primary; }
        let sym = SYMBOL_BASE + bold as usize;
        if self.faces[sym].advance(c).is_some() { return sym; }
        if let Some(i) = self.system_face(c) { return i; }
        primary
    }

    fn system_face(&mut self, c: char) -> Option<usize> {
        if let Some(v) = self.system_pick.get(&c) { return *v; }
        let db = self.system.get_or_insert_with(|| { let mut d = fontdb::Database::new(); d.load_system_fonts(); d });
        // Faces that cover a lot of scripts first, so one font tends to serve a whole run of CJK text.
        let score = |name: &str| {
            let n = name.to_lowercase();
            if n.contains("cjk") || n.contains("malgun") || n.contains("gothic") || n.contains("yahei") || n.contains("meiryo") || n.contains("apple sd") { 0 }
            else if n.contains("noto sans") || n.contains("unifont") || n.contains("arial unicode") { 1 } else { 2 }
        };
        let mut best: Option<(u8, fontdb::ID, u32)> = None;
        for info in db.faces() {
            if info.monospaced || info.style != fontdb::Style::Normal { continue; }
            let s = info.families.first().map(|f| score(&f.0)).unwrap_or(2);
            if best.as_ref().is_some_and(|b| b.0 <= s) { continue; }
            let has = db.with_face_data(info.id, |data, idx| ttf_parser::Face::parse(data, idx).ok().is_some_and(|f| f.glyph_index(c).is_some())).unwrap_or(false);
            if has { best = Some((s, info.id, info.index)); if s == 0 { break; } }
        }
        let picked = best.and_then(|(_, id, index)| {
            // Re-use a face we already loaded from the same file.
            let data = db.with_face_data(id, |d, _| (d.len() < 80 * 1024 * 1024).then(|| Arc::new(d.to_vec())))??;
            if let Some(i) = self.faces.iter().position(|f| f.index == index && f.data.len() == data.len() && f.data[..64.min(data.len())] == data[..64.min(data.len())]) { return Some(i); }
            let face = Face::load(data, index)?;
            self.faces.push(face);
            Some(self.faces.len() - 1)
        });
        self.system_pick.insert(c, picked);
        picked
    }

    /// Width of `text` in em, split into `(face, substring)` runs by which face draws each character.
    pub fn segments(&mut self, text: &str, bold: bool, italic: bool, mono: bool) -> Vec<(usize, String, f32)> {
        let mut out: Vec<(usize, String, f32)> = vec![];
        for c in text.chars() {
            let c = match c { '\u{a0}' | '\u{2007}' | '\u{202f}' => ' ', '\u{200b}' | '\u{feff}' | '\u{ad}' => continue, c if c.is_control() && c != '\t' => continue, c => c };
            let c = if c == '\t' { ' ' } else { c };
            let fi = self.pick(c, bold, italic, mono);
            let w = self.faces[fi].advance(c).unwrap_or(0.5);
            match out.last_mut() {
                Some(last) if last.0 == fi => { last.1.push(c); last.2 += w; }
                _ => out.push((fi, c.to_string(), w)),
            }
        }
        out
    }

    pub fn width(&mut self, text: &str, size: f32, bold: bool, italic: bool, mono: bool) -> f32 {
        self.segments(text, bold, italic, mono).iter().map(|s| s.2).sum::<f32>() * size
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn latin_uses_the_bundled_sans_and_widths_scale() {
        let mut f = FontSet::new();
        let w10 = f.width("Hello", 10.0, false, false, false);
        let w20 = f.width("Hello", 20.0, false, false, false);
        assert!((w20 - 2.0 * w10).abs() < 0.001 && w10 > 15.0 && w10 < 35.0, "{w10}");
        assert_eq!(f.pick('a', false, false, false), 0);
        assert_eq!(f.pick('a', true, true, false), 3);
        assert_eq!(f.pick('a', false, false, true), 4);
    }
    #[test] fn symbols_fall_back_to_dejavu_before_the_system() {
        let mut f = FontSet::new();
        assert_eq!(f.pick('\u{2713}', false, false, false), SYMBOL_BASE);
    }
}
