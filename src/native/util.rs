//! Small helpers shared by the built-in writers.

pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

pub fn esc_xml(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"), '<' => o.push_str("&lt;"), '>' => o.push_str("&gt;"), '"' => o.push_str("&quot;"),
            c if crate::extract::docx_tables::is_legal_xml_char(c as u32) => o.push(c),
            _ => {}
        }
    }
    o
}

pub fn ext_for_mime(mime: &str) -> &'static str {
    match mime { "image/png" => "png", "image/jpeg" => "jpg", "image/gif" => "gif", "image/bmp" => "bmp", "image/webp" => "webp",
        "image/tiff" => "tiff", "image/svg+xml" => "svg", _ => "bin" }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn base64_pads() {
        assert_eq!(base64(b""), ""); assert_eq!(base64(b"f"), "Zg=="); assert_eq!(base64(b"fo"), "Zm8="); assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }
}
