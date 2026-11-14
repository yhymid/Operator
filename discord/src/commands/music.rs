//! `/music` and the music panel components (port of `src/cmds/music.js`).

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

/// `90`, `1:30` or `1:02:03` → milliseconds.
pub fn parse_timestamp(raw: &str) -> Option<i64> {
    let parts: Vec<&str> = raw.trim().split(':').collect();
    if parts.is_empty() || parts.len() > 3 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let mut seconds: i64 = 0;
    for (index, part) in parts.iter().enumerate() {
        let value: i64 = part.parse().ok().filter(|v| *v >= 0)?;
        if index > 0 && value >= 60 {
            return None;
        }
        seconds = seconds.checked_mul(60)?.checked_add(value)?;
    }
    seconds.checked_mul(1000)
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
        Repeat::Track => "utwor",
        Repeat::Queue => "kolejka",
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
    let title = truncate_text(current.map_or("Nic nie gra", |t| t.title.as_str()), 80);
    let paused = snapshot.is_some_and(|s| s.paused);
    let status = if paused {
        "Pauza"
    } else if snapshot.is_some_and(|s| s.playing) {
        "Gra"
    } else {
        "Stop"
    };
    let queue = snapshot.map_or(&[][..], |s| s.queue.as_slice());
    let next_lines = if queue.is_empty() {
        "Brak następnych utworów.".to_owned()
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
        format!("Czas: **{}**", format_progress(snapshot)),
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
            "Kanał: {}",
            snapshot
                .and_then(|s| s.voice_channel_id)
                .map_or("brak".to_owned(), |c| format!("<#{c}>"))
        ),
        String::new(),
        "**Kolejka**".to_owned(),
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
                        select_option("Flat", "flat", "Neutralny dźwięk", eq_preset == "flat"),
                        select_option("Bassboost", "bassboost", "Mocniejszy dół", eq_preset == "bassboost"),
                        select_option("Vocal", "vocal", "Lepsza czytelność wokalu", eq_preset == "vocal"),
                        select_option("Treble", "treble", "Jaśniejsze wysokie tony", eq_preset == "treble"),
                        select_option("Nightcore", "nightcore", "Szybciej + wyższy pitch", eq_preset == "nightcore"),
                    ],
                    "disabled": disabled,
                }]},
                { "type": 1, "components": [{
                    "type": 3,
                    "custom_id": format!("{MUSIC_SELECT_PREFIX}:fx:{guild}"),
                    "placeholder": if active_fx.is_empty() { "FX: wybierz efekt".to_owned() } else { format!("FX: {}", active_fx.join(", ")) },
                    "options": [
                        select_option("Vaporwave", "vaporwave", "Wolniej i cieplej", false),
                        select_option("8D", "8d", "Efekt obrotu stereo", false),
                        select_option("Karaoke", "karaoke", "Próbuj stłumić wokal", false),
                        select_option("SoftLimiter", "softlimiter", "Łagodny low-pass", false),
                        select_option("FX Off", "off", "Wyłącz wszystkie FX", false),
                    ],
                    "disabled": disabled,
                }]},
            ],
        }],
    })
}

fn queue_preview(snapshot: Option<&Snapshot>) -> String {
    let Some(queue) = snapshot.map(|s| &s.queue).filter(|q| !q.is_empty()) else {
        return "Kolejka jest pusta.".to_owned();
    };
    let mut lines: Vec<String> = queue
        .iter()
        .take(10)
        .enumerate()
        .map(|(i, t)| format_track_line(t, Some(i + 1)))
        .collect();
    if queue.len() > lines.len() {
        lines.push(format!("... i jeszcze {}", queue.len() - lines.len()));
    }
    lines.join("\n")
}

