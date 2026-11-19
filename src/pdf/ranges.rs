//! Page selections like "2,5-7,10" → zero-based indices. Port of `page-ranges.ts`.
use crate::{Error, ErrorCode, Result};

fn bad(msg: String) -> Error { Error::new(ErrorCode::EBadPageRange, msg) }

pub fn parse_selection(spec: &str, page_count: usize) -> Result<Vec<usize>> {
    let spec = spec.trim();
    if spec.is_empty() { return Err(bad("No pages were specified.".into())); }
    let mut out = vec![];
    for raw in spec.split(',') {
        let token = raw.trim();
        let (a, b) = match token.split_once('-') { Some((a, b)) => (a, Some(b)), None => (token, None) };
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
        if !digits(a) || b.is_some_and(|b| !digits(b)) {
            return Err(bad(format!("\"{token}\" is not a valid page number or range. Use page numbers and ranges like \"2,5-7,10\".")));
        }
        let start: usize = a.parse().map_err(|_| bad(format!("\"{token}\" is not a valid page number.")))?;
        let end: usize = match b { Some(b) => b.parse().map_err(|_| bad(format!("\"{token}\" is not a valid page number.")))?, None => start };
        if end < start { return Err(bad(format!("\"{token}\" is not a valid range: it goes backwards."))); }
        if end > page_count.max(start) || start < 1 || end > page_count {
            let page = if start < 1 { start } else { end };
            return Err(bad(format!("Page {page} does not exist in this {page_count}-page document.")));
        }
        out.extend((start..=end).map(|p| p - 1));
    }
    Ok(out)
}

pub fn is_permutation(indices: &[usize], count: usize) -> bool {
    indices.len() == count && indices.iter().collect::<std::collections::HashSet<_>>().len() == count
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn ranges_and_lists() {
        assert_eq!(parse_selection("1, 3-4,2", 5).unwrap(), vec![0, 2, 3, 1]);
    }
    #[test] fn errors() {
        for (s, n) in [("", 3), ("a", 3), ("2-1", 3), ("0", 3), ("4", 3), ("1-", 3), ("-1", 3), ("1,,2", 3)] {
            assert_eq!(parse_selection(s, n).unwrap_err().code, ErrorCode::EBadPageRange, "{s}");
        }
        assert!(parse_selection("9", 3).unwrap_err().message.contains("does not exist in this 3-page"));
    }
    #[test] fn permutation() {
        assert!(is_permutation(&[2, 0, 1], 3));
        assert!(!is_permutation(&[0, 0, 1], 3));
        assert!(!is_permutation(&[0, 1], 3));
    }
}
