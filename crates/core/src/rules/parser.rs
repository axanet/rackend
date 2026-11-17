//! Recursive-descent parser (SPEC §9.2). Hand-written, no dependencies.

use super::RuleError;
use super::ast::{Expr, Literal, Modifier, Operand, Path};
use super::lexer::{Span, Tok, lex};

/// Maximum expression length in characters (SPEC §9.5).
pub const MAX_RULE_LEN: usize = 4096;
/// Maximum `filter` parameter length.
pub const MAX_FILTER_LEN: usize = 2048;
/// Maximum AST depth.
pub const MAX_DEPTH: usize = 32;

/// Parses `src`. `max_len` is [`MAX_RULE_LEN`] or [`MAX_FILTER_LEN`].
pub fn parse(src: &str, max_len: usize) -> Result<Expr, RuleError> {
    if src.chars().count() > max_len {
        return Err(RuleError::at(
            src,
            0,
            format!("expression is longer than {max_len} characters"),
        ));
    }
    let toks = lex(src)?;
    let mut p = Parser {
        src,
        toks,
        pos: 0,
        depth: 0,
    };
    let e = p.or_expr()?;
    match p.peek() {
        Tok::Eof => Ok(e),
        t => {
            let t = describe(t);
            Err(p.err_here(format!(
                "unexpected {t}; expected && or || or the end of the expression"
            )))
        }
    }
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<(Tok, Span)>,
    pos: usize,
    depth: usize,
}

fn describe(t: &Tok) -> String {
    match t {
        Tok::Eof => "end of expression".into(),
        Tok::Ident(s) => format!("'{s}'"),
        Tok::At(s) => format!("'@{s}'"),
        Tok::Str(_) => "a string".into(),
        Tok::Num(n) => format!("number {n}"),
        Tok::Op { op, any } => format!("'{}{}'", if *any { "?" } else { "" }, op.as_str()),
        other => format!("{other:?}").to_lowercase(),
    }
}

