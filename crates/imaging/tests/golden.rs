//! Golden tests for normalization and thumbnails (SPEC §12.3, §12.5).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Cursor;

use image::{DynamicImage, ImageFormat, Rgb, RgbImage, Rgba, RgbaImage};
use rackend_core::domain::files::ThumbSpec;
use rackend_core::ports::ImageProcessor;
use rackend_imaging::Imaging;

/// A 40x20 JPEG, left half red and right half blue.
fn jpeg() -> Vec<u8> {
    let img = RgbImage::from_fn(40, 20, |x, _| {
        if x < 20 {
            Rgb([230, 20, 20])
        } else {
            Rgb([20, 20, 230])
        }
    });
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(img)
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg)
        .unwrap();
    out
}

/// An EXIF APP1 segment: orientation 6 (rotate 90 degrees clockwise) and a
/// GPS position (big-endian TIFF, built by hand).
fn exif_app1() -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"MM\0\x2a\0\0\0\x08");
    // IFD0 at 8: 2 entries.
    t.extend_from_slice(&2u16.to_be_bytes());
    // Orientation (0x0112), SHORT, 1, value 6.
    t.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0]);
    // GPSInfo IFD pointer (0x8825), LONG, 1, offset 38.
    t.extend_from_slice(&[0x88, 0x25, 0, 4, 0, 0, 0, 1]);
    t.extend_from_slice(&38u32.to_be_bytes());
    t.extend_from_slice(&0u32.to_be_bytes()); // next IFD
    assert_eq!(t.len(), 38);
    // GPS IFD at 38: 2 entries.
    t.extend_from_slice(&2u16.to_be_bytes());
    // GPSLatitudeRef (1), ASCII, 2, "N\0".
    t.extend_from_slice(&[0, 1, 0, 2, 0, 0, 0, 2, b'N', 0, 0, 0]);
    // GPSLatitude (2), RATIONAL, 3, offset 68.
    t.extend_from_slice(&[0, 2, 0, 5, 0, 0, 0, 3]);
    t.extend_from_slice(&68u32.to_be_bytes());
    t.extend_from_slice(&0u32.to_be_bytes());
    assert_eq!(t.len(), 68);
    for (n, d) in [(48u32, 1u32), (8, 1), (3_000, 100)] {
        t.extend_from_slice(&n.to_be_bytes());
        t.extend_from_slice(&d.to_be_bytes());
    }
    let mut seg = vec![0xFF, 0xE1];
    seg.extend_from_slice(&((t.len() + 8) as u16).to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&t);
    seg
}

fn with_exif(jpeg: &[u8]) -> Vec<u8> {
    let mut out = jpeg[..2].to_vec(); // SOI
    out.extend_from_slice(&exif_app1());
    out.extend_from_slice(&jpeg[2..]);
    out
}

fn gps_of(bytes: &[u8]) -> Option<String> {
    let exif = exif::Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok()?;
    exif.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY)
        .map(|f| f.display_value().to_string())
}

#[test]
fn normalize_strips_gps_and_applies_orientation() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("in.jpg");
    let output = dir.path().join("out.jpg");
    let original = with_exif(&jpeg());
    assert!(gps_of(&original).is_some(), "the fixture carries GPS");
    std::fs::write(&input, &original).unwrap();

    let p = Imaging;
    let probe = p.probe(&input).unwrap().unwrap();
    assert_eq!(
        (probe.width, probe.height),
        (40, 20),
        "probe reports stored dimensions"
    );
    let n = p.normalize(&input, &output, "image/jpeg").unwrap();
    let out = std::fs::read(&output).unwrap();
    assert_eq!(n.size, out.len() as u64);
    assert_eq!(gps_of(&out), None, "GPS is gone");
    assert!(
        !out.windows(6).any(|w| w == b"Exif\0\0"),
        "no EXIF segment at all"
    );
    // Rotated 90 degrees clockwise: 20x40, the red left half is now on top.
    assert_eq!((n.info.width, n.info.height), (20, 40));
    let img = image::load_from_memory(&out).unwrap().to_rgb8();
    let top = img.get_pixel(10, 5);
    let bottom = img.get_pixel(10, 35);
    assert!(top[0] > 150 && top[2] < 100, "top is red: {top:?}");
    assert!(
        bottom[2] > 150 && bottom[0] < 100,
        "bottom is blue: {bottom:?}"
    );
}

