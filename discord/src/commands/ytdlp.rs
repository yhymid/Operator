//! `/ytdlp url:<URL>` (port of `src/cmds/ytdlp.js` from the VPS build).
//!
//! Downloads the best video+audio with yt-dlp, makes it play inline in Discord (H.264 8-bit +
//! AAC by default, see `YTDLP_VIDEO_CODEC`) and posts it as an attachment. Files above the
//! upload limit (or rejected by Discord as too large) are published on the website as a
//! player page that is deleted after 6 hours.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::discord::Ix;
use operator_common::env;

const MEDIA_TTL: Duration = Duration::from_secs(6 * 60 * 60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const DEFAULT_MAX_UPLOAD_BYTES: u64 = 25 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoCodec {
    H264,
    Av1,
    Source,
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
}

impl Settings {
    fn from_env() -> Self {
        let codec = match env::var("YTDLP_VIDEO_CODEC")
            .map(|v| v.to_ascii_lowercase())
            .as_deref()
        {
            Some("av1" | "av01") => VideoCodec::Av1,
            Some("source" | "copy" | "original") => VideoCodec::Source,
            _ => VideoCodec::H264,
        };
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
        return Err("Dozwolone sa tylko adresy http:// i https://.");
    }
    Ok(parsed.to_string())
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
            Err(last_line(&stderr).unwrap_or_else(|| format!("{program} zakonczyl sie bledem")))
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

#[derive(Debug, Default, PartialEq)]
struct StreamInfo {
    video: Option<String>,
    pix_fmt: Option<String>,
    audio: Option<String>,
}

fn parse_probe(output: &str) -> StreamInfo {
    let value: Value = serde_json::from_str(output).unwrap_or(Value::Null);
    let mut info = StreamInfo::default();
    for stream in value["streams"].as_array().into_iter().flatten() {
        let name = stream["codec_name"].as_str().map(str::to_owned);
        match stream["codec_type"].as_str() {
            Some("video") if info.video.is_none() => {
                info.video = name;
                info.pix_fmt = stream["pix_fmt"].as_str().map(str::to_owned);
            }
            Some("audio") if info.audio.is_none() => info.audio = name,
            _ => {}
        }
    }
    info
}

/// ffmpeg arguments producing an inline-playable MP4; returns whether video is re-encoded.
fn transcode_args(
    info: &StreamInfo,
    codec: VideoCodec,
    crf: Option<u32>,
    input: &Path,
    output: &Path,
) -> (Vec<String>, bool) {
    let eight_bit = matches!(info.pix_fmt.as_deref(), Some("yuv420p" | "yuvj420p"));
    let video_ok = match codec {
        VideoCodec::H264 => info.video.as_deref() == Some("h264") && eight_bit,
        VideoCodec::Av1 => info.video.as_deref() == Some("av1") && eight_bit,
        VideoCodec::Source => true,
    };
    let audio_ok = codec == VideoCodec::Source
        || info.audio.is_none()
        || matches!(info.audio.as_deref(), Some("aac" | "mp3"));
    let mut args: Vec<String> = ["-hide_banner", "-nostdin", "-y", "-i"]
        .map(str::to_owned)
        .to_vec();
    args.push(input.to_string_lossy().into_owned());
    args.extend(["-map", "0:v:0", "-map", "0:a:0?"].map(str::to_owned));
    if video_ok {
        args.extend(["-c:v", "copy"].map(str::to_owned));
    } else if codec == VideoCodec::Av1 {
        let crf = crf.unwrap_or(30).to_string();
        args.extend(
            [
                "-c:v",
                "libsvtav1",
                "-preset",
                "6",
                "-crf",
                &crf,
                "-pix_fmt",
                "yuv420p",
            ]
            .map(str::to_owned),
        );
    } else {
        let crf = crf.unwrap_or(18).to_string();
        args.extend(
            [
                "-c:v",
                "libx264",
                "-preset",
                "slow",
                "-crf",
                &crf,
                "-profile:v",
                "high",
                "-pix_fmt",
                "yuv420p",
            ]
            .map(str::to_owned),
        );
    }
    if audio_ok {
        args.extend(["-c:a", "copy"].map(str::to_owned));
    } else {
        args.extend(["-c:a", "aac", "-b:a", "192k"].map(str::to_owned));
    }
    args.extend(["-movflags", "+faststart"].map(str::to_owned));
    args.push(output.to_string_lossy().into_owned());
    (args, !video_ok)
}

/// Re-encodes HEVC/VP9/10-bit (Discord shows a frozen picture for HEVC) and remuxes for fast start.
async fn ensure_compatible(ix: &Ix<'_>, settings: &Settings, path: &Path) -> Result<PathBuf> {
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
    let (args, reencode) = transcode_args(&info, settings.codec, settings.crf, path, &output);
    tracing::info!(
        "ytdlp delivery video={:?} pix_fmt={:?} audio={:?} reencode={reencode}",
        info.video,
        info.pix_fmt,
        info.audio
    );
    if reencode {
        let label = if settings.codec == VideoCodec::Av1 {
            "AV1"
        } else {
            "H.264"
        };
        let _ = ix
            .edit_content(format!(
                "Konwertuje video do {label} (zrodlo: {}), zeby dzialalo w Discordzie...",
                info.video.as_deref().unwrap_or("?")
            ))
            .await;
    }
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
    let Ok(mut entries) = tokio::fs::read_dir(&root).await else {
        return;
    };
    let cutoff = SystemTime::now() - MEDIA_TTL;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Ok(meta) = entry.metadata().await
            && meta.is_file()
            && meta.modified().is_ok_and(|m| m < cutoff)
        {
            let _ = tokio::fs::remove_file(entry.path()).await;
        }
    }
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
    let url = match normalize_url(&raw) {
        Ok(url) => url,
        Err(message) => return ix.reply_ephemeral(message).await,
    };
    ix.defer(false).await?;
    cleanup_expired_media().await;
    let settings = Settings::from_env();
    let suffix: String = (0..6)
        .map(|_| char::from(b"abcdefghijklmnopqrstuvwxyz0123456789"[rand::random_range(0..36)]))
        .collect();
    let temp_dir = std::env::temp_dir().join(format!("yhymfiles-ytdlp-{suffix}"));
    tokio::fs::create_dir_all(&temp_dir).await?;
    let result = download_and_send(ix, &settings, &url, &temp_dir).await;
    if let Err(error) = tokio::fs::remove_dir_all(&temp_dir).await {
        tracing::warn!(
            "Nie udalo sie usunac katalogu tymczasowego {}: {error}",
            temp_dir.display()
        );
    }
    if let Err(error) = result {
        let details = last_line(&format!("{error:#}")).unwrap_or_default();
        tracing::error!("yt-dlp error: {error:#}");
        let text = if details.is_empty() {
            "Nie udalo sie pobrac video.".to_owned()
        } else {
            let short: String = details.chars().take(500).collect();
            format!("Nie udalo sie pobrac video: {short}")
        };
        ix.edit_content(text).await?;
    }
    Ok(())
}

async fn download_and_send(ix: &Ix<'_>, settings: &Settings, url: &str, dir: &Path) -> Result<()> {
    let mut args: Vec<String> = [
        "--no-playlist",
        "--no-progress",
        "-f",
        "bestvideo+bestaudio/best",
        "--format-sort",
        settings.format_sort(),
    ]
    .map(str::to_owned)
    .to_vec();
    if settings.codec != VideoCodec::Source {
        args.extend(["--merge-output-format", "mp4"].map(str::to_owned));
    }
    if is_youtube(url) && settings.cookies.exists() {
        args.extend([
            "--cookies".to_owned(),
            settings.cookies.to_string_lossy().into_owned(),
        ]);
    }
    args.push("-o".to_owned());
    args.push(dir.join("video.%(ext)s").to_string_lossy().into_owned());
    args.push(url.to_owned());
    run(&settings.ytdlp, &args, Some(dir), DOWNLOAD_TIMEOUT)
        .await
        .map_err(|e| anyhow!(e))?;
    let file = find_downloaded_file(dir)
        .await
        .ok_or_else(|| anyhow!("yt-dlp nie zwrocil pobranego pliku."))?;
    let file = ensure_compatible(ix, settings, &file).await?;
    let size = tokio::fs::metadata(&file).await?.len();
    if size > settings.max_upload_bytes {
        let link = publish(settings, &file).await?;
        return ix.edit_content(too_large_text(size, &link)).await;
    }
    let name = file
        .file_name()
        .map_or("video.mp4".to_owned(), |n| n.to_string_lossy().into_owned());
    let bytes = tokio::fs::read(&file).await?;
    match ix.edit_with_file("Gotowe:", &name, bytes).await {
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

/// Slash command definition (must match `commands.json`).
#[cfg(test)]
fn definition() -> Value {
    serde_json::json!({
        "name": "ytdlp",
        "description": "Pobiera video z podanego adresu i wysyla je na kanal",
        "dm_permission": false,
        "options": [{ "type": 3, "name": "url", "description": "Adres URL video", "required": true }]
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
    fn transcode_decisions() {
        let (p, o) = (Path::new("/t/video.mp4"), Path::new("/t/delivery.mp4"));
        let hevc = parse_probe(
            r#"{"streams":[{"codec_type":"video","codec_name":"hevc","pix_fmt":"yuv420p10le"},{"codec_type":"audio","codec_name":"opus"}]}"#,
        );
        let (args, reencode) = transcode_args(&hevc, VideoCodec::H264, None, p, o);
        assert!(reencode);
        assert!(args.join(" ").contains("-c:v libx264 -preset slow -crf 18"));
        assert!(args.join(" ").contains("-c:a aac -b:a 192k"));
        let h264 = parse_probe(
            r#"{"streams":[{"codec_type":"video","codec_name":"h264","pix_fmt":"yuv420p"},{"codec_type":"audio","codec_name":"aac"}]}"#,
        );
        assert!(!transcode_args(&h264, VideoCodec::H264, None, p, o).1);
    }

    #[test]
    fn manifest_entry_matches() {
        let manifest: Value = serde_json::from_str(include_str!("../../commands.json")).unwrap();
        let entry = manifest
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "ytdlp")
            .unwrap();
        assert_eq!(entry["options"], definition()["options"]);
    }
}
