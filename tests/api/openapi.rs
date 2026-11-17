//! M7: the OpenAPI document (SPEC §10.12). Validation and generated clients
//! are checked end to end by scripts/openapi-clients.sh.

use axum::http::{Method, StatusCode};
use serde_json::{Value, json};

use crate::common::*;

async fn create_collection(t: &TestApp, body: Value) -> Value {
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections",
        Some(&t.token),
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{out}");
    out
}

async fn fixture(t: &TestApp) {
    let authors = create_collection(
        t,
        json!({"name": "authors", "fields": [{"name": "name", "type": "text"}], "rules": {"list": "", "view": ""}}),
    )
    .await;
    create_collection(
        t,
        json!({
            "name": "posts",
            "fields": [
                {"name": "title", "type": "text", "required": true, "max": 120},
                {"name": "views", "type": "number", "integer_only": true},
                {"name": "pinned", "type": "bool"},
                {"name": "status", "type": "select", "values": ["draft", "published"]},
                {"name": "tags", "type": "select", "values": ["a", "b"], "max_select": 2},
                {"name": "meta", "type": "json"},
                {"name": "author", "type": "relation", "collection_id": authors["id"]},
                {"name": "cover", "type": "file"},
            ],
            "rules": {"list": "", "view": "", "create": "", "update": null, "delete": null},
        }),
    )
    .await;
}

async fn doc(t: &TestApp, query: &str, token: Option<&str>) -> (StatusCode, Value) {
    let (s, _, out) = call(
        &t.router,
        Method::GET,
        &format!("/api/openapi.json{query}"),
        token,
        None,
    )
    .await;
    (s, out)
}

