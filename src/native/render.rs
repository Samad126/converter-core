//! PDF page rasteriser (pure Rust, through `hayro`): the engine-free replacement for `pdftoppm`.
use crate::{Error, ErrorCode, Result};
use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{render, PixmapSettings, RenderCache, RenderSettings};

fn failed(msg: impl Into<String>) -> Error { Error::new(ErrorCode::EConvertFailed, msg) }

pub struct PageImage { pub image: image::RgbImage, /// Page size in points.
    pub width_pt: f32, pub height_pt: f32 }

/// Renders every page (at most `max_pages`) at `dpi`. Encrypted or damaged files are reported, not guessed at.
pub fn render_pdf(bytes: Vec<u8>, dpi: f32, max_pages: usize) -> Result<Vec<PageImage>> {
    let pdf = Pdf::new(bytes).map_err(|e| match format!("{e:?}") {
        s if s.to_lowercase().contains("encrypt") => Error::new(ErrorCode::EEncrypted, "This PDF is password protected."),
        s => failed(format!("not a readable PDF: {s}")),
    })?;
    let pages = pdf.pages();
    if pages.len() > max_pages {
        return Err(Error::new(ErrorCode::ETooLarge, format!("This document has {} pages; the limit is {max_pages}.", pages.len())));
    }
    let scale = (dpi / 72.0).clamp(0.1, 16.0);
    let cache = RenderCache::new();
    let settings = InterpreterSettings::default();
    let render_settings = RenderSettings::default();
    let mut out = Vec::with_capacity(pages.len());
    for page in pages.iter() {
        let (w, h) = page.render_dimensions();
        if w * scale > 16_000.0 || h * scale > 16_000.0 {
            return Err(Error::new(ErrorCode::ETooLarge, "A page is too large to render at this resolution."));
        }
        let pix = render(page, &cache, &settings, &render_settings, &PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE });
        let rgba = pix.data_as_u8_slice();
        let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
        for px in rgba.chunks_exact(4) { rgb.extend_from_slice(&px[..3]); }
        let image = image::RgbImage::from_raw(pix.width() as u32, pix.height() as u32, rgb).ok_or_else(|| failed("unexpected pixel buffer size"))?;
        out.push(PageImage { image, width_pt: w, height_pt: h });
    }
    if out.is_empty() { return Err(failed("The PDF has no pages.")); }
    Ok(out)
}

pub fn encode(img: &image::RgbImage, format: &str, jpeg_quality: u8) -> Result<Vec<u8>> {
    let fail = |e: image::ImageError| failed(format!("Could not write the image: {e}"));
    let mut buf = std::io::Cursor::new(Vec::new());
    match format {
        "jpg" => image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, jpeg_quality).encode_image(img).map_err(fail)?,
        "png" => img.write_to(&mut buf, image::ImageFormat::Png).map_err(fail)?,
        "tiff" => img.write_to(&mut buf, image::ImageFormat::Tiff).map_err(fail)?,
        other => return Err(Error::new(ErrorCode::EInternal, format!("no encoder for {other}"))),
    }
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn renders_our_own_pdf_with_text_visible() {
        let doc = crate::native::model::Doc { blocks: vec![crate::native::model::Block::Para(crate::native::model::Para {
            runs: vec![crate::native::model::Run { text: "Hello render".into(), size_pt: Some(40.0), ..Default::default() }], ..Default::default() })], ..Default::default() };
        let pdf = crate::native::write::pdf::doc_to_pdf(&doc, "t").unwrap();
        let pages = render_pdf(pdf, 72.0, 10).unwrap();
        assert_eq!(pages.len(), 1);
        assert!((pages[0].width_pt - 595.28).abs() < 0.5);
        let dark = pages[0].image.pixels().filter(|p| p.0[0] < 128).count();
        assert!(dark > 200, "text should leave dark pixels, got {dark}");
        let corner = pages[0].image.get_pixel(2, 2).0;
        assert_eq!(corner, [255, 255, 255]);
    }
    #[test] fn garbage_is_a_convert_failure() {
        assert_eq!(render_pdf(b"not a pdf".to_vec(), 72.0, 10).err().unwrap().code, ErrorCode::EConvertFailed);
    }
}
