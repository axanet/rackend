//! Examples from the rules guide (https://rackend.axanet.ch/docs/guides/rules/), run against the real stack so the
//! documented semantics cannot drift.

use axum::http::{Method, StatusCode};
use serde_json::json;

use crate::common::*;

#[tokio::test]
async fn rules_guide_list_table() {
    let t = start().await;
    let (s, _, out) = call(
        &t.router,
        Method::POST,
        "/api/collections",
        Some(&t.token),
        Some(json!({"name": "items", "fields": [
            {"name": "name", "type": "text"},
            {"name": "tags", "type": "select", "values": ["a", "b"], "max_select": 2},
        ], "rules": {"list": ""}})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{out}");
    for (name, tags) in [
        ("ab", json!(["a", "b"])),
        ("a", json!(["a"])),
        ("none", json!([])),
    ] {
        let (s, _, out) = call(
            &t.router,
            Method::POST,
            "/api/collections/items/records",
            Some(&t.token),
            Some(json!({"name": name, "tags": tags})),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{out}");
    }
    // filter → names that match, as in the table of the guide.
    let table = [
        ("tags ?= 'a'", vec!["a", "ab"]),
        ("tags = 'a'", vec!["a", "none"]),
        ("tags ?!= 'a'", vec!["ab"]),
        ("tags != 'a'", vec!["none"]),
        ("tags:length > 0 && tags = 'a'", vec!["a"]),
    ];
    for (filter, expected) in table {
        let uri = format!(
            "/api/collections/items/records?sort=name&filter={}",
            filter
                .replace(' ', "%20")
                .replace('\'', "%27")
                .replace('&', "%26")
                .replace('?', "%3F")
                .replace('!', "%21")
                .replace('=', "%3D")
                .replace('>', "%3E")
                .replace(':', "%3A")
        );
        let (s, _, out) = call(&t.router, Method::GET, &uri, None, None).await;
        assert_eq!(s, StatusCode::OK, "{filter}: {out}");
        let names: Vec<&str> = out["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["name"].as_str())
            .collect();
        assert_eq!(names, expected, "{filter}");
    }
}
