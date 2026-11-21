//! Music slash commands (`/play`, `/skip`, `/loop`, ...) and the music panel components (port of `src/cmds/music.js`).

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};
use twilight_model::guild::Permissions;
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, GuildMarker};

use crate::app::App;
use crate::discord::{self, FLAG_COMPONENTS_V2, FLAG_EPHEMERAL, Ix};
use crate::modules::music::{PublicTrack, Repeat, Snapshot};

const MUSIC_BUTTON_PREFIX: &str = "music_panel";
const MUSIC_SELECT_PREFIX: &str = "music_select";

fn audio_file_regex() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?i)\.(aac|aiff?|alac|flac|m4a|mp3|oga|ogg|opus|wav|weba|wma)(\?|$)")
            .expect("static regex")
    })
}

pub fn format_duration(seconds: i64) -> String {
    if seconds <= 0 {
        return "?:??".to_owned();
    }
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes}:{secs:02}")
    }
}

pub fn format_track_line(track: &PublicTrack, index: Option<usize>) -> String {
    let prefix = index.map(|i| format!("{i}. ")).unwrap_or_default();
    format!(
        "{prefix}{} ({})",
        track.title,
        format_duration(track.duration_sec)
    )
}

fn format_progress(snapshot: Option<&Snapshot>) -> String {
    match snapshot.and_then(|s| s.current_track.as_ref().map(|t| (s, t))) {
        Some((s, track)) => format!(
            "{} / {}",
            format_duration(s.position_sec),
            format_duration(track.duration_sec)
        ),
        None => "?:??".to_owned(),
    }
}

fn format_repeat(mode: Repeat) -> &'static str {
    match mode {
        Repeat::Track => "track",
        Repeat::Queue => "queue",
        Repeat::Off => "off",
    }
}

fn truncate_text(value: &str, max: usize) -> String {
    let raw = value.trim();
    let units: Vec<u16> = raw.encode_utf16().collect();
    if units.len() <= max {
        return raw.to_owned();
    }
    let cut = String::from_utf16_lossy(&units[..max.saturating_sub(3)]);
    format!("{}...", cut.trim_end())
}

fn progress_bar(snapshot: Option<&Snapshot>, slots: usize) -> String {
    let duration = snapshot
        .and_then(|s| s.current_track.as_ref())
        .map_or(0, |t| t.duration_sec);
    let position = snapshot.map_or(0, |s| s.position_sec);
    if duration <= 0 {
        return format!("{} 0%", "▱".repeat(slots));
    }
    let ratio = (position as f64 / duration as f64).clamp(0.0, 1.0);
    let filled = ((ratio * slots as f64).round() as usize).min(slots);
    let percent = (ratio * 100.0).round() as i64;
    format!(
        "{}{} {percent}%",
        "▰".repeat(filled),
        "▱".repeat(slots - filled)
    )
}

fn active_filter_labels(snapshot: Option<&Snapshot>) -> Vec<String> {
    let Some(f) = snapshot.map(|s| &s.filters) else {
        return Vec::new();
    };
    let mut labels = Vec::new();
    if f.eq_preset != "flat" {
        let mut chars = f.eq_preset.chars();
        if let Some(first) = chars.next() {
            labels.push(first.to_uppercase().collect::<String>() + chars.as_str());
        }
    }
    for (on, label) in [
        (f.vaporwave, "Vaporwave"),
        (f.eight_d, "8D"),
        (f.karaoke, "Karaoke"),
        (f.soft_limiter, "SoftLimiter"),
    ] {
        if on {
            labels.push(label.to_owned());
        }
    }
    labels
}

fn button(action: &str, guild: &str, label: impl Into<String>, style: u8, disabled: bool) -> Value {
    json!({
        "type": 2,
        "custom_id": format!("{MUSIC_BUTTON_PREFIX}:{action}:{guild}"),
        "label": label.into(),
        "style": style,
        "disabled": disabled,
    })
}

fn select_option(label: &str, value: &str, description: &str, default: bool) -> Value {
    json!({ "label": label, "value": value, "description": description, "default": default })
}

