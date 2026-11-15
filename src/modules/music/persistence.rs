#[allow(clippy::wildcard_imports)]
use super::*;

impl MusicManager {
    // --- saved queues --------------------------------------------------------------------------

    pub(super) fn queue_state_path(&self, guild_id: Id<GuildMarker>) -> PathBuf {
        self.queue_state_dir.join(format!("{guild_id}.json"))
    }

    /// Returns the number of saved tracks.
    pub async fn save_queue_state(&self, guild_id: Id<GuildMarker>) -> Result<usize> {
        let snapshot = self.snapshot(guild_id).await;
        let Some(snapshot) = snapshot.filter(|s| s.current_track.is_some()) else {
            bail!("No active queue to save.");
        };
        let tracks: Vec<Value> = snapshot
            .current_track
            .iter()
            .chain(snapshot.queue.iter())
            .filter(|t| !t.url.is_empty())
            .map(|t| json!({ "url": t.url, "title": t.title }))
            .collect();
        if tracks.is_empty() {
            bail!("No tracks with a link to save.");
        }
        let count = tracks.len();
        let payload = json!({
            "guildId": snapshot.guild_id.to_string(),
            "voiceChannelId": snapshot.voice_channel_id.map(|c| c.to_string()).unwrap_or_default(),
            "savedAt": chrono::Utc::now().timestamp_millis(),
            "volume": if snapshot.volume != 0 { snapshot.volume } else { self.default_volume },
            "repeatMode": snapshot.repeat_mode.as_str(),
            "autoplay": snapshot.autoplay,
            "tracks": tracks,
        });
        // JSON.stringify(payload, null, 2): two-space indent, no trailing newline.
        let bytes = serde_json::to_vec_pretty(&payload)?;
        std::fs::create_dir_all(&self.queue_state_dir)?;
        crate::common::fs::write_atomic(&self.queue_state_path(guild_id), &bytes)?;
        Ok(count)
    }

    pub(super) fn load_saved_queue_state(&self, guild_id: Id<GuildMarker>) -> Result<Value> {
        let path = self.queue_state_path(guild_id);
        if !path.exists() {
            bail!("No saved queue state.");
        }
        let mut parsed: Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        let tracks: Vec<Value> = parsed["tracks"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|e| e["url"].as_str().is_some_and(|u| !u.trim().is_empty()))
            .cloned()
            .collect();
        if tracks.is_empty() {
            bail!("The saved queue is empty.");
        }
        parsed["tracks"] = Value::Array(tracks);
        Ok(parsed)
    }

    /// Returns the number of restored tracks.
    pub async fn restore_queue_state(
        &self,
        app: &App,
        guild_id: Id<GuildMarker>,
        voice_channel_id: Id<ChannelMarker>,
    ) -> Result<usize> {
        let state = self.load_saved_queue_state(guild_id)?;
        let session = self.ensure_session(app, guild_id, voice_channel_id).await?;
        let mut restored = Vec::new();
        for entry in state["tracks"].as_array().into_iter().flatten() {
            let url = entry["url"].as_str().unwrap_or_default();
            match self.resolve_tracks(url).await {
                Ok(resolved) => restored.extend(resolved.tracks.into_iter().next()),
                Err(error) => tracing::warn!(
                    "Restore queue skip guild={guild_id} url=\"{url}\" error={error}"
                ),
            }
        }
        if restored.is_empty() {
            bail!("Could not play any track from the saved queue.");
        }
        self.enqueue_many(&session, &restored).await?;
        let volume = state["volume"]
            .as_f64()
            .filter(|v| *v != 0.0)
            .map_or(self.default_volume, js_round)
            .clamp(1, 150);
        let mut players = self.players.lock().await;
        if let Some(player) = players.get_mut(&guild_id) {
            self.node()?
                .update_player(&guild_id.to_string(), &json!({ "volume": volume }), false)
                .await?;
            player.volume = volume;
            player.repeat = Repeat::parse(state["repeatMode"].as_str().unwrap_or_default());
            player.autoplay = state["autoplay"] == Value::Bool(true);
        }
        Ok(restored.len())
    }

    // --- continue after a bot restart ---------------------------------------------------------

    fn restart_state_path(&self, guild_id: Id<GuildMarker>) -> PathBuf {
        self.queue_state_dir.join(format!("{guild_id}.resume.json"))
    }

