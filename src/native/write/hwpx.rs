//! HWPX (Hancom OWPML) writer: paragraphs with character styles, headings (개요 styles), lists, tables, pictures,
//! page size and margins. The package layout and header structure follow files written by Hancom Office.
use crate::native::{model::*, util::{esc_xml, ext_for_mime}};
use crate::{Error, ErrorCode, Result};
use std::collections::HashMap;
use std::io::Write;
use std::path::Path;

const NS: &str = "xmlns:ha=\"http://www.hancom.co.kr/hwpml/2011/app\" xmlns:hp=\"http://www.hancom.co.kr/hwpml/2011/paragraph\" xmlns:hp10=\"http://www.hancom.co.kr/hwpml/2016/paragraph\" xmlns:hs=\"http://www.hancom.co.kr/hwpml/2011/section\" xmlns:hc=\"http://www.hancom.co.kr/hwpml/2011/core\" xmlns:hh=\"http://www.hancom.co.kr/hwpml/2011/head\" xmlns:hhs=\"http://www.hancom.co.kr/hwpml/2011/history\" xmlns:hm=\"http://www.hancom.co.kr/hwpml/2011/master-page\" xmlns:hpf=\"http://www.hancom.co.kr/schema/2011/hpf\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:opf=\"http://www.idpf.org/2007/opf/\" xmlns:ooxmlchart=\"http://www.hancom.co.kr/hwpml/2016/ooxmlchart\" xmlns:hwpunitchar=\"http://www.hancom.co.kr/hwpml/2016/HwpUnitChar\" xmlns:epub=\"http://www.idpf.org/2007/ops\" xmlns:config=\"urn:oasis:names:tc:opendocument:xmlns:config:1.0\"";
const DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\" ?>";
const BORDER_NONE: u32 = 1;
const BORDER_TABLE: u32 = 3;

fn zerr(e: impl std::fmt::Display) -> Error { Error::new(ErrorCode::EInternal, e.to_string()) }
fn hwp(pt: f32) -> i64 { (pt * 100.0).round() as i64 }

#[derive(Hash, PartialEq, Eq, Clone)]
struct CharKey { size: u32, bold: bool, italic: bool, underline: bool, color: u32 }

#[derive(Hash, PartialEq, Eq, Clone)]
struct ParaKey { align: u8, kind: u8, level: u8 }

#[derive(Default)]
struct Reg { chars: Vec<CharKey>, char_ix: HashMap<CharKey, usize>, paras: Vec<ParaKey>, para_ix: HashMap<ParaKey, usize> }

impl Reg {
    /// Character property ids start at 1 (0 is the default).
    fn char_id(&mut self, k: CharKey) -> usize {
        if let Some(i) = self.char_ix.get(&k) { return *i + 1; }
        self.chars.push(k.clone());
        self.char_ix.insert(k, self.chars.len() - 1);
        self.chars.len()
    }
    /// Paragraph property ids start at 1 (0 is the default).
    fn para_id(&mut self, k: ParaKey) -> usize {
        if let Some(i) = self.para_ix.get(&k) { return *i + 1; }
        self.paras.push(k.clone());
        self.para_ix.insert(k, self.paras.len() - 1);
        self.paras.len()
    }
}

struct Ctx { reg: Reg, base: f32, next_id: u64, images: Vec<(String, String, Vec<u8>)>, text_width: i64 }

