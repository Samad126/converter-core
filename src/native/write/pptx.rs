//! Minimal PowerPoint writer: one full-bleed picture per slide (what "PDF to PPTX" without an engine produces; the
//! slides look right but their text is not editable).
use crate::{Error, ErrorCode, Result};
use std::io::Write;
use std::path::Path;

const NS_A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
const NS_R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const NS_P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
const REL: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
const EMU_PER_PT: f32 = 12_700.0;

fn zerr(e: impl std::fmt::Display) -> Error { Error::new(ErrorCode::EInternal, e.to_string()) }

pub struct SlideImage { pub width_pt: f32, pub height_pt: f32, pub png: Vec<u8> }

fn group() -> &'static str {
    "<p:nvGrpSpPr><p:cNvPr id=\"1\" name=\"\"/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr><p:grpSpPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/><a:chOff x=\"0\" y=\"0\"/><a:chExt cx=\"0\" cy=\"0\"/></a:xfrm></p:grpSpPr>"
}

fn theme() -> String {
    let fills = "<a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill>".repeat(3);
    let lines = "<a:ln w=\"9525\"><a:solidFill><a:schemeClr val=\"phClr\"/></a:solidFill></a:ln>".repeat(3);
    let effects = "<a:effectStyle><a:effectLst/></a:effectStyle>".repeat(3);
    let font = "<a:latin typeface=\"Calibri\"/><a:ea typeface=\"\"/><a:cs typeface=\"\"/>";
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<a:theme xmlns:a=\"{NS_A}\" name=\"Office\"><a:themeElements><a:clrScheme name=\"Office\"><a:dk1><a:sysClr val=\"windowText\" lastClr=\"000000\"/></a:dk1><a:lt1><a:sysClr val=\"window\" lastClr=\"FFFFFF\"/></a:lt1><a:dk2><a:srgbClr val=\"44546A\"/></a:dk2><a:lt2><a:srgbClr val=\"E7E6E6\"/></a:lt2><a:accent1><a:srgbClr val=\"4472C4\"/></a:accent1><a:accent2><a:srgbClr val=\"ED7D31\"/></a:accent2><a:accent3><a:srgbClr val=\"A5A5A5\"/></a:accent3><a:accent4><a:srgbClr val=\"FFC000\"/></a:accent4><a:accent5><a:srgbClr val=\"5B9BD5\"/></a:accent5><a:accent6><a:srgbClr val=\"70AD47\"/></a:accent6><a:hlink><a:srgbClr val=\"0563C1\"/></a:hlink><a:folHlink><a:srgbClr val=\"954F72\"/></a:folHlink></a:clrScheme><a:fontScheme name=\"Office\"><a:majorFont>{font}</a:majorFont><a:minorFont>{font}</a:minorFont></a:fontScheme><a:fmtScheme name=\"Office\"><a:fillStyleLst>{fills}</a:fillStyleLst><a:lnStyleLst>{lines}</a:lnStyleLst><a:effectStyleLst>{effects}</a:effectStyleLst><a:bgFillStyleLst>{fills}</a:bgFillStyleLst></a:fmtScheme></a:themeElements></a:theme>")
}

fn rels(items: &[(&str, &str, String)]) -> String {
    let body: String = items.iter().map(|(id, ty, target)| format!("<Relationship Id=\"{id}\" Type=\"{REL}/{ty}\" Target=\"{target}\"/>")).collect();
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{body}</Relationships>")
}

