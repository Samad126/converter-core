//! Minimal WordprocessingML writer: styles, lists, tables, images, hyperlinks, page size.
use crate::native::{model::*, util::{esc_xml, ext_for_mime}};
use crate::{Error, ErrorCode, Result};
use std::io::Write;
use std::path::Path;

const W: &str = "http://schemas.openxmlformats.org/wordprocessingml/2006/main";
const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const EMU_PER_PT: f32 = 12_700.0;

fn zerr(e: impl std::fmt::Display) -> Error { Error::new(ErrorCode::EInternal, e.to_string()) }

#[derive(Default)]
struct Ctx { rels: Vec<(String, String, String, bool)>, media: Vec<(String, Vec<u8>)>, next_id: u32 }

impl Ctx {
    fn rel(&mut self, ty: &str, target: String, external: bool) -> String {
        self.next_id += 1;
        let id = format!("rId{}", self.next_id + 10);
        self.rels.push((id.clone(), ty.to_string(), target, external));
        id
    }
}

fn run_xml(r: &Run, c: &mut Ctx) -> String {
    let mut pr = String::new();
    if r.mono { pr.push_str("<w:rFonts w:ascii=\"Courier New\" w:hAnsi=\"Courier New\" w:cs=\"Courier New\"/>"); }
    if r.bold { pr.push_str("<w:b/>"); }
    if r.italic { pr.push_str("<w:i/>"); }
    if r.underline || r.link.is_some() { pr.push_str("<w:u w:val=\"single\"/>"); }
    let color = r.color.or(r.link.as_ref().map(|_| [0x05, 0x63, 0xc1]));
    if let Some(col) = color { if col != [0, 0, 0] { pr.push_str(&format!("<w:color w:val=\"{:02X}{:02X}{:02X}\"/>", col[0], col[1], col[2])); } }
    if let Some(s) = r.size_pt { pr.push_str(&format!("<w:sz w:val=\"{}\"/>", (s * 2.0).round() as u32)); }
    let rpr = if pr.is_empty() { String::new() } else { format!("<w:rPr>{pr}</w:rPr>") };
    let mut body = String::new();
    for (i, part) in r.text.split('\n').enumerate() {
        if i > 0 { body.push_str("<w:br/>"); }
        for (j, seg) in part.split('\t').enumerate() {
            if j > 0 { body.push_str("<w:tab/>"); }
            if !seg.is_empty() { body.push_str(&format!("<w:t xml:space=\"preserve\">{}</w:t>", esc_xml(seg))); }
        }
    }
    let run = format!("<w:r>{rpr}{body}</w:r>");
    match &r.link {
        Some(url) => { let id = c.rel("hyperlink", url.clone(), true); format!("<w:hyperlink r:id=\"{id}\">{run}</w:hyperlink>") }
        None => run,
    }
}

fn para_xml(p: &Para, c: &mut Ctx) -> String {
    let mut ppr = String::new();
    if let Some(h) = p.heading { ppr.push_str(&format!("<w:pStyle w:val=\"Heading{}\"/>", h.clamp(1, 6))); }
    if let (Some(l), None) = (&p.list, p.heading) {
        ppr.push_str(&format!("<w:numPr><w:ilvl w:val=\"{}\"/><w:numId w:val=\"{}\"/></w:numPr>", l.level.min(8), if l.ordered { 2 } else { 1 }));
    }
    match p.align { Align::Center => ppr.push_str("<w:jc w:val=\"center\"/>"), Align::Right => ppr.push_str("<w:jc w:val=\"right\"/>"), Align::Justify => ppr.push_str("<w:jc w:val=\"both\"/>"), Align::Left => {} }
    let runs: String = p.runs.iter().map(|r| run_xml(r, c)).collect();
    format!("<w:p>{}{runs}</w:p>", if ppr.is_empty() { String::new() } else { format!("<w:pPr>{ppr}</w:pPr>") })
}

fn image_xml(img: &Image, c: &mut Ctx, max_w_pt: f32) -> String {
    let (nw, nh) = image::ImageReader::new(std::io::Cursor::new(&img.data)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok())
        .map(|(w, h)| (w as f32 * 0.75, h as f32 * 0.75)).unwrap_or((200.0, 150.0));
    let (mut w, mut h) = match (img.width_pt, img.height_pt) { (Some(w), Some(h)) if w > 0.0 && h > 0.0 => (w, h), _ => (nw, nh) };
    if w > max_w_pt { let k = max_w_pt / w; w *= k; h *= k; }
    let n = c.media.len() + 1;
    let name = format!("image{n}.{}", ext_for_mime(&img.mime));
    c.media.push((name.clone(), img.data.clone()));
    let id = c.rel("image", format!("media/{name}"), false);
    let (cx, cy) = ((w * EMU_PER_PT) as i64, (h * EMU_PER_PT) as i64);
    format!("<w:p><w:r><w:drawing><wp:inline distT=\"0\" distB=\"0\" distL=\"0\" distR=\"0\"><wp:extent cx=\"{cx}\" cy=\"{cy}\"/><wp:docPr id=\"{n}\" name=\"Picture {n}\"/><a:graphic xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\"><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:pic xmlns:pic=\"http://schemas.openxmlformats.org/drawingml/2006/picture\"><pic:nvPicPr><pic:cNvPr id=\"{n}\" name=\"{name}\"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed=\"{id}\"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{cx}\" cy=\"{cy}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>")
}

