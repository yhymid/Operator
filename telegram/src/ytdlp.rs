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
use crate::queue::{
    AddError, CancelOutcome, Job, JobState, NewJob, Queue, Runner, queue_status_text,
};
use crate::settings::VideoCodec;
use crate::status::StatusUpdater;

mod download;
mod format;
mod handlers;
mod jobs;
mod queue_view;
mod replies;
mod website;

pub use download::*;
pub use format::*;
pub use handlers::*;
pub use jobs::*;
use operator_common::media::{Encode, parse_probe, transcode_args};
use queue_view::*;
use replies::*;
pub use website::*;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_buttons_find_jobs_by_id_or_position() {
        let queue = Queue::new("t", 1, 5, 0);
        let add = |url: &str| {
            queue
                .add(NewJob {
                    ctx: crate::bot::Ctx::new(&json!({ "update_id": 1 })),
                    url: url.to_owned(),
                    estimated_size: None,
                    status: StatusUpdater::noop(),
                    status_message_id: None,
                })
                .expect("queued")
        };
        let first = add("a");
        let second = add("b");
        let by_id = job_at_position(&format!("id:{}", second.id), &queue).expect("by id");
        assert!(Arc::ptr_eq(&by_id, &second));
        let by_position = job_at_position("1", &queue).expect("by position");
        assert!(Arc::ptr_eq(&by_position, &first));
        assert!(job_at_position("id:t-999", &queue).is_none());
    }

    #[tokio::test]
    async fn expired_page_cleanup_only_touches_published_pages() {
        let root = std::env::temp_dir().join(format!("op-tg-cleanup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let page = root.join("0123456789abcdef0123456789abcdef");
        let staging = root.join("fedcba9876543210fedcba9876543210.tmp");
        let no_token = root.join("00000000000000000000000000000000");
        let foreign = root.join("UniwersumPanczkoweBuild1.4");
        for dir in [&page, &staging, &no_token, &foreign] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(page.join(".delete-token"), "x\n").unwrap();
        std::fs::write(page.join("video.mp4"), "v").unwrap();
        std::fs::write(foreign.join(".delete-token"), "x\n").unwrap();
        std::fs::write(root.join("index.html"), "site").unwrap();

        // Fresh pages survive a 1 h TTL.
        cleanup_expired_pages(&root, Duration::from_secs(3600)).await;
        assert!(page.exists() && staging.exists());

        cleanup_expired_pages(&root, Duration::ZERO).await;
        assert!(!page.exists(), "published page must expire");
        assert!(!staging.exists(), "leftover staging dir must expire");
        assert!(
            no_token.exists(),
            "hex dir without .delete-token is not ours"
        );
        assert!(foreign.exists(), "other website content must stay");
        assert!(root.join("index.html").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

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

    /// Plain messages trigger downloads only for the three supported hosts; look-alike hosts
    /// and non-http schemes must not get through.
    #[test]
    fn auto_links_and_url_validation_reject_lookalikes() {
        for text in [
            "https://tiktok.com.evil.example/x",
            "https://eviltiktok.com/@a/video/1",
            "https://youtube.com.evil.example/shorts/x",
            "https://instagram.com/p/abc/",
            "ftp://tiktok.com/x",
        ] {
            assert_eq!(extract_auto_media_url(text), None, "{text}");
        }
        assert_eq!(
            extract_auto_media_url("https://VM.TikTok.com/ZM123/"),
            Some("https://vm.tiktok.com/ZM123/".to_owned())
        );
        for raw in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "http://",
            "data:text/html,x",
        ] {
            assert!(normalize_url(raw).is_err(), "{raw}");
        }
        let long = format!("https://example.com/{}", "a".repeat(2100));
        assert!(normalize_url(&long).is_err());
        assert!(!is_youtube("https://youtube.com.evil.example/watch"));
    }

    #[test]
    fn player_page_escapes_names() {
        let html = player_html(
            "id",
            "</title><script>alert(1)</script>",
            "video\"onerror=\"x.mp4",
            ".mp4",
            "tok",
        );
        assert!(!html.contains("<script>alert(1)"));
        assert!(html.contains("&lt;/title&gt;&lt;script&gt;"));
        assert!(html.contains("download=\"video&quot;onerror=&quot;x.mp4\""));
    }
}
