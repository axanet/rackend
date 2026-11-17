//! SQLite value semantics for the reference evaluator (SPEC §9.6): storage
//! classes, comparison, LIKE, number-to-text and date arithmetic. The SQL
//! compiler produces queries with the same semantics.

use std::cmp::Ordering;

use serde_json::Value;
use time::{Date, Duration, Month, OffsetDateTime};

use crate::time::format_timestamp;

/// A value with SQLite storage-class semantics.
#[derive(Debug, Clone, PartialEq)]
pub enum SVal {
    /// NULL
    Null,
    /// INTEGER
    Int(i64),
    /// REAL
    Real(f64),
    /// TEXT
    Text(String),
}

impl SVal {
    fn class(&self) -> u8 {
        match self {
            Self::Null => 0,
            Self::Int(_) | Self::Real(_) => 1,
            Self::Text(_) => 2,
        }
    }

    /// Whether the value is NULL.
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// "Empty" for `= null` / `= ''`: NULL or the empty string.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Null => true,
            Self::Text(s) => s.is_empty(),
            _ => false,
        }
    }

    /// Text conversion as SQLite does it (`CAST(x AS TEXT)`); NULL stays None.
    pub fn as_text(&self) -> Option<String> {
        match self {
            Self::Null => None,
            Self::Int(i) => Some(i.to_string()),
            Self::Real(f) => Some(real_to_text(*f)),
            Self::Text(s) => Some(s.clone()),
        }
    }

    /// The value `json_extract` / `json_each` produce for a JSON value.
    pub fn from_json(v: &Value) -> Self {
        match v {
            Value::Null => Self::Null,
            Value::Bool(b) => Self::Int(i64::from(*b)),
            Value::Number(n) => n
                .as_i64()
                .map_or_else(|| Self::Real(n.as_f64().unwrap_or(0.0)), Self::Int),
            Value::String(s) => Self::Text(s.clone()),
            other => Self::Text(other.to_string()),
        }
    }
}

/// SQLite's ordering of two non-NULL-aware values (NULL < numbers < text;
/// numbers numerically; text by bytes).
pub fn compare(a: &SVal, b: &SVal) -> Ordering {
    match (a, b) {
        (SVal::Int(x), SVal::Int(y)) => x.cmp(y),
        (SVal::Int(x), SVal::Real(y)) => cmp_f64(*x as f64, *y),
        (SVal::Real(x), SVal::Int(y)) => cmp_f64(*x, *y as f64),
        (SVal::Real(x), SVal::Real(y)) => cmp_f64(*x, *y),
        (SVal::Text(x), SVal::Text(y)) => x.as_bytes().cmp(y.as_bytes()),
        _ => a.class().cmp(&b.class()),
    }
}

fn cmp_f64(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

/// SQLite's REAL to TEXT conversion (`%!.15g`): `5.0`, `0.1`, `1.0e+20`.
pub fn real_to_text(f: f64) -> String {
    if f == 0.0 {
        return "0.0".into();
    }
    if !f.is_finite() {
        return if f.is_nan() {
            "NaN".into()
        } else if f > 0.0 {
            "Inf".into()
        } else {
            "-Inf".into()
        };
    }
    let sci = format!("{f:.14e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if !(-5..15).contains(&exp) {
        let m = trim_zeros(mantissa);
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{m}e{sign}{:02}", exp.abs());
    }
    let decimals = usize::try_from((14 - exp).max(0)).unwrap_or(0);
    trim_zeros(&format!("{f:.decimals$}"))
}

fn trim_zeros(s: &str) -> String {
    if !s.contains('.') {
        return format!("{s}.0");
    }
    let t = s.trim_end_matches('0');
    if t.ends_with('.') {
        format!("{t}0")
    } else {
        t.to_owned()
    }
}

/// The LIKE pattern for the right-hand value of `~` (SPEC §9.3): the value
/// is used as-is if it contains `%`, otherwise wrapped in `%…%` with `%`,
/// `_` and `\` escaped (ESCAPE '\').
pub fn like_pattern(v: &str) -> String {
    if v.contains('%') {
        return v.to_owned();
    }
    let mut p = String::with_capacity(v.len() + 2);
    p.push('%');
    for c in v.chars() {
        if matches!(c, '%' | '_' | '\\') {
            p.push('\\');
        }
        p.push(c);
    }
    p.push('%');
    p
}

/// SQLite `LIKE` with `ESCAPE '\'`: ASCII case-insensitive, `%` any
/// sequence, `_` one character.
pub fn like(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.chars().collect();
    let p: Vec<char> = pattern.chars().collect();
    like_at(&t, &p)
}

fn like_at(t: &[char], p: &[char]) -> bool {
    let (mut ti, mut pi) = (0, 0);
    while pi < p.len() {
        match p[pi] {
            '%' => {
                while pi < p.len() && (p[pi] == '%' || p[pi] == '_') {
                    if p[pi] == '_' {
                        if ti >= t.len() {
                            return false;
                        }
                        ti += 1;
                    }
                    pi += 1;
                }
                if pi == p.len() {
                    return true;
                }
                return (ti..=t.len()).any(|k| like_at(&t[k..], &p[pi..]));
            }
            '_' => {
                if ti >= t.len() {
                    return false;
                }
                ti += 1;
                pi += 1;
            }
            c => {
                let lit = if c == '\\' {
                    pi += 1;
                    match p.get(pi) {
                        Some(&e) => e,
                        None => return false,
                    }
                } else {
                    c
                };
                if ti >= t.len() || !t[ti].eq_ignore_ascii_case(&lit) {
                    return false;
                }
                ti += 1;
                pi += 1;
            }
        }
    }
    ti == t.len()
}

/// One part of an ISO 8601 duration, applied like an SQLite date modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurationPart {
    /// `+N years`
    Years(i64),
    /// `+N months`
    Months(i64),
    /// `+N days`
    Days(i64),
    /// `+N hours`
    Hours(i64),
    /// `+N minutes`
    Minutes(i64),
    /// `+N seconds`
    Seconds(i64),
}

