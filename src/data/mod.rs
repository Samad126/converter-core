//! Pure-Rust data interchange: every source parses into one `serde_json::Value`
//! (key order preserved) and every target writes from it. Port of `data.service.ts`.
pub mod csv;
pub mod ini;
pub mod xml;

use crate::{Error, ErrorCode, Result};
use serde_json::{Map, Value};
use std::path::Path;

fn convert_failed(msg: impl Into<String>) -> Error { Error::new(ErrorCode::EConvertFailed, msg) }

fn not_tabular(label: &str, shape: &str) -> Error {
    Error::new(ErrorCode::ENotTabular, format!("This document is not {shape}, so it cannot become {label}."))
}

fn is_flat_record(v: &Value) -> bool {
    v.as_object().is_some_and(|o| o.values().all(|x| !x.is_object() && !x.is_array()))
}

fn tabular(v: &Value) -> Option<&Vec<Value>> {
    v.as_array().filter(|a| a.iter().all(is_flat_record))
}

pub(crate) fn table_to_records(rows: Vec<Vec<String>>) -> Value {
    let mut it = rows.into_iter();
    let Some(header) = it.next() else { return Value::Array(vec![]) };
    Value::Array(it.map(|row| {
        let mut rec = Map::new();
        for (i, key) in header.iter().enumerate() {
            rec.insert(key.clone(), Value::String(row.get(i).cloned().unwrap_or_default()));
        }
        Value::Object(rec)
    }).collect())
}

/// JS `String(value)` for scalars: `1.0` prints as `1`.
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Number(n) => match n.as_f64() {
            Some(f) if n.is_f64() && f.fract() == 0.0 && f.abs() < 1e21 => format!("{}", f as i128),
            _ => n.to_string(),
        },
        other => other.to_string(),
    }
}

fn records_to_table(records: &[Value]) -> Vec<Vec<String>> {
    let mut keys: Vec<&String> = vec![];
    for r in records { for k in r.as_object().unwrap().keys() { if !keys.contains(&k) { keys.push(k); } } }
    let mut rows = vec![keys.iter().map(|k| k.to_string()).collect::<Vec<_>>()];
    for r in records {
        let o = r.as_object().unwrap();
        rows.push(keys.iter().map(|k| o.get(*k).map(js_string).unwrap_or_default()).collect());
    }
    rows
}

/// JavaScript has one number type, so `25.0` reads back as `25`. Match that.
pub(crate) fn normalize_numbers(v: &mut Value) {
    match v {
        Value::Number(n) => if let Some(f) = n.as_f64() {
            if n.is_f64() && f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 { *v = Value::from(f as i64); }
        },
        Value::Array(a) => a.iter_mut().for_each(normalize_numbers),
        Value::Object(o) => o.values_mut().for_each(normalize_numbers),
        _ => {}
    }
}

pub fn parse_source(ext: &str, path: &Path) -> Result<Value> {
    let mut v = parse_raw(ext, path)?;
    normalize_numbers(&mut v);
    Ok(v)
}

fn parse_raw(ext: &str, path: &Path) -> Result<Value> {
    if ext == ".sqlite" { return parse_sqlite(path); }
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8(bytes).map_err(|_| convert_failed("The file is not valid UTF-8 text."))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    Ok(match ext {
        ".csv" => table_to_records(csv::parse_delimited(text, ',')),
        ".tsv" => table_to_records(csv::parse_delimited(text, '\t')),
        ".json" => serde_json::from_str(text).map_err(|e| convert_failed(format!("not valid JSON: {e}")))?,
        ".jsonl" => Value::Array(text.lines().filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l))
            .collect::<std::result::Result<_, _>>()
            .map_err(|e| convert_failed(format!("not valid JSON Lines: {e}")))?),
        ".yaml" | ".yml" => serde_yaml_ng::from_str(text).map_err(|e| convert_failed(format!("not valid YAML: {e}")))?,
        ".xml" => xml::parse(text)?,
        ".toml" => toml::from_str::<Value>(text).map_err(|e| convert_failed(format!("not valid TOML: {e}")))?,
        ".ini" => ini::parse(text),
        other => return Err(Error::new(ErrorCode::EInternal, format!("\"{other}\" has no data-engine reader"))),
    })
}

