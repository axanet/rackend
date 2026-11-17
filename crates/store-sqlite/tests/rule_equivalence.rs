#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! SPEC §9.6 / M1 acceptance: the in-memory reference evaluator and the SQL
//! compiler give identical results on randomized data and expressions.

use std::collections::HashMap;
use std::time::Duration;

use proptest::prelude::*;
use rackend_core::domain::value::number_json;
use rackend_core::domain::{Collection, DdlStep, Record, RuleOp, Schema};
use rackend_core::rules::eval::{AuthValues, Dataset, RequestValues, evaluate};
use rackend_core::rules::{Mode, compile};
use rackend_store_sqlite::{ddl, records};
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use uuid::Uuid;

/// Tiny deterministic RNG so a failing seed reproduces exactly.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

struct World {
    schema: Schema,
    users: Collection,
    posts: Collection,
    comments: Collection,
}

fn world() -> World {
    let users = Collection::from_input(
        &json!({"name": "users", "fields": [
            {"name": "name", "type": "text"},
            {"name": "age", "type": "number"},
            {"name": "verified", "type": "bool"},
            {"name": "tags", "type": "select", "values": ["a", "b", "c"], "max_select": 3},
            {"name": "meta", "type": "json"}
        ]}),
        None,
    )
    .unwrap();
    let posts = Collection::from_input(
        &json!({"name": "posts", "fields": [
            {"name": "title", "type": "text"},
            {"name": "owner", "type": "relation", "collection_id": users.id},
            {"name": "editors", "type": "relation", "collection_id": users.id, "max_select": 4},
            {"name": "score", "type": "number"},
            {"name": "published", "type": "date"},
            {"name": "labels", "type": "select", "values": ["a", "b", "c"], "max_select": 3},
            {"name": "data", "type": "json"}
        ]}),
        None,
    )
    .unwrap();
    let comments = Collection::from_input(
        &json!({"name": "comments", "fields": [
            {"name": "post", "type": "relation", "collection_id": posts.id},
            {"name": "author", "type": "relation", "collection_id": users.id},
            {"name": "body", "type": "text"}
        ]}),
        None,
    )
    .unwrap();
    World {
        schema: Schema::new(vec![users.clone(), posts.clone(), comments.clone()], 1),
        users,
        posts,
        comments,
    }
}

const TEXTS: &[&str] = &["", "a", "A", "ab", "aB", "b_c", "x%y", "back\\slash", "Zz"];
const NUMS: &[f64] = &[0.0, 1.0, 2.5, -1.0, 3.0, 10.0];
const DATES: &[&str] = &[
    "2026-01-15T00:00:00.000Z",
    "2026-01-31T10:00:00.000Z",
    "2026-02-28T23:59:59.999Z",
    "2026-03-01T00:00:00.000Z",
];

fn json_scalar(r: &mut Rng) -> Value {
    match r.below(7) {
        0 => Value::Null,
        1 => json!(r.pick(TEXTS)),
        2 => json!(*r.pick(&[0i64, 1, 2, -3])),
        3 => json!(*r.pick(&[2.5f64, -0.5])),
        4 => json!(r.chance(50)),
        5 => json!([r.pick(TEXTS), 1]),
        _ => json!({"z": r.pick(TEXTS)}),
    }
}

fn json_doc(r: &mut Rng) -> Value {
    if r.chance(15) {
        return Value::Null;
    }
    let mut m = Map::new();
    for k in ["k", "n", "arr"] {
        if r.chance(80) {
            let v = if k == "arr" {
                Value::Array((0..r.below(3)).map(|_| json_scalar(r)).collect())
            } else {
                json_scalar(r)
            };
            m.insert(k.into(), v);
        }
    }
    Value::Object(m)
}

fn opt_text(r: &mut Rng) -> Value {
    if r.chance(15) {
        Value::Null
    } else {
        json!(r.pick(TEXTS))
    }
}

fn opt_num(r: &mut Rng) -> Value {
    if r.chance(15) {
        Value::Null
    } else {
        number_json(*r.pick(NUMS))
    }
}

fn multi(r: &mut Rng, pool: &[String]) -> Value {
    if r.chance(20) || pool.is_empty() {
        return Value::Null;
    }
    let n = r.below(3);
    let mut out: Vec<Value> = Vec::new();
    for _ in 0..n {
        let v = json!(r.pick(pool));
        if !out.contains(&v) {
            out.push(v);
        }
    }
    Value::Array(out)
}

fn record(data: Value) -> Record {
    Record {
        id: Uuid::now_v7(),
        created: "2026-01-01T00:00:00.000Z".into(),
        updated: "2026-01-02T00:00:00.000Z".into(),
        data: data.as_object().unwrap().clone(),
    }
}

struct Data {
    by_coll: HashMap<Uuid, Vec<Record>>,
}

