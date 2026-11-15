//! `/cookies` (owner only): replaces the cookies file yt-dlp uses for YouTube, from an uploaded
//! `cookies.txt` or pasted text. The content is a secret: it is never echoed, logged or kept
//! in the reply, and the file is written with mode 0600.

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result};

use crate::discord::Ix;

/// A cookies.txt export is a few KB; anything far larger is not one.
const MAX_COOKIES_BYTES: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq)]
pub struct CookieSummary {
    pub total: usize,
    pub youtube: usize,
    /// Earliest expiry (unix seconds) of the YouTube/Google login cookies, if any expire.
    pub login_expires: Option<i64>,
}

/// Checks the Netscape cookie-file format (7 tab-separated fields per cookie line).
pub fn parse_cookies(text: &str) -> Result<CookieSummary, &'static str> {
    let mut summary = CookieSummary {
        total: 0,
        youtube: 0,
        login_expires: None,
    };
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 7 {
            return Err(
                "This is not a cookies.txt file (Netscape format, 7 tab-separated fields per line).",
            );
        }
        summary.total += 1;
        let domain = fields[0].trim_start_matches('.');
        if domain == "youtube.com" || domain.ends_with(".youtube.com") {
            summary.youtube += 1;
        }
        let login = matches!(
            fields[5],
            "LOGIN_INFO" | "SID" | "__Secure-1PSID" | "__Secure-3PSID" | "SAPISID"
        );
        if login
            && let Ok(expires) = fields[4].parse::<i64>()
            && expires > 0
        {
            summary.login_expires = Some(summary.login_expires.map_or(expires, |e| e.min(expires)));
        }
    }
    if summary.total == 0 {
        return Err("The file contains no cookies.");
    }
    if summary.youtube == 0 {
        return Err(
            "The file contains no youtube.com cookies. Export them while logged in on youtube.com.",
        );
    }
    Ok(summary)
}

/// Writes `content` atomically with mode 0600; the old file is kept as `<name>.prev`.
fn write_private(path: &Path, content: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name()
            .map_or("cookies.txt".into(), |n| n.to_string_lossy())
    ));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("create {}", tmp.display()))?;
        file.write_all(content)?;
        file.sync_all()?;
    }
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    if path.exists() {
        let prev = path.with_file_name(format!(
            "{}.prev",
            path.file_name()
                .map_or("cookies.txt".into(), |n| n.to_string_lossy())
        ));
        std::fs::copy(path, &prev)?;
        std::fs::set_permissions(&prev, std::fs::Permissions::from_mode(0o600))?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub async fn execute(ix: &Ix<'_>) -> Result<()> {
    if !ix.app.is_owner(ix.user_id()) {
        return ix.reply_ephemeral("This command is owner-only.").await;
    }
    let opts = ix.opts();
    let file = opts.attachment("file");
    let text = opts.string("text");
    let path = super::ytdlp::cookies_path();
    if file.is_none() && text.is_none() {
        let status = match std::fs::read_to_string(&path) {
            Ok(content) => match parse_cookies(&content) {
                Ok(s) => format!("Current cookies: {}", describe(&s)),
                Err(problem) => format!("The current cookies file is invalid: {problem}"),
            },
            Err(_) => {
                "No cookies file yet. Upload one with `file:` or paste it with `text:`.".to_owned()
            }
        };
        return ix.reply_ephemeral(status).await;
    }
    if file.is_some() && text.is_some() {
        return ix
            .reply_ephemeral("Use either `file` or `text`, not both.")
            .await;
    }
    ix.defer(true).await?;
    let content = match (file, text) {
        (Some(file), _) => {
            if file.size > MAX_COOKIES_BYTES as u64 {
                return ix
                    .edit_content("The file is too large for a cookies.txt.")
                    .await;
            }
            match crate::rest::download_attachment(&file.url, MAX_COOKIES_BYTES).await {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(error) => {
                    return ix
                        .edit_content(format!("Could not download the file: {error}"))
                        .await;
                }
            }
        }
        (None, Some(text)) => text,
        (None, None) => unreachable!("checked above"),
    };
    let summary = match parse_cookies(&content) {
        Ok(summary) => summary,
        Err(problem) => return ix.edit_content(problem).await,
    };
    let write_path = path.clone();
    let bytes = content.into_bytes();
    match tokio::task::spawn_blocking(move || write_private(&write_path, &bytes)).await? {
        Ok(()) => {
            tracing::info!(
                "Cookies replaced by {} ({} cookies, {} youtube.com)",
                ix.user_id(),
                summary.total,
                summary.youtube
            );
            ix.edit_content(format!(
                "Cookies saved: {}.\nThe next `/ytdlp` uses them. You can delete your copy of the file.",
                describe(&summary)
            ))
            .await
        }
        Err(error) => {
            ix.edit_content(format!("Could not save the cookies: {error:#}"))
                .await
        }
    }
}

fn describe(summary: &CookieSummary) -> String {
    let expiry = summary.login_expires.map_or(
        "login cookies do not expire or were not found".to_owned(),
        |t| format!("login expires <t:{t}:R>"),
    );
    format!(
        "{} cookies, {} for youtube.com, {expiry}",
        summary.total, summary.youtube
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# Netscape HTTP Cookie File\n\
        .youtube.com\tTRUE\t/\tTRUE\t1900000000\tLOGIN_INFO\tx\n\
        #HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t1800000000\t__Secure-3PSID\ty\n\
        .google.com\tTRUE\t/\tTRUE\t0\tNID\tz\n";

    #[test]
    fn parses_netscape_cookies() {
        assert_eq!(
            parse_cookies(SAMPLE),
            Ok(CookieSummary {
                total: 3,
                youtube: 2,
                login_expires: Some(1_800_000_000)
            })
        );
    }

    #[test]
    fn rejects_other_content() {
        assert!(parse_cookies("hello world").is_err());
        assert!(parse_cookies("# only comments\n").is_err());
        assert!(parse_cookies(".google.com\tTRUE\t/\tTRUE\t0\tNID\tz").is_err());
    }

    #[test]
    fn writes_private_file_and_keeps_previous() {
        let dir = std::env::temp_dir().join(format!("operator-cookies-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cookies.txt");
        write_private(&path, b"one").unwrap();
        write_private(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(std::fs::read(dir.join("cookies.txt.prev")).unwrap(), b"one");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
