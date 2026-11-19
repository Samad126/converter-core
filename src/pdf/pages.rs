use crate::{Error, ErrorCode, Result};
use lopdf::{Dictionary, Document, Object, ObjectId};

fn convert_failed(e: impl std::fmt::Display) -> Error { Error::new(ErrorCode::EConvertFailed, format!("Could not read the PDF: {e}")) }

pub fn load(bytes: &[u8]) -> Result<Document> {
    if bytes.is_empty() { return Err(Error::new(ErrorCode::EBadRequest, "PDF input is empty.")); }
    let doc = Document::load_mem(bytes).map_err(convert_failed)?;
    if doc.is_encrypted() { return Err(Error::new(ErrorCode::EEncrypted, "This document is password protected.")); }
    Ok(doc)
}

pub fn save(mut doc: Document) -> Result<Vec<u8>> {
    doc.prune_objects();
    let mut out = Vec::new();
    doc.save_to(&mut out).map_err(|e| Error::new(ErrorCode::EInternal, format!("Could not write the PDF: {e}")))?;
    Ok(out)
}

pub fn page_count(bytes: &[u8]) -> Result<usize> { Ok(load(bytes)?.get_pages().len()) }

fn page_ids(doc: &Document) -> Vec<ObjectId> { doc.get_pages().into_values().collect() }

const INHERITED: [&[u8]; 4] = [b"MediaBox", b"CropBox", b"Resources", b"Rotate"];

/// Pushes inherited page attributes down onto each page, so pages can be re-parented freely.
fn materialize_inherited(doc: &mut Document) {
    for id in page_ids(doc) {
        let mut found: Vec<(&[u8], Object)> = vec![];
        for key in INHERITED {
            let mut cur = id;
            for _ in 0..64 {
                let Ok(d) = doc.get_dictionary(cur) else { break };
                if let Ok(v) = d.get(key) { found.push((key, v.clone())); break; }
                match d.get(b"Parent").and_then(Object::as_reference) { Ok(p) => cur = p, Err(_) => break }
            }
        }
        if let Ok(page) = doc.get_object_mut(id).and_then(Object::as_dict_mut) {
            for (k, v) in found { if !page.has(k) { page.set(k, v); } }
        }
    }
}

/// A fresh single-level page tree holding `pages` in order.
fn rebuild_tree(doc: &mut Document, pages: Vec<ObjectId>) -> Result<()> {
    let root_id = doc.trailer.get(b"Root").and_then(Object::as_reference).map_err(convert_failed)?;
    let pages_id = doc.add_object(Dictionary::from_iter([
        ("Type", Object::Name(b"Pages".to_vec())),
        ("Kids", Object::Array(pages.iter().map(|p| Object::Reference(*p)).collect())),
        ("Count", Object::Integer(pages.len() as i64)),
    ]));
    for p in &pages {
        if let Ok(d) = doc.get_object_mut(*p).and_then(Object::as_dict_mut) { d.set("Parent", Object::Reference(pages_id)); }
    }
    doc.get_object_mut(root_id).and_then(Object::as_dict_mut).map_err(convert_failed)?
        .set("Pages", Object::Reference(pages_id));
    Ok(())
}

/// New document with the given zero-based pages, in order; a repeated index gets its own copy.
pub fn select_pages(bytes: &[u8], indices: &[usize]) -> Result<Vec<u8>> {
    let mut doc = load(bytes)?;
    materialize_inherited(&mut doc);
    let ids = page_ids(&doc);
    let mut used = std::collections::HashSet::new();
    let mut chosen = vec![];
    for &i in indices {
        let id = *ids.get(i).ok_or_else(|| Error::new(ErrorCode::EBadPageRange, format!("Page {} does not exist.", i + 1)))?;
        if used.insert(i) { chosen.push(id); }
        else {
            let copy = doc.get_object(id).map_err(convert_failed)?.clone();
            chosen.push(doc.add_object(copy));
        }
    }
    rebuild_tree(&mut doc, chosen)?;
    save(doc)
}

pub fn remove_pages(bytes: &[u8], remove: &[usize]) -> Result<Vec<u8>> {
    let n = page_count(bytes)?;
    let keep: Vec<usize> = (0..n).filter(|i| !remove.contains(i)).collect();
    if keep.is_empty() { return Err(Error::new(ErrorCode::EBadPageRange, "Removing these pages would leave the document empty.")); }
    select_pages(bytes, &keep)
}

