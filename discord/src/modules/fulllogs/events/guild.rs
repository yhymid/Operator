#[allow(clippy::wildcard_imports)]
use super::super::*;

impl FullLogs {
    pub(in super::super) async fn on_invite_create(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let Event::InviteCreate(invite) = event else {
            return Ok(());
        };
        let channel_name = |id: Option<Id<ChannelMarker>>| -> Value {
            id.and_then(|id| app.cache.channel(id).and_then(|c| c.name.clone()))
                .map_or(Value::Null, Value::String)
        };
        let expires = (invite.max_age > 0)
            .then(|| {
                DateTime::<Utc>::from_timestamp_micros(invite.created_at.as_micros()).map(|c| {
                    operator_common::time::to_iso(
                        c + chrono::Duration::seconds(invite.max_age as i64),
                    )
                })
            })
            .flatten()
            .unwrap_or_else(|| "never".to_owned());
        let inviter = invite.inviter.as_ref();
        self.log(app, invite.guild_id, json!({
            "category": "invite",
            "action": "create",
            "actor": actor_payload(inviter),
            "target": { "kind": "invite", "id": invite.code },
            "location": { "channelId": invite.channel_id.to_string(), "channelName": channel_name(Some(invite.channel_id)) },
            "summary": format!("Invite created: {}.", invite.code),
            "details": [
                format!("code={}", invite.code),
                format!("max_uses={}", invite.max_uses),
                format!("temporary={}", invite.temporary),
                format!("expires_at={expires}"),
            ],
            "metadata": {
                "code": invite.code,
                "channelId": invite.channel_id.to_string(),
                "inviterId": inviter.map(|i| i.id.to_string())
            }
        }), false);
        Ok(())
    }

    pub(in super::super) async fn on_invite_delete(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let Event::InviteDelete(invite) = event else {
            return Ok(());
        };
        let channel_name = |id: Option<Id<ChannelMarker>>| -> Value {
            id.and_then(|id| app.cache.channel(id).and_then(|c| c.name.clone()))
                .map_or(Value::Null, Value::String)
        };
        let name = channel_name(Some(invite.channel_id));
        self.log(app, invite.guild_id, json!({
            "category": "invite",
            "action": "delete",
            "target": { "kind": "invite", "id": invite.code },
            "location": { "channelId": invite.channel_id.to_string(), "channelName": name },
            "summary": format!("Invite deleted: {}.", invite.code),
            "details": [format!("code={}", invite.code), format!("channel=#{}", name.as_str().unwrap_or("unknown"))],
            "metadata": { "code": invite.code, "channelId": invite.channel_id.to_string() }
        }), false);
        Ok(())
    }

    pub(in super::super) async fn on_guild_emojis_update(
        &self,
        app: &SharedApp,
        event: &Event,
        before: Before,
    ) -> anyhow::Result<()> {
        let Event::GuildEmojisUpdate(update) = event else {
            return Ok(());
        };
        let Before::Emojis(old) = before else {
            return Ok(());
        };
        let new: Vec<(String, String, bool)> = update
            .emojis
            .iter()
            .map(|e| (e.id.to_string(), e.name.clone(), e.animated))
            .collect();
        let old_ids: HashSet<&String> = old.iter().map(|(id, _, _)| id).collect();
        let new_ids: HashSet<&String> = new.iter().map(|(id, _, _)| id).collect();
        for (id, name, animated) in new.iter().filter(|(id, _, _)| !old_ids.contains(id)) {
            self.log(
                app,
                update.guild_id,
                json!({
                    "category": "emoji", "action": "create",
                    "target": { "kind": "emoji", "id": id, "name": name },
                    "summary": format!("Emoji created: {name}."),
                    "details": [format!("emoji_id={id}"), format!("animated={animated}")],
                    "metadata": { "emojiId": id, "name": name }
                }),
                false,
            );
        }
        for (id, name, _) in old.iter().filter(|(id, _, _)| !new_ids.contains(id)) {
            self.log(
                app,
                update.guild_id,
                json!({
                    "category": "emoji", "action": "delete",
                    "target": { "kind": "emoji", "id": id, "name": name },
                    "summary": format!("Emoji deleted: {name}."),
                    "details": [format!("emoji_id={id}")],
                    "metadata": { "emojiId": id, "name": name }
                }),
                false,
            );
        }
        for (id, name, _) in &new {
            let Some((_, old_name, _)) = old.iter().find(|(old_id, _, _)| old_id == id) else {
                continue;
            };
            let Some(delta) = format_state_delta(old_name, name) else {
                continue;
            };
            self.log(
                app,
                update.guild_id,
                json!({
                    "category": "emoji", "action": "update",
                    "target": { "kind": "emoji", "id": id, "name": name },
                    "summary": "Emoji renamed.",
                    "details": [format!("name={delta}")],
                    "metadata": { "emojiId": id, "nameDelta": delta }
                }),
                false,
            );
        }
        Ok(())
    }

