//! `.mht` / `.mhtml` web archives (MIME multipart/related): the HTML part, with its images resolved from the other parts.
use super::{bad, html};
use crate::native::model::*;
use crate::Result;
use mail_parser::{MessageParser, MimeHeaders, PartType};

pub fn read(bytes: &[u8]) -> Result<Doc> {
    let msg = MessageParser::default().parse(bytes).ok_or_else(|| bad("not a readable web archive (MHT)"))?;
    let html_text = msg.parts.iter().find_map(|p| match &p.body { PartType::Html(h) => Some(h.to_string()), _ => None })
        .ok_or_else(|| bad("The web archive has no HTML part."))?;
    // Related parts, by Content-ID (cid:...) and Content-Location.
    let mut images: Vec<(Option<String>, Option<String>, Image)> = vec![];
    for p in &msg.parts {
        let data = match &p.body { PartType::Binary(b) | PartType::InlineBinary(b) => b.to_vec(), _ => continue };
        let mime = p.content_type().map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or(""))).unwrap_or_default();
        if !mime.starts_with("image/") { continue; }
        images.push((p.content_id().map(|c| c.trim_matches(['<', '>']).to_string()), p.content_location().map(String::from), Image { data, mime, width_pt: None, height_pt: None }));
    }
    let resolve = |src: &str| -> Option<Image> {
        let strip = |s: &str| s.split(['?', '#']).next().unwrap_or(s).to_string();
        if let Some(cid) = src.strip_prefix("cid:") { return images.iter().find(|i| i.0.as_deref() == Some(cid)).map(|i| i.2.clone()); }
        let want = strip(src);
        images.iter().find(|i| i.1.as_deref().is_some_and(|l| strip(l) == want || strip(l).ends_with(&want) || want.ends_with(&strip(l)))).map(|i| i.2.clone())
    };
    Ok(html::parse(&html_text, &resolve))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn html_part_with_a_related_image() {
        let mht = "From: <saved>\r\nSubject: Page\r\nMIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=\"b\"; type=\"text/html\"\r\n\r\n--b\r\nContent-Type: text/html; charset=utf-8\r\nContent-Location: http://x.test/page.html\r\n\r\n<html><body><h1>안녕</h1><p>Hello</p><img src=\"pic.png\"></body></html>\r\n--b\r\nContent-Type: image/png\r\nContent-Transfer-Encoding: base64\r\nContent-Location: http://x.test/pic.png\r\n\r\niVBORw0KGgo=\r\n--b--\r\n";
        let d = read(mht.as_bytes()).unwrap();
        let Block::Para(h) = &d.blocks[0] else { panic!("{:?}", d.blocks) };
        assert_eq!((h.heading, h.text().as_str()), (Some(1), "안녕"));
        assert!(d.blocks.iter().any(|b| matches!(b, Block::Image(i) if i.mime == "image/png")), "{:?}", d.blocks);
    }
}
