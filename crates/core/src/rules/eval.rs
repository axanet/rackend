//! In-memory reference evaluator (SPEC §9.6). It must give exactly the same
//! results as the SQL compiler in `store-sqlite`; a property test checks
//! that on randomized data.

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use time::OffsetDateTime;
use uuid::Uuid;

use super::ast::Modifier;
use super::lexer::CmpOp;
use super::typed::{
    FieldPath, Func, Hop, Leaf, Lit, Param, Root, TExpr, TKind, TOperand, TypedRule, VType,
};
use super::value::{SVal, compare, date_add, like, like_pattern};
use crate::domain::Record;
use crate::domain::value::normalize_date;
use crate::time::format_timestamp;

/// The authenticated subject, as seen by `@request.auth.*`.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthValues {
    /// Record (or superuser) id.
    pub id: String,
    /// Auth collection id (empty for superusers).
    pub collection_id: String,
    /// Auth collection name (`_superusers` for superusers).
    pub collection_name: String,
    /// Other fields (`email`, `email_verified`, user fields).
    pub fields: Map<String, Value>,
}

/// Request values available to rules.
#[derive(Debug, Clone, PartialEq)]
pub struct RequestValues {
    /// `@request.auth`; `None` for anonymous requests.
    pub auth: Option<AuthValues>,
    /// `@request.body` (create/update).
    pub body: Option<Map<String, Value>>,
    /// `@request.query.*`.
    pub query: BTreeMap<String, String>,
    /// `@request.headers.*` (keys lowercase with `_`).
    pub headers: BTreeMap<String, String>,
    /// `@request.method`.
    pub method: String,
    /// `@request.context`: `default`, `realtime` or `oidc`.
    pub context: String,
    /// `@channel.key`.
    pub channel_key: Option<String>,
    /// `@now`.
    pub now: String,
    /// `@today_start`.
    pub today_start: String,
    /// `@today_end`.
    pub today_end: String,
}

impl RequestValues {
    /// Empty request values at time `now`.
    pub fn new(now: OffsetDateTime) -> Self {
        let day = now.to_offset(time::UtcOffset::UTC).date();
        let start = day.midnight().assume_utc();
        let end = start + time::Duration::days(1) - time::Duration::milliseconds(1);
        Self {
            auth: None,
            body: None,
            query: BTreeMap::new(),
            headers: BTreeMap::new(),
            method: "GET".into(),
            context: "default".into(),
            channel_key: None,
            now: format_timestamp(now),
            today_start: format_timestamp(start),
            today_end: format_timestamp(end),
        }
    }
}

/// A request value bound to a parameter.
#[derive(Debug, Clone, PartialEq)]
pub enum ParamValue {
    /// One value.
    One(SVal),
    /// A set of values (multi-valued fields).
    Many(Vec<SVal>),
}

fn json_list(v: Option<&Value>, date: bool) -> Vec<SVal> {
    match v {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().map(|i| scalar(i, date)).collect(),
        Some(other) => vec![scalar(other, date)],
    }
}

fn scalar(v: &Value, date: bool) -> SVal {
    match v {
        Value::String(s) if date => SVal::Text(normalize_date(s).unwrap_or_else(|| s.clone())),
        other => SVal::from_json(other),
    }
}

