# Operator - Discord

A Discord bot written in Rust: one small binary, no runtime to install. This branch is the
**Discord** bot; the Telegram bot lives on the
[`telegram`](https://github.com/yhymid/Operator/tree/telegram) branch.

## Features

- **Music** - Lavalink v4: `/play`, queue, button panel, loop, autoplay, EQ/FX, saved queues
- **/ytdlp** - downloads a video with yt-dlp and posts it in the channel (AV1, small files that
  play inline); files too large for Discord get a web player link; `audio:true` posts MP3
- **Logs** - every server event as a card in a log channel, plus JSONL files with retention,
  export and purge
- **AutoMod** - blocked words and regexes, links, invites, caps, duplicates, mentions
- **Verify** - captcha panel, expiring role, exceptions
- **AutoClean** - scheduled channel cleanup (cron + time zone)
- **Diagnostics** - `/health`, `/debug`
- **Owner tools** - `/cookies`, `/update`, `/sync`, `/servers`, `/presence`, `/restart`, `/cache`

## Commands

| Group | Commands |
| --- | --- |
| Music | `/play` `/pause` `/resume` `/skip` `/stop` `/queue` `/nowplaying` `/volume` `/remove` `/move` `/clear` `/loop` `/reconnect` |
| Download | `/ytdlp url: [audio:]` |
| Moderation (admin) | `/automod status\|test\|rule` · `/verify setup\|status\|resend\|exception_add\|exception_remove\|expire\|cleanup` · `/logs setup\|status\|enable\|disable\|channel\|retention\|include_bots\|categories\|preview\|export\|purge\|test` · `/clean now\|set\|reset\|status` · `/dm` |
| Settings (admin) | `/settings show\|set` · `/health` · `/debug music\|permissions\|logs` |
| Owner | `/cookies` · `/update target:ytdlp\|lavalink\|server\|operator` · `/sync` · `/servers` · `/presence` · `/restart` · `/cache server` |
| General | `/help` |

## Build and run

Requirements: Rust stable (1.88 or newer). Optional: Lavalink v4 (Java 17+) for music;
`yt-dlp` and `ffmpeg`/`ffprobe` (with `libsvtav1`; `libx264` for H.264 mode) for `/ytdlp`.

```bash
git clone -b discord https://github.com/yhymid/Operator.git && cd Operator
cargo build --release
cp deploy/.env.example .env                  # fill in BOT_TOKEN and OWNER_IDS
cp deploy/config.example.json config.json
./target/release/operator-discord doctor     # checks the token, config and directories
./target/release/operator-discord            # starts the bot (registers the slash commands)
```

The bot reads `.env`, `config.json`, `cache/` and `logs/` from the directory it is started in
(or from `OPERATOR_CONFIG_PATH`, `OPERATOR_DATA_DIR`, `OPERATOR_LOG_DIR`). All settings are
described in [`deploy/.env.example`](deploy/.env.example).

Prebuilt Linux binaries are attached to the
[releases](https://github.com/yhymid/Operator/releases); changes are listed in
[CHANGELOG.md](CHANGELOG.md).

| Command line | What it does |
| --- | --- |
| `operator-discord` | starts the bot |
| `operator-discord doctor` | preflight: token, config, directories, owner IDs |
| `operator-discord commands sync` | registers the commands in every server (`--guild=ID[,ID]`, `--global`, `--clear-global`) |

`RUST_LOG=debug` gives detailed logs.

## Layout

```
Cargo.toml          the package (release profile: LTO + strip)
commands.json       slash command definitions
src/main.rs         startup, gateway loop, command line
src/commands/       slash commands
src/modules/        autoclean, automod, verification, fulllogs/, music/
src/common/         code shared with the Telegram bot (.env, logging, time, URL guard, media)
src/lavalink.rs     Lavalink v4 client (REST + WebSocket)
deploy/             .env.example, config.example.json, systemd units, lavalink/
scripts/            deploy.sh (release install with rollback), ops/ (backup, alerts, /update)
docs/               server setup
```

`src/common/` is identical on the `telegram` branch. Change it in its own commit, then run
`scripts/sync-common.sh` (CI warns when the copies differ).

## Lavalink

Config: [`deploy/lavalink/application.yml.example`](deploy/lavalink/application.yml.example),
systemd unit: `deploy/lavalink/lavalink.service` (Lavalink 4.2+). Lavalink downloads its
plugins itself. The connection is set in `.env` (`LAVALINK_URL`, or
`LAVALINK_HOST`/`PORT`/`PASSWORD`/`SECURE`). The bot runs without Lavalink and connects when
the node comes up. A dropped connection resumes the same Lavalink session, and a bot restart
continues each session from the same track and position.

The example config also loads LavaLyrics (the Lyrics button, lyrics from LRCLIB) and LavaDSPX
(Normalize and Echo effects). Deezer (FLAC / MP3 320) is prepared but off: it needs
`DEEZER_ARL` and `DEEZER_MASTER_KEY`, see the comments in the config.

YouTube from a server address (checked 2026-11):

- use the youtube-source plugin as a **snapshot** or built from source
  (`/update target:lavalink`); release 1.18.2 lacks fixes needed on datacenter addresses,
- OAuth enabled (TV client), deciphering through `remoteCipher`,
- keep LavaSrc's `ytdlp` source off: it registers before the plugin and takes over YouTube,
- do not set `preferIPv6Addresses`: YouTube may place the server's IPv6 in another country.

## /ytdlp

1. yt-dlp downloads the best video and audio.
2. `ffprobe` checks the file. Opus becomes AAC and matching streams are copied:
   - 8-bit AV1 is copied; 8-bit H.264 that fits the upload limit is only remuxed, and a larger
     one is encoded with SVT-AV1 to fit;
   - HEVC (Discord on PC shows a frozen picture), VP9 and 10-bit video are encoded with x264
     (`YTDLP_X264_PRESET`, default `veryfast`, CRF 18). On the VPS a 30 s 720p HEVC TikTok took
     8.9 s against 15.1 s with SVT-AV1 preset 10 and came out closer to the source (SSIM 0.988
     vs 0.986), as a larger file (14.2 vs 8.9 MiB). `YTDLP_REENCODE_CODEC=av1` goes back to AV1.
   - `YTDLP_VIDEO_CODEC=h264` makes H.264 the target for everything; `source` skips conversion.
3. A file up to `YTDLP_MAX_UPLOAD_BYTES` (default 25 MiB) is posted as an attachment. A larger
   one goes to `YTDLP_MEDIA_ROOT` and is linked as a player under `YTDLP_MEDIA_BASE_URL` for
   6 hours.

One job runs at a time, for at most 14 minutes. Videos longer than
`YTDLP_MAX_DURATION_SECONDS` (30 min) or larger than `YTDLP_MAX_DOWNLOAD_BYTES` (1 GiB) are
refused, and so are links to local and private addresses. YouTube needs cookies: the owner
uploads them with `/cookies`.

## Diagnostics

`OPERATOR_DIAGNOSTICS=1` in `.env` turns on a recorder that runs next to the bot and never
slows it down (its own thread, a bounded queue; events are dropped and counted when the disk
cannot keep up):

- `diagnostics/events-YYYY-MM-DD.txt` - one JSON object per line: every log line and every
  measured event, with tokens, passwords and webhook URLs removed. Files are readable only by
  the bot's user.
- `diagnostics/summary-YYYY-MM-DD.md` - rewritten every 15 minutes and at shutdown: p50 /
  p90 / p99 of every measured time, failures and status codes, memory and CPU, anomalies,
  the most frequent warnings, and hints on what causes the delay.

Measured: gateway delay and time to the first answer of every command (Discord allows 3 s),
every Discord and Lavalink request (time to first byte, status, server clock), rate limits,
time from the play request to the start of a track, Lavalink voice ping and lost audio
frames, voice server assignment, the /ytdlp stages (queue, download, conversion, upload
speed), yt-dlp/ffmpeg run times, heartbeat, how late the runtime wakes timers, memory, CPU
and child processes. Old days are deleted after `OPERATOR_DIAGNOSTICS_RETENTION_DAYS`
(14); one day's file stops growing at `OPERATOR_DIAGNOSTICS_MAX_FILE_MB` (256).

## Development

```bash
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

CI runs the same checks. Server setup: [docs/deploy-vps.md](docs/deploy-vps.md).
