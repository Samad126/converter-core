//! Native image transcoding: no process start, no ffmpeg. Formats the `image`
//! crate cannot read or write (avif, jxl, jp2, pcx, svg, subtitles) still go to ffmpeg.
use super::Ctx;
use crate::{formats::Resolved, Error, ErrorCode, Result};
use image::ImageFormat;
use std::path::{Path, PathBuf};

pub(crate) fn decodable(ext: &str) -> bool {
    matches!(ext, ".bmp" | ".gif" | ".ico" | ".jpg" | ".jpeg" | ".jpe" | ".jfif" | ".png" | ".apng"
        | ".qoi" | ".tga" | ".tif" | ".tiff" | ".webp" | ".dds" | ".exr" | ".hdr" | ".pnm" | ".pbm" | ".pgm" | ".ppm" | ".pam")
}

pub(crate) fn encoder_for(target_id: &str) -> Option<ImageFormat> {
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

/// HDR/EXR pictures decode to floating point, which PNG/JPEG/WebP/... cannot hold: clamp to the displayable range and
/// apply the usual sRGB transfer curve so they do not come out dark.
pub(crate) fn to_displayable(img: image::DynamicImage) -> image::DynamicImage {
    use image::DynamicImage as D;
    let enc = |v: f32| (v.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0 + 0.5) as u8;
    match img {
        D::ImageRgb32F(f) => D::ImageRgb8(image::ImageBuffer::from_fn(f.width(), f.height(), |x, y| { let p = f.get_pixel(x, y).0; image::Rgb([enc(p[0]), enc(p[1]), enc(p[2])]) })),
        D::ImageRgba32F(f) => D::ImageRgba8(image::ImageBuffer::from_fn(f.width(), f.height(), |x, y| { let p = f.get_pixel(x, y).0; image::Rgba([enc(p[0]), enc(p[1]), enc(p[2]), (p[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8]) })),
        other => other,
    }
}

/// Writes `img` in `fmt` to `out` (JPEG drops alpha; ICO refuses anything over 256x256 rather than silently shrinking).
pub(crate) fn encode_to(img: image::DynamicImage, fmt: ImageFormat, jpeg_quality: u8, out: &Path) -> Result<()> {
    if fmt == ImageFormat::Ico && (img.width() > 256 || img.height() > 256) {
        // A real format limit; we do not silently downscale (same rule as the TS backend).
        return Err(Error::new(ErrorCode::EConvertFailed, "ICO files cannot be larger than 256x256 pixels."));
    }
    let img = to_displayable(img);
    let fail = |e: image::ImageError| Error::new(ErrorCode::EConvertFailed, format!("Could not write the image: {e}"));
    let mut file = std::fs::File::create(out)?;
    match fmt {
        ImageFormat::Jpeg => {
            // JPEG has no alpha channel.
            let rgb = img.to_rgb8();
            image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), jpeg_quality).encode_image(&rgb).map_err(fail)?;
        }
        ImageFormat::Bmp | ImageFormat::Tga | ImageFormat::Gif | ImageFormat::Ico | ImageFormat::Png
        | ImageFormat::Tiff | ImageFormat::WebP | ImageFormat::Qoi => {
            let img = if fmt == ImageFormat::Gif { image::DynamicImage::ImageRgba8(img.to_rgba8()) } else { img };
            // Tiff needs Seek, so encode into memory and write the bytes for every format.
            let mut cur = std::io::Cursor::new(Vec::new());
            img.write_to(&mut cur, fmt).map_err(fail)?;
            std::io::Write::write_all(&mut file, cur.get_ref())?;
        }
        _ => unreachable!(),
    }
    Ok(())
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
        encode_to(img, fmt, quality, &out2)
    }).await.map_err(|e| Error::new(ErrorCode::EInternal, e.to_string()))??;
    Ok(vec![out])
}