pub fn merge(files: &[Vec<u8>]) -> Result<Vec<u8>> {
    let mut merged = Document::with_version("1.7");
    let mut all_pages: Vec<ObjectId> = vec![];
    for bytes in files {
        let mut doc = load(bytes)?;
        materialize_inherited(&mut doc);
        doc.renumber_objects_with(merged.max_id + 1);
        let ids = page_ids(&doc);
        merged.max_id = doc.max_id;
        merged.objects.extend(doc.objects);
        all_pages.extend(ids);
    }
    let pages_id = merged.new_object_id();
    merged.objects.insert(pages_id, Object::Dictionary(Dictionary::new()));
    let catalog = merged.add_object(Dictionary::from_iter([
        ("Type", Object::Name(b"Catalog".to_vec())), ("Pages", Object::Reference(pages_id))]));
    merged.trailer.set("Root", Object::Reference(catalog));
    rebuild_into(&mut merged, pages_id, all_pages);
    save(merged)
}

fn rebuild_into(doc: &mut Document, pages_id: ObjectId, pages: Vec<ObjectId>) {
    for p in &pages {
        if let Ok(d) = doc.get_object_mut(*p).and_then(Object::as_dict_mut) { d.set("Parent", Object::Reference(pages_id)); }
    }
    doc.objects.insert(pages_id, Object::Dictionary(Dictionary::from_iter([
        ("Type", Object::Name(b"Pages".to_vec())),
        ("Kids", Object::Array(pages.iter().map(|p| Object::Reference(*p)).collect())),
        ("Count", Object::Integer(pages.len() as i64)),
    ])));
}

/// `every` pages per part (the last may be shorter).
pub fn split(bytes: &[u8], every: usize) -> Result<Vec<Vec<u8>>> {
    if every < 1 { return Err(Error::new(ErrorCode::EBadRequest, "every must be a positive whole number.")); }
    let n = page_count(bytes)?;
    (0..n).step_by(every).map(|s| select_pages(bytes, &(s..(s + every).min(n)).collect::<Vec<_>>())).collect()
}

fn num(o: &Object) -> Option<f64> { o.as_float().ok().map(f64::from).or_else(|| o.as_i64().ok().map(|i| i as f64)) }

fn media_box(doc: &Document, id: ObjectId) -> Option<[f64; 4]> {
    let arr = doc.get_dictionary(id).ok()?.get(b"MediaBox").ok()?;
    let arr = match arr { Object::Reference(r) => doc.get_object(*r).ok()?, o => o }.as_array().ok()?;
    let v: Vec<f64> = arr.iter().filter_map(|o| match o { Object::Reference(r) => doc.get_object(*r).ok().and_then(num), o => num(o) }).collect();
    (v.len() == 4).then(|| [v[0], v[1], v[2], v[3]])
}

