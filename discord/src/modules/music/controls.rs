#[allow(clippy::wildcard_imports)]
use super::*;

impl MusicManager {
    // --- controls ------------------------------------------------------------------------------

    /// Mutates an active player (`getPlayerForGuild`), sends the returned PATCH body and rolls the
    /// local state back when Lavalink rejects it. Returns the fresh snapshot.
    pub(super) async fn with_active(
        &self,
        guild_id: Id<GuildMarker>,
        mutate: impl FnOnce(&mut Player) -> Option<Value>,
    ) -> Result<Option<Snapshot>> {
        let mut players = self.players.lock().await;
        let Some(player) = players.get_mut(&guild_id).filter(|p| p.has_music()) else {
            return Ok(None);
        };
        let backup = player.clone();
        if let Some(body) = mutate(player) {
            let node = self.node()?;
            if let Err(error) = node
                .update_player(&guild_id.to_string(), &body, false)
                .await
            {
                *player = backup;
                return Err(error);
            }
        }
        Ok(snapshot_of(player))
    }

    /// `pause` / `resume` / `togglePause` (`None` toggles).
    pub async fn set_paused(
        &self,
        guild_id: Id<GuildMarker>,
        paused: Option<bool>,
    ) -> Result<Option<Snapshot>> {
        let has_current = self
            .players
            .lock()
            .await
            .get(&guild_id)
            .is_some_and(|p| p.has_music() && p.current.is_some());
        if !has_current {
            return Ok(None);
        }
        let result = self
            .with_active(guild_id, |player| {
                let next = paused.unwrap_or(!player.paused);
                if next == player.paused {
                    return None;
                }
                if next {
                    player.last_position = player.position_ms();
                } else {
                    player.last_position_at = Some(Instant::now());
                }
                player.paused = next;
                Some(json!({ "paused": next }))
            })
            .await;
        self.refresh_status();
        result
    }

    pub async fn adjust_volume(
        &self,
        guild_id: Id<GuildMarker>,
        delta: i64,
    ) -> Result<Option<Snapshot>> {
        let default = self.default_volume;
        self.with_active(guild_id, |player| {
            let current = if player.volume != 0 {
                player.volume
            } else {
                default
            };
            if current > 0 {
                player.previous_volume = Some(current);
            }
            player.volume = (current + delta).clamp(0, 150);
            Some(json!({ "volume": player.volume }))
        })
        .await
    }

    pub async fn toggle_mute(&self, guild_id: Id<GuildMarker>) -> Result<Option<Snapshot>> {
        let default = self.default_volume;
        self.with_active(guild_id, |player| {
            if player.volume > 0 {
                player.previous_volume = Some(player.volume);
                player.volume = 0;
            } else {
                let fallback = if default != 0 { default } else { 80 };
                player.volume = player
                    .previous_volume
                    .filter(|v| *v != 0)
                    .unwrap_or(fallback)
                    .clamp(1, 150);
            }
            Some(json!({ "volume": player.volume }))
        })
        .await
    }

    pub async fn cycle_loop(&self, guild_id: Id<GuildMarker>) -> Result<Option<Snapshot>> {
        self.with_active(guild_id, |player| {
            player.repeat = match player.repeat {
                Repeat::Off => Repeat::Track,
                Repeat::Track => Repeat::Queue,
                Repeat::Queue => Repeat::Off,
            };
            None
        })
        .await
    }

    pub async fn toggle_autoplay(&self, guild_id: Id<GuildMarker>) -> bool {
        let mut players = self.players.lock().await;
        players.get_mut(&guild_id).is_some_and(|player| {
            player.autoplay = !player.autoplay;
            player.autoplay
        })
    }

    pub async fn toggle_bassboost(&self, guild_id: Id<GuildMarker>) -> Result<Option<Snapshot>> {
        let active = self
            .players
            .lock()
            .await
            .get(&guild_id)
            .is_some_and(|p| p.flags.bassboost);
        self.apply_eq_preset(guild_id, if active { "flat" } else { "bassboost" })
            .await
    }

