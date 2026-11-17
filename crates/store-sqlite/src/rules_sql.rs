//! Compiles typed rules to SQL (SPEC §9.5). Every literal and request value
//! is a bound parameter. Relation traversals become correlated `EXISTS` /
//! scalar subqueries; multi-valued fields use `json_each`.
//!
//! Semantics match `rackend_core::rules::eval` exactly:
//! - comparisons never yield NULL (`COALESCE(…, 0)`); a NULL operand makes
//!   `=`, `<`, `~`… false and their negations true;
//! - `= null` / `= ''` test for "empty" (NULL or '');
//! - plain operators over sets: all pairs match (vacuously true); `?`
//!   operators: at least one pair matches;
//! - when a side is a JSON value, column affinity is removed with unary `+`
//!   so values compare by storage class like the evaluator does.

use rackend_core::rules::ast::Modifier;
use rackend_core::rules::eval::{ParamValue, RequestValues, resolve_param};
use rackend_core::rules::lexer::CmpOp;
use rackend_core::rules::typed::{
    FieldPath, Func, Hop, Leaf, Lit, Param, Root, TExpr, TKind, TOperand, TypedRule, VType,
};
use rackend_core::rules::value::{SVal, like_pattern};
use rusqlite::types::Value as SqlValue;

/// Quotes an identifier.
pub fn q(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// A bound parameter: a constant or a request value resolved at execution.
#[derive(Debug, Clone, PartialEq)]
pub enum Bind {
    /// Constant.
    Value(SqlValue),
    /// A request value (single).
    Param(Param),
    /// A request value set, bound as a JSON array for `json_each(?)`.
    ParamList(Param),
}

/// A compiled boolean SQL fragment over the table aliased `t`.
#[derive(Debug, Clone, PartialEq)]
pub struct Fragment {
    /// SQL with numbered `?N` placeholders.
    pub sql: String,
    /// Parameters in placeholder order.
    pub binds: Vec<Bind>,
}

impl Fragment {
    /// The always-true fragment.
    pub fn always() -> Self {
        Self {
            sql: "1".into(),
            binds: Vec::new(),
        }
    }

    /// Resolves parameters for one request.
    pub fn values(&self, req: &RequestValues) -> Vec<SqlValue> {
        self.binds
            .iter()
            .map(|b| match b {
                Bind::Value(v) => v.clone(),
                Bind::Param(p) => match resolve_param(p, req) {
                    ParamValue::One(v) => sval(v),
                    ParamValue::Many(v) => SqlValue::Text(json_array(&v)),
                },
                Bind::ParamList(p) => match resolve_param(p, req) {
                    ParamValue::One(v) if v.is_null() => SqlValue::Text("[]".into()),
                    ParamValue::One(v) => SqlValue::Text(json_array(&[v])),
                    ParamValue::Many(v) => SqlValue::Text(json_array(&v)),
                },
            })
            .collect()
    }
}

/// SVal → SQLite value.
pub fn sval(v: SVal) -> SqlValue {
    match v {
        SVal::Null => SqlValue::Null,
        SVal::Int(i) => SqlValue::Integer(i),
        SVal::Real(f) => SqlValue::Real(f),
        SVal::Text(s) => SqlValue::Text(s),
    }
}

fn json_array(values: &[SVal]) -> String {
    let items: Vec<serde_json::Value> = values
        .iter()
        .map(|v| match v {
            SVal::Null => serde_json::Value::Null,
            SVal::Int(i) => serde_json::Value::from(*i),
            SVal::Real(f) => serde_json::Number::from_f64(*f)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            SVal::Text(s) => serde_json::Value::from(s.clone()),
        })
        .collect();
    serde_json::Value::Array(items).to_string()
}

/// Compiles `rule` for a query whose main table is aliased `t`. Placeholders
/// are numbered from `?{offset + 1}`, so fragments can be combined.
pub fn compile(rule: &TypedRule, offset: usize) -> Fragment {
    let mut c = Compiler {
        binds: Vec::new(),
        n: 0,
        offset,
    };
    let body = c.expr(&rule.expr);
    let sql = if rule.aliases.is_empty() {
        body
    } else {
        let joins: Vec<String> = rule
            .aliases
            .iter()
            .enumerate()
            .map(|(i, (_, table))| format!("LEFT JOIN {} c{i} ON 1", q(table)))
            .collect();
        format!(
            "EXISTS (SELECT 1 FROM (SELECT 1) _d {} WHERE {body})",
            joins.join(" ")
        )
    };
    Fragment {
        sql,
        binds: c.binds,
    }
}

enum Opd {
    Scalar(String),
    /// A query producing column `v`.
    Set(String),
}

struct Compiler {
    binds: Vec<Bind>,
    n: usize,
    offset: usize,
}

impl Compiler {
    /// Numbered placeholders keep the binding order independent of the
    /// order in which operands are compiled.
    fn bind(&mut self, b: Bind) -> String {
        self.binds.push(b);
        format!("?{}", self.offset + self.binds.len())
    }

    fn fresh(&mut self, prefix: &str) -> String {
        self.n += 1;
        format!("{prefix}{}", self.n)
    }

    fn expr(&mut self, e: &TExpr) -> String {
        match e {
            TExpr::Or(items) => format!(
                "({})",
                items
                    .iter()
                    .map(|i| self.expr(i))
                    .collect::<Vec<_>>()
                    .join(" OR ")
            ),
            TExpr::And(items) => format!(
                "({})",
                items
                    .iter()
                    .map(|i| self.expr(i))
                    .collect::<Vec<_>>()
                    .join(" AND ")
            ),
            TExpr::Not(inner) => format!("(NOT {})", self.expr(inner)),
            TExpr::Cmp {
                left,
                op,
                any,
                right,
            } => self.cmp(left, *op, *any, right),
        }
    }

    fn cmp(&mut self, left: &TOperand, op: CmpOp, any: bool, right: &TOperand) -> String {
        let unaff = left.ty == VType::Any || right.ty == VType::Any;
        if matches!(op, CmpOp::Eq | CmpOp::Ne)
            && (left.is_empty_literal() || right.is_empty_literal())
        {
            let other = if right.is_empty_literal() {
                left
            } else {
                right
            };
            let neg = op == CmpOp::Ne;
            let test = |x: &str| {
                let t = format!("({x} IS NULL OR {x} = '')");
                if neg { format!("(NOT {t})") } else { t }
            };
            return match self.operand(other) {
                Opd::Scalar(x) => test(&x),
                Opd::Set(s) => quantify(any, &s, None, &test("s.v")),
            };
        }
        // A literal pattern on the right of ~ is prepared at compile time.
        let r = match (&right.kind, op) {
            (TKind::Lit(Lit::Text(s)), CmpOp::Like | CmpOp::NotLike) => {
                let p = self.bind(Bind::Value(SqlValue::Text(like_pattern(s))));
                PreparedRight::Pattern(p)
            }
            _ => PreparedRight::Plain(self.operand(right)),
        };
        let l = self.operand(left);
        let w = |x: &str| {
            if unaff {
                format!("+({x})")
            } else {
                x.to_owned()
            }
        };
        let pair = |l: &str, r: &PreparedRhs<'_>| -> String {
            let (l, rv) = (
                w(l),
                match r {
                    PreparedRhs::Pattern(p) => (*p).to_owned(),
                    PreparedRhs::Value(v) => w(v),
                },
            );
            let pattern = |rv: &str| match r {
                PreparedRhs::Pattern(_) => rv.to_owned(),
                PreparedRhs::Value(_) => format!(
                    "(CASE WHEN instr(CAST({rv} AS TEXT), '%') > 0 THEN CAST({rv} AS TEXT) ELSE '%' || replace(replace(replace(CAST({rv} AS TEXT), '\\', '\\\\'), '%', '\\%'), '_', '\\_') || '%' END)"
                ),
            };
            match op {
                CmpOp::Eq => format!("COALESCE({l} = {rv}, 0)"),
                CmpOp::Ne => format!("(NOT COALESCE({l} = {rv}, 0))"),
                CmpOp::Gt => format!("COALESCE({l} > {rv}, 0)"),
                CmpOp::Ge => format!("COALESCE({l} >= {rv}, 0)"),
                CmpOp::Lt => format!("COALESCE({l} < {rv}, 0)"),
                CmpOp::Le => format!("COALESCE({l} <= {rv}, 0)"),
                CmpOp::Like => format!("COALESCE({l} LIKE {} ESCAPE '\\', 0)", pattern(&rv)),
                CmpOp::NotLike => {
                    format!("(NOT COALESCE({l} LIKE {} ESCAPE '\\', 0))", pattern(&rv))
                }
            }
        };
        match (l, r) {
            (Opd::Scalar(l), PreparedRight::Pattern(p)) => pair(&l, &PreparedRhs::Pattern(&p)),
            (Opd::Scalar(l), PreparedRight::Plain(Opd::Scalar(r))) => {
                pair(&l, &PreparedRhs::Value(&r))
            }
            (Opd::Set(ls), PreparedRight::Pattern(p)) => {
                let cond = pair("l.v", &PreparedRhs::Pattern(&p));
                quantify_one(any, &ls, "l", &cond)
            }
            (Opd::Set(ls), PreparedRight::Plain(Opd::Scalar(r))) => {
                let cond = pair("l.v", &PreparedRhs::Value(&r));
                quantify_one(any, &ls, "l", &cond)
            }
            (Opd::Scalar(l), PreparedRight::Plain(Opd::Set(rs))) => {
                let cond = pair(&l, &PreparedRhs::Value("r.v"));
                quantify_one(any, &rs, "r", &cond)
            }
            (Opd::Set(ls), PreparedRight::Plain(Opd::Set(rs))) => {
                let cond = pair("l.v", &PreparedRhs::Value("r.v"));
                quantify(any, &ls, Some(&rs), &cond)
            }
        }
    }

    fn operand(&mut self, o: &TOperand) -> Opd {
        match &o.kind {
            TKind::Lit(l) => {
                let v = match l {
                    Lit::Null => SqlValue::Null,
                    Lit::Text(s) => SqlValue::Text(s.clone()),
                    Lit::Int(i) => SqlValue::Integer(*i),
                    Lit::Real(f) => SqlValue::Real(*f),
                    Lit::Bool(b) => SqlValue::Integer(i64::from(*b)),
                };
                Opd::Scalar(self.bind(Bind::Value(v)))
            }
            TKind::Param(p) if o.multi => {
                let ph = self.bind(Bind::ParamList(p.clone()));
                Opd::Set(format!("SELECT value AS v FROM json_each({ph})"))
            }
            TKind::Param(p) => Opd::Scalar(self.bind(Bind::Param(p.clone()))),
            TKind::Path(fp) => self.path(fp),
            TKind::Func(f, args) => self.func(f, args),
        }
    }

    fn func(&mut self, f: &Func, args: &[TOperand]) -> Opd {
        match f {
            Func::Lower | Func::Upper => {
                let name = if matches!(f, Func::Lower) {
                    "lower"
                } else {
                    "upper"
                };
                match self.operand(&args[0]) {
                    Opd::Scalar(x) => Opd::Scalar(format!("{name}({x})")),
                    Opd::Set(s) => Opd::Set(format!("SELECT {name}(v) AS v FROM ({s})")),
                }
            }
            Func::Len => match self.operand(&args[0]) {
                Opd::Scalar(x) => Opd::Scalar(format!("COALESCE(length({x}), 0)")),
                Opd::Set(s) => Opd::Scalar(format!("(SELECT count(*) FROM ({s}))")),
            },
            Func::DateAdd(parts) => {
                let mods: String = parts
                    .iter()
                    .map(|p| format!(", '{}'", p.modifier()))
                    .collect();
                match self.operand(&args[0]) {
                    Opd::Scalar(x) => {
                        Opd::Scalar(format!("strftime('%Y-%m-%dT%H:%M:%fZ', {x}{mods})"))
                    }
                    Opd::Set(s) => Opd::Set(format!(
                        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', v{mods}) AS v FROM ({s})"
                    )),
                }
            }
            Func::Coalesce => {
                let parts: Vec<String> = args
                    .iter()
                    .map(|a| match self.operand(a) {
                        Opd::Scalar(x) => x,
                        Opd::Set(s) => format!("(SELECT v FROM ({s}) LIMIT 1)"),
                    })
                    .collect();
                Opd::Scalar(format!("COALESCE({})", parts.join(", ")))
            }
        }
    }

    fn root(root: Root) -> String {
        match root {
            Root::Record => "t".into(),
            Root::Alias(i) => format!("c{i}"),
        }
    }

    /// Scalar value of the leaf on row alias `a`.
    fn leaf_scalar(&mut self, a: &str, fp: &FieldPath) -> String {
        match &fp.leaf {
            Leaf::Column { column, multi, .. } => {
                if fp.modifier == Some(Modifier::Length) {
                    if *multi {
                        return format!("COALESCE(json_array_length({a}.{}), 0)", q(column));
                    }
                    return "0".into();
                }
                format!("{a}.{}", q(column))
            }
            Leaf::Json { column, path, .. } => {
                let p = self.bind(Bind::Value(SqlValue::Text(json_path(path))));
                if fp.modifier == Some(Modifier::Length) {
                    return format!("COALESCE(json_array_length({a}.{}, {p}), 0)", q(column));
                }
                format!("json_extract({a}.{}, {p})", q(column))
            }
        }
    }

    fn path(&mut self, fp: &FieldPath) -> Opd {
        let root = Self::root(fp.root);
        if !fp.is_multi() {
            if fp.hops.is_empty() {
                return Opd::Scalar(self.leaf_scalar(&root, fp));
            }
            let mut from = Vec::new();
            let mut conds = Vec::new();
            let mut cur = root;
            for hop in &fp.hops {
                if let Hop::Forward {
                    column,
                    target_table,
                    ..
                } = hop
                {
                    let h = self.fresh("h");
                    from.push(format!("{} {h}", q(target_table)));
                    conds.push(format!("{h}.\"id\" = {cur}.{}", q(column)));
                    cur = h;
                }
            }
            let leaf = self.leaf_scalar(&cur, fp);
            let sub = format!(
                "(SELECT {leaf} FROM {} WHERE {})",
                from.join(", "),
                conds.join(" AND ")
            );
            if fp.modifier == Some(Modifier::Length) {
                return Opd::Scalar(format!("COALESCE({sub}, 0)"));
            }
            return Opd::Scalar(sub);
        }

        let mut from = Vec::new();
        let mut conds = Vec::new();
        let mut cur = root;
        for hop in &fp.hops {
            let h = self.fresh("h");
            match hop {
                Hop::Forward {
                    column,
                    multi: false,
                    target_table,
                    ..
                } => {
                    from.push(format!("{} {h}", q(target_table)));
                    conds.push(format!("{h}.\"id\" = {cur}.{}", q(column)));
                }
                Hop::Forward {
                    column,
                    multi: true,
                    target_table,
                    ..
                } => {
                    let j = self.fresh("j");
                    from.push(format!("json_each({cur}.{}) {j}", q(column)));
                    from.push(format!("{} {h}", q(target_table)));
                    conds.push(format!("{h}.\"id\" = {j}.value"));
                }
                Hop::Back {
                    source_table,
                    column,
                    multi,
                    ..
                } => {
                    from.push(format!("{} {h}", q(source_table)));
                    if *multi {
                        conds.push(format!(
                            "EXISTS (SELECT 1 FROM json_each({h}.{}) WHERE value = {cur}.\"id\")",
                            q(column)
                        ));
                    } else {
                        conds.push(format!("{h}.{} = {cur}.\"id\"", q(column)));
                    }
                }
            }
            cur = h;
        }
        let v = match &fp.leaf {
            Leaf::Column {
                column,
                multi: true,
                ..
            } => {
                let j = self.fresh("j");
                from.push(format!("json_each({cur}.{}) {j}", q(column)));
                format!("{j}.value")
            }
            Leaf::Json { column, path, .. } if fp.modifier == Some(Modifier::Each) => {
                let j = self.fresh("j");
                let p = self.bind(Bind::Value(SqlValue::Text(json_path(path))));
                from.push(format!("json_each({cur}.{}, {p}) {j}", q(column)));
                format!("{j}.value")
            }
            _ => self.leaf_scalar(&cur, fp),
        };
        let mut sql = format!("SELECT {v} AS v FROM {}", from.join(", "));
        if !conds.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&conds.join(" AND "));
        }
        Opd::Set(sql)
    }
}

