#[allow(clippy::wildcard_imports)]
use super::super::*;

impl FullLogs {
    pub(in super::super) async fn on_message_update(
        &self,
        app: &SharedApp,
        event: &Event,
        before: Before,
    ) -> anyhow::Result<()> {
        let Event::MessageUpdate(update) = event else {
            return Ok(());
        };
        let channel_name = |id: Option<Id<ChannelMarker>>| -> Value {
            id.and_then(|id| app.cache.channel(id).and_then(|c| c.name.clone()))
                .map_or(Value::Null, Value::String)
        };
        let Some(guild_id) = update.guild_id else {
            return Ok(());
        };
        if update.author.id == app.bot_id {
            return Ok(());
        }
        let Before::Message(old) = before else {
            return Ok(());
        };
        let Some(old) = old else {
            // Sent before the bot cached it: log the new text without a "before". Embed
            // unfurls also arrive as updates; only real edits carry `edited_timestamp`.
            if update.edited_timestamp.is_none() {
                return Ok(());
            }
            let after_text = block(&update.content, MAX_DETAIL_VALUE_LENGTH);
            self.log(app, guild_id, json!({
                "category": "message",
                "action": "edit",
                "actor": actor_payload(Some(&update.author)),
                "target": { "kind": "message", "id": update.id.to_string() },
                "location": { "channelId": update.channel_id.to_string(), "channelName": channel_name(Some(update.channel_id)) },
                "summary": "Message edited (the earlier text was not cached).",
                "details": [format!("message_id={}", update.id)],
                "metadata": {
                    "messageId": update.id.to_string(),
                    "channelId": update.channel_id.to_string(),
                    "after": after_text,
                    "uncached": true,
                    "contentChanged": true
                }
            }), false);
            return Ok(());
        };
        let before_text = block(&old.content, MAX_DETAIL_VALUE_LENGTH);
        let after_text = block(&update.content, MAX_DETAIL_VALUE_LENGTH);
        let changed = before_text != after_text;
        let empty = |s: &str| {
            if s.is_empty() {
                "[empty]".to_owned()
            } else {
                s.to_owned()
            }
        };
        let mut details = vec![format!("message_id={}", update.id)];
        if changed {
            details.push(format!("before={}", empty(&before_text)));
            details.push(format!("after={}", empty(&after_text)));
        }
        self.log(app, guild_id, json!({
            "category": "message",
            "action": "edit",
            "actor": actor_payload(Some(&update.author)),
            "target": { "kind": "message", "id": update.id.to_string() },
            "location": { "channelId": update.channel_id.to_string(), "channelName": channel_name(Some(update.channel_id)) },
            "summary": if changed { "Message content edited." } else { "Message updated (non-content fields)." },
            "details": details,
            "metadata": {
                "messageId": update.id.to_string(),
                "channelId": update.channel_id.to_string(),
                "before": before_text,
                "after": after_text,
                "contentChanged": changed
            }
        }), false);
        Ok(())
    }