#[test]
fn png_metadata_is_dropped_and_alpha_kept() {
    let dir = tempfile::tempdir().unwrap();
    let img = RgbaImage::from_fn(16, 8, |x, _| Rgba([0, 0, 255, if x < 8 { 0 } else { 255 }]));
    let mut png = Vec::new();
    DynamicImage::ImageRgba8(img)
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .unwrap();
    // Insert a tEXt chunk after IHDR (8 signature + 25 IHDR bytes).
    let mut chunk = Vec::new();
    let data = b"Comment\0secret location";
    chunk.extend_from_slice(&(data.len() as u32).to_be_bytes());
    chunk.extend_from_slice(b"tEXt");
    chunk.extend_from_slice(data);
    chunk.extend_from_slice(&[0, 0, 0, 0]); // CRC is not checked by the decoder for ancillary chunks we skip
    let mut with_text = png[..33].to_vec();
    with_text.extend_from_slice(&chunk);
    with_text.extend_from_slice(&png[33..]);
    let input = dir.path().join("in.png");
    let output = dir.path().join("out.png");
    std::fs::write(&input, &with_text).unwrap();
    Imaging.normalize(&input, &output, "image/png").unwrap();
    let out = std::fs::read(&output).unwrap();
    assert!(!out.windows(15).any(|w| w == b"secret location"));
    let decoded = image::load_from_memory(&out).unwrap();
    assert!(decoded.color().has_alpha());
}

#[test]
fn thumbnails_are_webp_of_the_requested_size() {
    let p = Imaging;
    let src = jpeg();
    for (s, w, h) in [
        ("10x10", 10, 10),
        ("10x10t", 10, 10),
        ("20x20f", 20, 10),
        ("0x10", 20, 10),
        ("8x0", 8, 4),
    ] {
        let out = p.thumbnail(&src, &ThumbSpec::parse(s).unwrap()).unwrap();
        assert_eq!(image::guess_format(&out).unwrap(), ImageFormat::WebP, "{s}");
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!((img.width(), img.height()), (w, h), "{s}");
    }
    // Alpha survives.
    let rgba = RgbaImage::from_fn(10, 10, |_, _| Rgba([10, 200, 10, 100]));
    let mut png = Vec::new();
    DynamicImage::ImageRgba8(rgba)
        .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
        .unwrap();
    let out = p
        .thumbnail(&png, &ThumbSpec::parse("5x5").unwrap())
        .unwrap();
    assert!(image::load_from_memory(&out).unwrap().color().has_alpha());
    // Not an image.
    assert!(
        p.thumbnail(b"plain text", &ThumbSpec::parse("5x5").unwrap())
            .is_err()
    );
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// A PNG header that claims `w` x `h` pixels; no pixel data follows.
pub fn png_header(w: u32, h: u32) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = b"IHDR".to_vec();
    chunk.extend_from_slice(&w.to_be_bytes());
    chunk.extend_from_slice(&h.to_be_bytes());
    chunk.extend_from_slice(&[8, 2, 0, 0, 0]);
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(&chunk);
    png.extend_from_slice(&crc32(&chunk).to_be_bytes());
    for ty in [b"IDAT", b"IEND"] {
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(ty);
        png.extend_from_slice(&crc32(ty).to_be_bytes());
    }
    png
}

#[test]
fn oversized_images_are_refused_before_decoding() {
    // A PNG header that claims 8000 x 7500 (60 MP); no pixel data needed.
    let png = png_header(8000, 7500);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("huge.png");
    std::fs::write(&path, &png).unwrap();
    let info = Imaging.probe(&path).unwrap().unwrap();
    assert_eq!((info.width, info.height), (8000, 7500));
    assert!(
        Imaging
            .thumbnail(&png, &ThumbSpec::parse("10x10").unwrap())
            .is_err()
    );
}
