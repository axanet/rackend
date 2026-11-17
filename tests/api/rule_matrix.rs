//! Rule enforcement matrix (M1 acceptance): every operation × rule kind
//! (public / locked / expression) × caller (anonymous / owner / other user /
//! superuser). Users are synthetic principals until auth collections (M2).

use rackend_core::Error;
use rackend_core::domain::RuleOp;
use rackend_core::usecases::RequestCtx;
use rackend_core::usecases::records::ListParams;
use serde_json::{Map, Value, json};

use crate::common::{anon_ctx, start, superuser_ctx, user_ctx};

const OWNER: &str = "01936b2e-0000-7000-8000-000000000001";
const OTHER: &str = "01936b2e-0000-7000-8000-000000000002";

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Public,
    Locked,
    Expr,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Who {
    Anon,
    Owner,
    Other,
    Super,
}

fn ctx(who: Who) -> RequestCtx {
    match who {
        Who::Anon => anon_ctx(),
        Who::Owner => user_ctx(OWNER),
        Who::Other => user_ctx(OTHER),
        Who::Super => superuser_ctx(),
    }
}

fn rules(op: RuleOp, kind: Kind) -> Value {
    let rule = match kind {
        Kind::Public => json!(""),
        Kind::Locked => Value::Null,
        Kind::Expr => json!("owner = @request.auth.id"),
    };
    let mut r = json!({"list": "", "view": "", "create": "", "update": "", "delete": ""});
    r[op.as_str()] = rule;
    r
}

/// What the caller should observe.
#[derive(Debug, PartialEq)]
enum Outcome {
    Allowed,
    /// List: only the caller's own records.
    OwnOnly,
    Forbidden,
    NotFound,
}

fn expected(op: RuleOp, kind: Kind, who: Who) -> Outcome {
    use Outcome::*;
    if who == Who::Super {
        return Allowed;
    }
    match (kind, op) {
        (Kind::Public, _) => Allowed,
        (Kind::Locked, RuleOp::View) => NotFound,
        (Kind::Locked, _) => Forbidden,
        (Kind::Expr, RuleOp::List) => OwnOnly,
        (Kind::Expr, _) if who == Who::Owner => Allowed,
        (Kind::Expr, RuleOp::View) => NotFound,
        (Kind::Expr, _) => Forbidden,
    }
}

