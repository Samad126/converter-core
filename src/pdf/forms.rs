//! AcroForm fields: list, fill, flatten. Port of `pdf-forms.service.ts` (pdf-lib's `getForm`).
//! Text appearances are generated with Helvetica so a filled form looks right in any viewer.
use super::draw::{invalid, n, std_encode, Pdf, Std};
use crate::{Error, ErrorCode, Result};
use lopdf::{Dictionary, Document, Object, ObjectId, Stream, StringFormat};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FieldSummary {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum FieldValue { Bool(bool), Text(String) }

fn internal(e: impl std::fmt::Display) -> Error { Error::new(ErrorCode::EInternal, e.to_string()) }

// ---- low-level helpers -----------------------------------------------------------------

fn resolve<'a>(doc: &'a Document, o: &'a Object) -> &'a Object {
    let mut cur = o;
    for _ in 0..16 { match cur { Object::Reference(r) => match doc.get_object(*r) { Ok(n) => cur = n, Err(_) => return cur }, _ => return cur } }
    cur
}

fn dict<'a>(doc: &'a Document, id: ObjectId) -> Option<&'a Dictionary> { doc.get_dictionary(id).ok() }

/// Attribute looked up on the field, then up its /Parent chain.
fn inherited<'a>(doc: &'a Document, mut id: ObjectId, key: &[u8]) -> Option<&'a Object> {
    for _ in 0..32 {
        let d = dict(doc, id)?;
        if let Ok(v) = d.get(key) { return Some(resolve(doc, v)); }
        id = d.get(b"Parent").ok()?.as_reference().ok()?;
    }
    None
}

pub fn decode_text(b: &[u8]) -> String {
    if b.starts_with(&[0xFE, 0xFF]) {
        let u: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&u)
    } else { b.iter().map(|&c| c as char).collect() }
}

fn encode_text(s: &str) -> Object {
    if s.chars().all(|c| (c as u32) < 0x100) { Object::String(s.chars().map(|c| c as u8).collect(), StringFormat::Literal) }
    else {
        let mut v = vec![0xFE, 0xFF];
        for u in s.encode_utf16() { v.extend(u.to_be_bytes()); }
        Object::String(v, StringFormat::Hexadecimal)
    }
}

fn as_text(o: &Object) -> Option<String> {
    match o { Object::String(b, _) => Some(decode_text(b)), Object::Name(n) => Some(String::from_utf8_lossy(n).into_owned()), _ => None }
}

#[derive(Debug, Clone)]
struct Field { id: ObjectId, name: String, widgets: Vec<ObjectId> }

fn acroform_fields(doc: &Document) -> Result<Vec<ObjectId>> {
    let root = doc.trailer.get(b"Root").and_then(Object::as_reference).map_err(internal)?;
    let Some(af) = dict(doc, root).and_then(|c| c.get(b"AcroForm").ok()).map(|o| resolve(doc, o)) else { return Ok(vec![]) };
    let Ok(af) = af.as_dict() else { return Ok(vec![]) };
    Ok(af.get(b"Fields").ok().map(|f| resolve(doc, f)).and_then(|f| f.as_array().ok()).map(|a| a.iter().filter_map(|o| o.as_reference().ok()).collect()).unwrap_or_default())
}

fn collect(doc: &Document, id: ObjectId, prefix: &str, out: &mut Vec<Field>, depth: u32) {
    if depth > 32 { return; }
    let Some(d) = dict(doc, id) else { return };
    let partial = d.get(b"T").ok().and_then(|t| as_text(resolve(doc, t)));
    let name = match (&partial, prefix.is_empty()) { (Some(p), true) => p.clone(), (Some(p), false) => format!("{prefix}.{p}"), (None, _) => prefix.to_string() };
    let kids: Vec<ObjectId> = d.get(b"Kids").ok().map(|k| resolve(doc, k)).and_then(|k| k.as_array().ok())
        .map(|a| a.iter().filter_map(|o| o.as_reference().ok()).collect()).unwrap_or_default();
    let has_field_kids = kids.iter().any(|k| dict(doc, *k).is_some_and(|kd| kd.has(b"T")));
    if has_field_kids { for k in kids { collect(doc, k, &name, out, depth + 1); } return; }
    let widgets = if kids.is_empty() { vec![id] } else { kids };
    out.push(Field { id, name, widgets });
}

