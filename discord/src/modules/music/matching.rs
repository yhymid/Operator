//! Picking a playable mirror (YouTube/SoundCloud) for Spotify tracks.

#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) const SPOTIFY_PRIMARY_SOURCE_NAMES: [&str; 2] = ["youtube", "youtubemusic"];
pub(super) const SPOTIFY_FALLBACK_SOURCE_NAMES: [&str; 1] = ["soundcloud"];
pub(super) const SPOTIFY_MIRROR_MIN_SCORE: i64 = 70;
pub(super) const SPOTIFY_MIRROR_MIN_TITLE_OVERLAP: f64 = 0.46;
pub(super) const SPOTIFY_MIRROR_MIN_AUTHOR_OVERLAP: f64 = 0.24;
pub(super) const SPOTIFY_MIRROR_MAX_DURATION_DIFF_SEC: i64 = 22;
pub(super) const SPOTIFY_MIRROR_MAX_DURATION_DIFF_RATIO: f64 = 0.14;
pub(super) const MATCH_VARIANT_MARKERS: [&str; 14] = [
    "tribute",
    "cover",
    "karaoke",
    "remix",
    "bootleg",
    "edit",
    "mashup",
    "slowed",
    "sped",
    "nightcore",
    "instrumental",
    "8d",
    "reverb",
    "live",
];

/// `(query, source)` candidates; `source = None` means "default source / raw URL".
pub(super) fn youtube_resolve_candidates(raw: &str) -> Vec<(String, Option<String>)> {
    let raw = raw.trim().to_owned();
    let Some(id) = extract_youtube_video_id(&raw) else {
        return vec![(raw, None)];
    };
    let candidates = vec![
        (raw, None),
        (format!("https://www.youtube.com/watch?v={id}"), None),
        (id.clone(), Some("ytsearch".to_owned())),
        (id, Some("ytmsearch".to_owned())),
    ];
    let mut seen = HashSet::new();
    candidates
        .into_iter()
        .filter(|c| seen.insert(c.clone()))
        .collect()
}

pub(super) fn parse_csv_list(value: Option<String>, fallback: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in value
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
    {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    if out.is_empty() {
        fallback.iter().map(|s| (*s).to_owned()).collect()
    } else {
        out
    }
}

pub(super) fn info_str<'a>(track: &'a Value, key: &str) -> &'a str {
    track["info"][key].as_str().unwrap_or_default()
}

pub(super) fn or_unknown(value: &str) -> &str {
    if value.is_empty() { "unknown" } else { value }
}

pub(super) fn duration_ms(track: &Value) -> f64 {
    track["info"]["length"]
        .as_f64()
        .or_else(|| track["info"]["duration"].as_f64())
        .unwrap_or(0.0)
}

pub(super) fn js_round(value: f64) -> i64 {
    (value + 0.5).floor() as i64
}

pub(super) fn duration_sec(track: &Value) -> i64 {
    js_round(duration_ms(track) / 1000.0)
}

pub fn is_spotify_track(track: &Value) -> bool {
    info_str(track, "sourceName").eq_ignore_ascii_case("spotify")
        || info_str(track, "uri")
            .to_lowercase()
            .contains("open.spotify.com/")
}

pub(super) fn track_isrc(track: &Value) -> String {
    compact_whitespace(info_str(track, "isrc"))
}

pub(super) fn track_seed(track: &Value) -> String {
    let author = info_str(track, "author");
    let author = if author.eq_ignore_ascii_case("unknown artist") {
        ""
    } else {
        author
    };
    compact_whitespace(&format!("{author} {}", info_str(track, "title")))
}

