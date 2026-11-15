# Changelog

Releases are cut by hand when enough has changed. Each release on GitHub uses the matching
section below as its description.

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
