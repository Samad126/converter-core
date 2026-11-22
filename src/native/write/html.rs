//! HTML (one self-contained file) and the shared body renderer the EPUB writer reuses.
use crate::native::{model::*, util::{base64, esc_xml}};

fn run_html(r: &Run) -> String {
    let mut s = esc_xml(&r.text).replace('\n', "<br/>").replace('\t', "&#9;");
    if s.trim().is_empty() { return s; }
    let mut style = String::new();
    if let Some(c) = r.color { if c != [0, 0, 0] { style.push_str(&format!("color:#{:02x}{:02x}{:02x};", c[0], c[1], c[2])); } }
    if !style.is_empty() { s = format!("<span style=\"{style}\">{s}</span>"); }
    if r.mono { s = format!("<code>{s}</code>"); }
    if r.underline { s = format!("<u>{s}</u>"); }
    if r.italic { s = format!("<em>{s}</em>"); }
    if r.bold { s = format!("<strong>{s}</strong>"); }
    if let Some(u) = &r.link { s = format!("<a href=\"{}\">{s}</a>", esc_xml(u)); }
    s
}

fn align_attr(a: Align) -> &'static str {
    match a { Align::Left => "", Align::Center => " style=\"text-align:center\"", Align::Right => " style=\"text-align:right\"", Align::Justify => " style=\"text-align:justify\"" }
}

/// `img_src` turns an image into its `src` value (a data URI, or a file name the caller will package).
pub fn body_html(blocks: &[Block], img_src: &mut dyn FnMut(&Image) -> String) -> String {
    let mut out = String::new();
    let mut open: Vec<bool> = vec![]; // stack of open lists: true = ordered
    let close_to = |out: &mut String, open: &mut Vec<bool>, depth: usize| {
        while open.len() > depth { out.push_str(if open.pop().unwrap() { "</ol>\n" } else { "</ul>\n" }); }
    };
    for b in blocks {
        match b {
            Block::Para(p) if p.list.is_some() && p.heading.is_none() => {
                let l = p.list.as_ref().unwrap();
                let depth = l.level as usize + 1;
                close_to(&mut out, &mut open, depth);
                if open.len() == depth && open[depth - 1] != l.ordered { close_to(&mut out, &mut open, depth - 1); }
                while open.len() < depth { open.push(l.ordered); out.push_str(if l.ordered { "<ol>\n" } else { "<ul>\n" }); }
                out.push_str(&format!("<li>{}</li>\n", p.runs.iter().map(run_html).collect::<String>()));
            }
            Block::Para(p) => {
                close_to(&mut out, &mut open, 0);
                let inner: String = p.runs.iter().map(run_html).collect();
                match p.heading {
                    Some(h) => out.push_str(&format!("<h{h}{}>{inner}</h{h}>\n", align_attr(p.align))),
                    None if p.is_blank() => {}
                    None => out.push_str(&format!("<p{}>{inner}</p>\n", align_attr(p.align))),
                }
            }
            Block::Table(t) => {
                close_to(&mut out, &mut open, 0);
                out.push_str("<table>\n");
                for row in &t.rows {
                    out.push_str("<tr>");
                    for c in row {
                        let span = if c.col_span > 1 { format!(" colspan=\"{}\"", c.col_span) } else { String::new() };
                        let inner = match c.blocks.as_slice() {
                            [Block::Para(p)] if p.list.is_none() && p.heading.is_none() => p.runs.iter().map(run_html).collect(),
                            blocks => body_html(blocks, img_src).trim().replace('\n', ""),
                        };
                        out.push_str(&format!("<td{span}>{inner}</td>"));
                    }
                    out.push_str("</tr>\n");
                }
                out.push_str("</table>\n");
            }
            Block::Image(img) => {
                close_to(&mut out, &mut open, 0);
                let size = match (img.width_pt, img.height_pt) { (Some(w), Some(h)) => format!(" width=\"{}\" height=\"{}\"", (w * 96.0 / 72.0).round(), (h * 96.0 / 72.0).round()), _ => String::new() };
                out.push_str(&format!("<p><img src=\"{}\" alt=\"\"{size}/></p>\n", img_src(img)));
            }
            Block::PageBreak => { close_to(&mut out, &mut open, 0); out.push_str("<hr/>\n"); }
        }
    }
    close_to(&mut out, &mut open, 0);
    out
}

pub const CSS: &str = "body{font-family:system-ui,-apple-system,'Segoe UI',Roboto,sans-serif;line-height:1.55;max-width:50rem;margin:2rem auto;padding:0 1rem;color:#1b1b1f}\
table{border-collapse:collapse;margin:1rem 0}td{border:1px solid #bbb;padding:.3rem .55rem;vertical-align:top}img{max-width:100%;height:auto}code{font-family:ui-monospace,Menlo,Consolas,monospace}";

pub fn title_of(doc: &Doc, fallback: &str) -> String { doc.first_heading().unwrap_or_else(|| fallback.to_string()) }

pub fn to_html(doc: &Doc, fallback_title: &str) -> String {
    let body = body_html(&doc.blocks, &mut |img: &Image| format!("data:{};base64,{}", img.mime, base64(&img.data)));
    format!("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\"/>\n<title>{}</title>\n<style>{CSS}</style>\n</head>\n<body>\n{body}</body>\n</html>\n",
        esc_xml(&title_of(doc, fallback_title)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn lists_nest_and_close() {
        let mut d = Doc::default();
        for (txt, lvl) in [("a", 0u8), ("b", 1), ("c", 0)] {
            d.blocks.push(Block::Para(Para { runs: vec![Run::plain(txt)], list: Some(ListInfo { ordered: false, level: lvl }), ..Default::default() }));
        }
        d.para("end");
        let h = to_html(&d, "t");
        assert!(h.contains("<ul>\n<li>a</li>\n<ul>\n<li>b</li>\n</ul>\n<li>c</li>\n</ul>\n<p>end</p>"), "{h}");
    }
    #[test] fn escapes_and_formats() {
        let mut d = Doc::default();
        d.blocks.push(Block::Para(Para { runs: vec![Run { text: "a<b".into(), bold: true, ..Default::default() }], ..Default::default() }));
        assert!(to_html(&d, "t").contains("<p><strong>a&lt;b</strong></p>"));
    }
}
