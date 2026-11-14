//! `/ytdlp url:<URL>` (port of `src/cmds/ytdlp.js` from the VPS build).
//!
//! Downloads the best video+audio with yt-dlp, makes it play inline in Discord (AV1 8-bit +
//! AAC by default, H.264 optional, see `YTDLP_VIDEO_CODEC`) and posts it as an attachment. Files above the
//! upload limit (or rejected by Discord as too large) are published on the website as a
//! player page that is deleted after 6 hours.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use anyhow::{Result, anyhow, bail};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::discord::Ix;
use operator_common::env;
use operator_common::media::{Encode, VideoCodec, parse_probe, transcode_args};

const MEDIA_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const DEFAULT_MAX_UPLOAD_BYTES: u64 = 25 * 1024 * 1024;
const DEFAULT_AV1_PRESET: u8 = 10;
/// Longer videos are refused before download (`YTDLP_MAX_DURATION_SECONDS`, 0 = no limit).
const DEFAULT_MAX_DURATION_SECS: u64 = 30 * 60;
/// yt-dlp aborts formats above this size (`YTDLP_MAX_DOWNLOAD_BYTES`, 0 = no limit).
const DEFAULT_MAX_DOWNLOAD_BYTES: u64 = 1024 * 1024 * 1024;
/// Interaction tokens expire 15 minutes after the defer; keep a margin for the final edit.
const INTERACTION_BUDGET: Duration = Duration::from_secs(14 * 60);

/// One download/encode at a time: parallel SVT-AV1 encodes starve the VPS (and Lavalink audio).
fn job_slot() -> &'static tokio::sync::Semaphore {
    static SLOT: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    SLOT.get_or_init(|| tokio::sync::Semaphore::new(1))
}

struct Settings {
    ytdlp: String,
    ffmpeg: String,
    ffprobe: String,
    max_upload_bytes: u64,
    media_root: PathBuf,
    media_base_url: String,
    cookies: PathBuf,
    codec: VideoCodec,
    crf: Option<u32>,
    av1_preset: u8,
    js_runtime: String,
    max_duration_secs: u64,
    max_download_bytes: u64,
}

impl Settings {
    fn from_env() -> Self {
        let codec = VideoCodec::parse(env::var("YTDLP_VIDEO_CODEC").as_deref(), VideoCodec::Av1);
        let max_upload_bytes = env::var("YTDLP_MAX_UPLOAD_BYTES")
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .map_or(DEFAULT_MAX_UPLOAD_BYTES, |v| v as u64);
        Self {
            ytdlp: env::var("YTDLP_PATH").unwrap_or_else(|| "/usr/local/bin/yt-dlp".to_owned()),
            ffmpeg: env::var("FFMPEG_PATH").unwrap_or_else(|| "ffmpeg".to_owned()),
            ffprobe: env::var("FFPROBE_PATH").unwrap_or_else(|| "ffprobe".to_owned()),
            max_upload_bytes,
            media_root: PathBuf::from(
                env::var("YTDLP_MEDIA_ROOT")
                    .unwrap_or_else(|| "/home/ubuntu/Main Files/website/media".to_owned()),
            ),
            media_base_url: env::var("YTDLP_MEDIA_BASE_URL")
                .unwrap_or_else(|| "https://bladephoriafonk.xyz/media".to_owned())
                .trim_end_matches('/')
                .to_owned(),
            cookies: env::var("YTDLP_COOKIES_PATH")
                .map_or_else(|| PathBuf::from("cookies.txt"), PathBuf::from),
            codec,
            crf: env::var("YTDLP_VIDEO_CRF")
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|v| *v <= 63),
            // SVT-AV1 preset (0 = slowest/best .. 13 = fastest). 10 measured ~3x faster than 6 on the
            // VPS (6 vCPU) for ~11% larger files and nearly the same SSIM.
            av1_preset: env::var("YTDLP_AV1_PRESET")
                .and_then(|v| v.parse::<u8>().ok())
                .filter(|v| *v <= 13)
                .unwrap_or(DEFAULT_AV1_PRESET),
            // yt-dlp needs a JS runtime for YouTube challenges; it only looks for deno by default.
            js_runtime: env::var("YTDLP_JS_RUNTIME").unwrap_or_else(|| "node".to_owned()),
            max_duration_secs: env::var("YTDLP_MAX_DURATION_SECONDS")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(DEFAULT_MAX_DURATION_SECS),
            max_download_bytes: env::var("YTDLP_MAX_DOWNLOAD_BYTES")
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(DEFAULT_MAX_DOWNLOAD_BYTES),
        }
    }

    /// `--format-sort`: highest resolution/fps first, then SDR and the target codec on ties.
    fn format_sort(&self) -> &'static str {
        match self.codec {
            VideoCodec::H264 => "res,fps,hdr:sdr,vcodec:h264,acodec:aac,br,asr,size",
            VideoCodec::Av1 => "res,fps,hdr:sdr,vcodec:av01,acodec:aac,br,asr,size",
            VideoCodec::Source => "res,br,fps,asr,size",
        }
    }
}