fn lyrics_content(snapshot: Option<&Snapshot>) -> String {
    let Some(track) = snapshot.and_then(|s| s.current_track.as_ref()) else {
        return "Brak utworu do wyszukania tekstu.".to_owned();
    };
    let query: String = url::form_urlencoded::byte_serialize(track.title.as_bytes())
        .collect::<String>()
        .replace('+', "%20");
    format!("Tekst (wyszukiwanie): https://genius.com/search?q={query}")
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
        return Some("Musisz być na tym samym kanale głosowym co bot.");
    }
    if channel.is_none() && member_voice_channel(ix).is_none() {
        return Some("Dołącz do kanału głosowego, żeby sterować tą sesją.");
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
                "\"{}\" nie wygląda jak plik audio.",
                if a.filename.is_empty() {
                    "plik"
                } else {
                    a.filename.as_str()
                }
            );
        }
    }
    let query = opts.string("query").unwrap_or_default().trim().to_owned();
    match (attachment, query.is_empty()) {
        (None, true) => bail!("Podaj `query` albo plik `audio`."),
        (Some(_), false) => bail!("Wybierz jedno źródło: `query` albo `audio`."),
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

/// Pre-flight voice checks of `mode=play`; returns the reply text on failure.
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
        return Some("Ten kanał głosowy jest pełny.");
    }
    if app.cache.member(guild_id, app.bot_id).is_none() {
        return Some("Nie mogę sprawdzić uprawnień bota.");
    }
    let perms = discord::bot_channel_permissions(app, channel)?;
    if !discord::has(perms, Permissions::CONNECT) {
        return Some("Bot nie może dołączyć do tego kanału głosowego (joinable=false).");
    }
    if !discord::has(perms, Permissions::SPEAK) {
        return Some("Bot nie może nadawać na tym kanale głosowym (speakable=false).");
    }
    None
}

fn play_error_text(message: &str, requested: &str) -> String {
    let re = |p: &str| regex::Regex::new(p).expect("static regex");
    if re(r"(?i)no node|not connected|lavalink").is_match(message) {
        return "Błąd odtwarzania: Lavalink nie jest połączony albo nie obsługuje tego źródła. Sprawdź node i pluginy.".to_owned();
    }
    if re(r"(?i)got 429|too many requests|rate.?limit").is_match(message) {
        return "Błąd odtwarzania: źródło chwilowo blokuje zapytania (429/rate limit). Spróbuj ponownie za chwilę albo wybierz inne źródło.".to_owned();
    }
    if re(r"(?i)operation was aborted").is_match(message) {
        return "Błąd odtwarzania: The operation was aborted. Spróbuj ponownie za chwilę lub podaj bezpośredni link YouTube.".to_owned();
    }
    let nothing = re(r"(?i)nothing found").is_match(message);
    if nothing && re(r"(?i)(?:youtu\.be|youtube\.com)").is_match(requested) {
        return "Błąd odtwarzania: YouTube odrzucił ten link (wymaga logowania/anti-bot). Spróbuj innego linku, frazę wyszukiwania albo SoundCloud.".to_owned();
    }
    if nothing && re(r"(?i)cdn\.discordapp\.com/attachments/").is_match(requested) {
        return "Błąd odtwarzania: ten link do załącznika Discord jest niedostępny lub wygasł. Wyślij plik ponownie i użyj opcji `audio`.".to_owned();
    }
    if re(r"(?i)failed to retrieve secret from spotify|spotifytokentracker").is_match(message) {
        return "Błąd odtwarzania: Spotify playlist/albums tymczasowo nie odpowiadają po stronie API. Użyj linku do pojedynczego utworu Spotify albo frazy wyszukiwania.".to_owned();
    }
    format!("Błąd odtwarzania: {message}")
}

