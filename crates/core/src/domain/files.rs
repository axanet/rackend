//! Files (SPEC §12): content sniffing, stored filenames, storage keys and
//! thumbnail specs. Pure functions; the I/O lives in the adapters.

use std::fmt;

use uuid::Uuid;

/// Images larger than this many pixels are refused (decompression bombs).
pub const MAX_PIXELS: u64 = 50_000_000;

/// Default `max_size` of a file field: 10 MB.
pub const DEFAULT_MAX_SIZE: u64 = 10 * 1024 * 1024;

/// Thumb size always allowed besides the field's `thumbs`.
pub const DEFAULT_THUMB: ThumbSpec = ThumbSpec {
    width: 100,
    height: 100,
    mode: ThumbMode::Center,
};

/// Largest thumb edge.
pub const MAX_THUMB_EDGE: u32 = 2000;

/// Prefix of cached thumbnails in the blob store.
pub const THUMBS_PREFIX: &str = "_thumbs/";

/// How a thumbnail fits its box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThumbMode {
    /// Crop to the box, centered (`100x100`).
    Center,
    /// Crop to the box, keeping the top (`100x100t`).
    Top,
    /// Crop to the box, keeping the bottom (`100x100b`).
    Bottom,
    /// Fit inside the box without cropping (`100x100f`).
    Fit,
}

/// A thumbnail size: `WxH[t|b|f]`; one side may be 0 (keep the ratio).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ThumbSpec {
    /// Width in pixels (0: from the height and the ratio).
    pub width: u32,
    /// Height in pixels (0: from the width and the ratio).
    pub height: u32,
    /// Fit mode.
    pub mode: ThumbMode,
}

impl ThumbSpec {
    /// Parses `WxH[t|b|f]`.
    pub fn parse(s: &str) -> Option<Self> {
        let (dims, mode) = match s.as_bytes().last()? {
            b't' => (&s[..s.len() - 1], ThumbMode::Top),
            b'b' => (&s[..s.len() - 1], ThumbMode::Bottom),
            b'f' => (&s[..s.len() - 1], ThumbMode::Fit),
            _ => (s, ThumbMode::Center),
        };
        let (w, h) = dims.split_once('x')?;
        let digits =
            |x: &str| !x.is_empty() && x.len() <= 4 && x.bytes().all(|b| b.is_ascii_digit());
        if !digits(w) || !digits(h) {
            return None;
        }
        let (width, height) = (w.parse().ok()?, h.parse().ok()?);
        let ok = (width > 0 || height > 0) && width <= MAX_THUMB_EDGE && height <= MAX_THUMB_EDGE;
        // A 0 side keeps the ratio; modes only make sense with both sides.
        let mode_ok = mode == ThumbMode::Center || (width > 0 && height > 0);
        (ok && mode_ok).then_some(Self {
            width,
            height,
            mode,
        })
    }
}

impl fmt::Display for ThumbSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = match self.mode {
            ThumbMode::Center => "",
            ThumbMode::Top => "t",
            ThumbMode::Bottom => "b",
            ThumbMode::Fit => "f",
        };
        write!(f, "{}x{}{m}", self.width, self.height)
    }
}

/// A row of `_files`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    /// Row id.
    pub id: String,
    /// Owning collection.
    pub collection_id: String,
    /// Owning record.
    pub record_id: String,
    /// Field id.
    pub field: String,
    /// `{collection_id}/{record_id}/{filename}`.
    pub storage_key: String,
    /// Name the client sent.
    pub original_name: String,
    /// Bytes.
    pub size: u64,
    /// Sniffed MIME type.
    pub mime: String,
    /// Pixel width of images.
    pub width: Option<u32>,
    /// Pixel height of images.
    pub height: Option<u32>,
    /// Hex SHA-256 of the stored bytes.
    pub sha256: String,
    /// RFC 3339.
    pub created: String,
    /// Set when the file was removed; the blob goes after commit.
    pub deleted_at: Option<String>,
}

impl FileRow {
    /// The stored filename (last key segment).
    pub fn filename(&self) -> &str {
        self.storage_key.rsplit('/').next().unwrap_or_default()
    }
}

/// The storage key of a stored file.
pub fn storage_key(collection_id: &str, record_id: &str, filename: &str) -> String {
    format!("{collection_id}/{record_id}/{filename}")
}

/// The cache key of a thumbnail.
pub fn thumb_key(storage_key: &str, spec: &ThumbSpec) -> String {
    format!("{THUMBS_PREFIX}{storage_key}/{spec}.webp")
}

/// The prefix under which all thumbnails of a file live.
pub fn thumbs_prefix_of(storage_key: &str) -> String {
    format!("{THUMBS_PREFIX}{storage_key}/")
}

/// Whether thumbnails can be made from this type.
pub fn is_image(mime: &str) -> bool {
    matches!(
        mime,
        "image/jpeg" | "image/png" | "image/webp" | "image/gif"
    )
}