fn normalize_url(value: &str) -> Result<String, &'static str> {
    let raw = value.trim();
    if raw.is_empty() || raw.encode_utf16().count() > 2048 {
        return Err("Podaj poprawny adres URL.");
    }
    let parsed = url::Url::parse(raw).map_err(|_| "Podaj poprawny adres URL.")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("Dozwolone są tylko adresy http:// i https://.");
    }
    Ok(parsed.to_string())
}

/// Refuses links to local/private addresses (Lavalink, cloud metadata, LAN) before yt-dlp runs.
async fn check_target(url: &str) -> Result<(), &'static str> {
    let parsed = url::Url::parse(url).map_err(|_| "Podaj poprawny adres URL.")?;
    let host = parsed.host_str().unwrap_or_default();
    let port = parsed.port_or_known_default().unwrap_or(443);
    operator_common::net::ensure_public_host(host, port).await
}

fn is_youtube(url: &str) -> bool {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_lowercase))
        .unwrap_or_default();
    host == "youtube.com"
        || host.ends_with(".youtube.com")
        || host == "youtu.be"
        || host.ends_with(".youtu.be")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn last_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .map(str::to_owned)
}

/// Runs a process with a timeout; `Err` carries the last stderr line (or the spawn error).
async fn run(
    program: &str,
    args: &[String],
    cwd: Option<&Path>,
    timeout: Duration,
) -> Result<String, String> {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let mut child = command.spawn().map_err(|e| format!("{program}: {e}"))?;
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let read_out = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(pipe) = out.as_mut() {
            let _ = pipe.read_to_end(&mut buf).await;
        }
        String::from_utf8_lossy(&buf).into_owned()
    });
    let read_err = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(pipe) = err.as_mut() {
            let _ = pipe.read_to_end(&mut buf).await;
        }
        String::from_utf8_lossy(&buf).into_owned()
    });
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => status.ok(),
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            None
        }
    };
    let stdout = read_out.await.unwrap_or_default();
    let stderr = read_err.await.unwrap_or_default();
    match status {
        Some(s) if s.success() => Ok(stdout),
        None => Err(format!("Przekroczono limit czasu ({program})")),
        Some(_) => {
            Err(last_line(&stderr).unwrap_or_else(|| format!("{program} zakończył się błędem")))
        }
    }
}

async fn find_downloaded_file(directory: &Path) -> Option<PathBuf> {
    let mut entries = tokio::fs::read_dir(directory).await.ok()?;
    let mut files = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".part") || name.ends_with(".ytdl") {
            continue;
        }
        if let Ok(meta) = entry.metadata().await
            && meta.is_file()
            && meta.len() > 0
        {
            files.push((entry.path(), meta.len()));
        }
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    files.into_iter().next().map(|f| f.0)
}

// ---------------------------------------------------------------------------------------------
// Codec compatibility
// ---------------------------------------------------------------------------------------------

/// Re-encodes HEVC/VP9/10-bit (Discord shows a frozen picture for HEVC) and remuxes for fast start.
/// AV1 is only there to fit more video into Discord's upload limit. An 8-bit H.264 source that
/// already fits (with ~10% headroom for the audio conversion) plays everywhere as it is, so it is
/// remuxed instead of spending minutes of CPU on an AV1 encode.
fn effective_codec(
    configured: VideoCodec,
    info: &operator_common::media::StreamInfo,
    size: u64,
    max_upload_bytes: u64,
) -> VideoCodec {
    let h264_8bit = info.video.as_deref() == Some("h264")
        && matches!(info.pix_fmt.as_deref(), Some("yuv420p" | "yuvj420p"));
    if configured == VideoCodec::Av1 && h264_8bit && size <= max_upload_bytes / 10 * 9 {
        VideoCodec::H264
    } else {
        configured
    }
}

