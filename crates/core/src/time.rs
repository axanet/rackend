//! Timestamp conventions (SPEC §6.4): RFC 3339, UTC, milliseconds, `Z` suffix.

use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

/// Formats a timestamp as `2026-11-17T14:36:00.123Z`.
///
/// The output is lexicographically sortable.
pub fn format_timestamp(ts: OffsetDateTime) -> String {
    let fmt =
        format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");
    // Formatting a valid OffsetDateTime with a static description only fails for
    // years outside 0..=9999, which the UTC clock never produces; fall back to RFC 3339.
    let utc = ts.to_offset(UtcOffset::UTC);
    utc.format(&fmt).unwrap_or_else(|_| utc.to_string())
}

/// Formats a timestamp for use in file names: `20261117T143600Z`.
pub fn format_file_timestamp(ts: OffsetDateTime) -> String {
    let fmt = format_description!("[year][month][day]T[hour][minute][second]Z");
    let utc = ts.to_offset(UtcOffset::UTC);
    utc.format(&fmt)
        .unwrap_or_else(|_| utc.unix_timestamp().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn formats_with_millis_and_z() {
        let ts = datetime!(2026-11-17 14:36:00.1234 +02:00);
        assert_eq!(format_timestamp(ts), "2026-11-17T12:36:00.123Z");
    }

    #[test]
    fn formats_file_timestamp() {
        let ts = datetime!(2026-11-17 14:36:05 UTC);
        assert_eq!(format_file_timestamp(ts), "20261117T143605Z");
    }
}
