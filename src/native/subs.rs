//! Subtitle formats: SubRip, WebVTT, YouTube SBV, LRC lyrics and (Advanced) SubStation Alpha, converted between each
//! other through a list of timed cues.
use crate::{Error, ErrorCode, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Cue { pub start_ms: u64, pub end_ms: u64, pub text: String }

pub const EXTENSIONS: &[&str] = &[".srt", ".vtt", ".sbv", ".lrc", ".ass", ".ssa"];

fn bad(msg: impl Into<String>) -> Error { Error::new(ErrorCode::EConvertFailed, msg) }

/// `1:02:03,456`, `02:03.456`, `3.5` (seconds) → milliseconds.
fn parse_time(s: &str) -> Option<u64> {
    let s = s.trim().replace(',', ".");
    let parts: Vec<&str> = s.split(':').collect();
    let secs = |p: &str| -> Option<f64> { p.parse::<f64>().ok() };
    let total = match parts.as_slice() {
        [h, m, sec] => h.parse::<f64>().ok()? * 3600.0 + m.parse::<f64>().ok()? * 60.0 + secs(sec)?,
        [m, sec] => m.parse::<f64>().ok()? * 60.0 + secs(sec)?,
        [sec] => secs(sec)?,
        _ => return None,
    };
    (total >= 0.0).then(|| (total * 1000.0).round() as u64)
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let (mut in_tag, mut in_brace) = (false, false);
    for c in s.chars() {
        match c { '<' => in_tag = true, '>' if in_tag => in_tag = false, '{' => in_brace = true, '}' if in_brace => in_brace = false, _ if in_tag || in_brace => {}, c => out.push(c) }
    }
    out
}

fn parse_blocks(text: &str, vtt: bool) -> Vec<Cue> {
    let mut cues = vec![];
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    for block in text.split("\n\n") {
        let lines: Vec<&str> = block.lines().filter(|l| !l.trim().is_empty()).collect();
        let Some(i) = lines.iter().position(|l| l.contains("-->")) else { continue };
        let (a, rest) = lines[i].split_once("-->").unwrap();
        // WebVTT cue settings follow the end time ("00:01.000 --> 00:02.000 align:start").
        let end = rest.split_whitespace().next().unwrap_or("");
        let (Some(s), Some(e)) = (parse_time(a), parse_time(end)) else { continue };
        let body = lines[i + 1..].join("\n");
        let _ = vtt;
        cues.push(Cue { start_ms: s, end_ms: e, text: strip_tags(&body).trim().to_string() });
    }
    cues
}

fn parse_sbv(text: &str) -> Vec<Cue> {
    let text = text.replace("\r\n", "\n");
    let mut cues = vec![];
    for block in text.split("\n\n") {
        let mut lines = block.lines().filter(|l| !l.trim().is_empty());
        let Some(first) = lines.next() else { continue };
        let Some((a, b)) = first.split_once(',') else { continue };
        let (Some(s), Some(e)) = (parse_time(a), parse_time(b)) else { continue };
        cues.push(Cue { start_ms: s, end_ms: e, text: lines.collect::<Vec<_>>().join("\n").trim().to_string() });
    }
    cues
}

fn parse_lrc(text: &str) -> Vec<Cue> {
    let mut stamps: Vec<(u64, String)> = vec![];
    for line in text.lines() {
        let mut rest = line.trim();
        let mut times = vec![];
        while let Some(r) = rest.strip_prefix('[') {
            let Some((tag, after)) = r.split_once(']') else { break };
            match parse_time(tag) { Some(t) if tag.contains(':') && tag.chars().next().is_some_and(|c| c.is_ascii_digit()) => times.push(t), _ => {} }
            rest = after.trim_start();
        }
        for t in times { stamps.push((t, strip_tags(rest).trim().to_string())); }
    }
    stamps.sort_by_key(|s| s.0);
    (0..stamps.len()).map(|i| Cue { start_ms: stamps[i].0, end_ms: stamps.get(i + 1).map_or(stamps[i].0 + 3000, |n| n.0), text: stamps[i].1.clone() }).collect()
}

fn parse_ass(text: &str) -> Vec<Cue> {
    let (mut in_events, mut cols): (bool, Vec<String>) = (false, vec![]);
    let mut cues = vec![];
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') { in_events = l.eq_ignore_ascii_case("[events]"); continue; }
        if !in_events { continue; }
        if let Some(f) = l.strip_prefix("Format:") { cols = f.split(',').map(|c| c.trim().to_lowercase()).collect(); continue; }
        let Some(d) = l.strip_prefix("Dialogue:") else { continue };
        let n = cols.len().max(1);
        let fields: Vec<&str> = d.trim_start().splitn(n, ',').collect();
        let get = |name: &str| cols.iter().position(|c| c == name).and_then(|i| fields.get(i).copied());
        let (Some(s), Some(e), Some(t)) = (get("start").and_then(parse_time), get("end").and_then(parse_time), get("text")) else { continue };
        cues.push(Cue { start_ms: s, end_ms: e, text: strip_tags(&t.replace("\\N", "\n").replace("\\n", "\n")).trim().to_string() });
    }
    cues
}

