//! Cron schedules with the semantics of the npm `cron` package used by the JS bot:
//! 5-field (minute) or 6-field (second) patterns, POSIX weekdays (0/7 = Sunday), IANA timezone.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use croner::Cron;
use croner::parser::{CronParser, Seconds};

pub fn parse(pattern: &str) -> Result<Cron, String> {
    CronParser::builder()
        .seconds(Seconds::Optional)
        .build()
        .parse(pattern.trim())
        .map_err(|error| error.to_string())
}

/// Next run strictly after `after`, evaluated in `timezone`, returned in UTC.
pub fn next_after(cron: &Cron, timezone: Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    cron.find_next_occurrence(&after.with_timezone(&timezone), false)
        .ok()
        .map(|next| next.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn weekly_sunday_in_warsaw() {
        let cron = parse("0 3 * * 0").unwrap();
        // Friday 2026-11-13 12:00 UTC -> Sunday 2026-11-15 03:00 Europe/Warsaw = 02:00 UTC
        let after = Utc.with_ymd_and_hms(2026, 11, 13, 12, 0, 0).unwrap();
        let next = next_after(&cron, chrono_tz::Europe::Warsaw, after).unwrap();
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 11, 15, 2, 0, 0).unwrap());
    }

    #[test]
    fn six_fields_and_invalid() {
        assert!(parse("*/10 * * * * *").is_ok());
        assert!(parse("0 3 * * 7").is_ok());
        assert!(parse("nope").is_err());
    }
}
