//! User-supplied regular expressions (AutoMod `blockedRegex`) are written in JavaScript syntax.
//! `fancy-regex` supports the JS features people actually use (look-around, back-references).

use fancy_regex::{Regex, RegexBuilder};

pub fn compile(pattern: &str, case_insensitive: bool) -> Result<Regex, String> {
    RegexBuilder::new(pattern)
        .case_insensitive(case_insensitive)
        .build()
        .map_err(|error| error.to_string())
}

/// Escapes a literal for use inside a pattern (JS `escapeRegExp`).
pub fn escape(value: &str) -> String {
    fancy_regex::escape(value).into_owned()
}