fn table_xml(t: &Table, c: &mut Ctx, width_twips: u32) -> String {
    let cols = t.columns().max(1);
    let col_w = width_twips / cols as u32;
    let grid: String = (0..cols).map(|_| format!("<w:gridCol w:w=\"{col_w}\"/>")).collect();
    let rows: String = t.rows.iter().map(|row| {
        let cells: String = row.iter().map(|cell| {
            let span = cell.col_span.max(1);
            let mut inner: String = cell.blocks.iter().map(|b| block_xml(b, c, width_twips, col_w)).collect();
            if !inner.ends_with("</w:p>") { inner.push_str("<w:p/>"); }
            format!("<w:tc><w:tcPr><w:tcW w:w=\"{}\" w:type=\"dxa\"/>{}</w:tcPr>{inner}</w:tc>", col_w * span as u32, if span > 1 { format!("<w:gridSpan w:val=\"{span}\"/>") } else { String::new() })
        }).collect();
        format!("<w:tr>{cells}</w:tr>")
    }).collect();
    format!("<w:tbl><w:tblPr><w:tblStyle w:val=\"TableGrid\"/><w:tblW w:w=\"{width_twips}\" w:type=\"dxa\"/></w:tblPr><w:tblGrid>{grid}</w:tblGrid>{rows}</w:tbl>")
}

fn block_xml(b: &Block, c: &mut Ctx, width_twips: u32, max_img_twips: u32) -> String {
    match b {
        Block::Para(p) => para_xml(p, c),
        Block::Table(t) => format!("{}<w:p/>", table_xml(t, c, width_twips)),
        Block::Image(i) => image_xml(i, c, max_img_twips as f32 / 20.0),
        Block::PageBreak => "<w:p><w:r><w:br w:type=\"page\"/></w:r></w:p>".to_string(),
    }
}

fn styles() -> String {
    let mut s = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:styles xmlns:w=\"{W}\"><w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\" w:eastAsia=\"Malgun Gothic\" w:cs=\"Calibri\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after=\"120\" w:line=\"264\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults><w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>");
    for (i, sz) in [40u32, 32, 28, 25, 23, 22].iter().enumerate() {
        let n = i + 1;
        s.push_str(&format!("<w:style w:type=\"paragraph\" w:styleId=\"Heading{n}\"><w:name w:val=\"heading {n}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/><w:pPr><w:keepNext/><w:spacing w:before=\"240\" w:after=\"80\"/><w:outlineLvl w:val=\"{i}\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"{sz}\"/><w:szCs w:val=\"{sz}\"/></w:rPr></w:style>"));
    }
    s.push_str("<w:style w:type=\"table\" w:default=\"1\" w:styleId=\"TableNormal\"><w:name w:val=\"Normal Table\"/><w:tblPr><w:tblCellMar><w:top w:w=\"0\" w:type=\"dxa\"/><w:left w:w=\"108\" w:type=\"dxa\"/><w:bottom w:w=\"0\" w:type=\"dxa\"/><w:right w:w=\"108\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr></w:style><w:style w:type=\"table\" w:styleId=\"TableGrid\"><w:name w:val=\"Table Grid\"/><w:basedOn w:val=\"TableNormal\"/><w:tblPr><w:tblBorders><w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"999999\"/><w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"999999\"/><w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"999999\"/><w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"999999\"/><w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"999999\"/><w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"999999\"/></w:tblBorders></w:tblPr></w:style></w:styles>");
    s
}

fn numbering() -> String {
    let lvls = |bullet: bool| -> String {
        (0..9).map(|l| {
            let (fmt, text) = if bullet { ("bullet", "\u{2022}".to_string()) } else { ("decimal", format!("%{}.", l + 1)) };
            format!("<w:lvl w:ilvl=\"{l}\"><w:start w:val=\"1\"/><w:numFmt w:val=\"{fmt}\"/><w:lvlText w:val=\"{text}\"/><w:lvlJc w:val=\"left\"/><w:pPr><w:ind w:left=\"{}\" w:hanging=\"360\"/></w:pPr></w:lvl>", 720 + l * 360)
        }).collect()
    };
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:numbering xmlns:w=\"{W}\"><w:abstractNum w:abstractNumId=\"0\"><w:multiLevelType w:val=\"hybridMultilevel\"/>{}</w:abstractNum><w:abstractNum w:abstractNumId=\"1\"><w:multiLevelType w:val=\"hybridMultilevel\"/>{}</w:abstractNum><w:num w:numId=\"1\"><w:abstractNumId w:val=\"0\"/></w:num><w:num w:numId=\"2\"><w:abstractNumId w:val=\"1\"/></w:num></w:numbering>", lvls(true), lvls(false))
}

