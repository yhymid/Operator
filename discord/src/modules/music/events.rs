#[allow(clippy::wildcard_imports)]
use super::*;

impl MusicManager {
    // --- node events ---------------------------------------------------------------------------

    pub(super) fn node_id(&self) -> &str {
        self.node
            .as_ref()
            .map_or("unknown", |n| n.config.id.as_str())
    }

    pub(super) async fn on_node_event(&self, app: &Arc<App>, event: NodeEvent) {
        match event {
            NodeEvent::Ready {
                session_id,
                resumed,
            } => {
                tracing::info!("Music provider ready: Lavalink node {}", self.node_id());
                tracing::debug!("Lavalink session={session_id} resumed={resumed}");
                self.refresh_status();
            }
            NodeEvent::Disconnected { reason } => {
                tracing::warn!(
                    "Lavalink node disconnected ({}): {}",
                    self.node_id(),
                    if reason.is_empty() {
                        "unknown"
                    } else {
                        &reason
                    }
                );
                for player in self.players.lock().await.values_mut() {
                    player.connected = false;
                }
            }
            NodeEvent::PlayerUpdate { guild_id, state } => {
                let Some(guild_id) = crate::discord::parse_id::<GuildMarker>(&guild_id) else {
                    return;
                };
                if let Some(player) = self.players.lock().await.get_mut(&guild_id) {
                    player.last_position =
                        state["position"].as_i64().unwrap_or(player.last_position);
                    player.last_position_at = Some(Instant::now());
                    player.connected = state["connected"].as_bool().unwrap_or(false);
                    player.ping = state["ping"].as_i64().filter(|p| *p >= 0);
                }
            }
            NodeEvent::Event { guild_id, payload } => {
                let Some(guild_id) = crate::discord::parse_id::<GuildMarker>(&guild_id) else {
                    return;
                };
                self.on_track_event(app, guild_id, payload).await;
            }
        }
    }

    pub(super) async fn on_track_event(
        &self,
        app: &Arc<App>,
        guild_id: Id<GuildMarker>,
        payload: Value,
    ) {
        let track = payload["track"].clone();
        let title = or_unknown(info_str(&track, "title")).to_owned();
        match payload["type"].as_str().unwrap_or_default() {
            "TrackStartEvent" => {
                if let Some(player) = self.players.lock().await.get_mut(&guild_id) {
                    player.idle_generation += 1;
                    player.recovery.remove(&recovery_key(&track));
                    player.playing = true;
                }
                tracing::info!("Music track start guild={guild_id} title=\"{title}\"");
                self.refresh_status();
            }
            "TrackEndEvent" => {
                let reason = payload["reason"].as_str().unwrap_or_default();
                self.on_track_end(app, guild_id, track, reason).await;
            }
            "TrackExceptionEvent" => {
                let message = payload["exception"]["message"]
                    .as_str()
                    .unwrap_or("unknown");
                tracing::warn!("Music track error guild={guild_id} title=\"{title}\": {message}");
                let too_many = self
                    .players
                    .lock()
                    .await
                    .get_mut(&guild_id)
                    .is_some_and(|player| {
                        let window = Duration::from_millis(self.error_threshold_ms.max(0.0) as u64);
                        player.errors.retain(|at| at.elapsed() < window);
                        player.errors.push_back(Instant::now());
                        self.error_max_amount > 0.0
                            && player.errors.len() as f64 >= self.error_max_amount
                    });
                if too_many {
                    self.destroy(app, guild_id, "TrackErrorMaxTracksErroredPerTime")
                        .await;
                    return;
                }
                let app = app.clone();
                tokio::spawn(async move {
                    if let Err(error) = app.music.recover_track(guild_id, &track, &payload).await {
                        tracing::warn!("Music recovery error guild={guild_id}: {error}");
                    }
                });
            }
            "TrackStuckEvent" => {
                tracing::warn!("Music track stuck guild={guild_id} title=\"{title}\"");
                let mut players = self.players.lock().await;
                let Some(player) = players.get_mut(&guild_id) else {
                    return;
                };
                let Ok(node) = self.node() else { return };
                if player.tracks.is_empty() && player.repeat == Repeat::Off {
                    let body = json!({ "track": { "encoded": null } });
                    if node
                        .update_player(&guild_id.to_string(), &body, false)
                        .await
                        .is_ok()
                    {
                        return;
                    }
                    drop(players);
                    self.on_queue_end(app, guild_id, Some(track)).await;
                    return;
                }
                player.queue_track_end();
                if player.current.is_none() {
                    drop(players);
                    self.on_queue_end(app, guild_id, Some(track)).await;
                } else if let Err(error) = self.play_current(player, false).await {
                    tracing::warn!("Music play after stuck failed guild={guild_id}: {error}");
                }
            }
            "WebSocketClosedEvent" => {
                tracing::warn!(
                    "Lavalink voice websocket closed guild={guild_id} code={} reason=\"{}\"",
                    payload["code"],
                    payload["reason"].as_str().unwrap_or_default()
                );
            }
            _ => {}
        }
    }

