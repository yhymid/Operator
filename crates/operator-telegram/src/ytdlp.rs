//! Port of `cmd/ytdlp.js`: `/ytdlp`, `/ytdlp_admin`, automatic short-video links, the shared
//! download queue, yt-dlp execution, Telegram upload strategies and website publishing.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use regex::Regex;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use crate::api::{ApiError, is_rate_limit_error};
use crate::bot::{Bot, Ctx};
use crate::log;
use crate::queue::{CancelOutcome, Job, JobState, NewJob, Queue, Runner, queue_status_text};
use crate::settings::VideoCodec;
use crate::status::StatusUpdater;

const BEST_FORMAT: &str = "bestvideo+bestaudio/best";

/// `-S` sort order. Highest resolution and frame rate always win; among equal ones SDR and
/// the target codec are preferred so fewer files need re-encoding. (The JS version passed
/// `res:desc,...`, which yt-dlp reads as "preferred value `desc`" and effectively ignores.)
fn format_sort(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "res,fps,hdr:sdr,vcodec:h264,acodec:aac,br,asr,size",
        VideoCodec::Av1 => "res,fps,hdr:sdr,vcodec:av01,acodec:aac,br,asr,size",
        VideoCodec::Source => "res,fps,br,asr,size",
    }
}

/// Single-file format Telegram may fetch by URL: only streams that already match the target.
fn direct_format(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "b[vcodec~='^(avc|h264)']",
        VideoCodec::Av1 => "b[vcodec~='^(av01|avc|h264)']",
        VideoCodec::Source => "best",
    }
}
const VIDEO_EXTENSIONS: [&str; 4] = [".mp4", ".m4v", ".webm", ".mov"];

fn re(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("static regex"))
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn utf16_prefix(text: &str, max: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().take(max).collect();
    String::from_utf16_lossy(&units)
}

fn error_text(error: &anyhow::Error) -> String {
    match error.downcast_ref::<ApiError>() {
        Some(api) => api.message.clone(),
        None => format!("{error:#}"),
    }
}

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

fn format_opt_bytes(bytes: Option<f64>) -> String {
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

fn parse_size(value: &str, unit: &str) -> f64 {
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

fn short_url(url: &str) -> String {
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

fn escape_html(value: &str) -> String {
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

fn media_mime_type(extension: &str) -> &'static str {
    match extension {
        ".webm" => "video/webm",
        ".mov" => "video/quicktime",
        _ => "video/mp4",
    }
}

/// `path.extname(file).toLowerCase()`
fn extname(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.rfind('.') {
        Some(index) if index > 0 => name[index..].to_lowercase(),
        _ => String::new(),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------------------------
// yt-dlp execution
// ---------------------------------------------------------------------------------------------

/// `pickYtdlpError(stderr, stdout)`
pub fn pick_ytdlp_error(stderr: &str, stdout: &str) -> String {
    static ERR: OnceLock<Regex> = OnceLock::new();
    let combined = format!("{stderr}\n{stdout}");
    let lines: Vec<&str> = combined
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let pattern = re(
        &ERR,
        r"(?i)\bERROR\b|HTTP Error \d{3}|Forbidden|Sign in to confirm|Unable to download",
    );
    lines
        .iter()
        .find(|l| pattern.is_match(l))
        .or_else(|| lines.last())
        .map_or_else(|| "nieznany błąd".to_owned(), |l| (*l).to_owned())
}

struct ExecFailure {
    message: String,
    stdout: String,
    stderr: String,
}

fn spawn_error(program: &str, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        format!("spawn {program} ENOENT")
    } else {
        format!("spawn {program} {error}")
    }
}

/// `execFile(program, args, { timeout })` (SIGTERM on timeout).
async fn exec(program: &str, args: &[String], timeout: Duration) -> Result<String, ExecFailure> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| ExecFailure {
            message: spawn_error(program, &e),
            stdout: String::new(),
            stderr: String::new(),
        })?;
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let read_out = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf).await;
        }
        String::from_utf8_lossy(&buf).into_owned()
    });
    let read_err = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf).await;
        }
        String::from_utf8_lossy(&buf).into_owned()
    });
    let status = match tokio::time::timeout(timeout, child.wait()).await {
        Ok(status) => status.ok(),
        Err(_) => {
            if let Some(pid) = child.id() {
                let _ = kill(Pid::from_raw(pid as i32), Signal::SIGTERM);
            }
            let _ = child.wait().await;
            None
        }
    };
    let stdout = read_out.await.unwrap_or_default();
    let stderr = read_err.await.unwrap_or_default();
    if status.is_some_and(|s| s.success()) {
        return Ok(stdout);
    }
    Err(ExecFailure {
        message: format!("Command failed: {program} {}\n{stderr}", args.join(" ")),
        stdout,
        stderr,
    })
}

impl Bot {
    fn ytdlp_args(&self, url: &str, mut args: Vec<String>) -> (Vec<String>, bool) {
        args.extend(["--js-runtimes".to_owned(), self.settings.js_runtime.clone()]);
        let cookies = is_youtube(url) && self.settings.cookies_path.exists();
        if cookies {
            args.extend([
                "--cookies".to_owned(),
                self.settings.cookies_path.to_string_lossy().into_owned(),
            ]);
        }
        args.push(url.to_owned());
        (args, cookies)
    }

    /// `estimateDownloadSize(url)`; `None` when unknown or the probe is disabled.
    async fn estimate_download_size(&self, url: &str) -> Option<f64> {
        if !self.settings.size_probe {
            return None;
        }
        let base = [
            "--no-playlist",
            "--no-warnings",
            "--dump-single-json",
            "--skip-download",
            "-f",
            BEST_FORMAT,
            "-S",
            format_sort(self.settings.video_codec),
        ]
        .map(str::to_owned)
        .to_vec();
        let (args, _) = self.ytdlp_args(url, base);
        let started = Instant::now();
        let timeout = Duration::from_millis(self.settings.probe_timeout_ms);
        match exec(&self.settings.ytdlp_path, &args, timeout).await {
            Ok(stdout) => match parse_json_output(&stdout) {
                Ok(info) => {
                    let size = size_from_requested(&info);
                    log::debug(
                        "Oszacowano rozmiar materiału",
                        Some(json!({
                            "url": url,
                            "estimatedSizeBytes": size,
                            "durationMs": started.elapsed().as_millis() as u64,
                        })),
                    );
                    size
                }
                Err(error) => {
                    self.warn_probe(url, started, &error);
                    None
                }
            },
            Err(failure) => {
                let details = if failure.stderr.is_empty() && failure.stdout.is_empty() {
                    failure.message
                } else {
                    pick_ytdlp_error(&failure.stderr, &failure.stdout)
                };
                self.warn_probe(url, started, &details);
                None
            }
        }
    }

    fn warn_probe(&self, url: &str, started: Instant, details: &str) {
        log::warn(
            "Nie udało się oszacować rozmiaru; używam priorytetu nieznanego",
            Some(json!({
                "url": url,
                "durationMs": started.elapsed().as_millis() as u64,
                "details": utf16_prefix(details, 500),
            })),
        );
    }

