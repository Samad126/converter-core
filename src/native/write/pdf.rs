//! Layout → PDF through `krilla` (font subsetting, images, link annotations).
use crate::native::{fonts::FontSet, layout::*, model::*};
use crate::{Error, ErrorCode, Result};
use krilla::color::rgb;
use krilla::geom::{PathBuilder, Point, Rect, Size, Transform};
use krilla::num::NormalizedF32;
use krilla::page::PageSettings;
use krilla::paint::{Fill, FillRule, Stroke};
use krilla::text::TextDirection;
use krilla::Document;

fn internal(e: impl std::fmt::Debug) -> Error { Error::new(ErrorCode::EInternal, format!("cannot write PDF: {e:?}")) }

fn load_image(img: &crate::native::model::Image) -> Option<krilla::image::Image> {
    let data: krilla::Data = img.data.clone().into();
    let direct = match img.mime.as_str() {
        "image/png" => krilla::image::Image::from_png(data, true).ok(),
        "image/jpeg" => krilla::image::Image::from_jpeg(data, true).ok(),
        _ => None,
    };
    direct.or_else(|| {
        let rgba = image::load_from_memory(&img.data).ok()?.to_rgba8();
        let (w, h) = rgba.dimensions();
        Some(krilla::image::Image::from_rgba8(rgba.into_raw(), w, h))
    })
}

fn fill(c: [u8; 3]) -> Fill { Fill { paint: rgb::Color::new(c[0], c[1], c[2]).into(), opacity: NormalizedF32::ONE, rule: FillRule::default() } }

pub fn render(layout: &Layout, fonts: &FontSet, title: &str) -> Result<Vec<u8>> {
    let mut doc = Document::new();
    doc.set_metadata(krilla::metadata::Metadata::new().title(title.to_string()));
    let mut images: Vec<Option<Option<krilla::image::Image>>> = vec![None; layout.images.len()];
    let pages: Vec<&Page> = if layout.pages.is_empty() { vec![] } else { layout.pages.iter().collect() };
    for p in pages {
        let mut page = doc.start_page_with(PageSettings::from_wh(p.width, p.height).ok_or_else(|| internal("bad page size"))?);
        let mut links: Vec<(Rect, String)> = vec![];
        let mut surface = page.surface();
        for cmd in &p.cmds {
            match cmd {
                Cmd::Text(piece, y) => {
                    if piece.text.trim().is_empty() && !piece.underline { continue; }
                    surface.set_fill(Some(fill(piece.color)));
                    surface.set_stroke(None);
                    surface.draw_text(Point::from_xy(piece.x, *y), fonts.faces[piece.face].font.clone(), piece.size, &piece.text, false, TextDirection::Auto);
                    if piece.underline {
                        if let Some(r) = Rect::from_xywh(piece.x, *y + piece.size * 0.12, piece.width, (piece.size * 0.06).max(0.4)) {
                            let mut pb = PathBuilder::new(); pb.push_rect(r);
                            if let Some(path) = pb.finish() { surface.draw_path(&path); }
                        }
                    }
                    if let (Some(link), Some(r)) = (&piece.link, Rect::from_xywh(piece.x, *y - piece.size, piece.width, piece.size * 1.2)) { links.push((r, link.to_string())); }
                }
                Cmd::Rect { x, y, w, h, fill: f, stroke } => {
                    let Some(r) = Rect::from_xywh(*x, *y, *w, *h) else { continue };
                    let mut pb = PathBuilder::new(); pb.push_rect(r);
                    let Some(path) = pb.finish() else { continue };
                    surface.set_fill(f.map(fill));
                    surface.set_stroke(stroke.map(|(c, width)| Stroke { paint: rgb::Color::new(c[0], c[1], c[2]).into(), width, ..Default::default() }));
                    surface.draw_path(&path);
                }
                Cmd::Image { x, y, w, h, img } => {
                    let slot = &mut images[*img];
                    let loaded = slot.get_or_insert_with(|| load_image(&layout.images[*img]));
                    if let (Some(image), Some(size)) = (loaded.clone(), Size::from_wh(*w, *h)) {
                        surface.push_transform(&Transform::from_translate(*x, *y));
                        surface.draw_image(image, size);
                        surface.pop();
                    }
                }
            }
        }
        surface.finish();
        for (rect, uri) in links {
            use krilla::{action::{Action, LinkAction}, annotation::{Annotation, LinkAnnotation, Target}};
            page.add_annotation(Annotation::new_link(LinkAnnotation::new(rect, Target::Action(Action::Link(LinkAction::new(uri)))), None).into());
        }
        page.finish();
    }
    doc.finish().map_err(internal)
}

/// A document laid out on A4 (or the page size the file itself declares).
pub fn doc_to_pdf(doc: &Doc, title: &str) -> Result<Vec<u8>> {
    let mut fonts = FontSet::new();
    let setup = doc.page.map(|p| PageSetup { width: p.width_pt, height: p.height_pt, margin: p.margins }).unwrap_or(PageSetup::A4);
    let mut e = Engine::new(&mut fonts, setup, doc.base_size_pt.unwrap_or(11.0));
    let flows = e.flow_doc(doc);
    e.place(&flows);
    let layout = e.finish();
    render(&layout, &fonts, title)
}

/// One section per sheet; a wide sheet goes landscape and then shrinks its text to fit the page.
pub fn book_to_pdf(book: &Workbook, title: &str) -> Result<Vec<u8>> {
    let mut fonts = FontSet::new();
    let narrow = PageSetup { margin: [40.0, 36.0, 40.0, 36.0], ..PageSetup::A4 };
    let mut layout_pages: Vec<Page> = vec![];
    let mut images = vec![];
    for sheet in &book.sheets {
        let width = sheet.rows.iter().map(Vec::len).max().unwrap_or(0);
        let rows: Vec<Vec<String>> = sheet.rows.iter().map(|r| { let mut r = r.clone(); r.resize(width, String::new()); r }).collect();
        if rows.is_empty() { continue; }
        let mut table = Table::from_text(&rows);
        for (r, row) in table.rows.iter_mut().enumerate() {
            for (c, cell) in row.iter_mut().enumerate() {
                if super::sheet::as_number(&rows[r][c]).is_some() { if let Some(Block::Para(p)) = cell.blocks.first_mut() { p.align = Align::Right; } }
            }
        }
        let mut size = 9.0f32;
        let mut setup = narrow;
        {
            let mut probe = Engine::new(&mut fonts, setup, size);
            let natural = probe.natural_table_width(&table);
            if natural > setup.inner_width() { setup = narrow.landscape(); }
            if natural > setup.inner_width() { size = (size * setup.inner_width() / natural).clamp(5.0, 9.0); }
        }
        let mut e = Engine::new(&mut fonts, setup, size);
        e.stretch_tables = false;
        e.begin_section(setup, size);
        let mut d = Doc::default();
        if book.sheets.len() > 1 { d.heading(2, sheet.name.clone()); }
        d.blocks.push(Block::Table(table));
        let flows = e.flow_doc(&d);
        e.place(&flows);
        let l = e.finish();
        let offset = images.len();
        for mut p in l.pages {
            for c in &mut p.cmds { if let Cmd::Image { img, .. } = c { *img += offset; } }
            layout_pages.push(p);
        }
        images.extend(l.images);
    }
    if layout_pages.is_empty() {
        layout_pages.push(Page { width: narrow.width, height: narrow.height, cmds: vec![] });
    }
    render(&Layout { pages: layout_pages, images }, &fonts, title)
}