    pub async fn apply_eq_preset(
        &self,
        guild_id: Id<GuildMarker>,
        preset: &str,
    ) -> Result<Option<Snapshot>> {
        let preset = preset.to_lowercase();
        self.with_active(guild_id, |player| {
            let mut changed = false;
            if preset == "nightcore" {
                if !player.flags.nightcore {
                    player.timescale = Timescale::Nightcore;
                    changed = true;
                }
                player.flags.nightcore = true;
                player.flags.vaporwave = false;
                player.flags.eq_preset = "nightcore".to_owned();
                player.flags.bassboost = false;
                return changed.then(|| json!({ "filters": player.filters_body() }));
            }
            if player.flags.nightcore {
                player.timescale = if player.timescale == Timescale::Nightcore {
                    Timescale::Normal
                } else {
                    Timescale::Nightcore
                };
                player.flags.nightcore = false;
                changed = true;
            }
            match preset.as_str() {
                "flat" => {
                    player.set_eq(&[0.0; 15]);
                    player.flags.bassboost = false;
                    player.flags.eq_preset = "flat".to_owned();
                    changed = true;
                }
                "bassboost" => {
                    let bands: Vec<f64> = BASSBOOST_MEDIUM
                        .iter()
                        .map(|g| g * BASSBOOST_MULTIPLIER)
                        .collect();
                    player.set_eq(&bands);
                    player.flags.bassboost = true;
                    player.flags.eq_preset = "bassboost".to_owned();
                    changed = true;
                }
                "vocal" | "treble" => {
                    player.set_eq(if preset == "vocal" {
                        &VOCAL_EQ
                    } else {
                        &TREBLE_EQ
                    });
                    player.flags.bassboost = false;
                    player.flags.eq_preset.clone_from(&preset);
                    changed = true;
                }
                _ => {}
            }
            changed.then(|| json!({ "filters": player.filters_body() }))
        })
        .await
    }

    pub async fn toggle_fx(&self, guild_id: Id<GuildMarker>, fx: &str) -> Result<Option<Snapshot>> {
        let fx = fx.to_lowercase();
        self.with_active(guild_id, |player| {
            fn toggle_vaporwave(player: &mut Player) {
                player.timescale = if player.timescale == Timescale::Vaporwave {
                    Timescale::Normal
                } else {
                    Timescale::Vaporwave
                };
            }
            match fx.as_str() {
                "vaporwave" => {
                    toggle_vaporwave(player);
                    player.flags.vaporwave = !player.flags.vaporwave;
                    if player.flags.vaporwave {
                        player.flags.nightcore = false;
                    }
                }
                "8d" | "eightd" => {
                    player.rotation = !player.rotation;
                    player.flags.eight_d = !player.flags.eight_d;
                }
                "karaoke" => {
                    player.karaoke = !player.karaoke;
                    player.flags.karaoke = !player.flags.karaoke;
                }
                "softlimiter" => {
                    player.low_pass = !player.low_pass;
                    player.flags.soft_limiter = !player.flags.soft_limiter;
                }
                "off" => {
                    if player.flags.vaporwave {
                        toggle_vaporwave(player);
                    }
                    if player.flags.eight_d {
                        player.rotation = !player.rotation;
                    }
                    if player.flags.karaoke {
                        player.karaoke = !player.karaoke;
                    }
                    if player.flags.soft_limiter {
                        player.low_pass = !player.low_pass;
                    }
                    player.flags.vaporwave = false;
                    player.flags.eight_d = false;
                    player.flags.karaoke = false;
                    player.flags.soft_limiter = false;
                }
                _ => return None,
            }
            Some(json!({ "filters": player.filters_body() }))
        })
        .await
    }

    /// Removes the track at 1-based `position` after the current one.
    pub async fn remove_from_queue(
        &self,
        guild_id: Id<GuildMarker>,
        position: i64,
    ) -> Result<Option<PublicTrack>> {
        if position <= 0 {
            bail!("The queue position must be a number >= 1.");
        }
        let mut players = self.players.lock().await;
        let Some(player) = players.get_mut(&guild_id).filter(|p| p.has_music()) else {
            return Ok(None);
        };
        Ok(player
            .tracks
            .remove((position - 1) as usize)
            .map(|t| normalize_track(&t)))
    }

