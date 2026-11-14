#[allow(clippy::wildcard_imports)]
use super::super::*;

impl FullLogs {
    pub(in super::super) async fn on_channel_create(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let Event::ChannelCreate(channel) = event else {
            return Ok(());
        };
        let Some(guild_id) = channel.guild_id else {
            return Ok(());
        };
        let audit = self
            .resolve_audit(
                app,
                guild_id,
                AuditLogEventType::ChannelCreate,
                Some(channel.id.get()),
                |_| true,
            )
            .await;
        let actor = audit.executor.clone();
        let name = channel.name.clone();
        self.log(app, guild_id, json!({
            "category": "channel",
            "action": "create",
            "actor": actor_payload(actor.as_ref()),
            "target": { "kind": "channel", "id": channel.id.to_string(), "name": name },
            "summary": format!("Channel created: #{}.", display_name(name.as_deref())),
            "details": [
                format!("channel_id={}", channel.id),
                format!("type={}", channel_kind_number(channel)),
                format!("parent_id={}", channel.parent_id.map_or("none".to_owned(), |p| p.to_string())),
            ],
            "metadata": {
                "channelId": channel.id.to_string(),
                "type": channel_kind_number(channel),
                "parentId": channel.parent_id.map(|p| p.to_string()),
                "actorId": actor.as_ref().map(|a| a.id.to_string())
            }
        }), false);
        Ok(())
    }

    pub(in super::super) async fn on_channel_delete(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let Event::ChannelDelete(channel) = event else {
            return Ok(());
        };
        let Some(guild_id) = channel.guild_id else {
            return Ok(());
        };
        let audit = self
            .resolve_audit(
                app,
                guild_id,
                AuditLogEventType::ChannelDelete,
                Some(channel.id.get()),
                |_| true,
            )
            .await;
        let actor = audit.executor.clone();
        let name = channel.name.clone();
        let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
        let details: Vec<String> = [
            Some(format!("channel_id={}", channel.id)),
            Some(format!("type={}", channel_kind_number(channel))),
            actor_resolution_detail(audit.status),
            reason
                .as_deref()
                .map(|r| format!("reason={}", reason_text(r))),
        ]
        .into_iter()
        .flatten()
        .collect();
        self.log(
            app,
            guild_id,
            json!({
                "category": "channel",
                "action": "delete",
                "actor": actor_payload(actor.as_ref()),
                "target": { "kind": "channel", "id": channel.id.to_string(), "name": name },
                "summary": if actor.is_some() {
                    format!("Channel deleted: #{}.", display_name(name.as_deref()))
                } else {
                    format!("Channel deleted: #{}. Actor unknown.", display_name(name.as_deref()))
                },
                "details": details,
                "metadata": {
                    "channelId": channel.id.to_string(),
                    "actorId": actor.as_ref().map(|a| a.id.to_string()),
                    "actorResolution": actor_resolution(audit.status),
                    "reason": reason
                }
            }),
            false,
        );
        Ok(())
    }

    pub(in super::super) async fn on_channel_update(
        &self,
        app: &SharedApp,
        event: &Event,
        before: Before,
    ) -> anyhow::Result<()> {
        let Event::ChannelUpdate(update) = event else {
            return Ok(());
        };
        let Some(guild_id) = update.guild_id else {
            return Ok(());
        };
        let Before::Channel(Some(old)) = before else {
            return Ok(());
        };
        let new = &update.0;
        let mut details = Vec::new();
        if let Some(d) = format_state_delta(
            old.name.as_deref().unwrap_or_default(),
            new.name.as_deref().unwrap_or_default(),
        ) {
            details.push(format!("name={d}"));
        }
        if let Some(d) = format_state_delta(
            old.topic.as_deref().unwrap_or_default(),
            new.topic.as_deref().unwrap_or_default(),
        ) {
            details.push(format!("topic={d}"));
        }
        if let Some(d) = format_state_delta(&opt_bool(old.nsfw), &opt_bool(new.nsfw)) {
            details.push(format!("nsfw={d}"));
        }
        if let Some(d) = format_state_delta(
            &opt_num(old.rate_limit_per_user),
            &opt_num(new.rate_limit_per_user),
        ) {
            details.push(format!("slowmode_sec={d}"));
        }
        if let Some(d) = format_state_delta(
            &old.parent_id.map(|p| p.to_string()).unwrap_or_default(),
            &new.parent_id.map(|p| p.to_string()).unwrap_or_default(),
        ) {
            details.push(format!("parent_id={d}"));
        }
        if details.is_empty() {
            return Ok(());
        }
        let audit = self
            .resolve_audit(
                app,
                guild_id,
                AuditLogEventType::ChannelUpdate,
                Some(new.id.get()),
                |_| true,
            )
            .await;
        let actor = audit.executor.clone();
        self.log(
            app,
            guild_id,
            json!({
                "category": "channel",
                "action": "update",
                "actor": actor_payload(actor.as_ref()),
                "target": { "kind": "channel", "id": new.id.to_string(), "name": new.name },
                "summary": format!("Channel updated: #{}.", display_name(new.name.as_deref())),
                "details": details,
                "metadata": {
                    "channelId": new.id.to_string(),
                    "actorId": actor.as_ref().map(|a| a.id.to_string()),
                    "reason": audit.entry.as_ref().and_then(|e| e.reason.clone()),
                    "changes": details
                }
            }),
            false,
        );
        Ok(())
    }

