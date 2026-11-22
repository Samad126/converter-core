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

pub struct Parsed { pub headers: Vec<String>, pub text: Option<String>, pub html: Option<String> }

pub fn parse(bytes: &[u8]) -> Result<Parsed> {
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

pub fn render_text(bytes: &[u8]) -> Result<String> { Ok(text_of(&parse(bytes)?)) }

pub fn text_of(p: &Parsed) -> String {
    let body = p.text.clone().unwrap_or_else(|| "(no text content)".into());
    let mut lines = p.headers.clone();
    lines.push(String::new());
    lines.push(body);
    lines.join("\n")
}

pub fn render_html(bytes: &[u8]) -> Result<String> { Ok(html_of(&parse(bytes)?)) }

pub fn html_of(p: &Parsed) -> String {
    let header = p.headers.iter().map(|l| format!("<p>{}</p>", escape_html(l))).collect::<Vec<_>>().join("\n");
    let body = p.html.clone().unwrap_or_else(|| format!("<pre>{}</pre>", escape_html(p.text.as_deref().unwrap_or("(no text content)"))));
    format!("<!DOCTYPE html>\n<html><body>\n{header}\n<hr>\n{body}\n</body></html>\n")
}

/// Outlook `.msg` (an OLE compound file of MAPI properties) → the same shape as a parsed `.eml`.
pub fn parse_msg(bytes: &[u8]) -> Result<Parsed> {
    use std::io::Read;
    let bad = |m: &str| Error::new(ErrorCode::EConvertFailed, m.to_string());
    let mut cf = cfb::CompoundFile::open(std::io::Cursor::new(bytes)).map_err(|e| bad(&format!("not an Outlook message: {e}")))?;
    let mut stream = |name: &str| -> Option<Vec<u8>> {
        let mut s = cf.open_stream(format!("/{name}")).ok()?;
        let mut v = vec![];
        (&mut s).take(64 * 1024 * 1024).read_to_end(&mut v).ok()?;
        Some(v)
    };
    let decode = |raw: Vec<u8>, unicode: bool| -> String {
        if unicode {
            let u: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&u).trim_end_matches('\0').to_string()
        } else {
            let s = String::from_utf8(raw.clone()).unwrap_or_else(|_| raw.iter().map(|b| *b as char).collect());
            s.trim_end_matches('\0').to_string()
        }
    };
    // Sent time: FILETIME property 0x0039 (client submit) or 0x0E06 (delivery) in the properties stream.
    let date = stream("__properties_version1.0").and_then(|p| {
        p.get(32..)?.chunks_exact(16).find_map(|e| {
            let (ty, id) = (u16::from_le_bytes([e[0], e[1]]), u16::from_le_bytes([e[2], e[3]]));
            (ty == 0x0040 && (id == 0x0039 || id == 0x0E06)).then(|| u64::from_le_bytes(e[8..16].try_into().unwrap()))
        })
    }).filter(|ft| *ft > 116_444_736_000_000_000).map(|ft| ((ft / 10_000_000) as i64) - 11_644_473_600);
    let mut get = |tag: &str| -> Option<String> {
        for (ty, uni) in [("001F", true), ("001E", false), ("0102", false)] {
            if let Some(raw) = stream(&format!("__substg1.0_{tag}{ty}")) { let t = decode(raw, uni); if !t.is_empty() { return Some(t); } }
        }
        None
    };
    let subject = get("0037");
    let from = match (get("0C1A"), get("5D01").or_else(|| get("0C1F"))) {
        (Some(n), Some(a)) if n != a => Some(format!("\"{}\" <{a}>", n.replace('"', "\\\""))),
        (n, a) => a.or(n),
    };
    let (to, cc) = (get("0E04"), get("0E03"));
    let mut headers = vec![];
    if let Some(f) = from { headers.push(format!("From: {f}")); }
    if let Some(t) = to { headers.push(format!("To: {}", t.replace(';', ","))); }
    if let Some(c) = cc { headers.push(format!("Cc: {}", c.replace(';', ","))); }
    if let Some(s) = subject.filter(|s| !s.is_empty()) { headers.push(format!("Subject: {s}")); }
    if let Some(d) = date { headers.push(format!("Date: {}", utc_string(d))); }
    let (text, html) = (get("1000").map(|t| t.replace("\r\n", "\n")), get("1013"));
    if headers.is_empty() && text.is_none() && html.is_none() { return Err(bad("This Outlook message has no readable content.")); }
    Ok(Parsed { headers, text, html })
}

/// `.mbox`: messages separated by lines starting with `From `.
pub fn parse_mbox(bytes: &[u8]) -> Result<Vec<Parsed>> {
    let text = String::from_utf8_lossy(bytes);
    let (mut messages, mut cur): (Vec<String>, Option<String>) = (vec![], None);
    let mut prev_blank = true;
    for line in text.split_inclusive('\n') {
        if prev_blank && line.starts_with("From ") {
            if let Some(m) = cur.take() { messages.push(m); }
            cur = Some(String::new());
            prev_blank = false;
            continue;
        }
        prev_blank = line.trim_end_matches(['\r', '\n']).is_empty();
        if let Some(m) = &mut cur { m.push_str(line.strip_prefix('>').filter(|r| r.starts_with("From ")).unwrap_or(line)); }
    }
    if let Some(m) = cur { messages.push(m); }
    if messages.len() > 5000 { return Err(Error::new(ErrorCode::ETooLarge, "The mailbox has too many messages.")); }
    let parsed: Vec<Parsed> = messages.iter().filter_map(|m| parse(m.as_bytes()).ok()).collect();
    if parsed.is_empty() { return Err(Error::new(ErrorCode::EConvertFailed, "No messages were found in this mailbox.")); }
    Ok(parsed)
}

/// Several messages as one text (or HTML) document.
pub fn join_text(ps: &[Parsed]) -> String { ps.iter().map(text_of).collect::<Vec<_>>().join("\n\n----------------------------------------\n\n") }
pub fn join_html(ps: &[Parsed]) -> String {
    let bodies: Vec<String> = ps.iter().map(|p| { let h = html_of(p); h.split_once("<body>").and_then(|(_, b)| b.rsplit_once("</body>")).map(|(b, _)| b.to_string()).unwrap_or(h) }).collect();
    format!("<!DOCTYPE html>\n<html><body>\n{}\n</body></html>\n", bodies.join("\n<hr>\n<hr>\n"))
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
