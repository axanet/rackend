//! File downloads, file tokens and storage administration (SPEC §12.4,
//! §12.2, §10.11).

use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_RANGE,
    CONTENT_SECURITY_POLICY, CONTENT_TYPE, ETAG, IF_NONE_MATCH, LOCATION, RANGE,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use rackend_core::ports::ByteRange;
use rackend_core::usecases::files::FileServe;
use serde_json::Value;

use super::{ApiResult, flag, ok};
use crate::AppState;
use crate::ctx::Ctx;
use crate::problem::{ApiError, ProblemCode};

/// Parses a single `bytes=` range against `size`. `Ok(None)`: serve the
/// whole file (no header, or several ranges, which we may ignore).
fn parse_range(header: Option<&str>, size: u64) -> Result<Option<ByteRange>, ()> {
    let Some(h) = header else { return Ok(None) };
    let Some(spec) = h.trim().strip_prefix("bytes=") else {
        return Ok(None);
    };
    if spec.contains(',') || size == 0 {
        return if size == 0 { Err(()) } else { Ok(None) };
    }
    let (a, b) = spec.split_once('-').ok_or(())?;
    let (a, b) = (a.trim(), b.trim());
    let range = if a.is_empty() {
        // Suffix: the last n bytes.
        let n: u64 = b.parse().map_err(|_| ())?;
        if n == 0 {
            return Err(());
        }
        ByteRange {
            start: size.saturating_sub(n),
            end: size - 1,
        }
    } else {
        let start: u64 = a.parse().map_err(|_| ())?;
        let end = if b.is_empty() {
            size - 1
        } else {
            b.parse::<u64>().map_err(|_| ())?.min(size - 1)
        };
        if start > end || start >= size {
            return Err(());
        }
        ByteRange { start, end }
    };
    Ok(Some(range))
}