fn color_key(c: Option<[u8; 3]>) -> u32 { c.map_or(0, |c| (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32) }

fn heading_size(base: f32, level: u8) -> f32 { base * [1.8, 1.45, 1.25, 1.1, 1.0, 1.0][(level.clamp(1, 6) - 1) as usize] }

fn runs_xml(p: &Para, c: &mut Ctx) -> String {
    let mut out = String::new();
    let heading_bold = p.heading.is_some();
    for r in &p.runs {
        if r.text.is_empty() { continue; }
        let size = r.size_pt.unwrap_or_else(|| p.heading.map_or(c.base, |h| heading_size(c.base, h)));
        let cid = c.reg.char_id(CharKey {
            size: hwp(size) as u32, bold: r.bold || heading_bold, italic: r.italic, underline: r.underline || r.link.is_some(),
            color: color_key(r.color.or(r.link.as_ref().map(|_| [0x05, 0x63, 0xc1]))),
        });
        let mut t = String::new();
        for (i, line) in r.text.split('\n').enumerate() {
            if i > 0 { t.push_str("<hp:lineBreak/>"); }
            for (j, seg) in line.split('\t').enumerate() {
                if j > 0 { t.push_str("<hp:tab width=\"4000\" leader=\"0\" type=\"1\"/>"); }
                t.push_str(&esc_xml(seg));
            }
        }
        out.push_str(&format!("<hp:run charPrIDRef=\"{cid}\"><hp:t>{t}</hp:t></hp:run>"));
    }
    if out.is_empty() { out.push_str("<hp:run charPrIDRef=\"0\"><hp:t/></hp:run>"); }
    out
}

fn para_xml(p: &Para, c: &mut Ctx, first_extra: &str, page_break: bool) -> String {
    let align = match p.align { Align::Left => 0u8, Align::Center => 1, Align::Right => 2, Align::Justify => 3 };
    let (kind, level) = match (&p.heading, &p.list) {
        (Some(h), _) => (1u8, (*h).clamp(1, 6) - 1),
        (None, Some(l)) => (if l.ordered { 2 } else { 3 }, l.level.min(6)),
        _ => (0, 0),
    };
    let pid = if kind == 0 && align == 0 { 0 } else { c.reg.para_id(ParaKey { align, kind, level }) };
    let style = p.heading.map_or(0, |h| h.clamp(1, 6) as usize);
    let runs = runs_xml(p, c);
    let extra = if first_extra.is_empty() { String::new() } else { format!("<hp:run charPrIDRef=\"0\">{first_extra}</hp:run>") };
    format!("<hp:p id=\"0\" paraPrIDRef=\"{pid}\" styleIDRef=\"{style}\" pageBreak=\"{}\" columnBreak=\"0\" merged=\"0\">{extra}{runs}</hp:p>", page_break as u8)
}

fn empty_para(extra: &str, inner: &str, page_break: bool) -> String {
    format!("<hp:p id=\"0\" paraPrIDRef=\"0\" styleIDRef=\"0\" pageBreak=\"{}\" columnBreak=\"0\" merged=\"0\"><hp:run charPrIDRef=\"0\">{extra}{inner}<hp:t/></hp:run></hp:p>", page_break as u8)
}

fn image_xml(img: &Image, c: &mut Ctx) -> String {
    let (nw, nh) = image::ImageReader::new(std::io::Cursor::new(&img.data)).with_guessed_format().ok().and_then(|r| r.into_dimensions().ok())
        .map(|(w, h)| (w as f32 * 0.75, h as f32 * 0.75)).unwrap_or((200.0, 150.0));
    let (mut w, mut h) = match (img.width_pt, img.height_pt) { (Some(w), Some(h)) if w > 0.0 && h > 0.0 => (w, h), _ => (nw, nh) };
    let max_w = c.text_width as f32 / 100.0;
    if w > max_w { let k = max_w / w; w *= k; h *= k; }
    let n = c.images.len() + 1;
    let id = format!("image{n}");
    c.images.push((id.clone(), format!("BinData/{id}.{}", ext_for_mime(&img.mime)), img.data.clone()));
    c.next_id += 1;
    let (cw, ch) = (hwp(w), hwp(h));
    let pic_id = 1_000_000 + c.next_id;
    let pic = format!("<hp:pic id=\"{pic_id}\" zOrder=\"{n}\" numberingType=\"PICTURE\" textWrap=\"TOP_AND_BOTTOM\" textFlow=\"BOTH_SIDES\" lock=\"0\" dropcapstyle=\"None\" href=\"\" groupLevel=\"0\" instid=\"{pic_id}\" reverse=\"0\"><hp:offset x=\"0\" y=\"0\"/><hp:orgSz width=\"{cw}\" height=\"{ch}\"/><hp:curSz width=\"{cw}\" height=\"{ch}\"/><hp:flip horizontal=\"0\" vertical=\"0\"/><hp:rotationInfo angle=\"0\" centerX=\"{}\" centerY=\"{}\" rotateimage=\"1\"/><hp:renderingInfo><hc:transMatrix e1=\"1\" e2=\"0\" e3=\"0\" e4=\"0\" e5=\"1\" e6=\"0\"/><hc:scaMatrix e1=\"1\" e2=\"0\" e3=\"0\" e4=\"0\" e5=\"1\" e6=\"0\"/><hc:rotMatrix e1=\"1\" e2=\"0\" e3=\"0\" e4=\"0\" e5=\"1\" e6=\"0\"/></hp:renderingInfo><hp:imgRect><hc:pt0 x=\"0\" y=\"0\"/><hc:pt1 x=\"{cw}\" y=\"0\"/><hc:pt2 x=\"{cw}\" y=\"{ch}\"/><hc:pt3 x=\"0\" y=\"{ch}\"/></hp:imgRect><hp:imgClip left=\"0\" right=\"{cw}\" top=\"0\" bottom=\"{ch}\"/><hp:inMargin left=\"0\" right=\"0\" top=\"0\" bottom=\"0\"/><hp:imgDim dimwidth=\"{cw}\" dimheight=\"{ch}\"/><hc:img binaryItemIDRef=\"{id}\" bright=\"0\" contrast=\"0\" effect=\"REAL_PIC\" alpha=\"0\"/><hp:effects/><hp:sz width=\"{cw}\" widthRelTo=\"ABSOLUTE\" height=\"{ch}\" heightRelTo=\"ABSOLUTE\" protect=\"0\"/><hp:pos treatAsChar=\"1\" affectLSpacing=\"0\" flowWithText=\"1\" allowOverlap=\"0\" holdAnchorAndSO=\"0\" vertRelTo=\"PARA\" horzRelTo=\"PARA\" vertAlign=\"TOP\" horzAlign=\"LEFT\" vertOffset=\"0\" horzOffset=\"0\"/><hp:outMargin left=\"0\" right=\"0\" top=\"0\" bottom=\"0\"/></hp:pic>", cw / 2, ch / 2);
    empty_para("", &pic, false)
}

fn table_xml(t: &Table, c: &mut Ctx, width: i64) -> String {
    let cols = t.columns().max(1);
    let col_w = width / cols as i64;
    c.next_id += 1;
    let tid = 2_000_000 + c.next_id;
    let rows_xml: String = t.rows.iter().enumerate().map(|(ri, row)| {
        let mut addr = 0usize;
        let cells: String = row.iter().map(|cell| {
            let span = cell.col_span.max(1);
            let paras: String = {
                let mut v = String::new();
                for b in &cell.blocks { if let Block::Para(p) = b { v.push_str(&para_xml(p, c, "", false)); } }
                if v.is_empty() { v.push_str(&para_xml(&Para::default(), c, "", false)); }
                v
            };
            let s = format!("<hp:tc name=\"\" header=\"0\" hasMargin=\"0\" protect=\"0\" editable=\"0\" dirty=\"0\" borderFillIDRef=\"{BORDER_TABLE}\"><hp:subList id=\"\" textDirection=\"HORIZONTAL\" lineWrap=\"BREAK\" vertAlign=\"CENTER\" linkListIDRef=\"0\" linkListNextIDRef=\"0\" textWidth=\"0\" textHeight=\"0\" hasTextRef=\"0\" hasNumRef=\"0\">{paras}</hp:subList><hp:cellAddr colAddr=\"{addr}\" rowAddr=\"{ri}\"/><hp:cellSpan colSpan=\"{span}\" rowSpan=\"1\"/><hp:cellSz width=\"{}\" height=\"1700\"/><hp:cellMargin left=\"510\" right=\"510\" top=\"141\" bottom=\"141\"/></hp:tc>", col_w * span as i64);
            addr += span;
            s
        }).collect();
        format!("<hp:tr>{cells}</hp:tr>")
    }).collect();
    format!("<hp:tbl id=\"{tid}\" zOrder=\"0\" numberingType=\"TABLE\" textWrap=\"TOP_AND_BOTTOM\" textFlow=\"BOTH_SIDES\" lock=\"0\" dropcapstyle=\"None\" pageBreak=\"CELL\" repeatHeader=\"0\" rowCnt=\"{}\" colCnt=\"{cols}\" cellSpacing=\"0\" borderFillIDRef=\"{BORDER_NONE}\" noAdjust=\"0\"><hp:sz width=\"{width}\" widthRelTo=\"ABSOLUTE\" height=\"{}\" heightRelTo=\"ABSOLUTE\" protect=\"0\"/><hp:pos treatAsChar=\"1\" affectLSpacing=\"0\" flowWithText=\"1\" allowOverlap=\"0\" holdAnchorAndSO=\"0\" vertRelTo=\"PARA\" horzRelTo=\"PARA\" vertAlign=\"TOP\" horzAlign=\"LEFT\" vertOffset=\"0\" horzOffset=\"0\"/><hp:outMargin left=\"0\" right=\"0\" top=\"0\" bottom=\"283\"/><hp:inMargin left=\"0\" right=\"0\" top=\"0\" bottom=\"0\"/>{rows_xml}</hp:tbl>", t.rows.len(), t.rows.len() * 1700)
}

fn sec_pr(page: (f32, f32, [f32; 4])) -> String {
    let (w, h, m) = page;
    format!("<hp:secPr id=\"\" textDirection=\"HORIZONTAL\" spaceColumns=\"1134\" tabStop=\"8000\" tabStopVal=\"4000\" tabStopUnit=\"HWPUNIT\" outlineShapeIDRef=\"1\" memoShapeIDRef=\"0\" textVerticalWidthHead=\"0\" masterPageCnt=\"0\"><hp:grid lineGrid=\"0\" charGrid=\"0\" wonggojiFormat=\"0\"/><hp:startNum pageStartsOn=\"BOTH\" page=\"0\" pic=\"0\" tbl=\"0\" equation=\"0\"/><hp:visibility hideFirstHeader=\"0\" hideFirstFooter=\"0\" hideFirstMasterPage=\"0\" border=\"SHOW_ALL\" fill=\"SHOW_ALL\" hideFirstPageNum=\"0\" hideFirstEmptyLine=\"0\" showLineNumber=\"0\"/><hp:lineNumberShape restartType=\"0\" countBy=\"0\" distance=\"0\" startNumber=\"0\"/><hp:pagePr landscape=\"WIDELY\" width=\"{}\" height=\"{}\" gutterType=\"LEFT_RIGHT\"><hp:margin header=\"2835\" footer=\"2835\" gutter=\"0\" left=\"{}\" right=\"{}\" top=\"{}\" bottom=\"{}\"/></hp:pagePr><hp:footNotePr><hp:autoNumFormat type=\"DIGIT\" userChar=\"\" prefixChar=\"\" suffixChar=\")\" supscript=\"0\"/><hp:noteLine length=\"-1\" type=\"SOLID\" width=\"0.12 mm\" color=\"#000000\"/><hp:noteSpacing betweenNotes=\"283\" belowLine=\"567\" aboveLine=\"850\"/><hp:numbering type=\"CONTINUOUS\" newNum=\"1\"/><hp:placement place=\"EACH_COLUMN\" beneathText=\"0\"/></hp:footNotePr><hp:endNotePr><hp:autoNumFormat type=\"DIGIT\" userChar=\"\" prefixChar=\"\" suffixChar=\")\" supscript=\"0\"/><hp:noteLine length=\"14692344\" type=\"SOLID\" width=\"0.12 mm\" color=\"#000000\"/><hp:noteSpacing betweenNotes=\"0\" belowLine=\"567\" aboveLine=\"850\"/><hp:numbering type=\"CONTINUOUS\" newNum=\"1\"/><hp:placement place=\"END_OF_DOCUMENT\" beneathText=\"0\"/></hp:endNotePr><hp:pageBorderFill type=\"BOTH\" borderFillIDRef=\"{BORDER_NONE}\" textBorder=\"PAPER\" headerInside=\"0\" footerInside=\"0\" fillArea=\"PAPER\"><hp:offset left=\"1417\" right=\"1417\" top=\"1417\" bottom=\"1417\"/></hp:pageBorderFill></hp:secPr>",
        hwp(w), hwp(h), hwp(m[3]), hwp(m[1]), hwp(m[0]), hwp(m[2]))
}

fn header_xml(reg: &Reg, base: f32) -> String {
    let font = |lang: &str| format!("<hh:fontface lang=\"{lang}\" fontCnt=\"1\"><hh:font id=\"0\" face=\"함초롬바탕\" type=\"TTF\" isEmbedded=\"0\"/></hh:fontface>");
    let fonts: String = ["HANGUL", "LATIN", "HANJA", "JAPANESE", "OTHER", "SYMBOL", "USER"].iter().map(|l| font(l)).collect();
    let border = |id: u32, ty: &str| format!("<hh:borderFill id=\"{id}\" threeD=\"0\" shadow=\"0\" centerLine=\"NONE\" breakCellSeparateLine=\"0\"><hh:slash type=\"NONE\" Crooked=\"0\" isCounter=\"0\"/><hh:backSlash type=\"NONE\" Crooked=\"0\" isCounter=\"0\"/><hh:leftBorder type=\"{ty}\" width=\"0.12 mm\" color=\"#000000\"/><hh:rightBorder type=\"{ty}\" width=\"0.12 mm\" color=\"#000000\"/><hh:topBorder type=\"{ty}\" width=\"0.12 mm\" color=\"#000000\"/><hh:bottomBorder type=\"{ty}\" width=\"0.12 mm\" color=\"#000000\"/><hh:diagonal type=\"SOLID\" width=\"0.1 mm\" color=\"#000000\"/><hc:fillBrush><hc:winBrush faceColor=\"none\" hatchColor=\"#FF000000\" alpha=\"0\"/></hc:fillBrush></hh:borderFill>");
    let borders = format!("{}{}{}", border(1, "NONE"), border(2, "NONE"), border(3, "SOLID"));
    let char_pr = |id: usize, k: &CharKey| format!("<hh:charPr id=\"{id}\" height=\"{}\" textColor=\"#{:06X}\" shadeColor=\"none\" useFontSpace=\"0\" useKerning=\"0\" symMark=\"NONE\" borderFillIDRef=\"2\"><hh:fontRef hangul=\"0\" latin=\"0\" hanja=\"0\" japanese=\"0\" other=\"0\" symbol=\"0\" user=\"0\"/><hh:ratio hangul=\"100\" latin=\"100\" hanja=\"100\" japanese=\"100\" other=\"100\" symbol=\"100\" user=\"100\"/><hh:spacing hangul=\"0\" latin=\"0\" hanja=\"0\" japanese=\"0\" other=\"0\" symbol=\"0\" user=\"0\"/><hh:relSz hangul=\"100\" latin=\"100\" hanja=\"100\" japanese=\"100\" other=\"100\" symbol=\"100\" user=\"100\"/><hh:offset hangul=\"0\" latin=\"0\" hanja=\"0\" japanese=\"0\" other=\"0\" symbol=\"0\" user=\"0\"/>{}{}<hh:underline type=\"{}\" shape=\"SOLID\" color=\"#000000\"/><hh:strikeout shape=\"NONE\" color=\"#000000\"/><hh:outline type=\"NONE\"/><hh:shadow type=\"NONE\" color=\"#C0C0C0\" offsetX=\"10\" offsetY=\"10\"/></hh:charPr>",
        k.size, k.color, if k.bold { "<hh:bold/>" } else { "" }, if k.italic { "<hh:italic/>" } else { "" }, if k.underline { "BOTTOM" } else { "NONE" });
    let default_char = CharKey { size: hwp(base) as u32, bold: false, italic: false, underline: false, color: 0 };
    let mut chars = char_pr(0, &default_char);
    for (i, k) in reg.chars.iter().enumerate() { chars.push_str(&char_pr(i + 1, k)); }
    let tabs = "<hh:tabPr id=\"0\" autoTabLeft=\"0\" autoTabRight=\"0\"/>";
    let numbering = format!("<hh:numbering id=\"1\" start=\"0\">{}</hh:numbering>", (1..=7).map(|l| format!("<hh:paraHead start=\"1\" level=\"{l}\" align=\"LEFT\" useInstWidth=\"1\" autoIndent=\"1\" widthAdjust=\"0\" textOffsetType=\"PERCENT\" textOffset=\"50\" numFormat=\"DIGIT\" charPrIDRef=\"4294967295\" checkable=\"0\">^{l}.</hh:paraHead>")).collect::<String>());
    let bullets = "<hh:bullet id=\"1\" char=\"•\" useImage=\"0\"><hh:paraHead level=\"0\" align=\"LEFT\" useInstWidth=\"0\" autoIndent=\"1\" widthAdjust=\"0\" textOffsetType=\"PERCENT\" textOffset=\"30\" numFormat=\"DIGIT\" charPrIDRef=\"4294967295\" checkable=\"0\"/></hh:bullet>";
    let para_pr = |id: usize, align: &str, heading: &str, left: i64| format!("<hh:paraPr id=\"{id}\" tabPrIDRef=\"0\" condense=\"0\" fontLineHeight=\"0\" snapToGrid=\"1\" suppressLineNumbers=\"0\" checked=\"0\" textDir=\"LTR\"><hh:align horizontal=\"{align}\" vertical=\"BASELINE\"/>{heading}<hh:breakSetting breakLatinWord=\"KEEP_WORD\" breakNonLatinWord=\"BREAK_WORD\" widowOrphan=\"0\" keepWithNext=\"0\" keepLines=\"0\" pageBreakBefore=\"0\" lineWrap=\"BREAK\"/><hh:autoSpacing eAsianEng=\"0\" eAsianNum=\"0\"/><hp:switch><hp:case hp:required-namespace=\"http://www.hancom.co.kr/hwpml/2016/HwpUnitChar\"><hh:margin><hc:intent value=\"0\" unit=\"HWPUNIT\"/><hc:left value=\"{left}\" unit=\"HWPUNIT\"/><hc:right value=\"0\" unit=\"HWPUNIT\"/><hc:prev value=\"0\" unit=\"HWPUNIT\"/><hc:next value=\"300\" unit=\"HWPUNIT\"/></hh:margin><hh:lineSpacing type=\"PERCENT\" value=\"140\" unit=\"HWPUNIT\"/></hp:case><hp:default><hh:margin><hc:intent value=\"0\" unit=\"HWPUNIT\"/><hc:left value=\"{left}\" unit=\"HWPUNIT\"/><hc:right value=\"0\" unit=\"HWPUNIT\"/><hc:prev value=\"0\" unit=\"HWPUNIT\"/><hc:next value=\"300\" unit=\"HWPUNIT\"/></hh:margin><hh:lineSpacing type=\"PERCENT\" value=\"140\" unit=\"HWPUNIT\"/></hp:default></hp:switch><hh:border borderFillIDRef=\"2\" offsetLeft=\"0\" offsetRight=\"0\" offsetTop=\"0\" offsetBottom=\"0\" connect=\"0\" ignoreMargin=\"0\"/></hh:paraPr>");
    let mut paras = para_pr(0, "LEFT", "<hh:heading type=\"NONE\" idRef=\"0\" level=\"0\"/>", 0);
    for (i, k) in reg.paras.iter().enumerate() {
        let align = ["LEFT", "CENTER", "RIGHT", "JUSTIFY"][k.align as usize];
        let (heading, left) = match k.kind {
            1 => (format!("<hh:heading type=\"OUTLINE\" idRef=\"1\" level=\"{}\"/>", k.level), 0),
            2 => (format!("<hh:heading type=\"NUMBER\" idRef=\"1\" level=\"{}\"/>", k.level), 1500 * (k.level as i64 + 1)),
            3 => (format!("<hh:heading type=\"BULLET\" idRef=\"1\" level=\"{}\"/>", k.level), 1500 * (k.level as i64 + 1)),
            _ => ("<hh:heading type=\"NONE\" idRef=\"0\" level=\"0\"/>".to_string(), 0),
        };
        paras.push_str(&para_pr(i + 1, align, &heading, left));
    }
    // Styles: 0 = 바탕글 (normal), 1..=6 = 개요 1..6; headings reference these by style id.
    let heading_para = |lvl: u8| reg.para_ix.get(&ParaKey { align: 0, kind: 1, level: lvl - 1 }).map(|i| i + 1).unwrap_or(0);
    let mut styles = "<hh:style id=\"0\" type=\"PARA\" name=\"바탕글\" engName=\"Normal\" paraPrIDRef=\"0\" charPrIDRef=\"0\" nextStyleIDRef=\"0\" langID=\"1042\" lockForm=\"0\"/>".to_string();
    for l in 1..=6u8 { styles.push_str(&format!("<hh:style id=\"{l}\" type=\"PARA\" name=\"개요 {l}\" engName=\"Outline {l}\" paraPrIDRef=\"{}\" charPrIDRef=\"0\" nextStyleIDRef=\"0\" langID=\"1042\" lockForm=\"0\"/>", heading_para(l))); }
    format!("{DECL}<hh:head {NS} version=\"1.5\" secCnt=\"1\"><hh:beginNum page=\"1\" footnote=\"1\" endnote=\"1\" pic=\"1\" tbl=\"1\" equation=\"1\"/><hh:refList><hh:fontfaces itemCnt=\"7\">{fonts}</hh:fontfaces><hh:borderFills itemCnt=\"3\">{borders}</hh:borderFills><hh:charProperties itemCnt=\"{}\">{chars}</hh:charProperties><hh:tabProperties itemCnt=\"1\">{tabs}</hh:tabProperties><hh:numberings itemCnt=\"1\">{numbering}</hh:numberings><hh:bullets itemCnt=\"1\">{bullets}</hh:bullets><hh:paraProperties itemCnt=\"{}\">{paras}</hh:paraProperties><hh:styles itemCnt=\"7\">{styles}</hh:styles></hh:refList><hh:compatibleDocument targetProgram=\"HWP201X\"><hh:layoutCompatibility/></hh:compatibleDocument><hh:docOption><hh:linkinfo path=\"\" pageInherit=\"1\" footnoteInherit=\"0\"/></hh:docOption><hh:trackchageConfig flags=\"56\"/></hh:head>",
        reg.chars.len() + 1, reg.paras.len() + 1)
}

pub fn write(doc: &Doc, title: &str, out: &Path) -> Result<()> {
    let page = doc.page.map(|p| (p.width_pt, p.height_pt, p.margins)).unwrap_or((595.28, 841.89, [56.7, 56.7, 56.7, 56.7]));
    let text_width = hwp(page.0 - page.2[1] - page.2[3]).max(10_000);
    let base = doc.base_size_pt.unwrap_or(10.0);
    let mut c = Ctx { reg: Reg::default(), base, next_id: 0, images: vec![], text_width };
    // The six outline styles refer to paragraph properties, so make sure each of those exists.
    for l in 0..6u8 { c.reg.para_id(ParaKey { align: 0, kind: 1, level: l }); }

    let sec = format!("{}<hp:ctrl><hp:colPr id=\"\" type=\"NEWSPAPER\" layout=\"LEFT\" colCount=\"1\" sameSz=\"1\" sameGap=\"0\"/></hp:ctrl>", sec_pr(page));
    let mut body = String::new();
    let mut first = true;
    let mut pending_break = false;
    for b in &doc.blocks {
        // The section definition rides on the first paragraph's first run.
        let extra = if first { first = false; sec.clone() } else { String::new() };
        match b {
            Block::Para(p) => body.push_str(&para_xml(p, &mut c, &extra, std::mem::take(&mut pending_break))),
            Block::Table(t) => {
                let tx = table_xml(t, &mut c, text_width);
                if !extra.is_empty() { body.push_str(&empty_para(&extra, "", false)); }
                body.push_str(&empty_para("", &tx, std::mem::take(&mut pending_break)));
            }
            Block::Image(i) => {
                if !extra.is_empty() { body.push_str(&empty_para(&extra, "", false)); }
                body.push_str(&image_xml(i, &mut c));
            }
            Block::PageBreak => { pending_break = true; if !extra.is_empty() { first = true; } }
        }
    }
    if first { body.push_str(&empty_para(&sec, "", false)); }
    let section = format!("{DECL}<hs:sec {NS}>{body}</hs:sec>");
    let header = header_xml(&c.reg, base);

    let manifest_images: String = c.images.iter().map(|(id, href, _)| format!("<opf:item id=\"{id}\" href=\"{href}\" media-type=\"{}\" isEmbeded=\"1\"/>", match href.rsplit('.').next().unwrap_or("") { "png" => "image/png", "jpg" => "image/jpeg", "gif" => "image/gif", "bmp" => "image/bmp", _ => "application/octet-stream" })).collect();
    let hpf = format!("{DECL}<opf:package {NS} version=\"\" unique-identifier=\"\" id=\"\"><opf:metadata><opf:title>{}</opf:title><opf:language>ko</opf:language></opf:metadata><opf:manifest><opf:item id=\"header\" href=\"Contents/header.xml\" media-type=\"application/xml\"/><opf:item id=\"section0\" href=\"Contents/section0.xml\" media-type=\"application/xml\"/>{manifest_images}</opf:manifest><opf:spine><opf:itemref idref=\"header\" linear=\"yes\"/><opf:itemref idref=\"section0\" linear=\"yes\"/></opf:spine></opf:package>", esc_xml(title));

    let mut z = zip::ZipWriter::new(std::fs::File::create(out)?);
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = zip::write::SimpleFileOptions::default();
    let mut put = |name: &str, data: &[u8], o: zip::write::SimpleFileOptions| -> Result<()> { z.start_file(name, o).map_err(zerr)?; z.write_all(data)?; Ok(()) };
    put("mimetype", b"application/hwp+zip", stored)?;
    put("version.xml", format!("{DECL}<hv:HCFVersion xmlns:hv=\"http://www.hancom.co.kr/hwpml/2011/version\" tagetApplication=\"WORDPROCESSOR\" major=\"5\" minor=\"1\" micro=\"1\" buildNumber=\"0\" os=\"1\" xmlVersion=\"1.5\" application=\"Hancom Office Hangul\" appVersion=\"13, 0, 0, 711 WIN32LEWindows_10\"/>").as_bytes(), deflated)?;
    put("META-INF/container.xml", format!("{DECL}<ocf:container xmlns:ocf=\"urn:oasis:names:tc:opendocument:xmlns:container\" xmlns:hpf=\"http://www.hancom.co.kr/schema/2011/hpf\"><ocf:rootfiles><ocf:rootfile full-path=\"Contents/content.hpf\" media-type=\"application/hwpml-package+xml\"/></ocf:rootfiles></ocf:container>").as_bytes(), deflated)?;
    put("META-INF/manifest.xml", format!("{DECL}<odf:manifest xmlns:odf=\"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0\"/>").as_bytes(), deflated)?;
    put("Contents/content.hpf", hpf.as_bytes(), deflated)?;
    put("Contents/header.xml", header.as_bytes(), deflated)?;
    put("Contents/section0.xml", section.as_bytes(), deflated)?;
    for (_, href, data) in &c.images { put(href, data, deflated)?; }
    z.finish().map_err(zerr)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn round_trips_through_our_own_reader() {
        let mut d = Doc::default();
        d.heading(1, "한글 제목");
        d.blocks.push(Block::Para(Para { runs: vec![Run { text: "굵은 글씨 & <tag>".into(), bold: true, size_pt: Some(14.0), ..Default::default() }, Run::plain(" 일반\n둘째 줄")], align: Align::Center, ..Default::default() }));
        d.blocks.push(Block::Para(Para { runs: vec![Run::plain("항목")], list: Some(ListInfo { ordered: false, level: 0 }), ..Default::default() }));
        d.blocks.push(Block::Table(Table::from_text(&[vec!["이름".into(), "수량".into()], vec!["사과".into(), "12".into()]])));
        d.blocks.push(Block::PageBreak);
        d.para("두 번째 쪽");
        let p = std::env::temp_dir().join(format!("hwpx-rt-{}.hwpx", std::process::id()));
        write(&d, "t", &p).unwrap();
        let back = crate::native::read::hwpx::read(std::io::BufReader::new(std::fs::File::open(&p).unwrap()), &crate::Config::default()).unwrap();
        let Block::Para(h) = &back.blocks[0] else { panic!("{:?}", back.blocks) };
        assert_eq!((h.heading, h.text().as_str()), (Some(1), "한글 제목"));
        let Block::Para(p1) = &back.blocks[1] else { panic!() };
        assert!(p1.runs[0].bold && p1.runs[0].size_pt == Some(14.0));
        assert_eq!(p1.text(), "굵은 글씨 & <tag> 일반\n둘째 줄");
        assert_eq!(p1.align, Align::Center);
        let Block::Para(li) = &back.blocks[2] else { panic!() };
        assert_eq!(li.list.as_ref().map(|l| l.ordered), Some(false));
        let Block::Table(t) = &back.blocks[3] else { panic!() };
        assert_eq!(t.to_text(), vec![vec!["이름", "수량"], vec!["사과", "12"]]);
        assert!(back.blocks.iter().any(|b| matches!(b, Block::PageBreak)));
        assert!((back.page.unwrap().width_pt - 595.28).abs() < 0.1);
    }
}
