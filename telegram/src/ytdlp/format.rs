#[allow(clippy::wildcard_imports)]
use super::*;

// ---------------------------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------------------------

/// `formatBytes(bytes)`
pub fn format_bytes(bytes: f64) -> String {
    if !bytes.is_finite() || bytes < 0.0 {
        return "? B".to_owned();
    }
    if bytes < 1024.0 {
        return format!("{bytes} B");
    }
    let units = ["KiB", "MiB", "GiB"];
    let mut value = bytes;
    let mut unit = "B";
    for (index, next) in units.iter().enumerate() {
        value /= 1024.0;
        unit = next;
        if value < 1024.0 || index == units.len() - 1 {
            break;
        }
    }
    let digits = if value >= 100.0 {
        0
    } else if value >= 10.0 {
        1
    } else {
        2
    };
    format!("{value:.digits$} {unit}")
}

pub(super) fn format_opt_bytes(bytes: Option<f64>) -> String {
    format_bytes(bytes.unwrap_or(f64::NAN))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub percent: f64,
    pub total_bytes: Option<f64>,
    pub downloaded_bytes: Option<f64>,
    pub speed: Option<String>,
    pub eta: Option<String>,
}

pub(super) fn parse_size(value: &str, unit: &str) -> f64 {
    let multiplier = |u: &str| match u {
        "B" => Some(1.0),
        "KB" => Some(1000.0),
        "MB" => Some(1000f64.powi(2)),
        "GB" => Some(1000f64.powi(3)),
        "KiB" => Some(1024.0),
        "MiB" => Some(1024f64.powi(2)),
        "GiB" => Some(1024f64.powi(3)),
        _ => None,
    };
    let factor = multiplier(unit)
        .or_else(|| multiplier(&unit.to_uppercase()))
        .unwrap_or(1.0);
    value.parse::<f64>().unwrap_or(f64::NAN) * factor
}

/// `parseProgress(line)`
pub fn parse_progress(line: &str) -> Option<Progress> {
    static PERCENT: OnceLock<Regex> = OnceLock::new();
    static TOTAL: OnceLock<Regex> = OnceLock::new();
    static SPEED: OnceLock<Regex> = OnceLock::new();
    static ETA: OnceLock<Regex> = OnceLock::new();
    if !line.contains("[download]") {
        return None;
    }
    let percent = re(&PERCENT, r"\[download\]\s+([\d.]+)%").captures(line)?;
    let percent = percent[1].parse::<f64>().unwrap_or(f64::NAN);
    let total = re(&TOTAL, r"(?i)\s+of\s+~?\s*([\d.]+)\s*([KMG]?i?B)")
        .captures(line)
        .map(|c| parse_size(&c[1], &c[2]))
        .filter(|t| *t != 0.0 && !t.is_nan());
    let speed = re(&SPEED, r"(?i)\s+at\s+([\d.]+\s*[KMG]?i?B/s)")
        .captures(line)
        .map(|c| c[1].to_owned());
    let eta = re(&ETA, r"(?i)\s+ETA\s+([\d:]+)")
        .captures(line)
        .map(|c| c[1].to_owned());
    Some(Progress {
        percent,
        total_bytes: total,
        downloaded_bytes: total.map(|t| (t * percent / 100.0).round()),
        speed,
        eta,
    })
}

/// `progressText(progress)`
pub fn progress_text(progress: &Progress) -> String {
    let downloaded = format_opt_bytes(progress.downloaded_bytes);
    let total = progress.total_bytes.map_or("?".to_owned(), format_bytes);
    let mut extras: Vec<String> = Vec::new();
    if let Some(speed) = progress.speed.as_ref().filter(|s| !s.is_empty()) {
        extras.push(speed.clone());
    }
    if let Some(eta) = progress.eta.as_ref().filter(|s| !s.is_empty()) {
        extras.push(format!("ETA {eta}"));
    }
    let extras = if extras.is_empty() {
        String::new()
    } else {
        format!(" • {}", extras.join(" • "))
    };
    format!(
        "⏬ Pobieram… {:.1}% • {downloaded} / {total}{extras}",
        progress.percent
    )
}

/// `normalizeUrl(value)`
pub fn normalize_url(value: &str) -> Result<String, String> {
    let mut raw = value.trim().to_owned();
    if raw.to_lowercase().starts_with("url:") {
        raw = raw[4..].trim().to_owned();
    }
    let invalid = || "Podaj poprawny adres URL (http:// lub https://).".to_owned();
    if raw.is_empty() || utf16_len(&raw) > 2048 {
        return Err(invalid());
    }
    let parsed = url::Url::parse(&raw).map_err(|_| invalid())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none_or(str::is_empty) {
        return Err("Dozwolone są tylko poprawne adresy http:// i https://.".to_owned());
    }
    Ok(parsed.to_string())
}

pub fn is_youtube(url: &str) -> bool {
    static YT: OnceLock<Regex> = OnceLock::new();
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_lowercase))
        .unwrap_or_default();
    re(&YT, r"(^|\.)youtube\.com$|(^|\.)youtu\.be$").is_match(&host)
}

/// `extractAutoMediaUrl(text)`: YouTube Shorts, Instagram Reels and TikTok links.
pub fn extract_auto_media_url(text: &str) -> Option<String> {
    static LINK: OnceLock<Regex> = OnceLock::new();
    static TRAIL: OnceLock<Regex> = OnceLock::new();
    static REEL: OnceLock<Regex> = OnceLock::new();
    let found = re(&LINK, r"(?i)https?://[^\s<>]+").find(text)?;
    let candidate = re(&TRAIL, r#"[),.!?;:'"\]]+$"#)
        .replace(found.as_str(), "")
        .replace("\\&", "&");
    let parsed = url::Url::parse(&candidate).ok()?;
    let host = parsed.host_str()?.to_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let path = parsed.path().to_lowercase();
    let youtube_short =
        (host == "youtube.com" || host.ends_with(".youtube.com")) && path.starts_with("/shorts/");
    let instagram_reel = (host == "instagram.com" || host.ends_with(".instagram.com"))
        && re(&REEL, r"^/(reel|reels)/").is_match(&path);
    let tiktok = host == "tiktok.com" || host.ends_with(".tiktok.com");
    (youtube_short || instagram_reel || tiktok).then(|| parsed.to_string())
}

pub(super) fn short_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => {
            let value = format!("{}{}", parsed.host_str().unwrap_or_default(), parsed.path());
            if utf16_len(&value) > 80 {
                format!("{}…", utf16_prefix(&value, 77))
            } else {
                value
            }
        }
        Err(_) => utf16_prefix(url, 80),
    }
}

pub(super) fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

pub(super) fn media_mime_type(extension: &str) -> &'static str {
    match extension {
        ".webm" => "video/webm",
        ".mov" => "video/quicktime",
        _ => "video/mp4",
    }
}

/// `path.extname(file).toLowerCase()`
pub(super) fn extname(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.rfind('.') {
        Some(index) if index > 0 => name[index..].to_lowercase(),
        _ => String::new(),
    }
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
