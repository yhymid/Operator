//! `/dm` (port of `src/cmds/dm.js`).

use anyhow::Result;
use twilight_model::guild::Permissions;

use crate::discord::{self, Ix};

const BULK_DM_CONFIRM_THRESHOLD: usize = 10;

fn normalize_message(value: &str) -> Result<String, &'static str> {
    let text = value.trim();
    if text.is_empty() {
        return Err("Wiadomość nie może być pusta.");
    }
    if discord::js_len(text) > 2000 {
        return Err("Wiadomość jest za długa. Limit DM to 2000 znaków.");
    }
    Ok(text.to_owned())
}

fn failure_preview(failures: &[(String, String)], limit: usize) -> String {
    let preview = failures
        .iter()
        .take(limit)
        .map(|(label, reason)| format!("{label}: {reason}"))
        .collect::<Vec<_>>()
        .join("\n");
    if failures.len() <= limit {
        preview
    } else {
        format!("{preview}\n... i jeszcze {} błędów", failures.len() - limit)
    }
}

pub async fn execute(ix: &Ix<'_>) -> Result<()> {
    let Some(guild_id) = ix.guild_id() else {
        return ix
            .reply_ephemeral("Ta komenda dziala tylko na serwerze.")
            .await;
    };
    if !ix.member_has(Permissions::ADMINISTRATOR) {
        return ix.reply_ephemeral("Brak uprawnień Administratora.").await;
    }
    let opts = ix.opts();
    let target_user = opts.user("user");
    let target_role = opts.role("role");
    let confirm = opts.boolean("confirm") == Some(true);
    if target_user.is_some() == target_role.is_some() {
        return ix
            .reply_ephemeral("Podaj dokładnie jedno pole targetu: `user` albo `role`.")
            .await;
    }
    let message = match normalize_message(&opts.string("message").unwrap_or_default()) {
        Ok(message) => message,
        Err(text) => return ix.reply_ephemeral(text).await,
    };

    ix.defer(true).await?;

    if let Some(user) = target_user {
        return match discord::send_dm(ix.app, user.id, &message).await {
            Ok(()) => {
                ix.edit_content(format!("Wysłano DM do <@{}>.", user.id))
                    .await
            }
            Err(error) => {
                ix.edit_content(format!(
                    "Nie udało się wysłać DM do <@{}>: {}",
                    user.id,
                    discord::err_text(&error)
                ))
                .await
            }
        };
    }
    let Some(role) = target_role else {
        return Ok(());
    };

    let Some(members) = discord::fetch_all_members(ix.app, guild_id).await else {
        return ix
            .edit_content("Nie udało się pobrać listy członków serwera.")
            .await;
    };
    let targets: Vec<_> = members
        .into_iter()
        .filter(|m| m.roles.contains(&role.id) && !m.user.bot)
        .map(|m| m.user)
        .collect();
    if targets.is_empty() {
        return ix
            .edit_content(format!(
                "Rola <@&{}> nie ma żadnych odbiorców do wysyłki.",
                role.id
            ))
            .await;
    }
    if targets.len() > BULK_DM_CONFIRM_THRESHOLD && !confirm {
        let preview = targets
            .iter()
            .take(10)
            .map(|u| format!("- @{}", u.name))
            .collect::<Vec<_>>()
            .join("\n");
        return ix
            .edit_content(
                [
                    format!(
                        "Ta akcja wyśle DM do {} osób z roli <@&{}>.",
                        targets.len(),
                        role.id
                    ),
                    "Dla bezpieczeństwa masowa wysyłka wymaga dodatkowego potwierdzenia."
                        .to_owned(),
                    String::new(),
                    "Podgląd odbiorców:".to_owned(),
                    preview,
                    String::new(),
                    "Uruchom komendę ponownie z `confirm:true`, jeśli chcesz kontynuować."
                        .to_owned(),
                ]
                .join("\n"),
            )
            .await;
    }

    let mut sent = 0;
    let mut failures = Vec::new();
    for user in &targets {
        match discord::send_dm(ix.app, user.id, &message).await {
            Ok(()) => sent += 1,
            Err(error) => failures.push((format!("@{}", user.name), discord::err_text(&error))),
        }
    }
    let mut lines = vec![
        format!("Wysyłka DM do roli <@&{}> zakończona.", role.id),
        format!("Targety: {}", targets.len()),
        format!("Sukces: {sent}"),
        format!("Błędy: {}", failures.len()),
    ];
    let preview = failure_preview(&failures, 5);
    if !preview.is_empty() {
        lines.push(String::new());
        lines.push("Podgląd błędów:".to_owned());
        lines.push(preview);
    }
    ix.edit_content(lines.join("\n")).await
}
