//! `/dm` (port of `src/cmds/dm.js`).

use anyhow::Result;
use twilight_model::guild::Permissions;

use crate::discord::{self, Ix};

const BULK_DM_CONFIRM_THRESHOLD: usize = 10;
/// Mass DMs are what gets bots flagged as spam; a role send is capped and paced.
const BULK_DM_MAX_RECIPIENTS: usize = 200;
const BULK_DM_PAUSE: std::time::Duration = std::time::Duration::from_secs(1);
/// Stop before the 15-minute interaction token expires, so the summary can still be posted.
const BULK_DM_BUDGET: std::time::Duration = std::time::Duration::from_secs(13 * 60);

fn normalize_message(value: &str) -> Result<String, &'static str> {
    let text = value.trim();
    if text.is_empty() {
        return Err("The message cannot be empty.");
    }
    if discord::js_len(text) > 2000 {
        return Err("The message is too long. The DM limit is 2000 characters.");
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
        format!("{preview}\n... and {} more errors", failures.len() - limit)
    }
}

pub async fn execute(ix: &Ix<'_>) -> Result<()> {
    let Some(guild_id) = ix.guild_id() else {
        return ix
            .reply_ephemeral("This command only works in a server.")
            .await;
    };
    if !ix.member_has(Permissions::ADMINISTRATOR) {
        return ix
            .reply_ephemeral("Administrator permission required.")
            .await;
    }
    let opts = ix.opts();
    let target_user = opts.user("user");
    let target_role = opts.role("role");
    let confirm = opts.boolean("confirm") == Some(true);
    if target_user.is_some() == target_role.is_some() {
        return ix
            .reply_ephemeral("Provide exactly one target: `user` or `role`.")
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
                ix.edit_content(format!("Sent a DM to <@{}>.", user.id))
                    .await
            }
            Err(error) => {
                ix.edit_content(format!(
                    "Could not send a DM to <@{}>: {}",
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
    // A server admin may DM one member; a role-wide send goes out under the bot's name to
    // many people, so it is reserved for the bot owner.
    if !ix.app.is_owner(ix.user_id()) {
        return ix
            .edit_content("Sending a DM to a whole role is only available to the bot owner.")
            .await;
    }

    let Some(members) = discord::fetch_all_members(ix.app, guild_id).await else {
        return ix
            .edit_content("Could not fetch the server member list.")
            .await;
    };
    let targets: Vec<_> = members
        .into_iter()
        .filter(|m| m.roles.contains(&role.id) && !m.user.bot)
        .map(|m| m.user)
        .collect();
    if targets.is_empty() {
        return ix
            .edit_content(format!("Role <@&{}> has no recipients.", role.id))
            .await;
    }
    if targets.len() > BULK_DM_MAX_RECIPIENTS {
        return ix
            .edit_content(format!(
                "Role <@&{}> has {} recipients; the limit per send is {BULK_DM_MAX_RECIPIENTS}.",
                role.id,
                targets.len()
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
                        "This will send a DM to {} members of <@&{}>.",
                        targets.len(),
                        role.id
                    ),
                    "For safety, a bulk send needs an extra confirmation.".to_owned(),
                    String::new(),
                    "Recipient preview:".to_owned(),
                    preview,
                    String::new(),
                    "Run the command again with `confirm:true` to continue.".to_owned(),
                ]
                .join("\n"),
            )
            .await;
    }

    let mut sent = 0;
    let mut failures = Vec::new();
    let started = std::time::Instant::now();
    let mut stopped_early = false;
    for (index, user) in targets.iter().enumerate() {
        if started.elapsed() > BULK_DM_BUDGET {
            stopped_early = true;
            break;
        }
        if index > 0 {
            tokio::time::sleep(BULK_DM_PAUSE).await;
        }
        match discord::send_dm(ix.app, user.id, &message).await {
            Ok(()) => sent += 1,
            Err(error) => failures.push((format!("@{}", user.name), discord::err_text(&error))),
        }
    }
    let mut lines = vec![
        if stopped_early {
            format!(
                "DM send to <@&{}> stopped: the Discord response time is running out.",
                role.id
            )
        } else {
            format!("DM send to <@&{}> finished.", role.id)
        },
        format!("Targety: {}", targets.len()),
        format!("Sukces: {sent}"),
        format!("Errors: {}", failures.len()),
    ];
    let preview = failure_preview(&failures, 5);
    if !preview.is_empty() {
        lines.push(String::new());
        lines.push("Error preview:".to_owned());
        lines.push(preview);
    }
    ix.edit_content(lines.join("\n")).await
}
