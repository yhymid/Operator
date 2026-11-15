//! Environment variable helpers matching the JS parsing rules.

use std::str::FromStr;

/// Loads `.env` from the current directory (if present). Variables that are already set
/// in the process environment win, same as `dotenv` in Node.
///
/// Must run before any other thread starts (it calls `std::env::set_var`).
#[allow(unsafe_code)]
pub fn load_dotenv() {
    let Ok(text) = std::fs::read_to_string(".env") else {
        return;
    };
    for (key, value) in parse_dotenv(&text) {
        if std::env::var_os(&key).is_none() {
            // SAFETY: called at the very start of `main`, before any threads are spawned.
            unsafe { std::env::set_var(key, value) };
        }
    }
}

/// Parses `.env` text with the rules of Node's `dotenv` package, so every file the JS bots
/// accepted keeps working: unquoted values may contain spaces (`Main Files/...`), `#` starts a
/// comment outside quotes, `'..'`, `"..."` and `` `..` `` quotes may span lines, `"..."`
/// expands `\n`/`\r`, `export KEY=..` and `KEY: value` are accepted, invalid lines are skipped.
pub fn parse_dotenv(text: &str) -> Vec<(String, String)> {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = Vec::new();
    let mut rest = text.as_str();
    while !rest.is_empty() {
        let (line, next) = rest.split_once('\n').unwrap_or((rest, ""));
        rest = next;
        let mut line = line.trim_start();
        if let Some(stripped) = line.strip_prefix("export ") {
            line = stripped.trim_start();
        }
        let key_len = line
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-')))
            .unwrap_or(line.len());
        if key_len == 0 {
            continue;
        }
        let key = &line[..key_len];
        let after = line[key_len..].trim_start();
        let value = if let Some(v) = after.strip_prefix('=') {
            v
        } else if let Some(v) = after.strip_prefix(':')
            && v.starts_with(char::is_whitespace)
        {
            v
        } else {
            continue;
        };
        let value = value.trim_start();
        let quote = value
            .chars()
            .next()
            .filter(|c| matches!(c, '\'' | '"' | '`'));
        let parsed = match quote {
            Some(q) => {
                // The closing quote may be on a later line.
                let body_and_rest = format!("{}\n{rest}", &value[1..]);
                let body_and_rest = if rest.is_empty() {
                    value[1..].to_owned()
                } else {
                    body_and_rest
                };
                match find_closing(&body_and_rest, q) {
                    Some(end) => {
                        let body = &body_and_rest[..end];
                        let consumed_lines = body.matches('\n').count();
                        for _ in 0..consumed_lines {
                            rest = rest.split_once('\n').map_or("", |(_, r)| r);
                        }
                        let body = body.replace(&format!("\\{q}"), &q.to_string());
                        if q == '"' {
                            body.replace("\\n", "\n").replace("\\r", "\r")
                        } else {
                            body
                        }
                    }
                    // Unterminated quote: dotenv keeps the raw value of this line.
                    None => strip_comment(value).to_owned(),
                }
            }
            None => strip_comment(value).to_owned(),
        };
        out.push((key.to_owned(), parsed));
    }
    out
}

fn find_closing(text: &str, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (index, c) in text.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return Some(index);
        }
    }
    None
}

fn strip_comment(value: &str) -> &str {
    value.split('#').next().unwrap_or("").trim()
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
    use super::{parse_bool, parse_dotenv};

    #[test]
    fn dotenv_matches_node_rules() {
        let text = "TOKEN=abc\n# comment\nPATH_A=/home/ubuntu/Main Files/x.txt\n\nexport B = spaced value  # note\n\
                    C=\"quoted # not comment\"\nD='single'\nE=\"a\\nb\"\nF=\"multi\nline\"\nG: colon\n\
                    bad line\nH=\nI=1\r\n";
        let got = parse_dotenv(text);
        let get = |k: &str| {
            got.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("TOKEN"), Some("abc"));
        assert_eq!(get("PATH_A"), Some("/home/ubuntu/Main Files/x.txt"));
        assert_eq!(get("B"), Some("spaced value"));
        assert_eq!(get("C"), Some("quoted # not comment"));
        assert_eq!(get("D"), Some("single"));
        assert_eq!(get("E"), Some("a\nb"));
        assert_eq!(get("F"), Some("multi\nline"));
        assert_eq!(get("G"), Some("colon"));
        assert_eq!(get("H"), Some(""));
        assert_eq!(get("I"), Some("1"));
        assert_eq!(get("bad"), None);
    }

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
