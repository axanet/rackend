//! Value helpers shared by validation, storage and the rule evaluator.

use serde_json::{Number, Value};
use time::format_description::well_known::Rfc3339;
use time::macros::format_description;
use time::{Date, OffsetDateTime, PrimitiveDateTime, UtcOffset};

use crate::time::format_timestamp;

/// JSON for an `f64`: integral values within ±2^53 become JSON integers.
pub fn number_json(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() <= 9_007_199_254_740_992.0 {
        #[allow(clippy::cast_possible_truncation)]
        return Value::from(n as i64);
    }
    Number::from_f64(n).map_or(Value::Null, Value::Number)
}

/// Parses a date in one of the accepted forms and returns it in canonical
/// form (`2026-11-17T14:36:00.123Z`):
/// RFC 3339 with any offset, `YYYY-MM-DD HH:MM:SS[.fff][Z]`, or `YYYY-MM-DD`
/// (midnight UTC).
pub fn normalize_date(s: &str) -> Option<String> {
    parse_date(s).map(format_timestamp)
}

/// Parses a date (see [`normalize_date`]).
pub fn parse_date(s: &str) -> Option<OffsetDateTime> {
    let s = s.trim();
    if let Ok(t) = OffsetDateTime::parse(s, &Rfc3339) {
        return Some(t.to_offset(UtcOffset::UTC));
    }
    let spaced = s.replacen(' ', "T", 1);
    if let Ok(t) = OffsetDateTime::parse(&spaced, &Rfc3339) {
        return Some(t.to_offset(UtcOffset::UTC));
    }
    let naive = spaced.trim_end_matches('Z');
    let with_frac =
        format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond]");
    let no_frac = format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]");
    if let Ok(t) = PrimitiveDateTime::parse(naive, &with_frac)
        .or_else(|_| PrimitiveDateTime::parse(naive, &no_frac))
    {
        return Some(t.assume_utc());
    }
    let day = format_description!("[year]-[month]-[day]");
    Date::parse(s, &day).ok().map(|d| d.midnight().assume_utc())
}

/// Whether a value counts as "empty" for `required` (SPEC §8.4): null, empty
/// string or empty array. Numbers and booleans are never empty.
pub fn is_empty(v: &Value) -> bool {
    match v {
        Value::Null => true,
        Value::String(s) => s.is_empty(),
        Value::Array(a) => a.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_normalize_to_canonical_form() {
        for (input, want) in [
            ("2026-11-17T14:36:00.123Z", "2026-11-17T14:36:00.123Z"),
            ("2026-11-17T16:36:00+02:00", "2026-11-17T14:36:00.000Z"),
            ("2026-11-17 14:36:00", "2026-11-17T14:36:00.000Z"),
            ("2026-11-17 14:36:00.5Z", "2026-11-17T14:36:00.500Z"),
            ("2026-11-17", "2026-11-17T00:00:00.000Z"),
        ] {
            assert_eq!(normalize_date(input).as_deref(), Some(want), "{input}");
        }
        for bad in ["", "yesterday", "2026-13-01", "2026-11-17T25:00:00Z"] {
            assert_eq!(normalize_date(bad), None, "{bad}");
        }
    }

    #[test]
    fn integral_numbers_print_as_integers() {
        assert_eq!(number_json(3.0).to_string(), "3");
        assert_eq!(number_json(-2.5).to_string(), "-2.5");
    }
}