enum PreparedRight {
    Plain(Opd),
    /// Placeholder of a precomputed LIKE pattern.
    Pattern(String),
}

enum PreparedRhs<'a> {
    Value(&'a str),
    Pattern(&'a str),
}

/// `?op`: some row satisfies `cond`; plain: no row violates it.
fn quantify(any: bool, set: &str, other: Option<&str>, cond: &str) -> String {
    let (from, cond) = match other {
        None => (format!("({set}) s"), cond.to_owned()),
        Some(r) => (format!("({set}) l, ({r}) r"), cond.to_owned()),
    };
    if any {
        format!("EXISTS (SELECT 1 FROM {from} WHERE {cond})")
    } else {
        format!("(NOT EXISTS (SELECT 1 FROM {from} WHERE NOT {cond}))")
    }
}

fn quantify_one(any: bool, set: &str, alias: &str, cond: &str) -> String {
    if any {
        format!("EXISTS (SELECT 1 FROM ({set}) {alias} WHERE {cond})")
    } else {
        format!("(NOT EXISTS (SELECT 1 FROM ({set}) {alias} WHERE NOT {cond}))")
    }
}

/// `$."a"."b"` for json_extract.
fn json_path(keys: &[String]) -> String {
    let mut p = String::from("$");
    for k in keys {
        p.push_str(&format!(".\"{}\"", k.replace('"', "\\\"")));
    }
    p
}
