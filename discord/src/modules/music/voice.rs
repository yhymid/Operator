#[allow(clippy::wildcard_imports)]
use super::*;

impl MusicManager {
    // --- voice gateway -------------------------------------------------------------------------

    pub(super) fn send_voice(
        app: &App,
        guild_id: Id<GuildMarker>,
        channel: Option<Id<ChannelMarker>>,
    ) -> Result<()> {
        app.sender
            .command(&UpdateVoiceState::new(guild_id, channel, true, false))
            .map_err(|e| anyhow!("Could not send the voice state: {e}"))
    }

    pub(super) async fn push_voice(&self, player: &mut Player) {
        let Ok(node) = self.node() else { return };
        let Some(voice) = player.voice_body() else {
            return;
        };
        let mut body = json!({ "voice": voice });
        if player.node_session != node.session_id() {
            // Fresh player on this node session: restore volume, filters and the track.
            body["volume"] = json!(player.volume);
            body["filters"] = player.filters_body();
            if let Some(current) = &player.current {
                body["track"] = lavalink::track_body(current);
                body["position"] = json!(player.position_ms());
                body["paused"] = json!(player.paused);
            }
        }
        match node
            .update_player(&player.guild_id.to_string(), &body, false)
            .await
        {
            Ok(_) => player.node_session = node.session_id(),
            Err(error) => tracing::warn!(
                "Lavalink voice update error guild={}: {error}",
                player.guild_id
            ),
        }
    }

    /// Gateway `VOICE_STATE_UPDATE`: the bot's own state drives reconnect/autoMove; anyone's
    /// join/leave re-checks whether the bot is left alone in its channel.
    pub async fn handle_voice_state(&self, app: &Arc<App>, update: &VoiceStateUpdate) {
        let Some(guild_id) = update.0.guild_id else {
            return;
        };
        if update.0.user_id == app.bot_id {
            self.handle_own_voice_state(app, guild_id, update).await;
        }
        self.check_alone(app, guild_id).await;
    }

    /// Number of non-bot members in `channel` according to the gateway cache. Users missing from
    /// the cache count as listeners, so an incomplete cache never makes the bot leave.
    pub(super) fn human_listeners(app: &App, channel: Id<ChannelMarker>) -> usize {
        app.cache.voice_channel_states(channel).map_or(0, |states| {
            states
                .filter(|state| {
                    let user = state.user_id();
                    user != app.bot_id && app.cache.user(user).is_none_or(|u| !u.bot)
                })
                .count()
        })
    }