impl Dataset for Data {
    fn row(&self, c: Uuid, id: &str) -> Option<&Record> {
        self.by_coll
            .get(&c)?
            .iter()
            .find(|r| r.id.to_string() == id)
    }
    fn rows(&self, c: Uuid) -> Vec<&Record> {
        self.by_coll
            .get(&c)
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }
}

fn dataset(w: &World, r: &mut Rng) -> Data {
    let labels: Vec<String> = ["a", "b", "c"].iter().map(|s| (*s).to_owned()).collect();
    let users: Vec<Record> = (0..r.below(4))
        .map(|_| {
            record(json!({
                "name": opt_text(r), "age": opt_num(r), "verified": r.chance(50),
                "tags": multi(r, &labels), "meta": json_doc(r)
            }))
        })
        .collect();
    let mut user_ids: Vec<String> = users.iter().map(|u| u.id.to_string()).collect();
    // Dangling references exercise broken relation chains.
    user_ids.push(Uuid::now_v7().to_string());
    let posts: Vec<Record> = (0..1 + r.below(4))
        .map(|_| {
            let owner = if r.chance(20) {
                Value::Null
            } else {
                json!(r.pick(&user_ids))
            };
            record(json!({
                "title": opt_text(r), "owner": owner, "editors": multi(r, &user_ids),
                "score": opt_num(r),
                "published": if r.chance(15) { Value::Null } else { json!(r.pick(DATES)) },
                "labels": multi(r, &labels), "data": json_doc(r)
            }))
        })
        .collect();
    let post_ids: Vec<String> = posts.iter().map(|p| p.id.to_string()).collect();
    let comments: Vec<Record> = (0..r.below(5))
        .map(|_| {
            record(json!({
                "post": json!(r.pick(&post_ids)), "author": json!(r.pick(&user_ids)), "body": opt_text(r)
            }))
        })
        .collect();
    Data {
        by_coll: HashMap::from([
            (w.users.id, users),
            (w.posts.id, posts),
            (w.comments.id, comments),
        ]),
    }
}