    pub(in super::super) async fn on_guild_stickers_update(
        &self,
        app: &SharedApp,
        event: &Event,
        before: Before,
    ) -> anyhow::Result<()> {
        let Event::GuildStickersUpdate(update) = event else {
            return Ok(());
        };
        let Before::Stickers(old) = before else {
            return Ok(());
        };
        let new: Vec<(String, String, String, u8)> = update
            .stickers
            .iter()
            .map(|s| {
                (
                    s.id.to_string(),
                    s.name.clone(),
                    s.description.clone().unwrap_or_default(),
                    u8::from(s.format_type),
                )
            })
            .collect();
        let old_ids: HashSet<&String> = old.iter().map(|s| &s.0).collect();
        let new_ids: HashSet<&String> = new.iter().map(|s| &s.0).collect();
        for (id, name, _, format) in new.iter().filter(|s| !old_ids.contains(&s.0)) {
            self.log(
                app,
                update.guild_id,
                json!({
                    "category": "sticker", "action": "create",
                    "target": { "kind": "sticker", "id": id, "name": name },
                    "summary": format!("Sticker created: {name}."),
                    "details": [format!("sticker_id={id}"), format!("format={format}")],
                    "metadata": { "stickerId": id, "name": name }
                }),
                false,
            );
        }
        for (id, name, _, _) in old.iter().filter(|s| !new_ids.contains(&s.0)) {
            self.log(
                app,
                update.guild_id,
                json!({
                    "category": "sticker", "action": "delete",
                    "target": { "kind": "sticker", "id": id, "name": name },
                    "summary": format!("Sticker deleted: {name}."),
                    "details": [format!("sticker_id={id}")],
                    "metadata": { "stickerId": id, "name": name }
                }),
                false,
            );
        }
        for (id, name, description, _) in &new {
            let Some((_, old_name, old_description, _)) = old.iter().find(|s| &s.0 == id) else {
                continue;
            };
            let mut details = Vec::new();
            if let Some(d) = format_state_delta(old_name, name) {
                details.push(format!("name={d}"));
            }
            if let Some(d) = format_state_delta(old_description, description) {
                details.push(format!("description={d}"));
            }
            if details.is_empty() {
                continue;
            }
            self.log(
                app,
                update.guild_id,
                json!({
                    "category": "sticker", "action": "update",
                    "target": { "kind": "sticker", "id": id, "name": name },
                    "summary": format!("Sticker updated: {name}."),
                    "details": details,
                    "metadata": { "stickerId": id, "changes": details }
                }),
                false,
            );
        }
        Ok(())
    }

    pub(in super::super) async fn on_webhooks_update(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let Event::WebhooksUpdate(update) = event else {
            return Ok(());
        };
        let channel_name = |id: Option<Id<ChannelMarker>>| -> Value {
            id.and_then(|id| app.cache.channel(id).and_then(|c| c.name.clone()))
                .map_or(Value::Null, Value::String)
        };
        let name = channel_name(Some(update.channel_id));
        self.log(
            app,
            update.guild_id,
            json!({
                "category": "webhook",
                "action": "update",
                "target": { "kind": "channel", "id": update.channel_id.to_string(), "name": name },
                "location": { "channelId": update.channel_id.to_string(), "channelName": name },
                "summary": format!("Webhook list changed in #{}.", display_name(name.as_str())),
                "details": [format!("channel_id={}", update.channel_id)],
                "metadata": { "channelId": update.channel_id.to_string() }
            }),
            false,
        );
        Ok(())
    }

