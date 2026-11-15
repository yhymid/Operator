#[allow(clippy::wildcard_imports)]
use super::super::*;

impl FullLogs {
    pub(in super::super) async fn on_voice_state_update(
        &self,
        app: &SharedApp,
        event: &Event,
        before: Before,
    ) -> anyhow::Result<()> {
        let Event::VoiceStateUpdate(update) = event else {
            return Ok(());
        };
        let user_from_cache = |id: Id<UserMarker>| app.cache.user(id).map(|u| u.clone());
        let Some(guild_id) = update.guild_id else {
            return Ok(());
        };
        let user = update
            .member
            .as_ref()
            .map(|m| m.user.clone())
            .or_else(|| user_from_cache(update.user_id));
        let Some(user) = user else { return Ok(()) };
        let Before::Voice(old) = before else {
            return Ok(());
        };
        let (old_channel, old_mute, old_deaf, old_self_mute, old_self_deaf) =
            old.unwrap_or((None, false, false, false, false));
        let new_channel = update.channel_id;
        let label = |id: Option<Id<ChannelMarker>>| match id {
            None => "none".to_owned(),
            Some(id) => simplify_channel_name(
                app.cache
                    .channel(id)
                    .and_then(|c| c.name.clone())
                    .as_deref(),
            ),
        };
        let before_name = label(old_channel);
        let after_name = label(new_channel);
        let action = match (old_channel, new_channel) {
            (None, Some(_)) => "join",
            (Some(_), None) => "leave",
            (Some(a), Some(b)) if a != b => "move",
            _ => "state_update",
        };
        let mut details = Vec::new();
        if action != "state_update" {
            details.push(format!("from={before_name}"));
            details.push(format!("to={after_name}"));
        }
        if let Some(d) = delta_bool(old_mute, update.mute) {
            details.push(format!("Server mute: {d}"));
        }
        if let Some(d) = delta_bool(old_deaf, update.deaf) {
            details.push(format!("Server deaf: {d}"));
        }
        if let Some(d) = delta_bool(old_self_mute, update.self_mute) {
            details.push(format!("Self mute: {d}"));
        }
        if let Some(d) = delta_bool(old_self_deaf, update.self_deaf) {
            details.push(format!("Self deaf: {d}"));
        }
        if details.is_empty() {
            return Ok(());
        }
        let mut actor = actor_payload(Some(&user));
        let mut status = AuditStatus::Matched;
        if action == "move" {
            let audit = self
                .resolve_audit(
                    app,
                    guild_id,
                    AuditLogEventType::MemberMove,
                    Some(user.id.get()),
                    |entry| {
                        entry
                            .options
                            .as_ref()
                            .and_then(|o| o.count.as_deref())
                            .and_then(|c| c.parse::<f64>().ok())
                            .is_none_or(|c| c >= 1.0)
                    },
                )
                .await;
            match audit.executor {
                Some(executor) => actor = actor_payload(Some(&executor)),
                None => {
                    actor = Value::Null;
                    status = audit.status;
                }
            }
        }
        let location_name = if action == "move" {
            format!("{before_name} -> {after_name}")
        } else if after_name != "none" {
            after_name.clone()
        } else {
            before_name.clone()
        };
        let summary = match action {
            "join" => "User joined voice channel.".to_owned(),
            "leave" => "User left voice channel.".to_owned(),
            "move" if !actor.is_null() => "User moved between voice channels.".to_owned(),
            "move" => "User moved between voice channels. Actor unknown.".to_owned(),
            _ => format!("Voice state changed for {}.", discord::user_tag(&user)),
        };
        let mut all_details = details.clone();
        if action == "move"
            && let Some(detail) = actor_resolution_detail(status)
        {
            all_details.push(detail);
        }
        self.log(app, guild_id, json!({
            "category": "voice",
            "action": action,
            "actor": actor,
            "target": { "kind": "member", "id": user.id.to_string(), "tag": discord::user_tag(&user), "avatar": card::avatar_url(&user) },
            "location": {
                "channelId": new_channel.or(old_channel).map(|c| c.to_string()),
                "channelName": location_name
            },
            "summary": summary,
            "details": all_details,
            "metadata": {
                "userId": user.id.to_string(),
                "beforeChannel": old_channel.map(|c| c.to_string()),
                "afterChannel": new_channel.map(|c| c.to_string()),
                "changes": details,
                "actorResolution": if action == "move" { actor_resolution(status) } else { "event" }
            }
        }), false);
        Ok(())
    }
}
