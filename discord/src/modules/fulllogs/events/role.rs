#[allow(clippy::wildcard_imports)]
use super::super::*;

impl FullLogs {
    pub(in super::super) async fn on_role_create(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let Event::RoleCreate(created) = event else {
            return Ok(());
        };
        let role = &created.role;
        let audit = self
            .resolve_audit(
                app,
                created.guild_id,
                AuditLogEventType::RoleCreate,
                Some(role.id.get()),
                |_| true,
            )
            .await;
        let actor = audit.executor.clone();
        self.log(app, created.guild_id, json!({
            "category": "role",
            "action": "create",
            "actor": actor_payload(actor.as_ref()),
            "target": { "kind": "role", "id": role.id.to_string(), "name": role.name },
            "summary": format!("Role created: {}.", role.name),
            "details": [
                format!("role_id={}", role.id),
                format!("color={}", role_hex(role)),
                format!("hoist={}", role.hoist),
                format!("mentionable={}", role.mentionable),
            ],
            "metadata": { "roleId": role.id.to_string(), "actorId": actor.as_ref().map(|a| a.id.to_string()) }
        }), false);
        Ok(())
    }

    pub(in super::super) async fn on_role_delete(
        &self,
        app: &SharedApp,
        event: &Event,
        before: Before,
    ) -> anyhow::Result<()> {
        let Event::RoleDelete(deleted) = event else {
            return Ok(());
        };
        let Before::Role(old) = before else {
            return Ok(());
        };
        let name = old.as_ref().map(|r| r.name.clone());
        let position = old
            .as_ref()
            .map_or("unknown".to_owned(), |r| r.position.to_string());
        let audit = self
            .resolve_audit(
                app,
                deleted.guild_id,
                AuditLogEventType::RoleDelete,
                Some(deleted.role_id.get()),
                |_| true,
            )
            .await;
        let actor = audit.executor.clone();
        let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
        let label = name.clone().unwrap_or_else(|| "role".to_owned());
        let details: Vec<String> = [
            Some(format!("role_id={}", deleted.role_id)),
            Some(format!("position={position}")),
            actor_resolution_detail(audit.status),
            reason
                .as_deref()
                .map(|r| format!("reason={}", reason_text(r))),
        ]
        .into_iter()
        .flatten()
        .collect();
        self.log(app, deleted.guild_id, json!({
            "category": "role",
            "action": "delete",
            "actor": actor_payload(actor.as_ref()),
            "target": { "kind": "role", "id": deleted.role_id.to_string(), "name": name },
            "summary": if actor.is_some() { format!("Role deleted: {label}.") } else { format!("Role deleted: {label}. Actor unknown.") },
            "details": details,
            "metadata": {
                "roleId": deleted.role_id.to_string(),
                "actorId": actor.as_ref().map(|a| a.id.to_string()),
                "actorResolution": actor_resolution(audit.status),
                "reason": reason
            }
        }), false);
        Ok(())
    }

    pub(in super::super) async fn on_role_update(
        &self,
        app: &SharedApp,
        event: &Event,
        before: Before,
    ) -> anyhow::Result<()> {
        let Event::RoleUpdate(update) = event else {
            return Ok(());
        };
        let Before::Role(Some(old)) = before else {
            return Ok(());
        };
        let new = &update.role;
        let mut details = Vec::new();
        let mut changes = 0;
        if let Some(d) = format_state_delta(&old.name, &new.name) {
            details.push(format!("Name: {d}"));
            changes += 1;
        }
        if let Some(d) = format_state_delta(&role_hex(&old), &role_hex(new)) {
            details.push(format!("Color: {d}"));
            changes += 1;
        }
        let hoist = delta_bool(old.hoist, new.hoist);
        let mentionable = delta_bool(old.mentionable, new.mentionable);
        if hoist.is_some() || mentionable.is_some() {
            let mut parts = Vec::new();
            if let Some(d) = hoist {
                parts.push(format!("hoist={d}"));
                changes += 1;
            }
            if let Some(d) = mentionable {
                parts.push(format!("mentionable={d}"));
                changes += 1;
            }
            details.push(format!("Display: {}", parts.join(", ")));
        }
        if let Some(d) = format_state_delta(&old.position.to_string(), &new.position.to_string()) {
            details.push(format!("Position: {d}"));
            changes += 1;
        }
        let added = permission_names(new.permissions - old.permissions);
        let removed = permission_names(old.permissions - new.permissions);
        if !added.is_empty() {
            details.push(format!(
                "Permissions (+): {}",
                format_list_with_limit(&added, 5)
            ));
            changes += 1;
        }
        if !removed.is_empty() {
            details.push(format!(
                "Permissions (-): {}",
                format_list_with_limit(&removed, 5)
            ));
            changes += 1;
        }
        if details.is_empty() {
            return Ok(());
        }
        let audit = self
            .resolve_audit(
                app,
                update.guild_id,
                AuditLogEventType::RoleUpdate,
                Some(new.id.get()),
                |_| true,
            )
            .await;
        let actor = audit.executor.clone();
        let reason = audit.entry.as_ref().and_then(|e| e.reason.clone());
        let mut all_details = details.clone();
        if let Some(reason) = &reason {
            all_details.push(format!("Reason: {}", reason_text(reason)));
        }
        self.log(app, update.guild_id, json!({
            "category": "role",
            "action": "update",
            "actor": actor_payload(actor.as_ref()),
            "target": { "kind": "role", "id": new.id.to_string(), "name": new.name },
            "summary": format!("Role updated: {} ({changes} change{}).", new.name, if changes == 1 { "" } else { "s" }),
            "details": all_details,
            "metadata": {
                "roleId": new.id.to_string(),
                "actorId": actor.as_ref().map(|a| a.id.to_string()),
                "reason": reason,
                "changes": details
            }
        }), false);
        Ok(())
    }
}
