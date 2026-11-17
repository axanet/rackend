//! Type checker: resolves paths against the schema and checks operand types
//! (SPEC §9.3, §9.5 limits).

use super::ast::{Expr, Literal, Modifier, Operand, Path};
use super::lexer::{CmpOp, Span};
use super::typed::{
    FieldPath, Func, Hop, Leaf, Lit, Param, Root, TExpr, TKind, TOperand, TypedRule, VType,
};
use super::value::parse_duration;
use super::{Mode, RuleError};
use crate::domain::field::FieldKind;
use crate::domain::value::normalize_date;
use crate::domain::{Collection, Field, RuleOp, Schema, ValueType};

/// Maximum relation hops per path.
pub const MAX_HOPS: usize = 3;
/// Maximum joins/subqueries per expression.
pub const MAX_JOINS: usize = 16;

/// Auth fields every auth record has, with their types.
const AUTH_SYSTEM: &[(&str, VType)] = &[
    ("id", VType::Text),
    ("collection_id", VType::Text),
    ("collection_name", VType::Text),
    ("email", VType::Text),
    ("email_verified", VType::Bool),
    ("created", VType::Date),
    ("updated", VType::Date),
];

pub(super) fn check(
    schema: &Schema,
    collection: Option<&Collection>,
    src: &str,
    mode: Mode,
    ast: &Expr,
) -> Result<TypedRule, RuleError> {
    let mut c = Checker {
        schema,
        collection,
        src,
        mode,
        aliases: Vec::new(),
        joins: 0,
    };
    let expr = c.expr(ast)?;
    Ok(TypedRule {
        expr,
        aliases: c
            .aliases
            .into_iter()
            .map(|(_, id, table)| (id, table))
            .collect(),
        source: src.to_owned(),
    })
}

struct Checker<'a> {
    schema: &'a Schema,
    collection: Option<&'a Collection>,
    src: &'a str,
    mode: Mode,
    /// (name, collection id, table)
    aliases: Vec<(String, uuid::Uuid, String)>,
    joins: usize,
}