async fn ensure_compatible(settings: &Settings, path: &Path) -> Result<PathBuf> {
    if settings.codec == VideoCodec::Source {
        return Ok(path.to_owned());
    }
    let probe_args: Vec<String> = [
        "-v",
        "error",
        "-show_entries",
        "stream=codec_type,codec_name,pix_fmt",
        "-of",
        "json",
    ]
    .map(str::to_owned)
    .into_iter()
    .chain(std::iter::once(path.to_string_lossy().into_owned()))
    .collect();
    let info = match run(
        &settings.ffprobe,
        &probe_args,
        None,
        Duration::from_secs(60),
    )
    .await
    {
        Ok(output) => parse_probe(&output),
        Err(error) => {
            tracing::warn!("ffprobe failed, sending the file as downloaded: {error}");
            return Ok(path.to_owned());
        }
    };
    if info.video.is_none() {
        return Ok(path.to_owned());
    }
    let output = path.with_file_name("delivery.mp4");
    let size = tokio::fs::metadata(path)
        .await
        .map_or(u64::MAX, |m| m.len());
    let encode = Encode {
        codec: effective_codec(settings.codec, &info, size, settings.max_upload_bytes),
        crf: settings.crf,
        av1_preset: settings.av1_preset,
    };
    let (args, reencode) = transcode_args(&info, encode, path, &output);
    tracing::info!(
        "ytdlp delivery video={:?} pix_fmt={:?} audio={:?} reencode={reencode}",
        info.video,
        info.pix_fmt,
        info.audio
    );
    run(&settings.ffmpeg, &args, None, DOWNLOAD_TIMEOUT)
        .await
        .map_err(|e| anyhow!("ffmpeg: {e}"))?;
    let _ = tokio::fs::remove_file(path).await;
    Ok(output)
}

// ---------------------------------------------------------------------------------------------
// Website publishing
// ---------------------------------------------------------------------------------------------

/// `cleanupExpiredMedia()`: removes published files older than 6 hours.
pub async fn cleanup_expired_media() {
    let root = Settings::from_env().media_root;
    cleanup_expired_in(&root, SystemTime::now() - MEDIA_TTL).await;
}