    /// Writes every active session (track objects, position, settings) so the next start can
    /// continue it. Separate from the queue a user saves with the panel button.
    async fn save_for_restart(&self) {
        let players = self.players.lock().await;
        for player in players.values().filter(|p| p.has_music()) {
            let (Some(current), Some(channel)) = (&player.current, player.voice_channel_id) else {
                continue;
            };
            let payload = json!({
                "savedAt": chrono::Utc::now().timestamp_millis(),
                "voiceChannelId": channel.to_string(),
                "volume": player.volume,
                "repeatMode": player.repeat.as_str(),
                "autoplay": player.autoplay,
                "paused": player.paused,
                "positionMs": player.position_ms(),
                "current": current,
                "tracks": player.tracks,
            });
            let write = || -> Result<()> {
                std::fs::create_dir_all(&self.queue_state_dir)?;
                let bytes = serde_json::to_vec(&payload)?;
                crate::common::fs::write_atomic(&self.restart_state_path(player.guild_id), &bytes)?;
                Ok(())
            };
            let result = write();
            if let Err(error) = result {
                tracing::warn!(
                    "Music: could not save the session of guild={} for the restart: {error}",
                    player.guild_id
                );
            }
        }
    }

    /// First node connection after a start: continues the sessions saved by the previous
    /// shutdown, when it was recent and someone is still in the voice channel.
    pub(super) async fn resume_after_restart(&self, app: &Arc<App>) {
        static DONE: AtomicBool = AtomicBool::new(false);
        if DONE.swap(true, Ordering::SeqCst) {
            return;
        }
        let Ok(entries) = std::fs::read_dir(&self.queue_state_dir) else {
            return;
        };
        for path in entries.flatten().map(|e| e.path()) {
            let Some(guild_id) = path
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".resume.json"))
                .and_then(crate::discord::parse_id::<GuildMarker>)
            else {
                continue;
            };
            let state = std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
            // One attempt only: a state that fails must not be retried on every start.
            let _ = std::fs::remove_file(&path);
            let Some(state) = state else { continue };
            match self.continue_session(app, guild_id, &state).await {
                Ok(true) => tracing::info!(
                    "Music: continued the session of guild={guild_id} after the restart"
                ),
                Ok(false) => {}
                Err(error) => tracing::warn!(
                    "Music: could not continue the session of guild={guild_id}: {error}"
                ),
            }
        }
    }

    async fn continue_session(
        &self,
        app: &Arc<App>,
        guild_id: Id<GuildMarker>,
        state: &Value,
    ) -> Result<bool> {
        let age_ms = chrono::Utc::now().timestamp_millis() - state["savedAt"].as_i64().unwrap_or(0);
        if !(0..=RESTART_RESUME_MAX_AGE_MS).contains(&age_ms) {
            return Ok(false);
        }
        let Some(channel) = state["voiceChannelId"]
            .as_str()
            .and_then(crate::discord::parse_id::<ChannelMarker>)
        else {
            return Ok(false);
        };
        if state["current"]["encoded"]
            .as_str()
            .is_none_or(str::is_empty)
            || Self::human_listeners(app, channel) == 0
        {
            return Ok(false);
        }
        let session = self.ensure_session(app, guild_id, channel).await?;
        let mut players = self.players.lock().await;
        let Some(player) = players
            .get_mut(&guild_id)
            .filter(|p| p.instance == session.instance)
        else {
            return Ok(false);
        };
        player.current = Some(state["current"].clone());
        player.tracks = state["tracks"]
            .as_array()
            .map(|tracks| tracks.iter().cloned().collect())
            .unwrap_or_default();
        player.volume = state["volume"]
            .as_i64()
            .unwrap_or(self.default_volume)
            .clamp(0, 150);
        player.repeat = Repeat::parse(state["repeatMode"].as_str().unwrap_or_default());
        player.autoplay = state["autoplay"] == Value::Bool(true);
        self.play_current(player, false).await?;
        let paused = state["paused"] == Value::Bool(true);
        let live = state["current"]["info"]["isStream"] == Value::Bool(true);
        let position = if live {
            0
        } else {
            state["positionMs"].as_i64().unwrap_or(0).max(0)
        };
        if position > 0 || paused {
            self.node()?
                .update_player(
                    &guild_id.to_string(),
                    &json!({ "position": position, "paused": paused }),
                    false,
                )
                .await?;
            player.last_position = position;
            player.last_position_at = Some(Instant::now());
            player.paused = paused;
        }
        Ok(true)
    }

    pub async fn shutdown(&self, app: &App) {
        self.save_for_restart().await;
        let guilds: Vec<Id<GuildMarker>> = self.players.lock().await.keys().copied().collect();
        for guild_id in guilds {
            self.destroy(app, guild_id, "bot shutdown").await;
        }
        if let Some(node) = &self.node {
            node.shutdown();
        }
    }
}