pub fn serialize_target(id: &str, v: &Value, out: &Path) -> Result<()> {
    if id == "sqlite" { return write_sqlite(v, out); }
    let text = match id {
        "csv" | "tsv" => {
            let recs = tabular(v).ok_or_else(|| not_tabular(&id.to_uppercase(),
                "a flat table of records (a top-level list of flat objects)"))?;
            csv::stringify_delimited(&records_to_table(recs), if id == "csv" { ',' } else { '\t' })
        }
        "json" => serde_json::to_string_pretty(v).unwrap() + "\n",
        "jsonl" => {
            let a = v.as_array().ok_or_else(|| not_tabular("JSONL", "a top-level array, one element per line"))?;
            if a.is_empty() { String::new() } else {
                a.iter().map(|i| serde_json::to_string(i).unwrap()).collect::<Vec<_>>().join("\n") + "\n"
            }
        }
        "yaml" => serde_yaml_ng::to_string(v).map_err(|e| convert_failed(format!("cannot write YAML: {e}")))?,
        "xml" => {
            let o = v.as_object().filter(|o| o.len() == 1 || (o.len() == 2 && o.contains_key("_declaration")))
                .ok_or_else(|| not_tabular("XML",
                    "a plain object with exactly one top-level key (the root element's tag name)"))?;
            xml::stringify(o)?
        }
        "toml" => {
            if !v.is_object() { return Err(convert_failed("cannot write TOML: a TOML document must be a table at the top level")); }
            toml::to_string(v).map_err(|e| convert_failed(format!("cannot write TOML: {e}")))? 
        }
        "ini" => ini::stringify(v.as_object().ok_or_else(|| not_tabular("INI",
            "a plain object (its keys become sections and settings)"))?),
        other => return Err(Error::new(ErrorCode::EInternal, format!("\"{other}\" has no data-engine writer"))),
    };
    std::fs::write(out, text)?;
    Ok(())
}

fn sql_err(e: rusqlite::Error) -> Error { convert_failed(format!("SQLite error: {e}")) }

fn parse_sqlite(path: &Path) -> Result<Value> {
    let db = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(sql_err)?;
    let table: String = db.query_row(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' LIMIT 1",
        [], |r| r.get(0)).map_err(|_| convert_failed("the SQLite file has no tables to read"))?;
    let mut stmt = db.prepare(&format!("SELECT * FROM \"{}\"", table.replace('"', "\"\""))).map_err(sql_err)?;
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let rows = stmt.query_map([], |r| {
        let mut o = Map::new();
        for (i, c) in cols.iter().enumerate() {
            use rusqlite::types::ValueRef::*;
            o.insert(c.clone(), match r.get_ref(i)? {
                Null => Value::Null, Integer(n) => n.into(),
                Real(f) => serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null),
                Text(t) => Value::String(String::from_utf8_lossy(t).into()),
                Blob(_) => Value::Null,
            });
        }
        Ok(Value::Object(o))
    }).map_err(sql_err)?.collect::<std::result::Result<Vec<_>, _>>().map_err(sql_err)?;
    Ok(Value::Array(rows))
}

