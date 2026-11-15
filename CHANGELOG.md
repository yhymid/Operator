# Changelog

Releases are cut by hand when enough has changed. Each release on GitHub uses the matching
section below as its description.

## 2.0 - Rust Update

### Changed
- The bot is written in Rust now (the JavaScript version is gone): one small binary, lower
  memory use, faster start.
- Admins are recognised by numeric ID only (`ADMIN_IDS`); usernames can be taken over.
- Updates from different chats are handled in parallel, so one slow chat does not block the
  others.

### Added
- A limit of queued downloads per user (`YTDLP_QUEUE_MAX_PER_USER`, default 3).
- An optional list of chats the bot answers in (`ALLOWED_CHAT_IDS`).
- Website copies of large videos expire after `YTDLP_WEBSITE_TTL_HOURS` (default 24).
- Server scripts: release install with health check and rollback, daily backup, failure
  notifications, journal size limit.

### Fixed
- Links to local and private addresses are refused before yt-dlp runs.
- Cancel buttons cancel the job they were shown for, not whatever is at that queue position.
- Cancel also stops a paused download and its helper processes; a hung process is killed.
- Time spent in `/ytdlp_admin pause` no longer counts towards the download time limit.
- Cleanup of website pages only removes pages the bot created.