pub fn normalize_for_match(value: &str) -> String {
    let lowered = value.to_lowercase();
    let stripped: String = lowered
        .nfkd()
        .filter(|c| !('\u{0300}'..='\u{036f}').contains(c))
        .collect();
    let mut out = String::new();
    let mut gap = false;
    for c in stripped.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            if gap && !out.is_empty() {
                out.push(' ');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}

pub(super) fn tokens(value: &str) -> Vec<String> {
    normalize_for_match(value)
        .split(' ')
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

pub(super) fn token_overlap_ratio(left: &str, right: &str) -> f64 {
    let left_tokens = tokens(left);
    let right_tokens: HashSet<String> = tokens(right).into_iter().collect();
    if left_tokens.is_empty() || right_tokens.is_empty() {
        return 0.0;
    }
    let matches = left_tokens
        .iter()
        .filter(|t| right_tokens.contains(*t))
        .count();
    matches as f64 / left_tokens.len() as f64
}

pub(super) fn has_variant_marker(value: &str) -> bool {
    let normalized = normalize_for_match(value);
    !normalized.is_empty() && MATCH_VARIANT_MARKERS.iter().any(|m| normalized.contains(m))
}

pub(super) fn is_likely_preview_length(original: i64, candidate: i64) -> bool {
    original > 0 && candidate > 0 && original >= 120 && (25..=70).contains(&candidate)
}

pub(super) fn is_preview_track(track: &Value) -> bool {
    track["pluginInfo"]["isPreview"] == Value::Bool(true)
        || track["info"]["isPreview"] == Value::Bool(true)
}

pub(super) fn is_mirror_candidate_source(source: &str) -> bool {
    let source = source.to_lowercase();
    SPOTIFY_PRIMARY_SOURCE_NAMES.contains(&source.as_str())
        || SPOTIFY_FALLBACK_SOURCE_NAMES.contains(&source.as_str())
}

pub fn score_mirrored_candidate(original: &Value, candidate: &Value) -> i64 {
    if candidate["info"].is_null() || is_spotify_track(candidate) {
        return -100_000;
    }
    if candidate["info"]["isStream"] == Value::Bool(true) {
        return -5000;
    }
    if is_preview_track(candidate) {
        return -100_000;
    }
    let original_title = info_str(original, "title");
    let original_author = info_str(original, "author");
    let candidate_title = info_str(candidate, "title");
    let candidate_author = info_str(candidate, "author");
    let original_duration = duration_sec(original);
    let candidate_duration = duration_sec(candidate);

    let title_overlap = token_overlap_ratio(original_title, candidate_title);
    let author_overlap = token_overlap_ratio(original_author, candidate_author);
    let mut score = js_round(title_overlap * 65.0) + js_round(author_overlap * 45.0);

    let normalized_original = normalize_for_match(original_title);
    let normalized_candidate = normalize_for_match(candidate_title);
    if !normalized_original.is_empty() && normalized_original == normalized_candidate {
        score += 30;
    }
    if !normalized_original.is_empty() && normalized_candidate.contains(&normalized_original) {
        score += 10;
    }

    if original_duration > 0 && candidate_duration > 0 {
        if is_likely_preview_length(original_duration, candidate_duration) {
            return -10_000;
        }
        let diff = (original_duration - candidate_duration).abs();
        score += match diff {
            0..=2 => 25,
            3..=5 => 20,
            6..=10 => 14,
            11..=20 => 8,
            21..=35 => 3,
            36..=55 => -8,
            _ => -20,
        };
    }

    if !has_variant_marker(&format!("{original_title} {original_author}"))
        && has_variant_marker(&format!("{candidate_title} {candidate_author}"))
    {
        score -= 30;
    }

    match info_str(candidate, "sourceName").to_lowercase().as_str() {
        "deezer" => score += 14,
        "soundcloud" => score += 3,
        "youtube" | "youtubemusic" => score -= 8,
        _ => {}
    }
    if author_overlap < 0.25 && !original_author.is_empty() {
        score -= 15;
    }
    if title_overlap < 0.35 {
        score -= 20;
    }
    if !original_author.is_empty() && author_overlap < 0.2 {
        score -= 25;
    }
    if title_overlap < 0.25 {
        score -= 30;
    }
    score
}

pub(super) fn passes_spotify_threshold(original: &Value, candidate: &Value, score: i64) -> bool {
    let title_overlap =
        token_overlap_ratio(info_str(original, "title"), info_str(candidate, "title"));
    let author_overlap =
        token_overlap_ratio(info_str(original, "author"), info_str(candidate, "author"));
    let original_duration = duration_sec(original);
    let candidate_duration = duration_sec(candidate);
    let diff = (original_duration - candidate_duration).abs();
    let ratio = if original_duration > 0 {
        diff as f64 / original_duration as f64
    } else {
        1.0
    };
    score >= SPOTIFY_MIRROR_MIN_SCORE
        && title_overlap >= SPOTIFY_MIRROR_MIN_TITLE_OVERLAP
        && (info_str(original, "author").is_empty()
            || author_overlap >= SPOTIFY_MIRROR_MIN_AUTHOR_OVERLAP)
        && original_duration > 0
        && candidate_duration > 0
        && diff <= SPOTIFY_MIRROR_MAX_DURATION_DIFF_SEC
        && ratio <= SPOTIFY_MIRROR_MAX_DURATION_DIFF_RATIO
}

pub fn pick_mirrored_track(tracks: &[Value], original: &Value) -> Option<Value> {
    let spotify_uri = info_str(original, "uri");
    let original_is_spotify = is_spotify_track(original);
    let mut ranked: Vec<(i64, usize)> = tracks
        .iter()
        .enumerate()
        .filter(|(_, c)| !c["info"].is_null() && !is_spotify_track(c))
        .filter(|(_, c)| !is_preview_track(c))
        .filter(|(_, c)| spotify_uri.is_empty() || info_str(c, "uri") != spotify_uri)
        .filter(|(_, c)| {
            !original_is_spotify || is_mirror_candidate_source(info_str(c, "sourceName"))
        })
        .map(|(index, c)| (score_mirrored_candidate(original, c), index))
        .collect();
    // Stable sort, descending (Array.prototype.sort is stable as well).
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    let (score, index) = *ranked.first()?;
    let best = &tracks[index];
    if original_is_spotify && !passes_spotify_threshold(original, best, score) {
        return None;
    }
    if !original_is_spotify && score < 55 {
        return None;
    }
    Some(best.clone())
}