fn all_fields(doc: &Document) -> Result<Vec<Field>> {
    let mut out = vec![];
    for id in acroform_fields(doc)? { collect(doc, id, "", &mut out, 0); }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind { Text, Checkbox, Radio, Dropdown, OptionList, Button, Unknown }

impl Kind {
    fn name(self) -> &'static str {
        match self { Self::Text => "text", Self::Checkbox => "checkbox", Self::Radio => "radio", Self::Dropdown => "dropdown", Self::OptionList => "optionList", Self::Button => "button", Self::Unknown => "unknown" }
    }
}

fn kind_of(doc: &Document, f: &Field) -> Kind {
    let ft = inherited(doc, f.id, b"FT").and_then(|o| o.as_name().ok()).map(<[u8]>::to_vec).unwrap_or_default();
    let ff = inherited(doc, f.id, b"Ff").and_then(|o| o.as_i64().ok()).unwrap_or(0);
    match ft.as_slice() {
        b"Tx" => Kind::Text,
        b"Btn" => if ff & (1 << 16) != 0 { Kind::Button } else if ff & (1 << 15) != 0 { Kind::Radio } else { Kind::Checkbox },
        b"Ch" => if ff & (1 << 17) != 0 { Kind::Dropdown } else { Kind::OptionList },
        _ => Kind::Unknown,
    }
}

/// "On" appearance names of a button's widgets (everything in /AP /N except Off).
fn on_values(doc: &Document, f: &Field) -> Vec<String> {
    let mut v = vec![];
    for w in &f.widgets {
        let n = dict(doc, *w).and_then(|d| d.get(b"AP").ok()).map(|o| resolve(doc, o)).and_then(|o| o.as_dict().ok())
            .and_then(|ap| ap.get(b"N").ok()).map(|o| resolve(doc, o)).and_then(|o| o.as_dict().ok());
        if let Some(n) = n { for (k, _) in n.iter() { let k = String::from_utf8_lossy(k).into_owned(); if k != "Off" && !v.contains(&k) { v.push(k); } } }
    }
    v
}

fn choice_options(doc: &Document, f: &Field) -> Vec<String> {
    inherited(doc, f.id, b"Opt").and_then(|o| o.as_array().ok()).map(|a| a.iter().filter_map(|o| match resolve(doc, o) {
        Object::Array(pair) => pair.get(1).or(pair.first()).and_then(|x| as_text(resolve(doc, x))),
        other => as_text(other) }).collect()).unwrap_or_default()
}

fn current_value(doc: &Document, f: &Field) -> Option<Object> { inherited(doc, f.id, b"V").cloned() }

fn summarize(doc: &Document, f: &Field) -> FieldSummary {
    let kind = kind_of(doc, f);
    let v = current_value(doc, f);
    let sel = || -> Vec<Value> {
        match &v { Some(Object::Array(a)) => a.iter().filter_map(|o| as_text(resolve(doc, o))).map(Value::String).collect(),
                   Some(o) => as_text(o).into_iter().map(Value::String).collect(), None => vec![] }
    };
    let (value, options) = match kind {
        Kind::Text => (Some(Value::String(v.as_ref().and_then(as_text).unwrap_or_default())), None),
        Kind::Checkbox => {
            let on = on_values(doc, f);
            let checked = v.as_ref().and_then(|o| o.as_name().ok()).map(|n| String::from_utf8_lossy(n).into_owned())
                .is_some_and(|s| on.first().is_some_and(|o| *o == s));
            (Some(Value::Bool(checked)), None)
        }
        Kind::Radio => (v.as_ref().and_then(|o| o.as_name().ok()).filter(|n| *n != b"Off").map(|n| Value::String(String::from_utf8_lossy(n).into_owned())), Some(on_values(doc, f))),
        Kind::Dropdown | Kind::OptionList => (Some(Value::Array(sel())), Some(choice_options(doc, f))),
        Kind::Button | Kind::Unknown => (None, None),
    };
    FieldSummary { name: f.name.clone(), kind: kind.name(), value, options }
}