/// Whether browsers may show the file inline. SVG and HTML never are (XSS).
pub fn inline_ok(mime: &str) -> bool {
    (mime.starts_with("image/") && mime != "image/svg+xml")
        || mime == "application/pdf"
        || mime.starts_with("video/")
        || mime.starts_with("audio/")
}

/// Whether `mime` matches one of `patterns` (`image/png`, `image/*`).
/// An empty list allows everything.
pub fn mime_allowed(mime: &str, patterns: &[String]) -> bool {
    patterns.is_empty()
        || patterns.iter().any(|p| match p.strip_suffix("/*") {
            Some(top) => mime.split('/').next() == Some(top),
            None => p == mime,
        })
}

/// Lowercase `[a-z0-9_-]`, at most 100 characters; other characters become
/// `_`, runs collapse. Empty names become `file`.
pub fn sanitize_basename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let stem = match base.rsplit_once('.') {
        Some((s, _)) if !s.is_empty() => s,
        _ => base,
    };
    let mut out = String::new();
    for c in stem.chars().flat_map(char::to_lowercase) {
        let c = if c.is_ascii_alphanumeric() || c == '-' {
            c
        } else {
            '_'
        };
        if c == '_' && out.ends_with('_') {
            continue;
        }
        out.push(c);
        if out.len() >= 100 {
            break;
        }
    }
    let out = out.trim_matches('_').to_owned();
    if out.is_empty() { "file".into() } else { out }
}

/// The extension to store a file under: from the sniffed type when known,
/// else the client's extension (sanitized), else none.
pub fn extension_for(mime: &str, original: &str) -> Option<String> {
    let known = match mime {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "image/avif" => "avif",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/x-icon" => "ico",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/gzip" => "gz",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "audio/wav" => "wav",
        "audio/flac" => "flac",
        "text/html" => "html",
        _ => "",
    };
    if !known.is_empty() {
        return Some(known.into());
    }
    let ext = original.rsplit_once('.')?.1.to_ascii_lowercase();
    (!ext.is_empty() && ext.len() <= 10 && ext.bytes().all(|b| b.is_ascii_alphanumeric()))
        .then_some(ext)
}

/// `{basename}_{suffix}.{ext}`; `suffix` is 8 random `[a-z0-9]` characters.
pub fn stored_filename(original: &str, mime: &str, suffix: &str) -> String {
    let base = sanitize_basename(original);
    match extension_for(mime, original) {
        Some(ext) => format!("{base}_{suffix}.{ext}"),
        None => format!("{base}_{suffix}"),
    }
}

/// Whether `s` has the shape of a stored filename.
pub fn is_stored_filename(s: &str) -> bool {
    let (stem, ext) = match s.rsplit_once('.') {
        Some((stem, ext)) => (stem, Some(ext)),
        None => (s, None),
    };
    let Some((base, suffix)) = stem.rsplit_once('_') else {
        return false;
    };
    let name_ok = |x: &str, max: usize| {
        !x.is_empty()
            && x.len() <= max
            && x.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
    };
    name_ok(base, 100)
        && suffix.len() == 8
        && suffix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && ext.is_none_or(|e| {
            !e.is_empty()
                && e.len() <= 10
                && e.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// Whether a blob key is safe: relative, no `..`, no empty segments.
pub fn is_safe_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 1024
        && !key.starts_with('/')
        && !key.contains('\\')
        && !key.contains('\0')
        && key
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

/// Detects the type from the first bytes. The client's content type is
/// never trusted; the filename only separates text formats.
pub fn sniff_mime(head: &[u8], original_name: &str) -> &'static str {
    let starts = |sig: &[u8]| head.starts_with(sig);
    let at = |off: usize, sig: &[u8]| head.get(off..off + sig.len()) == Some(sig);
    if starts(b"\xFF\xD8\xFF") {
        return "image/jpeg";
    }
    if starts(b"\x89PNG\r\n\x1a\n") {
        return "image/png";
    }
    if starts(b"GIF87a") || starts(b"GIF89a") {
        return "image/gif";
    }
    if starts(b"RIFF") && at(8, b"WEBP") {
        return "image/webp";
    }
    if starts(b"RIFF") && at(8, b"WAVE") {
        return "audio/wav";
    }
    if starts(b"BM") && head.len() > 14 {
        return "image/bmp";
    }
    if starts(b"II*\0") || starts(b"MM\0*") {
        return "image/tiff";
    }
    if starts(b"\0\0\x01\0") {
        return "image/x-icon";
    }
    if starts(b"%PDF-") {
        return "application/pdf";
    }
    if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") {
        return "application/zip";
    }
    if starts(b"\x1f\x8b") {
        return "application/gzip";
    }
    if starts(b"\x1aE\xdf\xa3") {
        return "video/webm";
    }
    if starts(b"OggS") {
        return "audio/ogg";
    }
    if starts(b"fLaC") {
        return "audio/flac";
    }
    if starts(b"ID3") || (head.len() > 1 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0) {
        return "audio/mpeg";
    }
    if at(4, b"ftyp") {
        return match head.get(8..12) {
            Some(b"avif" | b"avis") => "image/avif",
            Some(b"qt  ") => "video/quicktime",
            Some(b"M4A ") => "audio/mp4",
            _ => "video/mp4",
        };
    }
    // Text formats: only when the bytes are valid UTF-8 without NULs.
    let text = std::str::from_utf8(head)
        .or_else(|e| std::str::from_utf8(&head[..e.valid_up_to()]))
        .ok()
        .filter(|t| !t.contains('\0') && (t.len() + 4 >= head.len()));
    let Some(text) = text else {
        return "application/octet-stream";
    };
    let lower = text
        .trim_start_matches('\u{feff}')
        .trim_start()
        .to_ascii_lowercase();
    if lower.starts_with("<svg")
        || ((lower.starts_with("<?xml") || lower.starts_with("<!--")) && lower.contains("<svg"))
    {
        return "image/svg+xml";
    }
    if lower.starts_with("<!doctype html")
        || lower.starts_with("<html")
        || lower.contains("<script")
    {
        return "text/html";
    }
    let ext = original_name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase());
    match ext.as_deref() {
        Some("json") => "application/json",
        Some("csv") => "text/csv",
        Some("md" | "markdown") => "text/markdown",
        _ => "text/plain",
    }
}

