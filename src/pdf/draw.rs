//! Drawing onto existing pages: appends content streams (original wrapped in q/Q so a sloppy
//! page cannot skew what we draw), registers fonts/images/opacity in page resources.
use super::pages::{media_box, page_ids};
use crate::{Error, ErrorCode, Result};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

pub fn invalid(msg: impl Into<String>) -> Error { Error::new(ErrorCode::EInvalidField, msg) }
fn internal(e: impl std::fmt::Display) -> Error { Error::new(ErrorCode::EInternal, e.to_string()) }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Std { Helvetica, HelveticaBold, HelveticaBoldOblique }

impl Std {
    fn base_font(self) -> &'static str {
        match self { Self::Helvetica => "Helvetica", Self::HelveticaBold => "Helvetica-Bold", Self::HelveticaBoldOblique => "Helvetica-BoldOblique" }
    }
    fn table(self) -> &'static [u16; 256] {
        // Bold and BoldOblique share advances.
        match self { Self::Helvetica => &super::widths::HELVETICA, _ => &super::widths::HELVETICA_BOLD }
    }
}

/// Unicode → WinAnsi byte, for the characters the standard fonts can draw.
pub fn win_ansi(c: char) -> Option<u8> {
    let cp = c as u32;
    if cp < 0x80 || (0xA0..=0xFF).contains(&cp) { return Some(cp as u8); }
    Some(match c {
        '€' => 0x80, '‚' => 0x82, 'ƒ' => 0x83, '„' => 0x84, '…' => 0x85, '†' => 0x86, '‡' => 0x87, 'ˆ' => 0x88, '‰' => 0x89,
        'Š' => 0x8A, '‹' => 0x8B, 'Œ' => 0x8C, 'Ž' => 0x8E, '‘' => 0x91, '’' => 0x92, '“' => 0x93, '”' => 0x94, '•' => 0x95,
        '–' => 0x96, '—' => 0x97, '˜' => 0x98, '™' => 0x99, 'š' => 0x9A, '›' => 0x9B, 'œ' => 0x9C, 'ž' => 0x9E, 'Ÿ' => 0x9F,
        _ => return None,
    })
}

pub fn std_encode(font: Std, text: &str) -> Result<Vec<u8>> {
    let _ = font;
    text.chars().map(|c| win_ansi(c).ok_or_else(|| invalid(format!("The text contains \"{c}\", which this font cannot draw.")))).collect()
}

pub fn std_width(font: Std, text: &str, size: f64) -> Result<f64> {
    let t = font.table();
    Ok(std_encode(font, text)?.iter().map(|b| t[*b as usize] as f64).sum::<f64>() * size / 1000.0)
}

/// An embedded TrueType font (Type0 / Identity-H), kept so we can measure text.
pub struct TrueType { data: Vec<u8>, units: f64 }

