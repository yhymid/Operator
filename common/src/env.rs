//! Environment variable helpers matching the JS parsing rules.

use std::str::FromStr;

/// Loads `.env` from the current directory (if present). Variables that are already set
/// in the process environment win, same as `dotenv` in Node.
pub fn load_dotenv() {
    let _ = dotenvy::dotenv();
}

/// Returns the trimmed value of `name`, or `None` when unset or blank.
pub fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Parses a truthy flag: `1`, `true`, `yes`, `on` (case-insensitive) → `true`,
/// any other non-empty value → `false`, unset/blank → `fallback`.
pub fn bool(name: &str, fallback: bool) -> bool {
    var(name).map_or(fallback, |raw| parse_bool(&raw))
}

/// The truthy rule used by [`bool`], exposed for config values coming from other sources.
pub fn parse_bool(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Parses `name` as `T`, falling back when unset, blank or invalid.
pub fn parse<T: FromStr>(name: &str, fallback: T) -> T {
    var(name)
        .and_then(|raw| raw.parse::<T>().ok())
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::parse_bool;

    #[test]
    fn truthy_values() {
        for raw in ["1", "true", "TRUE", " yes ", "On"] {
            assert!(parse_bool(raw), "{raw}");
        }
        for raw in ["0", "false", "no", "off", "enabled", ""] {
            assert!(!parse_bool(raw), "{raw}");
        }
    }
}