async fn execute_play(ix: &Ix<'_>, guild_id: Id<GuildMarker>) -> Result<()> {
    let app = ix.app;
    let Some(voice) = member_voice_channel(ix) else {
        return ix
            .reply_ephemeral("Najpierw dołącz do kanału głosowego.")
            .await;
    };
    if let Some(problem) = voice_join_problem(app, guild_id, voice) {
        return ix.reply_ephemeral(problem).await;
    }
    if discord::bot_channel_permissions(app, voice).is_none_or(|p| {
        !discord::has(p, Permissions::CONNECT) || !discord::has(p, Permissions::SPEAK)
    }) {
        return ix
            .reply_ephemeral("Bot nie ma uprawnień Connect i Speak na tym kanale.")
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
            operator_common::net::ensure_public_host(parsed.host_str().unwrap_or_default(), port)
                .await
                .map_err(|message| anyhow::anyhow!(message))?;
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
                    ix.edit_content("Rozpoznaję Spotify przez Lavalink. Przy dużych playlistach może to chwilę potrwać.")
                        .await?;
                }
                stage = "resolve";
                let resolved = app.music.resolve_tracks(&query).await?;
                tracing::info!(
                    "Music command play resolved guild={guild_id} user={user} type={} count={} firstUrl=\"{}\" firstTitle=\"{}\"",
                    if resolved.playlist { "playlist" } else { "single" },
                    resolved.tracks.len(),
                    resolved.tracks.first().map_or("", |t| t.url.as_str()),
                    resolved.tracks.first().map_or("", |t| t.title.as_str())
                );
                if resolved.tracks.is_empty() {
                    bail!("Nie znaleziono utworu.");
                }
                stage = "enqueue";
                if resolved.playlist {
                    let batch = app.music.enqueue_many(session, &resolved.tracks).await?;
                    session.created_player = false;
                    let title = resolved.playlist_title.unwrap_or_else(|| "Playlist".to_owned());
                    return Ok(match batch.started_track.filter(|_| batch.started) {
                        Some(track) => format!(
                            "Dodano playlistę \"{title}\" ({} utworów). Teraz gra: {}.",
                            batch.added,
                            format_track_line(&track, None)
                        ),
                        None => format!("Dodano playlistę \"{title}\" ({} utworów) do kolejki.", batch.added),
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
                    format!("Teraz gra: {}.", format_track_line(&result.track, None))
                } else {
                    format!(
                        "Dodano do kolejki (pozycja {}): {}.",
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
                    bail!("Nie udało się odtworzyć żadnego pliku audio.");
                }
                stage = "enqueue";
                let batch = app.music.enqueue_many(session, &tracks).await?;
                session.created_player = false;
                tracing::info!(
                    "Music command batch enqueued guild={guild_id} user={user} source=attachments added={} failed={failed}",
                    batch.added
                );
                let label = if batch.added == 1 { "plik audio" } else { "plików audio" };
                let skipped = if failed > 0 { format!(" Pominięte: {failed}.") } else { String::new() };
                Ok(match batch.started_track.filter(|_| batch.started) {
                    Some(track) => format!(
                        "Dodano {} {label}. Teraz gra: {}.{skipped}",
                        batch.added,
                        format_track_line(&track, None)
                    ),
                    None => format!("Dodano {} {label} do kolejki.{skipped}", batch.added),
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
                    .reply_ephemeral(format!("Błąd odtwarzania: {message}"))
                    .await;
            }
            ix.edit_content(play_error_text(&message, &requested)).await
        }
    }
}

pub async fn execute(ix: &Ix<'_>) -> Result<()> {
    let Some(guild_id) = ix.guild_id() else {
        return ix
            .reply_ephemeral("Ta komenda dziala tylko na serwerze.")
            .await;
    };
    let app = ix.app;
    let opts = ix.opts();
    let mode = opts.lower("mode");
    if mode == "play" {
        return execute_play(ix, guild_id).await;
    }
    let snapshot = app.music.snapshot(guild_id).await;
    let voice = snapshot.as_ref().and_then(|s| s.voice_channel_id);
    let no_session = || ix.reply_ephemeral("Brak aktywnej sesji muzyki.");

    match mode.as_str() {
        "queue" => {
            let Some(s) = snapshot.filter(|s| s.current_track.is_some() || !s.queue.is_empty())
            else {
                return ix.reply(json!({ "content": "Kolejka jest pusta." })).await;
            };
            let mut lines = vec![format!(
                "Kanał głosowy: {}",
                s.voice_channel_id
                    .map_or("<#null>".to_owned(), |c| format!("<#{c}>"))
            )];
            if let Some(track) = &s.current_track {
                lines.push(format!("Teraz gra: {}", format_track_line(track, None)));
            }
            if !s.queue.is_empty() {
                lines.push("Następne utwory:".to_owned());
                for (i, track) in s.queue.iter().take(10).enumerate() {
                    lines.push(format_track_line(track, Some(i + 1)));
                }
                if s.queue.len() > 10 {
                    lines.push(format!("... i jeszcze {}", s.queue.len() - 10));
                }
            }
            ix.reply(json!({ "content": lines.join("\n") })).await
        }
        "nowplaying" => {
            let Some(s) = snapshot.filter(|s| s.current_track.is_some() || !s.queue.is_empty())
            else {
                return ix.reply_ephemeral("Nic teraz nie gra.").await;
            };
            ix.reply(panel_payload(Some(&s))).await
        }
        "skip" => {
            if snapshot.as_ref().is_none_or(|s| s.current_track.is_none()) {
                return ix.reply_ephemeral("Nic nie gra.").await;
            }
            if !same_voice(ix, voice) {
                return ix
                    .reply_ephemeral("Musisz być na tym samym kanale głosowym co bot.")
                    .await;
            }
            match app.music.skip(guild_id).await? {
                Some(track) => {
                    ix.reply(json!({ "content": format!("Pominięto: {}.", format_track_line(&track, None)) }))
                        .await
                }
                None => ix.reply_ephemeral("Nic nie gra.").await,
            }
        }
        "stop" => {
            if snapshot.is_none() {
                return no_session().await;
            }
            if !same_voice(ix, voice) {
                return ix
                    .reply_ephemeral("Musisz być na tym samym kanale głosowym co bot.")
                    .await;
            }
            app.music.stop(app, guild_id).await;
            ix.reply(json!({ "content": "Zatrzymano muzykę i wyczyszczono kolejkę." }))
                .await
        }
        "pause" | "resume" => {
            let pause = mode == "pause";
            let Some(s) = snapshot.filter(|s| s.current_track.is_some()) else {
                return ix.reply_ephemeral("Nic nie gra.").await;
            };
            if let Some(error) = require_same_voice(ix, voice) {
                return ix.reply_ephemeral(error).await;
            }
            if pause && s.paused {
                return ix.reply_ephemeral("Muzyka jest już wstrzymana.").await;
            }
            if !pause && !s.paused {
                return ix.reply_ephemeral("Muzyka już gra.").await;
            }
            let next = app.music.set_paused(guild_id, Some(pause)).await?;
            match next.and_then(|n| n.current_track) {
                Some(track) => {
                    let verb = if pause { "Wstrzymano" } else { "Wznowiono" };
                    ix.reply(json!({ "content": format!("{verb}: {}.", format_track_line(&track, None)) }))
                        .await
                }
                None if pause => ix.reply_ephemeral("Nic nie gra.").await,
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
                    .reply_ephemeral("Dla mode=volume podaj `value`, np. `10` albo `-10`.")
                    .await;
            };
            match app.music.adjust_volume(guild_id, delta).await? {
                Some(next) => {
                    ix.reply(json!({ "content": format!(
                        "Głośność ustawiona na **{}%** (zmiana {}{delta}).",
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
                return ix
                    .reply_ephemeral("Dla mode=remove podaj `position` >= 1.")
                    .await;
            };
            if s.queue.is_empty() {
                return ix.reply_ephemeral("Kolejka jest pusta.").await;
            }
            if position as usize > s.queue.len() {
                return ix
                    .reply_ephemeral(format!(
                        "Nie ma utworu na pozycji {position}. Kolejka ma teraz {} pozycji.",
                        s.queue.len()
                    ))
                    .await;
            }
            match app.music.remove_from_queue(guild_id, position).await? {
                Some(track) => {
                    ix.reply(json!({ "content": format!(
                        "Usunięto z kolejki (pozycja {position}): {}.",
                        format_track_line(&track, None)
                    ) }))
                    .await
                }
                None => {
                    ix.reply_ephemeral(format!(
                        "Nie udało się usunąć pozycji {position} z kolejki."
                    ))
                    .await
                }
            }
        }
        "seek" => {
            let Some(s) = snapshot.filter(|s| s.current_track.is_some()) else {
                return ix.reply_ephemeral("Nic nie gra.").await;
            };
            if let Some(error) = require_same_voice(ix, voice) {
                return ix.reply_ephemeral(error).await;
            }
            let track = s.current_track.as_ref().expect("checked above");
            if track.raw["info"]["isStream"] == json!(true)
                || track.raw["info"]["isSeekable"] == json!(false)
            {
                return ix
                    .reply_ephemeral("Tego utworu nie da się przewinąć.")
                    .await;
            }
            let Some(position) = opts.string("time").as_deref().and_then(parse_timestamp) else {
                return ix
                    .reply_ephemeral("Dla mode=seek podaj `time`, np. `1:30` albo `90`.")
                    .await;
            };
            match app.music.seek(guild_id, position).await? {
                Some(next) => {
                    ix.reply(json!({ "content": format!(
                        "Przewinięto do {} / {}.",
                        format_duration(next.position_sec),
                        format_duration(track.duration_sec)
                    ) }))
                    .await
                }
                None => ix.reply_ephemeral("Nic nie gra.").await,
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
                opts.integer("position").filter(|p| *p > 0),
                opts.integer("to").filter(|p| *p > 0),
            ) else {
                return ix
                    .reply_ephemeral(
                        "Dla mode=move podaj `position` (skąd) i `to` (dokąd), oba >= 1.",
                    )
                    .await;
            };
            let len = s.queue.len() as i64;
            if from > len || to > len {
                return ix
                    .reply_ephemeral(format!("Kolejka ma teraz {len} pozycji."))
                    .await;
            }
            match app
                .music
                .move_in_queue(guild_id, from as usize, to as usize)
                .await
            {
                Some(track) => {
                    ix.reply(json!({ "content": format!(
                        "Przeniesiono z pozycji {from} na {to}: {}.",
                        format_track_line(&track, None)
                    ) }))
                    .await
                }
                None => ix.reply_ephemeral("Nie udało się przenieść utworu.").await,
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
                return ix.reply_ephemeral("Kolejka jest już pusta.").await;
            }
            let removed = app.music.clear_queue(guild_id).await.unwrap_or(0);
            ix.reply(json!({ "content": format!(
                "Wyczyszczono kolejkę. Usunięto **{removed}** oczekujących utworów."
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
                    None => ix.edit_content("Brak aktywnej sesji muzyki.").await,
                    Some(r) => {
                        ix.edit_content(format!(
                            "Reconnect zakończony. Kanał: <#{}>. Node: **{}**.",
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
                        .reply_ephemeral(format!("Reconnect nie powiódł się: {message}"))
                        .await;
                }
                return ix
                    .edit_content(format!("Reconnect nie powiódł się: {message}"))
                    .await;
            }
            Ok(())
        }
        _ => {
            ix.reply_ephemeral(format!("Nieznany mode music: {mode}."))
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
        ix.reply_ephemeral("Ten panel nie pasuje do tego serwera.")
            .await?;
        return Ok(true);
    };
    if let Err(error) = handle_panel_action(ix, guild_id, is_button, action).await {
        let message = discord::err_text(&error);
        tracing::error!("Music button error guild={guild_id} action={action}: {message}");
        if !ix.replied() && !ix.deferred() {
            ix.reply_ephemeral(format!("Błąd panelu muzyki: {message}"))
                .await?;
        } else {
            let _ = ix
                .follow_up(json!({ "content": format!("Błąd panelu muzyki: {message}"), "flags": FLAG_EPHEMERAL }))
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
            .reply_ephemeral("Dołącz do kanału głosowego, żeby przywrócić kolejkę.")
            .await;
    }
    if snapshot.is_none() && !restore {
        return ix.update(panel_payload(None)).await;
    }
    if let Some(s) = &snapshot
        && !same_voice(ix, s.voice_channel_id)
    {
        return ix
            .reply_ephemeral("Musisz być na tym samym kanale głosowym co bot.")
            .await;
    }
    if is_button {
        let text = match action {
            "queue" => Some(queue_preview(snapshot.as_ref())),
            "lyrics" => Some(lyrics_content(snapshot.as_ref())),
            "eq" => Some("Wybierz preset z menu EQ poniżej.".to_owned()),
            "fx" => Some("Wybierz efekt z menu FX poniżej.".to_owned()),
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
            follow_up = Some(format!("Zapisano kolejkę ({saved} utworów)."));
        }
        (_, "restorequeue") => {
            let voice = member_voice.ok_or_else(|| anyhow!("Brak guild albo voice channel."))?;
            let restored = app.music.restore_queue_state(app, guild_id, voice).await?;
            follow_up = Some(format!("Przywrócono kolejkę: {restored} utworów."));
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
    fn timestamps() {
        assert_eq!(parse_timestamp("90"), Some(90_000));
        assert_eq!(parse_timestamp(" 1:30 "), Some(90_000));
        assert_eq!(parse_timestamp("1:02:03"), Some(3_723_000));
        for bad in ["", "1:", "1:60", "-5", "a:10", "1:2:3:4"] {
            assert_eq!(parse_timestamp(bad), None, "{bad}");
        }
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
                .contains("Nic nie gra")
        );
        assert_eq!(progress_bar(None, 4), "▱▱▱▱ 0%");
    }

    #[test]
    fn error_mapping() {
        assert!(
            play_error_text("Nothing found", "https://youtu.be/x").contains("YouTube odrzucił")
        );
        assert!(play_error_text("boom", "x").ends_with("boom"));
    }
}
