//! YouTube → mp3/mp4 through yt-dlp. Port of `youtube-url.ts`, `ytdlp.engine.ts`, `youtube.service.ts`.
use crate::convert::output_path;
use crate::progress::{Progress, ProgressFn};
use crate::{engines, process, workspace::Workspace, Config, Error, ErrorCode, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target { Mp3, Mp4 }

impl Target {
    pub fn parse(s: &str) -> Option<Self> { match s { "mp3" => Some(Self::Mp3), "mp4" => Some(Self::Mp4), _ => None } }
    pub fn ext(self) -> &'static str { match self { Self::Mp3 => ".mp3", Self::Mp4 => ".mp4" } }
}

#[derive(Debug, PartialEq)]
pub struct Video { pub id: String, pub canonical_url: String }

const HOSTS: &[&str] = &["youtube.com", "www.youtube.com", "m.youtube.com", "music.youtube.com", "youtu.be"];

/// Accepts only real YouTube video links (watch / youtu.be / shorts / embed / live); the id is
/// validated to 11 safe characters, so nothing else is ever handed to yt-dlp.
pub fn parse_url(raw: &str) -> Option<Video> {
    let t = raw.trim();
    let has_scheme = t.split_once("://").is_some_and(|(s, _)| !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c)));
    let url = url::Url::parse(&if has_scheme { t.to_string() } else { format!("https://{t}") }).ok()?;
    if !matches!(url.scheme(), "http" | "https") { return None; }
    let host = url.host_str()?.to_lowercase();
    if !HOSTS.contains(&host.as_str()) { return None; }
    let segs: Vec<&str> = url.path().split('/').filter(|s| !s.is_empty()).collect();
    let id = if host == "youtu.be" {
        segs.first().map(|s| s.to_string())
    } else if segs.first() == Some(&"watch") {
        url.query_pairs().find(|(k, _)| k == "v").map(|(_, v)| v.into_owned())
    } else if matches!(segs.first(), Some(&"shorts" | &"embed" | &"live")) {
        segs.get(1).map(|s| s.to_string())
    } else { None }?;
    (id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
        .then(|| Video { canonical_url: format!("https://www.youtube.com/watch?v={id}"), id })
}

/// Title → safe file stem (port of `downloadNameForTitle`).
pub fn name_for_title(title: &str, ext: &str, max_chars: usize) -> String {
    let t = title.replace(['\\', '/'], "-");
    let cleaned: String = t.chars().filter(|c| !c.is_control() && *c != '"' && *c != '\\').collect();
    let cleaned = cleaned.trim().trim_start_matches('.');
    let base: String = cleaned.chars().take(max_chars).collect();
    let base = base.trim_end().to_string();
    if base.is_empty() { format!("converted{ext}") } else { format!("{base}{ext}") }
}

const MP4_SELECTOR: &str = "bv*[ext=mp4][vcodec^=avc1]+ba[ext=m4a]/b[ext=mp4][vcodec^=avc1]/bv*[ext=mp4]+ba[ext=m4a]/b[ext=mp4]/bv*+ba/b";

pub fn build_args(cfg: &Config, url: &str, target: Target, out_stem: &Path, title_path: &Path, ffmpeg: Option<&Path>, js_runtime: Option<&Path>) -> Vec<String> {
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let mut a: Vec<String> = ["--ignore-config", "--no-cache-dir", "--no-playlist", "--progress", "--newline",
        "--progress-template", "download:PROGRESS %(progress._percent_str)s", "--socket-timeout", "30",
        "--match-filter", "!is_live", "--max-filesize"].map(String::from).to_vec();
    a.push(cfg.youtube_max_filesize_bytes.to_string());
    if let Some(js) = js_runtime { a.extend(["--js-runtimes".into(), format!("node:{}", s(js))]); }
    a.extend(["-f".into(), if target == Target::Mp4 { MP4_SELECTOR } else { "ba/b" }.into(),
              "-o".into(), format!("{}.%(ext)s", s(out_stem).replace('%', "%%")),
              "--print-to-file".into(), "after_move:%(title)s".into(), s(title_path)]);
    if let Some(f) = ffmpeg { a.extend(["--ffmpeg-location".into(), s(f)]); }
    if target == Target::Mp4 { a.extend(["--merge-output-format".into(), "mp4".into()]); }
    else { a.extend(["-x", "--audio-format", "mp3", "--audio-quality", "0"].map(String::from)); }
    a.extend(["--".into(), url.into()]);
    a
}

/// `PROGRESS  45.3%` → 0.453
pub fn parse_progress(line: &str) -> Option<f32> {
    let v = line.trim().strip_prefix("PROGRESS")?.trim().trim_end_matches('%').trim().parse::<f32>().ok()?;
    Some((v / 100.0).clamp(0.0, 1.0))
}

pub fn failure_for(diag: &str, exit: Option<i32>, cfg: &Config) -> Error {
    let d = diag.to_lowercase();
    if d.contains("larger than max-filesize") {
        return Error::new(ErrorCode::ETooLarge, format!("This video is larger than the {} MB limit.", cfg.youtube_max_filesize_bytes / (1024 * 1024)));
    }
    if d.contains("does not pass filter (is_live)") {
        return Error::new(ErrorCode::EUnsupported, "This video is a live stream. Try again once it has ended.");
    }
    const UNAVAILABLE: &[&str] = &["video is unavailable", "this video is not available", "private video", "video has been removed",
        "removed by the uploader", "available in your country", "copyright grounds", "confirm your age",
        "members-only", "members only", "join this channel", "account has been terminated", "account terminated"];
    if UNAVAILABLE.iter().any(|p| d.contains(p)) {
        return Error::new(ErrorCode::EUnsupported, "This video is private, removed, age-restricted, or not available in this country.");
    }
    let tail: String = diag.trim().chars().rev().take(2000).collect::<Vec<_>>().into_iter().rev().collect();
    Error::new(ErrorCode::EConvertFailed, format!("This video could not be downloaded. Try again in a few minutes. (yt-dlp exited {exit:?}: {tail})"))
}

