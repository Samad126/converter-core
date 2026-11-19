//! INI reader/writer matching the `ini` npm package's shape: `[a.b]` nests,
//! `key[]=v` makes arrays, true/false/null become JSON values. Never fails (permissive).
use serde_json::{Map, Value};

fn scalar(v: &str) -> Value {
    match v { "true" => Value::Bool(true), "false" => Value::Bool(false), "null" => Value::Null,
              s => Value::String(s.to_string()) }
}

fn set_path<'a>(root: &'a mut Map<String, Value>, path: &[String]) -> &'a mut Map<String, Value> {
    let mut cur = root;
    for p in path {
        let e = cur.entry(p.clone()).or_insert_with(|| Value::Object(Map::new()));
        if !e.is_object() { *e = Value::Object(Map::new()); }
        cur = e.as_object_mut().unwrap();
    }
    cur
}

pub fn parse(text: &str) -> Value {
    let mut root = Map::new();
    let mut section: Vec<String> = vec![];
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') { continue; }
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].split('.').map(|s| s.trim().to_string()).collect();
            set_path(&mut root, &section);
            continue;
        }
        let (k, v) = match line.split_once('=') {
            Some((k, v)) => (k.trim(), scalar(v.trim().trim_matches('"'))),
            None => (line, Value::Bool(true)),
        };
        let target = set_path(&mut root, &section);
        if let Some(base) = k.strip_suffix("[]") {
            let e = target.entry(base.to_string()).or_insert_with(|| Value::Array(vec![]));
            if let Value::Array(a) = e { a.push(v); }
        } else { target.insert(k.to_string(), v); }
    }
    Value::Object(root)
}

fn val_str(v: &Value) -> String {
    match v { Value::String(s) => s.clone(), Value::Null => "null".into(), other => other.to_string() }
}

fn write_obj(m: &Map<String, Value>, path: &str, out: &mut String) {
    for (k, v) in m {
        match v {
            Value::Object(_) => {}
            Value::Array(a) => for item in a { out.push_str(&format!("{k}[]={}\n", val_str(item))); },
            other => out.push_str(&format!("{k}={}\n", val_str(other))),
        }
    }
    for (k, v) in m {
        if let Value::Object(child) = v {
            let p = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
            if !out.is_empty() { out.push('\n'); }
            out.push_str(&format!("[{p}]\n"));
            write_obj(child, &p, out);
        }
    }
}

pub fn stringify(m: &Map<String, Value>) -> String {
    let mut out = String::new();
    write_obj(m, "", &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sections_arrays_and_bools() {
        let v = parse("name=x\nflag=true\n[db.main]\nport=5432\nhost[]=a\nhost[]=b\n");
        assert_eq!(v["flag"], Value::Bool(true));
        assert_eq!(v["db"]["main"]["port"], Value::String("5432".into()));
        assert_eq!(v["db"]["main"]["host"].as_array().unwrap().len(), 2);
        let back = parse(&stringify(v.as_object().unwrap()));
        assert_eq!(back, v);
    }
}
