//! watermark, page numbers, scan-to-PDF, edit, sign. Ports of `pdf-pages.service.ts`,
//! `pdf-edit.service.ts`, `pdf-sign.service.ts` (same geometry, colours and fitting rules).
use super::draw::{invalid, n, FontRef, Image, ImageFormat, Pdf, Std, TrueType};
use crate::{Error, ErrorCode, Result};
use lopdf::{Dictionary, Document, Object};
use serde::Deserialize;

const DANCING_SCRIPT: &[u8] = include_bytes!("../../assets/fonts/DancingScript.ttf");

fn page_targets(pdf: &Pdf, indices: Option<&[usize]>) -> Vec<usize> {
    indices.map(<[usize]>::to_vec).unwrap_or_else(|| (0..pdf.page_count()).collect())
}

pub fn watermark(bytes: &[u8], text: &str, indices: Option<&[usize]>) -> Result<Vec<u8>> {
    let mut pdf = Pdf::load(bytes)?;
    for i in page_targets(&pdf, indices) {
        let page = pdf.page(i + 1)?;
        let (w, h) = pdf.page_size(page);
        let size = (w.min(h) * 0.12).max(8.0);
        let tw = pdf.text_width(FontRef::Std(Std::HelveticaBold), text, size)?;
        let gs = pdf.opacity(page, 0.45)?;
        let ops = pdf.text_ops(page, FontRef::Std(Std::HelveticaBold), text, size, w / 2.0 - tw / 2.0, h * 0.1, [0.5; 3])?;
        pdf.push(page, &format!("q /{gs} gs\n{ops}Q\n"));
    }
    pdf.finish()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NumberPosition { BottomCenter, BottomRight, BottomLeft }

pub fn page_numbers(bytes: &[u8], position: NumberPosition, start_at: i64) -> Result<Vec<u8>> {
    let mut pdf = Pdf::load(bytes)?;
    let margin = 24.0;
    for i in 0..pdf.page_count() {
        let page = pdf.page(i + 1)?;
        let (w, _) = pdf.page_size(page);
        let label = (start_at + i as i64).to_string();
        let tw = pdf.text_width(FontRef::Std(Std::Helvetica), &label, 10.0)?;
        let x = match position { NumberPosition::BottomLeft => margin, NumberPosition::BottomRight => w - margin - tw, NumberPosition::BottomCenter => w / 2.0 - tw / 2.0 };
        let ops = pdf.text_ops(page, FontRef::Std(Std::Helvetica), &label, 10.0, x, margin / 2.0, [0.3; 3])?;
        pdf.push(page, &format!("q\n{ops}Q\n"));
    }
    pdf.finish()
}

/// One page per image, sized to the image's pixel dimensions (as pdf-lib does).
pub fn scan_to_pdf(images: &[(Vec<u8>, ImageFormat)]) -> Result<Vec<u8>> {
    if images.is_empty() { return Err(Error::new(ErrorCode::ETooFewFiles, "scanToPdf needs at least one image.")); }
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let cat = doc.add_object(Dictionary::from_iter([("Type", Object::Name(b"Catalog".to_vec())), ("Pages", Object::Reference(pages_id))]));
    doc.trailer.set("Root", Object::Reference(cat));
    let mut pdf = Pdf::from_document(doc);
    let mut kids = vec![];
    for (data, fmt) in images {
        let img = pdf.embed_image(data, *fmt)?;
        let page = pdf.doc.add_object(Dictionary::from_iter([("Type", Object::Name(b"Page".to_vec())), ("Parent", Object::Reference(pages_id)),
            ("MediaBox", Object::Array(vec![0.into(), 0.into(), (img.width as i64).into(), (img.height as i64).into()])),
            ("Resources", Dictionary::new().into()), ("Contents", Object::Array(vec![]))]));
        kids.push(Object::Reference(page));
        let ops = pdf.image_ops(page, &img, 0.0, 0.0, img.width as f64, img.height as f64)?;
        pdf.push(page, &ops);
    }
    pdf.doc.objects.insert(pages_id, Object::Dictionary(Dictionary::from_iter([("Type", Object::Name(b"Pages".to_vec())),
        ("Count", Object::Integer(kids.len() as i64)), ("Kids", Object::Array(kids))])));
    pdf.finish()
}

// ---------------------------------------------------------------- edit

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EditColor { Black, Red, Blue, Green, Yellow, Orange }

impl EditColor {
    fn rgb(self) -> [f64; 3] {
        match self { Self::Black => [0.05, 0.05, 0.05], Self::Red => [0.75, 0.08, 0.08], Self::Blue => [0.09, 0.25, 0.7],
            Self::Green => [0.08, 0.5, 0.18], Self::Yellow => [0.85, 0.7, 0.05], Self::Orange => [0.85, 0.45, 0.05] }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Point { pub x: f64, pub y: f64 }

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EditKind { Text, Image, Rectangle, Ellipse, Line, Freehand }

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditElement {
    #[serde(rename = "type")] pub kind: EditKind,
    pub page: usize,
    pub color: Option<EditColor>,
    pub stroke_width: Option<f64>,
    pub fill: Option<bool>,
    pub value: Option<String>,
    pub font_size: Option<f64>,
    pub x: Option<f64>, pub y: Option<f64>, pub width: Option<f64>, pub height: Option<f64>,
    pub image_index: Option<usize>,
    pub x1: Option<f64>, pub y1: Option<f64>, pub x2: Option<f64>, pub y2: Option<f64>,
    pub points: Option<Vec<Point>>,
}

fn req<T>(v: Option<T>, what: &str) -> Result<T> { v.ok_or_else(|| invalid(format!("An element is missing \"{what}\"."))) }

fn ellipse_path(cx: f64, cy: f64, rx: f64, ry: f64) -> String {
    const K: f64 = 0.552_284_749_8;
    let (ox, oy) = (rx * K, ry * K);
    format!("{} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c h ",
        n(cx + rx), n(cy),
        n(cx + rx), n(cy + oy), n(cx + ox), n(cy + ry), n(cx), n(cy + ry),
        n(cx - ox), n(cy + ry), n(cx - rx), n(cy + oy), n(cx - rx), n(cy),
        n(cx - rx), n(cy - oy), n(cx - ox), n(cy - ry), n(cx), n(cy - ry),
        n(cx + ox), n(cy - ry), n(cx + rx), n(cy - oy), n(cx + rx), n(cy))
}

pub fn edit(bytes: &[u8], elements: &[EditElement], images: &[(Vec<u8>, ImageFormat)]) -> Result<Vec<u8>> {
    let mut pdf = Pdf::load(bytes)?;
    let mut embedded: std::collections::HashMap<usize, Image> = Default::default();
    for e in elements {
        let page = pdf.page(e.page)?;
        let (_, ph) = pdf.page_size(page);
        let c = e.color.unwrap_or(EditColor::Black).rgb();
        let sw = e.stroke_width.unwrap_or(2.0);
        let col = |op: &str| format!("{} {} {} {op}", n(c[0]), n(c[1]), n(c[2]));
        let ops = match e.kind {
            EditKind::Text => {
                let size = e.font_size.unwrap_or(14.0);
                pdf.text_ops(page, FontRef::Std(Std::Helvetica), e.value.as_deref().unwrap_or(""), size, req(e.x, "x")?, ph - req(e.y, "y")? - size, c)?
            }
            EditKind::Image => {
                let idx = req(e.image_index, "imageIndex")?;
                if !embedded.contains_key(&idx) {
                    let (data, fmt) = images.get(idx).ok_or_else(|| Error::new(ErrorCode::EInternal, format!("edit element referenced images[{idx}], which was not uploaded")))?;
                    embedded.insert(idx, pdf.embed_image(data, *fmt)?);
                }
                let (h, y) = (req(e.height, "height")?, req(e.y, "y")?);
                pdf.image_ops(page, &embedded[&idx], req(e.x, "x")?, ph - y - h, req(e.width, "width")?, h)?
            }
            EditKind::Rectangle => {
                let (x, y, w, h) = (req(e.x, "x")?, req(e.y, "y")?, req(e.width, "width")?, req(e.height, "height")?);
                let fill = e.fill == Some(true);
                format!("q {} {} w {} {} {} {} {} re {}\nQ\n", col("RG"), n(sw), if fill { col("rg") } else { String::new() },
                    n(x), n(ph - y - h), n(w), n(h), if fill { "B" } else { "S" })
            }
            EditKind::Ellipse => {
                let (x, y, w, h) = (req(e.x, "x")?, req(e.y, "y")?, req(e.width, "width")?, req(e.height, "height")?);
                format!("q {} {} w {} {}{}\nQ\n", col("RG"), n(sw), if e.fill == Some(true) { col("rg") + " " } else { String::new() },
                    ellipse_path(x + w / 2.0, ph - y - h / 2.0, w / 2.0, h / 2.0), if e.fill == Some(true) { "B" } else { "S" })
            }
            EditKind::Line => format!("q {} {} w {} {} m {} {} l S\nQ\n", col("RG"), n(sw),
                n(req(e.x1, "x1")?), n(ph - req(e.y1, "y1")?), n(req(e.x2, "x2")?), n(ph - req(e.y2, "y2")?)),
            EditKind::Freehand => {
                let pts = req(e.points.as_ref(), "points")?;
                let mut s = format!("q {} {} w ", col("RG"), n(sw));
                for w in pts.windows(2) { s.push_str(&format!("{} {} m {} {} l S ", n(w[0].x), n(ph - w[0].y), n(w[1].x), n(ph - w[1].y))); }
                s + "\nQ\n"
            }
        };
        pdf.push(page, &if e.kind == EditKind::Text { format!("q\n{ops}Q\n") } else { ops });
    }
    pdf.finish()
}

// ---------------------------------------------------------------- sign

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SignType { Signature, Initials, Stamp, Name, Date, Text }

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FontStyle { Cursive, Cursive2, Plain }

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SignColor { Black, Red, Blue, Green }

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignElement {
    #[serde(rename = "type")] pub kind: SignType,
    pub page: usize,
    pub x: f64, pub y: f64, pub width: f64, pub height: f64,
    pub value: Option<String>,
    pub image_index: Option<usize>,
    pub font_style: Option<FontStyle>,
    pub color: Option<SignColor>,
}

fn fit_font_size(pdf: &Pdf, font: FontRef, text: &str, bw: f64, bh: f64) -> Result<f64> {
    let mut size = 48f64.min(6f64.max(bh * 0.7));
    while size > 6.0 && pdf.text_width(font, text, size)? > bw * 0.95 { size -= 1.0; }
    Ok(size)
}

pub fn sign(bytes: &[u8], elements: &[SignElement], images: &[(Vec<u8>, ImageFormat)]) -> Result<Vec<u8>> {
    let mut pdf = Pdf::load(bytes)?;
    let cursive = pdf.add_truetype(TrueType::parse(DANCING_SCRIPT.to_vec())?);
    let mut embedded: std::collections::HashMap<usize, Image> = Default::default();
    for e in elements {
        let page = pdf.page(e.page)?;
        let (_, ph) = pdf.page_size(page);
        let pdf_y = ph - e.y - e.height;
        if let Some(idx) = e.image_index {
            if !embedded.contains_key(&idx) {
                let (data, fmt) = images.get(idx).ok_or_else(|| Error::new(ErrorCode::EInternal, format!("sign element referenced images[{idx}], which was not uploaded")))?;
                embedded.insert(idx, pdf.embed_image(data, *fmt)?);
            }
            let ops = pdf.image_ops(page, &embedded[&idx], e.x, pdf_y, e.width, e.height)?;
            pdf.push(page, &ops);
            continue;
        }
        let text = e.value.as_deref().unwrap_or("");
        let font = match (e.kind, e.font_style) {
            (SignType::Name | SignType::Date | SignType::Text, _) | (_, Some(FontStyle::Plain)) => FontRef::Std(Std::Helvetica),
            (_, Some(FontStyle::Cursive2)) => FontRef::Std(Std::HelveticaBoldOblique),
            _ => cursive,
        };
        let c = match e.color.unwrap_or(SignColor::Black) {
            SignColor::Black => [0.05, 0.05, 0.05], SignColor::Red => [0.75, 0.08, 0.08], SignColor::Blue => [0.09, 0.25, 0.7], SignColor::Green => [0.08, 0.5, 0.18] };
        let size = fit_font_size(&pdf, font, text, e.width, e.height)?;
        let tw = pdf.text_width(font, text, size)?;
        let ops = pdf.text_ops(page, font, text, size, e.x + ((e.width - tw) / 2.0).max(0.0), pdf_y + ((e.height - size) / 2.0).max(0.0), c)?;
        pdf.push(page, &format!("q\n{ops}Q\n"));
    }
    pdf.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::pages::{self, tests::sample};

    fn text_of(bytes: &[u8], page: u32) -> String { pages::load(bytes).unwrap().extract_text(&[page]).unwrap() }

    #[test] fn watermark_only_selected_pages_and_keeps_original() {
        let out = watermark(&sample(3), "DRAFT", Some(&[1])).unwrap();
        assert!(text_of(&out, 2).contains("DRAFT"));
        assert!(!text_of(&out, 1).contains("DRAFT") && !text_of(&out, 3).contains("DRAFT"));
        assert!(text_of(&out, 2).contains("P2"), "original page text must survive");
    }
    #[test] fn page_numbers_count_from_start() {
        let out = page_numbers(&sample(3), NumberPosition::BottomCenter, 5).unwrap();
        for (page, label) in [(1, "5"), (2, "6"), (3, "7")] { assert!(text_of(&out, page).contains(label), "page {page}"); }
    }
    #[test] fn original_content_is_isolated_by_q_q() {
        let out = watermark(&sample(1), "X", None).unwrap();
        let d = pages::load(&out).unwrap();
        let id = *d.get_pages().values().next().unwrap();
        let content = d.get_page_content(id);
        let s = String::from_utf8_lossy(&content);
        assert!(s.trim_start().starts_with('q') && s.contains("\nQ\n"), "{s}");
    }
    #[test] fn scan_to_pdf_sizes_pages_to_images() {
        let mut png = vec![]; image::RgbaImage::from_fn(30, 20, |x, _| image::Rgba([x as u8 * 8, 0, 0, if x < 5 { 0 } else { 255 }]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let mut jpg = vec![]; image::RgbImage::new(40, 50).write_to(&mut std::io::Cursor::new(&mut jpg), image::ImageFormat::Jpeg).unwrap();
        let out = scan_to_pdf(&[(png, ImageFormat::Png), (jpg, ImageFormat::Jpg)]).unwrap();
        let d = pages::load(&out).unwrap();
        let sizes: Vec<_> = d.get_pages().values().map(|id| { let m = pages::media_box(&d, *id).unwrap(); (m[2], m[3]) }).collect();
        assert_eq!(sizes, [(30.0, 20.0), (40.0, 50.0)]);
        assert_eq!(scan_to_pdf(&[]).unwrap_err().code, ErrorCode::ETooFewFiles);
    }
    #[test] fn edit_draws_text_and_validates() {
        let els: Vec<EditElement> = serde_json::from_str(r#"[{"type":"text","page":1,"x":10,"y":10,"value":"Hi there"},
            {"type":"ellipse","page":2,"x":10,"y":10,"width":50,"height":30,"fill":true}]"#).unwrap();
        let out = edit(&sample(2), &els, &[]).unwrap();
        assert!(text_of(&out, 1).contains("Hi there"));
        let bad: Vec<EditElement> = serde_json::from_str(r#"[{"type":"text","page":9,"x":1,"y":1}]"#).unwrap();
        assert_eq!(edit(&sample(2), &bad, &[]).unwrap_err().code, ErrorCode::EInvalidField);
        let missing: Vec<EditElement> = serde_json::from_str(r#"[{"type":"rectangle","page":1,"x":1,"y":1}]"#).unwrap();
        assert_eq!(edit(&sample(1), &missing, &[]).unwrap_err().code, ErrorCode::EInvalidField);
        let no_img: Vec<EditElement> = serde_json::from_str(r#"[{"type":"image","page":1,"x":1,"y":1,"width":5,"height":5,"imageIndex":0}]"#).unwrap();
        assert_eq!(edit(&sample(1), &no_img, &[]).unwrap_err().code, ErrorCode::EInternal);
    }
    #[test] fn sign_embeds_a_type0_font_with_unicode_map() {
        let els: Vec<SignElement> = serde_json::from_str(r#"[{"type":"signature","page":1,"x":50,"y":50,"width":200,"height":50,"value":"Ann Lee"}]"#).unwrap();
        let out = sign(&sample(1), &els, &[]).unwrap();
        let d = pages::load(&out).unwrap();
        let has = |sub: &str| d.objects.values().any(|o| matches!(o, Object::Dictionary(dict) if dict.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) == Some(sub.as_bytes())));
        assert!(has("Type0") && has("CIDFontType2"));
        assert!(d.objects.values().any(|o| matches!(o, Object::Dictionary(dict) if dict.has(b"ToUnicode"))));
        assert!(out.len() > 40_000, "font file should be embedded (compressed), got {}", out.len());
    }
    #[test] fn sign_text_is_fitted_into_the_box() {
        let pdf = Pdf::load(&sample(1)).unwrap();
        let size = fit_font_size(&pdf, FontRef::Std(Std::Helvetica), &"W".repeat(40), 100.0, 40.0).unwrap();
        assert!(size < 20.0 && size >= 6.0);
        assert!(pdf.text_width(FontRef::Std(Std::Helvetica), &"W".repeat(40), size).unwrap() <= 95.0 || size == 6.0);
    }
}
