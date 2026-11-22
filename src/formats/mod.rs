//! The conversion matrix. Data comes from `data/formats.json` (generated from
//! `backend/src/formats.ts` by `tools/dump-formats.mjs`); the resolution logic
//! is a port of `resolveConversion`/`targetsFor`.
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetFormat {
    pub id: String,
    pub extension: String,
    pub media_type: String,
    pub label: String,
    pub mode: String,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default)]
    pub filters: HashMap<String, String>,
    #[serde(default)]
    pub engine_from: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub extract_from: Option<Vec<String>>,
    #[serde(default)]
    pub archive_writer: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFormat {
    pub extension: String,
    #[serde(default)]
    pub family: Option<String>,
    pub media_type: String,
    #[serde(default)]
    pub import_filter: Option<String>,
    pub targets: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MediaFormat {
    pub extension: String,
    pub id: String,
    pub kind: String,
    #[serde(rename = "mediaType")]
    pub media_type: String,
    pub label: String,
}

#[derive(Debug, Deserialize)]
struct Raw {
    targets: BTreeMap<String, TargetFormat>,
    sources: BTreeMap<String, SourceFormat>,
    media: BTreeMap<String, MediaFormat>,
}

pub struct Matrix {
    pub targets: BTreeMap<String, TargetFormat>,
    pub sources: BTreeMap<String, SourceFormat>,
    pub media: BTreeMap<String, MediaFormat>,
    /// Pairs added on top of the generated matrix that only the built-in converters can do.
    pub native_only: std::collections::HashSet<(String, String)>,
}

/// Which route produces a source/target pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Engine {
    Soffice, Extract, PdfEngine, Pandoc, Archive, Ffmpeg, Data, Heif, Assimp, Ebook, Font, Email,
    /// Pure Rust, no external program (see `crate::native`).
    Native,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub source: SourceFormat,
    pub target: TargetFormat,
    /// soffice `--convert-to` argument; empty unless `engine == Soffice` and mode is direct.
    pub convert_to: String,
    pub engine: Engine,
}

static MATRIX: OnceLock<Matrix> = OnceLock::new();

pub fn matrix() -> &'static Matrix {
    MATRIX.get_or_init(|| {
        let raw: Raw = serde_json::from_str(include_str!("../../data/formats.json"))
            .expect("data/formats.json is valid");
        let mut m = Matrix { targets: raw.targets, sources: raw.sources, media: raw.media, native_only: Default::default() };
        // Formats only the built-in converters handle (see data/formats-extra.json): every pair they list is native-only.
        let extra: Raw = serde_json::from_str(include_str!("../../data/formats-extra.json")).expect("data/formats-extra.json is valid");
        m.targets.extend(extra.targets);
        m.media.extend(extra.media);
        for (ext, src) in extra.sources {
            // A source without a document family has no engine route: every pair it lists is built-in only.
            if src.family.is_none() { for t in &src.targets { m.native_only.insert((ext.clone(), t.clone())); } }
            m.sources.insert(ext, src);
        }
        for (ext, target) in crate::native::all_pairs() {
            let Some(src) = m.sources.get_mut(ext) else { continue };
            if !m.targets.contains_key(target) || src.targets.iter().any(|t| t == target) { continue; }
            src.targets.push(target.to_string());
            m.native_only.insert((ext.to_string(), target.to_string()));
        }
        m
    })
}

/// Lowercased, dot-prefixed extension of a path ("" when none).
pub fn extension_of(path: &std::path::Path) -> String {
    path.extension().map(|e| format!(".{}", e.to_string_lossy().to_lowercase())).unwrap_or_default()
}

pub fn is_allowed_extension(ext: &str) -> bool { matrix().sources.contains_key(ext) }
pub fn is_target_id(id: &str) -> bool { matrix().targets.contains_key(id) }

pub fn targets_for(ext: &str) -> &'static [String] {
    matrix().sources.get(ext).map(|s| s.targets.as_slice()).unwrap_or(&[])
}

