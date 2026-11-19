//! `.eml` → txt / html. Port of `email.service.ts`.
use crate::{Error, ErrorCode, Result};
use mail_parser::{Address, MessageParser, PartType};

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn addr_text(a: &Address) -> String {
    a.iter().map(|x| match (x.name(), x.address()) {
        (Some(n), Some(ad)) => format!("\"{}\" <{ad}>", n.replace('"', "\\\"")),
        (None, Some(ad)) => ad.to_string(),
        (Some(n), None) => n.to_string(),
        _ => String::new(),
    }).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ")
}

/// `Tue, 02 Jan 2024 10:00:00 GMT`, like JavaScript's `toUTCString()`.
fn utc_string(ts: i64) -> String {
    let (days, rem) = (ts.div_euclid(86_400), ts.rem_euclid(86_400));
    let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize];
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let mon = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"][(m - 1) as usize];
    format!("{wd}, {d:02} {mon} {y:04} {:02}:{:02}:{:02} GMT", rem / 3600, rem % 3600 / 60, rem % 60)
}

struct Parsed { headers: Vec<String>, text: Option<String>, html: Option<String> }

fn parse(bytes: &[u8]) -> Result<Parsed> {
    let msg = MessageParser::default().parse(bytes)
        .filter(|m| m.from().is_some() || m.subject().is_some() || m.date().is_some() || m.to().is_some() || m.body_text(0).is_some())
        .ok_or_else(|| Error::new(ErrorCode::EConvertFailed, "not a valid email: no headers or body found"))?;
    let mut headers = vec![];
    if let Some(t) = msg.from().map(addr_text).filter(|t| !t.is_empty()) { headers.push(format!("From: {t}")); }
    if let Some(t) = msg.to().map(addr_text).filter(|t| !t.is_empty()) { headers.push(format!("To: {t}")); }
    if let Some(s) = msg.subject().filter(|s| !s.is_empty()) { headers.push(format!("Subject: {s}")); }
    if let Some(d) = msg.date() { headers.push(format!("Date: {}", utc_string(d.to_timestamp()))); }
    let has_real_html = msg.parts.iter().any(|p| matches!(p.body, PartType::Html(_)));
    Ok(Parsed {
        headers,
        text: msg.body_text(0).map(|c| c.replace("\r\n", "\n")),
        html: if has_real_html { msg.body_html(0).map(|c| c.into_owned()) } else { None },
    })
}

pub fn render_text(bytes: &[u8]) -> Result<String> {
    let p = parse(bytes)?;
    let body = p.text.unwrap_or_else(|| "(no text content)".into());
    let mut lines = p.headers;
    lines.push(String::new());
    lines.push(body);
    Ok(lines.join("\n"))
}

pub fn render_html(bytes: &[u8]) -> Result<String> {
    let p = parse(bytes)?;
    let header = p.headers.iter().map(|l| format!("<p>{}</p>", escape_html(l))).collect::<Vec<_>>().join("\n");
    let body = p.html.unwrap_or_else(|| format!("<pre>{}</pre>", escape_html(&p.text.unwrap_or_else(|| "(no text content)".into()))));
    Ok(format!("<!DOCTYPE html>\n<html><body>\n{header}\n<hr>\n{body}\n</body></html>\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    const EML: &[u8] = b"From: Ann <ann@x.com>\r\nTo: bob@y.com, Cy <cy@z.org>\r\nSubject: Hi\r\nDate: Tue, 02 Jan 2024 10:00:00 +0000\r\nContent-Type: text/plain\r\n\r\nHello <there>\r\n";
    #[test] fn text_has_headers_and_body() {
        let t = render_text(EML).unwrap();
        assert!(t.starts_with("From: \"Ann\" <ann@x.com>\nTo: bob@y.com, \"Cy\" <cy@z.org>\nSubject: Hi\nDate: Tue, 02 Jan 2024 10:00:00 GMT\n\nHello <there>"), "{t}");
    }
    #[test] fn html_escapes_plain_body() {
        let h = render_html(EML).unwrap();
        assert!(h.contains("<pre>Hello &lt;there&gt;"), "{h}");
        assert!(h.contains("<p>From: &quot;Ann&quot; &lt;ann@x.com&gt;</p>"));
    }
    #[test] fn weekday_and_date() { assert_eq!(utc_string(1_704_189_600), "Tue, 02 Jan 2024 10:00:00 GMT"); }
}
