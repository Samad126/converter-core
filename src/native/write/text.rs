//! Plain text and Markdown.
use crate::native::model::*;

/// Numbering state per nesting level: `(is_ordered, count)`. A different kind at the same level restarts the count.
fn list_prefix(counters: &mut Vec<(bool, u32)>, l: &ListInfo) -> String {
    let lvl = l.level as usize;
    counters.truncate(lvl + 1);
    counters.resize(lvl + 1, (l.ordered, 0));
    if counters[lvl].0 != l.ordered { counters[lvl] = (l.ordered, 0); }
    counters[lvl].1 += 1;
    let indent = "  ".repeat(lvl);
    if l.ordered { format!("{indent}{}. ", counters[lvl].1) } else { format!("{indent}- ") }
}

/// True when this item starts a new list at the top level (so Markdown needs a blank line before it).
fn starts_new_list(counters: &[(bool, u32)], l: &ListInfo) -> bool {
    l.level == 0 && counters.first().is_some_and(|c| c.0 != l.ordered)
}

pub fn to_txt(doc: &Doc) -> String {
    fn blocks(bs: &[Block], out: &mut String) {
        let mut counters: Vec<(bool, u32)> = vec![];
        for b in bs {
            match b {
                Block::Para(p) => {
                    let text = p.text();
                    match &p.list {
                        Some(l) => { if starts_new_list(&counters, l) { out.push('\n'); } out.push_str(&list_prefix(&mut counters, l)); out.push_str(&text); out.push('\n'); continue; }
                        None => counters.clear(),
                    }
                    out.push_str(&text); out.push_str("\n\n");
                }
                Block::Table(t) => {
                    counters.clear();
                    for row in t.to_text().into_iter().filter(|r| r.iter().any(|c| !c.trim().is_empty())) { out.push_str(&row.iter().map(|c| c.replace('\n', " ")).collect::<Vec<_>>().join("\t")); out.push('\n'); }
                    out.push('\n');
                }
                Block::Image(_) | Block::PageBreak => counters.clear(),
            }
        }
    }
    let mut s = String::new();
    blocks(&doc.blocks, &mut s);
    format!("{}\n", s.trim_end())
}

fn md_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '*' | '_' | '`' | '[' | ']' | '|') { o.push('\\'); }
        o.push(c);
    }
    o
}

fn md_inline(p: &Para, in_cell: bool) -> String {
    let mut out = String::new();
    for r in &p.runs {
        let text = if in_cell { r.text.replace('\n', " ") } else { r.text.replace('\n', "  \n") };
        if text.trim().is_empty() { out.push_str(&text); continue; }
        let lead = &text[..text.len() - text.trim_start().len()];
        let trail = &text[text.trim_end().len()..];
        let core = text.trim();
        let mut s = if r.mono { format!("`{core}`") } else { md_escape(core) };
        if r.bold && p.heading.is_none() { s = format!("**{s}**"); }
        if r.italic { s = format!("*{s}*"); }
        if let Some(u) = &r.link { s = format!("[{s}]({u})"); }
        out.push_str(lead); out.push_str(&s); out.push_str(trail);
    }
    out
}

pub fn to_markdown(doc: &Doc) -> String {
    let mut out = String::new();
    let mut counters: Vec<(bool, u32)> = vec![];
    for b in &doc.blocks {
        match b {
            Block::Para(p) => {
                let inline = md_inline(p, false);
                if inline.trim().is_empty() { continue; }
                if let Some(h) = p.heading { counters.clear(); out.push_str(&format!("{} {}\n\n", "#".repeat(h as usize), inline.trim())); continue; }
                if let Some(l) = &p.list { if starts_new_list(&counters, l) { out.push('\n'); } out.push_str(&list_prefix(&mut counters, l)); out.push_str(inline.trim()); out.push('\n'); continue; }
                if !counters.is_empty() { out.push('\n'); counters.clear(); }
                out.push_str(inline.trim()); out.push_str("\n\n");
            }
            Block::Table(t) => {
                if !counters.is_empty() { out.push('\n'); counters.clear(); }
                let rows: Vec<Vec<String>> = t.rows.iter().map(|r| {
                    let mut cells = vec![];
                    for c in r {
                        let text = c.blocks.iter().filter_map(|b| if let Block::Para(p) = b { Some(md_inline(p, true)) } else { None })
                            .filter(|s| !s.trim().is_empty()).collect::<Vec<_>>().join(" ");
                        cells.push(text.trim().to_string());
                        for _ in 1..c.col_span.max(1) { cells.push(String::new()); }
                    }
                    cells
                }).collect();
                let width = rows.iter().map(Vec::len).max().unwrap_or(0);
                if width == 0 { continue; }
                for (i, r) in rows.iter().enumerate() {
                    let mut r = r.clone(); r.resize(width, String::new());
                    out.push_str(&format!("| {} |\n", r.join(" | ")));
                    if i == 0 { out.push_str(&format!("|{}|\n", vec![" --- "; width].join("|"))); }
                }
                out.push('\n');
            }
            Block::Image(_) | Block::PageBreak => { if !counters.is_empty() { out.push('\n'); counters.clear(); } }
        }
    }
    format!("{}\n", out.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn doc() -> Doc {
        let mut d = Doc::default();
        d.heading(1, "Hello");
        d.blocks.push(Block::Para(Para { runs: vec![Run { text: "bold".into(), bold: true, ..Default::default() }, Run::plain(" and "),
            Run { text: "link".into(), link: Some("http://x".into()), ..Default::default() }], ..Default::default() }));
        d.blocks.push(Block::Para(Para { runs: vec![Run::plain("one")], list: Some(ListInfo { ordered: true, level: 0 }), ..Default::default() }));
        d.blocks.push(Block::Para(Para { runs: vec![Run::plain("two")], list: Some(ListInfo { ordered: true, level: 0 }), ..Default::default() }));
        d.blocks.push(Block::Table(Table::from_text(&[vec!["a".into(), "b".into()], vec!["1".into(), "2".into()]])));
        d
    }
    #[test] fn a_new_list_kind_restarts_numbering() {
        let mut d = Doc::default();
        for (t, o) in [("a", false), ("b", false), ("c", true), ("d", true)] {
            d.blocks.push(Block::Para(Para { runs: vec![Run::plain(t)], list: Some(ListInfo { ordered: o, level: 0 }), ..Default::default() }));
        }
        assert_eq!(to_markdown(&d), "- a\n- b\n\n1. c\n2. d\n");
    }
    #[test] fn markdown_shape() {
        let md = to_markdown(&doc());
        assert!(md.starts_with("# Hello\n\n**bold** and [link](http://x)\n\n1. one\n2. two\n\n| a | b |\n| --- | --- |\n| 1 | 2 |"), "{md}");
    }
    #[test] fn txt_shape() {
        let t = to_txt(&doc());
        assert!(t.starts_with("Hello\n\nbold and link\n\n1. one\n2. two\n"), "{t}");
        assert!(t.contains("a\tb\n1\t2"));
    }
}