fn lit_text(r: &mut Rng) -> String {
    let t = r.pick(TEXTS);
    format!("'{}'", t.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// A random operand of the given kind: text, number, date, bool or json.
fn operand(r: &mut Rng, kind: &str) -> String {
    match kind {
        "text" => {
            let opts: Vec<String> = vec![
                "title".into(),
                "owner.name".into(),
                "editors.name".into(),
                "comments_via_post.body".into(),
                "comments_via_post.author.name".into(),
                "labels".into(),
                "labels:each".into(),
                "owner.tags".into(),
                "title:lower".into(),
                "lower(title)".into(),
                "upper(owner.name)".into(),
                "coalesce(title, 'zz')".into(),
                "@request.auth.id".into(),
                "@request.query.q".into(),
                "@collection.users.name".into(),
                "id".into(),
                lit_text(r),
                lit_text(r),
            ];
            r.pick(&opts).clone()
        }
        "number" => {
            let opts: Vec<String> = vec![
                "score".into(),
                "owner.age".into(),
                "editors.age".into(),
                "labels:length".into(),
                "editors:length".into(),
                "owner.tags:length".into(),
                "len(title)".into(),
                "coalesce(score, 0)".into(),
                "@collection.users.age".into(),
                (*r.pick(&["0", "1", "2.5", "-1", "3"])).to_owned(),
            ];
            r.pick(&opts).clone()
        }
        "date" => {
            let opts: Vec<String> = vec![
                "published".into(),
                "created".into(),
                "date_add(published, 'P1M')".into(),
                "date_add(published, '-P1DT2H')".into(),
                "@now".into(),
                format!(
                    "'{}'",
                    r.pick(&["2026-01-31", "2026-02-15 12:00:00", "2026-03-01T00:00:00Z"])
                ),
            ];
            r.pick(&opts).clone()
        }
        "bool" => r
            .pick(&["owner.verified", "editors.verified", "true", "false"])
            .to_string(),
        _ => {
            let opts: Vec<String> = vec![
                "data".into(),
                "data.k".into(),
                "data.n".into(),
                "data.arr:each".into(),
                "data.arr:length".into(),
                "data.k.z".into(),
                "owner.meta.k".into(),
                "editors.meta.n".into(),
                lit_text(r),
                "1".into(),
                "2.5".into(),
                "null".into(),
            ];
            r.pick(&opts).clone()
        }
    }
}

fn comparison(r: &mut Rng) -> String {
    let kind = *r.pick(&["text", "text", "number", "date", "bool", "json", "json"]);
    let left = operand(r, kind);
    let right_kind = if kind == "json" {
        *r.pick(&["json", "text", "number"])
    } else {
        kind
    };
    let mut right = operand(r, right_kind);
    let ops: &[&str] = match kind {
        "bool" => &["=", "!="],
        "text" | "json" => &["=", "!=", ">", ">=", "<", "<=", "~", "!~"],
        _ => &["=", "!=", ">", ">=", "<", "<="],
    };
    let mut op = r.pick(ops).to_string();
    if r.chance(10) && matches!(op.as_str(), "=" | "!=") {
        right = r.pick(&["null", "''"]).to_string();
    }
    if (op.contains('~')) && !(right_kind == "text" || right_kind == "json") {
        op = "=".into();
    }
    if r.chance(40) {
        op = format!("?{op}");
    }
    format!("{left} {op} {right}")
}

fn expression(r: &mut Rng, depth: u32) -> String {
    if depth == 0 || r.chance(40) {
        return comparison(r);
    }
    match r.below(3) {
        0 => format!(
            "({} && {})",
            expression(r, depth - 1),
            expression(r, depth - 1)
        ),
        1 => format!(
            "({} || {})",
            expression(r, depth - 1),
            expression(r, depth - 1)
        ),
        _ => format!("!({})", expression(r, depth - 1)),
    }
}

fn load(w: &World, data: &Data) -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    records::install_timeout_handler(&mut conn).unwrap();
    for c in [&w.users, &w.posts, &w.comments] {
        ddl::apply(
            &conn,
            &[DdlStep::CreateTable {
                auth: false,
                table: c.name.clone(),
                fields: c.fields.clone(),
            }],
        )
        .unwrap();
        for rec in data.rows(c.id) {
            records::insert(&conn, c, rec).unwrap();
        }
    }
    conn
}

thread_local! {
    /// (skipped expressions, compiled expressions, true results)
    static STATS: std::cell::RefCell<(usize, usize, usize)> = const { std::cell::RefCell::new((0, 0, 0)) };
}

/// Returns the number of (expression, record) pairs compared.
fn run_case(seed: u64) -> usize {
    let w = world();
    let mut r = Rng(seed | 1);
    let data = dataset(&w, &mut r);
    let conn = load(&w, &data);

    let mut req = RequestValues::new(time::macros::datetime!(2026-02-01 12:00:00 UTC));
    if r.chance(60)
        && let Some(u) = data.rows(w.users.id).first()
    {
        req.auth = Some(AuthValues {
            id: u.id.to_string(),
            collection_id: w.users.id.to_string(),
            collection_name: "users".into(),
            fields: Map::new(),
        });
    }
    if r.chance(50) {
        req.query.insert("q".into(), r.pick(TEXTS).to_string());
    }

    let mut compared = 0;
    for _ in 0..30 {
        let src = expression(&mut r, 3);
        let rule = match compile(&w.schema, Some(&w.posts), &src, Mode::Rule(RuleOp::List)) {
            Ok(rule) => rule,
            // Some generated combinations are rejected by the type checker
            // (e.g. ordering against null); they are not part of the test.
            Err(e) => {
                if std::env::var("EQ_DEBUG").is_ok() {
                    eprintln!("SKIP {src}: {}", e.message);
                }
                STATS.with(|s| s.borrow_mut().0 += 1);
                continue;
            }
        };
        STATS.with(|s| s.borrow_mut().1 += 1);
        for post in data.rows(w.posts.id) {
            let expected = evaluate(&rule, post, &data, &req);
            let got = records::matches(
                &conn,
                &w.posts,
                &post.id.to_string(),
                &rule,
                &req,
                Duration::from_secs(5),
            )
            .unwrap_or_else(|e| panic!("seed {seed}: SQL failed for {src}: {e:?}"));
            assert_eq!(
                got,
                expected,
                "seed {seed}: evaluator and SQL disagree\n  rule: {src}\n  post: {}\n  sql: {}",
                serde_json::Value::Object(post.data.clone()),
                rackend_store_sqlite::rules_sql::compile(&rule, 1).sql
            );
            compared += 1;
            if expected {
                STATS.with(|s| s.borrow_mut().2 += 1);
            }
        }
    }
    compared
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, failure_persistence: None, ..ProptestConfig::default() })]
    #[test]
    fn evaluator_matches_sqlite(seed in any::<u64>()) {
        run_case(seed);
    }
}

#[test]
fn at_least_ten_thousand_cases() {
    // Deterministic sweep so the count is reported and always ≥ 10k.
    let mut total = 0;
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    while total < 12_000 {
        total += run_case(seed);
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
    }
    let (skipped, compiled, truthy) = STATS.with(|s| *s.borrow());
    eprintln!(
        "compared {total} (rule, record) pairs; {compiled} rules compiled, {skipped} rejected by the checker, {truthy} true results"
    );
    assert!(compiled > skipped, "most generated rules must compile");
    assert!(
        truthy > total / 10 && truthy < total * 9 / 10,
        "results must be mixed"
    );
}