impl TrueType {
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        let f = ttf_parser::Face::parse(&data, 0).map_err(|e| internal(format!("bad font: {e}")))?;
        let units = f.units_per_em() as f64;
        Ok(Self { data, units })
    }
    fn face(&self) -> ttf_parser::Face<'_> { ttf_parser::Face::parse(&self.data, 0).expect("validated in parse") }
    fn glyphs(&self, text: &str) -> Vec<ttf_parser::GlyphId> {
        let f = self.face();
        text.chars().map(|c| f.glyph_index(c).unwrap_or(ttf_parser::GlyphId(0))).collect()
    }
    pub fn width(&self, text: &str, size: f64) -> f64 {
        let f = self.face();
        self.glyphs(text).iter().map(|g| f.glyph_hor_advance(*g).unwrap_or(0) as f64).sum::<f64>() * size / self.units
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontRef { Std(Std), Embedded(usize) }

pub struct Pdf {
    pub doc: Document,
    pages: Vec<ObjectId>,
    wrapped: HashSet<ObjectId>,
    new_content: HashMap<ObjectId, String>,
    registered: HashSet<(ObjectId, String)>,
    std_fonts: HashMap<Std, (String, ObjectId)>,
    tt_fonts: Vec<(TrueType, String, Option<ObjectId>, std::collections::BTreeMap<u16, char>)>,
    gs: HashMap<u32, (String, ObjectId)>,
    counter: usize,
}

pub struct Image { pub id: ObjectId, pub width: u32, pub height: u32 }

impl Pdf {
    pub fn load(bytes: &[u8]) -> Result<Self> {
        let mut doc = super::pages::load(bytes)?;
        super::pages::materialize_inherited(&mut doc);
        let pages = page_ids(&doc);
        Ok(Self { doc, pages, wrapped: HashSet::new(), new_content: HashMap::new(), registered: HashSet::new(),
                  std_fonts: HashMap::new(), tt_fonts: vec![], gs: HashMap::new(), counter: 0 })
    }
    pub fn from_document(doc: Document) -> Self {
        let pages = page_ids(&doc);
        Self { doc, pages, wrapped: HashSet::new(), new_content: HashMap::new(), registered: HashSet::new(),
               std_fonts: HashMap::new(), tt_fonts: vec![], gs: HashMap::new(), counter: 0 }
    }
    pub fn page_count(&self) -> usize { self.pages.len() }

    /// 1-based page → id.
    pub fn page(&self, n: usize) -> Result<ObjectId> {
        n.checked_sub(1).and_then(|i| self.pages.get(i)).copied()
            .ok_or_else(|| invalid(format!("Page {n} does not exist in this {}-page document.", self.pages.len())))
    }
    /// (width, height) of the page's media box, in points.
    pub fn page_size(&self, id: ObjectId) -> (f64, f64) {
        let [x0, y0, x1, y1] = media_box(&self.doc, id).unwrap_or([0.0, 0.0, 612.0, 792.0]);
        (x1 - x0, y1 - y0)
    }

    fn next_name(&mut self, p: &str) -> String { self.counter += 1; format!("{p}c{}", self.counter) }

    pub fn std_font(&mut self, f: Std) -> String {
        if let Some((n, _)) = self.std_fonts.get(&f) { return n.clone(); }
        let id = self.doc.add_object(Dictionary::from_iter([("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"Type1".to_vec())), ("BaseFont", Object::Name(f.base_font().as_bytes().to_vec())),
            ("Encoding", Object::Name(b"WinAnsiEncoding".to_vec()))]));
        let name = self.next_name("F");
        self.std_fonts.insert(f, (name.clone(), id));
        name
    }

    pub fn add_truetype(&mut self, tt: TrueType) -> FontRef {
        let name = self.next_name("T");
        self.tt_fonts.push((tt, name, None, Default::default()));
        FontRef::Embedded(self.tt_fonts.len() - 1)
    }

    pub fn text_width(&self, f: FontRef, text: &str, size: f64) -> Result<f64> {
        match f { FontRef::Std(s) => std_width(s, text, size), FontRef::Embedded(i) => Ok(self.tt_fonts[i].0.width(text, size)) }
    }

    /// Operators for one line of text in `font` at (x, y) baseline.
    pub fn text_ops(&mut self, page: ObjectId, f: FontRef, text: &str, size: f64, x: f64, y: f64, rgb: [f64; 3]) -> Result<String> {
        let (name, shown) = match f {
            FontRef::Std(s) => {
                let enc = std_encode(s, text)?;
                (self.std_font(s), enc.iter().fold(String::new(), |mut a, b| { let _ = write!(a, "{b:02X}"); a }))
            }
            FontRef::Embedded(i) => {
                let glyphs = self.tt_fonts[i].0.glyphs(text);
                for (g, c) in glyphs.iter().zip(text.chars()) { self.tt_fonts[i].3.insert(g.0, c); }
                (self.tt_fonts[i].1.clone(), glyphs.iter().fold(String::new(), |mut a, g| { let _ = write!(a, "{:04X}", g.0); a }))
            }
        };
        self.use_font(page, f, &name)?;
        Ok(format!("BT /{name} {} Tf {} {} {} rg {} {} Td <{shown}> Tj ET\n", n(size), n(rgb[0]), n(rgb[1]), n(rgb[2]), n(x), n(y)))
    }

    fn use_font(&mut self, page: ObjectId, f: FontRef, name: &str) -> Result<()> {
        if !self.registered.insert((page, name.to_string())) { return Ok(()); }
        let id = match f {
            FontRef::Std(s) => { self.std_font(s); self.std_fonts[&s].1 }
            FontRef::Embedded(i) => self.embedded_object(i)?,
        };
        self.add_resource(page, "Font", name, id)
    }

    /// Builds (once) the Type0 font objects for an embedded TrueType.
    fn embedded_object(&mut self, i: usize) -> Result<ObjectId> {
        if let Some(id) = self.tt_fonts[i].2 { return Ok(id); }
        let (data, units, used) = { let t = &self.tt_fonts[i]; (t.0.data.clone(), t.0.units, ()) };
        let _ = used;
        let face = ttf_parser::Face::parse(&data, 0).map_err(internal)?;
        let scale = |v: f64| (v * 1000.0 / units).round() as i64;
        let bbox = face.global_bounding_box();
        let ps_name = "EmbeddedFont";
        let file_stream = compressed(&data, Some(("Length1", data.len() as i64)))?;
        let file = self.doc.add_object(file_stream);
        let desc = self.doc.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"FontDescriptor".to_vec())), ("FontName", Object::Name(ps_name.into())),
            ("Flags", Object::Integer(4)),
            ("FontBBox", Object::Array(vec![scale(bbox.x_min as f64).into(), scale(bbox.y_min as f64).into(), scale(bbox.x_max as f64).into(), scale(bbox.y_max as f64).into()])),
            ("ItalicAngle", Object::Integer(0)), ("Ascent", scale(face.ascender() as f64).into()), ("Descent", scale(face.descender() as f64).into()),
            ("CapHeight", scale(face.capital_height().unwrap_or(face.ascender()) as f64).into()), ("StemV", Object::Integer(80)),
            ("FontFile2", Object::Reference(file))]));
        // Widths for every glyph (small fonts; keeps the W array simple and complete).
        let mut w: Vec<Object> = vec![];
        for g in 0..face.number_of_glyphs() {
            w.push(Object::Integer(scale(face.glyph_hor_advance(ttf_parser::GlyphId(g)).unwrap_or(0) as f64)));
        }
        let cid = self.doc.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Font".to_vec())), ("Subtype", Object::Name(b"CIDFontType2".to_vec())),
            ("BaseFont", Object::Name(ps_name.into())),
            ("CIDSystemInfo", Dictionary::from_iter([("Registry", Object::string_literal("Adobe")), ("Ordering", Object::string_literal("Identity")), ("Supplement", Object::Integer(0))]).into()),
            ("FontDescriptor", Object::Reference(desc)), ("CIDToGIDMap", Object::Name(b"Identity".to_vec())),
            ("W", Object::Array(vec![Object::Integer(0), Object::Array(w)]))]));
        let type0 = self.doc.add_object(Dictionary::from_iter([
            ("Type", Object::Name(b"Font".to_vec())), ("Subtype", Object::Name(b"Type0".to_vec())),
            ("BaseFont", Object::Name(ps_name.into())), ("Encoding", Object::Name(b"Identity-H".to_vec())),
            ("DescendantFonts", Object::Array(vec![Object::Reference(cid)]))]));
        self.tt_fonts[i].2 = Some(type0);
        Ok(type0)
    }

    /// Adds `ToUnicode` maps for embedded fonts so the drawn text stays searchable/copyable. Call once at the end.
    fn finish_fonts(&mut self) -> Result<()> {
        for i in 0..self.tt_fonts.len() {
            let Some(id) = self.tt_fonts[i].2 else { continue };
            let mut cmap = String::from("/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /Adobe-Identity-UCS def /CMapType 2 def\n1 begincodespacerange <0000> <FFFF> endcodespacerange\n");
            let entries: Vec<_> = self.tt_fonts[i].3.iter().collect();
            for chunk in entries.chunks(100) {
                let _ = writeln!(cmap, "{} beginbfchar", chunk.len());
                for (g, c) in chunk {
                    let mut buf = [0u16; 2];
                    let hex = c.encode_utf16(&mut buf).iter().fold(String::new(), |mut a, u| { let _ = write!(a, "{u:04X}"); a });
                    let _ = writeln!(cmap, "<{g:04X}> <{hex}>");
                }
                cmap.push_str("endbfchar\n");
            }
            cmap.push_str("endcmap CMapName currentdict /CMap defineresource pop end end");
            let tu = self.doc.add_object(Stream::new(Dictionary::new(), cmap.into_bytes()));
            self.doc.get_object_mut(id).and_then(Object::as_dict_mut).map_err(internal)?.set("ToUnicode", Object::Reference(tu));
        }
        Ok(())
    }

    /// Resource name for an ExtGState with fill/stroke opacity `alpha`.
    pub fn opacity(&mut self, page: ObjectId, alpha: f64) -> Result<String> {
        let key = (alpha * 1000.0).round() as u32;
        let (name, id) = match self.gs.get(&key) { Some(v) => v.clone(), None => {
            let id = self.doc.add_object(Dictionary::from_iter([("Type", Object::Name(b"ExtGState".to_vec())), ("ca", Object::Real(alpha as f32)), ("CA", Object::Real(alpha as f32))]));
            let name = self.next_name("G");
            self.gs.insert(key, (name.clone(), id));
            (name, id)
        } };
        if self.registered.insert((page, name.clone())) { self.add_resource(page, "ExtGState", &name, id)?; }
        Ok(name)
    }

    pub fn embed_image(&mut self, data: &[u8], format: ImageFormat) -> Result<Image> {
        use image::{GenericImageView, ImageDecoder};
        let bad = |e: image::ImageError| Error::new(ErrorCode::EConvertFailed, format!("Could not read the image: {e}"));
        match format {
            ImageFormat::Jpg => {
                let dec = image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(data)).map_err(bad)?;
                let (w, h) = dec.dimensions();
                let (cs, extra) = match dec.color_type().channel_count() {
                    1 => ("DeviceGray", None), 3 => ("DeviceRGB", None),
                    4 => ("DeviceCMYK", Some(Object::Array([1, 0, 1, 0, 1, 0, 1, 0].map(Object::Integer).to_vec()))),
                    c => return Err(Error::new(ErrorCode::EConvertFailed, format!("Unsupported JPEG with {c} channels."))),
                };
                let mut d = Dictionary::from_iter([("Type", Object::Name(b"XObject".to_vec())), ("Subtype", Object::Name(b"Image".to_vec())),
                    ("Width", Object::Integer(w as i64)), ("Height", Object::Integer(h as i64)), ("ColorSpace", Object::Name(cs.into())),
                    ("BitsPerComponent", Object::Integer(8)), ("Filter", Object::Name(b"DCTDecode".to_vec()))]);
                if let Some(e) = extra { d.set("Decode", e); }
                let id = self.doc.add_object(Stream::new(d, data.to_vec()).with_compression(false));
                Ok(Image { id, width: w, height: h })
            }
            ImageFormat::Png => {
                let img = image::load_from_memory_with_format(data, image::ImageFormat::Png).map_err(bad)?;
                let (w, h) = img.dimensions();
                let rgba = img.to_rgba8();
                let (mut rgb, mut alpha, mut has_alpha) = (Vec::with_capacity((w * h * 3) as usize), Vec::with_capacity((w * h) as usize), false);
                for p in rgba.pixels() { rgb.extend_from_slice(&p.0[..3]); alpha.push(p.0[3]); has_alpha |= p.0[3] != 255; }
                let mut d = Dictionary::from_iter([("Type", Object::Name(b"XObject".to_vec())), ("Subtype", Object::Name(b"Image".to_vec())),
                    ("Width", Object::Integer(w as i64)), ("Height", Object::Integer(h as i64)), ("ColorSpace", Object::Name(b"DeviceRGB".to_vec())),
                    ("BitsPerComponent", Object::Integer(8))]);
                if has_alpha {
                    let mut m = compressed(&alpha, None)?;
                    for (k, v) in [("Type", Object::Name(b"XObject".to_vec())), ("Subtype", Object::Name(b"Image".to_vec())), ("Width", Object::Integer(w as i64)),
                        ("Height", Object::Integer(h as i64)), ("ColorSpace", Object::Name(b"DeviceGray".to_vec())), ("BitsPerComponent", Object::Integer(8))] { m.dict.set(k, v); }
                    d.set("SMask", Object::Reference(self.doc.add_object(m)));
                }
                let mut s = compressed(&rgb, None)?;
                for (k, v) in d.iter() { s.dict.set(k.clone(), v.clone()); }
                Ok(Image { id: self.doc.add_object(s), width: w, height: h })
            }
        }
    }

    /// Operators drawing `img` into the box (x, y, w, h) in PDF space.
    pub fn image_ops(&mut self, page: ObjectId, img: &Image, x: f64, y: f64, w: f64, h: f64) -> Result<String> {
        let name = format!("Im{}", img.id.0);
        if self.registered.insert((page, name.clone())) { self.add_resource(page, "XObject", &name, img.id)?; }
        Ok(format!("q {} 0 0 {} {} {} cm /{name} Do Q\n", n(w), n(h), n(x), n(y)))
    }

    /// Queues operators to run after the page's existing content.
    pub fn push(&mut self, page: ObjectId, ops: &str) { self.new_content.entry(page).or_default().push_str(ops); }

    fn add_resource(&mut self, page: ObjectId, category: &str, name: &str, target: ObjectId) -> Result<()> {
        // Resources may be direct or indirect; so may the category dictionary.
        let res_ref = self.doc.get_dictionary(page).map_err(internal)?.get(b"Resources").ok().and_then(|o| o.as_reference().ok());
        let cat_obj = {
            let res = match res_ref { Some(r) => self.doc.get_dictionary(r), None => {
                match self.doc.get_dictionary(page).map_err(internal)?.get(b"Resources") { Ok(Object::Dictionary(d)) => Ok(d), _ => return self.ensure_resources(page, category, name, target) }
            } }.map_err(internal)?;
            res.get(category.as_bytes()).ok().cloned()
        };
        let cat_ref = cat_obj.as_ref().and_then(|o| o.as_reference().ok());
        let set = |d: &mut Dictionary| d.set(name, Object::Reference(target));
        if let Some(cr) = cat_ref { set(self.doc.get_object_mut(cr).and_then(Object::as_dict_mut).map_err(internal)?); return Ok(()); }
        let res = match res_ref {
            Some(r) => self.doc.get_object_mut(r).and_then(Object::as_dict_mut),
            None => self.doc.get_object_mut(page).and_then(Object::as_dict_mut).and_then(|p| p.get_mut(b"Resources")).and_then(Object::as_dict_mut),
        }.map_err(internal)?;
        match res.get_mut(category.as_bytes()).and_then(Object::as_dict_mut) {
            Ok(d) => set(d),
            Err(_) => res.set(category, Dictionary::from_iter([(name, Object::Reference(target))])),
        }
        Ok(())
    }

    fn ensure_resources(&mut self, page: ObjectId, category: &str, name: &str, target: ObjectId) -> Result<()> {
        let p = self.doc.get_object_mut(page).and_then(Object::as_dict_mut).map_err(internal)?;
        p.set("Resources", Dictionary::from_iter([(category, Object::Dictionary(Dictionary::from_iter([(name, Object::Reference(target))])))]));
        Ok(())
    }

    /// Flushes queued drawing into the pages and returns the finished file.
    pub fn finish(mut self) -> Result<Vec<u8>> {
        self.finish_fonts()?;
        let queued: Vec<(ObjectId, String)> = self.new_content.drain().collect();
        for (page, ops) in queued {
            let mut streams: Vec<Object> = vec![];
            if self.wrapped.insert(page) {
                let q = self.doc.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
                let big_q = self.doc.add_object(Stream::new(Dictionary::new(), b"\nQ\n".to_vec()));
                let existing = self.doc.get_dictionary(page).map_err(internal)?.get(b"Contents").ok().cloned();
                let mut mid: Vec<Object> = match existing {
                    Some(Object::Reference(r)) => match self.doc.get_object(r) { Ok(Object::Array(a)) => a.clone(), _ => vec![Object::Reference(r)] },
                    Some(Object::Array(a)) => a, _ => vec![],
                };
                streams.push(Object::Reference(q)); streams.append(&mut mid); streams.push(Object::Reference(big_q));
            } else if let Ok(Object::Array(a)) = self.doc.get_dictionary(page).map_err(internal)?.get(b"Contents") { streams = a.clone(); }
            let ours = self.doc.add_object(Stream::new(Dictionary::new(), ops.into_bytes()));
            streams.push(Object::Reference(ours));
            self.doc.get_object_mut(page).and_then(Object::as_dict_mut).map_err(internal)?.set("Contents", Object::Array(streams));
        }
        super::pages::save(self.doc)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat { Png, Jpg }

fn compressed(data: &[u8], extra: Option<(&str, i64)>) -> Result<Stream> {
    use std::io::Write;
    let mut e = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    e.write_all(data).map_err(internal)?;
    let mut d = Dictionary::from_iter([("Filter", Object::Name(b"FlateDecode".to_vec()))]);
    if let Some((k, v)) = extra { d.set(k, Object::Integer(v)); }
    Ok(Stream::new(d, e.finish().map_err(internal)?).with_compression(false))
}

/// Compact number formatting for content streams.
pub fn n(v: f64) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" { "0".into() } else { s.into() }
}

pub fn hex_literal(bytes: &[u8]) -> Object { Object::String(bytes.to_vec(), StringFormat::Hexadecimal) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn helvetica_widths_match_known_values() {
        // "Hello": H722 e556 l222 l222 o556 = 2278 → 22.78pt at 10pt
        assert!((std_width(Std::Helvetica, "Hello", 10.0).unwrap() - 22.78).abs() < 1e-9);
        assert!(std_width(Std::HelveticaBold, "Hello", 10.0).unwrap() > 22.78);
    }
    #[test] fn winansi_rejects_what_it_cannot_draw() {
        assert_eq!(std_encode(Std::Helvetica, "é€").unwrap(), vec![0xE9, 0x80]);
        assert_eq!(std_encode(Std::Helvetica, "ə").unwrap_err().code, ErrorCode::EInvalidField);
    }
    #[test] fn number_format() { assert_eq!(n(1.0), "1"); assert_eq!(n(0.4500), "0.45"); assert_eq!(n(-0.0001), "0"); assert_eq!(n(12.34567), "12.346"); }
}
