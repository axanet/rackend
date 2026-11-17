//! M3 acceptance: uploads, serving with rules, protected files, ranges,
//! thumbnails, cleanup and garbage collection (SPEC §12).

use std::io::Cursor;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use serde_json::{Value, json};

use crate::common::*;

// ---- fixtures ---------------------------------------------------------------

fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let img = RgbImage::from_fn(w, h, |x, _| {
        if x < w / 2 {
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

/// A JPEG with an EXIF segment holding a GPS latitude.
fn jpeg_with_gps() -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"MM\0\x2a\0\0\0\x08");
    t.extend_from_slice(&1u16.to_be_bytes());
    t.extend_from_slice(&[0x88, 0x25, 0, 4, 0, 0, 0, 1]);
    t.extend_from_slice(&26u32.to_be_bytes());
    t.extend_from_slice(&0u32.to_be_bytes());
    t.extend_from_slice(&2u16.to_be_bytes());
    t.extend_from_slice(&[0, 1, 0, 2, 0, 0, 0, 2, b'N', 0, 0, 0]);
    t.extend_from_slice(&[0, 2, 0, 5, 0, 0, 0, 3]);
    t.extend_from_slice(&56u32.to_be_bytes());
    t.extend_from_slice(&0u32.to_be_bytes());
    for (n, d) in [(48u32, 1u32), (8, 1), (3_000, 100)] {
        t.extend_from_slice(&n.to_be_bytes());
        t.extend_from_slice(&d.to_be_bytes());
    }
    let mut seg = vec![0xFF, 0xE1];
    seg.extend_from_slice(&((t.len() + 8) as u16).to_be_bytes());
    seg.extend_from_slice(b"Exif\0\0");
    seg.extend_from_slice(&t);
    let base = jpeg(40, 20);
    let mut out = base[..2].to_vec();
    out.extend_from_slice(&seg);
    out.extend_from_slice(&base[2..]);
    out
}

fn gps_of(bytes: &[u8]) -> Option<String> {
    let e = exif::Reader::new()
        .read_from_container(&mut Cursor::new(bytes))
        .ok()?;
    e.get_field(exif::Tag::GPSLatitude, exif::In::PRIMARY)
        .map(|f| f.display_value().to_string())
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

/// A tiny PNG file that claims `w` x `h` pixels.
fn png_header(w: u32, h: u32) -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = b"IHDR".to_vec();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    png.extend_from_slice(&13u32.to_be_bytes());
    png.extend_from_slice(&ihdr);
    png.extend_from_slice(&crc32(&ihdr).to_be_bytes());
    for ty in [b"IDAT", b"IEND"] {
        png.extend_from_slice(&0u32.to_be_bytes());
        png.extend_from_slice(ty);
        png.extend_from_slice(&crc32(ty).to_be_bytes());
    }
    png
}

// ---- helpers ----------------------------------------------------------------

async fn docs_collection(t: &TestApp, view: Value) {
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections",
        Some(&t.token),
        Some(json!({
            "name": "docs",
            "fields": [
                {"name": "title", "type": "text"},
                {"name": "owner", "type": "text"},
                {"name": "photo", "type": "file", "mime_types": ["image/*"], "thumbs": ["50x50", "0x20"]},
                {"name": "attachments", "type": "file", "max_select": 3, "max_size": 4000},
                {"name": "secret", "type": "file", "protected": true},
            ],
            "rules": {"list": view, "view": view, "create": "title != 'deny'", "update": "", "delete": ""},
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{out}");
}

async fn create_doc(
    t: &TestApp,
    json: Value,
    files: &[(&str, &str, &[u8])],
) -> (StatusCode, Value) {
    let (s, _, out) = call_multipart(
        &t.router,
        Method::POST,
        "/api/collections/docs/records",
        Some(&t.token),
        Some(json),
        files,
    )
    .await;
    (s, out)
}

async fn get_file(t: &TestApp, uri: &str, headers: &[(&str, &str)]) -> Reply {
    let mut b = Request::builder().method(Method::GET).uri(uri);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    send(&t.router, b.body(Body::empty()).unwrap()).await
}

fn stored_blobs(t: &TestApp) -> Vec<String> {
    fn walk(dir: &std::path::Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                walk(&e.path(), out);
            } else {
                out.push(e.path().display().to_string());
            }
        }
    }
    let mut out = Vec::new();
    walk(&t.dir.path().join("storage"), &mut out);
    out
}

// ---- tests ------------------------------------------------------------------

#[tokio::test]
async fn upload_strips_gps_and_serves_with_headers() {
    let t = start().await;
    docs_collection(&t, json!("")).await;
    let original = jpeg_with_gps();
    assert!(gps_of(&original).is_some());
    let (s, rec) = create_doc(
        &t,
        json!({"title": "trip"}),
        &[("photo", "Holiday Photo.JPG", &original)],
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{rec}");
    let name = rec["photo"].as_str().unwrap().to_owned();
    assert!(
        name.starts_with("holiday_photo_") && name.ends_with(".jpg"),
        "{name}"
    );
    let url = format!("/api/files/docs/{}/{name}", rec["id"].as_str().unwrap());

    let r = get_file(&t, &url, &[]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["content-type"], "image/jpeg");
    assert!(
        r.headers["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("inline;")
    );
    assert!(
        r.headers["content-disposition"]
            .to_str()
            .unwrap()
            .contains("Holiday%20Photo.JPG")
    );
    assert_eq!(
        r.headers["cache-control"], "public, max-age=86400",
        "anyone may view this record"
    );
    assert_eq!(r.headers["x-content-type-options"], "nosniff");
    assert!(
        r.headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("sandbox")
    );
    assert_eq!(gps_of(&r.body), None, "GPS is stripped");
    assert!(!r.body.windows(6).any(|w| w == b"Exif\0\0"));
    let etag = r.headers["etag"].to_str().unwrap().to_owned();

    // Conditional and partial requests.
    let r = get_file(&t, &url, &[("if-none-match", &etag)]).await;
    assert_eq!(r.status, StatusCode::NOT_MODIFIED);
    let full = get_file(&t, &url, &[]).await.body;
    let r = get_file(&t, &url, &[("range", "bytes=0-9")]).await;
    assert_eq!(r.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(r.body, &full[..10]);
    assert_eq!(
        r.headers["content-range"],
        format!("bytes 0-9/{}", full.len())
    );
    let r = get_file(
        &t,
        &url,
        &[("range", &format!("bytes={}-", full.len() + 5))],
    )
    .await;
    assert_eq!(r.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(r.json()["code"], "range_not_satisfiable");
    assert_eq!(
        r.headers["content-range"],
        format!("bytes */{}", full.len())
    );
    let r = get_file(&t, &format!("{url}?download=1"), &[]).await;
    assert!(
        r.headers["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );

    // Unknown names and records are 404.
    let r = get_file(
        &t,
        &format!(
            "/api/files/docs/{}/nope_abcdefgh.jpg",
            rec["id"].as_str().unwrap()
        ),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn uploads_are_validated() {
    let t = start().await;
    docs_collection(&t, json!("")).await;
    let before = stored_blobs(&t);

    // 60 megapixels: refused from the header alone.
    let (s, out) = create_doc(
        &t,
        json!({"title": "big"}),
        &[("photo", "huge.png", &png_header(8000, 7500))],
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");
    assert_eq!(out["errors"]["photo"][0]["code"], "too_many_pixels");

    // The type comes from the bytes, not the name.
    let (s, out) = create_doc(
        &t,
        json!({"title": "x"}),
        &[("photo", "cat.png", b"<html><script>alert(1)</script>")],
    )
    .await;
    assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{out}");
    assert_eq!(out["errors"]["photo"][0]["params"]["type"], "text/html");

    // Size limit per field: 413 with the limit.
    let (s, out) = create_doc(
        &t,
        json!({"title": "x"}),
        &[("attachments", "a.bin", &vec![7u8; 5000])],
    )
    .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{out}");
    assert_eq!(out["max_bytes"], 4000);

    // Single-file fields take one file; unknown and non-file fields refuse files.
    let img = jpeg(4, 4);
    let (s, _) = create_doc(
        &t,
        json!({"title": "x"}),
        &[("photo", "a.jpg", &img), ("photo", "b.jpg", &img)],
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = create_doc(&t, json!({"title": "x"}), &[("title", "a.jpg", &img)]).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // Too many files for a multi field.
    let (s, out) = create_doc(
        &t,
        json!({"title": "x"}),
        &[
            ("attachments", "1.txt", b"1"),
            ("attachments", "2.txt", b"2"),
            ("attachments", "3.txt", b"3"),
            ("attachments", "4.txt", b"4"),
        ],
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");

    // JSON cannot name files that were never uploaded.
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/docs/records",
        Some(&t.token),
        Some(json!({"title": "x", "photo": "evil_abcdefgh.jpg"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");
    let (s, _, out) = call(&t.router, Method::POST, "/api/batch", Some(&t.token), Some(json!({"requests": [{"method": "POST", "url": "/api/collections/docs/records", "body": {"title": "x", "photo": "evil_abcdefgh.jpg"}}]}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");

    assert_eq!(
        stored_blobs(&t),
        before,
        "nothing was stored for refused uploads"
    );
}

#[tokio::test]
async fn a_failed_transaction_leaves_no_orphan() {
    let t = start().await;
    docs_collection(&t, json!("")).await;
    // The insert fails (duplicate id) after the blob was stored.
    let (s, first) = create_doc(&t, json!({"title": "first"}), &[]).await;
    assert_eq!(s, StatusCode::CREATED, "{first}");
    let (s, out) = create_doc(
        &t,
        json!({"title": "dup", "id": first["id"]}),
        &[("attachments", "a.txt", b"hello")],
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{out}");
    assert!(
        stored_blobs(&t).is_empty(),
        "compensated right away: {:?}",
        stored_blobs(&t)
    );
    // Same when the create rule rejects an anonymous caller after the upload.
    let (s, _, out) = call_multipart(
        &t.router,
        Method::POST,
        "/api/collections/docs/records",
        None,
        Some(json!({"title": "deny"})),
        &[("attachments", "a.txt", b"hello")],
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{out}");
    assert!(stored_blobs(&t).is_empty());

    // A crash between blob and transaction leaves a stray blob: the GC
    // removes it, and keeps blobs that are referenced.
    let (s, rec) = create_doc(
        &t,
        json!({"title": "keep"}),
        &[("attachments", "k.txt", b"keep me")],
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{rec}");
    let c_id = t
        .app
        .schema
        .get()
        .schema
        .by_id_or_name("docs")
        .unwrap()
        .id
        .to_string();
    let stray = format!("{c_id}/{}/stray_abcdefgh.txt", uuid::Uuid::now_v7());
    t.app
        .blobs()
        .put_bytes(&stray, b"orphan".to_vec(), "text/plain")
        .await
        .unwrap();
    let report = t.app.files_gc(Duration::from_secs(3600)).await.unwrap();
    assert_eq!(report["orphans"], 0, "young orphans are left alone");
    let report = t.app.files_gc(Duration::ZERO).await.unwrap();
    assert_eq!(report["orphans"], 1, "{report}");
    assert!(t.app.blobs().head(&stray).await.unwrap().is_none());
    assert_eq!(stored_blobs(&t).len(), 1, "the referenced file stays");
}

#[tokio::test]
async fn updates_keep_append_replace_and_clean_up() {
    let t = start().await;
    docs_collection(&t, json!("")).await;
    let (_, rec) = create_doc(
        &t,
        json!({"title": "d"}),
        &[
            ("photo", "a.jpg", &jpeg(8, 8)),
            ("attachments", "1.txt", b"one"),
        ],
    )
    .await;
    let id = rec["id"].as_str().unwrap().to_owned();
    let url = format!("/api/collections/docs/records/{id}");
    let old_photo = rec["photo"].as_str().unwrap().to_owned();

    // Append to a multi field, replace a single one.
    let (s, _, rec) = call_multipart(
        &t.router,
        Method::PATCH,
        &url,
        Some(&t.token),
        None,
        &[
            ("attachments", "2.txt", b"two"),
            ("photo", "b.jpg", &jpeg(8, 8)),
        ],
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rec}");
    let atts: Vec<String> = serde_json::from_value(rec["attachments"].clone()).unwrap();
    assert_eq!(atts.len(), 2);
    assert_ne!(rec["photo"], old_photo.as_str());
    // Remove one with the `-` modifier; adding names via JSON is refused.
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        &url,
        Some(&t.token),
        Some(json!({"attachments": [atts[0].clone(), "evil_abcdefgh.txt"]})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");
    assert_eq!(out["errors"]["attachments"][0]["code"], "unknown_file");
    let (s, _, rec) = call(
        &t.router,
        Method::PATCH,
        &url,
        Some(&t.token),
        Some(json!({"attachments-": [atts[0].clone()]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rec}");
    assert_eq!(rec["attachments"], json!([atts[1].clone()]));

    t.app.files_cleanup().await.unwrap();
    assert_eq!(
        stored_blobs(&t).len(),
        2,
        "the replaced photo and removed attachment are gone"
    );
    let r = get_file(&t, &format!("/api/files/docs/{id}/{old_photo}"), &[]).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // Deleting the record removes its files.
    let (s, _, _) = call(&t.router, Method::DELETE, &url, Some(&t.token), None).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    t.app.files_cleanup().await.unwrap();
    assert!(stored_blobs(&t).is_empty(), "{:?}", stored_blobs(&t));
}

#[tokio::test]
async fn thumbnails_are_made_once_and_only_in_allowed_sizes() {
    let t = start().await;
    docs_collection(&t, json!("")).await;
    let (_, rec) = create_doc(
        &t,
        json!({"title": "t"}),
        &[
            ("photo", "p.jpg", &jpeg(120, 80)),
            ("attachments", "notes.txt", b"text"),
        ],
    )
    .await;
    let base = format!("/api/files/docs/{}", rec["id"].as_str().unwrap());
    let photo = format!("{base}/{}", rec["photo"].as_str().unwrap());

    let r = get_file(&t, &format!("{photo}?thumb=1000x1000"), &[]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST, "only listed sizes");
    let r = get_file(
        &t,
        &format!(
            "{base}/{}?thumb=100x100",
            rec["attachments"][0].as_str().unwrap()
        ),
        &[],
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "no thumbs of non-images");

    // 100 concurrent requests for a new thumb generate it exactly once.
    let before = t.app.thumb_generations();
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..100 {
        let router = t.router.clone();
        let uri = format!("{photo}?thumb=50x50");
        set.spawn(
            async move { send(&router, Request::get(uri).body(Body::empty()).unwrap()).await },
        );
    }
    while let Some(r) = set.join_next().await {
        let r = r.unwrap();
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.headers["content-type"], "image/webp");
        let img = image::load_from_memory(&r.body).unwrap();
        assert_eq!((img.width(), img.height()), (50, 50));
    }
    assert_eq!(t.app.thumb_generations() - before, 1);
    // The default size is always allowed; 0x20 keeps the ratio.
    for (spec, w, h) in [("100x100", 100, 100), ("0x20", 30, 20)] {
        let r = get_file(&t, &format!("{photo}?thumb={spec}"), &[]).await;
        assert_eq!(r.status, StatusCode::OK, "{spec}");
        let img = image::load_from_memory(&r.body).unwrap();
        assert_eq!((img.width(), img.height()), (w, h), "{spec}");
    }
}

#[tokio::test]
async fn view_rules_and_protected_files() {
    let t = start().await;
    // Only the owner may view.
    docs_collection(&t, json!("owner = @request.auth.id")).await;
    for email in ["ada@example.com", "bob@example.com"] {
        let (s, _, _) = call(
            &t.router,
            Method::POST,
            "/api/collections/users/records",
            None,
            Some(json!({"email": email, "password": "a long user password"})),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }
    let login = |email: &'static str| {
        let router = t.router.clone();
        async move {
            let (_, _, out) = call(
                &router,
                Method::POST,
                "/api/collections/users/auth-with-password",
                None,
                Some(json!({"email": email, "password": "a long user password"})),
            )
            .await;
            (
                out["access_token"].as_str().unwrap().to_owned(),
                out["record"]["id"].as_str().unwrap().to_owned(),
            )
        }
    };
    let (ada, ada_id) = login("ada@example.com").await;
    let (bob, _) = login("bob@example.com").await;
    let (s, rec) = create_doc(
        &t,
        json!({"title": "mine", "owner": ada_id}),
        &[
            ("photo", "p.jpg", &jpeg(8, 8)),
            ("secret", "s.txt", b"top secret"),
        ],
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{rec}");
    let base = format!("/api/files/docs/{}", rec["id"].as_str().unwrap());
    let photo = format!("{base}/{}", rec["photo"].as_str().unwrap());
    let secret = format!("{base}/{}", rec["secret"].as_str().unwrap());

    let with_bearer = |uri: &str, tok: &str| request(Method::GET, uri, Some(tok), None);
    assert_eq!(
        send(&t.router, request(Method::GET, &photo, None, None))
            .await
            .status,
        StatusCode::NOT_FOUND,
        "anonymous: not viewable"
    );
    assert_eq!(
        send(&t.router, with_bearer(&photo, &bob)).await.status,
        StatusCode::NOT_FOUND,
        "another user"
    );
    let r = send(&t.router, with_bearer(&photo, &ada)).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.headers["cache-control"], "private, max-age=3600");

    // Protected: a bearer alone is not enough; a file token is.
    let r = send(&t.router, with_bearer(&secret, &ada)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let (s, _, tok) = call(
        &t.router,
        Method::POST,
        "/api/files/token",
        Some(&ada),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(tok["expires_in"], 120);
    let token = tok["token"].as_str().unwrap();
    let r = send(
        &t.router,
        request(Method::GET, &format!("{secret}?token={token}"), None, None),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "the token stands in for the user (plain <img src>)"
    );
    assert_eq!(r.body, b"top secret");
    assert_eq!(
        r.headers["content-disposition"]
            .to_str()
            .unwrap()
            .split(';')
            .next(),
        Some("attachment")
    );
    // Bob's token does not open Ada's file; tampered tokens fail.
    let (_, _, bob_tok) = call(
        &t.router,
        Method::POST,
        "/api/files/token",
        Some(&bob),
        None,
    )
    .await;
    let r = send(
        &t.router,
        request(
            Method::GET,
            &format!("{secret}?token={}", bob_tok["token"].as_str().unwrap()),
            None,
            None,
        ),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let mut bad = token.to_owned();
    bad.push('x');
    let r = send(
        &t.router,
        request(Method::GET, &format!("{secret}?token={bad}"), None, None),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let (s, _, _) = call(&t.router, Method::POST, "/api/files/token", None, None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn svg_and_html_are_never_inline() {
    let t = start().await;
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections",
        Some(&t.token),
        Some(json!({
            "name": "raw", "fields": [{"name": "f", "type": "file"}],
            "rules": {"list": "", "view": "", "create": "", "update": null, "delete": null},
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{out}");
    for (name, body, mime) in [
        (
            "x.svg",
            &b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>"[..],
            "image/svg+xml",
        ),
        (
            "x.html",
            &b"<!doctype html><script>alert(1)</script>"[..],
            "text/html",
        ),
    ] {
        let (s, _, rec) = call_multipart(
            &t.router,
            Method::POST,
            "/api/collections/raw/records",
            None,
            None,
            &[("f", name, body)],
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{rec}");
        let r = get_file(
            &t,
            &format!(
                "/api/files/raw/{}/{}",
                rec["id"].as_str().unwrap(),
                rec["f"].as_str().unwrap()
            ),
            &[],
        )
        .await;
        assert_eq!(r.headers["content-type"], mime);
        assert!(
            r.headers["content-disposition"]
                .to_str()
                .unwrap()
                .starts_with("attachment;"),
            "{name}"
        );
        assert!(
            r.headers["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("sandbox")
        );
    }
}

#[tokio::test]
async fn purging_a_collection_removes_its_files() {
    let t = start().await;
    docs_collection(&t, json!("")).await;
    create_doc(&t, json!({"title": "a"}), &[("attachments", "a.txt", b"a")]).await;
    assert_eq!(stored_blobs(&t).len(), 1);
    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        "/api/collections/docs",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    assert_eq!(
        stored_blobs(&t).len(),
        1,
        "trashed collections keep their files"
    );
    let (_, _, trash) = call(&t.router, Method::GET, "/api/trash", Some(&t.token), None).await;
    let tid = trash["items"][0]["id"].as_str().unwrap().to_owned();
    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        &format!("/api/trash/{tid}?confirm=docs"),
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    t.app.files_cleanup().await.unwrap();
    assert!(stored_blobs(&t).is_empty());
}

#[tokio::test]
async fn storage_settings_secrets_and_backend_switch() {
    let t = start().await;
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/settings/test-storage",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["backend"], "fs");
    assert_eq!(out["steps"].as_array().unwrap().len(), 4);

    // The S3 secret is encrypted at rest, masked when read, kept when sent back masked.
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(json!({"storage": {"s3": {"bucket": "b", "secret_access_key": "very-secret-value"}}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(
        out["storage"]["s3"]["secret_access_key"],
        "\u{2022}\u{2022}\u{2022}\u{2022}"
    );
    assert_eq!(
        out["storage"]["s3"]["force_path_style"], true,
        "partial updates keep other keys"
    );
    let raw: String = rusqlite::Connection::open(t.dir.path().join("data.db"))
        .unwrap()
        .query_row(
            "SELECT value FROM _settings WHERE key = 'storage'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        !raw.contains("very-secret-value") && raw.contains("enc:v1:"),
        "{raw}"
    );
    let (s, _, out) = call(&t.router, Method::PATCH, "/api/settings", Some(&t.token), Some(json!({"storage": {"s3": {"secret_access_key": "\u{2022}\u{2022}\u{2022}\u{2022}", "region": "eu"}}}))).await;
    assert_eq!(s, StatusCode::OK, "{out}");
    let (_, _, audit) = call(
        &t.router,
        Method::GET,
        "/api/audit?action=settings.update",
        Some(&t.token),
        None,
    )
    .await;
    assert!(!audit.to_string().contains("very-secret-value"));
    assert!(
        !audit.to_string().contains("enc:v1:"),
        "no ciphertext in the audit trail either"
    );

    // Switching backends with stored files needs ?force=true.
    docs_collection(&t, json!("")).await;
    create_doc(&t, json!({"title": "a"}), &[("attachments", "a.txt", b"a")]).await;
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(json!({"storage": {"backend": "s3"}})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{out}");
    let (s, _, out) = call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(json!({"storage": {"backend": "bogus"}})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{out}");
    let (_, _, usage) = call(
        &t.router,
        Method::GET,
        "/api/files/usage",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(usage["files"], 1);
    assert_eq!(usage["backend"], "fs");
}

#[tokio::test]
async fn json_bodies_still_work_and_limits_apply() {
    let t = start().await;
    docs_collection(&t, json!("")).await;
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/docs/records",
        Some(&t.token),
        Some(json!({"title": "plain"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{out}");
    // Browsers send Blob parts with a filename; @json is still JSON.
    let r = send(&t.router, {
        let boundary = "b1";
        let body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"@json\"; filename=\"blob\"\r\nContent-Type: application/json\r\n\r\n{{\"title\":\"blob json\"}}\r\n--{boundary}--\r\n");
        Request::post("/api/collections/docs/records")
            .header("authorization", format!("Bearer {}", t.token))
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .body(Body::from(body))
            .unwrap()
    })
    .await;
    assert_eq!(r.status, StatusCode::CREATED, "{:?}", r.json());
    assert_eq!(r.json()["title"], "blob json");
    // A text part outside @json sets a field too.
    let r = send(&t.router, {
        let boundary = "b0";
        let body = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nfrom form\r\n--{boundary}--\r\n");
        Request::post("/api/collections/docs/records")
            .header("authorization", format!("Bearer {}", t.token))
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .body(Body::from(body))
            .unwrap()
    })
    .await;
    assert_eq!(r.status, StatusCode::CREATED);
    assert_eq!(r.json()["title"], "from form");
    // Wrong content type.
    let r = send(
        &t.router,
        Request::post("/api/collections/docs/records")
            .header("authorization", format!("Bearer {}", t.token))
            .header("content-type", "text/plain")
            .body(Body::from("x"))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    // The JSON limit still applies to JSON bodies.
    let big = json!({"title": "x".repeat(2 * 1024 * 1024)});
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/docs/records",
        Some(&t.token),
        Some(big),
    )
    .await;
    assert_eq!(s, StatusCode::PAYLOAD_TOO_LARGE, "{out}");
    let _ = Value::Null;
}
