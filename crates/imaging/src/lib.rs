//! `rackend-imaging`: the `ImageProcessor` port (SPEC §12.3, §12.5).
//!
//! - `probe` reads only the image header (dimensions), so oversized images
//!   are refused before any pixel is decoded.
//! - `normalize` applies the EXIF orientation and re-encodes without any
//!   metadata: JPEG at quality 90, PNG and WebP lossless.
//! - `thumbnail` resizes with Lanczos3 (`fast_image_resize`) and encodes
//!   lossy WebP at quality 80 with libwebp, keeping alpha. GIFs use their
//!   first frame.

use std::io::{BufWriter, Cursor, Write as _};
use std::path::Path;

use fast_image_resize::{FilterType, ResizeAlg, ResizeOptions, Resizer};
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use rackend_core::domain::files::{MAX_PIXELS, ThumbMode, ThumbSpec};
use rackend_core::ports::{ImageError, ImageInfo, ImageProcessor, NormalizedImage};
use sha2::{Digest as _, Sha256};

/// Decoder memory cap: a 50 MP RGBA image plus headroom.
const MAX_ALLOC: u64 = 512 * 1024 * 1024;

/// The image processor.
#[derive(Debug, Default, Clone, Copy)]
pub struct Imaging;

fn invalid(e: impl std::fmt::Display) -> ImageError {
    ImageError::Invalid(e.to_string())
}

fn io(e: impl std::fmt::Display) -> ImageError {
    ImageError::Io(e.to_string())
}

fn limits() -> Limits {
    let mut l = Limits::default();
    l.max_alloc = Some(MAX_ALLOC);
    l.max_image_width = Some(50_000);
    l.max_image_height = Some(50_000);
    l
}

/// Decodes `bytes`, refusing images above the pixel limit, and applies the
/// EXIF orientation.
fn decode_oriented(bytes: &[u8]) -> Result<DynamicImage, ImageError> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(io)?;
    reader.limits(limits());
    let mut decoder = reader.into_decoder().map_err(invalid)?;
    let (w, h) = decoder.dimensions();
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(ImageError::Invalid("more than 50 megapixels".into()));
    }
    let orientation = decoder.orientation().map_err(invalid)?;
    let mut img = DynamicImage::from_decoder(decoder).map_err(invalid)?;
    img.apply_orientation(orientation);
    Ok(img)
}

/// Encodes to WebP with libwebp: lossy at `quality`, or lossless.
fn encode_webp(img: &DynamicImage, quality: Option<f32>) -> Vec<u8> {
    let (w, h) = (img.width(), img.height());
    let mem = if img.color().has_alpha() {
        let rgba = img.to_rgba8();
        let enc = webp::Encoder::from_rgba(rgba.as_raw(), w, h);
        match quality {
            Some(q) => enc.encode(q),
            None => enc.encode_lossless(),
        }
    } else {
        let rgb = img.to_rgb8();
        let enc = webp::Encoder::from_rgb(rgb.as_raw(), w, h);
        match quality {
            Some(q) => enc.encode(q),
            None => enc.encode_lossless(),
        }
    };
    mem.to_vec()
}

/// A source crop: left, top, width, height.
type Crop = (f64, f64, f64, f64);

/// Output size and source crop for a thumbnail spec.
fn plan(src_w: u32, src_h: u32, spec: &ThumbSpec) -> (u32, u32, Option<Crop>) {
    let (sw, sh) = (f64::from(src_w), f64::from(src_h));
    let ratio = sw / sh;
    let round = |x: f64| (x.round() as u32).max(1);
    if spec.width == 0 {
        return (round(f64::from(spec.height) * ratio), spec.height, None);
    }
    if spec.height == 0 {
        return (spec.width, round(f64::from(spec.width) / ratio), None);
    }
    let (tw, th) = (f64::from(spec.width), f64::from(spec.height));
    if spec.mode == ThumbMode::Fit {
        let scale = (tw / sw).min(th / sh);
        return (round(sw * scale), round(sh * scale), None);
    }
    // Crop the source to the target ratio, then scale.
    let target = tw / th;
    let (cw, ch) = if ratio > target {
        (sh * target, sh)
    } else {
        (sw, sw / target)
    };
    let left = (sw - cw) / 2.0;
    let top = match spec.mode {
        ThumbMode::Top => 0.0,
        ThumbMode::Bottom => sh - ch,
        _ => (sh - ch) / 2.0,
    };
    (spec.width, spec.height, Some((left, top, cw, ch)))
}