pub fn list_fields(bytes: &[u8]) -> Result<Vec<FieldSummary>> {
    let doc = super::pages::load(bytes)?;
    Ok(all_fields(&doc)?.iter().map(|f| summarize(&doc, f)).collect())
}

// ---- filling ---------------------------------------------------------------------------

fn parse_da(doc: &Document, f: &Field) -> (f64, [f64; 3]) {
    let da = inherited(doc, f.id, b"DA").and_then(as_text).or_else(|| {
        let root = doc.trailer.get(b"Root").ok()?.as_reference().ok()?;
        let af = dict(doc, root)?.get(b"AcroForm").ok().map(|o| resolve(doc, o))?.as_dict().ok()?;
        af.get(b"DA").ok().and_then(|o| as_text(resolve(doc, o)))
    }).unwrap_or_default();
    let t: Vec<&str> = da.split_whitespace().collect();
    let mut size = 0.0;
    let mut color = [0.0; 3];
    for (i, tok) in t.iter().enumerate() {
        match *tok {
            "Tf" if i >= 1 => size = t[i - 1].parse().unwrap_or(0.0),
            "g" if i >= 1 => { let g = t[i - 1].parse().unwrap_or(0.0); color = [g; 3]; }
            "rg" if i >= 3 => color = [t[i - 3].parse().unwrap_or(0.0), t[i - 2].parse().unwrap_or(0.0), t[i - 1].parse().unwrap_or(0.0)],
            _ => {}
        }
    }
    (size, color)
}