/// A new file id.
pub fn new_file_id() -> String {
    Uuid::now_v7().to_string()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn thumb_specs() {
        for s in [
            "100x100", "100x100t", "100x100b", "100x100f", "0x200", "300x0",
        ] {
            assert_eq!(ThumbSpec::parse(s).unwrap().to_string(), s);
        }
        for s in [
            "", "x", "0x0", "100", "100x", "x100", "-1x5", "5000x10", "0x100f", "10x10z", "1e3x1",
        ] {
            assert!(ThumbSpec::parse(s).is_none(), "{s}");
        }
    }

    #[test]
    fn names_and_keys() {
        assert_eq!(sanitize_basename("My Photo (1).JPG"), "my_photo_1");
        assert_eq!(sanitize_basename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_basename("...."), "file");
        assert_eq!(sanitize_basename(&"a".repeat(300)).len(), 100);
        let f = stored_filename("Holiday.jpeg", "image/jpeg", "ab12cd34");
        assert_eq!(f, "holiday_ab12cd34.jpg");
        assert!(is_stored_filename(&f));
        assert!(is_stored_filename("notes_ab12cd34"));
        assert!(!is_stored_filename("holiday.jpg"));
        assert!(!is_stored_filename("../x_ab12cd34.jpg"));
        assert_eq!(
            stored_filename("run.sh", "text/plain", "ab12cd34"),
            "run_ab12cd34.sh"
        );
        assert!(is_safe_key("a/b/c.jpg"));
        for k in ["", "/a", "a/../b", "a//b", "a\\b", "./a"] {
            assert!(!is_safe_key(k), "{k}");
        }
        let spec = ThumbSpec::parse("10x20f").unwrap();
        assert_eq!(
            thumb_key("c/r/f.png", &spec),
            "_thumbs/c/r/f.png/10x20f.webp"
        );
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff_mime(b"\xFF\xD8\xFF\xE0rest", "x.png"), "image/jpeg");
        assert_eq!(sniff_mime(b"\x89PNG\r\n\x1a\nxxxx", "a.txt"), "image/png");
        assert_eq!(sniff_mime(b"RIFF\0\0\0\0WEBPVP8 ", ""), "image/webp");
        assert_eq!(
            sniff_mime(b"<?xml version=\"1.0\"?><svg xmlns=", "a.png"),
            "image/svg+xml"
        );
        assert_eq!(sniff_mime(b"  <!DOCTYPE html><p>", "a.txt"), "text/html");
        assert_eq!(
            sniff_mime(b"hello <script>alert(1)</script>", "a.txt"),
            "text/html"
        );
        assert_eq!(sniff_mime(b"{\"a\":1}", "a.json"), "application/json");
        assert_eq!(sniff_mime(b"plain words", "a.bin"), "text/plain");
        assert_eq!(
            sniff_mime(b"\0\x01\x02\x03binary", "a.txt"),
            "application/octet-stream"
        );
        assert_eq!(sniff_mime(b"\0\0\0\x18ftypmp42", ""), "video/mp4");
        assert_eq!(sniff_mime(b"%PDF-1.7", ""), "application/pdf");
    }

    #[test]
    fn mime_patterns_and_inline() {
        let p = vec!["image/*".to_owned(), "application/pdf".to_owned()];
        assert!(mime_allowed("image/png", &p));
        assert!(mime_allowed("application/pdf", &p));
        assert!(!mime_allowed("text/html", &p));
        assert!(mime_allowed("text/html", &[]));
        assert!(inline_ok("image/png") && inline_ok("video/mp4") && inline_ok("application/pdf"));
        assert!(!inline_ok("image/svg+xml") && !inline_ok("text/html") && !inline_ok("text/plain"));
    }
}
