//! Native image transcoding: no process start, no ffmpeg. Formats the `image`
//! crate cannot read or write (avif, jxl, jp2, pcx, svg, subtitles) still go to ffmpeg.
use super::Ctx;
use crate::{formats::Resolved, Error, ErrorCode, Result};
use image::ImageFormat;
use std::path::PathBuf;

fn decodable(ext: &str) -> bool {
    matches!(ext, ".bmp" | ".gif" | ".ico" | ".jpg" | ".jpeg" | ".jpe" | ".jfif" | ".png" | ".apng"
        | ".qoi" | ".tga" | ".tif" | ".tiff" | ".webp")
}

fn encoder_for(target_id: &str) -> Option<ImageFormat> {
    Some(match target_id {
        "bmp" => ImageFormat::Bmp, "gif" => ImageFormat::Gif, "tiff" => ImageFormat::Tiff,
        "webp" => ImageFormat::WebP, "ico" => ImageFormat::Ico, "png-image" => ImageFormat::Png,
        "jpg-image" => ImageFormat::Jpeg, "qoi" => ImageFormat::Qoi, "tga" => ImageFormat::Tga,
        _ => return None,
    })
}

pub fn can_handle(conv: &Resolved) -> bool {
    decodable(&conv.source.extension) && encoder_for(&conv.target.id).is_some()
}

pub async fn run(ctx: &Ctx<'_>, conv: &Resolved) -> Result<Vec<PathBuf>> {
    let fmt = encoder_for(&conv.target.id).expect("checked by can_handle");
    let input = ctx.input.clone();
    let out = ctx.out_dir.join(format!("converted{}", conv.target.extension));
    let out2 = out.clone();
    let quality = ctx.cfg.raster_jpeg_quality as u8;
    tokio::task::spawn_blocking(move || -> Result<()> {
        let img = image::open(&input).map_err(|e| Error::new(
            ErrorCode::EConvertFailed, format!("Could not read the image: {e}")))?;
        if fmt == ImageFormat::Ico && (img.width() > 256 || img.height() > 256) {
            // A real format limit; we do not silently downscale (same rule as the TS backend).
            return Err(Error::new(ErrorCode::EConvertFailed,
                "ICO files cannot be larger than 256x256 pixels."));
        }
        let fail = |e: image::ImageError| Error::new(ErrorCode::EConvertFailed, format!("Could not write the image: {e}"));
        let file = std::io::BufWriter::new(std::fs::File::create(&out2)?);
        match fmt {
            ImageFormat::Jpeg => {
                // JPEG has no alpha channel.
                let rgb = img.to_rgb8();
                image::codecs::jpeg::JpegEncoder::new_with_quality(file, quality)
                    .encode_image(&rgb).map_err(fail)?;
            }
            ImageFormat::Bmp | ImageFormat::Tga | ImageFormat::Gif | ImageFormat::Ico | ImageFormat::Png
            | ImageFormat::Tiff | ImageFormat::WebP | ImageFormat::Qoi => {
                let mut file = file.into_inner().map_err(|e| Error::from(e.into_error()))?;
                // Tiff needs Seek, so write through the File directly for every format.
                let img = if fmt == ImageFormat::Gif { image::DynamicImage::ImageRgba8(img.to_rgba8()) } else { img };
                let mut cur = std::io::Cursor::new(Vec::new());
                img.write_to(&mut cur, fmt).map_err(fail)?;
                std::io::Write::write_all(&mut file, cur.get_ref())?;
            }
            _ => unreachable!(),
        }
        Ok(())
    }).await.map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))??;
    Ok(vec![out])
}
