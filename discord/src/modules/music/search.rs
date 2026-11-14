#[allow(clippy::wildcard_imports)]
use super::*;

impl MusicManager {
    // --- searching -----------------------------------------------------------------------------

    pub(super) async fn search(
        &self,
        query: &str,
        source: Option<&str>,
        throw_on_empty: bool,
    ) -> Result<SearchResult> {
        lavalink::search(self.node()?, query, source, throw_on_empty).await
    }

    pub(super) fn mirror_candidates(&self, track: &Value) -> Vec<(String, String)> {
        let isrc = track_isrc(track);
        let seed = track_seed(track);
        let exact = compact_whitespace(&format!(
            "{} {}",
            compact_whitespace(info_str(track, "author")),
            compact_whitespace(info_str(track, "title"))
        ));
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for source in &self.spotify_mirror_sources {
            let source = source.trim().to_lowercase();
            if source.is_empty() {
                continue;
            }
            if source.ends_with("isrc") {
                if !isrc.is_empty() && seen.insert(format!("{source}|isrc|{isrc}")) {
                    out.push((source.clone(), isrc.clone()));
                }
                continue;
            }
            if (source == "dzsearch" || source == "scsearch")
                && !exact.is_empty()
                && seen.insert(format!("{source}|exact|{}", exact.to_lowercase()))
            {
                out.push((source.clone(), exact.clone()));
            }
            if !seed.is_empty() && seen.insert(format!("{source}|seed|{}", seed.to_lowercase())) {
                out.push((source.clone(), seed.clone()));
            }
        }
        out
    }

    pub(super) async fn resolve_spotify_mirror(&self, track: &Value) -> Option<Value> {
        for (source, query) in self.mirror_candidates(track) {
            match self.search(&query, Some(&source), false).await {
                Ok(result) => {
                    if let Some(mirrored) = pick_mirrored_track(&result.tracks, track) {
                        return Some(mirrored);
                    }
                }
                Err(error) => {
                    tracing::warn!("Spotify mirror lookup failed ({source}:{query}): {error}");
                }
            }
        }
        None
    }

    pub(super) async fn mirror_spotify_tracks(
        &self,
        tracks: Vec<Value>,
        playlist_mode: bool,
    ) -> Result<Vec<Value>> {
        let mut out = Vec::with_capacity(tracks.len());
        let mut replaced = 0;
        let mut dropped = 0;
        for track in tracks {
            if !is_spotify_track(&track) {
                out.push(track);
                continue;
            }
            if let Some(mirrored) = self.resolve_spotify_mirror(&track).await {
                out.push(mirrored);
                replaced += 1;
                continue;
            }
            if playlist_mode {
                dropped += 1;
                tracing::warn!(
                    "Spotify mirror dropped track (no valid candidate) title=\"{}\" author=\"{}\"",
                    or_unknown(info_str(&track, "title")),
                    or_unknown(info_str(&track, "author"))
                );
                continue;
            }
            bail!(
                "Could not mirror the Spotify track to a playable source. Try another track or retry in a moment."
            );
        }
        if replaced > 0 {
            tracing::info!("Spotify mirror replaced tracks: {replaced}");
        }
        if dropped > 0 {
            tracing::warn!("Spotify mirror dropped {dropped} track(s) from playlist");
        }
        Ok(out)
    }

    /// `resolveTracks`
    pub async fn resolve_tracks(&self, query: &str) -> Result<Resolved> {
        self.ensure_ready()?;
        let raw = compact_whitespace(query);
        if raw.is_empty() {
            bail!("Provide a link or a search phrase.");
        }
        let mut result: Option<SearchResult> = None;
        if !is_http_url(&raw) {
            result = Some(
                self.search(&raw, Some(&self.default_search_source), true)
                    .await?,
            );
        } else {
            let mut last_error = None;
            for (candidate, source) in youtube_resolve_candidates(&raw) {
                let source = source.unwrap_or_else(|| self.default_search_source.clone());
                match self.search(&candidate, Some(&source), true).await {
                    Ok(found) if !found.tracks.is_empty() => {
                        result = Some(found);
                        break;
                    }
                    Ok(_) => {}
                    Err(error) => last_error = Some(error),
                }
            }
            if result.is_none()
                && let Some(error) = last_error
            {
                return Err(error);
            }
        }
        let Some(result) = result.filter(|r| !r.tracks.is_empty()) else {
            bail!("No track found.");
        };
        let playlist = result.load_type == "playlist";
        let prepared = self
            .mirror_spotify_tracks(result.tracks.clone(), playlist)
            .await?;
        if prepared.is_empty() {
            bail!(if playlist {
                "Could not mirror the tracks of this Spotify playlist to playable sources."
            } else {
                "No playable version of the track was found."
            });
        }
        Ok(Resolved {
            playlist,
            playlist_title: playlist.then(|| playlist_title(&result)),
            tracks: prepared.iter().map(normalize_track).collect(),
        })
    }
}