async fn cleanup_expired_in(root: &Path, cutoff: SystemTime) {
    let Ok(mut entries) = tokio::fs::read_dir(root).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        if !is_published_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
        if let Ok(meta) = entry.metadata().await
            && meta.is_file()
            && meta.modified().is_ok_and(|m| m < cutoff)
        {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
}

/// Names written by [`publish`]: a 36-character lowercase hex token plus one extension.
fn is_published_name(name: &str) -> bool {
    let Some((token, ext)) = name.split_once('.') else {
        return false;
    };
    token.len() == 36
        && token
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && !ext.is_empty()
        && ext.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// `publishLargeVideo`: `<token>.<ext>` + `<token>.html` player, removed after 6 hours.
async fn publish(settings: &Settings, file: &Path) -> Result<String> {
    tokio::fs::create_dir_all(&settings.media_root).await?;
    let token = hex(&rand::random::<[u8; 18]>());
    let ext = file
        .extension()
        .map_or("mp4".to_owned(), |e| e.to_string_lossy().to_lowercase());
    let media_name = format!("{token}.{ext}");
    let media_path = settings.media_root.join(&media_name);
    let html_path = settings.media_root.join(format!("{token}.html"));
    if tokio::fs::rename(file, &media_path).await.is_err() {
        // Different filesystem (EXDEV): copy instead.
        tokio::fs::copy(file, &media_path).await?;
        let _ = tokio::fs::remove_file(file).await;
    }
    let video_url = format!("{}/{media_name}", settings.media_base_url);
    let html = format!(
        "<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>Video</title><style>body{{margin:0;background:#111;display:grid;place-items:center;min-height:100vh}}video{{max-width:100%;max-height:100vh}}</style><video controls playsinline preload=\"metadata\" src=\"{video_url}\"></video>"
    );
    tokio::fs::write(&html_path, html).await?;
    tokio::spawn(async move {
        tokio::time::sleep(MEDIA_TTL).await;
        let _ = tokio::fs::remove_file(media_path).await;
        let _ = tokio::fs::remove_file(html_path).await;
    });
    Ok(format!("{}/{token}.html", settings.media_base_url))
}

fn too_large_text(size: u64, link: &str) -> String {
    format!(
        "Video pobrane ({:.1} MiB) i jest za duże na Discorda. Otwórz klasyczny player (link ważny 6 godzin): {link}",
        size as f64 / 1024.0 / 1024.0
    )
}

// ---------------------------------------------------------------------------------------------
// Command
// ---------------------------------------------------------------------------------------------

pub async fn execute(ix: &Ix<'_>) -> Result<()> {
    if !ix.in_guild() {
        return ix
            .reply_ephemeral("Ta komenda dziala tylko na serwerze.")
            .await;
    }
    let raw = ix.opts().string("url").unwrap_or_default();
    let audio = ix.opts().boolean("audio") == Some(true);
    let url = match normalize_url(&raw) {
        Ok(url) => url,
        Err(message) => return ix.reply_ephemeral(message).await,
    };
    ix.defer(false).await?;
    if let Err(message) = check_target(&url).await {
        tracing::warn!("ytdlp: odrzucono adres ({message})");
        return ix
            .edit_content(format!("Nie udało się pobrać video: {message}"))
            .await;
    }
    cleanup_expired_media().await;
    let settings = Settings::from_env();
    let suffix: String = (0..6)
        .map(|_| char::from(b"abcdefghijklmnopqrstuvwxyz0123456789"[rand::random_range(0..36)]))
        .collect();
    let temp_dir = std::env::temp_dir().join(format!("yhymfiles-ytdlp-{suffix}"));
    tokio::fs::create_dir_all(&temp_dir).await?;
    // Waiting for the slot counts against the same 15-minute token, hence one timeout for both.
    // Dropping the future on timeout kills yt-dlp/ffmpeg (`kill_on_drop`).
    let result = tokio::time::timeout(INTERACTION_BUDGET, async {
        let _slot = job_slot().acquire().await?;
        download_and_send(ix, &settings, &url, &temp_dir, audio).await
    })
    .await
    .unwrap_or_else(|_| {
        Err(anyhow!(
            "Przekroczono limit czasu (Discord pozwala odpowiedzieć w ciągu 15 minut)."
        ))
    });
    if let Err(error) = tokio::fs::remove_dir_all(&temp_dir).await {
        tracing::warn!(
            "Nie udalo sie usunac katalogu tymczasowego {}: {error}",
            temp_dir.display()
        );
    }
    if let Err(error) = result {
        tracing::error!("yt-dlp error: {error:#}");
        // Raw yt-dlp/ffmpeg output stays in the log: it can reveal paths or internal addresses.
        ix.edit_content(friendly_error(&format!("{error:#}")))
            .await?;
    }
    Ok(())
}

fn duration_filter(max_secs: u64) -> String {
    format!("duration <=? {max_secs}")
}

/// yt-dlp arguments selecting the media: best video+audio, or with `audio` only the sound,
/// extracted to MP3 (plays inline in Discord on every client).
fn format_args(settings: &Settings, audio: bool) -> Vec<String> {
    let args: &[&str] = if audio {
        &[
            "-f",
            "bestaudio/best",
            "-x",
            "--audio-format",
            "mp3",
            "--audio-quality",
            "192K",
        ]
    } else {
        &[
            "-f",
            "bestvideo+bestaudio/best",
            "--format-sort",
            settings.format_sort(),
        ]
    };
    let mut out: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    if !audio && settings.codec != VideoCodec::Source {
        out.extend(["--merge-output-format", "mp4"].map(str::to_owned));
    }
    out
}

async fn download_and_send(
    ix: &Ix<'_>,
    settings: &Settings,
    url: &str,
    dir: &Path,
    audio: bool,
) -> Result<()> {
    let mut args: Vec<String> = ["--no-playlist", "--no-progress"]
        .map(str::to_owned)
        .to_vec();
    args.extend(format_args(settings, audio));
    args.extend(["--js-runtimes".to_owned(), settings.js_runtime.clone()]);
    if settings.max_duration_secs > 0 {
        // `<=?` also passes media without a known duration (live streams, direct files).
        args.extend([
            "--match-filter".to_owned(),
            duration_filter(settings.max_duration_secs),
        ]);
    }
    if settings.max_download_bytes > 0 {
        args.extend([
            "--max-filesize".to_owned(),
            settings.max_download_bytes.to_string(),
        ]);
    }
    if is_youtube(url) && settings.cookies.exists() {
        args.extend([
            "--cookies".to_owned(),
            settings.cookies.to_string_lossy().into_owned(),
        ]);
    }
    args.push("-o".to_owned());
    let stem = if audio { "audio" } else { "video" };
    args.push(
        dir.join(format!("{stem}.%(ext)s"))
            .to_string_lossy()
            .into_owned(),
    );
    args.push(url.to_owned());
    let stdout = run(&settings.ytdlp, &args, Some(dir), DOWNLOAD_TIMEOUT)
        .await
        .map_err(|e| anyhow!(e))?;
    let Some(file) = find_downloaded_file(dir).await else {
        // Both limits make yt-dlp skip the video and still exit with success.
        if stdout.contains("does not pass filter") {
            bail!(
                "Film jest dłuższy niż {} min.",
                settings.max_duration_secs.div_ceil(60)
            );
        }
        if stdout.contains("larger than max-filesize") {
            bail!(
                "Plik jest większy niż {} MB.",
                settings.max_download_bytes / (1024 * 1024)
            );
        }
        bail!("yt-dlp nie zwrócił pobranego pliku.");
    };
    let file = if audio {
        file
    } else {
        ensure_compatible(settings, &file).await?
    };
    let size = tokio::fs::metadata(&file).await?.len();
    if size > settings.max_upload_bytes {
        let link = publish(settings, &file).await?;
        return ix.edit_content(too_large_text(size, &link)).await;
    }
    let name = file
        .file_name()
        .map_or("video.mp4".to_owned(), |n| n.to_string_lossy().into_owned());
    let bytes = tokio::fs::read(&file).await?;
    match ix.edit_with_file("", &name, bytes).await {
        Ok(()) => Ok(()),
        Err(error) => {
            let text = format!("{error:#}");
            // The server's real limit can be below YTDLP_MAX_UPLOAD_BYTES (no boosts).
            if text.contains("413")
                || text.to_lowercase().contains("too large")
                || text.contains("40005")
            {
                let link = publish(settings, &file).await?;
                return ix.edit_content(too_large_text(size, &link)).await;
            }
            bail!(text)
        }
    }
}

/// Message for the channel. Known yt-dlp failures get a plain explanation; anything else is
/// generic so raw tool output (paths, internal hosts) never reaches the chat.
fn friendly_error(details: &str) -> String {
    let lower = details.to_lowercase();
    let reason =
        if details.starts_with("Film jest dłuższy") || details.starts_with("Plik jest większy") {
            details.lines().next().unwrap_or(details)
        } else if lower.contains("przekroczono limit czasu") {
            "Przekroczono limit czasu pobierania lub konwersji."
        } else if lower.contains("confirm your age") || lower.contains("age-restricted") {
            "Film ma ograniczenie wiekowe."
        } else if lower.contains("sign in to confirm") || lower.contains("not a bot") {
            "YouTube zablokował pobieranie (wymaga zalogowania). Trzeba odświeżyć cookies bota."
        } else if lower.contains("private video") {
            "Film jest prywatny."
        } else if lower.contains("not available in your country")
            || lower.contains("geo restrict")
            || lower.contains("geo-restrict")
        {
            "Film jest niedostępny w regionie serwera."
        } else if lower.contains("unsupported url") {
            "Ten adres nie jest obsługiwany."
        } else if lower.contains("video unavailable") || lower.contains("has been removed") {
            "Film jest niedostępny."
        } else if lower.contains("http error 403") {
            "Serwer odmówił dostępu (403)."
        } else if lower.contains("http error 404") {
            "Nie znaleziono strony (404)."
        } else if lower.contains("is live") || lower.contains("live event") {
            "Transmisji na żywo nie da się pobrać."
        } else {
            return "Nie udało się pobrać video.".to_owned();
        };
    format!("Nie udało się pobrać video: {reason}")
}

/// Slash command definition (must match `commands.json`).
#[cfg(test)]
fn definition() -> serde_json::Value {
    serde_json::json!({
        "name": "ytdlp",
        "description": "Pobiera video z podanego adresu i wysyła je na kanał",
        "dm_permission": false,
        "options": [
            { "type": 3, "name": "url", "description": "Adres URL video", "required": true },
            { "type": 5, "name": "audio", "description": "Tylko dźwięk (MP3)", "required": false }
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert_eq!(
            normalize_url(" https://example.com ").unwrap(),
            "https://example.com/"
        );
        assert!(normalize_url("ftp://x").is_err());
        assert!(is_youtube("https://www.youtube.com/watch?v=x"));
        assert!(!is_youtube("https://notyoutube.com/"));
    }

    #[test]
    fn small_h264_skips_the_av1_encode() {
        let h264 = parse_probe(
            r#"{"streams":[{"codec_type":"video","codec_name":"h264","pix_fmt":"yuv420p"}]}"#,
        );
        let hevc = parse_probe(
            r#"{"streams":[{"codec_type":"video","codec_name":"hevc","pix_fmt":"yuv420p"}]}"#,
        );
        let limit = 25 * 1024 * 1024;
        assert_eq!(
            effective_codec(VideoCodec::Av1, &h264, 10_000_000, limit),
            VideoCodec::H264
        );
        assert_eq!(
            effective_codec(VideoCodec::Av1, &h264, limit, limit),
            VideoCodec::Av1
        );
        assert_eq!(
            effective_codec(VideoCodec::Av1, &hevc, 1_000, limit),
            VideoCodec::Av1
        );
        assert_eq!(
            effective_codec(VideoCodec::H264, &hevc, 1_000, limit),
            VideoCodec::H264
        );
    }

    /// Checked against yt-dlp: `yt_dlp.utils.match_filter_func("duration <=? 1800")` skips a
    /// 2 h video and passes 10 min and unknown durations; the earlier `!duration | ...` form is
    /// rejected by yt-dlp ("Invalid filter part") and failed every download.
    #[test]
    fn duration_filter_uses_ytdlp_syntax() {
        assert_eq!(duration_filter(1800), "duration <=? 1800");
    }

    #[test]
    fn audio_and_video_format_args() {
        let settings = Settings::from_env();
        let audio = format_args(&settings, true).join(" ");
        assert!(audio.contains("-x --audio-format mp3"));
        assert!(!audio.contains("--merge-output-format"));
        let video = format_args(&settings, false).join(" ");
        assert!(video.starts_with("-f bestvideo+bestaudio/best --format-sort"));
    }

    #[test]
    fn friendly_errors() {
        assert_eq!(
            friendly_error("ERROR: [youtube] abc: Sign in to confirm you’re not a bot."),
            "Nie udało się pobrać video: YouTube zablokował pobieranie (wymaga zalogowania). \
             Trzeba odświeżyć cookies bota."
        );
        assert_eq!(
            friendly_error("Film jest dłuższy niż 30 min."),
            "Nie udało się pobrać video: Film jest dłuższy niż 30 min."
        );
        assert_eq!(
            friendly_error(
                "ERROR: Unable to download webpage: <urlopen error [Errno 111] Connection refused> (10.0.0.5)"
            ),
            "Nie udało się pobrać video."
        );
        assert_eq!(
            friendly_error("ERROR: [generic] Unsupported URL: http://example.com/"),
            "Nie udało się pobrać video: Ten adres nie jest obsługiwany."
        );
    }

    #[test]
    fn manifest_entry_matches() {
        let manifest: serde_json::Value =
            serde_json::from_str(include_str!("../../commands.json")).unwrap();
        let entry = manifest
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "ytdlp")
            .unwrap();
        assert_eq!(entry["options"], definition()["options"]);
    }

    /// `YTDLP_MEDIA_ROOT` is a directory of the public website; the 6 h cleanup may only remove
    /// what `publish` created (`<36 hex>.<ext>` and `<36 hex>.html`).
    #[tokio::test]
    async fn media_cleanup_only_touches_published_files() {
        let root = std::env::temp_dir().join(format!("operator-media-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let token = "0123456789abcdef0123456789abcdef0123";
        let old = SystemTime::now() - MEDIA_TTL - Duration::from_secs(60);
        for name in [
            format!("{token}.mp4"),
            format!("{token}.html"),
            "index.html".to_owned(),
            "site.css".to_owned(),
        ] {
            let file = std::fs::File::create(root.join(&name)).unwrap();
            file.set_modified(old).unwrap();
        }
        std::fs::File::create(root.join("fresh.html")).unwrap();

        cleanup_expired_in(&root, SystemTime::now() - MEDIA_TTL).await;

        let mut left: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(left, ["fresh.html", "index.html", "site.css"]);
    }
}