pub fn write(doc: &Doc, out: &Path) -> Result<()> {
    let mut c = Ctx::default();
    let (pw, ph, m) = match doc.page { Some(p) => (p.width_pt, p.height_pt, p.margins), None => (595.28, 841.89, [72.0; 4]) };
    let text_w = ((pw - m[1] - m[3]) * 20.0).max(2000.0) as u32;
    let body: String = doc.blocks.iter().map(|b| block_xml(b, &mut c, text_w, text_w)).collect();
    let sect = format!("<w:sectPr><w:pgSz w:w=\"{}\" w:h=\"{}\"/><w:pgMar w:top=\"{}\" w:right=\"{}\" w:bottom=\"{}\" w:left=\"{}\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr>",
        (pw * 20.0) as u32, (ph * 20.0) as u32, (m[0] * 20.0) as u32, (m[1] * 20.0) as u32, (m[2] * 20.0) as u32, (m[3] * 20.0) as u32);
    let document = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<w:document xmlns:w=\"{W}\" xmlns:r=\"{R}\" xmlns:wp=\"http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing\"><w:body>{body}{sect}</w:body></w:document>");

    let mut rels = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/><Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering\" Target=\"numbering.xml\"/>");
    for (id, ty, target, ext) in &c.rels {
        rels.push_str(&format!("<Relationship Id=\"{id}\" Type=\"{R}/{ty}\" Target=\"{}\"{}/>", esc_xml(target), if *ext { " TargetMode=\"External\"" } else { "" }));
    }
    rels.push_str("</Relationships>");

    let mut exts: Vec<&str> = c.media.iter().map(|(n, _)| n.rsplit('.').next().unwrap_or("png")).collect();
    exts.sort(); exts.dedup();
    let defaults: String = exts.iter().map(|e| format!("<Default Extension=\"{e}\" ContentType=\"{}\"/>", match *e { "jpg" => "image/jpeg", "png" => "image/png", "gif" => "image/gif", "bmp" => "image/bmp", "webp" => "image/webp", "tiff" => "image/tiff", _ => "application/octet-stream" })).collect();
    let types = format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/>{defaults}<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/><Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/><Override PartName=\"/word/numbering.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml\"/></Types>");

    let mut z = zip::ZipWriter::new(std::fs::File::create(out)?);
    let o = zip::write::SimpleFileOptions::default();
    let mut put = |name: &str, data: &[u8]| -> Result<()> { z.start_file(name, o).map_err(zerr)?; z.write_all(data)?; Ok(()) };
    put("[Content_Types].xml", types.as_bytes())?;
    put("_rels/.rels", format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"{R}/officeDocument\" Target=\"word/document.xml\"/></Relationships>").as_bytes())?;
    put("word/document.xml", document.as_bytes())?;
    put("word/_rels/document.xml.rels", rels.as_bytes())?;
    put("word/styles.xml", styles().as_bytes())?;
    put("word/numbering.xml", numbering().as_bytes())?;
    for (name, data) in &c.media { put(&format!("word/media/{name}"), data)?; }
    z.finish().map_err(zerr)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn round_trips_through_our_own_reader() {
        let mut d = Doc::default();
        d.heading(1, "Title");
        d.blocks.push(Block::Para(Para { runs: vec![Run { text: "bold & <tag>".into(), bold: true, ..Default::default() }, Run { text: " link".into(), link: Some("https://x.test/?a=1&b=2".into()), ..Default::default() }], ..Default::default() }));
        d.blocks.push(Block::Para(Para { runs: vec![Run::plain("item")], list: Some(ListInfo { ordered: true, level: 0 }), ..Default::default() }));
        d.blocks.push(Block::Table(Table::from_text(&[vec!["a".into(), "b".into()], vec!["1".into(), "2".into()]])));
        let p = std::env::temp_dir().join(format!("docx-rt-{}.docx", std::process::id()));
        write(&d, &p).unwrap();
        let back = crate::native::read::docx::read(std::io::BufReader::new(std::fs::File::open(&p).unwrap()), &crate::Config::default()).unwrap();
        let Block::Para(h) = &back.blocks[0] else { panic!() };
        assert_eq!((h.heading, h.text().as_str()), (Some(1), "Title"));
        let Block::Para(p1) = &back.blocks[1] else { panic!() };
        assert!(p1.runs[0].bold && p1.text() == "bold & <tag> link");
        assert_eq!(p1.runs[1].link.as_deref(), Some("https://x.test/?a=1&b=2"));
        let Block::Para(li) = &back.blocks[2] else { panic!() };
        assert_eq!(li.list.as_ref().map(|l| l.ordered), Some(true));
        let Block::Table(t) = &back.blocks[3] else { panic!() };
        assert_eq!(t.to_text(), vec![vec!["a", "b"], vec!["1", "2"]]);
    }
}
