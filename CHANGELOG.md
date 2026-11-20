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
  whole list (~8 s for 832 tracks): the first playable of the first 10 entries plays at once
  and the rest of the playlist is added to the queue in the background. Diagnostics record
  both times (`music.playlist`).

### Changed
- Default volume is 100 %, so Lavalink passes Opus audio through without re-encoding.
- Tracks start faster (`bufferDurationMs` 225 in the Lavalink config).
- `/ytdlp` converts HEVC (a frozen picture in Discord on PC), VP9 and 10-bit video to H.264
  with x264 `veryfast` instead of AV1: faster, closer to the source and plays on every client,
  with larger files. `YTDLP_REENCODE_CODEC=av1` restores the old behaviour.

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