/// `buildMusicPanelPayload(snapshot)`
pub fn panel_payload(snapshot: Option<&Snapshot>) -> Value {
    let current = snapshot.and_then(|s| s.current_track.as_ref());
    let title = truncate_text(current.map_or("Nothing playing", |t| t.title.as_str()), 80);
    let paused = snapshot.is_some_and(|s| s.paused);
    let status = if paused {
        "Paused"
    } else if snapshot.is_some_and(|s| s.playing) {
        "Playing"
    } else {
        "Stop"
    };
    let queue = snapshot.map_or(&[][..], |s| s.queue.as_slice());
    let next_lines = if queue.is_empty() {
        "No upcoming tracks.".to_owned()
    } else {
        queue
            .iter()
            .take(5)
            .enumerate()
            .map(|(i, t)| format!("{}. {}", i + 1, truncate_text(&t.title, 70)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let muted = snapshot.is_some_and(|s| s.muted);
    let disabled = current.is_none();
    let guild = snapshot.map_or("0".to_owned(), |s| s.guild_id.to_string());
    let filters = active_filter_labels(snapshot);
    let default_filters = Default::default();
    let f = snapshot.map_or(&default_filters, |s| &s.filters);
    let eq_preset = f.eq_preset.clone();
    let active_fx: Vec<&str> = [
        (f.vaporwave, "Vaporwave"),
        (f.eight_d, "8D"),
        (f.karaoke, "Karaoke"),
        (f.soft_limiter, "SoftLimiter"),
    ]
    .into_iter()
    .filter_map(|(on, label)| on.then_some(label))
    .collect();
    let has_fx = !active_fx.is_empty();
    let autoplay = snapshot.is_some_and(|s| s.autoplay);

    let mut text: Vec<String> = vec!["## Now Playing".to_owned(), format!("**{title}**")];
    if let Some(url) = current.map(|t| t.url.clone()).filter(|u| !u.is_empty()) {
        text.push(url);
    }
    text.extend([
        String::new(),
        format!("Time: **{}**", format_progress(snapshot)),
        progress_bar(snapshot, 12),
        format!(
            "Loop: {} | Vol: {}% | Filters: {} | Autoplay: {}",
            format_repeat(snapshot.map_or(Repeat::Off, |s| s.repeat_mode)),
            snapshot.map_or("?".to_owned(), |s| s.volume.to_string()),
            if filters.is_empty() {
                "off".to_owned()
            } else {
                filters.join(", ")
            },
            if autoplay { "ON" } else { "OFF" }
        ),
        format!("Status: **{status}**"),
        format!(
            "Channel: {}",
            snapshot
                .and_then(|s| s.voice_channel_id)
                .map_or("none".to_owned(), |c| format!("<#{c}>"))
        ),
        String::new(),
        "**Queue**".to_owned(),
        next_lines,
    ]);

    json!({
        "flags": FLAG_COMPONENTS_V2,
        "components": [{
            "type": 17,
            "components": [
                { "type": 10, "content": text.join("\n") },
                { "type": 1, "components": [
                    button("loop", &guild, "Loop", 2, disabled),
                    button("queue", &guild, format!("Queue ({})", queue.len()), 2, disabled),
                    button("playpause", &guild, if paused { "Play" } else { "Pause" }, if paused { 3 } else { 1 }, disabled),
                    button("stop", &guild, "Stop", 4, disabled),
                    button("shuffle", &guild, "Shuffle", 2, disabled || queue.len() < 2),
                ]},
                { "type": 1, "components": [
                    button("eq", &guild, "EQ", if eq_preset != "flat" { 3 } else { 2 }, disabled),
                    button("volumedown", &guild, "Volume -", 2, disabled),
                    button("mute", &guild, if muted { "Unmute" } else { "Mute" }, if muted { 3 } else { 2 }, disabled),
                    button("volumeup", &guild, "Volume +", 2, disabled),
                    button("fx", &guild, "FX", if has_fx { 3 } else { 2 }, disabled),
                ]},
                { "type": 1, "components": [
                    button("autoplay", &guild, "Autoplay", if autoplay { 3 } else { 2 }, disabled),
                    button("lyrics", &guild, "Lyrics", 2, disabled),
                    button("savequeue", &guild, "Save Queue", 2, disabled),
                    button("restorequeue", &guild, "Restore Queue", 2, disabled),
                    button("refresh", &guild, "Refresh", 2, disabled),
                ]},
                { "type": 1, "components": [{
                    "type": 3,
                    "custom_id": format!("{MUSIC_SELECT_PREFIX}:eq:{guild}"),
                    "placeholder": format!("EQ preset: {eq_preset}"),
                    "options": [
                        select_option("Flat", "flat", "Neutral sound", eq_preset == "flat"),
                        select_option("Bassboost", "bassboost", "Stronger bass", eq_preset == "bassboost"),
                        select_option("Vocal", "vocal", "Clearer vocals", eq_preset == "vocal"),
                        select_option("Treble", "treble", "Brighter highs", eq_preset == "treble"),
                        select_option("Nightcore", "nightcore", "Faster + higher pitch", eq_preset == "nightcore"),
                    ],
                    "disabled": disabled,
                }]},
                { "type": 1, "components": [{
                    "type": 3,
                    "custom_id": format!("{MUSIC_SELECT_PREFIX}:fx:{guild}"),
                    "placeholder": if active_fx.is_empty() { "FX: choose an effect".to_owned() } else { format!("FX: {}", active_fx.join(", ")) },
                    "options": [
                        select_option("Vaporwave", "vaporwave", "Slower and warmer", false),
                        select_option("8D", "8d", "Rotating stereo effect", false),
                        select_option("Karaoke", "karaoke", "Try to suppress vocals", false),
                        select_option("SoftLimiter", "softlimiter", "Gentle low-pass", false),
                        select_option("Normalize", "normalize", "Even out loud and quiet tracks", false),
                        select_option("Echo", "echo", "Echo effect", false),
                        select_option("FX Off", "off", "Turn off all FX", false),
                    ],
                    "disabled": disabled,
                }]},
            ],
        }],
    })
}

fn queue_preview(snapshot: Option<&Snapshot>) -> String {
    let Some(queue) = snapshot.map(|s| &s.queue).filter(|q| !q.is_empty()) else {
        return "The queue is empty.".to_owned();
    };
    let mut lines: Vec<String> = queue
        .iter()
        .take(10)
        .enumerate()
        .map(|(i, t)| format_track_line(t, Some(i + 1)))
        .collect();
    if queue.len() > lines.len() {
        lines.push(format!("... and {} more", queue.len() - lines.len()));
    }
    lines.join("\n")
}

/// Discord allows 2000 characters per message; the rest is the title line and the cut marker.
const MAX_LYRICS_CHARS: usize = 1800;

fn lyrics_search_link(title: &str) -> String {
    let query: String = url::form_urlencoded::byte_serialize(title.as_bytes())
        .collect::<String>()
        .replace('+', "%20");
    format!("https://genius.com/search?q={query}")
}

/// The lyrics from Lavalink, or a search link when it has none (or no lyrics plugin).
fn lyrics_reply(title: &str, lyrics: Option<&str>) -> String {
    match lyrics {
        Some(text) => {
            let mut body: String = text.chars().take(MAX_LYRICS_CHARS).collect();
            if text.chars().count() > MAX_LYRICS_CHARS {
                body.push_str("\n… ");
                body.push_str(&lyrics_search_link(title));
            }
            format!("**{}**\n{body}", truncate_text(title, 120))
        }
        None => format!("No lyrics found. Search: {}", lyrics_search_link(title)),
    }
}

fn member_voice_channel(ix: &Ix<'_>) -> Option<Id<ChannelMarker>> {
    let guild = ix.guild_id()?;
    ix.app
        .cache
        .voice_state(ix.user_id(), guild)
        .map(|v| v.channel_id())
}

fn same_voice(ix: &Ix<'_>, channel: Option<Id<ChannelMarker>>) -> bool {
    matches!((member_voice_channel(ix), channel), (Some(a), Some(b)) if a == b)
}

fn require_same_voice(ix: &Ix<'_>, channel: Option<Id<ChannelMarker>>) -> Option<&'static str> {
    if channel.is_some() && !same_voice(ix, channel) {
        return Some("You must be in the same voice channel as the bot.");
    }
    if channel.is_none() && member_voice_channel(ix).is_none() {
        return Some("Join a voice channel to control this session.");
    }
    None
}

enum PlaySource {
    Query(String),
    Attachment { url: String, label: String },
}

fn play_selection(ix: &Ix<'_>) -> Result<PlaySource> {
    let opts = ix.opts();
    let attachment = opts.attachment("audio");
    if let Some(a) = &attachment {
        let content_type = a.content_type.clone().unwrap_or_default().to_lowercase();
        let name = if a.filename.is_empty() {
            a.url.to_lowercase()
        } else {
            a.filename.to_lowercase()
        };
        if !content_type.starts_with("audio/") && !audio_file_regex().is_match(&name) {
            bail!(
                "\"{}\" does not look like an audio file.",
                if a.filename.is_empty() {
                    "file"
                } else {
                    a.filename.as_str()
                }
            );
        }
    }
    let query = opts.string("query").unwrap_or_default().trim().to_owned();
    match (attachment, query.is_empty()) {
        (None, true) => bail!("Provide `query` or an `audio` file."),
        (Some(_), false) => bail!("Choose one source: `query` or `audio`."),
        (Some(a), true) => Ok(PlaySource::Attachment {
            label: if a.filename.is_empty() {
                a.url.clone()
            } else {
                a.filename.clone()
            },
            url: a.url,
        }),
        (None, false) => Ok(PlaySource::Query(query)),
    }
}

/// Pre-flight voice checks of `/play`; returns the reply text on failure.
fn voice_join_problem(
    app: &App,
    guild_id: Id<GuildMarker>,
    channel: Id<ChannelMarker>,
) -> Option<&'static str> {
    let limit = app
        .cache
        .channel(channel)
        .and_then(|c| c.user_limit)
        .unwrap_or(0);
    let members = app
        .cache
        .voice_channel_states(channel)
        .map_or(0, Iterator::count);
    if limit > 0 && members >= limit as usize {
        return Some("This voice channel is full.");
    }
    if app.cache.member(guild_id, app.bot_id).is_none() {
        return Some("Cannot check the bot's permissions.");
    }
    let perms = discord::bot_channel_permissions(app, channel)?;
    if !discord::has(perms, Permissions::CONNECT) {
        return Some("The bot cannot join this voice channel (joinable=false).");
    }
    if !discord::has(perms, Permissions::SPEAK) {
        return Some("The bot cannot speak in this voice channel (speakable=false).");
    }
    None
}

fn play_error_text(message: &str, requested: &str) -> String {
    let re = |p: &str| regex::Regex::new(p).expect("static regex");
    if re(r"(?i)no node|not connected|lavalink").is_match(message) {
        return "Playback error: Lavalink is not connected or does not support this source. Check the node and plugins.".to_owned();
    }
    if re(r"(?i)got 429|too many requests|rate.?limit").is_match(message) {
        return "Playback error: the source is temporarily rate limiting requests (429). Try again in a moment or choose another source.".to_owned();
    }
    if re(r"(?i)operation was aborted").is_match(message) {
        return "Playback error: The operation was aborted. Try again in a moment or give a direct YouTube link.".to_owned();
    }
    let nothing = re(r"(?i)nothing found").is_match(message);
    if nothing && re(r"(?i)(?:youtu\.be|youtube\.com)").is_match(requested) {
        return "Playback error: YouTube rejected this link (login/anti-bot required). Try another link, a search phrase or SoundCloud.".to_owned();
    }
    if nothing && re(r"(?i)cdn\.discordapp\.com/attachments/").is_match(requested) {
        return "Playback error: this Discord attachment link is unavailable or expired. Upload the file again and use the `audio` option.".to_owned();
    }
    if re(r"(?i)failed to retrieve secret from spotify|spotifytokentracker").is_match(message) {
        return "Playback error: Spotify playlists/albums are temporarily not responding on the API side. Use a link to a single Spotify track or a search phrase.".to_owned();
    }
    format!("Playback error: {message}")
}

/// Big YouTube playlists take Lavalink ~1 s per 100 tracks to load. Instead of waiting, play
/// the first track that loads from the playlist's first 10 entries (yt-dlp, ~3 s), then load
/// the whole playlist in the background and append everything after that track. `None` (the
/// normal path) when yt-dlp or every one of the 10 tracks fails.
/// `MUSIC_PLAYLIST_FAST_START=0` turns it off (the whole playlist loads first, as before).
async fn fast_playlist_start(
    ix: &Ix<'_>,
    session: &mut crate::modules::music::Session,
    query: &str,
) -> Result<Option<String>> {
    use crate::common::diagnostics;
    let app = ix.app;
    let guild_id = session.guild_id;
    let started = std::time::Instant::now();
    let (title, ids) = match super::ytdlp::playlist_head(query, 10).await {
        Ok(head) if !head.1.is_empty() => head,
        Ok(_) | Err(_) => {
            tracing::warn!(
                "Music fast playlist start unavailable guild={guild_id}; loading it whole"
            );
            diagnostics::record(
                "music.playlist",
                json!({ "op": "fast_start", "ok": false,
                        "idsMs": diagnostics::elapsed_ms(started) }),
            );
            return Ok(None);
        }
    };
    let ids_ms = diagnostics::elapsed_ms(started);
    let mut first = None;
    for (index, id) in ids.iter().enumerate() {
        match app
            .music
            .resolve_tracks(&format!("https://www.youtube.com/watch?v={id}"))
            .await
        {
            Ok(resolved) => {
                if let Some(track) = resolved.tracks.into_iter().next() {
                    first = Some((index, track));
                    break;
                }
            }
            Err(error) => tracing::warn!("Music fast playlist skips id={id}: {error}"),
        }
    }
    let Some((skipped, track)) = first else {
        return Ok(None);
    };
    let started_id = track.raw["info"]["identifier"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    let batch = app
        .music
        .enqueue_many(session, std::slice::from_ref(&track))
        .await?;
    session.created_player = false;
    diagnostics::record(
        "music.playlist",
        json!({ "op": "fast_start", "ok": true, "idsMs": ids_ms,
                "firstTrackMs": diagnostics::elapsed_ms(started),
                "sinceCommandMs": diagnostics::elapsed_ms(ix.received), "skipped": skipped }),
    );
    let title = title.unwrap_or_else(|| "Playlist".to_owned());
    let now = if batch.started {
        format!("Now playing: {}.", format_track_line(&track, None))
    } else {
        format!("Queued: {}.", format_track_line(&track, None))
    };
    tracing::info!(
        "Music fast playlist start guild={guild_id} title=\"{title}\" first=\"{}\" skipped={skipped}",
        track.title
    );

    let app = std::sync::Arc::clone(app);
    let token = ix.interaction.token.clone();
    let session = session.clone();
    let query = query.to_owned();
    let received = ix.received;
    let (title_bg, now_bg) = (title.clone(), now.clone());
    tokio::spawn(async move {
        let load = std::time::Instant::now();
        let (text, payload) = match app.music.resolve_tracks(&query).await {
            Ok(resolved) => {
                let total = resolved.tracks.len();
                let title = resolved.playlist_title.unwrap_or(title_bg);
                match app
                    .music
                    .append_playlist_rest(&session, &resolved.tracks, &started_id)
                    .await
                {
                    Some(added) => (
                        Some(format!(
                            "Added playlist \"{title}\" ({} tracks). {now_bg}",
                            added + 1
                        )),
                        json!({ "op": "rest", "ok": true, "tracks": total, "added": added }),
                    ),
                    None => (
                        None,
                        json!({ "op": "rest", "ok": true, "tracks": total, "discarded": true }),
                    ),
                }
            }
            Err(error) => {
                tracing::warn!("Music playlist rest failed guild={guild_id}: {error}");
                (
                    Some(format!(
                        "Playlist \"{title_bg}\": {now_bg} The rest of the playlist could not be loaded: {}",
                        discord::err_text(&error)
                    )),
                    json!({ "op": "rest", "ok": false, "error": discord::err_text(&error) }),
                )
            }
        };
        let mut payload = payload;
        payload["loadMs"] = json!(diagnostics::elapsed_ms(load));
        payload["sinceCommandMs"] = json!(diagnostics::elapsed_ms(received));
        diagnostics::record("music.playlist", payload);
        let Some(text) = text else {
            return;
        };
        let Ok(body) = serde_json::to_vec(&discord::no_pings(json!({ "content": text }))) else {
            return;
        };
        let client = app.http.interaction(app.application_id);
        let request = client.update_response(&token).payload_json(&body);
        if let Err(error) = discord::timed("interaction.edit", "edit", request).await {
            tracing::warn!("Music playlist reply update failed guild={guild_id}: {error}");
        }
    });
    Ok(Some(format!(
        "Playlist \"{title}\": {now} Loading the rest of the playlist..."
    )))
}

async fn execute_play(ix: &Ix<'_>, guild_id: Id<GuildMarker>) -> Result<()> {
    let app = ix.app;
    let Some(voice) = member_voice_channel(ix) else {
        return ix.reply_ephemeral("Join a voice channel first.").await;
    };
    if let Some(problem) = voice_join_problem(app, guild_id, voice) {
        return ix.reply_ephemeral(problem).await;
    }
    if discord::bot_channel_permissions(app, voice).is_none_or(|p| {
        !discord::has(p, Permissions::CONNECT) || !discord::has(p, Permissions::SPEAK)
    }) {
        return ix
            .reply_ephemeral("The bot lacks Connect and Speak permissions in this channel.")
            .await;
    }

    let user = ix.user_id();
    let mut stage = "prepare";
    let mut requested = String::new();
    let mut session = None;
    let result: Result<String> = async {
        let selection = play_selection(ix)?;
        ix.defer(false).await?;
        let source = match &selection {
            PlaySource::Query(_) => "query",
            PlaySource::Attachment { .. } => "attachments",
        };
        tracing::info!("Music command play start guild={guild_id} user={user} source={source}");
        // Lavalink's `http` source would fetch any URL, including local services.
        if let PlaySource::Query(query) = &selection
            && let Ok(parsed) = url::Url::parse(query)
            && matches!(parsed.scheme(), "http" | "https")
        {
            let port = parsed.port_or_known_default().unwrap_or(443);
            crate::common::net::ensure_public_host(parsed.host_str().unwrap_or_default(), port)
                .await
                .map_err(|error| anyhow::anyhow!(super::ytdlp::host_error_text(error)))?;
        }
        stage = "connect";
        let created = app.music.ensure_session(app, guild_id, voice).await?;
        let session = session.insert(created);

        match selection {
            PlaySource::Query(query) => {
                requested.clone_from(&query);
                if regex::Regex::new(r"(?i)open\.spotify\.com/(playlist|album)")
                    .expect("static regex")
                    .is_match(&query)
                {
                    ix.edit_content("Resolving Spotify through Lavalink. Large playlists may take a moment.")
                        .await?;
                }
                stage = "resolve";
                if crate::common::env::bool("MUSIC_PLAYLIST_FAST_START", true)
                    && crate::modules::music::youtube_playlist_only(&query)
                    && let Some(text) = fast_playlist_start(ix, session, &query).await?
                {
                    return Ok(text);
                }
                let resolved = app.music.resolve_tracks(&query).await?;
                tracing::info!(
                    "Music command play resolved guild={guild_id} user={user} type={} count={} firstUrl=\"{}\" firstTitle=\"{}\"",
                    if resolved.playlist { "playlist" } else { "single" },
                    resolved.tracks.len(),
                    resolved.tracks.first().map_or("", |t| t.url.as_str()),
                    resolved.tracks.first().map_or("", |t| t.title.as_str())
                );
                if resolved.tracks.is_empty() {
                    bail!("No track found.");
                }
                stage = "enqueue";
                if resolved.playlist {
                    let batch = app.music.enqueue_many(session, &resolved.tracks).await?;
                    session.created_player = false;
                    let title = resolved.playlist_title.unwrap_or_else(|| "Playlist".to_owned());
                    return Ok(match batch.started_track.filter(|_| batch.started) {
                        Some(track) => format!(
                            "Added playlist \"{title}\" ({} tracks). Now playing: {}.",
                            batch.added,
                            format_track_line(&track, None)
                        ),
                        None => format!("Added playlist \"{title}\" ({} tracks) to the queue.", batch.added),
                    });
                }
                let result = app.music.enqueue(session, &resolved.tracks[0]).await?;
                session.created_player = false;
                tracing::info!(
                    "Music command play enqueued guild={guild_id} user={user} started={} trackUrl=\"{}\" trackTitle=\"{}\"",
                    result.started,
                    result.track.url,
                    result.track.title
                );
                Ok(if result.started {
                    format!("Now playing: {}.", format_track_line(&result.track, None))
                } else {
                    format!(
                        "Added to the queue (position {}): {}.",
                        result.position,
                        format_track_line(&result.track, None)
                    )
                })
            }
            PlaySource::Attachment { url, label } => {
                stage = "resolve";
                requested.clone_from(&url);
                let mut tracks = Vec::new();
                let mut failed = 0;
                match app.music.resolve_tracks(&url).await {
                    Ok(resolved) => match resolved.tracks.into_iter().next() {
                        Some(track) => tracks.push(track),
                        None => failed += 1,
                    },
                    Err(error) => {
                        tracing::warn!(
                            "Music attachment resolve failed guild={guild_id} user={user} source=attachment label=\"{label}\": {error}"
                        );
                        failed += 1;
                    }
                }
                if tracks.is_empty() {
                    bail!("Could not play any audio file.");
                }
                stage = "enqueue";
                let batch = app.music.enqueue_many(session, &tracks).await?;
                session.created_player = false;
                tracing::info!(
                    "Music command batch enqueued guild={guild_id} user={user} source=attachments added={} failed={failed}",
                    batch.added
                );
                let label = if batch.added == 1 { "audio file" } else { "audio files" };
                let skipped = if failed > 0 { format!(" Skipped: {failed}.") } else { String::new() };
                Ok(match batch.started_track.filter(|_| batch.started) {
                    Some(track) => format!(
                        "Added {} {label}. Now playing: {}.{skipped}",
                        batch.added,
                        format_track_line(&track, None)
                    ),
                    None => format!("Added {} {label} to the queue.{skipped}", batch.added),
                })
            }
        }
    }
    .await;

    match result {
        Ok(text) => ix.edit_content(text).await,
        Err(error) => {
            let cleaned = match &session {
                Some(s) if s.created_player => {
                    app.music
                        .cleanup_fresh_session(app, s, &format!("music play failed at {stage}"))
                        .await
                }
                _ => false,
            };
            let message = discord::err_text(&error);
            tracing::error!(
                "Music command play error guild={guild_id} user={user} stage={stage} source=\"{}\" cleanup={}: {message}",
                if requested.is_empty() {
                    "n/a"
                } else {
                    &requested
                },
                if cleaned {
                    "fresh_session_destroyed"
                } else {
                    "none"
                }
            );
            if !ix.deferred() && !ix.replied() {
                return ix
                    .reply_ephemeral(format!("Playback error: {message}"))
                    .await;
            }
            ix.edit_content(play_error_text(&message, &requested)).await
        }
    }
}

/// Slash commands handled here; each one is a former `/music mode:<name>`.
pub const COMMANDS: [&str; 13] = [
    "play",
    "pause",
    "resume",
    "skip",
    "stop",
    "queue",
    "nowplaying",
    "volume",
    "remove",
    "move",
    "clear",
    "loop",
    "reconnect",
];

pub async fn execute(ix: &Ix<'_>, mode: &str) -> Result<()> {
    let Some(guild_id) = ix.guild_id() else {
        return ix
            .reply_ephemeral("This command only works in a server.")
            .await;
    };
    let app = ix.app;
    let opts = ix.opts();
    if mode == "play" {
        return execute_play(ix, guild_id).await;
    }
    let snapshot = app.music.snapshot(guild_id).await;
    let voice = snapshot.as_ref().and_then(|s| s.voice_channel_id);
    let no_session = || ix.reply_ephemeral("No active music session.");

    match mode {
        "queue" => {
            let Some(s) = snapshot.filter(|s| s.current_track.is_some() || !s.queue.is_empty())
            else {
                return ix.reply(json!({ "content": "The queue is empty." })).await;
            };
            let mut lines = vec![format!(
                "Voice channel: {}",
                s.voice_channel_id
                    .map_or("<#null>".to_owned(), |c| format!("<#{c}>"))
            )];
            if let Some(track) = &s.current_track {
                lines.push(format!("Now playing: {}", format_track_line(track, None)));
            }
            if !s.queue.is_empty() {
                lines.push("Up next:".to_owned());
                for (i, track) in s.queue.iter().take(10).enumerate() {
                    lines.push(format_track_line(track, Some(i + 1)));
                }
                if s.queue.len() > 10 {
                    lines.push(format!("... and {} more", s.queue.len() - 10));
                }
            }
            ix.reply(json!({ "content": lines.join("\n") })).await
        }
        "nowplaying" => {
            let Some(s) = snapshot.filter(|s| s.current_track.is_some() || !s.queue.is_empty())
            else {
                return ix.reply_ephemeral("Nothing is playing right now.").await;
            };
            ix.reply(panel_payload(Some(&s))).await
        }
        "skip" => {
            if snapshot.as_ref().is_none_or(|s| s.current_track.is_none()) {
                return ix.reply_ephemeral("Nothing is playing.").await;
            }
            if !same_voice(ix, voice) {
                return ix
                    .reply_ephemeral("You must be in the same voice channel as the bot.")
                    .await;
            }
            match app.music.skip(guild_id).await? {
                Some(track) => {
                    ix.reply(json!({ "content": format!("Skipped: {}.", format_track_line(&track, None)) }))
                        .await
                }
                None => ix.reply_ephemeral("Nothing is playing.").await,
            }
        }
        "stop" => {
            if snapshot.is_none() {
                return no_session().await;
            }
            if !same_voice(ix, voice) {
                return ix
                    .reply_ephemeral("You must be in the same voice channel as the bot.")
                    .await;
            }
            app.music.stop(app, guild_id).await;
            ix.reply(json!({ "content": "Stopped the music and cleared the queue." }))
                .await
        }
        "pause" | "resume" => {
            let pause = mode == "pause";
            let Some(s) = snapshot.filter(|s| s.current_track.is_some()) else {
                return ix.reply_ephemeral("Nothing is playing.").await;
            };
            if let Some(error) = require_same_voice(ix, voice) {
                return ix.reply_ephemeral(error).await;
            }
            if pause && s.paused {
                return ix.reply_ephemeral("The music is already paused.").await;
            }
            if !pause && !s.paused {
                return ix.reply_ephemeral("The music is already playing.").await;
            }
            let next = app.music.set_paused(guild_id, Some(pause)).await?;
            match next.and_then(|n| n.current_track) {
                Some(track) => {
                    let verb = if pause { "Paused" } else { "Resumed" };
                    ix.reply(json!({ "content": format!("{verb}: {}.", format_track_line(&track, None)) }))
                        .await
                }
                None if pause => ix.reply_ephemeral("Nothing is playing.").await,
                None => no_session().await,
            }
        }
        "volume" => {
            if snapshot.is_none() {
                return no_session().await;
            }
            if let Some(error) = require_same_voice(ix, voice) {
                return ix.reply_ephemeral(error).await;
            }
            let Some(delta) = opts.integer("value").filter(|d| *d != 0) else {
                return ix
                    .reply_ephemeral("Provide `value`, e.g. `10` or `-10`.")
                    .await;
            };
            match app.music.adjust_volume(guild_id, delta).await? {
                Some(next) => {
                    ix.reply(json!({ "content": format!(
                        "Volume set to **{}%** (change {}{delta}).",
                        next.volume,
                        if delta > 0 { "+" } else { "" }
                    ) }))
                    .await
                }
                None => no_session().await,
            }
        }
        "remove" => {
            let Some(s) = snapshot else {
                return no_session().await;
            };
            if let Some(error) = require_same_voice(ix, voice) {
                return ix.reply_ephemeral(error).await;
            }
            let Some(position) = opts.integer("position").filter(|p| *p > 0) else {
                return ix.reply_ephemeral("Provide `position` >= 1.").await;
            };
            if s.queue.is_empty() {
                return ix.reply_ephemeral("The queue is empty.").await;
            }
            if position as usize > s.queue.len() {
                return ix
                    .reply_ephemeral(format!(
                        "There is no track at position {position}. The queue has {} entries.",
                        s.queue.len()
                    ))
                    .await;
            }
            match app.music.remove_from_queue(guild_id, position).await? {
                Some(track) => {
                    ix.reply(json!({ "content": format!(
                        "Removed from the queue (position {position}): {}.",
                        format_track_line(&track, None)
                    ) }))
                    .await
                }
                None => {
                    ix.reply_ephemeral(format!(
                        "Could not remove position {position} from the queue."
                    ))
                    .await
                }
            }
        }
        "move" => {
            let Some(s) = snapshot else {
                return no_session().await;
            };
            if let Some(error) = require_same_voice(ix, voice) {
                return ix.reply_ephemeral(error).await;
            }
            let (Some(from), Some(to)) = (
                opts.integer("from").filter(|p| *p > 0),
                opts.integer("to").filter(|p| *p > 0),
            ) else {
                return ix
                    .reply_ephemeral("Provide `from` and `to`, both >= 1.")
                    .await;
            };
            let len = s.queue.len() as i64;
            if from > len || to > len {
                return ix
                    .reply_ephemeral(format!("The queue has {len} entries."))
                    .await;
            }
            match app
                .music
                .move_in_queue(guild_id, from as usize, to as usize)
                .await
            {
                Some(track) => {
                    ix.reply(json!({ "content": format!(
                        "Moved from position {from} to {to}: {}.",
                        format_track_line(&track, None)
                    ) }))
                    .await
                }
                None => ix.reply_ephemeral("Could not move the track.").await,
            }
        }
        "loop" => {
            if snapshot.is_none() {
                return no_session().await;
            }
            if let Some(error) = require_same_voice(ix, voice) {
                return ix.reply_ephemeral(error).await;
            }
            let target = opts.string("mode").map(|m| Repeat::parse(&m));
            // Without `mode` it cycles off -> utwór -> kolejka like the panel button.
            let mut next = app.music.cycle_loop(guild_id).await?;
            if let Some(target) = target {
                for _ in 0..2 {
                    if next.as_ref().is_none_or(|s| s.repeat_mode == target) {
                        break;
                    }
                    next = app.music.cycle_loop(guild_id).await?;
                }
            }
            match next {
                Some(s) => {
                    ix.reply(json!({ "content": format!("Repeat: **{}**.", format_repeat(s.repeat_mode)) }))
                        .await
                }
                None => no_session().await,
            }
        }
        "clear" => {
            let Some(s) = snapshot else {
                return no_session().await;
            };
            if let Some(error) = require_same_voice(ix, voice) {
                return ix.reply_ephemeral(error).await;
            }
            if s.queue.is_empty() {
                return ix.reply_ephemeral("The queue is already empty.").await;
            }
            let removed = app.music.clear_queue(guild_id).await.unwrap_or(0);
            ix.reply(json!({ "content": format!(
                "Cleared the queue. Removed **{removed}** upcoming tracks."
            ) }))
            .await
        }
        "reconnect" => {
            if snapshot.is_none() {
                return no_session().await;
            }
            if let Some(error) = require_same_voice(ix, voice) {
                return ix.reply_ephemeral(error).await;
            }
            let result: Result<()> = async {
                ix.defer(true).await?;
                match app.music.reconnect_session(app, guild_id).await? {
                    None => ix.edit_content("No active music session.").await,
                    Some(r) => {
                        ix.edit_content(format!(
                            "Reconnected. Channel: <#{}>. Node: **{}**.",
                            r.voice_channel_id,
                            if r.node_connected {
                                "online"
                            } else {
                                "offline"
                            }
                        ))
                        .await
                    }
                }
            }
            .await;
            if let Err(error) = result {
                let message = discord::err_text(&error);
                tracing::warn!(
                    "Music reconnect error guild={guild_id} user={}: {message}",
                    ix.user_id()
                );
                if !ix.deferred() && !ix.replied() {
                    return ix
                        .reply_ephemeral(format!("Reconnect failed: {message}"))
                        .await;
                }
                return ix
                    .edit_content(format!("Reconnect failed: {message}"))
                    .await;
            }
            Ok(())
        }
        _ => {
            ix.reply_ephemeral(format!("Unknown music mode: {mode}."))
                .await
        }
    }
}

/// `executeMusicButtonInteraction`; returns `true` when the component was ours.
pub async fn handle_component(ix: &Ix<'_>) -> Result<bool> {
    let Some(custom_id) = ix.custom_id() else {
        return Ok(false);
    };
    let mut parts = custom_id.split(':');
    let (Some(prefix), Some(action), Some(panel_guild)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return Ok(false);
    };
    if action.is_empty() || panel_guild.is_empty() {
        return Ok(false);
    }
    let is_button = match prefix {
        MUSIC_BUTTON_PREFIX => true,
        MUSIC_SELECT_PREFIX => false,
        _ => return Ok(false),
    };
    let Some(guild_id) = ix.guild_id().filter(|g| g.to_string() == panel_guild) else {
        ix.reply_ephemeral("This panel does not belong to this server.")
            .await?;
        return Ok(true);
    };
    if let Err(error) = handle_panel_action(ix, guild_id, is_button, action).await {
        let message = discord::err_text(&error);
        tracing::error!("Music button error guild={guild_id} action={action}: {message}");
        if !ix.replied() && !ix.deferred() {
            ix.reply_ephemeral(format!("Music panel error: {message}"))
                .await?;
        } else {
            let _ = ix
                .follow_up(json!({ "content": format!("Music panel error: {message}"), "flags": FLAG_EPHEMERAL }))
                .await;
        }
    }
    Ok(true)
}

async fn handle_panel_action(
    ix: &Ix<'_>,
    guild_id: Id<GuildMarker>,
    is_button: bool,
    action: &str,
) -> Result<()> {
    let app = ix.app;
    let snapshot = app.music.snapshot(guild_id).await;
    let member_voice = member_voice_channel(ix);
    let restore = is_button && action == "restorequeue";
    if restore && member_voice.is_none() {
        return ix
            .reply_ephemeral("Join a voice channel to restore the queue.")
            .await;
    }
    if snapshot.is_none() && !restore {
        return ix.update(panel_payload(None)).await;
    }
    if let Some(s) = &snapshot
        && !same_voice(ix, s.voice_channel_id)
    {
        return ix
            .reply_ephemeral("You must be in the same voice channel as the bot.")
            .await;
    }
    if is_button && action == "lyrics" {
        let Some(title) = snapshot
            .as_ref()
            .and_then(|s| s.current_track.as_ref())
            .map(|t| t.title.clone())
        else {
            return ix.reply_ephemeral("No track to look up lyrics for.").await;
        };
        ix.defer(true).await?;
        let lyrics = match app.music.lyrics(guild_id).await {
            Ok(lyrics) => lyrics,
            Err(error) => {
                tracing::debug!("Lyrics lookup failed guild={guild_id}: {error}");
                None
            }
        };
        return ix
            .edit_content(lyrics_reply(&title, lyrics.as_deref()))
            .await;
    }
    if is_button {
        let text = match action {
            "queue" => Some(queue_preview(snapshot.as_ref())),

            "eq" => Some("Choose a preset from the EQ menu below.".to_owned()),
            "fx" => Some("Choose an effect from the FX menu below.".to_owned()),
            _ => None,
        };
        if let Some(text) = text {
            return ix.reply_ephemeral(text).await;
        }
    }

    ix.defer_update().await?;
    let value = ix.component_values().into_iter().next().unwrap_or_default();
    let mut follow_up = None;
    match (is_button, action) {
        (false, "eq") => {
            app.music.apply_eq_preset(guild_id, &value).await?;
        }
        (false, "fx") => {
            app.music.toggle_fx(guild_id, &value).await?;
        }
        (_, "loop") => {
            app.music.cycle_loop(guild_id).await?;
        }
        (_, "bassboost") => {
            app.music.toggle_bassboost(guild_id).await?;
        }
        (_, "volumedown") => {
            app.music.adjust_volume(guild_id, -10).await?;
        }
        (_, "volumeup") => {
            app.music.adjust_volume(guild_id, 10).await?;
        }
        (_, "mute") => {
            app.music.toggle_mute(guild_id).await?;
        }
        (_, "playpause") => {
            app.music.set_paused(guild_id, None).await?;
        }
        (_, "stop") => {
            app.music.stop(app, guild_id).await;
        }
        (_, "shuffle") => {
            app.music.shuffle(guild_id).await;
        }
        (_, "vaporwave") => {
            app.music.toggle_fx(guild_id, "vaporwave").await?;
        }
        (_, "autoplay") => {
            let enabled = app.music.toggle_autoplay(guild_id).await;
            follow_up = Some(format!(
                "Autoplay: **{}**",
                if enabled { "ON" } else { "OFF" }
            ));
        }
        (_, "savequeue") => {
            let saved = app.music.save_queue_state(guild_id).await?;
            follow_up = Some(format!("Saved the queue ({saved} tracks)."));
        }
        (_, "restorequeue") => {
            let voice = member_voice.ok_or_else(|| anyhow!("Missing guild or voice channel."))?;
            let restored = app.music.restore_queue_state(app, guild_id, voice).await?;
            follow_up = Some(format!("Restored the queue: {restored} tracks."));
        }
        _ => {}
    }
    let next = app.music.snapshot(guild_id).await;
    ix.edit(panel_payload(next.as_ref())).await?;
    if let Some(text) = follow_up {
        let _ = ix
            .follow_up(json!({ "content": text, "flags": FLAG_EPHEMERAL }))
            .await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lyrics_replies() {
        assert_eq!(
            lyrics_reply("Song", Some("line one\nline two")),
            "**Song**\nline one\nline two"
        );
        let long = "x".repeat(MAX_LYRICS_CHARS + 50);
        let reply = lyrics_reply("A B", Some(&long));
        assert!(reply.ends_with("… https://genius.com/search?q=A%20B"));
        assert!(reply.chars().count() <= 2000);
        assert_eq!(
            lyrics_reply("A B", None),
            "No lyrics found. Search: https://genius.com/search?q=A%20B"
        );
    }

    #[test]
    fn durations() {
        assert_eq!(format_duration(0), "?:??");
        assert_eq!(format_duration(65), "1:05");
        assert_eq!(format_duration(3725), "1:02:05");
    }

    #[test]
    fn empty_panel_is_disabled() {
        let panel = panel_payload(None);
        let row = &panel["components"][0]["components"][1]["components"][0];
        assert_eq!(row["disabled"], true);
        assert_eq!(row["custom_id"], "music_panel:loop:0");
        assert!(
            panel["components"][0]["components"][0]["content"]
                .as_str()
                .unwrap()
                .contains("Nothing playing")
        );
        assert_eq!(progress_bar(None, 4), "▱▱▱▱ 0%");
    }

    #[test]
    fn error_mapping() {
        assert!(
            play_error_text("Nothing found", "https://youtu.be/x").contains("YouTube rejected")
        );
        assert!(play_error_text("boom", "x").ends_with("boom"));
    }
}
