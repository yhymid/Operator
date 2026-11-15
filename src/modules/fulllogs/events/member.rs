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
            "target": { "kind": "member", "id": user.id.to_string(), "tag": discord::user_tag(user), "avatar": card::avatar_url(user) },
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
            "target": { "kind": "member", "id": user.id.to_string(), "tag": discord::user_tag(user), "avatar": card::avatar_url(user) },
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
        let Before::Member(Some((old_roles, old_nick, old_timeout))) = before else {
            return Ok(());
        };
        let user = &update.user;
        let target = json!({ "kind": "member", "id": user.id.to_string(), "tag": discord::user_tag(user), "avatar": card::avatar_url(user) });

        // Timeouts are a moderation action of their own (Quark/Dyno log them separately).
        let now = chrono::Utc::now().timestamp();
        let new_timeout = update
            .communication_disabled_until
            .map(|t| t.as_secs())
            .filter(|until| *until > now);
        let old_timeout = old_timeout.filter(|until| *until > now);
        if new_timeout != old_timeout {
            let audit = self
                .resolve_audit(
                    app,
                    update.guild_id,
                    AuditLogEventType::MemberUpdate,
                    Some(user.id.get()),
                    |_| true,
                )
                .await;
            let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
            let (action, summary) = match new_timeout {
                Some(_) => ("timeout", "Member timed out."),
                None => ("timeout_remove", "Timeout removed."),
            };
            let details: Vec<String> = new_timeout
                .map(|until| format!("until=<t:{until}:F> (<t:{until}:R>)"))
                .into_iter()
                .collect();
            self.log(
                app,
                update.guild_id,
                json!({
                    "category": "moderation",
                    "action": action,
                    "actor": actor_payload(audit.executor.as_ref()),
                    "target": target,
                    "summary": summary,
                    "details": details,
                    "metadata": {
                        "userId": user.id.to_string(),
                        "until": new_timeout,
                        "reason": reason,
                        "actorResolution": actor_resolution(audit.status)
                    }
                }),
                false,
            );
        }

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
        let roles_text = |ids: &[&String]| {
            ids.iter()
                .map(|r| format!("<@&{r}>"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut details = Vec::new();
        if let Some(delta) = &nick_delta {
            details.push(format!("nickname={delta}"));
        }
        if !added.is_empty() {
            details.push(format!("roles_added={}", roles_text(&added)));
        }
        if !removed.is_empty() {
            details.push(format!("roles_removed={}", roles_text(&removed)));
        }
        // Who changed it: roles come from the audit log; a nickname may be the member's own.
        let roles_changed = !added.is_empty() || !removed.is_empty();
        let audit = self
            .resolve_audit(
                app,
                update.guild_id,
                if roles_changed {
                    AuditLogEventType::MemberRoleUpdate
                } else {
                    AuditLogEventType::MemberUpdate
                },
                Some(user.id.get()),
                |_| true,
            )
            .await;
        let actor = audit.executor.clone().or_else(|| {
            // Without an audit entry only a nickname change can be attributed to the member.
            (!roles_changed).then(|| user.clone())
        });
        let summary = match (roles_changed, nick_delta.is_some()) {
            (true, true) => "Member roles and nickname updated.",
            (true, false) => "Member roles updated.",
            _ => "Member nickname updated.",
        };
        self.log(
            app,
            update.guild_id,
            json!({
                "category": "member",
                "action": if roles_changed { "roles_update" } else { "nickname_update" },
                "actor": actor_payload(actor.as_ref()),
                "target": target,
                "summary": summary,
                "details": details,
                "metadata": {
                    "userId": user.id.to_string(),
                    "nicknameDelta": nick_delta,
                    "addedRoles": added,
                    "removedRoles": removed,
                    "actorResolution": actor_resolution(audit.status)
                }
            }),
            false,
        );
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