    /// Moves the queued track at 1-based `from` to `to`; returns it.
    pub async fn move_in_queue(
        &self,
        guild_id: Id<GuildMarker>,
        from: usize,
        to: usize,
    ) -> Option<PublicTrack> {
        let mut players = self.players.lock().await;
        let player = players.get_mut(&guild_id).filter(|p| p.has_music())?;
        if from == 0 || to == 0 || from > player.tracks.len() || to > player.tracks.len() {
            return None;
        }
        let track = player.tracks.remove(from - 1)?;
        let public = normalize_track(&track);
        player.tracks.insert(to - 1, track);
        Some(public)
    }

    pub async fn clear_queue(&self, guild_id: Id<GuildMarker>) -> Option<usize> {
        let mut players = self.players.lock().await;
        let player = players.get_mut(&guild_id).filter(|p| p.has_music())?;
        let removed = player.tracks.len();
        player.tracks.clear();
        Some(removed)
    }

    pub async fn shuffle(&self, guild_id: Id<GuildMarker>) -> Option<Snapshot> {
        let mut players = self.players.lock().await;
        let player = players.get_mut(&guild_id).filter(|p| p.has_music())?;
        if player.tracks.len() > 1 {
            player.tracks.make_contiguous().shuffle(&mut rand::rng());
        }
        snapshot_of(player)
    }

    pub async fn skip(&self, guild_id: Id<GuildMarker>) -> Result<Option<PublicTrack>> {
        let mut players = self.players.lock().await;
        let Some(player) = players.get_mut(&guild_id) else {
            return Ok(None);
        };
        let Some(current) = player.current.as_ref().map(normalize_track) else {
            return Ok(None);
        };
        player.skipped = true;
        let node = self.node()?;
        let body = json!({ "track": { "encoded": null }, "paused": false });
        if let Err(error) = node
            .update_player(&guild_id.to_string(), &body, false)
            .await
        {
            tracing::warn!("Music skip error guild={guild_id}: {error}");
        }
        player.paused = false;
        drop(players);
        self.refresh_status();
        Ok(Some(current))
    }

    /// `stop`: destroys the session and clears every per-guild state.
    pub async fn stop(&self, app: &App, guild_id: Id<GuildMarker>) -> bool {
        let channel = self
            .players
            .lock()
            .await
            .get(&guild_id)
            .map(|p| p.voice_channel_id);
        let Some(channel) = channel else {
            self.refresh_status();
            return false;
        };
        self.clear_voice_status(app, channel).await;
        self.destroy(app, guild_id, "music stop").await;
        true
    }

    pub(super) async fn destroy(&self, app: &App, guild_id: Id<GuildMarker>, reason: &str) {
        let Some(mut player) = self.players.lock().await.remove(&guild_id) else {
            return;
        };
        player.destroying = true;
        if let Err(error) = Self::send_voice(app, guild_id, None) {
            tracing::warn!("Music disconnect error guild={guild_id}: {error}");
        }
        if let Ok(node) = self.node()
            && let Err(error) = node.destroy_player(&guild_id.to_string()).await
        {
            tracing::warn!("Music destroy error guild={guild_id} reason=\"{reason}\": {error}");
        }
        self.refresh_status();
    }

    pub async fn reconnect_session(
        &self,
        app: &App,
        guild_id: Id<GuildMarker>,
    ) -> Result<Option<ReconnectResult>> {
        let channel = {
            let players = self.players.lock().await;
            let Some(player) = players.get(&guild_id).filter(|p| p.has_music() || p.paused) else {
                return Ok(None);
            };
            player.voice_channel_id
        };
        let channel = channel.ok_or_else(|| anyhow!("No saved voice channel for this session."))?;
        let node = self.node()?;
        if !node.connected() {
            node.reconnect_now();
            let started = Instant::now();
            while !node.connected() && started.elapsed() < Duration::from_secs(3) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if !node.connected() {
                bail!("The Lavalink node is still offline after the reconnect attempt.");
            }
        }
        if let Some(player) = self.players.lock().await.get_mut(&guild_id) {
            player.reconnecting = true;
            player.idle_generation += 1;
        }
        let _ = Self::send_voice(app, guild_id, None);
        Self::send_voice(app, guild_id, Some(channel))?;
        self.refresh_status();
        Ok(Some(ReconnectResult {
            voice_channel_id: channel,
            node_connected: node.connected(),
        }))
    }
}
