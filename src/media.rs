//! Audio/video transcoding with progress. Port of the `/media/{target}` job path:
//! `ffmpeg -y -i in out`, plus `-progress pipe:1` so the UI gets a real percentage.
use crate::convert::{unique_path, ConvertOptions};
use crate::formats::{matrix, MediaFormat};
use crate::progress::{Progress, ProgressFn};
use crate::{engines, process, workspace::Workspace, Config, Error, ErrorCode, Result};
use std::path::{Path, PathBuf};

pub fn is_media_extension(ext: &str) -> bool { matrix().media.values().any(|f| f.extension == ext) }

fn kind_of(ext: &str) -> Option<&'static str> {
    matrix().media.values().find(|f| f.extension == ext).map(|f| f.kind.as_str())
}

pub fn resolve_media(ext: &str, target_id: &str) -> Option<&'static MediaFormat> {
    let t = matrix().media.get(target_id)?;
    // no self-conversion; audio stays audio, video stays video
    (t.extension != ext && kind_of(ext)? == t.kind).then_some(t)
}

pub fn media_targets_for(ext: &str) -> Vec<&'static str> {
    let Some(kind) = kind_of(ext) else { return vec![] };
    matrix().media.values().filter(|f| f.kind == kind && f.extension != ext).map(|f| f.id.as_str()).collect()
}

/// `00:01:23.45` → seconds.
pub fn parse_hms(s: &str) -> Option<f64> {
    let mut it = s.trim().split(':');
    let (h, m, sec) = (it.next()?.parse::<f64>().ok()?, it.next()?.parse::<f64>().ok()?, it.next()?.parse::<f64>().ok()?);
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// Stateful parser for ffmpeg's stderr (`Duration:`) and `-progress` stdout (`out_time_us=`).
#[derive(Default)]
pub struct FfmpegProgress { duration: Option<f64> }

impl FfmpegProgress {
    pub fn feed(&mut self, line: &str) -> Option<f32> {
        if let Some(rest) = line.trim().strip_prefix("Duration:") {
            self.duration = rest.split(',').next().and_then(parse_hms).filter(|d| *d > 0.0);
            return None;
        }
        let us = line.strip_prefix("out_time_us=").or_else(|| line.strip_prefix("out_time_ms="))?;
        let secs = us.trim().parse::<f64>().ok()? / 1_000_000.0;
        Some(((secs / self.duration?) as f32).clamp(0.0, 1.0))
    }
}

pub async fn convert_media(cfg: &Config, input: &Path, target: &str, opts: ConvertOptions, progress: ProgressFn) -> Result<PathBuf> {
    let ext = crate::formats::extension_of(input);
    if !matrix().media.contains_key(target) {
        return Err(Error::new(ErrorCode::EUnknownTarget, format!("Unknown media target \"{target}\".")));
    }
    if !is_media_extension(&ext) {
        return Err(Error::new(ErrorCode::EUnsupported, format!("\"{ext}\" is not an audio or video format we convert.")));
    }
    let fmt = resolve_media(&ext, target).ok_or_else(|| Error::new(
        ErrorCode::EUnsupportedTarget,
        format!("{ext} cannot become {target}. It can become: {}", media_targets_for(&ext).join(", "))))?;
    let ffmpeg = engines::resolve(engines::tool("ffmpeg").unwrap(), cfg, None).ok_or_else(|| Error::engine_missing("ffmpeg"))?;

    let ws = Workspace::create(cfg).await?;
    let work_in = ws.path().join(format!("input{ext}"));
    tokio::fs::copy(input, &work_in).await?;
    let work_out = ws.path().join(format!("converted{}", fmt.extension));

    progress(Progress::Started);
    let args: Vec<String> = ["-y", "-nostdin", "-nostats", "-progress", "pipe:1", "-i"].map(String::from).into_iter()
        .chain([work_in.to_string_lossy().into_owned(), work_out.to_string_lossy().into_owned()]).collect();
    let mut parser = FfmpegProgress::default();
    let cb = progress.clone();
    process::run_streaming(&ffmpeg, &args, ws.path(), cfg.media_timeout, &[], move |line| {
        if let Some(f) = parser.feed(line) { cb(Progress::Fraction(f)); }
    }).await?.require_ok("ffmpeg")?;
    if !work_out.metadata().is_ok_and(|m| m.len() > 0) {
        return Err(Error::new(ErrorCode::EConvertFailed, "ffmpeg produced no output file."));
    }

    let dest_dir = opts.out_dir.unwrap_or_else(|| input.parent().map(Path::to_path_buf).unwrap_or_default());
    tokio::fs::create_dir_all(&dest_dir).await?;
    let stem = input.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "converted".into());
    let dest = unique_path(&dest_dir, &stem, &fmt.extension);
    tokio::fs::copy(&work_out, &dest).await?;
    progress(Progress::Finished);
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn matrix_rules() {
        assert!(resolve_media(".mp3", "wav").is_some());
        assert!(resolve_media(".mp3", "mp3").is_none());   // self
        assert!(resolve_media(".mp3", "mp4").is_none());   // audio -> video
        assert!(media_targets_for(".mp4").contains(&"webm"));
        assert!(!media_targets_for(".mp4").contains(&"mp3"));
    }
    #[test] fn progress_parsing() {
        let mut p = FfmpegProgress::default();
        assert_eq!(p.feed("out_time_us=1000000"), None); // duration not known yet
        p.feed("  Duration: 00:00:04.00, start: 0.000000, bitrate: 128 kb/s");
        assert_eq!(p.feed("out_time_us=1000000"), Some(0.25));
        assert_eq!(p.feed("out_time_us=9000000"), Some(1.0)); // clamped
        assert_eq!(p.feed("progress=continue"), None);
    }
    #[test] fn hms() { assert_eq!(parse_hms("01:02:03.5"), Some(3723.5)); assert_eq!(parse_hms("N/A"), None); }
}
