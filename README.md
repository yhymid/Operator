# Operator - Telegram

A Telegram bot written in Rust: downloads videos with `yt-dlp`, with a queue, a progress bar
and output in a codec that plays in Telegram and Discord. One small binary, no runtime to
install. This branch is the **Telegram** bot; the Discord bot lives on the
[`discord`](https://github.com/yhymid/Operator/tree/discord) branch.

## Commands

- `/ytdlp <URL>` - adds a video to the shared queue (smaller files go first)
- `/ytdlp queue`, `/ytdlp status` - the queue, with buttons
- `/ytdlp_admin pause | resume | clear | cancel <position>` - only for `ADMIN_IDS`
- YouTube Shorts, Instagram Reels and TikTok links sent as a plain message are downloaded
  automatically

Limits: one user can have `YTDLP_QUEUE_MAX_PER_USER` jobs queued (default 3); links to local
and private addresses are refused; `ALLOWED_CHAT_IDS` (empty = all) restricts the chats the
bot answers in. Admins are recognised by numeric ID only.

## Build and run

Requirements: Rust stable (1.88 or newer); `yt-dlp`; `ffmpeg`/`ffprobe` with `libx264`
(and `libsvtav1` for AV1 mode); a JavaScript runtime for yt-dlp (`YTDLP_JS_RUNTIME`, default
`node`). Optional: a Local Bot API Server (`TELEGRAM_API_ROOT`) for files over 50 MB.

```bash
git clone -b telegram https://github.com/yhymid/Operator.git && cd Operator
cargo build --release
cp deploy/.env.example .env                  # fill in TELEGRAM_BOT_TOKEN and ADMIN_IDS
./target/release/operator-telegram
```

The bot reads `.env` (and `cookies.txt` for YouTube) from the directory it is started in.
All settings are described in [`deploy/.env.example`](deploy/.env.example);
`DEVELOPER_MODE=true` logs updates, yt-dlp and uploads in detail.

Prebuilt Linux binaries are attached to the
[releases](https://github.com/yhymid/Operator/releases); changes are listed in
[CHANGELOG.md](CHANGELOG.md).

## Layout

```
Cargo.toml          the package (release profile: LTO + strip)
src/main.rs         startup, long polling, shutdown
src/api.rs          Bot API client (JSON + streaming multipart upload)
src/limiter.rs      per-chat rate limiter (priorities, retry_after, merged edits)
src/queue.rs        job queue (pause / resume / cancel)
src/ytdlp.rs        /ytdlp helpers; ytdlp/{format,download,website,replies,queue_view,handlers,jobs}.rs
src/bot.rs          update and command routing
src/common/         code shared with the Discord bot (.env, logging, time, URL guard, media)
deploy/             .env.example, operator-telegram.service
scripts/            deploy.sh (release install with rollback), ops/ (backup, alerts, updates)
```

`src/common/` is identical on the `discord` branch. Change it in its own commit, then run
`scripts/sync-common.sh` (CI warns when the copies differ).

## Running as a service

```bash
sudo cp deploy/operator-telegram.service /etc/systemd/system/operator-tg.service
sudo systemctl daemon-reload && sudo systemctl enable --now operator-tg
journalctl -u operator-tg -f
```

Adjust `WorkingDirectory` and `ExecStart` in the unit. `scripts/deploy.sh telegram <tag>`
installs a released binary: it checks the SHA256, waits for an empty queue, restarts the
unit and rolls back when it does not come up. The unit of a Local Bot API Server contains
`--api-id`/`--api-hash`: keep it out of the repository.

## How a download works

1. **Format** - yt-dlp takes the best video and audio separately: highest resolution and
   frame rate, then SDR over HDR and a stream already in the target codec.
2. **Compatibility** - `ffprobe` checks the file. Video other than 8-bit H.264 (HEVC, VP9,
   AV1, 10-bit/HDR) is encoded to H.264 High; audio other than AAC/MP3 becomes AAC. Matching
   streams are copied. Always `+faststart`, so playback starts at once.
3. **Delivery** - a file up to `YTDLP_MAX_UPLOAD_BYTES` goes to Telegram. A larger one is
   published as a page with a player and a delete button (`YTDLP_WEBSITE_ROOT`,
   `YTDLP_PUBLIC_BASE_URL`), removed after `YTDLP_WEBSITE_TTL_HOURS` (24 h).

### Codec (`YTDLP_VIDEO_CODEC`)

| Value | Discord | Telegram | Size | Notes |
| --- | --- | --- | --- | --- |
| `h264` (default) | yes | yes | baseline | widest compatibility |
| `av1` | yes | depends on the client | usually much smaller | slower encode |
| `source` | HEVC stutters | depends | unchanged | no conversion |

`YTDLP_VIDEO_CRF` changes the conversion quality (lower = better and larger).

## Development

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs the same checks.
