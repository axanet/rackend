//! Five-field cron expressions (`minute hour day-of-month month day-of-week`)
//! in UTC, for scheduled backups (SPEC §15, §18.4).
//!
//! Supports `*`, numbers, lists (`1,15`), ranges (`1-5`) and steps (`*/15`,
//! `0-30/10`). Day of week is 0..=6 with Sunday 0 (7 is accepted as Sunday).
//! As in classic cron, when both day fields are restricted a day matches if
//! either does.

use time::{Duration, OffsetDateTime};

/// A parsed schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minutes: u64,
    hours: u64,
    days: u64,
    months: u64,
    weekdays: u64,
    days_any: bool,
    weekdays_any: bool,
}

fn field(src: &str, min: u32, max: u32) -> Option<(u64, bool)> {
    let mut bits = 0u64;
    let any = src == "*";
    for part in src.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().ok().filter(|s| *s > 0)?),
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (a.parse().ok()?, b.parse().ok()?)
        } else {
            let v: u32 = range.parse().ok()?;
            (v, if part.contains('/') { max } else { v })
        };
        if lo < min || hi > max || lo > hi {
            return None;
        }
        let mut v = lo;
        while v <= hi {
            bits |= 1 << v;
            v += step;
        }
    }
    (bits != 0).then_some((bits, any))
}

impl Cron {
    /// Parses five whitespace-separated fields.
    pub fn parse(src: &str) -> Option<Self> {
        let f: Vec<&str> = src.split_whitespace().collect();
        let [m, h, dom, mon, dow] = f.as_slice() else {
            return None;
        };
        let (minutes, _) = field(m, 0, 59)?;
        let (hours, _) = field(h, 0, 23)?;
        let (days, days_any) = field(dom, 1, 31)?;
        let (months, _) = field(mon, 1, 12)?;
        let (mut weekdays, weekdays_any) = field(dow, 0, 7)?;
        if weekdays & (1 << 7) != 0 {
            weekdays |= 1;
        }
        Some(Self {
            minutes,
            hours,
            days,
            months,
            weekdays,
            days_any,
            weekdays_any,
        })
    }

    /// Whether the minute containing `t` is scheduled.
    pub fn matches(&self, t: OffsetDateTime) -> bool {
        let has = |bits: u64, v: u8| bits & (1 << v) != 0;
        let dom = has(self.days, t.day());
        let dow = has(self.weekdays, t.weekday().number_days_from_sunday());
        let day = match (self.days_any, self.weekdays_any) {
            (true, true) => true,
            (false, true) => dom,
            (true, false) => dow,
            (false, false) => dom || dow,
        };
        has(self.minutes, t.minute())
            && has(self.hours, t.hour())
            && has(self.months, u8::from(t.month()))
            && day
    }

    /// The first scheduled minute strictly after `t` (within four years).
    pub fn next_after(&self, t: OffsetDateTime) -> Option<OffsetDateTime> {
        let mut c = t.replace_second(0).ok()?.replace_nanosecond(0).ok()? + Duration::minutes(1);
        for _ in 0..(4 * 366 * 24 * 60) {
            if self.matches(c) {
                return Some(c);
            }
            c += Duration::minutes(1);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use time::macros::datetime;

    #[test]
    fn parses_and_matches() {
        let c = Cron::parse("0 3 * * *").unwrap();
        assert_eq!(
            c.next_after(datetime!(2026-11-17 10:00 UTC)),
            Some(datetime!(2026-11-18 03:00 UTC))
        );
        assert_eq!(
            c.next_after(datetime!(2026-11-17 02:59:30 UTC)),
            Some(datetime!(2026-11-17 03:00 UTC))
        );
        let c = Cron::parse("*/15 9-17 * * 1-5").unwrap();
        // 2026-11-21 is a Saturday.
        assert_eq!(
            c.next_after(datetime!(2026-11-20 17:50 UTC)),
            Some(datetime!(2026-11-23 09:00 UTC))
        );
        let c = Cron::parse("30 0 1 * 0").unwrap();
        // Day of month OR Sunday: 2026-11-22 is a Sunday.
        assert!(c.matches(datetime!(2026-11-22 00:30 UTC)));
        assert!(c.matches(datetime!(2026-12-01 00:30 UTC)));
        assert!(
            Cron::parse("0 0 * * 7")
                .unwrap()
                .matches(datetime!(2026-11-22 00:00 UTC))
        );
        for bad in [
            "",
            "* * * *",
            "60 * * * *",
            "* 24 * * *",
            "*/0 * * * *",
            "5-1 * * * *",
            "x * * * *",
        ] {
            assert!(Cron::parse(bad).is_none(), "{bad}");
        }
    }
}
