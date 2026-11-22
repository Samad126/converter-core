//! Spreadsheet outputs: CSV, TSV, JSON, XLSX, ODS.
use crate::data::csv::stringify_delimited;
use crate::extract::docx_tables::is_legal_xml_char;
use crate::extract::xlsx::{MAX_CELL_CHARACTERS, MAX_COLUMNS, MAX_ROWS};
use crate::native::{model::*, util::esc_xml};
use crate::{Error, ErrorCode, Result};
use std::io::Write;
use std::path::Path;

fn internal(e: impl std::fmt::Display) -> Error { Error::new(ErrorCode::EInternal, e.to_string()) }

/// A plain decimal number (`12`, `-3.5`, `0.25`), not `007`, `+1`, `1e5` or something over 15 digits.
pub(crate) fn as_number(s: &str) -> Option<f64> {
    let t = s.trim();
    let body = t.strip_prefix('-').unwrap_or(t);
    let (int, frac) = body.split_once('.').map_or((body, None), |(a, b)| (a, Some(b)));
    let ok = !int.is_empty() && int.bytes().all(|b| b.is_ascii_digit()) && (int == "0" || !int.starts_with('0'))
        && frac.is_none_or(|f| !f.is_empty() && f.bytes().all(|b| b.is_ascii_digit()))
        && body.len() <= 15;
    if ok { t.parse().ok() } else { None }
}

pub fn delimited(book: &Workbook, delim: char, out: &Path) -> Result<()> {
    let rows = book.sheets.first().map(|s| s.rows.as_slice()).unwrap_or(&[]);
    std::fs::write(out, stringify_delimited(rows, delim))?;
    Ok(())
}

/// Spreadsheet cells arrive as text; plain numbers become JSON numbers.
fn numberize(v: &mut serde_json::Value) {
    use serde_json::Value;
    match v {
        Value::String(s) => if let Some(n) = as_number(s).and_then(serde_json::Number::from_f64) { *v = Value::Number(n); },
        Value::Array(a) => a.iter_mut().for_each(numberize),
        Value::Object(o) => o.values_mut().for_each(numberize),
        _ => {}
    }
}

pub fn json(book: &Workbook, out: &Path) -> Result<()> {
    let v = if book.sheets.len() == 1 {
        crate::data::table_to_records(book.sheets[0].rows.clone())
    } else {
        serde_json::Value::Object(book.sheets.iter().map(|s| (s.name.clone(), crate::data::table_to_records(s.rows.clone()))).collect())
    };
    let mut v = v;
    numberize(&mut v);
    crate::data::normalize_numbers(&mut v);
    std::fs::write(out, serde_json::to_string_pretty(&v).map_err(internal)? + "\n")?;
    Ok(())
}

fn unique_sheet_names(book: &Workbook) -> Vec<String> {
    let mut taken = std::collections::HashSet::new();
    book.sheets.iter().enumerate().map(|(i, s)| {
        let base: String = s.name.chars().map(|c| if "[]:*?/\\".contains(c) { '_' } else { c }).take(31).collect();
        let base = if base.trim().is_empty() { format!("Sheet{}", i + 1) } else { base };
        let mut cand = base.clone();
        let mut n = 2;
        while taken.contains(&cand.to_lowercase()) {
            let tail = format!("_{n}");
            cand = format!("{}{tail}", base.chars().take(31 - tail.len()).collect::<String>());
            n += 1;
        }
        taken.insert(cand.to_lowercase());
        cand
    }).collect()
}