/// Resolves a parameter for the current request. Used by both the evaluator
/// and the SQL executor, so both see identical values.
pub fn resolve_param(p: &Param, req: &RequestValues) -> ParamValue {
    let text =
        |s: Option<&String>| ParamValue::One(s.map_or(SVal::Null, |s| SVal::Text(s.clone())));
    match p {
        Param::Auth { field, multi } => {
            let Some(a) = &req.auth else {
                return if *multi {
                    ParamValue::Many(Vec::new())
                } else {
                    ParamValue::One(SVal::Null)
                };
            };
            match field.as_str() {
                "id" => ParamValue::One(SVal::Text(a.id.clone())),
                "collection_id" => ParamValue::One(SVal::Text(a.collection_id.clone())),
                "collection_name" => ParamValue::One(SVal::Text(a.collection_name.clone())),
                f if *multi => ParamValue::Many(json_list(a.fields.get(f), false)),
                f => ParamValue::One(a.fields.get(f).map_or(SVal::Null, |v| scalar(v, false))),
            }
        }
        Param::Body { field, multi, date } => {
            let v = req.body.as_ref().and_then(|b| b.get(field));
            if *multi {
                ParamValue::Many(json_list(v, *date))
            } else {
                ParamValue::One(v.map_or(SVal::Null, |v| scalar(v, *date)))
            }
        }
        Param::BodyIsSet(field) => ParamValue::One(SVal::Int(i64::from(
            req.body.as_ref().is_some_and(|b| b.contains_key(field)),
        ))),
        Param::Query(q) => text(req.query.get(q)),
        Param::Header(h) => text(req.headers.get(h)),
        Param::Method => ParamValue::One(SVal::Text(req.method.clone())),
        Param::Context => ParamValue::One(SVal::Text(req.context.clone())),
        Param::ChannelKey => text(req.channel_key.as_ref()),
        Param::Now => ParamValue::One(SVal::Text(req.now.clone())),
        Param::TodayStart => ParamValue::One(SVal::Text(req.today_start.clone())),
        Param::TodayEnd => ParamValue::One(SVal::Text(req.today_end.clone())),
    }
}

/// Records available to the evaluator, by collection id.
pub trait Dataset {
    /// One record.
    fn row(&self, collection: Uuid, id: &str) -> Option<&Record>;
    /// All records of a collection.
    fn rows(&self, collection: Uuid) -> Vec<&Record>;
}

/// Evaluates `rule` for `record` (SPEC §9.3 semantics).
pub fn evaluate(
    rule: &TypedRule,
    record: &Record,
    data: &dyn Dataset,
    req: &RequestValues,
) -> bool {
    let choices: Vec<Vec<Option<&Record>>> = rule
        .aliases
        .iter()
        .map(|(id, _)| {
            let rows: Vec<Option<&Record>> = data.rows(*id).into_iter().map(Some).collect();
            if rows.is_empty() { vec![None] } else { rows }
        })
        .collect();
    let mut idx = vec![0usize; choices.len()];
    loop {
        let bound: Vec<Option<&Record>> = choices.iter().zip(&idx).map(|(c, &i)| c[i]).collect();
        let ev = Eval {
            record,
            aliases: &bound,
            data,
            req,
        };
        if ev.expr(&rule.expr) {
            return true;
        }
        // Next combination.
        let mut k = 0;
        loop {
            if k == idx.len() {
                return false;
            }
            idx[k] += 1;
            if idx[k] < choices[k].len() {
                break;
            }
            idx[k] = 0;
            k += 1;
        }
    }
}

struct Eval<'a> {
    record: &'a Record,
    aliases: &'a [Option<&'a Record>],
    data: &'a dyn Dataset,
    req: &'a RequestValues,
}

fn pair(op: CmpOp, l: &SVal, r: &SVal) -> bool {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let both = !l.is_null() && !r.is_null();
    match op {
        CmpOp::Eq => both && compare(l, r) == Equal,
        CmpOp::Ne => !pair(CmpOp::Eq, l, r),
        CmpOp::Gt => both && compare(l, r) == Greater,
        CmpOp::Ge => both && compare(l, r) != Less,
        CmpOp::Lt => both && compare(l, r) == Less,
        CmpOp::Le => both && compare(l, r) != Greater,
        CmpOp::Like => match (l.as_text(), r.as_text()) {
            (Some(t), Some(p)) => like(&t, &like_pattern(&p)),
            _ => false,
        },
        CmpOp::NotLike => !pair(CmpOp::Like, l, r),
    }
}

/// Converts a stored column value according to the operand type.
fn column_value(v: &Value, ty: VType) -> SVal {
    match (v, ty) {
        (Value::Null, _) => SVal::Null,
        (Value::Number(n), VType::Number) => SVal::Real(n.as_f64().unwrap_or(0.0)),
        (Value::Bool(b), _) => SVal::Int(i64::from(*b)),
        (other, _) => SVal::from_json(other),
    }
}

