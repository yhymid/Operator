#[allow(clippy::wildcard_imports)]
use super::*;

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

pub(super) struct ExecFailure {
    pub(super) message: String,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

pub(super) fn spawn_error(program: &str, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        format!("spawn {program} ENOENT")
    } else {
        format!("spawn {program} {error}")
    }
}

/// `execFile(program, args, { timeout })` (SIGTERM on timeout).
pub(super) async fn exec(
    program: &str,
    args: &[String],
    timeout: Duration,
) -> Result<String, ExecFailure> {
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
            // A process ignoring SIGTERM must not hang the job forever.
            if tokio::time::timeout(Duration::from_secs(5), child.wait())
                .await
                .is_err()
            {
                let _ = child.start_kill();
                let _ = child.wait().await;
            }
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
    pub(super) fn ytdlp_args(&self, url: &str, mut args: Vec<String>) -> (Vec<String>, bool) {
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
    pub(super) async fn estimate_download_size(&self, url: &str) -> Option<f64> {
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

    pub(super) fn warn_probe(&self, url: &str, started: Instant, details: &str) {
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
    pub(super) async fn download(
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

    pub(super) async fn run_download(
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
            .process_group(0)
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
                if let Some(pid) = child.id() {
                    let _ = crate::queue::signal_group(pid, Signal::SIGKILL);
                }
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
    pub(super) async fn resolve_direct_url(&self, url: &str) -> Result<String> {
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

/// SVT-AV1 preset used by the Telegram bot (Discord reads `YTDLP_AV1_PRESET`).
pub(super) const AV1_PRESET: u8 = 6;

pub(super) fn encode(codec: VideoCodec, crf: Option<u32>) -> Encode {
    Encode {
        codec,
        crf,
        av1_preset: AV1_PRESET,
    }
}

impl Bot {
    /// Makes the downloaded file play inline in Discord/Telegram: re-encodes HEVC, VP9,
    /// 10-bit/HDR (or anything not matching `YTDLP_VIDEO_CODEC`) and remuxes for fast start.
    pub(super) async fn ensure_compatible(&self, path: &Path, job: &Arc<Job>) -> Result<PathBuf> {
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
        let (args, reencode) =
            transcode_args(&info, encode(codec, self.settings.video_crf), path, &output);
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
            .process_group(0)
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

pub(super) fn parse_json_output(output: &str) -> Result<Value, String> {
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
pub(super) fn size_from_info(info: &Value) -> Option<f64> {
    let positive = |v: &Value| v.as_f64().filter(|n| n.is_finite() && *n > 0.0);
    positive(&info["filesize"]).or_else(|| positive(&info["filesize_approx"]))
}

pub(super) fn size_from_requested(info: &Value) -> Option<f64> {
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

pub(super) async fn find_downloaded_file(directory: &Path) -> Option<PathBuf> {
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
pub(super) async fn make_temp_dir() -> std::io::Result<PathBuf> {
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