pub async fn download(cfg: &Config, raw_url: &str, target: Target, out_dir: &Path, progress: ProgressFn) -> Result<PathBuf> {
    let video = parse_url(raw_url).ok_or_else(|| Error::new(ErrorCode::EBadRequest, "That doesn't look like a YouTube video link."))?;
    let ytdlp = engines::resolve(engines::tool("yt-dlp").unwrap(), cfg, None).ok_or_else(|| Error::engine_missing("yt-dlp"))?;
    let ffmpeg = engines::resolve(engines::tool("ffmpeg").unwrap(), cfg, None);
    if ffmpeg.is_none() { return Err(Error::engine_missing("ffmpeg")); }
    // Recent YouTube needs a JS runtime for yt-dlp's signature solving; use node/deno if present.
    let js = which::which("node").ok();

    let ws = Workspace::create(cfg).await?;
    let out_path = ws.path().join(format!("video{}", target.ext()));
    let title_path = ws.path().join("title.txt");
    let args = build_args(cfg, &video.canonical_url, target, &ws.path().join("video"), &title_path, ffmpeg.as_deref(), js.as_deref());

    progress(Progress::Started);
    let cb = progress.clone();
    let o = process::run_streaming(&ytdlp, &args, ws.path(), cfg.media_timeout, &[], move |l| {
        if let Some(f) = parse_progress(l) { cb(Progress::Fraction(f)); }
    }).await?;

    let size = out_path.metadata().map(|m| m.len()).unwrap_or(0);
    if o.code != Some(0) || size == 0 {
        return Err(failure_for(&format!("{}\n{}", o.stderr, o.stdout), o.code, cfg));
    }
    // Audio and video streams are each under the limit but merge into a bigger file.
    if size > cfg.youtube_max_filesize_bytes { return Err(failure_for("larger than max-filesize", o.code, cfg)); }
    let title = std::fs::read_to_string(&title_path).ok().and_then(|t| t.lines().next().map(|l| l.trim().to_string()))
        .filter(|t| !t.is_empty()).unwrap_or_else(|| video.id.clone());
    let name = name_for_title(&title, target.ext(), 100);
    let stem = name.strip_suffix(target.ext()).unwrap_or(&name).to_string();
    tokio::fs::create_dir_all(out_dir).await?;
    let dest = output_path(cfg, out_dir, &stem, target.ext());
    tokio::fs::copy(&out_path, &dest).await?;
    progress(Progress::Finished);
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(u: &str) -> Option<String> { parse_url(u).map(|v| v.id) }
    #[test] fn accepts_the_common_shapes() {
        for u in ["https://www.youtube.com/watch?v=dQw4w9WgXcQ", "youtu.be/dQw4w9WgXcQ?t=5", "http://m.youtube.com/watch?v=dQw4w9WgXcQ&list=x",
                  "https://youtube.com/shorts/dQw4w9WgXcQ", "https://www.youtube.com/embed/dQw4w9WgXcQ", "music.youtube.com/watch?v=dQw4w9WgXcQ"] {
            assert_eq!(id(u).as_deref(), Some("dQw4w9WgXcQ"), "{u}");
        }
        assert_eq!(parse_url("youtu.be/dQw4w9WgXcQ").unwrap().canonical_url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
    }
    #[test] fn rejects_everything_else() {
        for u in ["https://evil.com/watch?v=dQw4w9WgXcQ", "https://youtube.com.evil.com/watch?v=dQw4w9WgXcQ", "ftp://youtube.com/watch?v=dQw4w9WgXcQ",
                  "https://www.youtube.com/watch?v=short", "https://www.youtube.com/watch?v=dQw4w9WgXc;", "https://www.youtube.com/", "https://www.youtube.com/playlist?list=abc", "not a url", "--exec=x"] {
            assert!(parse_url(u).is_none(), "{u}");
        }
    }
    #[test] fn names_are_safe() {
        assert_eq!(name_for_title("A/B \"quoted\"", ".mp3", 100), "A-B quoted.mp3");
        assert_eq!(name_for_title("...", ".mp4", 100), "converted.mp4");
        assert_eq!(name_for_title("  ..hidden  ", ".mp3", 100), "hidden.mp3");
        assert_eq!(name_for_title(&"x".repeat(300), ".mp3", 100).len(), 104);
    }
    #[test] fn progress_lines() {
        assert!((parse_progress("PROGRESS  45.3%").unwrap() - 0.453).abs() < 1e-5);
        assert_eq!(parse_progress("[download] Destination: x"), None);
    }
    #[test] fn failures_are_classified() {
        let c = Config::default();
        assert_eq!(failure_for("ERROR: Private video", Some(1), &c).code, ErrorCode::EUnsupported);
        assert_eq!(failure_for("File is larger than max-filesize", Some(1), &c).code, ErrorCode::ETooLarge);
        assert!(failure_for("boom", Some(1), &c).message.contains("boom"));
    }
    #[test] fn args_never_pass_url_as_option() {
        let c = Config::default();
        let a = build_args(&c, "https://www.youtube.com/watch?v=dQw4w9WgXcQ", Target::Mp3, Path::new("/w/v%x"), Path::new("/w/t"), None, None);
        let dd = a.iter().position(|x| x == "--").unwrap();
        assert_eq!(a.len() - dd, 2);
        assert!(a.contains(&"/w/v%%x.%(ext)s".to_string()));
    }
}