    /// `download(url, directory, onProgress, job)`
    async fn download(
        &self,
        url: &str,
        directory: &Path,
        on_progress: Arc<dyn Fn(Progress) + Send + Sync>,
        job: &Arc<Job>,
    ) -> Result<PathBuf> {
        let base = vec![
            "--no-playlist".to_owned(),
            "--newline".to_owned(),
            "--progress".to_owned(),
            "-f".to_owned(),
            BEST_FORMAT.to_owned(),
            "-S".to_owned(),
            format_sort(self.settings.video_codec).to_owned(),
            "--merge-output-format".to_owned(),
            "mp4".to_owned(),
            "-o".to_owned(),
            directory
                .join("video.%(ext)s")
                .to_string_lossy()
                .into_owned(),
        ];
        let (args, cookies) = self.ytdlp_args(url, base);
        log::info(
            "yt-dlp: rozpoczynam pobieranie",
            Some(json!({ "url": url, "directory": directory, "cookies": cookies })),
        );
        let started = Instant::now();
        if let Err(details) = self.run_download(&args, directory, on_progress, job).await {
            log::error(
                "yt-dlp zakończył się błędem",
                Some(json!({
                    "url": url,
                    "durationMs": started.elapsed().as_millis() as u64,
                    "details": details,
                })),
            );
            let details = if details.is_empty() {
                "nieznany błąd".to_owned()
            } else {
                details
            };
            bail!("yt-dlp: {}", utf16_prefix(&details, 500));
        }
        let path = find_downloaded_file(directory)
            .await
            .ok_or_else(|| anyhow!("yt-dlp nie zwrócił pobranego pliku."))?;
        let size = tokio::fs::metadata(&path).await.map_or(0, |m| m.len());
        log::info(
            "yt-dlp: pobieranie zakończone",
            Some(json!({
                "filePath": path,
                "sizeBytes": size,
                "durationMs": started.elapsed().as_millis() as u64,
            })),
        );
        Ok(path)
    }

    async fn run_download(
        &self,
        args: &[String],
        directory: &Path,
        on_progress: Arc<dyn Fn(Progress) + Send + Sync>,
        job: &Arc<Job>,
    ) -> Result<(), String> {
        let program = &self.settings.ytdlp_path;
        let mut child = Command::new(program)
            .args(args)
            .current_dir(directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| spawn_error(program, &e))?;
        job.set_process(child.id());
        let reader = |pipe: Option<Box<dyn tokio::io::AsyncRead + Unpin + Send>>| {
            let on_progress = on_progress.clone();
            tokio::spawn(async move {
                let mut collected = String::new();
                let Some(pipe) = pipe else { return collected };
                let mut lines = BufReader::new(pipe);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match lines.read_until(b'\n', &mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let line = String::from_utf8_lossy(&buf);
                            collected.push_str(&line);
                            if let Some(progress) =
                                parse_progress(line.trim_end_matches(['\r', '\n']))
                            {
                                on_progress(progress);
                            }
                        }
                    }
                }
                collected
            })
        };
        let stdout = reader(
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        );
        let stderr = reader(
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn tokio::io::AsyncRead + Unpin + Send>),
        );
        let timeout = Duration::from_secs_f64(self.settings.download_timeout_secs.max(0.0));
        let status = tokio::select! {
            status = child.wait() => status,
            () = tokio::time::sleep(timeout) => {
                let _ = child.start_kill();
                child.wait().await
            }
        };
        job.set_process(None);
        let stdout = stdout.await.unwrap_or_default();
        let stderr = stderr.await.unwrap_or_default();
        let status = status.map_err(|e| e.to_string())?;
        if status.success() {
            return Ok(());
        }
        use std::os::unix::process::ExitStatusExt;
        let prefix = if status.signal() == Some(Signal::SIGKILL as i32) {
            "Przekroczono limit czasu"
        } else {
            "yt-dlp"
        };
        Err(format!("{prefix}: {}", pick_ytdlp_error(&stderr, &stdout)))
    }

    /// `resolveDirectUrl(url)`: a single combined stream Telegram can fetch itself.
    async fn resolve_direct_url(&self, url: &str) -> Result<String> {
        let base = [
            "--no-playlist",
            "--no-warnings",
            "--get-url",
            "-f",
            direct_format(self.settings.video_codec),
            "-S",
            format_sort(self.settings.video_codec),
        ]
        .map(str::to_owned)
        .to_vec();
        let (args, _) = self.ytdlp_args(url, base);
        log::info(
            "Próbuję uzyskać bezpośredni URL dla Telegrama",
            Some(json!({ "url": url })),
        );
        let stdout = exec(&self.settings.ytdlp_path, &args, Duration::from_secs(120))
            .await
            .map_err(|f| anyhow!(f.message))?;
        let direct = stdout
            .lines()
            .map(str::trim)
            .rfind(|l| {
                l.to_lowercase().starts_with("http://") || l.to_lowercase().starts_with("https://")
            })
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("yt-dlp nie zwrócił bezpośredniego URL."))?;
        log::debug(
            "Uzyskano bezpośredni URL",
            Some(json!({ "length": utf16_len(&direct) })),
        );
        Ok(direct)
    }
}

/// Codecs of the first video and audio stream (`ffprobe`).
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

/// ffmpeg arguments that turn `input` into `output` playable with the target codec.
/// Streams that already match are copied; everything gets `+faststart` for streaming.
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
    let reencode = !video_ok;
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
        // CRF 18 with x264 is visually transparent for almost all sources.
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
    (args, reencode)
}

impl Bot {
    /// Makes the downloaded file play inline in Discord/Telegram: re-encodes HEVC, VP9,
    /// 10-bit/HDR (or anything not matching `YTDLP_VIDEO_CODEC`) and remuxes for fast start.
    async fn ensure_compatible(&self, path: &Path, job: &Arc<Job>) -> Result<PathBuf> {
        let codec = self.settings.video_codec;
        if !VIDEO_EXTENSIONS.contains(&extname(path).as_str()) {
            return Ok(path.to_owned());
        }
        let probe_args = [
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
        .collect::<Vec<_>>();
        let info = match exec(
            &self.settings.ffprobe_path,
            &probe_args,
            Duration::from_secs(60),
        )
        .await
        {
            Ok(output) => parse_probe(&output),
            Err(failure) => {
                log::warn(
                    "ffprobe nie powiódł się; wysyłam plik bez konwersji",
                    Some(json!(utf16_prefix(&failure.message, 300))),
                );
                return Ok(path.to_owned());
            }
        };
        if info.video.is_none() {
            return Ok(path.to_owned());
        }
        let output = path.with_file_name("delivery.mp4");
        let (args, reencode) = transcode_args(&info, codec, self.settings.video_crf, path, &output);
        log::info(
            "Przygotowuję plik do wysyłki",
            Some(json!({
                "jobId": job.id,
                "videoCodec": info.video,
                "pixFmt": info.pix_fmt,
                "audioCodec": info.audio,
                "target": format!("{codec:?}"),
                "reencode": reencode,
            })),
        );
        if reencode {
            let label = if codec == VideoCodec::Av1 {
                "AV1"
            } else {
                "H.264"
            };
            job.set_status_text(&format!("Konwersja do {label}…"));
            job.status
                .set(format!(
                    "🎞️ Konwertuję wideo do {label} (źródło: {}), żeby działało w Discordzie i Telegramie…",
                    info.video.as_deref().unwrap_or("?")
                ))
                .await;
        }
        let started = Instant::now();
        let timeout = Duration::from_secs_f64(self.settings.download_timeout_secs.max(60.0));
        let mut child = Command::new(&self.settings.ffmpeg_path)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| anyhow!("ffmpeg: {}", spawn_error(&self.settings.ffmpeg_path, &e)))?;
        job.set_process(child.id());
        let mut stderr_pipe = child.stderr.take();
        let stderr = tokio::spawn(async move {
            let mut buf = Vec::new();
            if let Some(pipe) = stderr_pipe.as_mut() {
                let _ = pipe.read_to_end(&mut buf).await;
            }
            String::from_utf8_lossy(&buf).into_owned()
        });
        let status = tokio::select! {
            status = child.wait() => status,
            () = tokio::time::sleep(timeout) => {
                let _ = child.start_kill();
                child.wait().await
            }
        };
        job.set_process(None);
        let stderr = stderr.await.unwrap_or_default();
        if !status.is_ok_and(|s| s.success()) {
            let last = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("nieznany błąd");
            bail!("ffmpeg: {}", utf16_prefix(last.trim(), 300));
        }
        let _ = tokio::fs::remove_file(path).await;
        let size = tokio::fs::metadata(&output).await.map_or(0, |m| m.len());
        log::info(
            "Plik gotowy do wysyłki",
            Some(json!({
                "jobId": job.id,
                "reencode": reencode,
                "sizeBytes": size,
                "durationMs": started.elapsed().as_millis() as u64,
            })),
        );
        Ok(output)
    }
}