/// `Content-Disposition` with an ASCII fallback and an RFC 5987 name.
fn disposition(inline: bool, name: &str) -> HeaderValue {
    let kind = if inline { "inline" } else { "attachment" };
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && c != '"' && c != '\\' || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let encoded: String = name
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    HeaderValue::from_str(&format!(
        "{kind}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

fn common_headers(h: &mut HeaderMap, serve: &FileServe) {
    let etag = format!("\"{}\"", serve.etag);
    if let Ok(v) = HeaderValue::from_str(&etag) {
        h.insert(ETAG, v);
    }
    h.insert(
        CACHE_CONTROL,
        HeaderValue::from_static(if serve.public {
            "public, max-age=86400"
        } else {
            "private, max-age=3600"
        }),
    );
    h.insert(ACCEPT_RANGES, HeaderValue::from_static("bytes"));
}

/// `GET /api/files/{c}/{record_id}/{filename}?thumb=&download=1&token=`.
pub async fn download(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    Path((c, id, filename)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> ApiResult {
    let token = ctx.query.get("token").cloned();
    let thumb = ctx.query.get("thumb").cloned();
    let download = flag(&ctx.query, "download");
    let app = state.app();
    let serve = app
        .file_serve(
            &ctx,
            rackend_core::usecases::files::FileRequest {
                collection: &c,
                record_id: &id,
                filename: &filename,
                token: token.as_deref(),
                thumb: thumb.as_deref(),
                download,
            },
        )
        .await?;
    if let Some(url) = &serve.redirect {
        let mut res = StatusCode::FOUND.into_response();
        if let Ok(v) = HeaderValue::from_str(url) {
            res.headers_mut().insert(LOCATION, v);
        }
        res.headers_mut()
            .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return Ok(res);
    }
    let etag = format!("\"{}\"", serve.etag);
    let not_modified = headers
        .get(IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|t| t.trim() == etag || t.trim() == "*"));
    if not_modified {
        let mut res = StatusCode::NOT_MODIFIED.into_response();
        common_headers(res.headers_mut(), &serve);
        return Ok(res);
    }
    let range = match parse_range(headers.get(RANGE).and_then(|v| v.to_str().ok()), serve.size) {
        Ok(r) => r,
        Err(()) => {
            let mut e = ApiError::with_detail(
                ProblemCode::RangeNotSatisfiable,
                "The requested range is outside the file.",
            );
            e.problem_mut()
                .extra
                .insert("size".into(), Value::from(serve.size));
            let mut res = e.into_response();
            if let Ok(v) = HeaderValue::from_str(&format!("bytes */{}", serve.size)) {
                res.headers_mut().insert(CONTENT_RANGE, v);
            }
            return Ok(res);
        }
    };
    let reader = app.file_read(&serve, range).await?;
    let len = range.map_or(serve.size, |r| r.len());
    let stream = futures_util::stream::unfold(reader.chunks, |mut chunks| async move {
        chunks.next_chunk().await.map(|r| {
            (
                r.map(bytes::Bytes::from).map_err(std::io::Error::other),
                chunks,
            )
        })
    });
    let mut res = Response::new(Body::from_stream(stream));
    let h = res.headers_mut();
    common_headers(h, &serve);
    if let Ok(v) = HeaderValue::from_str(&serve.mime) {
        h.insert(CONTENT_TYPE, v);
    }
    h.insert(CONTENT_LENGTH, HeaderValue::from(len));
    h.insert(
        CONTENT_DISPOSITION,
        disposition(serve.inline, &serve.download_name),
    );
    // Uploaded content never runs script in our origin (SPEC §17.7); PDF
    // viewers do not work inside a sandbox, and PDFs cannot run our script.
    if serve.mime != "application/pdf" {
        h.insert(
            CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("default-src 'none'; img-src 'self' data:; media-src 'self'; style-src 'unsafe-inline'; sandbox"),
        );
    }
    if let Some(r) = range {
        *res.status_mut() = StatusCode::PARTIAL_CONTENT;
        if let Ok(v) = HeaderValue::from_str(&format!("bytes {}-{}/{}", r.start, r.end, serve.size))
        {
            res.headers_mut().insert(CONTENT_RANGE, v);
        }
    }
    Ok(res)
}

/// `POST /api/files/token`: a 2-minute token for protected files.
pub async fn token(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    Ok(ok(state.app().file_token(&ctx)?))
}

/// `GET /api/files/usage`: stored files and bytes (superusers).
pub async fn usage(State(state): State<AppState>, Ctx(ctx): Ctx) -> ApiResult {
    Ok(ok(state.app().files_usage(&ctx).await?))
}

/// `POST /api/settings/test-storage {…partial storage section}`.
pub async fn test_storage(
    State(state): State<AppState>,
    Ctx(ctx): Ctx,
    body: Option<Json<Value>>,
) -> ApiResult {
    Ok(ok(state
        .app()
        .storage_test(&ctx, body.map(|b| b.0))
        .await?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        let r = |h: &str| parse_range(Some(h), 100);
        assert_eq!(r("bytes=0-9"), Ok(Some(ByteRange { start: 0, end: 9 })));
        assert_eq!(r("bytes=90-"), Ok(Some(ByteRange { start: 90, end: 99 })));
        assert_eq!(r("bytes=-10"), Ok(Some(ByteRange { start: 90, end: 99 })));
        assert_eq!(
            r("bytes=50-500"),
            Ok(Some(ByteRange { start: 50, end: 99 }))
        );
        assert_eq!(r("bytes=0-1,5-6"), Ok(None));
        assert_eq!(r("items=0-1"), Ok(None));
        assert_eq!(r("bytes=100-"), Err(()));
        assert_eq!(r("bytes=5-1"), Err(()));
        assert_eq!(r("bytes=-0"), Err(()));
        assert_eq!(parse_range(None, 100), Ok(None));
    }

    #[test]
    fn dispositions() {
        let v = disposition(true, "Résumé \"final\".pdf");
        let s = v.to_str().unwrap_or_default();
        assert!(
            s.starts_with("inline; filename=\"R_sum_ _final_.pdf\""),
            "{s}"
        );
        assert!(
            s.contains("filename*=UTF-8''R%C3%A9sum%C3%A9%20%22final%22.pdf"),
            "{s}"
        );
    }
}
