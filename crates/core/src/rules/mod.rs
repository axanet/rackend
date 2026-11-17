//! The rule & filter language (SPEC §9): lexer, parser, type checker and a
//! reference evaluator. SQL compilation lives in `store-sqlite`.

pub mod ast;
mod check;
pub mod eval;
pub mod lexer;
pub mod parser;
pub mod typed;
pub mod value;

use crate::domain::{Collection, RuleOp, Schema};
use crate::error::Position;

pub use typed::TypedRule;

/// Where an expression is used; decides which macros are allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A collection access rule for one operation.
    Rule(RuleOp),
    /// The client `filter` parameter (no `@collection`, max 2,048 chars).
    Filter,
    /// A realtime subscription filter (same restrictions as `filter`).
    Subscription,
    /// A channel join/publish rule (`@channel.key`, no current record).
    Channel,
}

/// A compile error with its position (SPEC §10.2.2 `rule_compile_error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleError {
    /// What is wrong.
    pub message: String,
    /// Where.
    pub position: Position,
}

impl RuleError {
    /// An error at byte `offset` of `src`.
    pub fn at(src: &str, offset: usize, message: impl Into<String>) -> Self {
        let offset = offset.min(src.len());
        let before = src.get(..offset).unwrap_or(src);
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        Self {
            message: message.into(),
            position: Position {
                line: u32::try_from(line).unwrap_or(u32::MAX),
                column: u32::try_from(column).unwrap_or(u32::MAX),
                offset: u32::try_from(offset).unwrap_or(u32::MAX),
            },
        }
    }
}

impl From<RuleError> for crate::Error {
    fn from(e: RuleError) -> Self {
        Self::RuleCompile {
            message: e.message,
            position: e.position,
        }
    }
}

/// Parses and type-checks `src` for `collection` (None for channel rules).
pub fn compile(
    schema: &Schema,
    collection: Option<&Collection>,
    src: &str,
    mode: Mode,
) -> Result<TypedRule, RuleError> {
    let max = match mode {
        Mode::Filter | Mode::Subscription => parser::MAX_FILTER_LEN,
        Mode::Rule(_) | Mode::Channel => parser::MAX_RULE_LEN,
    };
    let ast = parser::parse(src, max)?;
    check::check(schema, collection, src, mode, &ast)
}

/// The compiled state of one collection rule (SPEC §9.1).
#[derive(Debug, Clone)]
pub enum RuleState {
    /// `null`: superusers only.
    Locked,
    /// `""`: anyone.
    Public,
    /// An expression.
    Expr(std::sync::Arc<TypedRule>),
}

/// Compiles the rule of `op` for `collection`. A stored rule that fails to
/// compile is treated as locked (SPEC §5.4 step 7) and the error is logged.
pub fn compile_collection_rule(
    schema: &Schema,
    collection: &Collection,
    op: RuleOp,
) -> Result<RuleState, RuleError> {
    match collection.rules.get(op) {
        None => Ok(RuleState::Locked),
        Some(s) if s.trim().is_empty() => Ok(RuleState::Public),
        Some(s) => compile(schema, Some(collection), s, Mode::Rule(op))
            .map(|r| RuleState::Expr(std::sync::Arc::new(r))),
    }
}
