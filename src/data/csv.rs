//! Port of `backend/src/lib/csv.ts` (RFC-4180-ish; ragged rows allowed).
pub fn parse_delimited(text: &str, delim: char) -> Vec<Vec<String>> {
    let chars: Vec<char> = text.chars().collect();
    let (mut rows, mut row, mut field) = (Vec::new(), Vec::new(), String::new());
    let (mut in_quotes, mut saw_any) = (false, false);
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if in_quotes {
            if ch == '"' {
                if chars.get(i + 1) == Some(&'"') { field.push('"'); i += 2; continue; }
                in_quotes = false; i += 1; continue;
            }
            field.push(ch); i += 1; continue;
        }
        if ch == '"' && field.is_empty() { in_quotes = true; saw_any = true; i += 1; continue; }
        if ch == delim { row.push(std::mem::take(&mut field)); saw_any = true; i += 1; continue; }
        if ch == '\r' && chars.get(i + 1) == Some(&'\n') {
            row.push(std::mem::take(&mut field)); rows.push(std::mem::take(&mut row)); saw_any = false; i += 2; continue;
        }
        if ch == '\n' {
            row.push(std::mem::take(&mut field)); rows.push(std::mem::take(&mut row)); saw_any = false; i += 1; continue;
        }
        field.push(ch); saw_any = true; i += 1;
    }
    if saw_any || !field.is_empty() { row.push(field); rows.push(row); }
    rows
}

pub fn stringify_delimited(rows: &[Vec<String>], delim: char) -> String {
    if rows.is_empty() { return String::new(); }
    let quote = |f: &str| -> String {
        if f.contains(delim) || f.contains('"') || f.contains('\n') || f.contains('\r') {
            format!("\"{}\"", f.replace('"', "\"\""))
        } else { f.to_string() }
    };
    let mut out = rows.iter()
        .map(|r| r.iter().map(|f| quote(f)).collect::<Vec<_>>().join(&delim.to_string()))
        .collect::<Vec<_>>().join("\r\n");
    out.push_str("\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_fields_and_escapes() {
        let r = parse_delimited("a,b\r\n\"x,1\",\"he said \"\"hi\"\"\"\r\n", ',');
        assert_eq!(r, vec![vec!["a", "b"], vec!["x,1", "he said \"hi\""]]);
    }
    #[test]
    fn trailing_row_without_newline() {
        assert_eq!(parse_delimited("a,b\n1,2", ',').len(), 2);
    }
    #[test]
    fn roundtrip_quotes() {
        let rows = vec![vec!["a,b".to_string(), "c\"d".to_string()]];
        assert_eq!(parse_delimited(&stringify_delimited(&rows, ','), ','), rows);
    }
}