    pub(super) async fn on_track_end(
        &self,
        app: &Arc<App>,
        guild_id: Id<GuildMarker>,
        track: Value,
        reason: &str,
    ) {
        if reason == "replaced" {
            return;
        }
        let mut players = self.players.lock().await;
        let Some(player) = players.get_mut(&guild_id) else {
            return;
        };
        if player.destroying {
            return;
        }
        if reason == "loadFailed"
            || reason == "cleanup"
            || player.repeat != Repeat::Track
            || player.skipped
        {
            player.queue_track_end();
        }
        player.skipped = false;
        if player.current.is_none() {
            drop(players);
            self.on_queue_end(app, guild_id, Some(track)).await;
            return;
        }
        if let Err(error) = self.play_current(player, true).await {
            tracing::warn!("Music autoskip play failed guild={guild_id}: {error}");
        }
    }

    /// `queueEnd` of lavalink-client + the bot's autoplay function and idle timer.
    pub(super) async fn on_queue_end(
        &self,
        app: &Arc<App>,
        guild_id: Id<GuildMarker>,
        last_track: Option<Value>,
    ) {
        let autoplay = {
            let mut players = self.players.lock().await;
            let Some(player) = players.get_mut(&guild_id) else {
                return;
            };
            player.current = None;
            player.playing = false;
            player.autoplay
        };
        if autoplay && let Some(track) = &last_track {
            self.handle_autoplay(guild_id, track).await;
            let mut players = self.players.lock().await;
            if let Some(player) = players.get_mut(&guild_id)
                && player.current.is_none()
                && !player.tracks.is_empty()
            {
                player.queue_track_end();
                match self.play_current(player, true).await {
                    Ok(()) => return,
                    Err(error) => tracing::warn!("Autoplay play failed guild={guild_id}: {error}"),
                }
            }
        }
        if let Some(track) = last_track
            && let Some(player) = self.players.lock().await.get_mut(&guild_id)
        {
            player.previous.push_front(track);
            player.previous.truncate(MAX_PREVIOUS_TRACKS);
        }
        self.schedule_idle_destroy(app, guild_id).await;
        self.refresh_status();
    }