impl Parser<'_> {
    fn peek(&self) -> &Tok {
        &self.toks[self.pos.min(self.toks.len() - 1)].0
    }

    fn span(&self) -> Span {
        self.toks[self.pos.min(self.toks.len() - 1)].1
    }

    fn bump(&mut self) -> (Tok, Span) {
        let t = self.toks[self.pos.min(self.toks.len() - 1)].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn err_here(&self, msg: impl Into<String>) -> RuleError {
        RuleError::at(self.src, self.span().start, msg)
    }

    fn enter(&mut self) -> Result<(), RuleError> {
        self.depth += 1;
        if self.depth > super::parser::MAX_DEPTH {
            return Err(self.err_here(format!(
                "expression is nested deeper than {MAX_DEPTH} levels"
            )));
        }
        Ok(())
    }

    fn or_expr(&mut self) -> Result<Expr, RuleError> {
        self.enter()?;
        let mut items = vec![self.and_expr()?];
        while *self.peek() == Tok::Or {
            self.bump();
            items.push(self.and_expr()?);
        }
        self.depth -= 1;
        Ok(if items.len() == 1 {
            items.remove(0)
        } else {
            Expr::Or(items)
        })
    }

    fn and_expr(&mut self) -> Result<Expr, RuleError> {
        self.enter()?;
        let mut items = vec![self.unary()?];
        while *self.peek() == Tok::And {
            self.bump();
            items.push(self.unary()?);
        }
        self.depth -= 1;
        Ok(if items.len() == 1 {
            items.remove(0)
        } else {
            Expr::And(items)
        })
    }

    fn unary(&mut self) -> Result<Expr, RuleError> {
        if *self.peek() == Tok::Not {
            self.bump();
            self.enter()?;
            let e = self.primary()?;
            self.depth -= 1;
            return Ok(Expr::Not(Box::new(e)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, RuleError> {
        if *self.peek() == Tok::LParen {
            // A parenthesised expression, unless it is a parenthesised operand
            // (not supported by the grammar, so always an expression).
            self.bump();
            let e = self.or_expr()?;
            if *self.peek() != Tok::RParen {
                return Err(self.err_here(format!("expected ')', found {}", describe(self.peek()))));
            }
            self.bump();
            return Ok(e);
        }
        let start = self.span().start;
        let left = self.operand()?;
        let (op, any) = match self.peek() {
            Tok::Op { op, any } => (*op, *any),
            t => {
                let t = describe(t);
                return Err(self.err_here(format!("expected a comparison operator, found {t}")));
            }
        };
        self.bump();
        let right = self.operand()?;
        let end = right.span().end;
        Ok(Expr::Cmp {
            left,
            op,
            any,
            right,
            span: Span { start, end },
        })
    }

    fn operand(&mut self) -> Result<Operand, RuleError> {
        let (tok, span) = self.bump();
        match tok {
            Tok::Str(s) => Ok(Operand::Lit(Literal::Str(s), span)),
            Tok::Num(n) => Ok(Operand::Lit(Literal::Num(n), span)),
            Tok::True => Ok(Operand::Lit(Literal::Bool(true), span)),
            Tok::False => Ok(Operand::Lit(Literal::Bool(false), span)),
            Tok::Null => Ok(Operand::Lit(Literal::Null, span)),
            Tok::At(name) => {
                let mut path = Path {
                    segments: Vec::new(),
                    modifier: None,
                    span,
                };
                if *self.peek() == Tok::Dot {
                    self.bump();
                    path = self.path(span.start)?;
                } else if *self.peek() == Tok::Colon {
                    path.modifier = Some(self.modifier()?);
                    path.span.end = self.toks[self.pos - 1].1.end;
                }
                Ok(Operand::Macro { name, path })
            }
            Tok::Ident(name) => {
                if *self.peek() == Tok::LParen {
                    self.bump();
                    let mut args = Vec::new();
                    if *self.peek() != Tok::RParen {
                        loop {
                            args.push(self.operand()?);
                            if *self.peek() == Tok::Comma {
                                self.bump();
                                continue;
                            }
                            break;
                        }
                    }
                    if *self.peek() != Tok::RParen {
                        return Err(self.err_here(format!(
                            "expected ')' or ',', found {}",
                            describe(self.peek())
                        )));
                    }
                    let end = self.bump().1.end;
                    return Ok(Operand::Func {
                        name,
                        args,
                        span: Span {
                            start: span.start,
                            end,
                        },
                    });
                }
                self.pos -= 1;
                Ok(Operand::Path(self.path(span.start)?))
            }
            other => {
                let msg = format!(
                    "expected a value, field or macro, found {}",
                    describe(&other)
                );
                Err(RuleError::at(self.src, span.start, msg))
            }
        }
    }

    fn path(&mut self, start: usize) -> Result<Path, RuleError> {
        let mut segments = Vec::new();
        loop {
            match self.bump() {
                (Tok::Ident(s), _) => segments.push(s),
                // Keywords are valid path segments after a dot (e.g. `meta.null`).
                (Tok::True, _) => segments.push("true".into()),
                (Tok::False, _) => segments.push("false".into()),
                (Tok::Null, _) => segments.push("null".into()),
                (t, sp) => {
                    return Err(RuleError::at(
                        self.src,
                        sp.start,
                        format!("expected a name, found {}", describe(&t)),
                    ));
                }
            }
            if *self.peek() == Tok::Dot {
                self.bump();
                continue;
            }
            break;
        }
        let modifier = if *self.peek() == Tok::Colon {
            Some(self.modifier()?)
        } else {
            None
        };
        let end = self.toks[self.pos.saturating_sub(1)].1.end;
        Ok(Path {
            segments,
            modifier,
            span: Span { start, end },
        })
    }

    fn modifier(&mut self) -> Result<Modifier, RuleError> {
        self.bump(); // ':'
        let (tok, span) = self.bump();
        match tok {
            Tok::Ident(s) => match s.as_str() {
                "isset" => Ok(Modifier::IsSet),
                "length" => Ok(Modifier::Length),
                "lower" => Ok(Modifier::Lower),
                "each" => Ok(Modifier::Each),
                _ => Err(RuleError::at(
                    self.src,
                    span.start,
                    format!("unknown modifier ':{s}' (isset, length, lower, each)"),
                )),
            },
            t => Err(RuleError::at(
                self.src,
                span.start,
                format!("expected a modifier, found {}", describe(&t)),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn p(s: &str) -> Expr {
        parse(s, MAX_RULE_LEN).unwrap_or_else(|e| panic!("{s}: {e:?}"))
    }

    #[test]
    fn parses_the_documented_forms() {
        for s in [
            "id = @request.auth.id",
            "@request.auth.id != ''",
            "@request.body.owner:isset = false || @request.body.owner = @request.auth.id",
            "comments_via_post.author ?= @request.auth.id",
            "tags:length > 2 && tags:each ~ 'a%'",
            "!(status = 'draft') && created >= @today_start",
            "lower(title) ~ \"foo\" // a comment",
            "date_add(created, 'P1D') < @now",
            "coalesce(a, b) = -1.5",
            "@collection.members.room = id && @collection.members.user = @request.auth.id",
            "@channel.key = 'x'",
        ] {
            p(s);
        }
    }

    #[test]
    fn precedence_and_or() {
        let e = p("a = 1 || b = 2 && c = 3");
        assert!(matches!(e, Expr::Or(ref v) if v.len() == 2 && matches!(v[1], Expr::And(_))));
    }

    #[test]
    fn errors_carry_positions() {
        let e = parse("title = ", MAX_RULE_LEN).unwrap_err();
        assert_eq!(e.position.column, 9);
        let e = parse("a = 1 &&\n  b == 2", MAX_RULE_LEN).unwrap_err();
        assert_eq!((e.position.line, e.position.column), (2, 6));
        assert!(
            parse("a = 'x", MAX_RULE_LEN)
                .unwrap_err()
                .message
                .contains("unterminated")
        );
        assert!(
            parse("a:bogus = 1", MAX_RULE_LEN)
                .unwrap_err()
                .message
                .contains("unknown modifier")
        );
        assert!(
            parse(&"x".repeat(5000), MAX_RULE_LEN)
                .unwrap_err()
                .message
                .contains("longer")
        );
        let deep = format!("{}a = 1{}", "(".repeat(40), ")".repeat(40));
        assert!(
            parse(&deep, MAX_RULE_LEN)
                .unwrap_err()
                .message
                .contains("deeper")
        );
    }

    #[test]
    fn printer_round_trips() {
        for s in [
            "a = 'it\\'s' && !(b ?!= null) || c:length >= -2.5",
            "@request.auth.id != '' && lower(x) ~ 'y'",
        ] {
            let e = p(s);
            assert_eq!(p(&e.to_string()), e, "{}", e);
        }
    }
}

#[cfg(test)]
mod proptests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use proptest::prelude::*;

    fn ident() -> impl Strategy<Value = String> {
        prop::sample::select(vec!["a", "title", "owner", "x_1", "meta", "tags"])
            .prop_map(str::to_owned)
    }

    fn operand() -> impl Strategy<Value = String> {
        prop_oneof![
            ".{0,6}".prop_map(|s: String| format!(
                "'{}'",
                s.replace('\\', "\\\\").replace('\'', "\\'")
            )),
            (-1000i64..1000).prop_map(|n| n.to_string()),
            (-100.0f64..100.0).prop_map(|f| format!("{f:.2}")),
            Just("true".to_owned()),
            Just("null".to_owned()),
            (
                prop::collection::vec(ident(), 1..4),
                prop::option::of(prop::sample::select(vec![
                    ":isset", ":length", ":lower", ":each"
                ]))
            )
                .prop_map(|(segs, m)| format!("{}{}", segs.join("."), m.unwrap_or(""))),
            ident().prop_map(|i| format!("@request.auth.{i}")),
            Just("@now".to_owned()),
            ident().prop_map(|a| format!("lower({a})")),
            (ident(), ident()).prop_map(|(a, b)| format!("coalesce({a}, {b})")),
        ]
    }

    fn expr() -> impl Strategy<Value = String> {
        let ops = prop::sample::select(vec![
            "=", "!=", ">", ">=", "<", "<=", "~", "!~", "?=", "?!=", "?>", "?~", "?!~", "?<=",
        ]);
        let leaf = (operand(), ops, operand()).prop_map(|(l, o, r)| format!("{l} {o} {r}"));
        leaf.prop_recursive(4, 24, 3, |inner| {
            prop_oneof![
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a}) && ({b})")),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("{a} || {b}")),
                inner.prop_map(|a| format!("!({a})")),
            ]
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 2000, failure_persistence: None, ..ProptestConfig::default() })]
        #[test]
        fn parse_print_round_trip(src in expr()) {
            let ast = parse(&src, MAX_RULE_LEN).unwrap();
            let printed = ast.to_string();
            prop_assert_eq!(parse(&printed, MAX_RULE_LEN).unwrap(), ast, "{}", printed);
        }

        #[test]
        fn never_panics_on_arbitrary_input(src in ".{0,200}") {
            let _ = parse(&src, MAX_RULE_LEN);
        }
    }
}