fn rect_of(doc: &Document, id: ObjectId) -> Option<[f64; 4]> {
    let a = dict(doc, id)?.get(b"Rect").ok().map(|o| resolve(doc, o))?.as_array().ok()?;
    let v: Vec<f64> = a.iter().filter_map(|o| match resolve(doc, o) { Object::Integer(i) => Some(*i as f64), Object::Real(r) => Some(*r as f64), _ => None }).collect();
    (v.len() == 4).then(|| [v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

/// Single-line text appearance for a widget; `None` when the text needs glyphs Helvetica/WinAnsi lacks.
fn text_appearance(pdf: &mut Pdf, f: &Field, widget: ObjectId, text: &str) -> Result<Option<()>> {
    let Some([x0, y0, x1, y1]) = rect_of(&pdf.doc, widget) else { return Ok(None) };
    let (w, h) = (x1 - x0, y1 - y0);
    let Ok(enc) = std_encode(Std::Helvetica, text) else { return Ok(None) };
    let (mut size, color) = parse_da(&pdf.doc, f);
    if size <= 0.0 { size = (h - 4.0).clamp(4.0, 12.0); }
    let hex: String = enc.iter().map(|b| format!("{b:02X}")).collect();
    let y = (h - size) / 2.0 + size * 0.22;
    let content = format!("/Tx BMC q BT /Helv {} Tf {} {} {} rg 2 {} Td <{hex}> Tj ET Q EMC", n(size), n(color[0]), n(color[1]), n(color[2]), n(y));
    let font = pdf.std_font_object(Std::Helvetica);
    let mut res = Dictionary::new();
    res.set("Font", Dictionary::from_iter([("Helv", Object::Reference(font))]));
    let stream = Stream::new(Dictionary::from_iter([("Type", Object::Name(b"XObject".to_vec())), ("Subtype", Object::Name(b"Form".to_vec())),
        ("BBox", Object::Array(vec![0.into(), 0.into(), n_obj(w), n_obj(h)])), ("Resources", Object::Dictionary(res))]), content.into_bytes());
    let sid = pdf.doc.add_object(stream);
    let wd = pdf.doc.get_object_mut(widget).and_then(Object::as_dict_mut).map_err(internal)?;
    wd.set("AP", Dictionary::from_iter([("N", Object::Reference(sid))]));
    Ok(Some(()))
}

fn n_obj(v: f64) -> Object { Object::Real(v as f32) }

fn set_on_field(pdf: &mut Pdf, f: &Field, kind: Kind, value: &FieldValue) -> std::result::Result<(), String> {
    let set = |pdf: &mut Pdf, id: ObjectId, k: &str, v: Object| { if let Ok(d) = pdf.doc.get_object_mut(id).and_then(Object::as_dict_mut) { d.set(k, v); } };
    match (kind, value) {
        (Kind::Text, v) => {
            let text = match v { FieldValue::Text(s) => s.clone(), FieldValue::Bool(b) => b.to_string() };
            set(pdf, f.id, "V", encode_text(&text));
            for w in &f.widgets {
                if text_appearance(pdf, f, *w, &text).map_err(|e| e.message)?.is_none() {
                    // Characters outside WinAnsi: let the viewer draw it.
                    if let Ok(root) = pdf.doc.trailer.get(b"Root").and_then(Object::as_reference) {
                        if let Ok(Object::Dictionary(af)) = pdf.doc.get_object(root).and_then(Object::as_dict).and_then(|c| c.get(b"AcroForm")).map(|o| o.clone()) {
                            let mut af = af; af.set("NeedAppearances", Object::Boolean(true));
                            set(pdf, root, "AcroForm", Object::Dictionary(af));
                        }
                    }
                }
            }
            Ok(())
        }
        (Kind::Checkbox, FieldValue::Bool(b)) => {
            let on = on_values(&pdf.doc, f).into_iter().next().unwrap_or_else(|| "Yes".into());
            let state = if *b { on } else { "Off".into() };
            set(pdf, f.id, "V", Object::Name(state.clone().into_bytes()));
            for w in &f.widgets { set(pdf, *w, "AS", Object::Name(state.clone().into_bytes())); }
            Ok(())
        }
        (Kind::Checkbox, _) => Err("a checkbox needs a boolean value (true or false)".into()),
        (Kind::Radio, v) => {
            let want = match v { FieldValue::Text(s) => s.clone(), FieldValue::Bool(b) => b.to_string() };
            let options = on_values(&pdf.doc, f);
            if !options.contains(&want) { return Err(format!("Failed to select invalid option: \"{want}\"")); }
            set(pdf, f.id, "V", Object::Name(want.clone().into_bytes()));
            for w in &f.widgets {
                let has = on_values(&pdf.doc, &Field { id: *w, name: String::new(), widgets: vec![*w] }).contains(&want);
                set(pdf, *w, "AS", Object::Name(if has { want.clone() } else { "Off".into() }.into_bytes()));
            }
            Ok(())
        }
        (Kind::Dropdown | Kind::OptionList, v) => {
            let want = match v { FieldValue::Text(s) => s.clone(), FieldValue::Bool(b) => b.to_string() };
            if !choice_options(&pdf.doc, f).contains(&want) { return Err(format!("Failed to select invalid option: \"{want}\"")); }
            set(pdf, f.id, "V", encode_text(&want));
            for w in &f.widgets { let _ = text_appearance(pdf, f, *w, &want); }
            Ok(())
        }
        (Kind::Button | Kind::Unknown, _) => Err("fields of this type cannot be filled".into()),
    }
}

pub fn fill(bytes: &[u8], values: &[(String, FieldValue)], flatten: bool) -> Result<Vec<u8>> {
    let mut pdf = Pdf::load(bytes)?;
    let fields = all_fields(&pdf.doc)?;
    for (name, value) in values {
        let f = fields.iter().find(|f| &f.name == name).ok_or_else(|| invalid(format!("There is no form field named \"{name}\" in this PDF.")))?;
        let kind = kind_of(&pdf.doc, f);
        set_on_field(&mut pdf, f, kind, value).map_err(|m| invalid(format!("The value given for field \"{name}\" is not valid: {m}")))?;
    }
    if flatten { flatten_all(&mut pdf, &fields)?; }
    pdf.finish()
}

// ---- flattening ------------------------------------------------------------------------

fn flatten_all(pdf: &mut Pdf, fields: &[Field]) -> Result<()> {
    let widgets: std::collections::HashSet<ObjectId> = fields.iter().flat_map(|f| f.widgets.iter().copied()).collect();
    for pn in 1..=pdf.page_count() {
        let page = pdf.page(pn)?;
        let annots: Vec<Object> = match dict(&pdf.doc, page).and_then(|d| d.get(b"Annots").ok()).map(|o| resolve(&pdf.doc, o)) {
            Some(Object::Array(a)) => a.clone(), _ => continue };
        let mut keep = vec![];
        for a in annots {
            let Ok(id) = a.as_reference() else { keep.push(a); continue };
            if !widgets.contains(&id) { keep.push(a); continue; }
            let Some([rx0, ry0, rx1, ry1]) = rect_of(&pdf.doc, id) else { continue };
            // Normal appearance: a stream, or a dict of states chosen by /AS.
            let ap_n = dict(&pdf.doc, id).and_then(|d| d.get(b"AP").ok()).map(|o| resolve(&pdf.doc, o)).and_then(|o| o.as_dict().ok()).and_then(|ap| ap.get(b"N").ok()).cloned();
            let stream_id = match ap_n {
                Some(Object::Reference(r)) => match pdf.doc.get_object(r) { Ok(Object::Dictionary(states)) => {
                    let st = dict(&pdf.doc, id).and_then(|d| d.get(b"AS").ok()).and_then(|o| o.as_name().ok()).map(<[u8]>::to_vec);
                    st.and_then(|s| states.get(&s).ok()).and_then(|o| o.as_reference().ok()) } _ => Some(r) },
                Some(Object::Dictionary(states)) => dict(&pdf.doc, id).and_then(|d| d.get(b"AS").ok()).and_then(|o| o.as_name().ok()).and_then(|s| states.get(s).ok()).and_then(|o| o.as_reference().ok()),
                _ => None,
            };
            let Some(sid) = stream_id else { continue };
            let Some(bbox) = pdf.doc.get_object(sid).ok().and_then(|o| match o { Object::Stream(s) => s.dict.get(b"BBox").ok().cloned(), _ => None }).and_then(|b| b.as_array().ok().cloned()) else { continue };
            let b: Vec<f64> = bbox.iter().filter_map(|o| match o { Object::Integer(i) => Some(*i as f64), Object::Real(r) => Some(*r as f64), _ => None }).collect();
            if b.len() != 4 { continue; }
            let (bw, bh) = ((b[2] - b[0]).max(1e-6), (b[3] - b[1]).max(1e-6));
            let (sx, sy) = ((rx1 - rx0) / bw, (ry1 - ry0) / bh);
            let name = format!("Fl{}", sid.0);
            pdf.add_resource(page, "XObject", &name, sid)?;
            pdf.push(page, &format!("q {} 0 0 {} {} {} cm /{name} Do Q\n", n(sx), n(sy), n(rx0 - b[0] * sx), n(ry0 - b[1] * sy)));
        }
        pdf.doc.get_object_mut(page).and_then(Object::as_dict_mut).map_err(internal)?.set("Annots", Object::Array(keep));
    }
    // The fields are gone, so drop the form itself.
    if let Ok(root) = pdf.doc.trailer.get(b"Root").and_then(Object::as_reference) {
        if let Ok(c) = pdf.doc.get_object_mut(root).and_then(Object::as_dict_mut) { c.remove(b"AcroForm"); }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdf::pages::tests::sample;

    /// `sample(1)` plus an AcroForm: a text field `name` and a checkbox `agree`.
    fn form() -> Vec<u8> {
        let mut doc = Document::load_mem(&sample(1)).unwrap();
        let page = *doc.get_pages().values().next().unwrap();
        let widget = |doc: &mut Document, name: &str, ft: &str, rect: [i64; 4], extra: Vec<(&str, Object)>| {
            let mut d = Dictionary::from_iter([("Type", Object::Name(b"Annot".to_vec())), ("Subtype", Object::Name(b"Widget".to_vec())),
                ("FT", Object::Name(ft.as_bytes().to_vec())), ("T", Object::string_literal(name)), ("F", Object::Integer(4)),
                ("Rect", Object::Array(rect.iter().map(|v| Object::Integer(*v)).collect())), ("P", Object::Reference(page))]);
            for (k, v) in extra { d.set(k, v); }
            doc.add_object(d)
        };
        let on = doc.add_object(Stream::new(Dictionary::from_iter([("Subtype", Object::Name(b"Form".to_vec())), ("BBox", Object::Array(vec![0.into(), 0.into(), 20.into(), 20.into()]))]), b"0 0 20 20 re f".to_vec()));
        let name = widget(&mut doc, "name", "Tx", [50, 600, 250, 620], vec![("DA", Object::string_literal("/Helv 12 Tf 0 g"))]);
        let agree = widget(&mut doc, "agree", "Btn", [50, 560, 70, 580], vec![("AS", Object::Name(b"Off".to_vec())),
            ("AP", Dictionary::from_iter([("N", Dictionary::from_iter([("Yes", Object::Reference(on))]).into())]).into())]);
        doc.get_object_mut(page).and_then(Object::as_dict_mut).unwrap().set("Annots", Object::Array(vec![Object::Reference(name), Object::Reference(agree)]));
        let root = doc.trailer.get(b"Root").unwrap().as_reference().unwrap();
        doc.get_object_mut(root).and_then(Object::as_dict_mut).unwrap().set("AcroForm",
            Dictionary::from_iter([("Fields", Object::Array(vec![Object::Reference(name), Object::Reference(agree)]))]));
        let mut out = vec![]; doc.save_to(&mut out).unwrap(); out
    }

    fn t(s: &str) -> FieldValue { FieldValue::Text(s.into()) }

    #[test] fn lists_fields_with_types_and_values() {
        let f = list_fields(&form()).unwrap();
        assert_eq!(f.iter().map(|x| (x.name.as_str(), x.kind)).collect::<Vec<_>>(), [("name", "text"), ("agree", "checkbox")]);
        assert_eq!(f[1].value, Some(Value::Bool(false)));
        assert!(list_fields(&sample(1)).unwrap().is_empty());
    }
    #[test] fn fill_sets_values_and_appearance() {
        let out = fill(&form(), &[("name".into(), t("Ann é")), ("agree".into(), FieldValue::Bool(true))], false).unwrap();
        let f = list_fields(&out).unwrap();
        assert_eq!(f[0].value, Some(Value::String("Ann é".into())));
        assert_eq!(f[1].value, Some(Value::Bool(true)));
        let d = super::super::pages::load(&out).unwrap();
        assert!(d.get_pages().values().all(|id| d.get_page_content(*id).len() > 0));
        let w = all_fields(&d).unwrap()[0].widgets[0];
        assert!(d.get_dictionary(w).unwrap().has(b"AP"), "text field needs an appearance stream");
    }
    #[test] fn flatten_removes_widgets_and_draws_them() {
        let out = fill(&form(), &[("name".into(), t("Flat"))], true).unwrap();
        assert!(list_fields(&out).unwrap().is_empty());
        let d = super::super::pages::load(&out).unwrap();
        let page = *d.get_pages().values().next().unwrap();
        assert!(d.get_dictionary(page).unwrap().get(b"Annots").unwrap().as_array().unwrap().is_empty());
        assert!(String::from_utf8_lossy(&d.get_page_content(page)).contains(" Do"));
    }
    #[test] fn bad_requests_are_invalid_field() {
        for (n, v) in [("nope", t("x")), ("agree", t("yes")), ("name", FieldValue::Bool(true))] {
            let r = fill(&form(), &[(n.into(), v)], false);
            if n == "name" { assert!(r.is_ok(), "bool into text field stringifies like pdf-lib's String(value)"); }
            else { assert_eq!(r.unwrap_err().code, ErrorCode::EInvalidField, "{n}"); }
        }
    }
    #[test] fn text_encoding_roundtrip() {
        for s in ["plain", "é", "ə Azərbaycan", "日本"] { assert_eq!(as_text(&encode_text(s)).unwrap(), s); }
    }
}
