//! Tokens of the rule language (SPEC §9.2).

use super::RuleError;

/// Byte range in the source.
#[derive(Debug, Clone, Copy, Default, Eq)]
pub struct Span {
    /// Start offset.
    pub start: usize,
    /// End offset (exclusive).
    pub end: usize,
}

/// Spans never take part in AST equality (round-trip tests compare shape).
impl PartialEq for Span {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// Comparison operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CmpOp {
    /// `=`
    Eq,
    /// `!=`
    Ne,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `~` (contains / LIKE)
    Like,
    /// `!~`
    NotLike,
}

impl CmpOp {
    /// Source text.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Eq => "=",
            Self::Ne => "!=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Like => "~",
            Self::NotLike => "!~",
        }
    }
}

/// A token.
#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    LParen,
    RParen,
    Comma,
    Dot,
    Colon,
    And,
    Or,
    Not,
    /// Comparison; `any` for the `?`-prefixed forms.
    Op {
        op: CmpOp,
        any: bool,
    },
    Str(String),
    /// Number source text (validated).
    Num(String),
    True,
    False,
    Null,
    Ident(String),
    /// `@name`
    At(String),
    Eof,
}

/// Splits `src` into tokens with spans.
pub fn lex(src: &str) -> Result<Vec<(Tok, Span)>, RuleError> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let start = i;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let two = bytes.get(i..i + 2).unwrap_or_default();
        let three = bytes.get(i..i + 3).unwrap_or_default();
        let (tok, len) = if three == b"?!=" {
            (
                Tok::Op {
                    op: CmpOp::Ne,
                    any: true,
                },
                3,
            )
        } else if three == b"?!~" {
            (
                Tok::Op {
                    op: CmpOp::NotLike,
                    any: true,
                },
                3,
            )
        } else if three == b"?>=" {
            (
                Tok::Op {
                    op: CmpOp::Ge,
                    any: true,
                },
                3,
            )
        } else if three == b"?<=" {
            (
                Tok::Op {
                    op: CmpOp::Le,
                    any: true,
                },
                3,
            )
        } else if two == b"&&" {
            (Tok::And, 2)
        } else if two == b"||" {
            (Tok::Or, 2)
        } else if two == b"!=" {
            (
                Tok::Op {
                    op: CmpOp::Ne,
                    any: false,
                },
                2,
            )
        } else if two == b"!~" {
            (
                Tok::Op {
                    op: CmpOp::NotLike,
                    any: false,
                },
                2,
            )
        } else if two == b">=" {
            (
                Tok::Op {
                    op: CmpOp::Ge,
                    any: false,
                },
                2,
            )
        } else if two == b"<=" {
            (
                Tok::Op {
                    op: CmpOp::Le,
                    any: false,
                },
                2,
            )
        } else if two == b"?=" {
            (
                Tok::Op {
                    op: CmpOp::Eq,
                    any: true,
                },
                2,
            )
        } else if two == b"?>" {
            (
                Tok::Op {
                    op: CmpOp::Gt,
                    any: true,
                },
                2,
            )
        } else if two == b"?<" {
            (
                Tok::Op {
                    op: CmpOp::Lt,
                    any: true,
                },
                2,
            )
        } else if two == b"?~" {
            (
                Tok::Op {
                    op: CmpOp::Like,
                    any: true,
                },
                2,
            )
        } else {
            match c {
                b'(' => (Tok::LParen, 1),
                b')' => (Tok::RParen, 1),
                b',' => (Tok::Comma, 1),
                b'.' => (Tok::Dot, 1),
                b':' => (Tok::Colon, 1),
                b'!' => (Tok::Not, 1),
                b'=' => (
                    Tok::Op {
                        op: CmpOp::Eq,
                        any: false,
                    },
                    1,
                ),
                b'>' => (
                    Tok::Op {
                        op: CmpOp::Gt,
                        any: false,
                    },
                    1,
                ),
                b'<' => (
                    Tok::Op {
                        op: CmpOp::Lt,
                        any: false,
                    },
                    1,
                ),
                b'~' => (
                    Tok::Op {
                        op: CmpOp::Like,
                        any: false,
                    },
                    1,
                ),
                b'\'' | b'"' => {
                    let (s, len) = lex_string(src, i)?;
                    out.push((
                        Tok::Str(s),
                        Span {
                            start,
                            end: start + len,
                        },
                    ));
                    i += len;
                    continue;
                }
                b'0'..=b'9' | b'-' => {
                    let mut j = i + 1;
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    if c == b'-' && j == i + 1 {
                        return Err(RuleError::at(src, i, "expected a digit after '-'"));
                    }
                    if j < bytes.len() && bytes[j] == b'.' {
                        let k = j + 1;
                        let mut m = k;
                        while m < bytes.len() && bytes[m].is_ascii_digit() {
                            m += 1;
                        }
                        if m == k {
                            return Err(RuleError::at(
                                src,
                                j,
                                "expected digits after the decimal point",
                            ));
                        }
                        j = m;
                    }
                    (Tok::Num(src[i..j].to_owned()), j - i)
                }
                b'@' => {
                    let (name, len) = ident(src, i + 1);
                    if len == 0 {
                        return Err(RuleError::at(src, i, "expected a name after '@'"));
                    }
                    (Tok::At(name.to_owned()), len + 1)
                }
                c if c.is_ascii_alphabetic() || c == b'_' => {
                    let (name, len) = ident(src, i);
                    let tok = match name {
                        "true" => Tok::True,
                        "false" => Tok::False,
                        "null" => Tok::Null,
                        _ => Tok::Ident(name.to_owned()),
                    };
                    (tok, len)
                }
                _ => {
                    let ch = src[i..].chars().next().unwrap_or('?');
                    return Err(RuleError::at(
                        src,
                        i,
                        format!("unexpected character {ch:?}"),
                    ));
                }
            }
        };
        out.push((
            tok,
            Span {
                start,
                end: start + len,
            },
        ));
        i += len;
    }
    out.push((
        Tok::Eof,
        Span {
            start: src.len(),
            end: src.len(),
        },
    ));
    Ok(out)
}

fn ident(src: &str, from: usize) -> (&str, usize) {
    let bytes = src.as_bytes();
    let mut j = from;
    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
        j += 1;
    }
    if j > from && bytes[from].is_ascii_digit() {
        return ("", 0);
    }
    (&src[from..j], j - from)
}

fn lex_string(src: &str, start: usize) -> Result<(String, usize), RuleError> {
    let quote = src.as_bytes()[start] as char;
    let mut out = String::new();
    let mut chars = src[start + 1..].char_indices();
    while let Some((k, c)) = chars.next() {
        match c {
            c if c == quote => return Ok((out, k + 2)),
            '\\' => match chars.next() {
                Some((_, 'n')) => out.push('\n'),
                Some((_, 't')) => out.push('\t'),
                Some((_, 'r')) => out.push('\r'),
                Some((_, c)) => out.push(c),
                None => break,
            },
            c => out.push(c),
        }
    }
    Err(RuleError::at(src, start, "unterminated string"))
}
