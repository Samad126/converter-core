//! PSD → one PNG per layer (in group folders) + `manifest.json`. Port of `psd-layers.ts`: the same naming and
//! skipping rules, and the same bomb limits applied to *declared* sizes before any pixel data is decoded.
use crate::config::Config;
use crate::{Error, ErrorCode, Result};
use serde_json::{json, Value};
use std::collections::HashMap;

pub const MANIFEST_FILENAME: &str = "manifest.json";
const MAX_NAME_CHARS: usize = 100;

#[derive(Debug)]
pub struct Extraction { pub files: Vec<(String, Vec<u8>)>, pub manifest: Value }

fn too_large(msg: String) -> Error { Error::new(ErrorCode::ETooLarge, msg) }
fn unreadable(msg: impl std::fmt::Display) -> Error { Error::new(ErrorCode::EConvertFailed, format!("could not read layers from the upload: {msg}")) }

/// A layer/group name that is safe as a file name on every OS (port of `safeLayerName`).
pub fn safe_layer_name(raw: &str, fallback: &str) -> String {
    let no_ctrl: String = raw.chars().filter(|c| !matches!(*c as u32, 0..=0x1f | 0x7f)).collect();
    let replaced: String = no_ctrl.chars().map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c }).collect();
    // Slightly stricter than the TS version: whitespace is trimmed *before* leading dots are stripped, so "  ..x" cannot
    // end up as a hidden-file name.
    let s = replaced.trim().trim_start_matches('.');
    let s = s.trim_end_matches(['.', ' ']).trim();
    let t: String = s.chars().take(MAX_NAME_CHARS).collect();
    if t.is_empty() { fallback.to_string() } else { t }
}

/// `name`, then `name_2`, `name_3` … unique case-insensitively within one folder (port of `reserveName`).
fn reserve_name(taken: &mut HashMap<String, usize>, name: &str) -> String {
    let key = name.to_lowercase();
    let Some(&seen) = taken.get(&key) else { taken.insert(key, 1); return name.to_string(); };
    let mut suffix = seen + 1;
    let mut cand = format!("{name}_{suffix}");
    while taken.contains_key(&cand.to_lowercase()) { suffix += 1; cand = format!("{name}_{suffix}"); }
    taken.insert(key, suffix);
    taken.insert(cand.to_lowercase(), 1);
    cand
}

fn encode_png(w: u32, h: u32, rgba: Vec<u8>) -> std::result::Result<Vec<u8>, String> {
    let img = image::RgbaImage::from_raw(w, h, rgba).ok_or("pixel buffer does not match the layer size")?;
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(out)
}

struct Found<'a> { layer: &'a ag_psd::psd::Layer, groups: Vec<String> }

fn walk<'a>(layers: &'a [ag_psd::psd::Layer], groups: &[String], out: &mut Vec<Found<'a>>, depth: usize) {
    if depth > 64 { return; }
    for layer in layers {
        if let Some(children) = &layer.children {
            let name = safe_layer_name(layer.additional_info.name.as_deref().unwrap_or(""), &format!("Group {}", groups.len() + 1));
            let mut g = groups.to_vec(); g.push(name);
            walk(children, &g, out, depth + 1);
        } else { out.push(Found { layer, groups: groups.to_vec() }); }
    }
}

