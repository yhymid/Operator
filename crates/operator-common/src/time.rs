//! Timestamps in the exact format JavaScript's `Date#toISOString()` produces
//! (`2026-01-31T12:00:00.000Z`), so state files stay interchangeable with the JS bots.

use chrono::{DateTime, Datelike, NaiveDate, SecondsFormat, TimeZone, Utc};

pub fn now_iso() -> String {
    to_iso(Utc::now())
}

pub fn to_iso(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Lenient ISO/RFC 3339 parser (accepts what `toISOString()` and most `Date.parse` inputs give).
pub fn parse_iso(raw: &str) -> Option<DateTime<Utc>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(value) = DateTime::parse_from_rfc3339(raw) {
        return Some(value.with_timezone(&Utc));
    }
    // Date-only input is interpreted as UTC midnight, same as `Date.parse("2026-01-31")`.
    NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .map(|naive| Utc.from_utc_datetime(&naive))
}

/// Unix seconds for Discord `<t:...>` markdown.
pub fn unix_seconds(value: DateTime<Utc>) -> i64 {
    value.timestamp()
}

/// Adds one calendar month (UTC), clamping the day to the target month's length.
/// `2026-01-31` → `2026-02-28`.
pub fn add_one_calendar_month(value: DateTime<Utc>) -> DateTime<Utc> {
    let (year, month) = if value.month() == 12 {
        (value.year() + 1, 1)
    } else {
        (value.year(), value.month() + 1)
    };
    let last_day = days_in_month(year, month);
    let day = value.day().min(last_day);
    let date = NaiveDate::from_ymd_opt(year, month, day).expect("valid clamped date");
    Utc.from_utc_datetime(&date.and_time(value.time()))
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (next_year, next_month) = if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    };
    NaiveDate::from_ymd_opt(next_year, next_month, 1)
        .and_then(|first| first.pred_opt())
        .map_or(28, |last| last.day())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_matches_js_format() {
        let value = Utc.with_ymd_and_hms(2026, 1, 31, 12, 5, 9).unwrap();
        assert_eq!(to_iso(value), "2026-01-31T12:05:09.000Z");
        assert_eq!(parse_iso("2026-01-31T12:05:09.000Z"), Some(value));
        assert_eq!(parse_iso(""), None);
        assert_eq!(parse_iso("garbage"), None);
    }

    #[test]
    fn calendar_month_clamps() {
        let jan31 = Utc.with_ymd_and_hms(2026, 1, 31, 8, 0, 0).unwrap();
        assert_eq!(
            to_iso(add_one_calendar_month(jan31)),
            "2026-02-28T08:00:00.000Z"
        );
        let dec15 = Utc.with_ymd_and_hms(2026, 12, 15, 0, 0, 0).unwrap();
        assert_eq!(
            to_iso(add_one_calendar_month(dec15)),
            "2027-01-15T00:00:00.000Z"
        );
        let jan31_leap = Utc.with_ymd_and_hms(2028, 1, 31, 0, 0, 0).unwrap();
        assert_eq!(
            to_iso(add_one_calendar_month(jan31_leap)),
            "2028-02-29T00:00:00.000Z"
        );
    }
}
