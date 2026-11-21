# Changelog

Releases are cut by hand when enough has changed. Each release on GitHub uses the matching
section below as its description.

## Unreleased

### Added
- Music continues after a bot restart: the bot rejoins the voice channel and plays on from
  the same track and position (when the restart took under 10 minutes and someone is still
  listening).
- A short break in the connection to Lavalink no longer stops the music (session resuming),
  and after a Lavalink restart every session gets its track and position back.
- The Lyrics button shows the real lyrics (LavaLyrics + LRCLIB) instead of a search link.
- Two new effects in the FX menu: Normalize and Echo (LavaDSPX).
- Optional 24/7 diagnostics (`OPERATOR_DIAGNOSTICS=1`): every event and log line, without
  tokens or passwords, in `diagnostics/events-YYYY-MM-DD.txt`, and a latency report every
  15 minutes in `diagnostics/summary-YYYY-MM-DD.md` (time to answer a command, Discord and
  Lavalink requests, time until a track starts, voice ping, lost audio frames, /ytdlp
  stages, rate limits, memory and CPU).
- Diagnostics also record the silence between tracks, tell stutter in the middle of a track
  from the gap while the next track loads, and sample the whole server (CPU, iowait, steal,
  free memory, swap); `/ytdlp` jobs record the source codec and what was done to it.
- `/play` with a YouTube playlist link starts the music after ~4 s instead of waiting for the
  whole list: the first playable of the first 10 entries plays at once and the rest of the
  playlist is added to the queue in the background. Diagnostics record both times
  (`music.playlist`); `MUSIC_PLAYLIST_FAST_START=0` turns it off. Measured on the VPS with an 832-track playlist (before: the whole list
  loaded first, music after 8.97 s):

  ```text
  ┌──────────────────────────────┬──────────┬──────────┐
  │            Stage             │  Run 1   │  Run 2   │
  ├──────────────────────────────┼──────────┼──────────┤
  │ yt-dlp: first 10 entries     │ 3.33 s   │ 3.20 s   │
  ├──────────────────────────────┼──────────┼──────────┤
  │ Lavalink: first track        │ 0.36 s   │ 0.19 s   │
  ├──────────────────────────────┼──────────┼──────────┤
  │ Command to music             │ 4.09 s   │ 3.89 s   │
  ├──────────────────────────────┼──────────┼──────────┤
  │ Whole playlist (background)  │ 9.2 s    │ 8.7 s    │
  ├──────────────────────────────┼──────────┼──────────┤
  │ Command to full queue        │ 13.3 s   │ 12.6 s   │
  ├──────────────────────────────┼──────────┼──────────┤
  │ Tracks added in background   │ 831      │ 831      │
  └──────────────────────────────┴──────────┴──────────┘
  ```

### Changed
- No message pings anyone unless it is meant to: every Discord request defaults to no
  mentions (autoclean notices and DMs included), and AutoMod warnings ping only the author.
- `--version`, `/health`, `/diagnostics` and the diagnostics files show the git commit, e.g.
  `2.0.0 (a7b5498)`.
- systemd: the bot starts after Lavalink, and the bot and Lavalink create private files
  (`UMask=0077`).
- Releases are created as drafts after the binary passed a smoke test and are published by
  hand.
- Default volume is 100 %, so Lavalink passes Opus audio through without re-encoding.
- Tracks start faster (`bufferDurationMs` 225 in the Lavalink config).
- `/ytdlp` converts HEVC (a frozen picture in Discord on PC), VP9 and 10-bit video to H.264
  with x264 `veryfast` instead of AV1: faster, closer to the source and plays on every client,
  with larger files. `YTDLP_REENCODE_CODEC=av1` restores the old behaviour. Measured on the
  VPS (6 vCPU, no GPU) with a 30 s 720p HEVC TikTok (source 2.7 MiB; SSIM against the source,
  1 = identical):

  ```text
  ┌────────────────────────────────────┬─────────┬──────────┬───────┐
  │              Encoder               │  Time   │   File   │ SSIM  │
  ├────────────────────────────────────┼─────────┼──────────┼───────┤
  │ x264 veryfast, CRF 18 (new)        │ 8.9 s   │ 14.2 MiB │ 0.988 │
  ├────────────────────────────────────┼─────────┼──────────┼───────┤
  │ SVT-AV1 preset 10, CRF 30 (before) │ 15.1 s  │ 8.9 MiB  │ 0.986 │
  ├────────────────────────────────────┼─────────┼──────────┼───────┤
  │ x264 slow, CRF 18                  │ 31.9 s  │ 15.0 MiB │ 0.992 │
  └────────────────────────────────────┴─────────┴──────────┴───────┘
  ```

  TikTok offers H.264 only up to 540p (720p and up are HEVC only), so the source has to be
  converted to keep its resolution.

## 2.0 - Rust Update

### Changed
- The bot is written in Rust now (the JavaScript version is gone): one small binary, lower
  memory use, faster start.
- Every command, reply, panel and log message is in English.
- `/music mode:...` is replaced by plain commands: `/play`, `/pause`, `/resume`, `/skip`,
  `/stop`, `/queue`, `/nowplaying`, `/volume`, `/remove`, `/move`, `/clear`, `/loop`,
  `/reconnect`.
- `/logs`, `/verify`, `/clean`, `/settings`, `/debug` and `/cache` use subcommands
  (`/logs setup`, `/verify status`, ...) and show only the options that apply.
- `/clean` is available to administrators, not only the bot owner.
- `/help` is a single button panel; `/health` is a four-line overview with problems listed
  only when there are any.
- Server logs are cards with a colour per kind of event, the member's avatar, before/after
  text for edits, the moderator and reason from the audit log, IDs and a jump link.

### Added
- `/ytdlp audio:true` posts the sound only (MP3), with limits on video length and file size.
- Logs cover timeouts, deletes and edits of messages sent before the bot started, and name the
  moderator for role changes.
- AutoMod also checks edited messages and catches blocked words written with accents, digits
  for letters or invisible characters.
- The bot leaves the voice channel when it is alone.
- Owner commands: `/cookies`, `/update` (yt-dlp, the Lavalink YouTube plugin built from
  source, system packages, both bots), `/sync`, `/servers`, `/presence`, `/restart`.
- Server scripts: release install with health check and rollback, daily backup, failure
  notifications, journal size limit.

### Removed
- `/announce` and scheduled announcements.
- `/music` (see above).

### Fixed
- `/ytdlp` and `/play` refuse links to local and private addresses.
- AutoClean no longer deletes messages of protected roles it had not cached.
- Replies never ping `@everyone` or roles through track titles or error texts.
- A corrupt server data file is kept aside instead of being overwritten with defaults.
- Concurrent setting changes no longer overwrite each other.