fn vtype(f: &Field) -> VType {
    match f.value_type() {
        ValueType::Text | ValueType::Relation(_) => VType::Text,
        ValueType::Number => VType::Number,
        ValueType::Bool => VType::Bool,
        ValueType::Date => VType::Date,
        ValueType::Any => VType::Any,
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur.push((prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

fn did_you_mean<'n>(name: &str, candidates: impl Iterator<Item = &'n str>) -> String {
    candidates
        .map(|c| (levenshtein(name, c), c))
        .filter(|(d, _)| *d <= 2)
        .min()
        .map(|(_, c)| format!(" Did you mean '{c}'?"))
        .unwrap_or_default()
}

impl Checker<'_> {
    fn err(&self, span: Span, msg: impl Into<String>) -> RuleError {
        RuleError::at(self.src, span.start, msg)
    }

    fn add_joins(&mut self, n: usize, span: Span) -> Result<(), RuleError> {
        self.joins += n;
        if self.joins > MAX_JOINS {
            return Err(self.err(
                span,
                format!("expression needs more than {MAX_JOINS} joins/subqueries"),
            ));
        }
        Ok(())
    }

    fn expr(&mut self, e: &Expr) -> Result<TExpr, RuleError> {
        Ok(match e {
            Expr::Or(items) => TExpr::Or(
                items
                    .iter()
                    .map(|i| self.expr(i))
                    .collect::<Result<_, _>>()?,
            ),
            Expr::And(items) => TExpr::And(
                items
                    .iter()
                    .map(|i| self.expr(i))
                    .collect::<Result<_, _>>()?,
            ),
            Expr::Not(inner) => TExpr::Not(Box::new(self.expr(inner)?)),
            Expr::Cmp {
                left,
                op,
                any,
                right,
                span,
            } => {
                let mut l = self.operand(left)?;
                let mut r = self.operand(right)?;
                self.check_cmp(&mut l, *op, &mut r, *span)?;
                TExpr::Cmp {
                    left: l,
                    op: *op,
                    any: *any,
                    right: r,
                }
            }
        })
    }

    /// Checks operand types for `op` and coerces date literals.
    fn check_cmp(
        &self,
        l: &mut TOperand,
        op: CmpOp,
        r: &mut TOperand,
        span: Span,
    ) -> Result<(), RuleError> {
        coerce_date_literal(l, r)
            .map_err(|lit| self.err(span, format!("'{lit}' is not a valid date")))?;
        coerce_date_literal(r, l)
            .map_err(|lit| self.err(span, format!("'{lit}' is not a valid date")))?;
        let (lt, rt) = (l.ty, r.ty);
        let mismatch = || {
            self.err(
                span,
                format!(
                    "cannot compare {} with {} using '{}'",
                    lt.name(),
                    rt.name(),
                    op.as_str()
                ),
            )
        };
        let compatible = lt == rt || lt == VType::Any || rt == VType::Any;
        match op {
            CmpOp::Eq | CmpOp::Ne => {
                if !(compatible || l.is_empty_literal() || r.is_empty_literal()) {
                    return Err(mismatch());
                }
            }
            CmpOp::Gt | CmpOp::Ge | CmpOp::Lt | CmpOp::Le => {
                if lt == VType::Null || rt == VType::Null {
                    return Err(self.err(span, "cannot order against null; use = null or != null"));
                }
                if lt == VType::Bool || rt == VType::Bool || !compatible {
                    return Err(mismatch());
                }
            }
            CmpOp::Like | CmpOp::NotLike => {
                let ok = |t: VType| matches!(t, VType::Text | VType::Any);
                if !ok(lt) || !ok(rt) {
                    return Err(self.err(
                        span,
                        format!(
                            "'{}' needs text on both sides, found {} and {}",
                            op.as_str(),
                            lt.name(),
                            rt.name()
                        ),
                    ));
                }
            }
        }
        Ok(())
    }

    fn operand(&mut self, o: &Operand) -> Result<TOperand, RuleError> {
        match o {
            Operand::Lit(lit, span) => {
                let (lit, ty) = match lit {
                    Literal::Null => (Lit::Null, VType::Null),
                    Literal::Bool(b) => (Lit::Bool(*b), VType::Bool),
                    Literal::Str(s) => (Lit::Text(s.clone()), VType::Text),
                    Literal::Num(n) => match n.parse::<i64>() {
                        Ok(i) if !n.contains('.') => (Lit::Int(i), VType::Number),
                        _ => match n.parse::<f64>() {
                            Ok(f) => (Lit::Real(f), VType::Number),
                            Err(_) => return Err(self.err(*span, format!("invalid number {n}"))),
                        },
                    },
                };
                Ok(TOperand {
                    kind: TKind::Lit(lit),
                    ty,
                    multi: false,
                    span: *span,
                })
            }
            Operand::Path(p) => {
                let Some(coll) = self.collection else {
                    return Err(self.err(
                        p.span,
                        "channel rules have no record; use @request, @channel or @collection",
                    ));
                };
                if let Some(lit) = collection_constant(coll, p) {
                    return Ok(lit);
                }
                let path = self.resolve_path(coll, Root::Record, p)?;
                Ok(self.wrap_path(path, p))
            }
            Operand::Macro { name, path } => self.macro_operand(name, path),
            Operand::Func { name, args, span } => self.func(name, args, *span),
        }
    }

    /// Turns a resolved path into an operand, applying `:lower`.
    fn wrap_path(&self, (fp, ty): (FieldPath, VType), p: &Path) -> TOperand {
        let multi = fp.is_multi();
        let ty = if fp.modifier == Some(Modifier::Length) {
            VType::Number
        } else {
            ty
        };
        let op = TOperand {
            kind: TKind::Path(fp),
            ty,
            multi,
            span: p.span,
        };
        if p.modifier == Some(Modifier::Lower) {
            return TOperand {
                kind: TKind::Func(Func::Lower, vec![op]),
                ty: VType::Text,
                multi,
                span: p.span,
            };
        }
        op
    }

    fn resolve_path(
        &mut self,
        start: &Collection,
        root: Root,
        p: &Path,
    ) -> Result<(FieldPath, VType), RuleError> {
        let mut coll: &Collection = start;
        let mut hops = Vec::new();
        let segs = &p.segments;
        let mut i = 0;
        loop {
            let seg = &segs[i];
            let last = i + 1 == segs.len();

            // System columns.
            if matches!(
                seg.as_str(),
                "id" | "created" | "updated" | "collection_id" | "collection_name"
            ) {
                if !last {
                    return Err(self.err(p.span, format!("'{seg}' has no fields")));
                }
                if seg == "collection_id" || seg == "collection_name" {
                    return Err(self.err(
                        p.span,
                        format!("'{seg}' is only available on the record itself"),
                    ));
                }
                let ty = if seg == "created" || seg == "updated" {
                    VType::Date
                } else {
                    VType::Text
                };
                let leaf = Leaf::Column {
                    field_id: None,
                    column: seg.clone(),
                    multi: false,
                };
                return self.finish(
                    FieldPath {
                        root,
                        hops,
                        leaf,
                        modifier: None,
                    },
                    ty,
                    p,
                );
            }

            // Back-relation: <collection>_via_<field>.
            if let Some((src_name, field_name)) = seg.split_once("_via_") {
                let Some(src) = self.schema.by_name(src_name) else {
                    return Err(self.err(
                        p.span,
                        format!("unknown collection '{src_name}' in '{seg}'"),
                    ));
                };
                let Some(f) = src
                    .field(field_name)
                    .filter(|f| f.relation_target() == Some(coll.id))
                else {
                    return Err(self.err(
                        p.span,
                        format!(
                            "'{src_name}.{field_name}' is not a relation to '{}'",
                            coll.name
                        ),
                    ));
                };
                if last {
                    return Err(self.err(
                        p.span,
                        format!("'{seg}' is a set of records; add a field, e.g. '{seg}.id'"),
                    ));
                }
                hops.push(Hop::Back {
                    source: src.id,
                    source_table: src.name.clone(),
                    field_id: f.id,
                    column: f.name.clone(),
                    multi: f.is_multi(),
                });
                self.after_hop(&hops, p.span)?;
                coll = src;
                i += 1;
                continue;
            }

            let Some(f) = coll.field(seg) else {
                let hint = did_you_mean(seg, coll.fields.iter().map(|f| f.name.as_str()));
                return Err(self.err(
                    p.span,
                    format!("unknown field '{seg}' in collection '{}'.{hint}", coll.name),
                ));
            };

            if last {
                let leaf = match f.kind {
                    FieldKind::Json { .. } => Leaf::Json {
                        field_id: f.id,
                        column: f.name.clone(),
                        path: Vec::new(),
                    },
                    _ => Leaf::Column {
                        field_id: Some(f.id),
                        column: f.name.clone(),
                        multi: f.is_multi(),
                    },
                };
                return self.finish(
                    FieldPath {
                        root,
                        hops,
                        leaf,
                        modifier: None,
                    },
                    vtype(f),
                    p,
                );
            }
            match &f.kind {
                FieldKind::Json { .. } => {
                    let leaf = Leaf::Json {
                        field_id: f.id,
                        column: f.name.clone(),
                        path: segs[i + 1..].to_vec(),
                    };
                    return self.finish(
                        FieldPath {
                            root,
                            hops,
                            leaf,
                            modifier: None,
                        },
                        VType::Any,
                        p,
                    );
                }
                FieldKind::Relation { collection_id, .. } => {
                    let Some(target) = self.schema.by_id(*collection_id) else {
                        return Err(self.err(
                            p.span,
                            format!("relation '{seg}' points to a missing collection"),
                        ));
                    };
                    hops.push(Hop::Forward {
                        field_id: f.id,
                        column: f.name.clone(),
                        multi: f.is_multi(),
                        target: target.id,
                        target_table: target.name.clone(),
                    });
                    self.after_hop(&hops, p.span)?;
                    coll = target;
                    i += 1;
                }
                _ => {
                    return Err(
                        self.err(p.span, format!("'{seg}' is not a relation or json field"))
                    );
                }
            }
        }
    }

    fn after_hop(&mut self, hops: &[Hop], span: Span) -> Result<(), RuleError> {
        if hops.len() > MAX_HOPS {
            return Err(self.err(
                span,
                format!("paths may traverse at most {MAX_HOPS} relations"),
            ));
        }
        self.add_joins(1, span)
    }

    /// Applies the modifier of `p` to a resolved path.
    fn finish(
        &mut self,
        mut fp: FieldPath,
        ty: VType,
        p: &Path,
    ) -> Result<(FieldPath, VType), RuleError> {
        let leaf_multi = matches!(fp.leaf, Leaf::Column { multi: true, .. });
        let is_json = matches!(fp.leaf, Leaf::Json { .. });
        match p.modifier {
            None | Some(Modifier::Lower) => {
                if p.modifier == Some(Modifier::Lower) && !matches!(ty, VType::Text | VType::Any) {
                    return Err(self.err(p.span, ":lower needs a text field"));
                }
            }
            Some(Modifier::IsSet) => {
                return Err(self.err(p.span, ":isset is only valid on @request.body fields"));
            }
            Some(Modifier::Length) => {
                if !(leaf_multi || is_json)
                    || !fp
                        .hops
                        .iter()
                        .all(|h| matches!(h, Hop::Forward { multi: false, .. }))
                {
                    return Err(self.err(p.span, ":length needs a multi-valued or json field without multi-valued relations before it"));
                }
                fp.modifier = Some(Modifier::Length);
            }
            Some(Modifier::Each) => {
                if !(leaf_multi || is_json) {
                    return Err(self.err(p.span, ":each needs a multi-valued or json field"));
                }
                fp.modifier = Some(Modifier::Each);
            }
        }
        if fp.is_multi() && (leaf_multi || fp.modifier == Some(Modifier::Each)) {
            self.add_joins(1, p.span)?;
        }
        Ok((fp, ty))
    }

    fn param(&self, param: Param, ty: VType, multi: bool, span: Span) -> TOperand {
        TOperand {
            kind: TKind::Param(param),
            ty,
            multi,
            span,
        }
    }

    fn macro_operand(&mut self, name: &str, path: &Path) -> Result<TOperand, RuleError> {
        let span = path.span;
        let segs: Vec<&str> = path.segments.iter().map(String::as_str).collect();
        let no_modifier = |c: &Self| -> Result<(), RuleError> {
            if path.modifier.is_some() {
                return Err(c.err(span, "modifiers are not supported here"));
            }
            Ok(())
        };
        match (name, segs.as_slice()) {
            ("now" | "today_start" | "today_end", []) => {
                no_modifier(self)?;
                let p = match name {
                    "now" => Param::Now,
                    "today_start" => Param::TodayStart,
                    _ => Param::TodayEnd,
                };
                Ok(self.param(p, VType::Date, false, span))
            }
            ("channel", ["key"]) => {
                if self.mode != Mode::Channel {
                    return Err(self.err(span, "@channel.key is only available in channel rules"));
                }
                no_modifier(self)?;
                Ok(self.param(Param::ChannelKey, VType::Text, false, span))
            }
            ("request", ["method"]) => {
                no_modifier(self)?;
                Ok(self.param(Param::Method, VType::Text, false, span))
            }
            ("request", ["context"]) => {
                no_modifier(self)?;
                Ok(self.param(Param::Context, VType::Text, false, span))
            }
            ("request", ["query", q]) => {
                let op = self.param(Param::Query((*q).to_owned()), VType::Text, false, span);
                self.lower_only(op, path)
            }
            ("request", ["headers", h]) => {
                let name = h.to_ascii_lowercase().replace('-', "_");
                let op = self.param(Param::Header(name), VType::Text, false, span);
                self.lower_only(op, path)
            }
            ("request", ["auth", field]) => {
                let (ty, multi) = self.auth_field(field, span)?;
                let op = self.param(
                    Param::Auth {
                        field: (*field).to_owned(),
                        multi,
                    },
                    ty,
                    multi,
                    span,
                );
                self.lower_only(op, path)
            }
            ("request", ["auth", _, ..]) => Err(self.err(
                span,
                "@request.auth fields cannot be traversed; compare ids instead",
            )),
            ("request", ["body", field]) => self.body_field(field, path),
            ("request", ["body", _, ..]) => {
                Err(self.err(span, "@request.body fields cannot be traversed"))
            }
            ("collection", [cname, rest @ ..]) if !rest.is_empty() => {
                if matches!(self.mode, Mode::Filter | Mode::Subscription) {
                    return Err(self.err(span, "@collection is not allowed in filters"));
                }
                let Some(target) = self.schema.by_name(cname) else {
                    let hint =
                        did_you_mean(cname, self.schema.all().iter().map(|c| c.name.as_str()));
                    return Err(self.err(span, format!("unknown collection '{cname}'.{hint}")));
                };
                let target = target.clone();
                let alias = match self.aliases.iter().position(|(n, _, _)| n == cname) {
                    Some(a) => a,
                    None => {
                        self.add_joins(1, span)?;
                        self.aliases
                            .push(((*cname).to_owned(), target.id, target.name.clone()));
                        self.aliases.len() - 1
                    }
                };
                let sub = Path {
                    segments: rest.iter().map(|s| (*s).to_owned()).collect(),
                    modifier: path.modifier,
                    span,
                };
                if let Some(lit) = collection_constant(&target, &sub) {
                    return Ok(lit);
                }
                let resolved = self.resolve_path(&target, Root::Alias(alias), &sub)?;
                Ok(self.wrap_path(resolved, &sub))
            }
            _ => {
                let full = if segs.is_empty() {
                    format!("@{name}")
                } else {
                    format!("@{name}.{}", segs.join("."))
                };
                Err(self.err(
                    span,
                    format!(
                        "unknown macro '{full}' (use @request.auth.*, @request.body.*, @request.query.*, @request.headers.*, @request.method, @request.context, @collection.*, @channel.key, @now, @today_start, @today_end)"
                    ),
                ))
            }
        }
    }

    /// Allows only `:lower` on request values.
    fn lower_only(&self, op: TOperand, path: &Path) -> Result<TOperand, RuleError> {
        match path.modifier {
            None => Ok(op),
            Some(Modifier::Lower) if matches!(op.ty, VType::Text | VType::Any) => Ok(TOperand {
                ty: VType::Text,
                multi: op.multi,
                span: op.span,
                kind: TKind::Func(Func::Lower, vec![op]),
            }),
            Some(m) => Err(self.err(path.span, format!(":{} is not supported here", m.as_str()))),
        }
    }

    fn auth_field(&self, field: &str, span: Span) -> Result<(VType, bool), RuleError> {
        if let Some((_, ty)) = AUTH_SYSTEM.iter().find(|(n, _)| *n == field) {
            return Ok((*ty, false));
        }
        // Fields of auth collections: the type must agree across them.
        let mut found: Option<(VType, bool)> = None;
        for c in self
            .schema
            .all()
            .iter()
            .filter(|c| c.kind == crate::domain::CollectionType::Auth)
        {
            if let Some(f) = c.field(field) {
                let t = (vtype(f), f.is_multi());
                found = Some(match found {
                    None => t,
                    Some(prev) if prev == t => prev,
                    Some(_) => (VType::Any, t.1),
                });
            }
        }
        found.ok_or_else(|| {
            let names = AUTH_SYSTEM.iter().map(|(n, _)| *n);
            let hint = did_you_mean(field, names);
            self.err(
                span,
                format!("unknown field '{field}' of @request.auth.{hint}"),
            )
        })
    }

    fn body_field(&mut self, field: &str, path: &Path) -> Result<TOperand, RuleError> {
        let span = path.span;
        if !matches!(self.mode, Mode::Rule(RuleOp::Create | RuleOp::Update)) {
            return Err(self.err(
                span,
                "@request.body is only available in create and update rules",
            ));
        }
        let Some(coll) = self.collection else {
            return Err(self.err(span, "@request.body needs a collection"));
        };
        let Some(f) = coll.field(field) else {
            let hint = did_you_mean(field, coll.fields.iter().map(|f| f.name.as_str()));
            return Err(self.err(
                span,
                format!(
                    "unknown field '{field}' in collection '{}'.{hint}",
                    coll.name
                ),
            ));
        };
        if path.modifier == Some(Modifier::IsSet) {
            return Ok(self.param(Param::BodyIsSet(field.to_owned()), VType::Bool, false, span));
        }
        let ty = vtype(f);
        let multi = f.is_multi();
        let op = self.param(
            Param::Body {
                field: field.to_owned(),
                multi,
                date: ty == VType::Date,
            },
            ty,
            multi,
            span,
        );
        match path.modifier {
            Some(Modifier::Each) if multi => Ok(op),
            Some(Modifier::Length) if multi => Ok(TOperand {
                ty: VType::Number,
                multi: false,
                span,
                kind: TKind::Func(Func::Len, vec![op]),
            }),
            _ => self.lower_only(op, path),
        }
    }

    fn func(&mut self, name: &str, args: &[Operand], span: Span) -> Result<TOperand, RuleError> {
        let mut typed: Vec<TOperand> = args
            .iter()
            .map(|a| self.operand(a))
            .collect::<Result<_, _>>()?;
        let arity = |n: usize, c: &Self| -> Result<(), RuleError> {
            if args.len() != n {
                return Err(c.err(span, format!("{name}() takes {n} argument(s)")));
            }
            Ok(())
        };
        if typed.iter().any(|t| t.multi) {
            return Err(self.err(
                span,
                format!(
                    "{name}() takes single values; use :each or :length for multi-valued fields"
                ),
            ));
        }
        let (func, ty) = match name {
            "lower" | "upper" => {
                arity(1, self)?;
                if !matches!(typed[0].ty, VType::Text | VType::Any | VType::Null) {
                    return Err(self.err(
                        span,
                        format!("{name}() needs text, found {}", typed[0].ty.name()),
                    ));
                }
                (
                    if name == "lower" {
                        Func::Lower
                    } else {
                        Func::Upper
                    },
                    VType::Text,
                )
            }
            "len" => {
                arity(1, self)?;
                if !matches!(typed[0].ty, VType::Text | VType::Any | VType::Null) {
                    return Err(self.err(
                        span,
                        format!("len() needs text, found {}", typed[0].ty.name()),
                    ));
                }
                (Func::Len, VType::Number)
            }
            "date_add" => {
                arity(2, self)?;
                coerce_date_literal(
                    &mut typed[0],
                    &TOperand {
                        kind: TKind::Lit(Lit::Null),
                        ty: VType::Date,
                        multi: false,
                        span,
                    },
                )
                .map_err(|lit| self.err(span, format!("'{lit}' is not a valid date")))?;
                if typed[0].ty != VType::Date {
                    return Err(self.err(
                        span,
                        format!("date_add() needs a date, found {}", typed[0].ty.name()),
                    ));
                }
                let parts = match &typed[1].kind {
                    TKind::Lit(Lit::Text(s)) => parse_duration(s),
                    _ => None,
                };
                let Some(parts) = parts else {
                    return Err(self.err(
                        span,
                        "date_add() needs an ISO 8601 duration literal such as 'P1D' or 'PT2H'",
                    ));
                };
                typed.truncate(1);
                (Func::DateAdd(parts), VType::Date)
            }
            "coalesce" => {
                if args.len() < 2 {
                    return Err(self.err(span, "coalesce() takes at least 2 arguments"));
                }
                let mut ty = VType::Null;
                for t in &typed {
                    ty = match (ty, t.ty) {
                        (VType::Null, x) | (x, VType::Null) => x,
                        (a, b) if a == b => a,
                        (VType::Any, _) | (_, VType::Any) => VType::Any,
                        (a, b) => {
                            return Err(self.err(
                                span,
                                format!(
                                    "coalesce() arguments must have one type, found {} and {}",
                                    a.name(),
                                    b.name()
                                ),
                            ));
                        }
                    };
                }
                (Func::Coalesce, ty)
            }
            _ => {
                return Err(self.err(
                    span,
                    format!("unknown function '{name}' (lower, upper, len, date_add, coalesce)"),
                ));
            }
        };
        Ok(TOperand {
            kind: TKind::Func(func, typed),
            ty,
            multi: false,
            span,
        })
    }
}

/// `collection_name` / `collection_id` of a collection are constants.
fn collection_constant(c: &Collection, p: &Path) -> Option<TOperand> {
    let value = match p.segments.as_slice() {
        [s] if s == "collection_name" => c.name.clone(),
        [s] if s == "collection_id" => c.id.to_string(),
        _ => return None,
    };
    if p.modifier.is_some() {
        return None;
    }
    Some(TOperand {
        kind: TKind::Lit(Lit::Text(value)),
        ty: VType::Text,
        multi: false,
        span: p.span,
    })
}

/// If `lit` is a non-empty text literal and `other` is a date, normalizes
/// the literal to canonical date form. Returns the bad literal on failure.
fn coerce_date_literal(lit: &mut TOperand, other: &TOperand) -> Result<(), String> {
    if other.ty != VType::Date || lit.ty != VType::Text {
        return Ok(());
    }
    if let TKind::Lit(Lit::Text(s)) = &lit.kind {
        if s.is_empty() {
            return Ok(());
        }
        let d = normalize_date(s).ok_or_else(|| s.clone())?;
        lit.kind = TKind::Lit(Lit::Text(d));
        lit.ty = VType::Date;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::super::{Mode, compile};
    use crate::domain::{Collection, RuleOp, Schema};
    use serde_json::json;

    fn schema() -> Schema {
        let users = Collection::from_input(
            &json!({"name": "users", "fields": [
                {"name": "name", "type": "text"}, {"name": "age", "type": "number"}
            ]}),
            None,
        )
        .unwrap();
        let posts = Collection::from_input(
            &json!({"name": "posts", "fields": [
                {"name": "title", "type": "text"},
                {"name": "author", "type": "relation", "collection_id": users.id},
                {"name": "editors", "type": "relation", "collection_id": users.id, "max_select": 5},
                {"name": "tags", "type": "select", "values": ["a", "b"], "max_select": 2},
                {"name": "meta", "type": "json"},
                {"name": "published", "type": "date"},
                {"name": "done", "type": "bool"}
            ]}),
            None,
        )
        .unwrap();
        let comments = Collection::from_input(
            &json!({"name": "comments", "fields": [
                {"name": "post", "type": "relation", "collection_id": posts.id},
                {"name": "body", "type": "text"}
            ]}),
            None,
        )
        .unwrap();
        Schema::new(vec![users, posts, comments], 1)
    }

    fn ok(s: &str, mode: Mode) {
        let sc = schema();
        let posts = sc.by_name("posts").unwrap().clone();
        compile(&sc, Some(&posts), s, mode).unwrap_or_else(|e| panic!("{s}: {e:?}"));
    }

    fn err(s: &str, mode: Mode) -> String {
        let sc = schema();
        let posts = sc.by_name("posts").unwrap().clone();
        compile(&sc, Some(&posts), s, mode)
            .map(|_| ())
            .unwrap_err()
            .message
    }

    #[test]
    fn valid_rules() {
        let r = Mode::Rule(RuleOp::List);
        for s in [
            "author = @request.auth.id",
            "author.name ~ 'x' && author.age > 3",
            "editors.name ?= 'bob'",
            "tags:length > 1",
            "tags:each ?= 'a'",
            "meta.a.b = 5 || meta = null",
            "published > '2026-01-01' && published < @now",
            "done = true",
            "comments_via_post.body ?~ 'hi'",
            "lower(title) = 'x' && title:lower = 'x'",
            "date_add(published, 'P1D') > @now",
            "coalesce(title, '') != ''",
            "@collection.users.name = 'x' && @collection.users.id = author",
            "collection_name = 'posts'",
        ] {
            ok(s, r);
        }
        ok(
            "@request.body.title:isset = false || @request.body.author = @request.auth.id",
            Mode::Rule(RuleOp::Update),
        );
        ok("@request.body.tags:length < 3", Mode::Rule(RuleOp::Create));
    }

    #[test]
    fn type_errors() {
        let r = Mode::Rule(RuleOp::List);
        assert!(err("done = 'x'", r).contains("cannot compare bool with text"));
        assert!(err("published > 5", r).contains("cannot compare date with number"));
        assert!(err("published > 'soon'", r).contains("not a valid date"));
        assert!(err("done > false", r).contains("cannot compare"));
        assert!(err("title > null", r).contains("cannot order against null"));
        assert!(err("author.age ~ 1", r).contains("needs text"));
    }

    #[test]
    fn resolution_errors_with_hints() {
        let r = Mode::Rule(RuleOp::List);
        assert!(err("titel = 'x'", r).contains("Did you mean 'title'?"));
        assert!(err("title.x = 1", r).contains("not a relation"));
        assert!(err("author.nope = 1", r).contains("unknown field 'nope' in collection 'users'"));
        assert!(err("users_via_post.id = 1", r).contains("is not a relation"));
        assert!(
            err("@request.body.title = 'x'", r).contains("only available in create and update")
        );
        assert!(
            err("@collection.users.name = 'x'", Mode::Filter).contains("not allowed in filters")
        );
        assert!(err("@channel.key = 'x'", r).contains("only available in channel rules"));
        assert!(err("@request.auth.team.name = 'x'", r).contains("cannot be traversed"));
        assert!(err("@nope = 1", r).contains("unknown macro"));
        assert!(err("title:length > 1", r).contains(":length needs"));
        assert!(err("title:isset = true", r).contains(":isset is only valid"));
        assert!(err("date_add(published, '1 day') > @now", r).contains("ISO 8601"));
        assert!(err("upper(tags) = 'A'", r).contains("single values"));
    }

    #[test]
    fn limits() {
        let r = Mode::Rule(RuleOp::List);
        let many = (0..17)
            .map(|i| format!("editors.name ?= '{i}'"))
            .collect::<Vec<_>>()
            .join(" || ");
        assert!(err(&many, r).contains("joins"));
    }
}
