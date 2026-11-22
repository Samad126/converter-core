//! HTML (and the HTML part of `.mht`/`.mhtml`) → `Doc`: headings, paragraphs, inline styles, links, lists, tables,
//! preformatted text and images (data URIs, or whatever the caller's resolver finds for the other `src` values).
use crate::native::model::*;
use crate::native::util::base64_decode;

fn named_entity(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&', "lt" => '<', "gt" => '>', "quot" => '"', "apos" => '\'', "nbsp" => '\u{a0}', "copy" => '©', "reg" => '®',
        "ndash" => '–', "mdash" => '—', "hellip" => '…', "lsquo" => '‘', "rsquo" => '’', "ldquo" => '“', "rdquo" => '”',
        "bull" => '•', "middot" => '·', "trade" => '™', "euro" => '€', "pound" => '£', "yen" => '¥', "cent" => '¢', "deg" => '°',
        "times" => '×', "divide" => '÷', "laquo" => '«', "raquo" => '»', "para" => '¶', "sect" => '§', "larr" => '←', "rarr" => '→',
        "uarr" => '↑', "darr" => '↓', "hearts" => '♥', "check" => '✓', _ => return None,
    })
}

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') { return s.to_string(); }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let decoded = tail.find(';').filter(|e| *e > 1 && *e <= 10).and_then(|e| {
            let body = &tail[1..e];
            let c = if let Some(h) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) { u32::from_str_radix(h, 16).ok().and_then(char::from_u32) }
                else if let Some(d) = body.strip_prefix('#') { d.parse::<u32>().ok().and_then(char::from_u32) }
                else { named_entity(body) };
            c.map(|c| (c, e + 1))
        });
        match decoded { Some((c, n)) => { out.push(c); rest = &tail[n..]; } None => { out.push('&'); rest = &tail[1..]; } }
    }
    out.push_str(rest);
    out
}

#[derive(Debug)]
enum Tok { Text(String), Start(String, Vec<(String, String)>, bool), End(String) }

fn tokenize(html: &str) -> Vec<Tok> {
    let b = html.as_bytes();
    let (mut toks, mut i) = (vec![], 0usize);
    while i < b.len() {
        if b[i] != b'<' {
            let j = html[i..].find('<').map_or(b.len(), |k| i + k);
            toks.push(Tok::Text(decode_entities(&html[i..j])));
            i = j;
            continue;
        }
        if html[i..].starts_with("<!--") { i = html[i..].find("-->").map_or(b.len(), |k| i + k + 3); continue; }
        if html[i..].starts_with("<!") || html[i..].starts_with("<?") { i = html[i..].find('>').map_or(b.len(), |k| i + k + 1); continue; }
        let Some(end) = find_tag_end(b, i) else { toks.push(Tok::Text(html[i..].to_string())); break };
        let inner = &html[i + 1..end];
        i = end + 1;
        if let Some(name) = inner.strip_prefix('/') { toks.push(Tok::End(name.trim().to_ascii_lowercase())); continue; }
        let self_closing = inner.ends_with('/');
        let inner = inner.trim_end_matches('/');
        let name_end = inner.find(|c: char| c.is_whitespace()).unwrap_or(inner.len());
        let name = inner[..name_end].to_ascii_lowercase();
        if name.is_empty() || !name.chars().next().unwrap().is_ascii_alphabetic() { toks.push(Tok::Text(format!("<{inner}>"))); continue; }
        let attrs = parse_attrs(&inner[name_end..]);
        // Raw-text elements: everything up to the closing tag is not markup.
        if name == "script" || name == "style" {
            let close = format!("</{name}");
            let from = i;
            let to = html[from..].to_ascii_lowercase().find(&close).map_or(b.len(), |k| from + k);
            i = html[to..].find('>').map_or(b.len(), |k| to + k + 1);
            continue;
        }
        toks.push(Tok::Start(name, attrs, self_closing));
    }
    toks
}

