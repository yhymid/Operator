#[allow(clippy::wildcard_imports)]
use super::super::*;

impl FullLogs {
    pub(in super::super) async fn on_member_add(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let Event::MemberAdd(member) = event else {
            return Ok(());
        };
        let user = &member.member.user;
        let created = snowflake_ms(user.id.get()) / 1000;
        let member_count = app
            .cache
            .guild(member.guild_id)
            .and_then(|g| g.member_count());
        self.log(app, member.guild_id, json!({
            "category": "member",
            "action": "join",
            "actor": actor_payload(Some(user)),
            "target": { "kind": "member", "id": user.id.to_string(), "tag": discord::user_tag(user) },
            "summary": "Member joined the server.",
            "details": [
                format!("account_created=<t:{created}:F>"),
                format!("member_count={}", member_count.map_or("unknown".to_owned(), |c| c.to_string())),
            ],
            "metadata": { "userId": user.id.to_string(), "memberCount": member_count }
        }), false);
        Ok(())
    }

    pub(in super::super) async fn on_member_remove(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let Event::MemberRemove(member) = event else {
            return Ok(());
        };
        let user = &member.user;
        let (kick, ban) = tokio::join!(
            self.resolve_audit(
                app,
                member.guild_id,
                AuditLogEventType::MemberKick,
                Some(user.id.get()),
                |_| true
            ),
            self.resolve_audit(
                app,
                member.guild_id,
                AuditLogEventType::MemberBanAdd,
                Some(user.id.get()),
                |_| true
            ),
        );
        let (action, audit) = if ban.entry.is_some() {
            ("ban", &ban)
        } else if kick.entry.is_some() {
            ("kick", &kick)
        } else if ban.status == AuditStatus::Miss && kick.status == AuditStatus::Miss {
            ("leave", &ban)
        } else {
            ("remove", &ban)
        };
        let status = if audit.entry.is_some() {
            AuditStatus::Matched
        } else if ban.status != AuditStatus::Miss {
            ban.status
        } else {
            kick.status
        };
        let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
        let actor = if audit.entry.is_some() {
            audit.executor.clone()
        } else {
            None
        };
        let summary = match action {
            "ban" => "Member banned.",
            "kick" => "Member kicked.",
            "leave" => "Member left the server.",
            _ => "Member left or was removed from the server. Actor unknown.",
        };
        let details: Vec<String> = [
            Some(format!("user={} ({})", discord::user_tag(user), user.id)),
            actor_resolution_detail(status),
            reason
                .as_deref()
                .map(|r| format!("reason={}", reason_text(r))),
        ]
        .into_iter()
        .flatten()
        .collect();
        self.log(app, member.guild_id, json!({
            "category": "member",
            "action": action,
            "actor": actor_payload(actor.as_ref()),
            "target": { "kind": "member", "id": user.id.to_string(), "tag": discord::user_tag(user) },
            "summary": summary,
            "details": details,
            "metadata": {
                "userId": user.id.to_string(),
                "reason": reason,
                "actorId": actor.as_ref().map(|a| a.id.to_string()),
                "actorResolution": actor_resolution(status),
                "action": action
            }
        }), false);
        Ok(())
    }

    pub(in super::super) async fn on_member_update(
        &self,
        app: &SharedApp,
        event: &Event,
        before: Before,
    ) -> anyhow::Result<()> {
        let Event::MemberUpdate(update) = event else {
            return Ok(());
        };
        let Before::Member(Some((old_roles, old_nick))) = before else {
            return Ok(());
        };
        let nick_delta = format_state_delta(
            old_nick.as_deref().unwrap_or_default(),
            update.nick.as_deref().unwrap_or_default(),
        );
        let new_roles: Vec<String> = update.roles.iter().map(ToString::to_string).collect();
        let added: Vec<&String> = new_roles
            .iter()
            .filter(|r| !old_roles.contains(r))
            .collect();
        let removed: Vec<&String> = old_roles
            .iter()
            .filter(|r| !new_roles.contains(r))
            .collect();
        if nick_delta.is_none() && added.is_empty() && removed.is_empty() {
            return Ok(());
        }
        let role_name = |id: &String| {
            discord::parse_id(id).and_then(|rid| app.cache.role(rid).map(|r| r.name.clone()))
        };
        let mut details = Vec::new();
        if let Some(delta) = &nick_delta {
            details.push(format!("nickname={delta}"));
        }
        if !added.is_empty() {
            details.push(format!(
                "roles_added={}",
                added
                    .iter()
                    .map(|r| format_role_ref(role_name(r).as_deref()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !removed.is_empty() {
            details.push(format!(
                "roles_removed={}",
                removed
                    .iter()
                    .map(|r| format_role_ref(role_name(r).as_deref()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let user = &update.user;
        self.log(app, update.guild_id, json!({
            "category": "member",
            "action": "update",
            "actor": actor_payload(Some(user)),
            "target": { "kind": "member", "id": user.id.to_string(), "tag": discord::user_tag(user) },
            "summary": "Member profile/roles updated.",
            "details": details,
            "metadata": {
                "userId": user.id.to_string(),
                "nicknameDelta": nick_delta,
                "addedRoles": added,
                "removedRoles": removed
            }
        }), false);
        Ok(())
    }

    pub(in super::super) async fn on_ban(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let (ban_guild, ban_user, added) = match event {
            Event::BanAdd(b) => (b.guild_id, &b.user, true),
            Event::BanRemove(b) => (b.guild_id, &b.user, false),
            _ => return Ok(()),
        };
        let kind = if added {
            AuditLogEventType::MemberBanAdd
        } else {
            AuditLogEventType::MemberBanRemove
        };
        let audit = self
            .resolve_audit(app, ban_guild, kind, Some(ban_user.id.get()), |_| true)
            .await;
        let actor = audit.executor.clone();
        let tag = discord::user_tag(ban_user);
        let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
        let verb = if added { "added" } else { "removed" };
        let summary = if actor.is_some() {
            format!("Ban {verb} for {tag}.")
        } else {
            format!("Ban {verb} for {tag}. Actor unknown.")
        };
        let details: Vec<String> = [
            Some(format!("target={tag} ({})", ban_user.id)),
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
            ban_guild,
            json!({
                "category": "moderation",
                "action": if added { "ban_add" } else { "ban_remove" },
                "actor": actor_payload(actor.as_ref()),
                "target": { "kind": "member", "id": ban_user.id.to_string(), "tag": tag },
                "summary": summary,
                "details": details,
                "metadata": {
                    "userId": ban_user.id.to_string(),
                    "actorId": actor.as_ref().map(|a| a.id.to_string()),
                    "actorResolution": actor_resolution(audit.status),
                    "reason": reason
                }
            }),
            false,
        );
        Ok(())
    }
}