#[tokio::test]
async fn document_access() {
    let t = start().await;
    assert_eq!(doc(&t, "", None).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(doc(&t, "", Some(&t.token)).await.0, StatusCode::OK);
    let (s, _, _) = call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(json!({"api": {"openapi_public": true}})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(doc(&t, "", None).await.0, StatusCode::OK);
    assert_eq!(
        doc(&t, "?include=admin", None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        doc(&t, "?version=2.0", Some(&t.token)).await.0,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn per_collection_schemas_and_operations() {
    let t = start().await;
    fixture(&t).await;
    let (s, d) = doc(&t, "", Some(&t.token)).await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(d["openapi"], "3.1.0");
    let schemas = &d["components"]["schemas"];
    let rec = &schemas["PostsRecord"]["properties"];
    assert_eq!(rec["views"]["type"], json!(["integer", "null"]));
    assert_eq!(rec["pinned"]["type"], "boolean");
    assert_eq!(rec["tags"]["type"], json!(["array", "null"]));
    assert_eq!(
        rec["tags"]["items"]["$ref"],
        "#/components/schemas/PostsTags"
    );
    assert_eq!(schemas["PostsTags"]["enum"], json!(["a", "b"]));
    assert_eq!(rec["author"]["format"], "uuid");
    assert_eq!(rec["meta"], json!({}));
    assert_eq!(
        schemas["PostsExpand"]["properties"]["author"]["$ref"],
        "#/components/schemas/AuthorsRecord"
    );
    assert_eq!(schemas["PostsCreate"]["required"], json!(["title"]));
    assert!(
        schemas["PostsUpdate"]["description"]
            .as_str()
            .unwrap()
            .contains("`tags+` appends")
    );
    assert_eq!(
        schemas["PostsMultipart"]["properties"]["cover"]["format"],
        "binary"
    );
    assert!(schemas["UsersCreate"]["properties"]["password"]["writeOnly"] == true);

    let ops: Vec<String> = d["paths"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|p| {
            p.as_object()
                .unwrap()
                .values()
                .map(|o| o["operationId"].as_str().unwrap().to_owned())
        })
        .collect();
    let mut unique = ops.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), ops.len(), "operation ids are unique");
    for id in [
        "posts_list",
        "posts_get",
        "posts_create",
        "posts_update",
        "posts_delete",
        "users_auth_with_password",
        "health_get",
        "files_get",
    ] {
        assert!(ops.iter().any(|o| o == id), "{id} missing");
    }
    assert!(
        !ops.iter().any(|o| o == "collections_list"),
        "admin endpoints only with include=admin"
    );
    assert_eq!(
        d["paths"]["/api/collections/posts/records"]["post"]["responses"]["201"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/PostsRecord"
    );

    // The public document leaves out what only superusers may call.
    call(
        &t.router,
        Method::PATCH,
        "/api/settings",
        Some(&t.token),
        Some(json!({"api": {"openapi_public": true}})),
    )
    .await;
    let (_, public) = doc(&t, "", None).await;
    let posts = &public["paths"]["/api/collections/posts/records/{id}"];
    assert!(
        posts.get("get").is_some() && posts.get("patch").is_none() && posts.get("delete").is_none(),
        "{posts}"
    );

    let (_, admin) = doc(&t, "?include=admin", Some(&t.token)).await;
    assert!(admin["paths"]["/api/collections"]["get"]["operationId"] == "collections_list");
}

#[tokio::test]
async fn version_30_and_yaml_are_deterministic() {
    let t = start().await;
    fixture(&t).await;
    let a = send(
        &t.router,
        request(
            Method::GET,
            "/api/openapi.json?version=3.0",
            Some(&t.token),
            None,
        ),
    )
    .await;
    let b = send(
        &t.router,
        request(
            Method::GET,
            "/api/openapi.json?version=3.0",
            Some(&t.token),
            None,
        ),
    )
    .await;
    assert_eq!(a.body, b.body, "deterministic");
    let d = a.json();
    assert_eq!(d["openapi"], "3.0.3");
    let text = String::from_utf8(a.body).unwrap();
    assert!(!text.contains("\"null\""), "no 3.1 null types in 3.0");
    let rec = &d["components"]["schemas"]["PostsRecord"]["properties"];
    assert_eq!(rec["views"], json!({"type": "integer", "nullable": true}));
    assert_eq!(
        rec["status"],
        json!({"allOf": [{"$ref": "#/components/schemas/PostsStatus"}], "nullable": true})
    );
    // Generators use the first media type: multipart for collections with files.
    let body = &d["paths"]["/api/collections/posts/records"]["post"]["requestBody"]["content"];
    assert!(body.get("multipart/form-data").is_some() && body.get("application/json").is_none());

    let y = send(
        &t.router,
        request(Method::GET, "/api/openapi.yaml", Some(&t.token), None),
    )
    .await;
    assert_eq!(y.status, StatusCode::OK);
    assert_eq!(y.headers["content-type"], "application/yaml");
    assert!(String::from_utf8_lossy(&y.body).starts_with("\"openapi\": \"3.1.0\"\n\"info\":"));
}

#[tokio::test]
async fn multipart_text_parts_are_typed() {
    let t = start().await;
    fixture(&t).await;
    let boundary = "x-boundary";
    let mut body = String::new();
    for (k, v) in [
        ("title", "typed"),
        ("views", "42"),
        ("pinned", "True"),
        ("tags", "a"),
        ("tags", "b"),
        ("meta", "{\"k\":[1,2]}"),
    ] {
        body.push_str(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n"
        ));
    }
    body.push_str(&format!("--{boundary}--\r\n"));
    let req = axum::http::Request::builder()
        .method(Method::POST)
        .uri("/api/collections/posts/records")
        .header("authorization", format!("Bearer {}", t.token))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(axum::body::Body::from(body))
        .unwrap();
    let r = send(&t.router, req).await;
    assert_eq!(
        r.status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&r.body)
    );
    let rec = r.json();
    assert_eq!(rec["views"], 42);
    assert_eq!(rec["pinned"], true);
    assert_eq!(rec["tags"], json!(["a", "b"]));
    assert_eq!(rec["meta"], json!({"k": [1, 2]}));
}
