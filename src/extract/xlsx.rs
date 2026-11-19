use super::docx_tables::is_legal_xml_char;
use crate::{Error, ErrorCode, Result};
use rust_xlsxwriter::Workbook;

const MAX_SHEET_NAME: usize = 31;
pub const MAX_CELL_CHARACTERS: usize = 32_767;
pub const MAX_ROWS: usize = 1_048_576;
pub const MAX_COLUMNS: usize = 16_384;

fn too_large(msg: String) -> Error { Error::new(ErrorCode::ETooLarge, msg) }

/// `Table_1`, `Table_2`, … (unique case-insensitively, ≤31 chars).
pub fn sheet_name_for(index: usize, taken: &mut std::collections::HashSet<String>) -> String {
    let base: String = format!("Table_{index}").chars()
        .map(|c| if "[]:*?/\\".contains(c) { '_' } else { c }).take(MAX_SHEET_NAME).collect();
    let mut cand = base.clone();
    let mut suffix = 2;
    while taken.contains(&cand.to_lowercase()) {
        let tail = format!("_{suffix}");
        cand = format!("{}{tail}", base.chars().take(MAX_SHEET_NAME - tail.len()).collect::<String>());
        suffix += 1;
    }
    taken.insert(cand.to_lowercase());
    cand
}

pub fn build(sheets: &[(String, Vec<Vec<String>>)], out: &std::path::Path) -> Result<()> {
    let mut wb = Workbook::new();
    for (name, rows) in sheets {
        if rows.len() > MAX_ROWS { return Err(too_large(format!("{} rows exceeds Excel's {MAX_ROWS}", rows.len()))); }
        let ws = wb.add_worksheet();
        ws.set_name(name).map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))?;
        for (r, row) in rows.iter().enumerate() {
            if row.len() > MAX_COLUMNS { return Err(too_large(format!("{} columns exceeds Excel's {MAX_COLUMNS}", row.len()))); }
            for (c, v) in row.iter().enumerate() {
                if v.is_empty() { continue; }
                let clean: String = v.chars().filter(|ch| is_legal_xml_char(*ch as u32)).collect();
                if clean.chars().count() > MAX_CELL_CHARACTERS {
                    return Err(too_large(format!("cell text of {} characters exceeds Excel's {MAX_CELL_CHARACTERS}", clean.chars().count())));
                }
                ws.write_string(r as u32, c as u16, &clean).map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))?;
            }
        }
    }
    wb.save(out).map_err(|e| Error::new(ErrorCode::EInternal, format!("cannot write workbook: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn sheet_names_unique() {
        let mut t = Default::default();
        assert_eq!(sheet_name_for(1, &mut t), "Table_1");
        assert_eq!(sheet_name_for(1, &mut t), "Table_1_2");
    }
}