    pub(in super::super) async fn on_guild_update(
        &self,
        app: &SharedApp,
        event: &Event,
        before: Before,
    ) -> anyhow::Result<()> {
        let Event::GuildUpdate(update) = event else {
            return Ok(());
        };
        let Before::Guild(Some((old_name, old_icon, old_afk, old_verification))) = before else {
            return Ok(());
        };
        let mut details = Vec::new();
        if let Some(d) = format_state_delta(&old_name, &update.name) {
            details.push(format!("name={d}"));
        }
        if let Some(d) = format_state_delta(
            old_icon.as_deref().unwrap_or_default(),
            &update.icon.map(|i| i.to_string()).unwrap_or_default(),
        ) {
            details.push(format!("icon_hash={d}"));
        }
        if let Some(d) = format_state_delta(
            old_afk.as_deref().unwrap_or_default(),
            &update
                .afk_channel_id
                .map(|c| c.to_string())
                .unwrap_or_default(),
        ) {
            details.push(format!("afk_channel={d}"));
        }
        if let Some(d) = format_state_delta(
            &old_verification.to_string(),
            &u8::from(update.verification_level).to_string(),
        ) {
            details.push(format!("verification_level={d}"));
        }
        if details.is_empty() {
            return Ok(());
        }
        let audit = self
            .resolve_audit(app, update.id, AuditLogEventType::GuildUpdate, None, |_| {
                true
            })
            .await;
        let actor = audit.executor.clone();
        self.log(
            app,
            update.id,
            json!({
                "category": "guild",
                "action": "update",
                "actor": actor_payload(actor.as_ref()),
                "target": { "kind": "guild", "id": update.id.to_string(), "name": update.name },
                "summary": format!("Guild updated: {}.", update.name),
                "details": details,
                "metadata": {
                    "guildId": update.id.to_string(),
                    "actorId": actor.as_ref().map(|a| a.id.to_string()),
                    "reason": audit.entry.as_ref().and_then(|e| e.reason.clone()),
                    "changes": details
                }
            }),
            false,
        );
        Ok(())
    }

    pub(in super::super) async fn on_auto_moderation_action_execution(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let Event::AutoModerationActionExecution(execution) = event else {
            return Ok(());
        };
        let channel_name = |id: Option<Id<ChannelMarker>>| -> Value {
            id.and_then(|id| app.cache.channel(id).and_then(|c| c.name.clone()))
                .map_or(Value::Null, Value::String)
        };
        let user_from_cache = |id: Id<UserMarker>| app.cache.user(id).map(|u| u.clone());
        let user = user_from_cache(execution.user_id);
        let action_type = u8::from(execution.action.kind);
        let trigger = u8::from(execution.rule_trigger_type);
        let details: Vec<String> = [
            Some(format!("rule_id={}", execution.rule_id)),
            Some(format!("rule_trigger={trigger}")),
            Some(format!("action_type={action_type}")),
            execution
                .matched_keyword
                .as_deref()
                .map(|k| format!("keyword={}", inline(k, 80))),
            (!execution.content.is_empty()).then(|| {
                format!(
                    "content={}",
                    block(&execution.content, MAX_DETAIL_VALUE_LENGTH)
                )
            }),
        ]
        .into_iter()
        .flatten()
        .collect();
        self.log(
            app,
            execution.guild_id,
            json!({
                "category": "automod",
                "action": "execution",
                "actor": actor_payload(user.as_ref()),
                "target": {
                    "kind": "member",
                    "id": execution.user_id.to_string(),
                    "tag": user.as_ref().map(discord::user_tag)
                },
                "location": {
                    "channelId": execution.channel_id.map(|c| c.to_string()),
                    "channelName": channel_name(execution.channel_id)
                },
                "summary": format!("AutoMod action executed: {action_type}."),
                "details": details,
                "metadata": {
                    "ruleId": execution.rule_id.to_string(),
                    "channelId": execution.channel_id.map(|c| c.to_string()),
                    "userId": execution.user_id.to_string(),
                    "actionType": action_type,
                    "matchedKeyword": execution.matched_keyword
                }
            }),
            false,
        );
        Ok(())
    }
}