fn body(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[tokio::test]
async fn rule_enforcement_matrix() {
    let t = start().await;
    let su = superuser_ctx();
    let mut n = 0;
    for op in RuleOp::ALL {
        for kind in [Kind::Public, Kind::Locked, Kind::Expr] {
            let name = format!("m_{}_{}", op.as_str(), format!("{kind:?}").to_lowercase());
            t.app
                .collection_create(&su, json!({"name": name, "fields": [{"name": "owner", "type": "text"}, {"name": "n", "type": "number"}], "rules": rules(op, kind)}), false)
                .await
                .unwrap();
            for who in [Who::Anon, Who::Owner, Who::Other, Who::Super] {
                // Fresh records: one owned by OWNER, one by OTHER.
                let mine = t
                    .app
                    .record_create(&su, &name, body(json!({"owner": OWNER})))
                    .await
                    .unwrap()
                    .unwrap();
                let theirs = t
                    .app
                    .record_create(&su, &name, body(json!({"owner": OTHER})))
                    .await
                    .unwrap()
                    .unwrap();
                let mine_id = mine["id"].as_str().unwrap().to_owned();
                let c = ctx(who);
                let want = expected(op, kind, who);
                let got = match op {
                    RuleOp::List => {
                        match t.app.records_list(&c, &name, ListParams::default()).await {
                            Ok(v) => {
                                let ids: Vec<&str> = v["items"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .filter_map(|i| i["owner"].as_str())
                                    .collect();
                                let all = t
                                    .app
                                    .records_list(&su, &name, ListParams::default())
                                    .await
                                    .unwrap()["items"]
                                    .as_array()
                                    .unwrap()
                                    .len();
                                if ids.len() == all {
                                    Outcome::Allowed
                                } else {
                                    let own = match who {
                                        Who::Owner => OWNER,
                                        Who::Other => OTHER,
                                        _ => "",
                                    };
                                    assert!(
                                        ids.iter().all(|o| *o == own),
                                        "{name} {who:?}: {ids:?}"
                                    );
                                    Outcome::OwnOnly
                                }
                            }
                            Err(Error::Forbidden(_)) => Outcome::Forbidden,
                            Err(e) => panic!("{name} {who:?}: {e:?}"),
                        }
                    }
                    RuleOp::View => match t
                        .app
                        .record_view(&c, &name, &mine_id, ListParams::default())
                        .await
                    {
                        Ok(_) => Outcome::Allowed,
                        Err(Error::NotFound) => Outcome::NotFound,
                        Err(e) => panic!("{name} {who:?}: {e:?}"),
                    },
                    RuleOp::Create => {
                        // Everyone claims OWNER: only OWNER (and superusers) may.
                        match t
                            .app
                            .record_create(&c, &name, body(json!({"owner": OWNER})))
                            .await
                        {
                            Ok(_) => Outcome::Allowed,
                            Err(Error::Forbidden(_)) => Outcome::Forbidden,
                            Err(e) => panic!("{name} {who:?}: {e:?}"),
                        }
                    }
                    RuleOp::Update => match t
                        .app
                        .record_update(&c, &name, &mine_id, body(json!({"n": 1})), None)
                        .await
                    {
                        Ok(_) => Outcome::Allowed,
                        Err(Error::Forbidden(_)) => Outcome::Forbidden,
                        Err(Error::NotFound) => Outcome::NotFound,
                        Err(e) => panic!("{name} {who:?}: {e:?}"),
                    },
                    RuleOp::Delete => match t.app.record_delete(&c, &name, &mine_id, None).await {
                        Ok(()) => Outcome::Allowed,
                        Err(Error::Forbidden(_)) => Outcome::Forbidden,
                        Err(Error::NotFound) => Outcome::NotFound,
                        Err(e) => panic!("{name} {who:?}: {e:?}"),
                    },
                };
                assert_eq!(got, want, "{} {kind:?} as {who:?}", op.as_str());
                let _ = theirs;
                n += 1;
            }
        }
    }
    assert_eq!(n, 5 * 3 * 4);
}

#[tokio::test]
async fn realistic_ownership_rules() {
    let t = start().await;
    let su = superuser_ctx();
    t.app
        .collection_create(&su, json!({
            "name": "docs",
            "fields": [{"name": "owner", "type": "text"}, {"name": "public", "type": "bool"}],
            "rules": {
                "list": "public = true || owner = @request.auth.id",
                "view": "public = true || owner = @request.auth.id",
                "create": "@request.auth.id != '' && owner = @request.auth.id",
                "update": "owner = @request.auth.id && (@request.body.owner:isset = false || @request.body.owner = @request.auth.id)",
                "delete": "owner = @request.auth.id"
            }
        }), false)
        .await
        .unwrap();
    let owner = user_ctx(OWNER);
    let doc = t
        .app
        .record_create(&owner, "docs", body(json!({"owner": OWNER})))
        .await
        .unwrap()
        .unwrap();
    let id = doc["id"].as_str().unwrap();
    // Ownership cannot be handed to someone else.
    let err = t
        .app
        .record_update(&owner, "docs", id, body(json!({"owner": OTHER})), None)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Forbidden(_)));
    t.app
        .record_update(&owner, "docs", id, body(json!({"public": true})), None)
        .await
        .unwrap();
    // Public now: others can view but not change.
    t.app
        .record_view(&user_ctx(OTHER), "docs", id, ListParams::default())
        .await
        .unwrap();
    t.app
        .record_view(&anon_ctx(), "docs", id, ListParams::default())
        .await
        .unwrap();
    assert!(matches!(
        t.app
            .record_delete(&user_ctx(OTHER), "docs", id, None)
            .await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        t.app
            .record_create(&anon_ctx(), "docs", body(json!({})))
            .await,
        Err(Error::Forbidden(_))
    ));
}