    pub(in super::super) async fn on_message_delete(
        &self,
        app: &SharedApp,
        event: &Event,
        before: Before,
    ) -> anyhow::Result<()> {
        let Event::MessageDelete(delete) = event else {
            return Ok(());
        };
        let channel_name = |id: Option<Id<ChannelMarker>>| -> Value {
            id.and_then(|id| app.cache.channel(id).and_then(|c| c.name.clone()))
                .map_or(Value::Null, Value::String)
        };
        let user_from_cache = |id: Id<UserMarker>| app.cache.user(id).map(|u| u.clone());
        let Some(guild_id) = delete.guild_id else {
            return Ok(());
        };
        let Before::Message(old) = before else {
            return Ok(());
        };
        let Some(old) = old else {
            // Not in the message cache (sent before the bot started or long ago): Discord only
            // sends the IDs, so the content and author are unknown.
            self.log(app, guild_id, json!({
                "category": "message",
                "action": "delete",
                "actor": null,
                "target": { "kind": "message", "id": delete.id.to_string() },
                "location": { "channelId": delete.channel_id.to_string(), "channelName": channel_name(Some(delete.channel_id)) },
                "summary": "Message deleted (not cached: content and author unknown).",
                "details": [format!("message_id={}", delete.id)],
                "metadata": {
                    "messageId": delete.id.to_string(),
                    "channelId": delete.channel_id.to_string(),
                    "uncached": true
                }
            }), false);
            return Ok(());
        };
        if old.author_id == app.bot_id {
            return Ok(());
        }
        let author = user_from_cache(old.author_id);
        let channel_id = delete.channel_id;
        let audit = self
            .resolve_audit(
                app,
                guild_id,
                AuditLogEventType::MessageDelete,
                Some(old.author_id.get()),
                |entry| {
                    let options = entry.options.as_ref();
                    if let Some(ch) = options.and_then(|o| o.channel_id)
                        && ch != channel_id
                    {
                        return false;
                    }
                    if let Some(count) = options
                        .and_then(|o| o.count.as_deref())
                        .and_then(|c| c.parse::<f64>().ok())
                        && count != 1.0
                    {
                        return false;
                    }
                    true
                },
            )
            .await;
        let deleter = audit.executor.clone();
        let content = block(&old.content, MAX_DETAIL_VALUE_LENGTH);
        let author_tag = author.as_ref().map(discord::user_tag);
        let attachments_list = old
            .attachments
            .iter()
            .map(|a| {
                str_field(a, "name")
                    .or_else(|| str_field(a, "url"))
                    .or_else(|| str_field(a, "id"))
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect::<Vec<_>>();
        let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
        let details: Vec<Option<String>> = vec![
            Some(format!("message_id={}", delete.id)),
            Some(format!(
                "author={} ({})",
                author_tag
                    .clone()
                    .unwrap_or_else(|| old.author_id.to_string()),
                old.author_id
            )),
            Some(if content.is_empty() {
                "content=[empty]".to_owned()
            } else {
                format!("content={content}")
            }),
            Some(if attachments_list.is_empty() {
                "attachments=[none]".to_owned()
            } else {
                format!("attachments={}", attachments_list.join(", "))
            }),
            actor_resolution_detail(audit.status),
            reason
                .as_deref()
                .map(|r| format!("reason={}", reason_text(r))),
        ];
        self.log(app, guild_id, json!({
            "category": "message",
            "action": "delete",
            "actor": actor_payload(deleter.as_ref()),
            "target": {
                "kind": "message",
                "id": delete.id.to_string(),
                "ownerId": old.author_id.to_string(),
                "ownerTag": author_tag,
                "avatar": author.as_ref().map(card::avatar_url)
            },
            "location": { "channelId": channel_id.to_string(), "channelName": channel_name(Some(channel_id)) },
            "summary": match &deleter {
                Some(d) => format!("Message deleted by moderator ({}).", discord::user_tag(d)),
                None => "Message deleted. Actor unknown.".to_owned(),
            },
            "details": details.into_iter().flatten().collect::<Vec<_>>(),
            "metadata": {
                "messageId": delete.id.to_string(),
                "channelId": channel_id.to_string(),
                "ownerId": old.author_id.to_string(),
                "ownerTag": author_tag,
                "deletedBy": deleter.as_ref().map(|d| d.id.to_string()),
                "actorResolution": actor_resolution(audit.status),
                "reason": reason,
                "content": content,
                "attachments": old.attachments
            }
        }), false);
        Ok(())
    }

    pub(in super::super) async fn on_message_delete_bulk(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let Event::MessageDeleteBulk(bulk) = event else {
            return Ok(());
        };
        let channel_name = |id: Option<Id<ChannelMarker>>| -> Value {
            id.and_then(|id| app.cache.channel(id).and_then(|c| c.name.clone()))
                .map_or(Value::Null, Value::String)
        };
        let Some(guild_id) = bulk.guild_id else {
            return Ok(());
        };
        let audit = self
            .resolve_audit(
                app,
                guild_id,
                AuditLogEventType::MessageBulkDelete,
                None,
                |_| true,
            )
            .await;
        let executor = audit.executor.clone();
        let name = channel_name(Some(bulk.channel_id));
        let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
        let mut details = vec![
            format!("count={}", bulk.ids.len()),
            format!("channel=#{}", name.as_str().unwrap_or("unknown")),
        ];
        if let Some(reason) = &reason {
            details.push(format!("reason={}", reason_text(reason)));
        }
        self.log(
            app,
            guild_id,
            json!({
                "category": "message",
                "action": "bulk_delete",
                "actor": actor_payload(executor.as_ref()),
                "target": { "kind": "channel", "id": bulk.channel_id.to_string(), "name": name },
                "location": { "channelId": bulk.channel_id.to_string(), "channelName": name },
                "summary": format!("{} messages deleted in bulk.", bulk.ids.len()),
                "details": details,
                "metadata": {
                    "count": bulk.ids.len(),
                    "channelId": bulk.channel_id.to_string(),
                    "deletedBy": executor.as_ref().map(|e| e.id.to_string()),
                    "reason": reason
                }
            }),
            false,
        );
        Ok(())
    }

    pub(in super::super) async fn on_reaction(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let channel_name = |id: Option<Id<ChannelMarker>>| -> Value {
            id.and_then(|id| app.cache.channel(id).and_then(|c| c.name.clone()))
                .map_or(Value::Null, Value::String)
        };
        let user_from_cache = |id: Id<UserMarker>| app.cache.user(id).map(|u| u.clone());
        let (reaction, added) = match event {
            Event::ReactionAdd(r) => (&r.0, true),
            Event::ReactionRemove(r) => (&r.0, false),
            _ => return Ok(()),
        };
        let Some(guild_id) = reaction.guild_id else {
            return Ok(());
        };
        if reaction.user_id == app.bot_id || app.cache.message(reaction.message_id).is_none() {
            return Ok(());
        }
        let user = reaction
            .member
            .as_ref()
            .map(|m| m.user.clone())
            .or_else(|| user_from_cache(reaction.user_id));
        let Some(user) = user else { return Ok(()) };
        use twilight_model::channel::message::EmojiReactionType;
        let (identifier, display) = match &reaction.emoji {
            EmojiReactionType::Custom { id, name, animated } => {
                let name = name.clone().unwrap_or_default();
                (
                    format!("{}{name}:{id}", if *animated { "a:" } else { "" }),
                    format!("<{}:{name}:{id}>", if *animated { "a" } else { "" }),
                )
            }
            EmojiReactionType::Unicode { name } => (name.clone(), name.clone()),
        };
        let verb = if added { "added" } else { "removed" };
        self.log(app, guild_id, json!({
            "category": "reaction",
            "action": if added { "add" } else { "remove" },
            "actor": actor_payload(Some(&user)),
            "target": { "kind": "message", "id": reaction.message_id.to_string() },
            "location": { "channelId": reaction.channel_id.to_string(), "channelName": channel_name(Some(reaction.channel_id)) },
            "summary": format!("Reaction {verb}: {display}."),
            "details": [format!("emoji={identifier}"), format!("message_id={}", reaction.message_id)],
            "metadata": {
                "emoji": identifier,
                "channelId": reaction.channel_id.to_string(),
                "messageId": reaction.message_id.to_string()
            }
        }), false);
        Ok(())
    }
}