    pub(super) async fn handle_autoplay(&self, guild_id: Id<GuildMarker>, track: &Value) {
        let seed = compact_whitespace(&format!(
            "{} {}",
            info_str(track, "author"),
            info_str(track, "title")
        ));
        if seed.is_empty() {
            return;
        }
        let result = match self
            .search(&seed, Some(&self.default_search_source), false)
            .await
        {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!("Autoplay error guild={guild_id}: {error}");
                return;
            }
        };
        let current_uri = info_str(track, "uri");
        let Some(next) = result
            .tracks
            .iter()
            .find(|c| info_str(c, "uri") != current_uri)
            .or_else(|| result.tracks.first())
            .cloned()
        else {
            return;
        };
        let title = or_unknown(info_str(&next, "title")).to_owned();
        if let Some(player) = self.players.lock().await.get_mut(&guild_id) {
            player.tracks.push_back(next);
            tracing::info!("Autoplay added a track guild={guild_id} title=\"{title}\"");
        }
    }

    pub(super) async fn schedule_idle_destroy(&self, app: &Arc<App>, guild_id: Id<GuildMarker>) {
        let generation = {
            let mut players = self.players.lock().await;
            let Some(player) = players.get_mut(&guild_id) else {
                return;
            };
            player.idle_generation += 1;
            player.idle_generation
        };
        let delay = Duration::from_millis(self.idle_disconnect_ms.max(0.0) as u64);
        let app = app.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let idle = app
                .music
                .players
                .lock()
                .await
                .get(&guild_id)
                .is_some_and(|p| {
                    p.idle_generation == generation
                        && !p.playing
                        && p.current.is_none()
                        && p.tracks.is_empty()
                });
            if idle {
                app.music
                    .destroy(&app, guild_id, "empty queue idle timeout")
                    .await;
            }
        });
    }

    /// `handleTrackErrorRecovery`
    pub(super) async fn recover_track(
        &self,
        guild_id: Id<GuildMarker>,
        track: &Value,
        payload: &Value,
    ) -> Result<()> {
        let text = format!(
            "{} {}",
            payload["exception"]["message"].as_str().unwrap_or_default(),
            payload["exception"]["cause"].as_str().unwrap_or_default()
        );
        if !recoverable_error_regex().is_match(&text)
            || !info_str(track, "sourceName").eq_ignore_ascii_case("spotify")
        {
            return Ok(());
        }
        {
            let mut players = self.players.lock().await;
            let Some(player) = players.get_mut(&guild_id) else {
                return Ok(());
            };
            let key = recovery_key(track);
            if player
                .recovery
                .get(&key)
                .is_some_and(|at| at.elapsed() < TRACK_RECOVERY_COOLDOWN)
            {
                return Ok(());
            }
            player.recovery.insert(key, Instant::now());
        }
        let seed = track_seed(track);
        let isrc = track_isrc(track);
        if seed.is_empty() {
            return Ok(());
        }
        let title = compact_whitespace(info_str(track, "title"));
        let author = compact_whitespace(info_str(track, "author"));
        for source in &self.recovery_sources {
            let isrc_source = source.to_lowercase().ends_with("isrc");
            let mut queries = Vec::new();
            if isrc_source {
                if !isrc.is_empty() {
                    queries.push(isrc.clone());
                }
            } else if source == "scsearch" {
                if !title.is_empty() && !author.is_empty() {
                    queries.push(format!("\"{author}\" \"{title}\""));
                }
            } else if source == "dzsearch" && !title.is_empty() && !author.is_empty() {
                queries.push(format!("{author} {title}"));
            }
            if !isrc_source {
                queries.push(seed.clone());
            }
            for query in queries.into_iter().filter(|q| !q.is_empty()) {
                let result = match self.search(&query, Some(source), false).await {
                    Ok(result) => result,
                    Err(error) => {
                        tracing::warn!(
                            "Music recovery lookup failed guild={guild_id} source={source} query=\"{query}\": {error}"
                        );
                        continue;
                    }
                };
                let Some(candidate) = pick_mirrored_track(&result.tracks, track) else {
                    continue;
                };
                let candidate_title = or_unknown(info_str(&candidate, "title")).to_owned();
                let mut players = self.players.lock().await;
                let Some(player) = players.get_mut(&guild_id) else {
                    return Ok(());
                };
                player.tracks.push_front(candidate);
                if !player.playing && player.current.is_none() {
                    self.play_current(player, false).await?;
                }
                tracing::warn!(
                    "Music recovery queued fallback guild={guild_id} source={source} scoreQuery=\"{query}\" title=\"{candidate_title}\""
                );
                return Ok(());
            }
        }
        Ok(())
    }
}