pub fn extract(bytes: &[u8], cfg: &Config) -> Result<Extraction> {
    if bytes.len() < 26 || &bytes[0..4] != b"8BPS" { return Err(unreadable("this is not a PSD file")); }
    if u16::from_be_bytes([bytes[4], bytes[5]]) != 1 { return Err(unreadable("this is a PSB (large document) file, not a PSD")); }

    // The parser is a young port, so a panic inside it means "unreadable file", never a crash. Its own memory budget
    // (`total_memory_limit`) refuses files that declare more decoded pixels than we allow.
    let opts = ag_psd::psd::ReadOptions {
        use_image_data: Some(true), skip_composite_image_data: Some(true), skip_thumbnail: Some(true), skip_linked_files_data: Some(true),
        total_memory_limit: Some(cfg.max_psd_decode_bytes as usize), ..Default::default()
    };
    let psd = std::panic::catch_unwind(|| ag_psd::reader::read_psd(bytes, &opts))
        .map_err(|_| unreadable("the file is damaged"))?
        .map_err(|e| { let m = format!("{e}"); if m.to_lowercase().contains("memory") { too_large(format!("the layers need more memory than the {}-byte limit", cfg.max_psd_decode_bytes)) } else { unreadable(m) } })?;

    let mut found = vec![];
    walk(psd.children.as_deref().unwrap_or(&[]), &[], &mut found, 0);
    if found.is_empty() { return Err(Error::new(ErrorCode::ENoLayers, "This document has no layers.")); }
    if found.len() > cfg.max_psd_layers {
        return Err(too_large(format!("the document holds {} layers, over the {} layer limit", found.len(), cfg.max_psd_layers)));
    }

    let (mut files, mut records): (Vec<(String, Vec<u8>)>, Vec<Value>) = (vec![], vec![]);
    let mut taken: HashMap<String, HashMap<String, usize>> = HashMap::new();
    let mut output_bytes: u64 = 0;

    for Found { layer, groups } in found {
        let directory = groups.join("/");
        let name = safe_layer_name(layer.additional_info.name.as_deref().unwrap_or(""), &format!("Layer {}", records.len() + 1));
        let int = |v: Option<f64>| v.unwrap_or(0.0).round() as i64;
        let mut rec = json!({
            "name": name, "group": if directory.is_empty() { Value::Null } else { Value::String(directory.clone()) }, "file": Value::Null,
            "width": 0, "height": 0, "left": int(layer.left), "top": int(layer.top), "right": int(layer.right), "bottom": int(layer.bottom),
            "hidden": layer.hidden == Some(true), "opacity": (layer.opacity.unwrap_or(1.0) * 255.0).round() as i64,
            "bytes": 0, "skippedReason": Value::Null });

        let mut skipped: Option<String> = None;
        match &layer.image_data {
            None => skipped = Some("no pixel data (adjustment, text or empty layer)".into()),
            Some(px) if px.width == 0 || px.height == 0 => skipped = Some("zero-sized layer".into()),
            Some(px) => match encode_png(px.width, px.height, px.data.clone()) {
                Err(e) => skipped = Some(format!("could not be encoded: {e}")),
                Ok(png) => {
                    if output_bytes + png.len() as u64 > cfg.max_layer_output_bytes {
                        return Err(too_large(format!("the layers produce more than the {} byte limit for image output", cfg.max_layer_output_bytes)));
                    }
                    let unique = reserve_name(taken.entry(directory.clone()).or_default(), &name);
                    let file = if directory.is_empty() { format!("{unique}.png") } else { format!("{directory}/{unique}.png") };
                    rec["file"] = Value::String(file.clone());
                    rec["width"] = px.width.into(); rec["height"] = px.height.into(); rec["bytes"] = png.len().into();
                    output_bytes += png.len() as u64;
                    files.push((file, png));
                }
            },
        }
        if let Some(reason) = skipped { rec["skippedReason"] = Value::String(reason); }
        records.push(rec);
    }

    let exported = files.len();
    if exported == 0 { return Err(Error::new(ErrorCode::ENoLayers, "No layer in this document has pixels that can be saved.")); }
    let manifest = json!({ "canvas": { "width": psd.width as u64, "height": psd.height as u64 }, "exported": exported, "skipped": records.len() - exported, "layers": records });
    Ok(Extraction { files, manifest })
}

pub fn manifest_json(m: &Value) -> Vec<u8> { let mut s = serde_json::to_string_pretty(m).unwrap(); s.push('\n'); s.into_bytes() }

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn names_are_made_safe_like_the_ts_version() {
        assert_eq!(safe_layer_name("Bad/Name:*?", "x"), "Bad_Name___");
        assert_eq!(safe_layer_name("  ..hidden. ", "x"), "hidden");
        assert_eq!(safe_layer_name("a\u{0007}b", "x"), "ab");
        assert_eq!(safe_layer_name("", "Layer 3"), "Layer 3");
        assert_eq!(safe_layer_name("...", "Layer 3"), "Layer 3");
        assert_eq!(safe_layer_name(&"ə".repeat(150), "x").chars().count(), 100);
        assert_eq!(safe_layer_name("Qatar/Şəhər", "x"), "Qatar_Şəhər", "non-ASCII names are kept");
    }
    #[test] fn duplicate_names_get_numbered_case_insensitively() {
        let mut t = HashMap::new();
        assert_eq!(reserve_name(&mut t, "star"), "star");
        assert_eq!(reserve_name(&mut t, "Star"), "Star_2");
        assert_eq!(reserve_name(&mut t, "STAR"), "STAR_3");
        assert_eq!(reserve_name(&mut t, "star_2"), "star_2_2", "a literal star_2 must not collide with the generated one");
    }
    #[test] fn non_psd_and_psb_are_refused_with_a_reason() {
        let cfg = Config::default();
        assert!(extract(b"not a psd at all, definitely not", &cfg).unwrap_err().message.contains("not a PSD"));
        let mut psb = b"8BPS\x00\x02".to_vec(); psb.resize(40, 0);
        assert!(extract(&psb, &cfg).unwrap_err().message.contains("PSB"));
    }
    #[test] fn garbage_after_a_valid_signature_is_an_error_not_a_panic() {
        let mut v = b"8BPS\x00\x01".to_vec(); v.extend((0..200u8).map(|i| i.wrapping_mul(37)));
        assert!(extract(&v, &Config::default()).is_err());
    }
}
