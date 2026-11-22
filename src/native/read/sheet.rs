//! Spreadsheets (`.xlsx .xlsm .xlsb .xls .ods ...`) → `Workbook`, through `calamine`.
use super::bad;
use crate::native::model::*;
use crate::{Config, Result};
use calamine::{open_workbook_auto, Data, Reader};
use std::path::Path;

fn cell_text(d: &Data) -> String {
    match d {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Float(f) => float_text(*f),
        Data::Int(i) => i.to_string(),
        Data::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
        Data::DateTime(dt) => match dt.as_datetime() {
            Some(n) if dt.as_f64().fract() == 0.0 => n.format("%Y-%m-%d").to_string(),
            Some(n) => n.format("%Y-%m-%d %H:%M:%S").to_string(),
            None => float_text(dt.as_f64()),
        },
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(e) => format!("{e}"),
    }
}

fn float_text(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 { format!("{}", f as i64) } else { format!("{f}") }
}

pub fn read(path: &Path, cfg: &Config) -> Result<Workbook> {
    let mut wb = open_workbook_auto(path).map_err(|e| bad(format!("not a readable spreadsheet: {e}")))?;
    let mut out = Workbook::default();
    let mut cells = 0usize;
    for name in wb.sheet_names() {
        let range = match wb.worksheet_range(&name) { Ok(r) => r, Err(_) => continue };
        let (start_row, start_col) = range.start().map(|(r, c)| (r as usize, c as usize)).unwrap_or((0, 0));
        let mut rows: Vec<Vec<String>> = vec![vec![]; start_row];
        for row in range.rows() {
            cells += row.len();
            if cells > cfg.max_table_cells.max(5_000_000) {
                return Err(crate::Error::new(crate::ErrorCode::ETooLarge, "The spreadsheet has too many cells."));
            }
            let mut r = vec![String::new(); start_col];
            r.extend(row.iter().map(cell_text));
            rows.push(r);
        }
        while rows.last().is_some_and(|r| r.iter().all(String::is_empty)) { rows.pop(); }
        out.sheets.push(Sheet { name, rows });
    }
    if out.sheets.is_empty() { return Err(bad("The spreadsheet has no sheets.")); }
    Ok(out)
}