pub fn xlsx(book: &Workbook, out: &Path) -> Result<()> {
    let mut wb = rust_xlsxwriter::Workbook::new();
    for (s, name) in book.sheets.iter().zip(unique_sheet_names(book)) {
        if s.rows.len() > MAX_ROWS { return Err(Error::new(ErrorCode::ETooLarge, format!("{} rows exceeds Excel's {MAX_ROWS}", s.rows.len()))); }
        let ws = wb.add_worksheet();
        ws.set_name(&name).map_err(internal)?;
        for (r, row) in s.rows.iter().enumerate() {
            if row.len() > MAX_COLUMNS { return Err(Error::new(ErrorCode::ETooLarge, format!("{} columns exceeds Excel's {MAX_COLUMNS}", row.len()))); }
            for (c, v) in row.iter().enumerate() {
                if v.is_empty() { continue; }
                if let Some(n) = as_number(v) { ws.write_number(r as u32, c as u16, n).map_err(internal)?; continue; }
                let clean: String = v.chars().filter(|ch| is_legal_xml_char(*ch as u32)).take(MAX_CELL_CHARACTERS).collect();
                ws.write_string(r as u32, c as u16, &clean).map_err(internal)?;
            }
        }
    }
    wb.save(out).map_err(|e| internal(format!("cannot write workbook: {e}")))
}

pub fn ods(book: &Workbook, out: &Path) -> Result<()> {
    let mut content = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<office:document-content xmlns:office=\"urn:oasis:names:tc:opendocument:xmlns:office:1.0\" xmlns:table=\"urn:oasis:names:tc:opendocument:xmlns:table:1.0\" xmlns:text=\"urn:oasis:names:tc:opendocument:xmlns:text:1.0\" office:version=\"1.2\"><office:body><office:spreadsheet>");
    for (s, name) in book.sheets.iter().zip(unique_sheet_names(book)) {
        content.push_str(&format!("<table:table table:name=\"{}\">", esc_xml(&name)));
        for row in &s.rows {
            content.push_str("<table:table-row>");
            for v in row {
                if v.is_empty() { content.push_str("<table:table-cell/>"); continue; }
                match as_number(v) {
                    Some(n) => content.push_str(&format!("<table:table-cell office:value-type=\"float\" office:value=\"{n}\"><text:p>{}</text:p></table:table-cell>", esc_xml(v))),
                    None => {
                        let lines: String = v.split('\n').map(|l| format!("<text:p>{}</text:p>", esc_xml(l))).collect();
                        content.push_str(&format!("<table:table-cell office:value-type=\"string\">{lines}</table:table-cell>"));
                    }
                }
            }
            content.push_str("</table:table-row>");
        }
        content.push_str("</table:table>");
    }
    content.push_str("</office:spreadsheet></office:body></office:document-content>");
    let mut z = zip::ZipWriter::new(std::fs::File::create(out)?);
    let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let deflated = zip::write::SimpleFileOptions::default();
    z.start_file("mimetype", stored).map_err(internal)?;
    z.write_all(b"application/vnd.oasis.opendocument.spreadsheet")?;
    z.start_file("content.xml", deflated).map_err(internal)?;
    z.write_all(content.as_bytes())?;
    z.start_file("META-INF/manifest.xml", deflated).map_err(internal)?;
    z.write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<manifest:manifest xmlns:manifest=\"urn:oasis:names:tc:opendocument:xmlns:manifest:1.0\" manifest:version=\"1.2\"><manifest:file-entry manifest:full-path=\"/\" manifest:media-type=\"application/vnd.oasis.opendocument.spreadsheet\"/><manifest:file-entry manifest:full-path=\"content.xml\" manifest:media-type=\"text/xml\"/></manifest:manifest>")?;
    z.finish().map_err(internal)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn number_detection_is_conservative() {
        for s in ["12", "-3.5", "0", "0.25"] { assert!(as_number(s).is_some(), "{s}"); }
        for s in ["007", "+1", "1e5", "1,5", "", "-", "1.", ".5", "1234567890123456"] { assert!(as_number(s).is_none(), "{s}"); }
    }
    #[test] fn sheet_names_are_unique_and_clean() {
        let b = Workbook { sheets: vec![Sheet { name: "a/b".into(), rows: vec![] }, Sheet { name: "A_B".into(), rows: vec![] }, Sheet { name: "".into(), rows: vec![] }] };
        assert_eq!(unique_sheet_names(&b), vec!["a_b", "A_B_2", "Sheet3"]);
    }
}