fn json_at<'v>(v: &'v Value, path: &[String]) -> Option<&'v Value> {
    path.iter().try_fold(v, |cur, k| cur.as_object()?.get(k))
}

/// `json_each` rows of a value (array → elements, object → values, scalar → itself).
fn json_each(v: Option<&Value>) -> Vec<SVal> {
    match v {
        None => Vec::new(),
        Some(Value::Array(a)) => a.iter().map(SVal::from_json).collect(),
        Some(Value::Object(o)) => o.values().map(SVal::from_json).collect(),
        Some(other) => vec![SVal::from_json(other)],
    }
}

impl Eval<'_> {
    fn expr(&self, e: &TExpr) -> bool {
        match e {
            TExpr::Or(items) => items.iter().any(|i| self.expr(i)),
            TExpr::And(items) => items.iter().all(|i| self.expr(i)),
            TExpr::Not(inner) => !self.expr(inner),
            TExpr::Cmp {
                left,
                op,
                any,
                right,
            } => self.cmp(left, *op, *any, right),
        }
    }

    fn cmp(&self, left: &TOperand, op: CmpOp, any: bool, right: &TOperand) -> bool {
        let empty_check = matches!(op, CmpOp::Eq | CmpOp::Ne)
            && (left.is_empty_literal() || right.is_empty_literal());
        let quantify = |values: Vec<SVal>, f: &dyn Fn(&SVal) -> bool| {
            if any {
                values.iter().any(f)
            } else {
                values.iter().all(f)
            }
        };
        if empty_check {
            let other = if right.is_empty_literal() {
                left
            } else {
                right
            };
            let values = self.operand(other);
            let want_empty = op == CmpOp::Eq;
            return quantify(values, &|v| v.is_empty() == want_empty);
        }
        let ls = self.operand(left);
        let rs = self.operand(right);
        let mut pairs = Vec::with_capacity(ls.len() * rs.len());
        for l in &ls {
            for r in &rs {
                pairs.push((l, r));
            }
        }
        if any {
            pairs.iter().any(|(l, r)| pair(op, l, r))
        } else {
            pairs.iter().all(|(l, r)| pair(op, l, r))
        }
    }

    /// The values of an operand: exactly one for single operands.
    fn operand(&self, o: &TOperand) -> Vec<SVal> {
        match &o.kind {
            TKind::Lit(l) => vec![match l {
                Lit::Null => SVal::Null,
                Lit::Text(s) => SVal::Text(s.clone()),
                Lit::Int(i) => SVal::Int(*i),
                Lit::Real(f) => SVal::Real(*f),
                Lit::Bool(b) => SVal::Int(i64::from(*b)),
            }],
            TKind::Param(p) => match resolve_param(p, self.req) {
                ParamValue::One(v) => vec![v],
                ParamValue::Many(v) => v,
            },
            TKind::Path(fp) => self.path(fp, o.ty),
            TKind::Func(f, args) => self.func(f, args),
        }
    }

    fn func(&self, f: &Func, args: &[TOperand]) -> Vec<SVal> {
        match f {
            Func::Lower | Func::Upper => self
                .operand(&args[0])
                .into_iter()
                .map(|v| match v.as_text() {
                    None => SVal::Null,
                    Some(t) => SVal::Text(if matches!(f, Func::Lower) {
                        t.to_ascii_lowercase()
                    } else {
                        t.to_ascii_uppercase()
                    }),
                })
                .collect(),
            Func::Len => {
                let values = self.operand(&args[0]);
                if args[0].multi {
                    return vec![SVal::Int(i64::try_from(values.len()).unwrap_or(i64::MAX))];
                }
                let n = values
                    .first()
                    .and_then(SVal::as_text)
                    .map_or(0, |t| t.chars().count());
                vec![SVal::Int(i64::try_from(n).unwrap_or(i64::MAX))]
            }
            Func::DateAdd(parts) => self
                .operand(&args[0])
                .into_iter()
                .map(|v| match v {
                    SVal::Text(t) => date_add(&t, parts).map_or(SVal::Null, SVal::Text),
                    _ => SVal::Null,
                })
                .collect(),
            Func::Coalesce => {
                let v = args
                    .iter()
                    .map(|a| self.operand(a).into_iter().next().unwrap_or(SVal::Null))
                    .find(|v| !v.is_null())
                    .unwrap_or(SVal::Null);
                vec![v]
            }
        }
    }

    fn root(&self, root: Root) -> Option<&Record> {
        match root {
            Root::Record => Some(self.record),
            Root::Alias(i) => self.aliases.get(i).copied().flatten(),
        }
    }

    fn path(&self, fp: &FieldPath, ty: VType) -> Vec<SVal> {
        let start = self.root(fp.root);
        if !fp.is_multi() {
            // Scalar: single hops only; a broken chain yields NULL.
            let mut row = start;
            for hop in &fp.hops {
                row = match (hop, row) {
                    (Hop::Forward { column, target, .. }, Some(r)) => r
                        .get(column)
                        .as_str()
                        .and_then(|id| self.data.row(*target, id)),
                    _ => None,
                };
            }
            let Some(row) = row else {
                return vec![if fp.modifier == Some(Modifier::Length) {
                    SVal::Int(0)
                } else {
                    SVal::Null
                }];
            };
            return vec![self.leaf_scalar(row, fp, ty)];
        }

        let mut rows: Vec<&Record> = start.into_iter().collect();
        for hop in &fp.hops {
            let mut next = Vec::new();
            for r in &rows {
                match hop {
                    Hop::Forward {
                        column,
                        multi: false,
                        target,
                        ..
                    } => {
                        if let Some(t) = r
                            .get(column)
                            .as_str()
                            .and_then(|id| self.data.row(*target, id))
                        {
                            next.push(t);
                        }
                    }
                    Hop::Forward {
                        column,
                        multi: true,
                        target,
                        ..
                    } => {
                        for id in r.get(column).as_array().into_iter().flatten() {
                            if let Some(t) = id.as_str().and_then(|id| self.data.row(*target, id)) {
                                next.push(t);
                            }
                        }
                    }
                    Hop::Back {
                        source,
                        column,
                        multi,
                        ..
                    } => {
                        let me = Value::String(r.id.to_string());
                        for s in self.data.rows(*source) {
                            let refs = if *multi {
                                s.get(column).as_array().is_some_and(|a| a.contains(&me))
                            } else {
                                *s.get(column) == me
                            };
                            if refs {
                                next.push(s);
                            }
                        }
                    }
                }
            }
            rows = next;
        }
        let mut out = Vec::new();
        for r in rows {
            match &fp.leaf {
                Leaf::Column {
                    column,
                    multi: true,
                    ..
                } => {
                    out.extend(
                        r.get(column)
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(SVal::from_json),
                    );
                }
                Leaf::Json { column, path, .. } if fp.modifier == Some(Modifier::Each) => {
                    let col = r.get(column);
                    if !col.is_null() {
                        out.extend(json_each(json_at(col, path)));
                    }
                }
                _ => out.push(self.leaf_scalar(r, fp, ty)),
            }
        }
        out
    }

    fn leaf_scalar(&self, row: &Record, fp: &FieldPath, ty: VType) -> SVal {
        match &fp.leaf {
            Leaf::Column { column, multi, .. } => {
                let v = match column.as_str() {
                    "id" => return SVal::Text(row.id.to_string()),
                    "created" => return SVal::Text(row.created.clone()),
                    "updated" => return SVal::Text(row.updated.clone()),
                    c => row.get(c),
                };
                if fp.modifier == Some(Modifier::Length) {
                    let n = if *multi {
                        v.as_array().map_or(0, Vec::len)
                    } else {
                        0
                    };
                    return SVal::Int(i64::try_from(n).unwrap_or(i64::MAX));
                }
                column_value(v, ty)
            }
            Leaf::Json { column, path, .. } => {
                let col = row.get(column);
                if col.is_null() {
                    return if fp.modifier == Some(Modifier::Length) {
                        SVal::Int(0)
                    } else {
                        SVal::Null
                    };
                }
                let at = json_at(col, path);
                if fp.modifier == Some(Modifier::Length) {
                    let n = at.and_then(Value::as_array).map_or(0, Vec::len);
                    return SVal::Int(i64::try_from(n).unwrap_or(i64::MAX));
                }
                at.map_or(SVal::Null, SVal::from_json)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::super::{Mode, compile};
    use super::*;
    use crate::domain::{Collection, RuleOp, Schema};
    use serde_json::json;
    use std::collections::HashMap;

    struct Data(HashMap<Uuid, Vec<Record>>);
    impl Dataset for Data {
        fn row(&self, c: Uuid, id: &str) -> Option<&Record> {
            self.0.get(&c)?.iter().find(|r| r.id.to_string() == id)
        }
        fn rows(&self, c: Uuid) -> Vec<&Record> {
            self.0
                .get(&c)
                .map(|v| v.iter().collect())
                .unwrap_or_default()
        }
    }

    fn rec(data: Value) -> Record {
        Record {
            id: Uuid::now_v7(),
            created: "2026-01-01T00:00:00.000Z".into(),
            updated: "2026-01-01T00:00:00.000Z".into(),
            data: data.as_object().unwrap().clone(),
        }
    }

    #[test]
    fn semantics_end_to_end() {
        let users = Collection::from_input(
            &json!({"name": "users", "fields": [{"name": "name", "type": "text"}]}),
            None,
        )
        .unwrap();
        let posts = Collection::from_input(
            &json!({"name": "posts", "fields": [
                {"name": "owner", "type": "relation", "collection_id": users.id},
                {"name": "tags", "type": "select", "values": ["a", "b"], "max_select": 2},
                {"name": "title", "type": "text"}
            ]}),
            None,
        )
        .unwrap();
        let schema = Schema::new(vec![users.clone(), posts.clone()], 1);
        let alice = rec(json!({"name": "Alice"}));
        let post =
            rec(json!({"owner": alice.id.to_string(), "tags": ["a"], "title": "Hello World"}));
        let orphan = rec(json!({"owner": null, "tags": null, "title": ""}));
        let data = Data(HashMap::from([(users.id, vec![alice.clone()])]));

        let mut anon = RequestValues::new(OffsetDateTime::now_utc());
        let mut me = anon.clone();
        me.auth = Some(AuthValues {
            id: alice.id.to_string(),
            collection_id: users.id.to_string(),
            collection_name: "users".into(),
            fields: Map::new(),
        });
        let check = |src: &str, r: &Record, req: &RequestValues| {
            let rule = compile(&schema, Some(&posts), src, Mode::Rule(RuleOp::List)).unwrap();
            evaluate(&rule, r, &data, req)
        };

        assert!(check("owner = @request.auth.id", &post, &me));
        assert!(
            !check("owner = @request.auth.id", &orphan, &anon),
            "null never equals null"
        );
        assert!(check("@request.auth.id != ''", &post, &me));
        assert!(!check("@request.auth.id != ''", &post, &anon));
        assert!(check("owner.name = 'Alice'", &post, &anon));
        assert!(
            check("title ~ 'world'", &post, &anon),
            "case-insensitive contains"
        );
        assert!(
            check("title = null", &orphan, &anon),
            "empty string is empty"
        );
        assert!(check("tags ?= 'a' && tags != 'b'", &post, &anon));
        assert!(
            check("tags = 'zzz'", &orphan, &anon),
            "plain op on an empty set is vacuously true"
        );
        assert!(
            !check("tags ?= 'a'", &orphan, &anon),
            "?-op on an empty set is false"
        );
        assert!(check("tags:length = 1", &post, &anon));
        assert!(check("tags:length = 0", &orphan, &anon));
        assert!(check("@collection.users.name = 'Alice'", &orphan, &anon));
        anon.method = "POST".into();
        assert!(check("@request.method = 'POST'", &post, &anon));
    }
}