fn write_sqlite(v: &Value, out: &Path) -> Result<()> {
    let recs = tabular(v).ok_or_else(|| not_tabular("SQLite",
        "a flat table of records (a top-level list of flat objects)"))?;
    let mut keys: Vec<&String> = vec![];
    for r in recs { for k in r.as_object().unwrap().keys() { if !keys.contains(&k) { keys.push(k); } } }
    let q = |k: &str| format!("\"{}\"", k.replace('"', "\"\""));
    let cols = keys.iter().map(|k| q(k)).collect::<Vec<_>>().join(", ");
    let _ = std::fs::remove_file(out);
    let mut db = rusqlite::Connection::open(out).map_err(sql_err)?;
    db.execute(&format!("CREATE TABLE \"data\" ({cols})"), []).map_err(sql_err)?;
    let tx = db.transaction().map_err(sql_err)?;
    {
        let marks = keys.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        let mut ins = tx.prepare(&format!("INSERT INTO \"data\" ({cols}) VALUES ({marks})")).map_err(sql_err)?;
        for r in recs {
            let o = r.as_object().unwrap();
            let params: Vec<rusqlite::types::Value> = keys.iter().map(|k| match o.get(*k) {
                None | Some(Value::Null) => rusqlite::types::Value::Null,
                Some(Value::Bool(b)) => rusqlite::types::Value::Integer(*b as i64),
                Some(Value::Number(n)) => n.as_i64().map(rusqlite::types::Value::Integer)
                    .unwrap_or_else(|| rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0))),
                Some(Value::String(s)) => rusqlite::types::Value::Text(s.clone()),
                Some(other) => rusqlite::types::Value::Text(other.to_string()),
            }).collect();
            ins.execute(rusqlite::params_from_iter(params)).map_err(sql_err)?;
        }
    }
    tx.commit().map_err(sql_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(n: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("data-test-{}", std::process::id())); std::fs::create_dir_all(&d).unwrap(); d.join(n)
    }
    #[test]
    fn csv_to_json_records_in_header_order() {
        let p = tmp("a.csv"); std::fs::write(&p, "b,a\n1,2\n3,4\n").unwrap();
        let v = parse_source(".csv", &p).unwrap();
        assert_eq!(serde_json::to_string(&v).unwrap(), r#"[{"b":"1","a":"2"},{"b":"3","a":"4"}]"#);
    }
    #[test]
    fn json_nested_cannot_become_csv() {
        let v: Value = serde_json::from_str(r#"[{"a":{"b":1}}]"#).unwrap();
        let e = serialize_target("csv", &v, &tmp("x.csv")).unwrap_err();
        assert_eq!(e.code, ErrorCode::ENotTabular);
    }
    #[test]
    fn number_formats_like_js() {
        let v: Value = serde_json::from_str(r#"[{"n":1.0,"m":2.5,"b":true,"z":null}]"#).unwrap();
        let p = tmp("n.csv"); serialize_target("csv", &v, &p).unwrap();
        assert_eq!(std::fs::read_to_string(p).unwrap(), "n,m,b,z\r\n1,2.5,true,\r\n");
    }
    #[test]
    fn sqlite_roundtrip() {
        let v: Value = serde_json::from_str(r#"[{"a":1,"b":"x"},{"a":2,"b":null}]"#).unwrap();
        let p = tmp("r.sqlite"); serialize_target("sqlite", &v, &p).unwrap();
        assert_eq!(parse_source(".sqlite", &p).unwrap(), v);
    }
    #[test]
    fn whole_floats_read_as_integers_like_js() {
        let p = tmp("f.json"); std::fs::write(&p, r#"{"a":25.0,"b":2.5}"#).unwrap();
        assert_eq!(serde_json::to_string(&parse_source(".json", &p).unwrap()).unwrap(), r#"{"a":25,"b":2.5}"#);
    }
    #[test]
    fn toml_requires_table() {
        let v: Value = serde_json::from_str("[1,2]").unwrap();
        assert!(serialize_target("toml", &v, &tmp("t.toml")).is_err());
    }
    #[test]
    fn xml_needs_single_root() {
        let v: Value = serde_json::from_str(r#"{"a":{}, "b":{}}"#).unwrap();
        assert_eq!(serialize_target("xml", &v, &tmp("x.xml")).unwrap_err().code, ErrorCode::ENotTabular);
    }
    #[test]
    fn yaml_json_roundtrip() {
        let p = tmp("y.yaml"); std::fs::write(&p, "a: 1\nb:\n  - x\n  - y\n").unwrap();
        let v = parse_source(".yaml", &p).unwrap();
        let o = tmp("y.json"); serialize_target("json", &v, &o).unwrap();
        assert_eq!(parse_source(".json", &o).unwrap(), v);
    }
}
