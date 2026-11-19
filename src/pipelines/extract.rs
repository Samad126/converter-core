use super::Ctx;
use crate::{extract::{docx_tables::{self, Extraction}, xlsx}, formats::Resolved, Error, ErrorCode, Result};
use std::io::Read;
use std::path::PathBuf;

const DOCUMENT_PART: &str = "word/document.xml";

pub async fn run(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    match conv.target.id.as_str() {
        "tables" => tables(ctx, conv).await,
        "layers" => layers(ctx).await,
        other => Err(Error::new(ErrorCode::EUnsupported, format!("Extracting \"{other}\" is not available in this build yet."))),
    }
}

async fn tables(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let (input, cfg) = (ctx.input.clone(), ctx.cfg.clone());
    let out = ctx.out_dir.join(format!("converted{}", conv.target.extension));
    let out2 = out.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let mut zip = zip::ZipArchive::new(std::fs::File::open(&input)?)
            .map_err(|e| Error::new(ErrorCode::EConvertFailed, format!("not a readable document: {e}")))?;
        let entry = zip.by_name(DOCUMENT_PART)
            .map_err(|_| Error::new(ErrorCode::EConvertFailed, format!("the document has no readable {DOCUMENT_PART}")))?;
        // Decompression-bomb bound: the declared size, then a hard cap on what we actually inflate.
        if entry.size() > cfg.max_document_xml_bytes {
            return Err(Error::new(ErrorCode::ETooLarge, format!(
                "{DOCUMENT_PART} declares {} bytes, over the {} byte ceiling", entry.size(), cfg.max_document_xml_bytes)));
        }
        let mut xml = String::new();
        entry.take(cfg.max_document_xml_bytes + 1).read_to_string(&mut xml)
            .map_err(|e| Error::new(ErrorCode::EConvertFailed, format!("cannot read {DOCUMENT_PART}: {e}")))?;
        if xml.len() as u64 > cfg.max_document_xml_bytes {
            return Err(Error::new(ErrorCode::ETooLarge, format!("{DOCUMENT_PART} inflates past the {} byte ceiling", cfg.max_document_xml_bytes)));
        }
        let tables = match docx_tables::extract_tables(&xml, cfg.max_table_cells, cfg.max_tables) {
            Extraction::Ok(t) => t,
            Extraction::TooLarge { cells: true } => return Err(Error::new(ErrorCode::ETooLarge, format!("document holds more than {} table cells", cfg.max_table_cells))),
            Extraction::TooLarge { cells: false } => return Err(Error::new(ErrorCode::ETooLarge, format!("document holds more than {} tables", cfg.max_tables))),
        };
        if tables.is_empty() {
            return Err(Error::new(ErrorCode::ENoTables, "This document does not contain any tables."));
        }
        let mut taken = Default::default();
        let sheets: Vec<_> = tables.into_iter().enumerate()
            .map(|(i, rows)| (xlsx::sheet_name_for(i + 1, &mut taken), rows)).collect();
        xlsx::build(&sheets, &out2)
    }).await.map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))??;
    Ok(vec![out])
}

async fn layers(ctx: &Ctx<'_>) -> Result<Vec<PathBuf>> {
    let (input, cfg, out_dir) = (ctx.input.clone(), ctx.cfg.clone(), ctx.out_dir.clone());
    tokio::task::spawn_blocking(move || -> Result<Vec<PathBuf>> {
        let bytes = std::fs::read(&input)?;
        let ex = crate::extract::psd_layers::extract(&bytes, &cfg)?;
        let mut written = vec![];
        let mut put = |rel: &str, data: &[u8]| -> Result<()> {
            let p = out_dir.join(rel);
            if let Some(parent) = p.parent() { std::fs::create_dir_all(parent)?; }
            std::fs::write(&p, data)?;
            written.push(p);
            Ok(())
        };
        for (name, data) in &ex.files { put(name, data)?; }
        put(crate::extract::psd_layers::MANIFEST_FILENAME, &crate::extract::psd_layers::manifest_json(&ex.manifest))?;
        Ok(written)
    }).await.map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))?
}
