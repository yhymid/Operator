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
        operator_common::fs::write_atomic(&self.queue_state_path(guild_id), &bytes)?;
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

    pub async fn shutdown(&self, app: &App) {
        let guilds: Vec<Id<GuildMarker>> = self.players.lock().await.keys().copied().collect();
        for guild_id in guilds {
            self.destroy(app, guild_id, "bot shutdown").await;
        }
        if let Some(node) = &self.node {
            node.shutdown();
        }
    }
}
