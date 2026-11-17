//! The typed expression tree produced by the checker. Paths are resolved
//! against the schema: every hop and leaf names its field id and column.

use uuid::Uuid;

use super::ast::Modifier;
use super::lexer::{CmpOp, Span};
use super::value::DurationPart;

/// Scalar type of an operand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VType {
    /// Text (including ids, select values, emails, urls).
    Text,
    /// Number.
    Number,
    /// Boolean (stored as 0/1).
    Bool,
    /// Date-time (canonical RFC 3339 text).
    Date,
    /// Dynamic (JSON values).
    Any,
    /// The `null` literal.
    Null,
}

impl VType {
    /// Name for error messages.
    pub fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Bool => "bool",
            Self::Date => "date",
            Self::Any => "json",
            Self::Null => "null",
        }
    }
}

/// A compile-time constant.
#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    /// `null`
    Null,
    /// Text.
    Text(String),
    /// Integer.
    Int(i64),
    /// Real.
    Real(f64),
    /// Boolean (stored as 0/1).
    Bool(bool),
}

/// A value supplied by the request at evaluation time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Param {
    /// `@request.auth.<field>`.
    Auth {
        /// Field name.
        field: String,
        /// Multi-valued field.
        multi: bool,
    },
    /// `@request.body.<field>`.
    Body {
        /// Field name.
        field: String,
        /// Multi-valued field.
        multi: bool,
        /// Normalize text values as dates.
        date: bool,
    },
    /// `@request.body.<field>:isset`.
    BodyIsSet(String),
    /// `@request.query.<name>`.
    Query(String),
    /// `@request.headers.<name>` (lowercase, `-` → `_`).
    Header(String),
    /// `@request.method`.
    Method,
    /// `@request.context`.
    Context,
    /// `@channel.key`.
    ChannelKey,
    /// `@now`.
    Now,
    /// `@today_start`.
    TodayStart,
    /// `@today_end`.
    TodayEnd,
}

/// Where a path starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Root {
    /// The record the rule is evaluated for.
    Record,
    /// A row of `@collection.<name>` (alias index into [`TypedRule::aliases`]).
    Alias(usize),
}

/// One relation step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hop {
    /// Follow a relation field of the current row to `target`.
    Forward {
        /// Field id.
        field_id: Uuid,
        /// Column name.
        column: String,
        /// Multi-valued (JSON array of ids).
        multi: bool,
        /// Target collection id.
        target: Uuid,
        /// Target table name.
        target_table: String,
    },
    /// Rows of `source` whose relation `column` references the current row.
    Back {
        /// Collection holding the relation.
        source: Uuid,
        /// Its table name.
        source_table: String,
        /// Relation field id.
        field_id: Uuid,
        /// Relation column.
        column: String,
        /// The relation is multi-valued.
        multi: bool,
    },
}

/// The value a path ends in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leaf {
    /// A column. `multi`: JSON array of values (multi select/relation).
    Column {
        /// Field id (`None` for system columns).
        field_id: Option<Uuid>,
        /// Column name.
        column: String,
        /// Multi-valued.
        multi: bool,
    },
    /// A value inside a `json` column (`json_extract(col, '$.a.b')`); an
    /// empty path is the whole value.
    Json {
        /// Field id.
        field_id: Uuid,
        /// Column name.
        column: String,
        /// Keys.
        path: Vec<String>,
    },
}

/// A resolved field path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldPath {
    /// Start.
    pub root: Root,
    /// Relation steps (≤ 3).
    pub hops: Vec<Hop>,
    /// Final value.
    pub leaf: Leaf,
    /// `:each` or `:length` (`:lower` is a function, `:isset` a param).
    pub modifier: Option<Modifier>,
}

impl FieldPath {
    /// Whether the path yields a set of values.
    pub fn is_multi(&self) -> bool {
        if self.modifier == Some(Modifier::Length) {
            return false;
        }
        self.hops
            .iter()
            .any(|h| matches!(h, Hop::Forward { multi: true, .. } | Hop::Back { .. }))
            || matches!(self.leaf, Leaf::Column { multi: true, .. })
            || self.modifier == Some(Modifier::Each)
    }
}

/// Built-in function.
#[derive(Debug, Clone, PartialEq)]
pub enum Func {
    /// `lower(x)`
    Lower,
    /// `upper(x)`
    Upper,
    /// `len(x)`: character length, 0 for null.
    Len,
    /// `date_add(date, 'P…')`
    DateAdd(Vec<DurationPart>),
    /// `coalesce(a, b, …)`
    Coalesce,
}