fn parse_json_output(output: &str) -> Result<Value, String> {
    let text = output.trim();
    if text.is_empty() {
        return Err("yt-dlp nie zwrócił metadanych.".to_owned());
    }
    if let Ok(value) = serde_json::from_str(text) {
        return Ok(value);
    }
    let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) else {
        return Err("yt-dlp zwrócił niepoprawne metadane.".to_owned());
    };
    if end <= start {
        return Err("yt-dlp zwrócił niepoprawne metadane.".to_owned());
    }
    serde_json::from_str(&text[start..=end]).map_err(|e| e.to_string())
}

/// `formatSizeFromInfo(info)`
fn size_from_info(info: &Value) -> Option<f64> {
    let positive = |v: &Value| v.as_f64().filter(|n| n.is_finite() && *n > 0.0);
    positive(&info["filesize"]).or_else(|| positive(&info["filesize_approx"]))
}

fn size_from_requested(info: &Value) -> Option<f64> {
    if let Some(formats) = info["requested_formats"]
        .as_array()
        .filter(|f| !f.is_empty())
    {
        let sizes: Option<Vec<f64>> = formats.iter().map(size_from_info).collect();
        if let Some(total) = sizes.map(|s| s.iter().sum::<f64>()).filter(|t| *t > 0.0) {
            return Some(total);
        }
    }
    size_from_info(info)
}

async fn find_downloaded_file(directory: &Path) -> Option<PathBuf> {
    let mut entries = tokio::fs::read_dir(directory).await.ok()?;
    let mut files = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(kind) = entry.file_type().await else {
            continue;
        };
        if !kind.is_file() || name.ends_with(".part") || name.ends_with(".ytdl") {
            continue;
        }
        if let Ok(meta) = entry.metadata().await
            && meta.len() > 0
        {
            files.push((entry.path(), meta.len()));
        }
    }
    files.sort_by_key(|f| std::cmp::Reverse(f.1));
    files.into_iter().next().map(|f| f.0)
}

