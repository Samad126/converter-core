//! EPUB 3: one XHTML chapter per top-level heading, images packaged as files.
use super::html::{body_html, title_of};
use crate::native::{model::*, util::{esc_xml, ext_for_mime}};
use crate::{Error, ErrorCode, Result};
use std::io::Write;

fn zerr(e: impl std::fmt::Display) -> Error { Error::new(ErrorCode::EInternal, e.to_string()) }

fn split_chapters(doc: &Doc) -> Vec<(String, Vec<Block>)> {
    let mut chapters: Vec<(String, Vec<Block>)> = vec![];
    for b in &doc.blocks {
        if let Block::Para(p) = b { if p.heading == Some(1) && !p.is_blank() { chapters.push((p.text(), vec![])); } }
        if chapters.is_empty() { chapters.push((String::new(), vec![])); }
        chapters.last_mut().unwrap().1.push(b.clone());
    }
    if chapters.is_empty() { chapters.push((String::new(), vec![])); }
    chapters
}

pub fn to_epub(doc: &Doc, fallback_title: &str, out: &std::path::Path) -> Result<()> {
    let title = title_of(doc, fallback_title);
    let chapters = split_chapters(doc);
    let mut images: Vec<(String, String, Vec<u8>)> = vec![]; // (file name, mime, data)

    let mut files: Vec<(String, String)> = vec![]; // (file name, xhtml)
    for (i, (name, blocks)) in chapters.iter().enumerate() {
        let body = body_html(blocks, &mut |img: &Image| {
            let n = format!("img{}.{}", images.len() + 1, ext_for_mime(&img.mime));
            images.push((n.clone(), img.mime.clone(), img.data.clone()));
            format!("images/{n}")
        });
        let heading = if name.is_empty() { title.clone() } else { name.clone() };
        files.push((format!("chapter{}.xhtml", i + 1), format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE html>\n<html xmlns=\"http://www.w3.org/1999/xhtml\"><head><title>{}</title><meta charset=\"utf-8\"/></head><body>\n{body}</body></html>\n",
            esc_xml(&heading))));
    }

    let mut manifest = String::from("<item id=\"nav\" href=\"nav.xhtml\" media-type=\"application/xhtml+xml\" properties=\"nav\"/>\n");
    let mut spine = String::new();
    for (i, (name, _)) in files.iter().enumerate() {
        manifest.push_str(&format!("<item id=\"c{i}\" href=\"{name}\" media-type=\"application/xhtml+xml\"/>\n"));
        spine.push_str(&format!("<itemref idref=\"c{i}\"/>\n"));
    }
    for (i, (name, mime, _)) in images.iter().enumerate() {
        manifest.push_str(&format!("<item id=\"i{i}\" href=\"images/{name}\" media-type=\"{mime}\"/>\n"));
    }
    let opf = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<package xmlns=\"http://www.idpf.org/2007/opf\" version=\"3.0\" unique-identifier=\"id\">\n<metadata xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n<dc:identifier id=\"id\">urn:converter:{:x}</dc:identifier>\n<dc:title>{}</dc:title>\n<dc:language>en</dc:language>\n<meta property=\"dcterms:modified\">2000-01-01T00:00:00Z</meta>\n</metadata>\n<manifest>\n{manifest}</manifest>\n<spine>\n{spine}</spine>\n</package>\n",
        title.bytes().fold(0u64, |h, b| h.wrapping_mul(131).wrapping_add(b as u64)), esc_xml(&title));
    let toc: String = chapters.iter().zip(&files).map(|((name, _), (file, _))|
        format!("<li><a href=\"{file}\">{}</a></li>\n", esc_xml(if name.is_empty() { &title } else { name }))).collect();
    let nav = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<!DOCTYPE html>\n<html xmlns=\"http://www.w3.org/1999/xhtml\" xmlns:epub=\"http://www.idpf.org/2007/ops\"><head><title>{}</title></head><body><nav epub:type=\"toc\"><ol>\n{toc}</ol></nav></body></html>\n", esc_xml(&title));

    let file = std::fs::File::create(out)?;
    let mut z = zip::ZipWriter::new(file);
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = zip::write::SimpleFileOptions::default();
    z.start_file("mimetype", stored).map_err(zerr)?;
    z.write_all(b"application/epub+zip")?;
    z.start_file("META-INF/container.xml", deflated).map_err(zerr)?;
    z.write_all(b"<?xml version=\"1.0\"?>\n<container version=\"1.0\" xmlns=\"urn:oasis:names:tc:opendocument:xmlns:container\"><rootfiles><rootfile full-path=\"OEBPS/content.opf\" media-type=\"application/oebps-package+xml\"/></rootfiles></container>\n")?;
    z.start_file("OEBPS/content.opf", deflated).map_err(zerr)?;
    z.write_all(opf.as_bytes())?;
    z.start_file("OEBPS/nav.xhtml", deflated).map_err(zerr)?;
    z.write_all(nav.as_bytes())?;
    for (name, xhtml) in &files {
        z.start_file(format!("OEBPS/{name}"), deflated).map_err(zerr)?;
        z.write_all(xhtml.as_bytes())?;
    }
    for (name, _, data) in &images {
        z.start_file(format!("OEBPS/images/{name}"), deflated).map_err(zerr)?;
        z.write_all(data)?;
    }
    z.finish().map_err(zerr)?;
    Ok(())
}
