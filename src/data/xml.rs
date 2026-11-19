//! XML <-> the `xml-js` "compact" JSON shape: an element is an object keyed by tag,
//! attributes under `_attributes`, text under `_text`, repeated tags become arrays.
use crate::{Error, ErrorCode, Result};
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use serde_json::{Map, Value};

fn bad(msg: impl std::fmt::Display) -> Error {
    Error::new(ErrorCode::EConvertFailed, format!("not valid XML: {msg}"))
}

fn push_child(parent: &mut Map<String, Value>, key: String, val: Value) {
    match parent.get_mut(&key) {
        None => { parent.insert(key, val); }
        Some(Value::Array(a)) => a.push(val),
        Some(existing) => { let first = existing.take(); *existing = Value::Array(vec![first, val]); }
    }
}

fn start_to_obj(e: &BytesStart) -> Result<(String, Map<String, Value>)> {
    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let mut obj = Map::new();
    let mut attrs = Map::new();
    for a in e.attributes() {
        let a = a.map_err(bad)?;
        attrs.insert(String::from_utf8_lossy(a.key.as_ref()).into_owned(),
                     Value::String(a.unescape_value().map_err(bad)?.into_owned()));
    }
    if !attrs.is_empty() { obj.insert("_attributes".into(), Value::Object(attrs)); }
    Ok((name, obj))
}

pub fn parse(text: &str) -> Result<Value> {
    let mut reader = Reader::from_str(text);
    let mut root = Map::new();
    let mut stack: Vec<(String, Map<String, Value>)> = vec![];
    loop {
        match reader.read_event().map_err(bad)? {
            Event::Decl(d) => {
                let mut attrs = Map::new();
                if let Ok(v) = d.version() { attrs.insert("version".into(), Value::String(String::from_utf8_lossy(&v).into())); }
                if let Some(Ok(v)) = d.encoding() { attrs.insert("encoding".into(), Value::String(String::from_utf8_lossy(&v).into())); }
                let mut decl = Map::new(); decl.insert("_attributes".into(), Value::Object(attrs));
                root.insert("_declaration".into(), Value::Object(decl));
            }
            Event::Start(e) => stack.push(start_to_obj(&e)?),
            Event::Empty(e) => {
                let (n, o) = start_to_obj(&e)?;
                let parent = stack.last_mut().map(|s| &mut s.1).unwrap_or(&mut root);
                push_child(parent, n, Value::Object(o));
            }
            Event::End(_) => {
                let (n, o) = stack.pop().ok_or_else(|| bad("unbalanced tags"))?;
                let parent = stack.last_mut().map(|s| &mut s.1).unwrap_or(&mut root);
                push_child(parent, n, Value::Object(o));
            }
            Event::Text(t) => {
                let raw = t.decode().map_err(bad)?;
                let s = quick_xml::escape::unescape(&raw).map_err(bad)?.into_owned();
                if !s.trim().is_empty() {
                    if let Some((_, o)) = stack.last_mut() {
                        match o.get_mut("_text") {
                            Some(Value::String(prev)) => prev.push_str(&s),
                            _ => { o.insert("_text".into(), Value::String(s)); }
                        }
                    }
                }
            }
            Event::CData(c) => {
                if let Some((_, o)) = stack.last_mut() {
                    o.insert("_cdata".into(), Value::String(String::from_utf8_lossy(&c).into()));
                }
            }
            Event::Comment(c) => {
                let parent = stack.last_mut().map(|s| &mut s.1).unwrap_or(&mut root);
                push_child(parent, "_comment".into(), Value::String(String::from_utf8_lossy(&c).into()));
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() { return Err(bad("unclosed element")); }
    Ok(Value::Object(root))
}

fn write_el(w: &mut Writer<Vec<u8>>, name: &str, v: &Value) -> Result<()> {
    let werr = |e: std::io::Error| Error::new(ErrorCode::EConvertFailed, format!("cannot write XML: {e}"));
    match v {
        Value::Array(items) => { for i in items { write_el(w, name, i)?; } return Ok(()); }
        Value::Object(o) => {
            let mut start = BytesStart::new(name);
            if let Some(Value::Object(a)) = o.get("_attributes") {
                for (k, val) in a { start.push_attribute((k.as_str(), plain(val).as_str())); }
            }
            let children: Vec<_> = o.iter().filter(|(k, _)| !matches!(k.as_str(), "_attributes" | "_text" | "_cdata")).collect();
            let text = o.get("_text").map(plain);
            if children.is_empty() && text.is_none() && !o.contains_key("_cdata") {
                w.write_event(Event::Empty(start)).map_err(werr)?;
                return Ok(());
            }
            w.write_event(Event::Start(start)).map_err(werr)?;
            if let Some(t) = text { w.write_event(Event::Text(BytesText::new(&t))).map_err(werr)?; }
            if let Some(Value::String(c)) = o.get("_cdata") { w.write_event(Event::CData(quick_xml::events::BytesCData::new(c.as_str()))).map_err(werr)?; }
            for (k, child) in children {
                if k == "_comment" {
                    for c in match child { Value::Array(a) => a.clone(), o => vec![o.clone()] } {
                        w.write_event(Event::Comment(BytesText::new(&plain(&c)))).map_err(werr)?;
                    }
                } else { write_el(w, k, child)?; }
            }
            w.write_event(Event::End(BytesEnd::new(name))).map_err(werr)?;
        }
        scalar => {
            w.write_event(Event::Start(BytesStart::new(name))).map_err(werr)?;
            w.write_event(Event::Text(BytesText::new(&plain(scalar)))).map_err(werr)?;
            w.write_event(Event::End(BytesEnd::new(name))).map_err(werr)?;
        }
    }
    Ok(())
}

fn plain(v: &Value) -> String {
    match v { Value::String(s) => s.clone(), Value::Null => String::new(), o => o.to_string() }
}

pub fn stringify(root: &Map<String, Value>) -> Result<String> {
    let mut w = Writer::new_with_indent(Vec::new(), b' ', 2);
    if let Some(Value::Object(d)) = root.get("_declaration") {
        let a = d.get("_attributes").and_then(Value::as_object);
        let g = |k: &str| a.and_then(|m| m.get(k)).map(plain);
        let decl = BytesDecl::new(&g("version").unwrap_or_else(|| "1.0".into()), g("encoding").as_deref(), None);
        w.write_event(Event::Decl(decl)).map_err(|e| Error::new(ErrorCode::EConvertFailed, e.to_string()))?;
    }
    for (k, v) in root.iter().filter(|(k, _)| k.as_str() != "_declaration") { write_el(&mut w, k, v)?; }
    let mut s = String::from_utf8(w.into_inner()).map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))?;
    s.push('\n');
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compact_shape() {
        let v = parse("<?xml version=\"1.0\"?><r a=\"1\"><i>x</i><i>y</i><e/></r>").unwrap();
        assert_eq!(v["r"]["_attributes"]["a"], "1");
        assert_eq!(v["r"]["i"][1]["_text"], "y");
        assert!(v["r"]["e"].is_object());
        assert_eq!(v["_declaration"]["_attributes"]["version"], "1.0");
    }
    #[test]
    fn roundtrip() {
        let v = parse("<r a=\"1\"><i>x</i><i>y</i></r>").unwrap();
        let s = stringify(v.as_object().unwrap()).unwrap();
        assert_eq!(parse(&s).unwrap(), v);
    }
    #[test]
    fn malformed_is_error() { assert!(parse("<a><b></a>").is_err()); }
}