pub fn parse(ext: &str, bytes: &[u8]) -> Result<Vec<Cue>> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let cues = match ext {
        ".srt" => parse_blocks(text, false),
        ".vtt" => parse_blocks(text, true),
        ".sbv" => parse_sbv(text),
        ".lrc" => parse_lrc(text),
        ".ass" | ".ssa" => parse_ass(text),
        other => return Err(bad(format!("\"{other}\" is not a subtitle format"))),
    };
    if cues.is_empty() { return Err(bad("No subtitle cues were found in this file.")); }
    Ok(cues)
}

fn ts(ms: u64, sep: char, hours: bool) -> String {
    let (h, m, s, f) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
    if hours || h > 0 { format!("{h:02}:{m:02}:{s:02}{sep}{f:03}") } else { format!("{m:02}:{s:02}{sep}{f:03}") }
}

pub fn write(target: &str, cues: &[Cue]) -> Result<String> {
    let mut out = String::new();
    match target {
        "srt" => for (i, c) in cues.iter().enumerate() { out.push_str(&format!("{}\n{} --> {}\n{}\n\n", i + 1, ts(c.start_ms, ',', true), ts(c.end_ms, ',', true), c.text)); },
        "vtt" => { out.push_str("WEBVTT\n\n"); for c in cues { out.push_str(&format!("{} --> {}\n{}\n\n", ts(c.start_ms, '.', true), ts(c.end_ms, '.', true), c.text)); } }
        "sbv" => for c in cues {
            let t = |ms: u64| format!("{}:{:02}:{:02}.{:03}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000);
            out.push_str(&format!("{},{}\n{}\n\n", t(c.start_ms), t(c.end_ms), c.text));
        },
        "lrc" => for c in cues { out.push_str(&format!("[{:02}:{:02}.{:02}]{}\n", c.start_ms / 60_000, c.start_ms / 1000 % 60, c.start_ms % 1000 / 10, c.text.replace('\n', " "))); },
        "ass" | "ssa" => {
            let ass = target == "ass";
            let cs = |ms: u64| format!("{}:{:02}:{:02}.{:02}", ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60, ms % 1000 / 10);
            out.push_str(if ass { "[Script Info]\nScriptType: v4.00+\nPlayResX: 384\nPlayResY: 288\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,16,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n" }
                else { "[Script Info]\nScriptType: v4.00\nPlayResX: 384\nPlayResY: 288\n\n[V4 Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, TertiaryColour, BackColour, Bold, Italic, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, AlphaLevel, Encoding\nStyle: Default,Arial,16,16777215,255,0,0,0,0,1,1,0,2,10,10,10,0,1\n\n[Events]\nFormat: Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n" });
            for c in cues { out.push_str(&format!("Dialogue: {},{},{},Default,,0,0,0,,{}\n", if ass { "0" } else { "Marked=0" }, cs(c.start_ms), cs(c.end_ms), c.text.replace('\n', "\\N"))); }
        }
        other => return Err(bad(format!("\"{other}\" is not a subtitle target"))),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    const SRT: &str = "1\n00:00:01,000 --> 00:00:02,500\nHello <i>there</i>\n\n2\n00:01:02,000 --> 00:01:04,250\nTwo\nlines\n\n";
    fn cues() -> Vec<Cue> { parse(".srt", SRT.as_bytes()).unwrap() }
    #[test] fn srt_parses_with_tags_stripped() {
        let c = cues();
        assert_eq!(c, vec![Cue { start_ms: 1000, end_ms: 2500, text: "Hello there".into() }, Cue { start_ms: 62_000, end_ms: 64_250, text: "Two\nlines".into() }]);
    }
    #[test] fn every_format_round_trips_the_timing_and_text() {
        for fmt in ["srt", "vtt", "sbv", "ass", "ssa"] {
            let out = write(fmt, &cues()).unwrap();
            let back = parse(&format!(".{fmt}"), out.as_bytes()).unwrap();
            let src = cues();
            let want: Vec<(u64, u64, &str)> = src.iter().map(|c| (c.start_ms, c.end_ms, c.text.as_str())).collect();
            let got: Vec<(u64, u64, String)> = back.iter().map(|c| (c.start_ms, c.end_ms, c.text.clone())).collect();
            // ASS keeps centiseconds only.
            let tol = if fmt == "ass" || fmt == "ssa" { 10 } else { 0 };
            for (w, g) in want.iter().zip(&got) { assert!(w.0.abs_diff(g.0) <= tol && w.1.abs_diff(g.1) <= tol && w.2 == g.2, "{fmt}: {w:?} vs {g:?}"); }
            assert_eq!(want.len(), got.len(), "{fmt}");
        }
    }
    #[test] fn lrc_uses_the_next_line_as_the_end() {
        let c = parse(".lrc", b"[ti:Song]\n[00:01.50]First\n[00:04.00]Second\n").unwrap();
        assert_eq!((c[0].start_ms, c[0].end_ms, c[1].start_ms), (1500, 4000, 4000));
        assert_eq!(write("lrc", &c).unwrap(), "[00:01.50]First\n[00:04.00]Second\n");
    }
    #[test] fn vtt_settings_and_short_timestamps() {
        let c = parse(".vtt", b"WEBVTT\n\nintro\n00:01.000 --> 00:02.000 align:start position:0%\nHi\n").unwrap();
        assert_eq!((c[0].start_ms, c[0].end_ms, c[0].text.as_str()), (1000, 2000, "Hi"));
    }
    #[test] fn files_without_cues_are_refused() { assert!(parse(".srt", b"nothing here").is_err()); }
}