/// Targets reachable from EVERY given extension, in the first file's order.
/// Used by the quick-convert ring for multi-file drops.
pub fn common_targets<'a>(exts: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut it = exts.into_iter();
    let Some(first) = it.next() else { return vec![] };
    let mut out: Vec<String> = targets_for(first).to_vec();
    for e in it {
        let t = targets_for(e);
        out.retain(|x| t.contains(x));
    }
    out
}

fn pdf_filter_for(family: Option<&str>) -> Option<&'static String> {
    matrix().targets.get("pdf")?.filters.get(family?)
}

pub fn pdf_filter(family: Option<&str>) -> Option<String> { pdf_filter_for(family).cloned() }

pub fn resolve_conversion(ext: &str, target_id: &str) -> Option<Resolved> {
    let m = matrix();
    let source = m.sources.get(ext)?;
    let target = m.targets.get(target_id)?;
    if !source.targets.iter().any(|t| t == target_id) { return None; }
    let mk = |engine, convert_to: String| Some(Resolved {
        source: source.clone(), target: target.clone(), convert_to, engine,
    });

    if m.native_only.contains(&(ext.to_string(), target_id.to_string())) { return mk(Engine::Native, String::new()); }
    for (key, engine) in [
        ("pdf", Engine::PdfEngine), ("pandoc", Engine::Pandoc), ("data", Engine::Data),
        ("ebook", Engine::Ebook), ("email", Engine::Email),
    ] {
        if target.engine_from.get(key).is_some_and(|v| v.iter().any(|e| e == ext)) {
            return mk(engine, String::new());
        }
    }
    match target.mode.as_str() {
        "extract" => return mk(Engine::Extract, String::new()),
        "archive" => return mk(Engine::Archive, String::new()),
        "transcode" if ext == ".heic" || ext == ".heif" => return mk(Engine::Heif, String::new()),
        "transcode" => return mk(Engine::Ffmpeg, String::new()),
        "heif" => return mk(Engine::Heif, String::new()),
        "3d" => return mk(Engine::Assimp, String::new()),
        "ebook" => return mk(Engine::Ebook, String::new()),
        "font" => return mk(Engine::Font, String::new()),
        "data" => return mk(Engine::Data, String::new()),
        _ => {}
    }
    let family = source.family.as_deref()?;
    if target.mode == "raster" {
        pdf_filter_for(Some(family))?;
        return mk(Engine::Soffice, String::new());
    }
    let filter = target.filters.get(family)?;
    mk(Engine::Soffice, format!("{}:{}", &target.extension[1..], filter))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_loads_with_expected_sizes() {
        let m = matrix();
        assert_eq!(m.targets.len(), 79);
        assert_eq!(m.sources.len(), 127);
        assert_eq!(m.media.len(), 28);
    }

    #[test]
    fn every_advertised_pair_resolves() {
        for (ext, s) in &matrix().sources {
            for t in &s.targets {
                assert!(resolve_conversion(ext, t).is_some(), "{ext} -> {t} advertised but unresolvable");
            }
        }
    }

    #[test]
    fn docx_to_pdf_uses_soffice_filter() {
        let r = resolve_conversion(".docx", "pdf").unwrap();
        assert_eq!(r.engine, Engine::Soffice);
        assert_eq!(r.convert_to, "pdf:writer_pdf_Export");
    }

    #[test]
    fn pdf_to_docx_uses_pdf_engine() {
        assert_eq!(resolve_conversion(".pdf", "docx").unwrap().engine, Engine::PdfEngine);
    }

    #[test]
    fn unknown_pairs_are_none() {
        assert!(resolve_conversion(".docx", "mp3").is_none());
        assert!(resolve_conversion(".nope", "pdf").is_none());
    }

    #[test]
    fn common_targets_intersects() {
        let c = common_targets([".png", ".jpg"]);
        assert!(c.contains(&"webp".to_string()));
        assert!(common_targets([".png", ".docx"]).iter().all(|t| targets_for(".docx").contains(t)));
    }
}