    pub(super) async fn check_alone(&self, app: &Arc<App>, guild_id: Id<GuildMarker>) {
        if self.alone_disconnect_ms <= 0.0 {
            return;
        }
        let generation = {
            let mut players = self.players.lock().await;
            let Some(player) = players.get_mut(&guild_id) else {
                return;
            };
            let alone = player
                .voice_channel_id
                .is_some_and(|channel| Self::human_listeners(app, channel) == 0);
            // Any change invalidates a pending timer; a new one starts only while alone.
            player.alone_generation += 1;
            if !alone {
                return;
            }
            player.alone_generation
        };
        let delay = Duration::from_millis(self.alone_disconnect_ms as u64);
        let app = app.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let still_alone = app
                .music
                .players
                .lock()
                .await
                .get(&guild_id)
                .is_some_and(|p| {
                    p.alone_generation == generation
                        && p.voice_channel_id
                            .is_some_and(|channel| Self::human_listeners(&app, channel) == 0)
                });
            if still_alone {
                tracing::info!("Music: nobody is listening, disconnecting guild={guild_id}");
                app.music
                    .destroy(&app, guild_id, "alone in voice channel")
                    .await;
            }
        });
    }

    pub(super) async fn handle_own_voice_state(
        &self,
        app: &App,
        guild_id: Id<GuildMarker>,
        update: &VoiceStateUpdate,
    ) {
        let mut players = self.players.lock().await;
        let Some(player) = players.get_mut(&guild_id) else {
            return;
        };
        player.voice_session = Some(update.0.session_id.clone());
        match update.0.channel_id {
            None => {
                if player.destroying {
                    return;
                }
                if player.reconnecting {
                    player.reconnecting = false;
                    return;
                }
                // onDisconnect.autoReconnect
                if let Some(channel) = player.voice_channel_id
                    && let Err(error) = Self::send_voice(app, guild_id, Some(channel))
                {
                    tracing::warn!("Music auto reconnect failed guild={guild_id}: {error}");
                }
            }
            Some(channel) if Some(channel) != player.voice_channel_id => {
                // autoMove
                player.voice_channel_id = Some(channel);
                drop(players);
                self.refresh_status();
            }
            Some(_) => {}
        }
    }

    pub async fn handle_voice_server(&self, update: &VoiceServerUpdate) {
        let mut players = self.players.lock().await;
        let Some(player) = players.get_mut(&update.guild_id) else {
            return;
        };
        player.voice_token = Some(update.token.clone());
        player.voice_endpoint = update.endpoint.clone();
        self.push_voice(player).await;
    }
    // --- voice channel status ------------------------------------------------------------------

    /// The JS bot showed the track as the bot's presence; replace that with the status saved
    /// by `/presence` (or none).
    pub(super) fn clear_legacy_presence(&self, app: &App) {
        crate::presence::apply(app);
        self.legacy_presence_cleared.store(true, Ordering::SeqCst);
    }

    pub(super) async fn set_voice_status(
        app: &App,
        channel: Id<ChannelMarker>,
        status: Option<&str>,
    ) -> Result<()> {
        app.raw
            .put_json(
                &format!("/channels/{channel}/voice-status"),
                &json!({ "status": status.map(|s| truncate_inline(s, MAX_VOICE_CHANNEL_STATUS_LENGTH)) }),
            )
            .await
    }

    pub(super) fn cached_status(&self, channel: Id<ChannelMarker>) -> Option<String> {
        self.voice_statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&channel)
            .cloned()
    }

    pub(super) fn store_status(&self, channel: Id<ChannelMarker>, status: Option<String>) {
        let mut statuses = self
            .voice_statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        match status {
            Some(text) => {
                statuses.insert(channel, text);
            }
            None => {
                statuses.remove(&channel);
            }
        }
    }

    pub(super) async fn sync_voice_statuses(&self, app: &App) {
        let _guard = self.voice_sync.lock().await;
        if !self.legacy_presence_cleared.load(Ordering::SeqCst) {
            self.clear_legacy_presence(app);
        }
        let desired: HashMap<Id<ChannelMarker>, String> = {
            let players = self.players.lock().await;
            players
                .values()
                .filter_map(|p| Some((p.voice_channel_id?, p.status_text()?)))
                .collect()
        };
        let mut channels: Vec<Id<ChannelMarker>> = self
            .voice_statuses
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .copied()
            .collect();
        for channel in desired.keys() {
            if !channels.contains(channel) {
                channels.push(*channel);
            }
        }
        for channel in channels {
            let next = desired.get(&channel).cloned();
            if self.cached_status(channel) == next {
                continue;
            }
            match Self::set_voice_status(app, channel, next.as_deref()).await {
                Ok(()) => self.store_status(channel, next),
                Err(error) => {
                    tracing::warn!("Music voice status update failed channel={channel}: {error}");
                }
            }
        }
    }

    /// `refreshMusicPresence`: serialised background sync of voice channel statuses.
    pub fn refresh_status(&self) {
        let Some(app) = self.app() else { return };
        tokio::spawn(async move {
            app.music.sync_voice_statuses(&app).await;
        });
    }

    pub(super) async fn clear_voice_status(&self, app: &App, channel: Option<Id<ChannelMarker>>) {
        let Some(channel) = channel else { return };
        let _guard = self.voice_sync.lock().await;
        if !self.legacy_presence_cleared.load(Ordering::SeqCst) {
            self.clear_legacy_presence(app);
        }
        match Self::set_voice_status(app, channel, None).await {
            Ok(()) => self.store_status(channel, None),
            Err(error) => {
                tracing::warn!("Music voice status clear failed channel={channel}: {error}");
            }
        }
    }
}