    pub(in super::super) async fn on_thread_create(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let Event::ThreadCreate(thread) = event else {
            return Ok(());
        };
        let channel_name = |id: Option<Id<ChannelMarker>>| -> Value {
            id.and_then(|id| app.cache.channel(id).and_then(|c| c.name.clone()))
                .map_or(Value::Null, Value::String)
        };
        let Some(guild_id) = thread.guild_id else {
            return Ok(());
        };
        let archived = thread.thread_metadata.as_ref().is_some_and(|m| m.archived);
        self.log(app, guild_id, json!({
            "category": "thread",
            "action": "create",
            "actor": thread.owner_id.map_or(Value::Null, |o| json!({ "id": o.to_string(), "tag": null, "bot": false })),
            "target": { "kind": "thread", "id": thread.id.to_string(), "name": thread.name },
            "location": { "channelId": thread.parent_id.map(|p| p.to_string()), "channelName": channel_name(thread.parent_id) },
            "summary": format!("Thread created: {}.", display_name(thread.name.as_deref())),
            "details": [
                format!("thread_id={}", thread.id),
                format!("parent_id={}", thread.parent_id.map_or("unknown".to_owned(), |p| p.to_string())),
                format!("archived={archived}"),
            ],
            "metadata": { "threadId": thread.id.to_string(), "parentId": thread.parent_id.map(|p| p.to_string()) }
        }), false);
        Ok(())
    }

    pub(in super::super) async fn on_thread_delete(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let Event::ThreadDelete(thread) = event else {
            return Ok(());
        };
        let channel_name = |id: Option<Id<ChannelMarker>>| -> Value {
            id.and_then(|id| app.cache.channel(id).and_then(|c| c.name.clone()))
                .map_or(Value::Null, Value::String)
        };
        let name = app.cache.channel(thread.id).and_then(|c| c.name.clone());
        self.log(app, thread.guild_id, json!({
            "category": "thread",
            "action": "delete",
            "target": { "kind": "thread", "id": thread.id.to_string(), "name": name },
            "location": { "channelId": thread.parent_id.to_string(), "channelName": channel_name(Some(thread.parent_id)) },
            "summary": format!("Thread deleted: {}.", display_name(name.as_deref())),
            "details": [format!("thread_id={}", thread.id), format!("parent_id={}", thread.parent_id)],
            "metadata": { "threadId": thread.id.to_string(), "parentId": thread.parent_id.to_string() }
        }), false);
        Ok(())
    }

    pub(in super::super) async fn on_thread_update(
        &self,
        app: &SharedApp,
        event: &Event,
        before: Before,
    ) -> anyhow::Result<()> {
        let Event::ThreadUpdate(update) = event else {
            return Ok(());
        };
        let channel_name = |id: Option<Id<ChannelMarker>>| -> Value {
            id.and_then(|id| app.cache.channel(id).and_then(|c| c.name.clone()))
                .map_or(Value::Null, Value::String)
        };
        let Some(guild_id) = update.guild_id else {
            return Ok(());
        };
        let Before::Channel(Some(old)) = before else {
            return Ok(());
        };
        let new = &update.0;
        let meta = |c: &Channel| {
            c.thread_metadata
                .as_ref()
                .map(|m| (m.archived, m.locked))
                .unwrap_or_default()
        };
        let (old_archived, old_locked) = meta(&old);
        let (new_archived, new_locked) = meta(new);
        let mut details = Vec::new();
        if let Some(d) = format_state_delta(
            old.name.as_deref().unwrap_or_default(),
            new.name.as_deref().unwrap_or_default(),
        ) {
            details.push(format!("name={d}"));
        }
        if let Some(d) = delta_bool(old_archived, new_archived) {
            details.push(format!("archived={d}"));
        }
        if let Some(d) = delta_bool(old_locked, new_locked) {
            details.push(format!("locked={d}"));
        }
        if details.is_empty() {
            return Ok(());
        }
        self.log(app, guild_id, json!({
            "category": "thread",
            "action": "update",
            "target": { "kind": "thread", "id": new.id.to_string(), "name": new.name },
            "location": { "channelId": new.parent_id.map(|p| p.to_string()), "channelName": channel_name(new.parent_id) },
            "summary": format!("Thread updated: {}.", display_name(new.name.as_deref())),
            "details": details,
            "metadata": { "threadId": new.id.to_string(), "parentId": new.parent_id.map(|p| p.to_string()), "changes": details }
        }), false);
        Ok(())
    }
}
