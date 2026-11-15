//! Port of `lib/logger.js`: `[ISO] [LEVEL] message {json}` lines on stdout/stderr.

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

static DEVELOPER_MODE: AtomicBool = AtomicBool::new(false);

/// `DEVELOPER_MODE` uses `/^(1|true|yes|on)$/i` without trimming, like the JS logger.
pub fn init() {
    let raw = std::env::var("DEVELOPER_MODE").unwrap_or_default();
    let on = matches!(
        raw.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    );
    DEVELOPER_MODE.store(on, Ordering::Relaxed);
}

pub fn developer_mode() -> bool {
    DEVELOPER_MODE.load(Ordering::Relaxed)
}

/// `{ name, message }` the way `JSON.stringify` replacer in the JS logger renders an Error.
pub fn error_value(error: &anyhow::Error) -> Value {
    serde_json::json!({ "name": "Error", "message": format!("{error:#}") })
}

fn write(level: &str, message: &str, details: Option<&Value>) {
    let suffix = match details {
        None => String::new(),
        Some(Value::String(text)) => format!(" {text}"),
        Some(value) => format!(" {value}"),
    };
    let line = format!(
        "[{}] [{}] {message}{suffix}",
        crate::common::time::now_iso(),
        level.to_uppercase()
    );
    if level == "log" {
        println!("{line}");
    } else {
        eprintln!("{line}");
    }
}

pub fn debug(message: &str, details: Option<Value>) {
    if developer_mode() {
        write("log", message, details.as_ref());
    }
}

pub fn info(message: &str, details: Option<Value>) {
    write("log", message, details.as_ref());
}

pub fn warn(message: &str, details: Option<Value>) {
    write("warn", message, details.as_ref());
}

pub fn error(message: &str, details: Option<Value>) {
    write("error", message, details.as_ref());
}
