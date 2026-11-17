//! The admin UI bundle embedded into the binary (SPEC §16, §18.1).
//!
//! `build.rs` points `RACKEND_UI_DIR` at `admin-ui/build` when it exists and
//! at a placeholder page otherwise, so `cargo build` always works.

use std::borrow::Cow;

use rackend_http::{UiAssets, UiFile};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$RACKEND_UI_DIR"]
struct Bundle;

/// Embedded admin UI files.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmbeddedUi;

impl UiAssets for EmbeddedUi {
    fn get(&self, path: &str) -> Option<UiFile> {
        let file = Bundle::get(path)?;
        Some(UiFile {
            data: match file.data {
                Cow::Borrowed(b) => Cow::Borrowed(b),
                Cow::Owned(v) => Cow::Owned(v),
            },
            mime: mime_for(path),
        })
    }
}

/// Media type by file extension (only what a SvelteKit static build emits).
fn mime_for(path: &str) -> &'static str {
    let ext = path
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "txt" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_is_always_embedded() {
        let f = EmbeddedUi.get("index.html");
        assert!(f.is_some_and(|f| f.mime.starts_with("text/html")));
    }

    #[test]
    fn mime_types() {
        assert_eq!(
            mime_for("_app/immutable/x.JS"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(mime_for("noext"), "application/octet-stream");
    }
}