/// What an operand is.
#[derive(Debug, Clone, PartialEq)]
pub enum TKind {
    /// Constant.
    Lit(Lit),
    /// Request value.
    Param(Param),
    /// Field path.
    Path(FieldPath),
    /// Function call (`:lower` compiles to `Func::Lower`).
    Func(Func, Vec<TOperand>),
}

/// A typed operand.
#[derive(Debug, Clone, PartialEq)]
pub struct TOperand {
    /// What it is.
    pub kind: TKind,
    /// Scalar type of its values.
    pub ty: VType,
    /// Whether it yields a set of values.
    pub multi: bool,
    /// Source range.
    pub span: Span,
}

impl TOperand {
    /// Whether this is the `null` literal or the empty-string literal: both
    /// mean "empty" for `=` and `!=` (SPEC §9.3).
    pub fn is_empty_literal(&self) -> bool {
        matches!(&self.kind, TKind::Lit(Lit::Null))
            || matches!(&self.kind, TKind::Lit(Lit::Text(s)) if s.is_empty())
    }
}

/// A typed expression.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)] // comparisons are the common case; boxing them only adds indirection
pub enum TExpr {
    /// Any is true.
    Or(Vec<TExpr>),
    /// All are true.
    And(Vec<TExpr>),
    /// Negation.
    Not(Box<TExpr>),
    /// Comparison.
    Cmp {
        /// Left operand.
        left: TOperand,
        /// Operator.
        op: CmpOp,
        /// `?`-form: at least one pair matches; plain: all pairs match.
        any: bool,
        /// Right operand.
        right: TOperand,
    },
}

/// A compiled rule.
#[derive(Debug, Clone, PartialEq)]
pub struct TypedRule {
    /// The expression.
    pub expr: TExpr,
    /// `@collection` aliases: (collection id, table name), by alias index.
    pub aliases: Vec<(Uuid, String)>,
    /// Source text.
    pub source: String,
}

impl TypedRule {
    /// Whether the rule reads request values only (no record data), so its
    /// result is the same for every record.
    pub fn uses_record(&self) -> bool {
        fn op(o: &TOperand) -> bool {
            match &o.kind {
                TKind::Path(_) => true,
                TKind::Func(_, args) => args.iter().any(op),
                _ => false,
            }
        }
        fn ex(e: &TExpr) -> bool {
            match e {
                TExpr::Or(v) | TExpr::And(v) => v.iter().any(ex),
                TExpr::Not(e) => ex(e),
                TExpr::Cmp { left, right, .. } => op(left) || op(right),
            }
        }
        ex(&self.expr)
    }
}

impl TypedRule {
    /// Whether any path of the rule reads `column` of a collection for which
    /// `pred(collection_id, column)` holds. `root` is the collection the rule
    /// is evaluated on.
    pub fn reads_column(&self, root: Uuid, pred: &dyn Fn(Uuid, &str) -> bool) -> bool {
        fn path(
            fp: &FieldPath,
            root: Uuid,
            aliases: &[(Uuid, String)],
            pred: &dyn Fn(Uuid, &str) -> bool,
        ) -> bool {
            let mut coll = match fp.root {
                Root::Record => root,
                Root::Alias(i) => aliases.get(i).map_or(root, |a| a.0),
            };
            for h in &fp.hops {
                coll = match h {
                    Hop::Forward { target, .. } => *target,
                    Hop::Back { source, .. } => *source,
                };
            }
            match &fp.leaf {
                Leaf::Column { column, .. } | Leaf::Json { column, .. } => pred(coll, column),
            }
        }
        fn op(
            o: &TOperand,
            root: Uuid,
            aliases: &[(Uuid, String)],
            pred: &dyn Fn(Uuid, &str) -> bool,
        ) -> bool {
            match &o.kind {
                TKind::Path(fp) => path(fp, root, aliases, pred),
                TKind::Func(_, args) => args.iter().any(|a| op(a, root, aliases, pred)),
                _ => false,
            }
        }
        fn ex(
            e: &TExpr,
            root: Uuid,
            aliases: &[(Uuid, String)],
            pred: &dyn Fn(Uuid, &str) -> bool,
        ) -> bool {
            match e {
                TExpr::Or(v) | TExpr::And(v) => v.iter().any(|x| ex(x, root, aliases, pred)),
                TExpr::Not(x) => ex(x, root, aliases, pred),
                TExpr::Cmp { left, right, .. } => {
                    op(left, root, aliases, pred) || op(right, root, aliases, pred)
                }
            }
        }
        ex(&self.expr, root, &self.aliases, pred)
    }
}