impl ImageProcessor for Imaging {
    fn probe(&self, path: &Path) -> Result<Option<ImageInfo>, ImageError> {
        let reader = ImageReader::open(path)
            .map_err(io)?
            .with_guessed_format()
            .map_err(io)?;
        if reader.format().is_none() {
            return Ok(None);
        }
        let (width, height) = reader.into_dimensions().map_err(invalid)?;
        Ok(Some(ImageInfo { width, height }))
    }

    fn normalize(
        &self,
        input: &Path,
        output: &Path,
        mime: &str,
    ) -> Result<NormalizedImage, ImageError> {
        let bytes = std::fs::read(input).map_err(io)?;
        let encoded = if mime == "image/gif" {
            // GIFs carry no EXIF; re-encoding would drop animation.
            bytes.clone()
        } else {
            let img = decode_oriented(&bytes)?;
            let mut out = Vec::new();
            match mime {
                "image/jpeg" => {
                    let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
                    JpegEncoder::new_with_quality(&mut out, 90)
                        .encode_image(&rgb)
                        .map_err(io)?;
                }
                "image/png" => img
                    .write_with_encoder(PngEncoder::new(&mut out))
                    .map_err(io)?,
                "image/webp" => out = encode_webp(&img, None),
                other => return Err(ImageError::Invalid(format!("cannot normalize {other}"))),
            }
            out
        };
        let (width, height) = image::ImageReader::new(Cursor::new(&encoded))
            .with_guessed_format()
            .map_err(io)?
            .into_dimensions()
            .map_err(invalid)?;
        let mut f = BufWriter::new(std::fs::File::create(output).map_err(io)?);
        f.write_all(&encoded).map_err(io)?;
        f.into_inner().map_err(io)?.sync_all().map_err(io)?;
        let digest = Sha256::digest(&encoded);
        Ok(NormalizedImage {
            info: ImageInfo { width, height },
            size: encoded.len() as u64,
            sha256: digest.iter().map(|b| format!("{b:02x}")).collect(),
        })
    }

    fn thumbnail(&self, input: &[u8], spec: &ThumbSpec) -> Result<Vec<u8>, ImageError> {
        let img = decode_oriented(input)?;
        let src = if img.color().has_alpha() {
            DynamicImage::ImageRgba8(img.to_rgba8())
        } else {
            DynamicImage::ImageRgb8(img.to_rgb8())
        };
        let (w, h, crop) = plan(src.width(), src.height(), spec);
        let mut dst = DynamicImage::new(w, h, src.color());
        let mut opts =
            ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3));
        if let Some((l, t, cw, ch)) = crop {
            opts = opts.crop(l, t, cw, ch);
        }
        Resizer::new()
            .resize(&src, &mut dst, &opts)
            .map_err(invalid)?;
        Ok(encode_webp(&dst, Some(80.0)))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn spec(s: &str) -> ThumbSpec {
        ThumbSpec::parse(s).unwrap()
    }

    #[test]
    fn plans() {
        assert_eq!(
            plan(400, 200, &spec("100x100")),
            (100, 100, Some((100.0, 0.0, 200.0, 200.0)))
        );
        assert_eq!(
            plan(200, 400, &spec("100x100t")),
            (100, 100, Some((0.0, 0.0, 200.0, 200.0)))
        );
        assert_eq!(
            plan(200, 400, &spec("100x100b")),
            (100, 100, Some((0.0, 200.0, 200.0, 200.0)))
        );
        assert_eq!(plan(400, 200, &spec("100x100f")), (100, 50, None));
        assert_eq!(plan(400, 200, &spec("0x50")), (100, 50, None));
        assert_eq!(plan(400, 200, &spec("300x0")), (300, 150, None));
    }
}
