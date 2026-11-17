//! M1 acceptance: every write produces exactly one audit row in the same
//! transaction; if the audit insert fails, the change does not happen.

use rackend_core::domain::RuleOp;
use serde_json::{Map, Value, json};

use crate::common::{start, superuser_ctx};

fn body(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

fn db(t: &crate::common::TestApp) -> rusqlite::Connection {
    rusqlite::Connection::open(t.dir.path().join("data.db")).unwrap()
}

fn count(conn: &rusqlite::Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[tokio::test]
async fn each_write_has_exactly_one_audit_row() {
    let t = start().await;
    let su = superuser_ctx();
    let conn = db(&t);
    let audits = |c: &rusqlite::Connection| count(c, "SELECT count(*) FROM _audit");

    let before = audits(&conn);
    t.app
        .collection_create(
            &su,
            json!({"name": "notes", "fields": [{"name": "text", "type": "text"}]}),
            false,
        )
        .await
        .unwrap();
    assert_eq!(audits(&conn) - before, 1, "collection.create");

    let before = audits(&conn);
    let r = t
        .app
        .record_create(&su, "notes", body(json!({"text": "a"})))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(audits(&conn) - before, 1, "record.create");
    let id = r["id"].as_str().unwrap();

    let before = audits(&conn);
    t.app
        .record_update(&su, "notes", id, body(json!({"text": "b"})), None)
        .await
        .unwrap();
    assert_eq!(audits(&conn) - before, 1, "record.update");
    let diff: String = conn
        .query_row(
            "SELECT diff FROM _audit WHERE action = 'record.update'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&diff).unwrap(),
        json!({"text": {"before": "a", "after": "b"}})
    );

    let before = audits(&conn);
    t.app.record_delete(&su, "notes", id, None).await.unwrap();
    assert_eq!(audits(&conn) - before, 1, "record.delete");

    let before = audits(&conn);
    t.app
        .settings_patch(&su, json!({"audit": {"retention_days": 30}}))
        .await
        .unwrap();
    assert_eq!(audits(&conn) - before, 1, "settings.update");
    let _ = RuleOp::List;
}

#[tokio::test]
async fn failing_audit_insert_rolls_back_the_change() {
    let t = start().await;
    let su = superuser_ctx();
    t.app.collection_create(&su, json!({"name": "notes", "fields": [{"name": "text", "type": "text"}], "rules": {"list": ""}}), false).await.unwrap();
    let existing = t
        .app
        .record_create(&su, "notes", body(json!({"text": "keep"})))
        .await
        .unwrap()
        .unwrap();
    let id = existing["id"].as_str().unwrap().to_owned();

    // Fault injection: every audit insert now fails.
    let conn = db(&t);
    conn.execute_batch("CREATE TRIGGER fail_audit BEFORE INSERT ON _audit BEGIN SELECT RAISE(ABORT, 'injected audit failure'); END;").unwrap();

    assert!(
        t.app
            .record_create(&su, "notes", body(json!({"text": "new"})))
            .await
            .is_err()
    );
    assert!(
        t.app
            .record_update(&su, "notes", &id, body(json!({"text": "changed"})), None)
            .await
            .is_err()
    );
    assert!(t.app.record_delete(&su, "notes", &id, None).await.is_err());
    assert!(
        t.app
            .collection_create(&su, json!({"name": "other"}), false)
            .await
            .is_err()
    );
    assert!(
        t.app
            .collection_update(&su, "notes", json!({"fields": []}), false)
            .await
            .is_err()
    );
    assert!(
        t.app
            .settings_patch(&su, json!({"audit": {"retention_days": 1}}))
            .await
            .is_err()
    );

    assert_eq!(
        count(&conn, "SELECT count(*) FROM notes"),
        1,
        "no record inserted or deleted"
    );
    let text: String = conn
        .query_row("SELECT text FROM notes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(text, "keep", "no update");
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM sqlite_master WHERE name = 'other'"
        ),
        0,
        "no table created"
    );
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM pragma_table_info('notes') WHERE name = 'text'"
        ),
        1,
        "no column dropped"
    );
    assert_eq!(count(&conn, "SELECT count(*) FROM _trash"), 0);
    assert_eq!(
        count(&conn, "SELECT count(*) FROM _settings WHERE key = 'audit'"),
        0
    );
}
