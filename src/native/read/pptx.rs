//! `.pptx` (and show/template/macro variants) → `Deck`: slide size, positioned text boxes, pictures, simple tables.
//! Placeholders without their own position take it from their slide layout, then the master.
use super::{attr, bad, mime_for, read_part, resolve_target};
use crate::native::model::*;
use crate::{Config, Result};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::{Read, Seek};

const EMU: f32 = 12_700.0;

type Rels = HashMap<String, String>;
type Zip<R> = zip::ZipArchive<R>;

fn parse_rels(xml: &[u8]) -> Rels {
    let mut m = HashMap::new();
    let mut r = Reader::from_reader(xml);
    let mut buf = vec![];
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.local_name().as_ref() == b"Relationship" => {
                if let (Some(id), Some(t)) = (attr(e.attributes(), "Id"), attr(e.attributes(), "Target")) { m.insert(id, t); }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    m
}

fn attr_exact(e: &quick_xml::events::BytesStart, key: &str) -> Option<String> {
    e.attributes().flatten().find(|a| a.key.as_ref() == key.as_bytes()).and_then(|a| a.unescape_value().ok().map(|v| v.into_owned()))
}

fn rels_of<R: Read + Seek>(zip: &mut Zip<R>, part: &str, max: u64) -> Rels {
    let (dir, file) = part.rsplit_once('/').unwrap_or(("", part));
    read_part(zip, &format!("{dir}/_rels/{file}.rels"), max).ok().flatten().map(|x| parse_rels(&x)).unwrap_or_default()
}

#[derive(Default, Clone)]
struct Theme { colors: HashMap<String, [u8; 3]> }

fn parse_hex(v: &str) -> Option<[u8; 3]> {
    if v.len() != 6 { return None; }
    let n = u32::from_str_radix(v, 16).ok()?;
    Some([(n >> 16) as u8, (n >> 8) as u8, n as u8])
}

fn parse_theme(xml: &[u8]) -> Theme {
    let mut t = Theme::default();
    let mut cur: Option<String> = None;
    let mut r = Reader::from_reader(xml);
    let mut buf = vec![];
    loop {
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                match name.as_str() {
                    "dk1" | "lt1" | "dk2" | "lt2" | "accent1" | "accent2" | "accent3" | "accent4" | "accent5" | "accent6" | "hlink" | "folHlink" => cur = Some(name),
                    "srgbClr" => if let (Some(c), Some(v)) = (&cur, attr(e.attributes(), "val").and_then(|v| parse_hex(&v))) { t.colors.insert(c.clone(), v); },
                    "sysClr" => if let (Some(c), Some(v)) = (&cur, attr(e.attributes(), "lastClr").and_then(|v| parse_hex(&v))) { t.colors.insert(c.clone(), v); },
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
        buf.clear();
    }
    t
}

impl Theme {
    fn scheme(&self, name: &str) -> Option<[u8; 3]> {
        let mapped = match name { "tx1" => "dk1", "bg1" => "lt1", "tx2" => "dk2", "bg2" => "lt2", n => n };
        self.colors.get(mapped).copied()
    }
}

fn rgb_to_hsl(c: [u8; 3]) -> (f32, f32, f32) {
    let (r, g, b) = (c[0] as f32 / 255.0, c[1] as f32 / 255.0, c[2] as f32 / 255.0);
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let l = (mx + mn) / 2.0;
    if mx == mn { return (0.0, 0.0, l); }
    let d = mx - mn;
    let s = if l > 0.5 { d / (2.0 - mx - mn) } else { d / (mx + mn) };
    let h = if mx == r { (g - b) / d + if g < b { 6.0 } else { 0.0 } } else if mx == g { (b - r) / d + 2.0 } else { (r - g) / d + 4.0 } / 6.0;
    (h, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> [u8; 3] {
    if s == 0.0 { let v = (l * 255.0).round() as u8; return [v, v, v]; }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        if t < 0.0 { t += 1.0 } if t > 1.0 { t -= 1.0 }
        let v = if t < 1.0 / 6.0 { p + (q - p) * 6.0 * t } else if t < 0.5 { q } else if t < 2.0 / 3.0 { p + (q - p) * (2.0 / 3.0 - t) * 6.0 } else { p };
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    };
    [f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0)]
}

fn luminance(c: [u8; 3]) -> f32 { (0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32) / 255.0 }

#[derive(Clone, Copy, Default)]
struct Xf { x: f32, y: f32, w: f32, h: f32 }

#[derive(Clone, Copy)]
struct Group { sx: f32, sy: f32, tx: f32, ty: f32 }
const IDENTITY: Group = Group { sx: 1.0, sy: 1.0, tx: 0.0, ty: 0.0 };

#[derive(Default)]
struct Parsed {
    ph: Option<(Option<String>, Option<String>)>,
    xf: Option<Xf>,
    fill: Option<[u8; 3]>,
    line: Option<[u8; 3]>,
    paras: Vec<Para>,
    middle: bool,
    font_scale: f32,
    pic: Option<Image>,
    table: Option<Table>,
}

#[derive(Clone, Copy, PartialEq)]
enum Target { None, ShapeFill, Line, Run, Bg }

/// Parses the shapes of one slide / layout / master part.
fn parse_shapes<R: Read + Seek>(xml: &[u8], theme: &Theme, rels: &Rels, part_dir: &str, zip: &mut Zip<R>, max: u64) -> (Vec<Parsed>, Option<[u8; 3]>) {
    let mut out: Vec<Parsed> = vec![];
    let mut bg: Option<[u8; 3]> = None;
    let mut groups: Vec<Group> = vec![IDENTITY];
    let mut cur: Option<Parsed> = None;
    let mut path: Vec<String> = vec![];
    let (mut xf, mut off, mut ext, mut choff, mut chext) = (Xf::default(), (0.0f32, 0.0f32), (0.0f32, 0.0f32), (0.0f32, 0.0f32), (0.0f32, 0.0f32));
    let (mut in_ln, mut in_rpr, mut in_sppr, mut in_bg) = (false, false, false, false);
    let mut para: Option<Para> = None;
    let mut run = Run::default();
    let mut run_size: Option<f32> = None;
    let mut para_level = 0u8;
    let (mut in_t, mut in_tbl) = (false, false);
    let mut table_rows: Vec<Vec<Cell>> = vec![];
    let mut row: Vec<Cell> = vec![];
    let mut cell_paras: Vec<Block> = vec![];
    let mut cell_span = 1usize;
    let mut table_w = 0.0f32;
    let mut pending: Option<[u8; 3]> = None;
    let mut target = Target::None;

    let commit = |c: [u8; 3], target: Target, cur: &mut Option<Parsed>, run: &mut Run, bg: &mut Option<[u8; 3]>| match target {
        Target::ShapeFill => if let Some(s) = cur { s.fill = Some(c); },
        Target::Line => if let Some(s) = cur { s.line = Some(c); },
        Target::Run => run.color = Some(c),
        Target::Bg => *bg = Some(c),
        Target::None => {}
    };

    let mut r = Reader::from_reader(xml);
    let mut buf = vec![];
    loop {
        let ev = match r.read_event_into(&mut buf) { Ok(e) => e, Err(_) => break };
        let is_start = matches!(ev, Event::Start(_));
        match &ev {
            Event::Start(e) | Event::Empty(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                match name.as_str() {
                    "sp" | "pic" | "graphicFrame" if is_start => cur = Some(Parsed { font_scale: 1.0, ..Default::default() }),
                    "grpSp" if is_start => groups.push(*groups.last().unwrap()),
                    "bg" if is_start => in_bg = true,
                    "ph" => if let Some(s) = &mut cur { s.ph = Some((attr(e.attributes(), "type"), attr(e.attributes(), "idx"))); },
                    "spPr" | "grpSpPr" | "bgPr" if is_start => in_sppr = true,
                    "xfrm" if is_start => { xf = Xf::default(); off = (0.0, 0.0); ext = (0.0, 0.0); choff = (0.0, 0.0); chext = (0.0, 0.0); }
                    "off" => off = (attr(e.attributes(), "x").and_then(|v| v.parse().ok()).unwrap_or(0.0), attr(e.attributes(), "y").and_then(|v| v.parse().ok()).unwrap_or(0.0)),
                    "ext" if !path.iter().any(|p| p == "extLst") => ext = (attr(e.attributes(), "cx").and_then(|v| v.parse().ok()).unwrap_or(0.0), attr(e.attributes(), "cy").and_then(|v| v.parse().ok()).unwrap_or(0.0)),
                    "chOff" => choff = (attr(e.attributes(), "x").and_then(|v| v.parse().ok()).unwrap_or(0.0), attr(e.attributes(), "y").and_then(|v| v.parse().ok()).unwrap_or(0.0)),
                    "chExt" => chext = (attr(e.attributes(), "cx").and_then(|v| v.parse().ok()).unwrap_or(0.0), attr(e.attributes(), "cy").and_then(|v| v.parse().ok()).unwrap_or(0.0)),
                    "ln" if is_start => { in_ln = true; }
                    "solidFill" => target = if in_rpr { Target::Run } else if in_ln { Target::Line } else if in_bg { Target::Bg } else if in_sppr { Target::ShapeFill } else { Target::None },
                    "noFill" if in_sppr && !in_ln && !in_rpr => if let Some(s) = &mut cur { s.fill = None; },
                    "srgbClr" | "schemeClr" | "sysClr" => {
                        let c = match name.as_str() {
                            "srgbClr" => attr(e.attributes(), "val").and_then(|v| parse_hex(&v)),
                            "sysClr" => attr(e.attributes(), "lastClr").and_then(|v| parse_hex(&v)),
                            _ => attr(e.attributes(), "val").and_then(|v| theme.scheme(&v)),
                        };
                        if is_start { pending = c; } else if let Some(c) = c { commit(c, target, &mut cur, &mut run, &mut bg); }
                    }
                    "lumMod" | "lumOff" => if let Some(c) = pending {
                        let v = attr(e.attributes(), "val").and_then(|v| v.parse::<f32>().ok()).unwrap_or(100_000.0) / 100_000.0;
                        let (h, s, l) = rgb_to_hsl(c);
                        pending = Some(hsl_to_rgb(h, s, if name == "lumMod" { l * v } else { l + v }.clamp(0.0, 1.0)));
                    },
                    "bodyPr" => if let Some(s) = &mut cur { s.middle = attr(e.attributes(), "anchor").as_deref() == Some("ctr"); },
                    "normAutofit" => if let Some(s) = &mut cur { s.font_scale = attr(e.attributes(), "fontScale").and_then(|v| v.parse::<f32>().ok()).map(|v| v / 100_000.0).unwrap_or(1.0); },
                    "p" if is_start => para = Some(Para::default()),
                    "pPr" => if let Some(p) = &mut para {
                        p.align = match attr(e.attributes(), "algn").as_deref() { Some("ctr") => Align::Center, Some("r") => Align::Right, Some("just") => Align::Justify, _ => Align::Left };
                        para_level = attr(e.attributes(), "lvl").and_then(|v| v.parse::<u8>().ok()).unwrap_or(0).min(6);
                    },
                    "buChar" => if let Some(p) = &mut para { p.list = Some(ListInfo { ordered: false, level: para_level }); },
                    "buAutoNum" => if let Some(p) = &mut para { p.list = Some(ListInfo { ordered: true, level: para_level }); },
                    "r" | "fld" if is_start => { run = Run::default(); run_size = None; }
                    "rPr" => {
                        in_rpr = is_start;
                        run.bold = attr(e.attributes(), "b").as_deref() == Some("1");
                        run.italic = attr(e.attributes(), "i").as_deref() == Some("1");
                        run.underline = attr(e.attributes(), "u").is_some_and(|u| u != "none");
                        run_size = attr(e.attributes(), "sz").and_then(|v| v.parse::<f32>().ok()).map(|v| v / 100.0);
                    }
                    "t" if is_start => in_t = true,
                    "br" => if let Some(p) = &mut para { p.runs.push(Run::plain("\n")); },
                    "blip" => if let (Some(rid), Some(s)) = (attr(e.attributes(), "embed"), &mut cur) {
                        if let Some(t) = rels.get(&rid) {
                            let path = resolve_target(part_dir, t);
                            if let Ok(Some(data)) = read_part(zip, &path, 64 * 1024 * 1024) { s.pic = Some(Image { data, mime: mime_for(&path).to_string(), width_pt: None, height_pt: None }); }
                        }
                    }
                    "tbl" if is_start => { in_tbl = true; table_rows.clear(); }
                    "gridCol" => table_w += attr(e.attributes(), "w").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0) / EMU,
                    "tr" if is_start => row = vec![],
                    "tc" if is_start => { cell_paras = vec![]; cell_span = attr(e.attributes(), "gridSpan").and_then(|v| v.parse().ok()).unwrap_or(1usize).clamp(1, 256); }
                    _ => {}
                }
                if is_start { path.push(name); }
            }
            Event::End(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                path.pop();
                match name.as_str() {
                    "srgbClr" | "schemeClr" | "sysClr" => if let Some(c) = pending.take() { commit(c, target, &mut cur, &mut run, &mut bg); },
                    "solidFill" => target = Target::None,
                    "bg" => in_bg = false,
                    "spPr" | "grpSpPr" | "bgPr" => in_sppr = false,
                    "ln" => in_ln = false,
                    "rPr" => in_rpr = false,
                    "t" => in_t = false,
                    "xfrm" => {
                        let parent = *groups.last().unwrap_or(&IDENTITY);
                        let in_group_props = path.last().is_some_and(|p| p == "grpSpPr");
                        if in_group_props {
                            let (sx, sy) = (if chext.0 > 0.0 { ext.0 / chext.0 } else { 1.0 }, if chext.1 > 0.0 { ext.1 / chext.1 } else { 1.0 });
                            let g = Group { sx: parent.sx * sx, sy: parent.sy * sy, tx: parent.tx + (off.0 - choff.0 * sx) * parent.sx, ty: parent.ty + (off.1 - choff.1 * sy) * parent.sy };
                            // The raw transform from parent space: x' = parent.tx + parent.sx * (off + (x - choff) * sx)
                            if let Some(last) = groups.last_mut() { *last = g; }
                        } else {
                            xf = Xf { x: parent.tx + off.0 * parent.sx, y: parent.ty + off.1 * parent.sy, w: ext.0 * parent.sx, h: ext.1 * parent.sy };
                            if let Some(s) = &mut cur { s.xf = Some(Xf { x: xf.x / EMU, y: xf.y / EMU, w: xf.w / EMU, h: xf.h / EMU }); }
                        }
                    }
                    "r" | "fld" => if let Some(p) = &mut para {
                        if !run.text.is_empty() { run.size_pt = run_size; p.runs.push(std::mem::take(&mut run)); }
                    },
                    "p" => if let Some(p) = para.take() {
                        if in_tbl { cell_paras.push(Block::Para(p)); } else if let Some(s) = &mut cur { s.paras.push(p); }
                    },
                    "tc" => row.push(Cell { blocks: std::mem::take(&mut cell_paras), col_span: cell_span }),
                    "tr" => table_rows.push(std::mem::take(&mut row)),
                    "tbl" => {
                        in_tbl = false;
                        if let Some(s) = &mut cur { s.table = Some(Table { rows: std::mem::take(&mut table_rows) }); }
                    }
                    "grpSp" => { groups.pop(); }
                    "sp" | "pic" | "graphicFrame" => if let Some(mut s) = cur.take() {
                        if name == "graphicFrame" && table_w > 0.0 { if let Some(x) = &mut s.xf { if x.w <= 0.0 { x.w = table_w; } } }
                        table_w = 0.0;
                        out.push(s);
                    },
                    _ => {}
                }
            }
            Event::Text(t) if in_t => { if let Ok(s) = t.decode() { run.text.push_str(&s); } }
            Event::GeneralRef(rf) if in_t => {
                let ch = rf.resolve_char_ref().ok().flatten().or_else(|| match rf.decode().ok()?.as_ref() { "amp" => Some('&'), "lt" => Some('<'), "gt" => Some('>'), "quot" => Some('"'), "apos" => Some('\''), _ => None });
                if let Some(c) = ch { run.text.push(c); }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    (out, bg)
}

pub fn read<R: Read + Seek>(reader: R, cfg: &Config) -> Result<Deck> {
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| bad(format!("not a readable presentation: {e}")))?;
    let max = cfg.max_document_xml_bytes;
    let pres = read_part(&mut zip, "ppt/presentation.xml", max)?.ok_or_else(|| bad("the file has no ppt/presentation.xml"))?;
    let pres_rels = rels_of(&mut zip, "ppt/presentation.xml", max);
    let (mut w, mut h) = (720.0f32, 405.0f32);
    let mut slide_ids: Vec<String> = vec![];
    {
        let mut r = Reader::from_reader(pres.as_slice());
        let mut buf = vec![];
        loop {
            match r.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) => match e.local_name().as_ref() {
                    b"sldSz" => {
                        if let Some(cx) = attr(e.attributes(), "cx").and_then(|v| v.parse::<f32>().ok()) { w = cx / EMU; }
                        if let Some(cy) = attr(e.attributes(), "cy").and_then(|v| v.parse::<f32>().ok()) { h = cy / EMU; }
                    }
                    b"sldId" => if let Some(rid) = attr_exact(&e, "r:id") { slide_ids.push(rid); },
                    _ => {}
                },
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
            buf.clear();
        }
    }
    let theme = read_part(&mut zip, "ppt/theme/theme1.xml", max)?.map(|x| parse_theme(&x)).unwrap_or_default();
    let mut deck = Deck { width_pt: w, height_pt: h, slides: vec![] };
    let mut budget: usize = cfg.max_raster_pages.max(500);
    for rid in slide_ids {
        let Some(target) = pres_rels.get(&rid) else { continue };
        let part = resolve_target("ppt", target);
        let Some(xml) = read_part(&mut zip, &part, max)? else { continue };
        budget = budget.checked_sub(1).ok_or_else(|| crate::Error::new(crate::ErrorCode::ETooLarge, "The presentation has too many slides."))?;
        let rels = rels_of(&mut zip, &part, max);
        let dir = part.rsplit_once('/').map(|x| x.0).unwrap_or("ppt/slides").to_string();

        // Layout and master supply positions for placeholders and a background.
        let mut inherited: Vec<Parsed> = vec![];
        let mut bg_chain: Vec<Option<[u8; 3]>> = vec![];
        let layout_part = rels.values().find(|t| t.contains("slideLayouts/")).map(|t| resolve_target(&dir, t));
        let mut layout_master: Option<String> = None;
        if let Some(lp) = &layout_part {
            if let Some(lx) = read_part(&mut zip, lp, max)? {
                let lrels = rels_of(&mut zip, lp, max);
                let ldir = lp.rsplit_once('/').map(|x| x.0).unwrap_or("").to_string();
                let (shapes, bg) = parse_shapes(&lx, &theme, &lrels, &ldir, &mut zip, max);
                inherited.extend(shapes); bg_chain.push(bg);
                layout_master = lrels.values().find(|t| t.contains("slideMasters/")).map(|t| resolve_target(&ldir, t));
            }
        }
        if let Some(mp) = &layout_master {
            if let Some(mx) = read_part(&mut zip, mp, max)? {
                let mrels = rels_of(&mut zip, mp, max);
                let mdir = mp.rsplit_once('/').map(|x| x.0).unwrap_or("").to_string();
                let (shapes, bg) = parse_shapes(&mx, &theme, &mrels, &mdir, &mut zip, max);
                inherited.extend(shapes); bg_chain.push(bg);
            }
        }
        let (shapes, own_bg) = parse_shapes(&xml, &theme, &rels, &dir, &mut zip, max);
        let background = own_bg.or_else(|| bg_chain.iter().flatten().next().copied());
        let default_text = if background.is_some_and(|b| luminance(b) < 0.4) { [255, 255, 255] } else { [0, 0, 0] };

        let find_xf = |ph: &(Option<String>, Option<String>)| -> Option<Xf> {
            let norm = |t: &Option<String>| t.as_deref().map(|t| if t == "ctrTitle" { "title" } else { t }.to_string());
            let by_idx = ph.1.as_ref().and_then(|i| inherited.iter().find(|s| s.ph.as_ref().is_some_and(|p| p.1.as_ref() == Some(i)) && s.xf.is_some()));
            let by_type = || inherited.iter().find(|s| s.ph.as_ref().is_some_and(|p| norm(&p.0) == norm(&ph.0) && (ph.0.is_some() || p.0.is_none())) && s.xf.is_some());
            by_idx.or_else(by_type).and_then(|s| s.xf)
        };

        let mut slide = Slide { background, shapes: vec![] };
        for s in shapes {
            let kind = s.ph.as_ref().and_then(|p| p.0.clone());
            if matches!(kind.as_deref(), Some("dt") | Some("ftr") | Some("sldNum")) { continue; }
            let xf = s.xf.or_else(|| s.ph.as_ref().and_then(&find_xf));
            if let (Some(img), Some(xf)) = (s.pic, xf) {
                slide.shapes.push(Shape::Picture { x: xf.x, y: xf.y, w: xf.w, h: xf.h, image: img });
                continue;
            }
            if let (Some(t), Some(xf)) = (s.table, xf) {
                slide.shapes.push(Shape::Table { x: xf.x, y: xf.y, w: xf.w, table: t });
                continue;
            }
            let has_text = s.paras.iter().any(|p| !p.is_blank());
            if !has_text && s.fill.is_none() && s.line.is_none() { continue; }
            let Some(xf) = xf else { continue };
            let is_title = matches!(kind.as_deref(), Some("title") | Some("ctrTitle"));
            let mut paras = s.paras;
            for p in &mut paras {
                for r in &mut p.runs {
                    let base = r.size_pt.unwrap_or(if is_title { 32.0 } else { 18.0 });
                    r.size_pt = Some((base * s.font_scale).max(6.0));
                    if r.color.is_none() { r.color = Some(default_text); }
                }
            }
            slide.shapes.push(Shape::Text { x: xf.x, y: xf.y, w: xf.w, h: xf.h, fill: s.fill, line: s.line, paras, middle: s.middle || is_title });
        }
        deck.slides.push(slide);
    }
    if deck.slides.is_empty() { return Err(bad("The presentation has no slides.")); }
    Ok(deck)
}
