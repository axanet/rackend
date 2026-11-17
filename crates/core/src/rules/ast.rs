//! Untyped syntax tree and its canonical printer.

use std::fmt;

use super::lexer::{CmpOp, Span};

/// Modifier suffix of a path (SPEC §9.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    /// `:isset`
    IsSet,
    /// `:length`
    Length,
    /// `:lower`
    Lower,
    /// `:each`
    Each,
}

impl Modifier {
    /// Source text without the colon.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IsSet => "isset",
            Self::Length => "length",
            Self::Lower => "lower",
            Self::Each => "each",
        }
    }
}

/// A dotted path with an optional modifier.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    /// Segments.
    pub segments: Vec<String>,
    /// Modifier.
    pub modifier: Option<Modifier>,
    /// Source range.
    pub span: Span,
}

/// A literal value.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    /// String.
    Str(String),
    /// Number (source text).
    Num(String),
    /// Boolean.
    Bool(bool),
    /// `null`
    Null,
}

/// An operand.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    /// Literal.
    Lit(Literal, Span),
    /// Field path on the current record.
    Path(Path),
    /// `@name` followed by a path (`@request.auth.id` → name `request`, path `auth.id`).
    /// `path.segments` may be empty (`@now`).
    Macro {
        /// Name after `@`.
        name: String,
        /// The rest.
        path: Path,
    },
    /// Function call.
    Func {
        /// Function name.
        name: String,
        /// Arguments.
        args: Vec<Operand>,
        /// Source range.
        span: Span,
    },
}

impl Operand {
    /// Source range.
    pub fn span(&self) -> Span {
        match self {
            Self::Lit(_, s) | Self::Func { span: s, .. } => *s,
            Self::Path(p) | Self::Macro { path: p, .. } => p.span,
        }
    }
}

/// An expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// `a || b || ...`
    Or(Vec<Expr>),
    /// `a && b && ...`
    And(Vec<Expr>),
    /// `!a`
    Not(Box<Expr>),
    /// `left op right`
    Cmp {
        /// Left operand.
        left: Operand,
        /// Operator.
        op: CmpOp,
        /// `?`-prefixed (at least one).
        any: bool,
        /// Right operand.
        right: Operand,
        /// Source range of the comparison.
        span: Span,
    },
}

fn write_path(f: &mut fmt::Formatter<'_>, p: &Path) -> fmt::Result {
    f.write_str(&p.segments.join("."))?;
    if let Some(m) = p.modifier {
        write!(f, ":{}", m.as_str())?;
    }
    Ok(())
}

impl fmt::Display for Operand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lit(Literal::Str(s), _) => {
                f.write_str("'")?;
                for c in s.chars() {
                    match c {
                        '\'' => f.write_str("\\'")?,
                        '\\' => f.write_str("\\\\")?,
                        '\n' => f.write_str("\\n")?,
                        '\t' => f.write_str("\\t")?,
                        '\r' => f.write_str("\\r")?,
                        c => write!(f, "{c}")?,
                    }
                }
                f.write_str("'")
            }
            Self::Lit(Literal::Num(n), _) => f.write_str(n),
            Self::Lit(Literal::Bool(b), _) => write!(f, "{b}"),
            Self::Lit(Literal::Null, _) => f.write_str("null"),
            Self::Path(p) => write_path(f, p),
            Self::Macro { name, path } => {
                write!(f, "@{name}")?;
                if !path.segments.is_empty() {
                    f.write_str(".")?;
                }
                write_path(f, path)
            }
            Self::Func { name, args, .. } => {
                write!(f, "{name}(")?;
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{a}")?;
                }
                f.write_str(")")
            }
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Or(items) | Self::And(items) => {
                let sep = if matches!(self, Self::Or(_)) {
                    " || "
                } else {
                    " && "
                };
                for (i, e) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(sep)?;
                    }
                    write!(f, "({e})")?;
                }
                Ok(())
            }
            Self::Not(e) => write!(f, "!({e})"),
            Self::Cmp {
                left,
                op,
                any,
                right,
                ..
            } => {
                write!(
                    f,
                    "{left} {}{} {right}",
                    if *any { "?" } else { "" },
                    op.as_str()
                )
            }
        }
    }
}
