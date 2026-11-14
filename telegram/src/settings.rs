//! Environment configuration (the constants at the top of `bot.js` / `cmd/ytdlp.js`).

use std::collections::HashSet;
use std::path::PathBuf;

/// `Number(value)` for env strings: trimmed, empty → 0, invalid → NaN.
pub fn js_number(raw: Option<&str>) -> f64 {
    let Some(raw) = raw else { return f64::NAN };
    let text = raw.trim();
    if text.is_empty() {
        return 0.0;
    }
    text.parse::<f64>().unwrap_or(f64::NAN)
}

/// `Number(env) || fallback`
fn number_or(name: &str, fallback: f64) -> f64 {
    let value = js_number(std::env::var(name).ok().as_deref());
    if value.is_nan() || value == 0.0 {
        fallback
    } else {
        value
    }
}

/// `positiveInteger(env, fallback)`
pub fn positive_integer(raw: Option<&str>, fallback: u64) -> u64 {
    let value = js_number(raw);
    if value.is_finite() && value.fract() == 0.0 && value > 0.0 {
        value as u64
    } else {
        fallback
    }
}

fn positive_env(name: &str, fallback: u64) -> u64 {
    positive_integer(std::env::var(name).ok().as_deref(), fallback)
}

/// `!/^(0|false|no|off)$/i.test(env || 'true')`
fn not_disabled(name: &str) -> bool {
    let raw = std::env::var(name).ok().filter(|v| !v.is_empty());
    let raw = raw.unwrap_or_else(|| "true".to_owned());
    !matches!(
        raw.to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

fn env_or(name: &str, fallback: &str) -> String {
    std::env::var(name)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

fn split_list(raw: &str) -> impl Iterator<Item = &str> {
    raw.split(|c: char| c.is_whitespace() || c == ',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// Target video codec for delivered files (`YTDLP_VIDEO_CODEC`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    /// H.264 8-bit yuv420p + AAC in MP4: plays inline in Discord, Telegram and browsers.
    H264,
    /// AV1 8-bit (SVT-AV1) + AAC: smaller files; inline playback is not guaranteed everywhere.
    Av1,
    /// Keep whatever the source provides (only remux for fast start).
    Source,
}

impl VideoCodec {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Some("av1" | "av01") => Self::Av1,
            Some("source" | "copy" | "original") => Self::Source,
            _ => Self::H264,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub token: String,
    pub api_root: String,
    pub handler_timeout_ms: u64,
    pub ytdlp_path: String,
    pub max_upload_bytes: f64,
    pub download_timeout_secs: f64,
    pub cookies_path: PathBuf,
    pub js_runtime: String,
    pub direct_url_first: bool,
    pub progress_interval_ms: f64,
    pub website_root: PathBuf,
    pub public_base_url: String,
    /// Published player pages older than this are deleted (`YTDLP_WEBSITE_TTL_HOURS`).
    pub website_ttl_hours: u64,
    pub queue_concurrency: usize,
    pub queue_max_pending: usize,
    /// Queued + running jobs per user (`YTDLP_QUEUE_MAX_PER_USER`, 0 = no limit).
    pub queue_max_per_user: usize,
    /// Chats the bot serves (`ALLOWED_CHAT_IDS`); empty = every chat. Admins are always served.
    pub allowed_chat_ids: HashSet<i64>,
    pub size_probe: bool,
    pub probe_timeout_ms: u64,
    pub video_codec: VideoCodec,
    pub video_crf: Option<u32>,
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
    pub max_message_age_secs: u64,
    pub admin_ids: HashSet<String>,
    pub chat_min_interval_ms: u64,
    pub rate_limit_retries: u64,
    pub rate_limit_jitter_ms: u64,
}

impl Settings {
    pub fn from_env() -> anyhow::Result<Self> {
        let token = std::env::var("TELEGRAM_BOT_TOKEN")
            .ok()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Brak TELEGRAM_BOT_TOKEN w pliku .env"))?;
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let website_root = match std::env::var("YTDLP_WEBSITE_ROOT")
            .ok()
            .filter(|v| !v.is_empty())
        {
            Some(path) => cwd.join(path),
            None => cwd
                .parent()
                .map_or_else(|| cwd.join("website"), |p| p.join("website")),
        };
        let cookies_path = match std::env::var("YTDLP_COOKIES_PATH")
            .ok()
            .filter(|v| !v.is_empty())
        {
            Some(path) => PathBuf::from(path),
            None => cwd.join("cookies.txt"),
        };
        // `Math.max(0, Number(env || 1000) || 0)`
        let jitter = std::env::var("TELEGRAM_RATE_LIMIT_JITTER_MS")
            .ok()
            .filter(|v| !v.is_empty())
            .map_or(1000.0, |v| js_number(Some(&v)));
        Ok(Self {
            token,
            api_root: env_or("TELEGRAM_API_ROOT", "https://api.telegram.org")
                .trim_end_matches('/')
                .to_owned(),
            handler_timeout_ms: number_or("TELEGRAM_HANDLER_TIMEOUT_MS", 30.0 * 60.0 * 1000.0)
                as u64,
            ytdlp_path: env_or("YTDLP_PATH", "yt-dlp"),
            max_upload_bytes: number_or("YTDLP_MAX_UPLOAD_BYTES", 49.0 * 1000.0 * 1000.0),
            download_timeout_secs: number_or("YTDLP_TIMEOUT_SECONDS", 15.0 * 60.0),
            cookies_path,
            js_runtime: env_or("YTDLP_JS_RUNTIME", "node"),
            direct_url_first: not_disabled("TELEGRAM_DIRECT_URL_FIRST"),
            progress_interval_ms: number_or("YTDLP_PROGRESS_INTERVAL_MS", 2500.0),
            website_root,
            website_ttl_hours: positive_env("YTDLP_WEBSITE_TTL_HOURS", 24),
            public_base_url: env_or("YTDLP_PUBLIC_BASE_URL", "https://bladephoriafonk.xyz")
                .trim_end_matches('/')
                .to_owned(),
            queue_concurrency: positive_env("YTDLP_QUEUE_CONCURRENCY", 1) as usize,
            queue_max_pending: positive_env("YTDLP_QUEUE_MAX_PENDING", 20) as usize,
            queue_max_per_user: std::env::var("YTDLP_QUEUE_MAX_PER_USER")
                .ok()
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(3),
            allowed_chat_ids: split_list(&std::env::var("ALLOWED_CHAT_IDS").unwrap_or_default())
                .filter_map(|v| v.parse::<i64>().ok())
                .collect(),
            size_probe: not_disabled("YTDLP_QUEUE_SIZE_PROBE"),
            probe_timeout_ms: positive_env("YTDLP_QUEUE_PROBE_TIMEOUT_SECONDS", 30) * 1000,
            video_codec: VideoCodec::parse(std::env::var("YTDLP_VIDEO_CODEC").ok().as_deref()),
            video_crf: std::env::var("YTDLP_VIDEO_CRF")
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
                .filter(|v| *v <= 63),
            ffmpeg_path: env_or("FFMPEG_PATH", "ffmpeg"),
            ffprobe_path: env_or("FFPROBE_PATH", "ffprobe"),
            max_message_age_secs: positive_env("YTDLP_MAX_MESSAGE_AGE_SECONDS", 10 * 60),
            admin_ids: split_list(&std::env::var("ADMIN_IDS").unwrap_or_default())
                .map(str::to_owned)
                .collect(),
            chat_min_interval_ms: positive_env("TELEGRAM_CHAT_MIN_INTERVAL_MS", 3000),
            rate_limit_retries: positive_env("TELEGRAM_RATE_LIMIT_RETRIES", 5),
            rate_limit_jitter_ms: if jitter.is_finite() && jitter > 0.0 {
                jitter as u64
            } else {
                0
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_number_rules() {
        assert_eq!(js_number(Some(" 5 ")), 5.0);
        assert_eq!(js_number(Some("")), 0.0);
        assert!(js_number(Some("abc")).is_nan());
        assert!(js_number(None).is_nan());
        assert_eq!(positive_integer(Some("2.5"), 1), 1);
        assert_eq!(positive_integer(Some("3"), 1), 3);
        assert_eq!(positive_integer(Some("-3"), 1), 1);
    }
}
