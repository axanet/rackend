//! End-to-end API flows for M1.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::common::{ADMIN_PASSWORD, call, request, send, start, start_empty};
use crate::error_contract::assert_problem;

#[tokio::test]
async fn setup_flow_creates_first_superuser_once() {
    let (app, router, _dir) = start_empty().await;
    let (_, _, info) = call(&router, Method::GET, "/api/info", None, None).await;
    assert_eq!(info["needs_setup"], true);

    let token = app.setup_token_if_needed().await.unwrap().unwrap();
    let body = json!({"token": "wrong", "email": "a@b.co", "password": ADMIN_PASSWORD, "public_url": "https://rk.example.com/"});
    let (s, _, _) = call(&router, Method::POST, "/api/setup", None, Some(body)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let body = json!({"token": token, "email": "A@B.co", "password": ADMIN_PASSWORD, "public_url": "https://rk.example.com/"});
    let (s, _, out) = call(
        &router,
        Method::POST,
        "/api/setup",
        None,
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["record"]["email"], "a@b.co");
    let access = out["access_token"].as_str().unwrap().to_owned();

    let (_, _, info) = call(&router, Method::GET, "/api/info", None, None).await;
    assert_eq!(info["needs_setup"], false);
    assert_eq!(info["public_url"], "https://rk.example.com");
    let (s, _, _) = call(&router, Method::POST, "/api/setup", None, Some(body)).await;
    assert_eq!(s, StatusCode::CONFLICT, "setup works only once");
    assert!(app.setup_token_if_needed().await.unwrap().is_none());

    let (s, _, settings) = call(&router, Method::GET, "/api/settings", Some(&access), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(settings["app"]["public_url"], "https://rk.example.com");
}

#[tokio::test]
async fn refresh_rotation_reuse_detection_and_logout() {
    let t = start().await;
    let login = |r: &axum::Router| {
        let r = r.clone();
        async move {
            call(
                &r,
                Method::POST,
                "/api/superusers/auth-with-password",
                None,
                Some(json!({"email": "admin@example.com", "password": ADMIN_PASSWORD})),
            )
            .await
            .2
        }
    };
    let first = login(&t.router).await;
    let rt1 = first["refresh_token"].as_str().unwrap().to_owned();
    let (s, _, second) = call(
        &t.router,
        Method::POST,
        "/api/superusers/auth-refresh",
        None,
        Some(json!({"refresh_token": rt1})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let at2 = second["access_token"].as_str().unwrap().to_owned();
    // Grace window: the previous token works once more and returns the same pair.
    let (s, _, again) = call(
        &t.router,
        Method::POST,
        "/api/superusers/auth-refresh",
        None,
        Some(json!({"refresh_token": rt1})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(again["access_token"], second["access_token"]);
    // A second reuse is theft: the session is revoked, and audited.
    let (s, _, _) = call(
        &t.router,
        Method::POST,
        "/api/superusers/auth-refresh",
        None,
        Some(json!({"refresh_token": rt1})),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = call(&t.router, Method::GET, "/api/collections", Some(&at2), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "revoked immediately");
    let (_, _, audit) = call(
        &t.router,
        Method::GET,
        "/api/audit?action=auth.refresh_reuse",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(audit["total_items"], 1);

    // Logout revokes within the same request cycle.
    let other = login(&t.router).await;
    let at = other["access_token"].as_str().unwrap().to_owned();
    let (s, _, _) = call(&t.router, Method::GET, "/api/collections", Some(&at), None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = call(
        &t.router,
        Method::POST,
        "/api/superusers/logout",
        Some(&at),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _, _) = call(&t.router, Method::GET, "/api/collections", Some(&at), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn cookie_mode_with_csrf_origin_check() {
    let t = start().await;
    let r = send(
        &t.router,
        axum::http::Request::builder()
            .method(Method::POST)
            .uri("/api/superusers/auth-with-password")
            .header("content-type", "application/json")
            .header("x-rackend-auth-mode", "cookie")
            .body(axum::body::Body::from(
                json!({"email": "admin@example.com", "password": ADMIN_PASSWORD}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(
        r.json().get("access_token").is_none(),
        "tokens only in cookies"
    );
    let cookies: Vec<String> = r
        .headers
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_owned())
        .collect();
    assert!(
        cookies
            .iter()
            .all(|c| c.contains("HttpOnly") && c.contains("SameSite=Lax") && c.contains("Secure"))
    );
    let session = cookies
        .iter()
        .find(|c| c.starts_with("rk_session="))
        .unwrap();
    let session = session.split(';').next().unwrap().to_owned();

    let with_cookie = |method: Method, origin: Option<&str>| {
        let mut b = axum::http::Request::builder()
            .method(method)
            .uri("/api/collections")
            .header("cookie", &session)
            .header("host", "rk.local");
        if let Some(o) = origin {
            b = b.header("origin", o);
        }
        b.header("content-type", "application/json")
            .body(axum::body::Body::from(json!({"name": "c1"}).to_string()))
            .unwrap()
    };
    assert_eq!(
        send(&t.router, with_cookie(Method::GET, None)).await.status,
        StatusCode::OK
    );
    let r = send(
        &t.router,
        with_cookie(Method::POST, Some("https://evil.example")),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::FORBIDDEN,
        "cross-origin write rejected"
    );
    assert_problem(&r);
    let r = send(&t.router, with_cookie(Method::POST, None)).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "missing origin rejected");
    let r = send(
        &t.router,
        with_cookie(Method::POST, Some("http://rk.local")),
    )
    .await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "same-origin write accepted: {}",
        String::from_utf8_lossy(&r.body)
    );
}

async fn create_blog(t: &crate::common::TestApp) -> (String, String) {
    let (s, _, authors) = call(
        &t.router,
        Method::POST,
        "/api/collections",
        Some(&t.token),
        Some(json!({
            "name": "authors",
            "fields": [{"name": "name", "type": "text", "required": true}],
            "rules": {"list": "", "view": ""}
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{authors}");
    let (s, _, posts) = call(&t.router, Method::POST, "/api/collections", Some(&t.token), Some(json!({
        "name": "posts",
        "fields": [
            {"name": "title", "type": "text", "required": true, "max": 50},
            {"name": "views", "type": "number"},
            {"name": "tags", "type": "select", "values": ["a", "b", "c"], "max_select": 3},
            {"name": "author", "type": "relation", "collection_id": authors["id"], "on_delete": "cascade"},
            {"name": "meta", "type": "json"}
        ],
        "indexes": [{"name": "title", "fields": ["title"], "unique": true}],
        "rules": {"list": "", "view": "", "create": "", "update": "", "delete": ""}
    }))).await;
    assert_eq!(s, StatusCode::CREATED, "{posts}");
    (
        authors["id"].as_str().unwrap().to_owned(),
        posts["id"].as_str().unwrap().to_owned(),
    )
}

#[tokio::test]
async fn records_crud_query_expand_modifiers_and_etag() {
    let t = start().await;
    create_blog(&t).await;
    let (s, _, ada) = call(
        &t.router,
        Method::POST,
        "/api/collections/authors/records",
        Some(&t.token),
        Some(json!({"name": "Ada"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let ada_id = ada["id"].as_str().unwrap();

    for (i, title) in ["Hello", "World", "Rust"].iter().enumerate() {
        let (s, _, p) = call(
            &t.router,
            Method::POST,
            "/api/collections/posts/records",
            None,
            Some(json!({
                "title": title, "views": i, "tags": ["a"], "author": ada_id, "meta": {"n": i}
            })),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{p}");
        assert_eq!(p["collection_name"], "posts");
    }
    // Validation and unique index.
    let (s, _, e) = call(
        &t.router,
        Method::POST,
        "/api/collections/posts/records",
        None,
        Some(json!({"title": "", "nope": 1})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(e["errors"]["title"][0]["code"], "required");
    assert_eq!(e["errors"]["nope"][0]["code"], "unknown_field");
    let (s, _, _) = call(
        &t.router,
        Method::POST,
        "/api/collections/posts/records",
        None,
        Some(json!({"title": "Rust"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _, e) = call(
        &t.router,
        Method::POST,
        "/api/collections/posts/records",
        None,
        Some(json!({"title": "X", "author": uuid::Uuid::now_v7()})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(e["errors"]["author"][0]["code"], "not_found");

    // Filter, sort, pagination, expand, fields.
    let (s, _, list) = call(&t.router, Method::GET, "/api/collections/posts/records?filter=views%20%3E%3D%201&sort=-views&per_page=1&expand=author&fields=title,expand.author.name", None, None).await;
    assert_eq!(s, StatusCode::OK, "{list}");
    assert_eq!(list["total_items"], 2);
    assert_eq!(list["total_pages"], 2);
    assert_eq!(
        list["items"][0],
        json!({"title": "Rust", "expand": {"author": {"name": "Ada"}}})
    );
    let (_, _, list) = call(
        &t.router,
        Method::GET,
        "/api/collections/posts/records?skip_total=1&filter=meta.n%20%3D%202",
        None,
        None,
    )
    .await;
    assert_eq!(list["total_items"], -1);
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let (_, _, back) = call(
        &t.router,
        Method::GET,
        &format!("/api/collections/authors/records/{ada_id}?expand=posts_via_author"),
        None,
        None,
    )
    .await;
    assert_eq!(
        back["expand"]["posts_via_author"].as_array().unwrap().len(),
        3
    );
    let (s, _, e) = call(
        &t.router,
        Method::GET,
        "/api/collections/posts/records?filter=titel%20%3D%201",
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(
        e["detail"]
            .as_str()
            .unwrap()
            .contains("Did you mean 'title'?")
    );
    let (s, _, _) = call(
        &t.router,
        Method::GET,
        "/api/collections/posts/records?filter=%40collection.authors.name%20%3D%20%27x%27",
        None,
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "@collection is not allowed in filters"
    );

    // Modifiers and optimistic concurrency.
    let (_, h, rust) = call(
        &t.router,
        Method::GET,
        "/api/collections/posts/records?filter=title%3D%27Rust%27",
        None,
        None,
    )
    .await;
    let _ = h;
    let id = rust["items"][0]["id"].as_str().unwrap().to_owned();
    let uri = format!("/api/collections/posts/records/{id}");
    let r = send(&t.router, request(Method::GET, &uri, None, None)).await;
    let etag = r.headers.get("etag").unwrap().to_str().unwrap().to_owned();
    let mut req = request(
        Method::PATCH,
        &uri,
        None,
        Some(json!({"views+": 10, "tags+": ["b", "a"]})),
    );
    req.headers_mut().insert("if-match", etag.parse().unwrap());
    let r = send(&t.router, req).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    assert_eq!(r.json()["views"], 12);
    assert_eq!(r.json()["tags"], json!(["a", "b"]));
    let mut stale = request(Method::PATCH, &uri, None, Some(json!({"views": 0})));
    stale
        .headers_mut()
        .insert("if-match", etag.parse().unwrap());
    let r = send(&t.router, stale).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_FAILED);
    assert_problem(&r);

    // Cascade delete of the author removes all posts, audited with a cause.
    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        &format!("/api/collections/authors/records/{ada_id}"),
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "authors delete rule is locked");
    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        &format!("/api/collections/authors/records/{ada_id}"),
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, _, list) = call(
        &t.router,
        Method::GET,
        "/api/collections/posts/records",
        None,
        None,
    )
    .await;
    assert_eq!(list["total_items"], 0);
    let (_, _, audit) = call(
        &t.router,
        Method::GET,
        "/api/audit?action=record.delete",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(audit["total_items"], 4);
    let caused = audit["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["meta"]["cause"].is_string())
        .count();
    assert_eq!(caused, 3);
}

#[tokio::test]
async fn batch_is_atomic() {
    let t = start().await;
    create_blog(&t).await;
    let ok = json!({"requests": [
        {"method": "POST", "url": "/api/collections/posts/records", "body": {"title": "one"}},
        {"method": "POST", "url": "/api/collections/posts/records", "body": {"title": "two"}}
    ]});
    let (s, _, out) = call(&t.router, Method::POST, "/api/batch", None, Some(ok)).await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out.as_array().unwrap().len(), 2);
    let bad = json!({"requests": [
        {"method": "POST", "url": "/api/collections/posts/records", "body": {"title": "three"}},
        {"method": "POST", "url": "/api/collections/posts/records", "body": {"title": "one"}},
        {"method": "POST", "url": "/api/collections/posts/records", "body": {"title": "four"}}
    ]});
    let r = send(
        &t.router,
        request(Method::POST, "/api/batch", None, Some(bad)),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    let p = assert_problem(&r);
    assert_eq!(p["failed_index"], 1);
    assert_eq!(
        p["results"],
        json!([{"status": "rolled_back"}, {"status": 409}, {"status": "skipped"}])
    );
    let (_, _, list) = call(
        &t.router,
        Method::GET,
        "/api/collections/posts/records",
        None,
        None,
    )
    .await;
    assert_eq!(
        list["total_items"], 2,
        "nothing from the failed batch was committed"
    );
}

#[tokio::test]
async fn schema_changes_rename_trash_restore_purge_and_history() {
    let t = start().await;
    let (_, posts_id) = create_blog(&t).await;
    call(
        &t.router,
        Method::POST,
        "/api/collections/posts/records",
        None,
        Some(json!({"title": "keep", "views": 3, "tags": ["a"]})),
    )
    .await;
    let (_, _, posts) = call(
        &t.router,
        Method::GET,
        "/api/collections/posts",
        Some(&t.token),
        None,
    )
    .await;
    let mut fields = posts["fields"].as_array().unwrap().clone();
    // Rename title → headline, drop views, tags multi → single, add a required field.
    for f in &mut fields {
        if f["name"] == "title" {
            f["name"] = json!("headline");
        }
        if f["name"] == "tags" {
            f["max_select"] = json!(1);
        }
    }
    fields.retain(|f| f["name"] != "views");
    fields.push(json!({"name": "summary", "type": "text", "required": true}));
    let change = json!({"fields": fields, "indexes": [{"name": "headline", "fields": ["headline"], "unique": true}]});
    let (s, _, dry) = call(
        &t.router,
        Method::PATCH,
        "/api/collections/posts?dry_run=true",
        Some(&t.token),
        Some(change.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{dry}");
    assert!(dry["ddl"].as_str().unwrap().contains("RENAME COLUMN"));
    let (_, _, still) = call(
        &t.router,
        Method::GET,
        "/api/collections/posts/records",
        None,
        None,
    )
    .await;
    assert_eq!(
        still["items"][0]["title"], "keep",
        "dry run changed nothing"
    );

    let (s, _, updated) = call(
        &t.router,
        Method::PATCH,
        "/api/collections/posts",
        Some(&t.token),
        Some(change),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{updated}");
    assert!(updated["warnings"][0].as_str().unwrap().contains("summary"));
    let (_, _, list) = call(
        &t.router,
        Method::GET,
        "/api/collections/posts/records",
        None,
        None,
    )
    .await;
    let item = &list["items"][0];
    assert_eq!(item["headline"], "keep");
    assert_eq!(item["tags"], "a");
    assert!(item.get("views").is_none());

    // Incompatible type change is rejected.
    let mut bad = updated["fields"].as_array().unwrap().clone();
    bad.iter_mut().find(|f| f["name"] == "headline").unwrap()["type"] = json!("number");
    let (s, _, e) = call(
        &t.router,
        Method::PATCH,
        "/api/collections/posts",
        Some(&t.token),
        Some(json!({"fields": bad})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");

    // Trash: restore views, then purge requires confirmation.
    let (_, _, trash) = call(&t.router, Method::GET, "/api/trash", Some(&t.token), None).await;
    let entry = trash["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["original_name"] == "views")
        .unwrap()
        .clone();
    let tid = entry["id"].as_str().unwrap();
    let (s, _, restored) = call(
        &t.router,
        Method::POST,
        &format!("/api/trash/{tid}/restore"),
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{restored}");
    let (_, _, list) = call(
        &t.router,
        Method::GET,
        "/api/collections/posts/records",
        None,
        None,
    )
    .await;
    assert_eq!(list["items"][0]["views"], 3, "data restored with the field");

    // Rules referencing a field block its removal.
    let (s, _, e) = call(
        &t.router,
        Method::PATCH,
        "/api/collections/posts",
        Some(&t.token),
        Some(json!({"rules": {"list": "views > 1"}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{e}");
    let fields: Vec<Value> = restored["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["name"] != "views")
        .cloned()
        .collect();
    let (s, _, e) = call(
        &t.router,
        Method::PATCH,
        "/api/collections/posts",
        Some(&t.token),
        Some(json!({"fields": fields})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{e}");
    assert_eq!(e["code"], "rule_compile_error");

    // Delete the collection: blocked by nothing here; trash then purge.
    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        "/api/collections/authors",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "posts.author still points to authors"
    );
    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        "/api/collections/posts",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (_, _, trash) = call(&t.router, Method::GET, "/api/trash", Some(&t.token), None).await;
    let coll = trash["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "collection")
        .unwrap()
        .clone();
    let cid = coll["id"].as_str().unwrap();
    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        &format!("/api/trash/{cid}"),
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "purge needs ?confirm=");
    let (s, _, _) = call(
        &t.router,
        Method::DELETE,
        &format!("/api/trash/{cid}?confirm=posts"),
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    let (_, _, history) = call(
        &t.router,
        Method::GET,
        "/api/audit?action=collection.",
        Some(&t.token),
        None,
    )
    .await;
    assert!(history["total_items"].as_u64().unwrap() >= 6);
    let _ = posts_id;
}

#[tokio::test]
async fn export_import_round_trip() {
    let t = start().await;
    create_blog(&t).await;
    let (_, _, export) = call(
        &t.router,
        Method::GET,
        "/api/collections/export",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(export.as_array().unwrap().len(), 2);
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/import?dry_run=true",
        Some(&t.token),
        Some(export.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["dry_run"], true);
    let mut changed = export.clone();
    changed[1]["rules"]["list"] = json!(null);
    changed
        .as_array_mut()
        .unwrap()
        .push(json!({"name": "tags", "fields": [{"name": "label", "type": "text"}]}));
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections/import",
        Some(&t.token),
        Some(changed),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    let (_, _, list) = call(
        &t.router,
        Method::GET,
        "/api/collections",
        Some(&t.token),
        None,
    )
    .await;
    assert_eq!(list["total_items"], 3);
    let (s, _, _) = call(
        &t.router,
        Method::GET,
        "/api/collections/posts/records",
        None,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "list rule is now locked");
}