fn find_tag_end(b: &[u8], from: usize) -> Option<usize> {
    let mut quote = 0u8;
    for (k, &c) in b.iter().enumerate().skip(from + 1) {
        if quote != 0 { if c == quote { quote = 0; } continue; }
        if c == b'"' || c == b'\'' { quote = c; continue; }
        if c == b'>' { return Some(k); }
    }
    None
}

fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let (mut out, b, mut i) = (vec![], s.as_bytes(), 0usize);
    while i < b.len() {
        while i < b.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') { i += 1; }
        let ks = i;
        while i < b.len() && !b[i].is_ascii_whitespace() && b[i] != b'=' && b[i] != b'/' { i += 1; }
        if ks == i { i += 1; continue; }
        let key = s[ks..i].to_ascii_lowercase();
        while i < b.len() && b[i].is_ascii_whitespace() { i += 1; }
        if i < b.len() && b[i] == b'=' {
            i += 1;
            while i < b.len() && b[i].is_ascii_whitespace() { i += 1; }
            let val = if i < b.len() && (b[i] == b'"' || b[i] == b'\'') {
                let q = b[i]; i += 1; let vs = i;
                while i < b.len() && b[i] != q { i += 1; }
                let v = &s[vs..i]; i = (i + 1).min(b.len()); v
            } else { let vs = i; while i < b.len() && !b[i].is_ascii_whitespace() { i += 1; } &s[vs..i] };
            out.push((key, decode_entities(val)));
        } else { out.push((key, String::new())); }
    }
    out
}

fn attr<'a>(attrs: &'a [(String, String)], k: &str) -> Option<&'a str> { attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str()) }

#[derive(Clone, Default)]
struct Inline { bold: bool, italic: bool, underline: bool, mono: bool, link: Option<String>, color: Option<[u8; 3]>, size: Option<f32> }

struct TableB { cells: Vec<(usize, usize, usize, usize, Cell)>, row: usize, col: usize, span: usize, header: bool }

fn css_color(v: &str) -> Option<[u8; 3]> {
    let v = v.trim();
    if let Some(h) = v.strip_prefix('#') {
        let h = if h.len() == 3 { h.chars().flat_map(|c| [c, c]).collect::<String>() } else { h.to_string() };
        if h.len() == 6 { let n = u32::from_str_radix(&h, 16).ok()?; return Some([(n >> 16) as u8, (n >> 8) as u8, n as u8]); }
    }
    match v { "red" => Some([255, 0, 0]), "blue" => Some([0, 0, 255]), "green" => Some([0, 128, 0]), "gray" | "grey" => Some([128, 128, 128]), _ => None }
}

fn style_inline(base: &Inline, style: &str) -> Inline {
    let mut s = base.clone();
    for decl in style.split(';') {
        let Some((k, v)) = decl.split_once(':') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim().to_ascii_lowercase());
        match k.as_str() {
            "font-weight" => s.bold = v == "bold" || v.parse::<u32>().is_ok_and(|n| n >= 600),
            "font-style" => s.italic = v == "italic" || v == "oblique",
            "text-decoration" | "text-decoration-line" => s.underline = v.contains("underline"),
            "color" => s.color = css_color(&v).or(s.color),
            "font-size" => if let Some(pt) = v.strip_suffix("pt").and_then(|n| n.trim().parse::<f32>().ok()) { s.size = Some(pt); } else if let Some(px) = v.strip_suffix("px").and_then(|n| n.trim().parse::<f32>().ok()) { s.size = Some(px * 0.75); },
            "font-family" => s.mono = v.contains("mono") || v.contains("courier") || v.contains("consolas"),
            _ => {}
        }
    }
    s
}

