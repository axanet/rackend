//! Serving the embedded admin UI shell at `/_/` (SPEC §16).

use std::borrow::Cow;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{self, HeaderValue};
use axum::response::{IntoResponse, Redirect, Response};

use crate::AppState;
use crate::problem::{ApiError, ProblemCode};

/// Content Security Policy of the admin UI (SPEC §17.3).
pub const ADMIN_CSP: &str = "default-src 'self'; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'";

/// One embedded file.
#[derive(Debug, Clone)]
pub struct UiFile {
    /// File contents.
    pub data: Cow<'static, [u8]>,
    /// Media type.
    pub mime: &'static str,
}

/// Source of the admin UI bundle. Implemented by the binary (embedded assets).
pub trait UiAssets: Send + Sync + std::fmt::Debug {
    /// The file at `path` (relative, no leading slash), if it exists.
    fn get(&self, path: &str) -> Option<UiFile>;
}

pub(crate) async fn redirect_to_ui() -> Redirect {
    Redirect::permanent("/_/")
}

pub(crate) async fn index(State(state): State<AppState>) -> Result<Response, ApiError> {
    serve(&state, "index.html")
}

pub(crate) async fn asset(
    State(state): State<AppState>,
    Path(path): Path<String>,
) -> Result<Response, ApiError> {
    serve(&state, &path)
}

fn serve(state: &AppState, path: &str) -> Result<Response, ApiError> {
    let ui = state
        .ui()
        .ok_or_else(|| ApiError::new(ProblemCode::NotFound))?;
    if path.contains('\\') || path.split('/').any(|seg| seg == ".." || seg == ".") {
        return Err(ApiError::new(ProblemCode::NotFound));
    }
    let (file, name) = match ui.get(path) {
        Some(f) => (f, path),
        // SPA fallback for client-side routes; missing assets stay 404.
        None if !last_segment_has_extension(path) => (
            ui.get("index.html")
                .ok_or_else(|| ApiError::new(ProblemCode::NotFound))?,
            "index.html",
        ),
        None => return Err(ApiError::new(ProblemCode::NotFound)),
    };
    let cache = if name.starts_with("_app/immutable/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    let bytes = file.data.into_owned();
    let body_text = if file.mime.starts_with("text/html") {
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::new()
    };
    let mut res = Body::from(bytes).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(file.mime));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    if file.mime.starts_with("text/html") {
        let csp = admin_csp(&body_text);
        if let Ok(v) = HeaderValue::from_str(&csp) {
            h.insert(header::CONTENT_SECURITY_POLICY, v);
        }
    } else {
        h.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(ADMIN_CSP),
        );
    }
    Ok(res)
}

/// The admin CSP for an HTML page: [`ADMIN_CSP`] plus a `script-src` that
/// allows exactly the inline scripts of this page by hash. SvelteKit boots
/// the SPA with one inline script; `'unsafe-inline'` is never used (ADR 0005).
pub fn admin_csp(html: &str) -> String {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};

    let mut sources = vec!["'self'".to_owned()];
    for script in inline_scripts(html) {
        let digest = Sha256::digest(script.as_bytes());
        sources.push(format!(
            "'sha256-{}'",
            base64::engine::general_purpose::STANDARD.encode(digest)
        ));
    }
    format!("{ADMIN_CSP}; script-src {}", sources.join(" "))
}

/// Bodies of `<script>` elements without a `src` attribute.
fn inline_scripts(html: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(start) = rest.find("<script") {
        let after = &rest[start..];
        let Some(tag_end) = after.find('>') else {
            break;
        };
        let tag = &after[..tag_end];
        let body_start = start + tag_end + 1;
        let Some(close) = rest[body_start..].find("</script>") else {
            break;
        };
        if !tag.contains(" src=") && !tag.contains("\tsrc=") {
            out.push(&rest[body_start..body_start + close]);
        }
        rest = &rest[body_start + close + "</script>".len()..];
    }
    out
}

fn last_segment_has_extension(path: &str) -> bool {
    path.rsplit('/').next().is_some_and(|seg| seg.contains('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_inline_scripts_only() {
        let html = "<script src=\"/a.js\"></script><script>\n  boot();\n</script>";
        assert_eq!(inline_scripts(html), vec!["\n  boot();\n"]);
        let csp = admin_csp(html);
        assert!(csp.starts_with(ADMIN_CSP));
        assert!(csp.contains("script-src 'self' 'sha256-"));
        assert!(!csp.contains("unsafe-inline'; script"));
    }
}