/// `fs.mkdtemp(path.join(os.tmpdir(), 'operator-ytdlp-'))`
async fn make_temp_dir() -> std::io::Result<PathBuf> {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    loop {
        let suffix: String = (0..6)
            .map(|_| CHARS[rand::random_range(0..CHARS.len())] as char)
            .collect();
        let path = std::env::temp_dir().join(format!("operator-ytdlp-{suffix}"));
        match tokio::fs::create_dir(&path).await {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Website publishing (files above the Telegram upload limit)
// ---------------------------------------------------------------------------------------------

fn player_html(
    id: &str,
    display_name: &str,
    video_name: &str,
    extension: &str,
    delete_token: &str,
) -> String {
    let video_url = format!("./{video_name}");
    let name = escape_html(display_name);
    let id_json = serde_json::to_string(id).unwrap_or_default();
    let token_json = serde_json::to_string(delete_token).unwrap_or_default();
    format!(
        r#"<!doctype html>
<html lang="pl">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{name} — Bladephoria</title>
  <style>
    :root {{ color-scheme: dark; }}
    body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; background: #101218; color: #f4f5f7; font: 16px system-ui, sans-serif; }}
    main {{ width: min(960px, calc(100% - 32px)); }}
    h1 {{ font-size: clamp(1.1rem, 2vw, 1.6rem); overflow-wrap: anywhere; }}
    video {{ width: 100%; max-height: 75vh; background: #000; border-radius: 10px; }}
    .actions {{ display: flex; gap: 12px; flex-wrap: wrap; margin-top: 16px; }}
    a, button {{ border: 0; border-radius: 7px; padding: 10px 14px; color: #fff; background: #5865f2; text-decoration: none; cursor: pointer; font: inherit; }}
    button {{ background: #b42318; }}
    #message {{ min-height: 1.4em; color: #ffb4ab; }}
  </style>
</head>
<body>
  <main>
    <h1>{name}</h1>
    <video controls preload="metadata">
      <source src="{video_url}" type="{mime}">
      Twoja przeglądarka nie obsługuje odtwarzania tego filmu.
    </video>
    <div class="actions">
      <a href="{video_url}" download="{video_attr}">Pobierz plik</a>
      <button id="delete" type="button">Usuń plik</button>
    </div>
    <p id="message"></p>
  </main>
  <script>
    const mediaId = {id_json};
    const deleteToken = {token_json};
    const button = document.getElementById('delete');
    const message = document.getElementById('message');
    button.addEventListener('click', async () => {{
      if (!window.confirm('Usunąć ten plik bezpowrotnie?')) return;
      button.disabled = true;
      try {{
        const response = await fetch('/auth/media-delete', {{
          method: 'POST',
          headers: {{ 'Content-Type': 'application/json' }},
          body: JSON.stringify({{ id: mediaId, token: deleteToken }}),
        }});
        if (!response.ok) throw new Error('Nie udało się usunąć pliku.');
        document.querySelector('main').innerHTML = '<h1>Plik został usunięty.</h1>';
      }} catch (error) {{
        message.textContent = error.message;
        button.disabled = false;
      }}
    }});
  </script>
</body>
</html>"#,
        mime = media_mime_type(extension),
        video_attr = escape_html(video_name),
    )
}

fn write_with_mode(path: &Path, contents: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(path)?;
    file.write_all(contents)
}

/// `publishToWebsite(filePath)` → public player page URL.
async fn publish_to_website(bot: &Bot, file_path: &Path) -> Result<String> {
    let media_root = bot.settings.website_root.join("media");
    let id = hex(&rand::random::<[u8; 16]>());
    let staging = media_root.join(format!("{id}.tmp"));
    let target = media_root.join(&id);
    let filtered: String = extname(file_path)
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '.')
        .collect();
    let extension = if filtered.is_empty() {
        ".mp4".to_owned()
    } else {
        filtered
    };
    let video_name = format!("video{extension}");
    let display_name = file_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let delete_token = hex(&rand::random::<[u8; 32]>());
    let page_url = format!("{}/media/{id}/", bot.settings.public_base_url);

    let result: Result<()> = async {
        tokio::fs::create_dir_all(&staging).await?;
        tokio::fs::copy(file_path, staging.join(&video_name)).await?;
        let html = player_html(&id, &display_name, &video_name, &extension, &delete_token);
        let (staging_c, token_line) = (staging.clone(), format!("{delete_token}\n"));
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            write_with_mode(
                &staging_c.join(".delete-token"),
                token_line.as_bytes(),
                0o600,
            )?;
            write_with_mode(&staging_c.join("index.html"), html.as_bytes(), 0o640)
        })
        .await??;
        tokio::fs::rename(&staging, &target).await?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(error);
    }
    let size = tokio::fs::metadata(file_path).await.map_or(0, |m| m.len());
    log::info(
        "Opublikowano plik na stronie",
        Some(json!({ "id": id, "sizeBytes": size, "pageUrl": page_url })),
    );
    Ok(page_url)
}

// ---------------------------------------------------------------------------------------------
// Replies, permissions, help
// ---------------------------------------------------------------------------------------------

fn command_argument(text: &str, command: &str) -> String {
    let pattern = format!(r"(?i)^/{command}(?:@[^\s]+)?\s*");
    Regex::new(&pattern)
        .map(|r| r.replace(text, "").trim().to_owned())
        .unwrap_or_default()
}

/// `replyEphemeral(ctx, text)`
async fn reply_ephemeral(bot: &Bot, ctx: &Ctx, text: &str) -> Result<Value, ApiError> {
    if !ctx.is_group() {
        return bot.reply(ctx, text, Value::Null).await;
    }
    let mut extra = json!({ "ephemeral_message_parameters": {} });
    if let Some(user) = ctx.sender_id() {
        extra["ephemeral_message_parameters"]["receiver_user_id"] = json!(user);
    }
    if let Some(id) = ctx.message["ephemeral_message_id"].as_i64() {
        extra["reply_parameters"] = json!({ "ephemeral_message_id": id });
    }
    bot.send_message(ctx.chat_id(), text, extra).await
}

/// `replyGlobal(ctx, text, extra)`
async fn reply_global(bot: &Bot, ctx: &Ctx, text: &str, extra: Value) -> Result<Value, ApiError> {
    if ctx.is_group() {
        return bot.send_message(ctx.chat_id(), text, extra).await;
    }
    bot.reply(ctx, text, extra).await
}

fn is_admin(bot: &Bot, ctx: &Ctx) -> bool {
    let user_id = ctx.sender_id().map(|id| id.to_string()).unwrap_or_default();
    let username = ctx
        .sender_username()
        .unwrap_or_default()
        .trim_start_matches('@')
        .to_lowercase();
    let allowed = bot.settings.admin_ids.contains(&user_id)
        || bot.settings.admin_usernames.contains(&username);
    log::info(
        "Sprawdzono uprawnienia administratora",
        Some(json!({
            "userId": user_id,
            "username": if username.is_empty() { Value::Null } else { json!(username) },
            "allowed": allowed,
        })),
    );
    allowed
}

fn user_help() -> String {
    [
        "Użytkownika Komendy",
        "/ytdlp <URL> — dodaj materiał do kolejki",
        "/ytdlp queue — pokaż kolejkę",
        "/ytdlp status — pokaż aktywne zadanie i postęp",
    ]
    .join("\n")
}

fn admin_help() -> String {
    [
        "/ytdlp_admin cancel <pozycja> — zatrzymaj zadanie (1 = aktywne)",
        "/ytdlp_admin pause — pauza aktywnego i kolejnych zadań",
        "/ytdlp_admin resume — wznow kolejkę",
        "/ytdlp_admin clear — zatrzymaj wszystko",
    ]
    .join("\n")
}

fn command_help(bot: &Bot, ctx: &Ctx) -> String {
    if is_admin(bot, ctx) {
        [
            user_help(),
            String::new(),
            "Administratorskie Komendy".to_owned(),
            admin_help(),
        ]
        .join("\n")
    } else {
        user_help()
    }
}

// ---------------------------------------------------------------------------------------------
// Queue overview
// ---------------------------------------------------------------------------------------------

fn queue_job_line(job: &Arc<Job>, queue: &Queue) -> String {
    let size = job
        .estimated_size
        .map_or("rozmiar ?".to_owned(), format_bytes);
    if job.state() == JobState::Running {
        return format!(
            "▶️ {} — {}\n   {} • {size}",
            job.id,
            job.status_text().unwrap_or_else(|| "w trakcie".to_owned()),
            short_url(&job.url)
        );
    }
    let position = queue
        .position(job)
        .map_or("?".to_owned(), |p| p.to_string());
    format!(
        "⏳ {} — pozycja {position}\n   {} • {size}",
        job.id,
        short_url(&job.url)
    )
}

fn format_queue_overview(queue: &Queue, status_only: bool) -> String {
    let jobs = queue.active_and_pending();
    let active = queue.active_jobs();
    let paused = queue.paused();
    if jobs.is_empty() {
        return format!(
            "📥 Kolejka /ytdlp jest pusta.{}",
            if paused {
                "\n⏸️ Stan: wstrzymana"
            } else {
                ""
            }
        );
    }
    let mut lines = vec![
        if status_only {
            "📊 Status /ytdlp"
        } else {
            "📥 Kolejka /ytdlp"
        }
        .to_owned(),
        format!(
            "Stan: {} • aktywne: {} • oczekujące: {}",
            if paused {
                "⏸️ wstrzymana"
            } else {
                "▶️ aktywna"
            },
            active.len(),
            queue.pending_count()
        ),
    ];
    let visible = if status_only { &active } else { &jobs };
    if visible.is_empty() {
        lines.push("Brak aktywnego zadania.".to_owned());
    } else {
        lines.extend(visible.iter().map(|job| queue_job_line(job, queue)));
    }
    lines.join("\n")
}

fn button(text: impl Into<String>, data: impl Into<String>) -> Value {
    json!({ "text": text.into(), "callback_data": data.into() })
}

/// `queueKeyboard(queue, ctx)` → `{ reply_markup: { inline_keyboard } }`
fn queue_keyboard(bot: &Bot, ctx: &Ctx) -> Value {
    let queue = &bot.queue;
    let mut rows = vec![vec![
        button("📥 Queue", "ytdlp:queue"),
        button("📊 Status", "ytdlp:status"),
    ]];
    if is_admin(bot, ctx) {
        let paused = queue.paused();
        rows.push(vec![
            button(
                if paused {
                    "▶️ Wznów"
                } else {
                    "⏸️ Pauza"
                },
                if paused {
                    "ytdlp:resume"
                } else {
                    "ytdlp:pause"
                },
            ),
            button("🗑️ Wyczyść", "ytdlp:clear"),
        ]);
        for job in queue.active_and_pending() {
            let position = queue
                .position(&job)
                .map_or("null".to_owned(), |p| p.to_string());
            rows.push(vec![button(
                format!("❌ Anuluj pozycję {position}"),
                format!("ytdlp:cancel:{position}"),
            )]);
        }
    }
    json!({ "reply_markup": { "inline_keyboard": rows } })
}

fn job_at_position(raw: &str, queue: &Queue) -> Option<Arc<Job>> {
    let position = crate::settings::js_number(Some(raw));
    if !position.is_finite() || position.fract() != 0.0 || position < 1.0 {
        return None;
    }
    queue
        .active_and_pending()
        .into_iter()
        .nth(position as usize - 1)
}

async fn handle_queue_command(bot: &Bot, ctx: &Ctx, status_only: bool) -> Result<()> {
    log::info(
        if status_only {
            "Odebrano /ytdlp status"
        } else {
            "Odebrano /ytdlp queue"
        },
        Some(
            json!({ "updateId": ctx.update_id, "chatId": ctx.chat_id(), "userId": ctx.sender_id() }),
        ),
    );
    let text = format_queue_overview(&bot.queue, status_only);
    reply_global(bot, ctx, &text, queue_keyboard(bot, ctx)).await?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Stale updates
// ---------------------------------------------------------------------------------------------

fn message_age_seconds(ctx: &Ctx, now_ms: u64) -> Option<f64> {
    let date = ctx.message_date().filter(|d| d.is_finite() && *d > 0.0)?;
    Some((now_ms as f64 / 1000.0 - date).max(0.0))
}

/// `ignoreStaleTelegramMessage(ctx, source)`
fn ignore_stale(bot: &Bot, ctx: &Ctx, source: &str) -> bool {
    let max = bot.settings.max_message_age_secs as f64;
    let Some(age) = message_age_seconds(ctx, crate::queue::now_ms()).filter(|a| *a > max) else {
        return false;
    };
    log::warn(
        "Pominięto opóźnioną aktualizację Telegrama",
        Some(json!({
            "source": source,
            "updateId": ctx.update_id,
            "messageDate": ctx.message["date"],
            "ageSeconds": age.round() as i64,
            "maxAgeSeconds": bot.settings.max_message_age_secs,
            "chatId": ctx.chat_id(),
            "userId": ctx.sender_id(),
        })),
    );
    true
}

// ---------------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------------

fn cancel_text(outcome: &CancelOutcome, position: &str) -> Option<String> {
    match outcome {
        CancelOutcome::CancelRequested => Some(format!("Zatrzymuję pozycję {position}.")),
        CancelOutcome::Canceled => Some(format!("Anulowano pozycję {position}.")),
        CancelOutcome::NotPending(_) => None,
    }
}

/// `/ytdlp_admin`
pub async fn handle_admin(bot: &Arc<Bot>, ctx: &Ctx) -> Result<()> {
    if ignore_stale(bot, ctx, "/ytdlp_admin") {
        return Ok(());
    }
    if !is_admin(bot, ctx) {
        reply_global(bot, ctx, "🚫 Brak uprawnień administratora.", Value::Null).await?;
        return Ok(());
    }
    let argument = command_argument(ctx.text().unwrap_or_default(), "ytdlp_admin");
    let mut parts = argument.split_whitespace();
    let action = parts.next().unwrap_or_default().to_lowercase();
    let rest: Vec<&str> = parts.collect();
    log::info(
        "Odebrano /ytdlp_admin",
        Some(json!({
            "action": if action.is_empty() { Value::Null } else { json!(action) },
            "args": rest,
            "chatId": ctx.chat_id(),
            "userId": ctx.sender_id(),
        })),
    );
    let queue = &bot.queue;
    let text = match action.as_str() {
        "" => {
            reply_ephemeral(bot, ctx, &admin_help()).await?;
            return Ok(());
        }
        "pause" => {
            queue.pause();
            format!(
                "⏸️ Pauza. Zatrzymano aktywne pobieranie i wstrzymano kolejkę.\nOczekujących: {}",
                queue.pending_count()
            )
        }
        "resume" => {
            queue.resume();
            format!(
                "▶️ Kolejka wznowiona. Aktywne: {}, oczekujące: {}.",
                queue.active_count(),
                queue.pending_count()
            )
        }
        "clear" => {
            let count = queue.clear();
            format!(
                "🗑️ Zatrzymano/anulowano {count} {} w kolejce.",
                if count == 1 { "zadanie" } else { "zadań" }
            )
        }
        "cancel" => match rest.first() {
            None => "Podaj pozycję, np. `/ytdlp_admin cancel 1`".to_owned(),
            Some(requested) => match job_at_position(requested, queue) {
                None => format!("⚠️ Nie znaleziono pozycji {requested}. Użyj /ytdlp queue."),
                Some(job) => {
                    let outcome = queue.cancel(&job);
                    match (&outcome, cancel_text(&outcome, requested)) {
                        (_, Some(text)) => format!("❌ {text}"),
                        (CancelOutcome::NotPending(state), None) if *state != JobState::Queued => {
                            format!("⚠️ Pozycja {requested} ma już stan: {}.", state.as_str())
                        }
                        _ => format!("⚠️ Nie udało się zatrzymać pozycji {requested}."),
                    }
                }
            },
        },
        _ => admin_help(),
    };
    reply_global(bot, ctx, &text, Value::Null).await?;
    Ok(())
}

/// Inline keyboard callbacks (`ytdlp:*`).
pub async fn handle_callback(bot: &Arc<Bot>, ctx: &Ctx) -> Result<()> {
    static DATA: OnceLock<Regex> = OnceLock::new();
    let data = ctx.callback["data"].as_str().unwrap_or_default();
    let Some(captures) = re(
        &DATA,
        r"^ytdlp:(queue|status|pause|resume|clear|cancel)(?::(.+))?$",
    )
    .captures(data) else {
        return Ok(());
    };
    let action = captures[1].to_owned();
    let requested = captures
        .get(2)
        .map_or("undefined".to_owned(), |m| m.as_str().to_owned());
    let callback_id = ctx.callback["id"].clone();
    let answer = |text: Option<String>, alert: bool| {
        let bot = bot.clone();
        let callback_id = callback_id.clone();
        async move {
            let mut payload = json!({ "callback_query_id": callback_id });
            if let Some(text) = text {
                payload["text"] = json!(text);
            }
            if alert {
                payload["show_alert"] = json!(true);
            }
            let _ = bot.api.call("answerCallbackQuery", payload).await;
        }
    };
    let edit = |status_only: bool| {
        let bot = bot.clone();
        let ctx = ctx.clone();
        async move {
            let text = format_queue_overview(&bot.queue, status_only);
            let extra = queue_keyboard(&bot, &ctx);
            let message_id = ctx.callback["message"]["message_id"].clone();
            if !message_id.is_null() {
                let mut payload = json!({
                    "chat_id": ctx.chat_id(),
                    "message_id": message_id,
                    "text": text,
                });
                payload["reply_markup"] = extra["reply_markup"].clone();
                match bot.api.call("editMessageText", payload).await {
                    Ok(_) => return Ok(()),
                    Err(error)
                        if error
                            .message
                            .to_lowercase()
                            .contains("message is not modified") =>
                    {
                        return Ok(());
                    }
                    Err(error) => log::debug(
                        "Nie udało się odświeżyć wiadomości kolejki",
                        Some(json!(error.message)),
                    ),
                }
            }
            bot.reply(&ctx, &text, extra).await.map(|_| ())
        }
    };

    if action == "queue" || action == "status" {
        answer(None, false).await;
        edit(action == "status").await?;
        return Ok(());
    }
    if !is_admin(bot, ctx) {
        answer(Some("Brak uprawnień administratora.".to_owned()), true).await;
        return Ok(());
    }
    let queue = &bot.queue;
    match action.as_str() {
        "pause" => {
            queue.pause();
            answer(
                Some("Pauza aktywnego pobierania i kolejki.".to_owned()),
                false,
            )
            .await;
        }
        "resume" => {
            queue.resume();
            answer(Some("Kolejka wznowiona.".to_owned()), false).await;
        }
        "clear" => {
            let count = queue.clear();
            answer(Some(format!("Zatrzymano/anulowano: {count}.")), false).await;
        }
        _ => {
            let outcome = job_at_position(&requested, queue).map(|job| queue.cancel(&job));
            match outcome.as_ref().and_then(|o| cancel_text(o, &requested)) {
                Some(text) => answer(Some(text), false).await,
                None => answer(Some(format!("Nie znaleziono pozycji {requested}.")), true).await,
            }
        }
    }
    edit(false).await?;
    Ok(())
}

/// `/ytdlp`
pub async fn handle_ytdlp(bot: &Arc<Bot>, ctx: &Ctx) -> Result<()> {
    if ignore_stale(bot, ctx, "/ytdlp") {
        return Ok(());
    }
    let requested_at = Instant::now();
    log::info(
        "Odebrano /ytdlp",
        Some(json!({
            "updateId": ctx.update_id,
            "chatId": ctx.chat_id(),
            "userId": ctx.sender_id(),
            "raw": ctx.text(),
        })),
    );
    let argument = command_argument(ctx.text().unwrap_or_default(), "ytdlp");
    if argument.is_empty() {
        reply_ephemeral(bot, ctx, &command_help(bot, ctx)).await?;
        return Ok(());
    }
    let subcommand = argument
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    match subcommand.as_str() {
        "queue" => return handle_queue_command(bot, ctx, false).await,
        "status" => return handle_queue_command(bot, ctx, true).await,
        "cancel" | "pause" | "resume" | "clear" => {
            reply_global(
                bot,
                ctx,
                "Ta opcja jest dostępna tylko przez /ytdlp_admin.",
                Value::Null,
            )
            .await?;
            return Ok(());
        }
        _ => {}
    }
    match normalize_url(&argument) {
        Ok(url) => enqueue(bot, ctx, url, requested_at).await,
        Err(message) => {
            log::warn("Niepoprawny URL w /ytdlp", Some(json!(message)));
            reply_global(bot, ctx, &message, Value::Null).await?;
            Ok(())
        }
    }
}

/// Plain text messages with a supported short-video link.
pub async fn handle_detected_media(bot: &Arc<Bot>, ctx: &Ctx) -> Result<()> {
    let Some(url) = extract_auto_media_url(ctx.text().unwrap_or_default()) else {
        return Ok(());
    };
    if ignore_stale(bot, ctx, "auto-media-link") {
        return Ok(());
    }
    log::info(
        "Wykryto link multimedialny bez komendy",
        Some(
            json!({ "updateId": ctx.update_id, "chatId": ctx.chat_id(), "userId": ctx.sender_id() }),
        ),
    );
    enqueue(bot, ctx, url, Instant::now()).await
}

fn estimate_line(estimated: Option<f64>) -> String {
    estimated.map_or(String::new(), |size| {
        format!("\n📦 Szacowany rozmiar: {}", format_bytes(size))
    })
}

/// `enqueueYtdlp(ctx, url)`
async fn enqueue(bot: &Arc<Bot>, ctx: &Ctx, url: String, requested_at: Instant) -> Result<()> {
    let queue = &bot.queue;
    let first = if bot.settings.size_probe {
        "🔎 Sprawdzam rozmiar materiału…"
    } else {
        "⏳ Przyjmuję zadanie do kolejki…"
    };
    let status = reply_global(bot, ctx, first, Value::Null).await?;
    let updater = StatusUpdater::new(
        bot.api.clone(),
        ctx.chat_id(),
        status["message_id"].as_i64(),
    );
    let estimated = bot.estimate_download_size(&url).await;
    if bot.settings.size_probe {
        updater
            .set(match estimated {
                Some(size) => format!(
                    "📦 Szacowany rozmiar: {}\n⏳ Dodaję do kolejki…",
                    format_bytes(size)
                ),
                None => "⚠️ Nie udało się ustalić rozmiaru.\n⏳ Dodaję do kolejki…".to_owned(),
            })
            .await;
    }
    let job = match queue.add(NewJob {
        ctx: ctx.clone(),
        url: url.clone(),
        estimated_size: estimated,
        status: updater.clone(),
        status_message_id: status["message_id"].as_i64(),
    }) {
        Ok(job) => job,
        Err(()) => {
            log::warn(
                "Nie udało się dodać /ytdlp do kolejki",
                Some(json!({ "error": "QUEUE_FULL", "chatId": ctx.chat_id() })),
            );
            updater
                .set(format!(
                    "⚠️ Twoja kolejka jest pełna (maksymalnie {} oczekujących zadań). Spróbuj ponownie później.",
                    queue.max_pending
                ))
                .await;
            return Ok(());
        }
    };
    let position = queue.position(&job);
    let state = job.state();
    if state == JobState::Queued {
        updater
            .set(queue_status_text(
                position,
                queue.pending_count(),
                estimated,
            ))
            .await;
    } else {
        updater
            .set(format!("⏳ Pobieram materiał…{}", estimate_line(estimated)))
            .await;
    }
    log::info(
        "Dodano /ytdlp do kolejki",
        Some(json!({
            "jobId": job.id,
            "queue": queue.name,
            "state": state.as_str(),
            "position": position,
            "pendingCount": queue.pending_count(),
            "queueSize": queue.active_count() + queue.pending_count(),
            "chatId": ctx.chat_id(),
            "userId": ctx.sender_id(),
            "estimatedSizeBytes": estimated,
            "priority": estimated,
            "enqueueDurationMs": requested_at.elapsed().as_millis() as u64,
        })),
    );
    Ok(())
}

/// Queue runner: `processYtdlp(ctx, url, status, updateStatus, job)`.
pub fn runner(bot: Weak<Bot>) -> Runner {
    Arc::new(move |job: Arc<Job>| {
        let bot = bot.clone();
        Box::pin(async move {
            if let Some(bot) = bot.upgrade() {
                process_job(&bot, &job).await;
            }
        })
    })
}

async fn process_job(bot: &Arc<Bot>, job: &Arc<Job>) {
    let started = Instant::now();
    let queued_ms = job
        .started_at_ms()
        .map(|s| s.saturating_sub(job.created_at_ms));
    log::info(
        "Rozpoczynam zadanie /ytdlp",
        Some(json!({
            "jobId": job.id,
            "chatId": job.ctx.chat_id(),
            "userId": job.ctx.sender_id(),
            "queuedDurationMs": queued_ms,
            "estimatedSizeBytes": job.estimated_size,
            "priority": job.priority.is_finite().then_some(job.priority),
        })),
    );
    let mut temp_dir: Option<PathBuf> = None;
    let result = deliver(bot, job, &mut temp_dir).await;
    if let Err(error) = result {
        let message = error_text(&error);
        log::error(
            "Obsługa /ytdlp zakończona błędem",
            Some(json!({
                "jobId": job.id,
                "durationMs": started.elapsed().as_millis() as u64,
                "queuedDurationMs": queued_ms,
                "error": log::error_value(&error),
            })),
        );
        job.status
            .set(format!(
                "Nie udało się pobrać materiału: {}",
                utf16_prefix(&message, 500)
            ))
            .await;
    }
    if let Some(dir) = temp_dir {
        let _ = tokio::fs::remove_dir_all(dir).await;
    }
}

async fn delete_status(bot: &Bot, job: &Job) {
    if let Some(message_id) = job.status_message_id() {
        let _ = bot
            .api
            .call(
                "deleteMessage",
                json!({ "chat_id": job.ctx.chat_id(), "message_id": message_id }),
            )
            .await;
    }
}

async fn send_by_url(bot: &Bot, job: &Job, method: &str, field: &str, url: &str) -> Result<()> {
    let mut payload = json!({ "chat_id": job.ctx.chat_id() });
    payload[field] = json!(url);
    if method == "sendVideo" {
        payload["supports_streaming"] = json!(true);
    }
    bot.api.call(method, payload).await?;
    Ok(())
}

async fn upload(
    bot: &Bot,
    job: &Job,
    method: &str,
    path: &Path,
    fields: Vec<(&'static str, String)>,
) -> Result<()> {
    let chat_id = job
        .ctx
        .chat_id()
        .ok_or_else(|| anyhow!("Brak chat_id dla uploadu."))?;
    bot.api.upload_file(chat_id, method, path, fields).await?;
    Ok(())
}

const DOCUMENT_CAPTION: &str = "Materiał w załączniku (Telegram nie przyjął go jako film).";

fn warn_error(message: &str, error: &anyhow::Error) {
    log::warn(message, Some(json!(error_text(error))));
}

async fn deliver(bot: &Arc<Bot>, job: &Arc<Job>, temp_dir: &mut Option<PathBuf>) -> Result<()> {
    let started = Instant::now();
    let url = job.url.clone();
    job.set_status_text("Pobieranie materiału…");
    job.status
        .set(format!(
            "⏳ Pobieram materiał…{}",
            estimate_line(job.estimated_size)
        ))
        .await;
    let _ = bot.send_chat_action(&job.ctx, "upload_video").await;
    let dir = make_temp_dir().await?;
    *temp_dir = Some(dir.clone());
    log::debug(
        "Utworzono katalog tymczasowy",
        Some(json!({ "tempDir": dir, "url": url, "jobId": job.id })),
    );

    let interval = bot.settings.progress_interval_ms;
    let last_update: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let progress_job = job.clone();
    let on_progress: Arc<dyn Fn(Progress) + Send + Sync> = Arc::new(move |progress: Progress| {
        let mut last = last_update.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        let recent = last.is_some_and(|t| (now - t).as_secs_f64() * 1000.0 < interval);
        if recent && progress.percent < 100.0 {
            return;
        }
        *last = Some(now);
        let text = progress_text(&progress);
        progress_job.set_status_text(text.trim_start_matches('⏬').trim_start());
        progress_job.status.set_detached(text);
    });
    let file_path = bot.download(&url, &dir, on_progress, job).await?;
    let file_path = bot.ensure_compatible(&file_path, job).await?;
    let size = tokio::fs::metadata(&file_path).await?.len() as f64;
    if size > bot.settings.max_upload_bytes {
        log::warn(
            "Plik przekracza limit Telegrama; publikuję na stronie",
            Some(json!({
                "sizeBytes": size,
                "maxUploadBytes": bot.settings.max_upload_bytes,
                "jobId": job.id,
            })),
        );
        let page = publish_to_website(bot, &file_path).await?;
        job.set_status_text("Opublikowano na stronie");
        job.status
            .set(format!(
                "✅ Plik ma {} i przekracza limit Telegrama.\n🌐 Player: {page}\n🗑️ Na stronie jest przycisk usuwania pliku.",
                format_bytes(size)
            ))
            .await;
        return Ok(());
    }

    job.status
        .set(format!(
            "✅ Pobrano {}\n📤 Wysyłam plik…",
            format_bytes(size)
        ))
        .await;
    job.set_status_text("Wysyłka pliku…");
    let extension = extname(&file_path);
    log::info(
        "Rozpoczynam upload do Telegrama",
        Some(json!({
            "sizeBytes": size,
            "extension": extension,
            "chatId": job.ctx.chat_id(),
            "jobId": job.id,
        })),
    );
    let youtube = is_youtube(&url);
    if VIDEO_EXTENSIONS.contains(&extension.as_str()) {
        if bot.settings.direct_url_first && !youtube {
            let attempt: Result<()> = async {
                let direct = bot.resolve_direct_url(&url).await?;
                log::info(
                    "Wysyłam materiał przez bezpośredni URL (bez lokalnego uploadu)",
                    Some(json!({ "chatId": job.ctx.chat_id(), "jobId": job.id })),
                );
                send_by_url(bot, job, "sendVideo", "video", &direct).await
            }
            .await;
            match attempt {
                Ok(()) => {
                    delete_status(bot, job).await;
                    log::info(
                        "Wysyłka URL-em zakończona pomyślnie",
                        Some(json!({
                            "chatId": job.ctx.chat_id(),
                            "jobId": job.id,
                            "durationMs": started.elapsed().as_millis() as u64,
                        })),
                    );
                    return Ok(());
                }
                Err(error) if is_rate_limit_error(&error) => return Err(error),
                Err(error) => warn_error(
                    "Wysyłka URL-em nie powiodła się; próbuję lokalnego uploadu",
                    &error,
                ),
            }
        }
        let video = upload(
            bot,
            job,
            "sendVideo",
            &file_path,
            vec![("supports_streaming", "true".to_owned())],
        )
        .await;
        if let Err(video_error) = video {
            if is_rate_limit_error(&video_error) {
                return Err(video_error);
            }
            let caption = vec![("caption", DOCUMENT_CAPTION.to_owned())];
            if youtube {
                warn_error(
                    "sendVideo YouTube nie powiódł się; ponawiam jako dokument lokalny",
                    &video_error,
                );
                upload(bot, job, "sendDocument", &file_path, caption).await?;
            } else {
                warn_error(
                    "sendVideo nie powiódł się; próbuję wysyłki URL-em",
                    &video_error,
                );
                let direct: Result<()> = async {
                    let direct = bot.resolve_direct_url(&url).await?;
                    send_by_url(bot, job, "sendVideo", "video", &direct).await
                }
                .await;
                if let Err(direct_error) = direct {
                    if is_rate_limit_error(&direct_error) {
                        return Err(direct_error);
                    }
                    warn_error(
                        "Wysyłka URL-em nie powiodła się; ponawiam jako dokument lokalny",
                        &direct_error,
                    );
                    upload(bot, job, "sendDocument", &file_path, caption).await?;
                }
            }
        }
    } else if let Err(document_error) =
        upload(bot, job, "sendDocument", &file_path, Vec::new()).await
    {
        if is_rate_limit_error(&document_error) {
            return Err(document_error);
        }
        warn_error(
            "sendDocument nie powiódł się; próbuję wysyłki URL-em",
            &document_error,
        );
        let direct = bot.resolve_direct_url(&url).await?;
        send_by_url(bot, job, "sendDocument", "document", &direct).await?;
    }
    delete_status(bot, job).await;
    log::info(
        "Upload zakończony pomyślnie",
        Some(json!({
            "chatId": job.ctx.chat_id(),
            "jobId": job.id,
            "durationMs": started.elapsed().as_millis() as u64,
        })),
    );
    Ok(())
}

/// `configureCommandMenu(bot)`
pub async fn configure_command_menu(bot: &Bot) {
    let user = json!([{ "command": "ytdlp", "description": "Pobierz wideo, kolejka lub status", "is_ephemeral": true }]);
    let mut admin = user.clone();
    if let Some(list) = admin.as_array_mut() {
        list.push(json!({ "command": "ytdlp_admin", "description": "Zarządzanie kolejką", "is_ephemeral": true }));
    }
    let mut calls = vec![bot.api.call("setMyCommands", json!({ "commands": user }))];
    for raw in &bot.settings.admin_ids {
        let numeric = raw.strip_prefix('-').unwrap_or(raw);
        if numeric.is_empty() || !numeric.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(chat_id) = raw.parse::<i64>() else {
            continue;
        };
        calls.push(bot.api.call(
            "setMyCommands",
            json!({ "commands": admin, "scope": { "type": "chat", "chat_id": chat_id } }),
        ));
    }
    let results = futures_util::future::join_all(calls).await;
    if let Some(error) = results.into_iter().find_map(Result::err) {
        log::warn(
            "Nie udało się skonfigurować menu komend Telegrama",
            Some(json!(error.message)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_formatting() {
        assert_eq!(format_bytes(512.0), "512 B");
        assert_eq!(format_bytes(1536.0), "1.50 KiB");
        assert_eq!(format_bytes(49_000_000.0), "46.7 MiB");
        assert_eq!(format_bytes(5.0 * 1024f64.powi(4)), "5120 GiB");
        assert_eq!(format_bytes(f64::NAN), "? B");
    }

    #[test]
    fn progress_parsing() {
        let p = parse_progress("[download]  42.5% of ~ 10.00MiB at  1.20MiB/s ETA 00:05").unwrap();
        assert_eq!(p.percent, 42.5);
        assert_eq!(p.total_bytes, Some(10.0 * 1024.0 * 1024.0));
        assert_eq!(p.speed.as_deref(), Some("1.20MiB/s"));
        assert_eq!(p.eta.as_deref(), Some("00:05"));
        assert_eq!(
            progress_text(&p),
            "⏬ Pobieram… 42.5% • 4.25 MiB / 10.0 MiB • 1.20MiB/s • ETA 00:05"
        );
        assert!(parse_progress("[info] nothing").is_none());
    }

    #[test]
    fn urls() {
        assert_eq!(
            normalize_url(" url: https://example.com ").unwrap(),
            "https://example.com/"
        );
        assert!(normalize_url("ftp://x").is_err());
        assert!(normalize_url("nope").is_err());
        assert!(is_youtube("https://www.youtube.com/watch?v=1"));
        assert!(is_youtube("https://youtu.be/x"));
        assert!(!is_youtube("https://notyoutube.com/x"));
        assert_eq!(
            extract_auto_media_url("look https://www.tiktok.com/@a/video/1)."),
            Some("https://www.tiktok.com/@a/video/1".to_owned())
        );
        assert!(extract_auto_media_url("https://youtube.com/shorts/abc").is_some());
        assert!(extract_auto_media_url("https://youtube.com/watch?v=abc").is_none());
        assert!(extract_auto_media_url("https://instagram.com/reel/x/").is_some());
        assert_eq!(short_url("https://example.com/a?b=1"), "example.com/a");
        assert_eq!(
            command_argument("/ytdlp@Bot  https://x ", "ytdlp"),
            "https://x"
        );
    }

    #[test]
    fn ytdlp_errors_and_json() {
        assert_eq!(
            pick_ytdlp_error("WARNING: x\nERROR: Video unavailable\n", "tail"),
            "ERROR: Video unavailable"
        );
        assert_eq!(pick_ytdlp_error("", "last line\n"), "last line");
        assert_eq!(pick_ytdlp_error("", ""), "nieznany błąd");
        let info = parse_json_output(
            "[info] x\n{\"requested_formats\":[{\"filesize\":10},{\"filesize_approx\":5}]}",
        )
        .unwrap();
        assert_eq!(size_from_requested(&info), Some(15.0));
        assert_eq!(
            size_from_requested(&json!({"filesize": 0, "filesize_approx": 7})),
            Some(7.0)
        );
    }

    #[test]
    fn transcode_decisions() {
        let p = Path::new("/t/video.mp4");
        let o = Path::new("/t/delivery.mp4");
        let hevc = parse_probe(
            r#"{"streams":[{"codec_type":"video","codec_name":"hevc","pix_fmt":"yuv420p10le"},{"codec_type":"audio","codec_name":"opus"}]}"#,
        );
        assert_eq!(hevc.video.as_deref(), Some("hevc"));
        let (args, reencode) = transcode_args(&hevc, VideoCodec::H264, None, p, o);
        assert!(reencode);
        let joined = args.join(" ");
        assert!(
            joined.contains("-c:v libx264 -preset slow -crf 18 -profile:v high -pix_fmt yuv420p")
        );
        assert!(joined.contains("-c:a aac -b:a 192k"));
        assert!(joined.ends_with("-movflags +faststart /t/delivery.mp4"));

        let h264 = parse_probe(
            r#"{"streams":[{"codec_type":"video","codec_name":"h264","pix_fmt":"yuv420p"},{"codec_type":"audio","codec_name":"aac"}]}"#,
        );
        let (args, reencode) = transcode_args(&h264, VideoCodec::H264, None, p, o);
        assert!(!reencode);
        assert!(args.join(" ").contains("-c:v copy -c:a copy"));

        let (args, reencode) = transcode_args(&hevc, VideoCodec::Av1, Some(28), p, o);
        assert!(reencode);
        assert!(args.join(" ").contains("-c:v libsvtav1 -preset 6 -crf 28"));
        assert!(!transcode_args(&hevc, VideoCodec::Source, None, p, o).1);
    }

    #[test]
    fn html_and_paths() {
        assert_eq!(
            escape_html("<a href=\"x\">'&'"),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;"
        );
        assert_eq!(extname(Path::new("/tmp/video.MP4")), ".mp4");
        assert_eq!(extname(Path::new("/tmp/.hidden")), "");
        let html = player_html("abc", "a<b>.mp4", "video.mp4", ".mp4", "tok");
        assert!(html.contains("<title>a&lt;b&gt;.mp4 — Bladephoria</title>"));
        assert!(html.contains("const mediaId = \"abc\";"));
        assert!(html.contains("type=\"video/mp4\""));
    }
}