/// Converts HTML. `resolve` supplies images for `src` values that are not data URIs.
pub fn parse(html: &str, resolve: &dyn Fn(&str) -> Option<Image>) -> Doc {
    let toks = tokenize(html);
    let mut containers: Vec<Vec<Block>> = vec![vec![]];
    let mut tables: Vec<TableB> = vec![];
    let mut styles: Vec<Inline> = vec![Inline::default()];
    // Open elements that pushed a style frame: (name, style stack length before). Closing restores it, which also
    // copes with missing end tags (<p>, <li>) and unknown tags.
    let mut open: Vec<(String, usize)> = vec![];
    let mut runs: Vec<Run> = vec![];
    let (mut heading, mut pre, mut in_head, mut in_title) = (None::<u8>, 0usize, 0usize, false);
    let mut lists: Vec<bool> = vec![];
    let mut pending_item = false;
    let mut align = Align::Left;
    let mut skip_depth = 0usize;

    fn flush(runs: &mut Vec<Run>, heading: Option<u8>, lists: &[bool], pending_item: &mut bool, align: Align, out: &mut Vec<Block>) {
        // Trim the edges of the paragraph (collapsed whitespace may leave a leading or trailing space).
        if let Some(f) = runs.first_mut() { f.text = f.text.trim_start().to_string(); }
        if let Some(l) = runs.last_mut() { l.text = l.text.trim_end().to_string(); }
        runs.retain(|r| !r.text.is_empty());
        if runs.is_empty() { return; }
        let list = if *pending_item || (!lists.is_empty() && false) { Some(ListInfo { ordered: *lists.last().unwrap_or(&false), level: (lists.len().saturating_sub(1)).min(6) as u8 }) } else { None };
        *pending_item = false;
        out.push(Block::Para(Para { runs: std::mem::take(runs), heading, list, align, ..Default::default() }));
    }

    let mut last_space = true;
    for t in toks {
        match t {
            Tok::Text(s) => {
                if skip_depth > 0 || in_head > 0 && !in_title { continue; }
                if in_title { continue; }
                let st = styles.last().unwrap();
                let text = if pre > 0 { s.replace("\r\n", "\n") } else {
                    let mut o = String::with_capacity(s.len());
                    for ch in s.chars() {
                        if ch.is_whitespace() && ch != '\u{a0}' { if !last_space { o.push(' '); last_space = true; } } else { o.push(if ch == '\u{a0}' { ' ' } else { ch }); last_space = false; }
                    }
                    o
                };
                if text.is_empty() { continue; }
                runs.push(Run { text, bold: st.bold || heading.is_some(), italic: st.italic, underline: st.underline || st.link.is_some(), mono: st.mono || pre > 0, link: st.link.clone(), color: st.color, size_pt: st.size });
            }
            Tok::Start(name, attrs, self_closing) => {
                if skip_depth > 0 { if !self_closing { skip_depth += 1; } continue; }
                let mut push_style = |f: &dyn Fn(&mut Inline)| { let mut s = styles.last().unwrap().clone(); f(&mut s); if let Some(css) = attr(&attrs, "style") { s = style_inline(&s, css); } open.push((name.clone(), styles.len())); styles.push(s); };
                match name.as_str() {
                    "head" => in_head += 1,
                    "title" => in_title = true,
                    "script" | "style" | "noscript" | "template" => { if !self_closing { skip_depth = 1; } }
                    "br" => { runs.push(Run::plain("\n")); last_space = true; }
                    "hr" => { flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap()); }
                    "p" | "div" | "section" | "article" | "header" | "footer" | "nav" | "main" | "aside" | "blockquote" | "figure" | "figcaption" | "address" | "dl" | "dt" | "dd" | "form" | "fieldset" => {
                        flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap());
                        align = match attr(&attrs, "align").or_else(|| attr(&attrs, "style").and_then(|s| s.split(';').find_map(|d| d.trim().strip_prefix("text-align:")))).map(|v| v.trim().to_ascii_lowercase()).as_deref() { Some("center") => Align::Center, Some("right") => Align::Right, Some("justify") => Align::Justify, _ => Align::Left };
                        if !self_closing { push_style(&|_| {}); }
                        last_space = true;
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap());
                        heading = Some(name.as_bytes()[1] - b'0');
                        push_style(&|_| {}); last_space = true;
                    }
                    "pre" => { flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap()); pre += 1; push_style(&|s| s.mono = true); }
                    "ul" | "ol" => { flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap()); lists.push(name == "ol"); push_style(&|_| {}); }
                    "li" => { flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap()); pending_item = true; last_space = true; push_style(&|_| {}); }
                    "b" | "strong" => push_style(&|s| s.bold = true),
                    "i" | "em" | "cite" | "dfn" | "var" => push_style(&|s| s.italic = true),
                    "u" | "ins" => push_style(&|s| s.underline = true),
                    "code" | "tt" | "kbd" | "samp" => push_style(&|s| s.mono = true),
                    "a" => { let href = attr(&attrs, "href").filter(|h| !h.starts_with('#') && !h.to_ascii_lowercase().starts_with("javascript:")).map(String::from); push_style(&|s| s.link = href.clone()); }
                    "font" => { let (c, sz) = (attr(&attrs, "color").and_then(css_color), attr(&attrs, "size").and_then(|v| v.parse::<f32>().ok()).map(|n| 8.0 + 2.0 * n.clamp(1.0, 7.0))); push_style(&|s| { s.color = c.or(s.color); s.size = sz.or(s.size); }) }
                    "span" | "small" | "big" | "sub" | "sup" | "label" | "mark" | "abbr" | "q" | "s" | "strike" | "del" | "center" => {
                        if name == "center" { flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap()); align = Align::Center; }
                        push_style(&|_| {});
                    }
                    "table" => {
                        flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap());
                        tables.push(TableB { cells: vec![], row: 0, col: 0, span: 1, header: false }); push_style(&|_| {});
                    }
                    "thead" | "tbody" | "tfoot" | "colgroup" | "col" | "caption" => { if !self_closing { push_style(&|_| {}); } }
                    "tr" => { if let Some(t) = tables.last_mut() { if t.col > 0 { t.row += 1; t.col = 0; } } push_style(&|_| {}); }
                    "td" | "th" => {
                        flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap());
                        if let Some(t) = tables.last_mut() { t.span = attr(&attrs, "colspan").and_then(|v| v.parse::<usize>().ok()).unwrap_or(1).clamp(1, 256); t.header = name == "th"; }
                        containers.push(vec![]);
                        let th = name == "th";
                        push_style(&|s| if th { s.bold = true });
                        last_space = true;
                    }
                    "img" => {
                        flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap());
                        if let Some(src) = attr(&attrs, "src") {
                            let img = if let Some(rest) = src.strip_prefix("data:") {
                                rest.split_once(',').and_then(|(meta, data)| meta.contains("base64").then(|| (meta.split(';').next().unwrap_or("image/png").to_string(), base64_decode(data))).and_then(|(m, d)| d.map(|d| (m, d))))
                                    .map(|(mime, data)| Image { data, mime, width_pt: None, height_pt: None })
                            } else { resolve(src) };
                            if let Some(mut img) = img {
                                let px = |k: &str| attr(&attrs, k).and_then(|v| v.trim_end_matches("px").parse::<f32>().ok()).map(|v| v * 0.75);
                                if let (Some(w), Some(h)) = (px("width"), px("height")) { img.width_pt = Some(w); img.height_pt = Some(h); }
                                containers.last_mut().unwrap().push(Block::Image(img));
                            }
                        }
                    }
                    _ => { if !self_closing { push_style(&|_| {}); } }
                }
                if self_closing {
                    if let Some(pos) = open.iter().rposition(|(n, _)| *n == name) { let len = open[pos].1; open.truncate(pos); styles.truncate(len); }
                }
            }
            Tok::End(name) => {
                if skip_depth > 0 { skip_depth -= 1; continue; }
                // Block ends flush while the element's own style still applies, then the frame is restored below.
                match name.as_str() {
                    "head" => in_head = in_head.saturating_sub(1),
                    "title" => in_title = false,
                    "p" | "div" | "section" | "article" | "header" | "footer" | "nav" | "main" | "aside" | "blockquote" | "figure" | "figcaption" | "address" | "dl" | "dt" | "dd" | "form" | "fieldset" | "center" => {
                        flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap());
                        align = Align::Left; last_space = true;
                    }
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => { flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap()); heading = None; last_space = true; }
                    "pre" => { flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap()); pre = pre.saturating_sub(1); }
                    "ul" | "ol" => { flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap()); lists.pop(); }
                    "li" => { flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap()); pending_item = false; }
                    "td" | "th" => {
                        flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap());
                        if containers.len() > 1 {
                            let blocks = containers.pop().unwrap();
                            if let Some(t) = tables.last_mut() {
                                let (r, c, sp) = (t.row, t.col, t.span);
                                t.cells.push((r, c, 1, sp, Cell { blocks: if blocks.is_empty() { vec![Block::Para(Para::default())] } else { blocks }, col_span: sp }));
                                t.col += sp; t.span = 1;
                            }
                        }
                    }
                    "tr" => { if let Some(t) = tables.last_mut() { if t.col > 0 { t.row += 1; t.col = 0; } } }
                    "table" => {
                        if let Some(t) = tables.pop() {
                            let rows = super::grid_to_rows(t.cells);
                            if !rows.is_empty() { containers.last_mut().unwrap().push(Block::Table(Table { rows })); }
                        }
                    }
                    _ => {}
                }
                if let Some(pos) = open.iter().rposition(|(n, _)| *n == name) { let len = open[pos].1; open.truncate(pos); styles.truncate(len); }
            }
        }
    }
    flush(&mut runs, heading, &lists, &mut pending_item, align, containers.last_mut().unwrap());
    Doc { blocks: containers.into_iter().next().unwrap_or_default(), ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn doc(h: &str) -> Doc { parse(h, &|_| None) }
    #[test] fn structure_and_inline_styles() {
        let d = doc("<html><head><title>T</title><style>p{}</style></head><body><h1>Head &amp; <i>it</i></h1><p>Some <b>bold</b> and <a href='http://x.y/?a=1&amp;b=2'>link</a>.<br>Next&nbsp;line</p><ul><li>one<ul><li>deep</li></ul></li><li>two</li></ul><ol><li>first</li></ol></body></html>");
        let Block::Para(h) = &d.blocks[0] else { panic!("{:?}", d.blocks) };
        assert_eq!((h.heading, h.text().as_str()), (Some(1), "Head & it"));
        let Block::Para(p) = &d.blocks[1] else { panic!() };
        assert_eq!(p.text(), "Some bold and link.\nNext line");
        assert!(p.runs.iter().any(|r| r.bold && r.text == "bold") && p.runs.iter().any(|r| r.link.as_deref() == Some("http://x.y/?a=1&b=2")));
        let items: Vec<(&str, u8, bool)> = d.blocks.iter().filter_map(|b| if let Block::Para(p) = b { p.list.as_ref().map(|l| (Box::leak(p.text().into_boxed_str()) as &str, l.level, l.ordered)) } else { None }).collect();
        assert_eq!(items, vec![("one", 0, false), ("deep", 1, false), ("two", 0, false), ("first", 0, true)]);
    }
    #[test] fn tables_with_spans_and_headers() {
        let d = doc("<table><tr><th colspan=2>Title</th></tr><tr><td>a</td><td>b<br>c</td></tr></table><p>after</p>");
        let Block::Table(t) = &d.blocks[0] else { panic!("{:?}", d.blocks) };
        assert_eq!(t.rows[0][0].col_span, 2);
        assert_eq!(t.to_text(), vec![vec!["Title", ""], vec!["a", "b\nc"]]);
        let Block::Para(p) = &d.blocks[1] else { panic!() };
        assert_eq!(p.text(), "after");
    }
    #[test] fn pre_keeps_whitespace_and_entities_decode() {
        let d = doc("<pre>a  b\n  c &lt;x&gt; &#8364; &#x41;</pre>");
        let Block::Para(p) = &d.blocks[0] else { panic!() };
        assert_eq!(p.text(), "a  b\n  c <x> € A");
        assert!(p.runs[0].mono);
    }
    #[test] fn data_uri_images_are_decoded() {
        let d = doc("<p>x</p><img width='40' height='20' src='data:image/png;base64,iVBORw0KGgo='>");
        let Block::Image(i) = &d.blocks[1] else { panic!("{:?}", d.blocks) };
        assert_eq!((i.mime.as_str(), i.data.len(), i.width_pt), ("image/png", 8, Some(30.0)));
    }
}
