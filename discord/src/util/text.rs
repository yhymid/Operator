//! String helpers matching the JS formatting helpers (`truncateInline`, `normalizeInline`, ...).

/// Collapses whitespace runs into single spaces and trims.
pub fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Trim, then cut to `max` characters ending with `...` (JS `truncateText`).
pub fn truncate(value: &str, max: usize) -> String {
    let text = value.trim();
    if text.is_empty() {
        return String::new();
    }
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let head: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{}...", head.trim_end())
}

/// Collapse whitespace, then truncate (JS `truncateInline` / `normalizeInline`).
pub fn inline(value: &str, max: usize) -> String {
    truncate(&compact_whitespace(value), max)
}

/// Normalizes line endings, trims and truncates (JS `normalizeBlock`).
pub fn block(value: &str, max: usize) -> String {
    let text = value.replace("\r\n", "\n").replace('\r', "\n");
    truncate(text.trim(), max)
}

/// `/^\d{min,max}$/`
pub fn is_snowflake(value: &str, min: usize) -> bool {
    let len = value.len();
    (min..=22).contains(&len) && value.bytes().all(|b| b.is_ascii_digit())
}

/// `date.toLocaleString('pl-PL', { timeZone })` → `4.11.2026, 03:00:00` (day not padded).
pub fn pl_datetime(value: chrono::DateTime<chrono::Utc>, timezone: chrono_tz::Tz) -> String {
    value
        .with_timezone(&timezone)
        .format("%-d.%m.%Y, %H:%M:%S")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation_matches_js() {
        assert_eq!(truncate("  abc  ", 5), "abc");
        assert_eq!(truncate("abcdefghij", 6), "abc...");
        assert_eq!(inline("a \n\t b", 10), "a b");
        assert!(is_snowflake("780552865445969920", 16));
        assert!(!is_snowflake("12345", 16));
    }
}
