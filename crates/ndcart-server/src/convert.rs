//! Image conversion: PNG / JPEG / GIF / BMP / WebP in, PNG / WebP / AVIF out.
//! Used by `ndcart convert` and `POST /api/convert`. Decoding is bounded
//! (pixel budget, decoder limits) because the bytes come from outside
//! (plan §17.7). AVIF *input* is not supported: decoding AVIF needs a C
//! library (dav1d), which the static musl image deliberately avoids.

use crate::tiles::ImageFormat;
use image::{DynamicImage, ImageEncoder, ImageReader, Limits};
use std::io::Cursor;

pub const MAX_INPUT_BYTES: usize = 80 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 40_000_000;
pub const MAX_SIDE: u32 = 20_000;

#[derive(Debug)]
pub struct Converted {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

pub(crate) fn decode(input: &[u8]) -> Result<DynamicImage, String> {
    if input.is_empty() {
        return Err("empty input".into());
    }
    if input.len() > MAX_INPUT_BYTES {
        return Err(format!("input over {} MB", MAX_INPUT_BYTES / 1_048_576));
    }
    if input.len() > 12 && &input[4..12] == b"ftypavif" {
        return Err("AVIF input is not supported (convert it to PNG/WebP first)".into());
    }
    let mut reader = ImageReader::new(Cursor::new(input))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(MAX_PIXELS * 4 + (64 << 20));
    reader.limits(limits);
    let img = reader
        .decode()
        .map_err(|e| format!("cannot read the image: {e}"))?;
    if u64::from(img.width()) * u64::from(img.height()) > MAX_PIXELS {
        return Err(format!("image over {} megapixels", MAX_PIXELS / 1_000_000));
    }
    Ok(img)
}

/// Decode to straight RGBA8 `(pixels, width, height)` within the size limits.
pub(crate) fn decode_rgba(input: &[u8]) -> Result<(Vec<u8>, u32, u32), String> {
    let img = decode(input)?.to_rgba8();
    let (w, h) = img.dimensions();
    Ok((img.into_raw(), w, h))
}

/// Convert `input` to `format`. `quality` (1..=100) applies to AVIF; WebP is
/// lossless. Transparency is preserved.
pub fn convert(input: &[u8], format: ImageFormat, quality: u8) -> Result<Converted, String> {
    let img = decode(input)?.to_rgba8();
    let (w, h) = img.dimensions();
    let mut out = Vec::new();
    match format {
        ImageFormat::Png => image::codecs::png::PngEncoder::new(&mut out).write_image(
            img.as_raw(),
            w,
            h,
            image::ExtendedColorType::Rgba8,
        ),
        ImageFormat::Webp => image::codecs::webp::WebPEncoder::new_lossless(&mut out).write_image(
            img.as_raw(),
            w,
            h,
            image::ExtendedColorType::Rgba8,
        ),
        ImageFormat::Avif => image::codecs::avif::AvifEncoder::new_with_speed_quality(
            &mut out,
            8,
            quality.clamp(1, 100),
        )
        .write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgba8),
    }
    .map_err(|e| format!("{} encode failed: {e}", format.ext()))?;
    Ok(Converted {
        bytes: out,
        width: w,
        height: h,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_png() -> Vec<u8> {
        let mut pm = tiny_skia::Pixmap::new(12, 8).unwrap();
        pm.fill(tiny_skia::Color::from_rgba8(200, 30, 60, 255));
        pm.encode_png().unwrap()
    }

    #[test]
    fn png_to_webp_is_lossless_and_to_avif_works() {
        let png = sample_png();
        let w = convert(&png, ImageFormat::Webp, 80).unwrap();
        assert_eq!((w.width, w.height), (12, 8));
        assert_eq!(&w.bytes[8..12], b"WEBP");
        // webp back to png keeps the pixels
        let back = convert(&w.bytes, ImageFormat::Png, 80).unwrap();
        let a = tiny_skia::Pixmap::decode_png(&back.bytes).unwrap();
        let b = tiny_skia::Pixmap::decode_png(&png).unwrap();
        assert_eq!(a.data(), b.data());
        let a = convert(&png, ImageFormat::Avif, 60).unwrap();
        assert_eq!(&a.bytes[4..12], b"ftypavif");
    }

    #[test]
    fn bad_input_is_refused_not_panicked() {
        assert!(convert(b"", ImageFormat::Webp, 80).is_err());
        assert!(convert(b"not an image at all", ImageFormat::Webp, 80).is_err());
        let avif = convert(&sample_png(), ImageFormat::Avif, 60).unwrap().bytes;
        assert!(convert(&avif, ImageFormat::Webp, 80)
            .unwrap_err()
            .contains("AVIF input"));
    }
}