/// Adds `delta` degrees (multiple of 90) to the chosen pages (all when `None`).
pub fn rotate(bytes: &[u8], delta: i64, indices: Option<&[usize]>) -> Result<Vec<u8>> {
    let mut doc = load(bytes)?;
    materialize_inherited(&mut doc);
    let ids = page_ids(&doc);
    let targets: Vec<usize> = indices.map(<[usize]>::to_vec).unwrap_or_else(|| (0..ids.len()).collect());
    for i in targets {
        let id = ids[i];
        let page = doc.get_object_mut(id).and_then(Object::as_dict_mut).map_err(convert_failed)?;
        let cur = page.get(b"Rotate").ok().and_then(|o| o.as_i64().ok()).unwrap_or(0);
        page.set("Rotate", Object::Integer((cur + delta).rem_euclid(360)));
    }
    save(doc)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CropMargins { pub left: f64, pub right: f64, pub top: f64, pub bottom: f64 }

pub fn crop(bytes: &[u8], m: CropMargins, indices: Option<&[usize]>) -> Result<Vec<u8>> {
    let mut doc = load(bytes)?;
    materialize_inherited(&mut doc);
    let ids = page_ids(&doc);
    let targets: Vec<usize> = indices.map(<[usize]>::to_vec).unwrap_or_else(|| (0..ids.len()).collect());
    for i in targets {
        let id = ids[i];
        let [x0, y0, x1, y1] = media_box(&doc, id).unwrap_or([0.0, 0.0, 612.0, 792.0]);
        let (w, h) = (x1 - x0 - m.left - m.right, y1 - y0 - m.top - m.bottom);
        if w <= 0.0 || h <= 0.0 {
            return Err(Error::new(ErrorCode::EInvalidField, format!(
                "Cropping page {} by these margins would leave nothing: it is {}x{}pt.", i + 1, x1 - x0, y1 - y0)));
        }
        let (cx, cy) = (x0 + m.left, y0 + m.bottom);
        let page = doc.get_object_mut(id).and_then(Object::as_dict_mut).map_err(convert_failed)?;
        page.set("CropBox", Object::Array(vec![cx.into(), cy.into(), (cx + w).into(), (cy + h).into()]));
    }
    save(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::content::{Content, Operation};
    use lopdf::Stream;

    /// A real n-page PDF; page i draws the text "P<i>" so tests can tell pages apart.
    pub fn sample(n: usize) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font = doc.add_object(Dictionary::from_iter([("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"Type1".to_vec())), ("BaseFont", Object::Name(b"Helvetica".to_vec()))]));
        let mut kids = vec![];
        for i in 1..=n {
            let content = Content { operations: vec![
                Operation::new("BT", vec![]), Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![50.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal(format!("P{i}"))]), Operation::new("ET", vec![])] };
            let cid = doc.add_object(Stream::new(Dictionary::new(), content.encode().unwrap()));
            let page = doc.add_object(Dictionary::from_iter([("Type", Object::Name(b"Page".to_vec())),
                ("Parent", Object::Reference(pages_id)), ("Contents", Object::Reference(cid))]));
            kids.push(Object::Reference(page));
        }
        doc.objects.insert(pages_id, Object::Dictionary(Dictionary::from_iter([
            ("Type", Object::Name(b"Pages".to_vec())), ("Kids", Object::Array(kids)), ("Count", Object::Integer(n as i64)),
            ("MediaBox", vec![0.into(), 0.into(), 612.into(), 792.into()].into()),
            ("Resources", Dictionary::from_iter([("Font", Dictionary::from_iter([("F1", Object::Reference(font))]).into())]).into())])));
        let cat = doc.add_object(Dictionary::from_iter([("Type", Object::Name(b"Catalog".to_vec())), ("Pages", Object::Reference(pages_id))]));
        doc.trailer.set("Root", Object::Reference(cat));
        let mut out = vec![]; doc.save_to(&mut out).unwrap(); out
    }

    /// The text of each page, in order ("P1", "P3", …).
    fn labels(bytes: &[u8]) -> Vec<String> {
        let d = load(bytes).unwrap();
        d.get_pages().keys().map(|n| d.extract_text(&[*n]).unwrap().trim().to_string()).collect()
    }

    #[test] fn count_and_select_in_order_with_repeats() {
        let pdf = sample(4);
        assert_eq!(page_count(&pdf).unwrap(), 4);
        assert_eq!(labels(&select_pages(&pdf, &[2, 0, 2]).unwrap()), ["P3", "P1", "P3"]);
    }
    #[test] fn remove_and_refuse_empty() {
        let pdf = sample(3);
        assert_eq!(labels(&remove_pages(&pdf, &[1]).unwrap()), ["P1", "P3"]);
        assert_eq!(remove_pages(&pdf, &[0, 1, 2]).unwrap_err().code, ErrorCode::EBadPageRange);
    }
    #[test] fn merge_keeps_order_across_files() {
        let m = merge(&[sample(2), sample(1)]).unwrap();
        assert_eq!(labels(&m), ["P1", "P2", "P1"]);
    }
    #[test] fn split_every_n() {
        let parts = split(&sample(5), 2).unwrap();
        assert_eq!(parts.iter().map(|p| page_count(p).unwrap()).collect::<Vec<_>>(), [2, 2, 1]);
        assert_eq!(labels(&parts[2]), ["P5"]);
    }
    #[test] fn rotate_accumulates_and_wraps() {
        let r = rotate(&sample(2), 90, Some(&[1])).unwrap();
        let r = rotate(&r, -180, None).unwrap();
        let d = load(&r).unwrap();
        let rot: Vec<i64> = d.get_pages().values().map(|id| d.get_dictionary(*id).unwrap().get(b"Rotate").unwrap().as_i64().unwrap()).collect();
        assert_eq!(rot, [180, 270]);
    }
    #[test] fn crop_sets_cropbox_and_refuses_empty() {
        let c = crop(&sample(1), CropMargins { left: 10.0, right: 20.0, top: 30.0, bottom: 40.0 }, None).unwrap();
        let d = load(&c).unwrap();
        let id = *d.get_pages().values().next().unwrap();
        let b = d.get_dictionary(id).unwrap().get(b"CropBox").unwrap().as_array().unwrap();
        assert_eq!(b.iter().map(|o| num(o).unwrap()).collect::<Vec<_>>(), [10.0, 40.0, 592.0, 762.0]);
        let e = crop(&sample(1), CropMargins { left: 400.0, right: 400.0, ..Default::default() }, None).unwrap_err();
        assert_eq!(e.code, ErrorCode::EInvalidField);
    }
    #[test] fn garbage_is_convert_failed() {
        assert_eq!(load(b"not a pdf").unwrap_err().code, ErrorCode::EConvertFailed);
        assert_eq!(load(b"").unwrap_err().code, ErrorCode::EBadRequest);
    }
}