pub fn write(slides: &[SlideImage], out: &Path) -> Result<()> {
    if slides.is_empty() { return Err(Error::new(ErrorCode::EConvertFailed, "There are no pages to put on slides.")); }
    let emu = |pt: f32| (pt * EMU_PER_PT).round() as i64;
    // One slide size for the deck (PowerPoint has one); every picture is scaled to fill its own slide's aspect.
    let (sw, sh) = (emu(slides[0].width_pt), emu(slides[0].height_pt));
    let xml = |body: String| format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n{body}");

    let mut z = zip::ZipWriter::new(std::fs::File::create(out)?);
    let o = zip::write::SimpleFileOptions::default();
    let mut put = |name: &str, data: &[u8]| -> Result<()> { z.start_file(name, o).map_err(zerr)?; z.write_all(data)?; Ok(()) };

    let mut types = String::from("<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Default Extension=\"png\" ContentType=\"image/png\"/><Override PartName=\"/ppt/presentation.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml\"/><Override PartName=\"/ppt/slideMasters/slideMaster1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideMaster+xml\"/><Override PartName=\"/ppt/slideLayouts/slideLayout1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slideLayout+xml\"/><Override PartName=\"/ppt/theme/theme1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.theme+xml\"/>");
    for i in 1..=slides.len() { types.push_str(&format!("<Override PartName=\"/ppt/slides/slide{i}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.presentationml.slide+xml\"/>")); }
    types.push_str("</Types>");
    put("[Content_Types].xml", xml(types).as_bytes())?;
    put("_rels/.rels", rels(&[("rId1", "officeDocument", "ppt/presentation.xml".into())]).as_bytes())?;

    let ids: String = (0..slides.len()).map(|i| format!("<p:sldId id=\"{}\" r:id=\"rId{}\"/>", 256 + i, i + 3)).collect();
    put("ppt/presentation.xml", xml(format!("<p:presentation xmlns:a=\"{NS_A}\" xmlns:r=\"{NS_R}\" xmlns:p=\"{NS_P}\"><p:sldMasterIdLst><p:sldMasterId id=\"2147483648\" r:id=\"rId1\"/></p:sldMasterIdLst><p:sldIdLst>{ids}</p:sldIdLst><p:sldSz cx=\"{sw}\" cy=\"{sh}\"/><p:notesSz cx=\"6858000\" cy=\"9144000\"/></p:presentation>")).as_bytes())?;
    let mut pres_rels = vec![("rId1".to_string(), "slideMaster", "slideMasters/slideMaster1.xml".to_string()), ("rId2".to_string(), "theme", "theme/theme1.xml".to_string())];
    for i in 1..=slides.len() { pres_rels.push((format!("rId{}", i + 2), "slide", format!("slides/slide{i}.xml"))); }
    let pr: Vec<(&str, &str, String)> = pres_rels.iter().map(|(a, b, c)| (a.as_str(), *b, c.clone())).collect();
    put("ppt/_rels/presentation.xml.rels", rels(&pr).as_bytes())?;
    put("ppt/theme/theme1.xml", theme().as_bytes())?;

    let clr = "<p:clrMap bg1=\"lt1\" tx1=\"dk1\" bg2=\"lt2\" tx2=\"dk2\" accent1=\"accent1\" accent2=\"accent2\" accent3=\"accent3\" accent4=\"accent4\" accent5=\"accent5\" accent6=\"accent6\" hlink=\"hlink\" folHlink=\"folHlink\"/>";
    put("ppt/slideMasters/slideMaster1.xml", xml(format!("<p:sldMaster xmlns:a=\"{NS_A}\" xmlns:r=\"{NS_R}\" xmlns:p=\"{NS_P}\"><p:cSld><p:bg><p:bgRef idx=\"1001\"><a:schemeClr val=\"bg1\"/></p:bgRef></p:bg><p:spTree>{}</p:spTree></p:cSld>{clr}<p:sldLayoutIdLst><p:sldLayoutId id=\"2147483649\" r:id=\"rId1\"/></p:sldLayoutIdLst></p:sldMaster>", group())).as_bytes())?;
    put("ppt/slideMasters/_rels/slideMaster1.xml.rels", rels(&[("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml".into()), ("rId2", "theme", "../theme/theme1.xml".into())]).as_bytes())?;
    put("ppt/slideLayouts/slideLayout1.xml", xml(format!("<p:sldLayout xmlns:a=\"{NS_A}\" xmlns:r=\"{NS_R}\" xmlns:p=\"{NS_P}\" type=\"blank\" preserve=\"1\"><p:cSld name=\"Blank\"><p:spTree>{}</p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sldLayout>", group())).as_bytes())?;
    put("ppt/slideLayouts/_rels/slideLayout1.xml.rels", rels(&[("rId1", "slideMaster", "../slideMasters/slideMaster1.xml".into())]).as_bytes())?;

    for (i, s) in slides.iter().enumerate() {
        let n = i + 1;
        put(&format!("ppt/media/image{n}.png"), &s.png)?;
        put(&format!("ppt/slides/slide{n}.xml"), xml(format!("<p:sld xmlns:a=\"{NS_A}\" xmlns:r=\"{NS_R}\" xmlns:p=\"{NS_P}\"><p:cSld><p:spTree>{}<p:pic><p:nvPicPr><p:cNvPr id=\"2\" name=\"Page {n}\"/><p:cNvPicPr><a:picLocks noChangeAspect=\"1\"/></p:cNvPicPr><p:nvPr/></p:nvPicPr><p:blipFill><a:blip r:embed=\"rId2\"/><a:stretch><a:fillRect/></a:stretch></p:blipFill><p:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"{sw}\" cy=\"{sh}\"/></a:xfrm><a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom></p:spPr></p:pic></p:spTree></p:cSld><p:clrMapOvr><a:masterClrMapping/></p:clrMapOvr></p:sld>", group())).as_bytes())?;
        put(&format!("ppt/slides/_rels/slide{n}.xml.rels"), rels(&[("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml".into()), ("rId2", "image", format!("../media/image{n}.png"))]).as_bytes())?;
    }
    z.finish().map_err(zerr)?;
    Ok(())
}