impl DurationPart {
    /// The SQLite modifier text.
    pub fn modifier(self) -> String {
        let (n, unit) = match self {
            Self::Years(n) => (n, "years"),
            Self::Months(n) => (n, "months"),
            Self::Days(n) => (n, "days"),
            Self::Hours(n) => (n, "hours"),
            Self::Minutes(n) => (n, "minutes"),
            Self::Seconds(n) => (n, "seconds"),
        };
        format!("{n:+} {unit}")
    }
}

/// Parses `[-]P[nY][nM][nW][nD][T[nH][nM][nS]]` (integers only).
pub fn parse_duration(s: &str) -> Option<Vec<DurationPart>> {
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => (-1, r),
        None => (1, s),
    };
    let rest = rest.strip_prefix('P')?;
    let mut parts = Vec::new();
    let mut in_time = false;
    let mut num = String::new();
    for c in rest.chars() {
        match c {
            'T' if !in_time && num.is_empty() => in_time = true,
            '0'..='9' => num.push(c),
            _ => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                let n = n.checked_mul(sign)?;
                parts.push(match (in_time, c) {
                    (false, 'Y') => DurationPart::Years(n),
                    (false, 'M') => DurationPart::Months(n),
                    (false, 'W') => DurationPart::Days(n.checked_mul(7)?),
                    (false, 'D') => DurationPart::Days(n),
                    (true, 'H') => DurationPart::Hours(n),
                    (true, 'M') => DurationPart::Minutes(n),
                    (true, 'S') => DurationPart::Seconds(n),
                    _ => return None,
                });
            }
        }
    }
    (num.is_empty() && !parts.is_empty()).then_some(parts)
}

/// Applies duration parts to a canonical date the way SQLite's date
/// modifiers do (month/year overflow rolls into the next month).
pub fn date_add(date: &str, parts: &[DurationPart]) -> Option<String> {
    let mut t = crate::domain::value::parse_date(date)?;
    for p in parts {
        t = match *p {
            DurationPart::Years(n) => add_months(t, n.checked_mul(12)?)?,
            DurationPart::Months(n) => add_months(t, n)?,
            DurationPart::Days(n) => t.checked_add(Duration::days(n))?,
            DurationPart::Hours(n) => t.checked_add(Duration::hours(n))?,
            DurationPart::Minutes(n) => t.checked_add(Duration::minutes(n))?,
            DurationPart::Seconds(n) => t.checked_add(Duration::seconds(n))?,
        };
    }
    Some(format_timestamp(t))
}

fn add_months(t: OffsetDateTime, n: i64) -> Option<OffsetDateTime> {
    let total = i64::from(t.year()) * 12 + i64::from(u8::from(t.month())) - 1 + n;
    let year = i32::try_from(total.div_euclid(12)).ok()?;
    let month = Month::try_from(u8::try_from(total.rem_euclid(12) + 1).ok()?).ok()?;
    let first = Date::from_calendar_date(year, month, 1).ok()?;
    // Day overflow rolls forward, like SQLite's computeJD.
    let day = first.checked_add(Duration::days(i64::from(t.day()) - 1))?;
    Some(t.replace_date(day))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_text_matches_sqlite() {
        for (f, s) in [
            (5.0, "5.0"),
            (0.1, "0.1"),
            (-2.5, "-2.5"),
            (1e20, "1.0e+20"),
            (123456.789, "123456.789"),
            (1e-7, "1.0e-07"),
        ] {
            assert_eq!(real_to_text(f), s, "{f}");
        }
    }

    #[test]
    fn like_semantics() {
        assert!(like("Hello", "%ell%"));
        assert!(like("HELLO", "%ell%"));
        assert!(!like("Héllo", "%ÉLL%"), "non-ASCII is case-sensitive");
        assert!(like("a_b", &like_pattern("_")));
        assert!(!like("ab", &like_pattern("_")));
        assert!(like("50%", "50\\%"));
        assert!(like("abc", "a_c"));
        assert!(!like("abc", "a\\"));
        assert_eq!(like_pattern("a%b"), "a%b");
    }

    #[test]
    fn comparison_classes() {
        assert_eq!(compare(&SVal::Int(1), &SVal::Real(1.0)), Ordering::Equal);
        assert_eq!(
            compare(&SVal::Int(9), &SVal::Text("1".into())),
            Ordering::Less
        );
        assert_eq!(compare(&SVal::Null, &SVal::Int(0)), Ordering::Less);
    }

    #[test]
    fn durations_and_month_overflow() {
        let p = parse_duration("P1M").unwrap_or_default();
        assert_eq!(
            date_add("2026-01-31T10:00:00.000Z", &p).as_deref(),
            Some("2026-03-03T10:00:00.000Z")
        );
        let p = parse_duration("-P1DT2H").unwrap_or_default();
        assert_eq!(
            date_add("2026-03-01T01:00:00.000Z", &p).as_deref(),
            Some("2026-02-27T23:00:00.000Z")
        );
        assert!(parse_duration("1D").is_none());
        assert!(parse_duration("P").is_none());
        assert_eq!(
            parse_duration("P2W").unwrap_or_default(),
            vec![DurationPart::Days(14)]
        );
    }
}
