//! `/logs` (port of `src/cmds/logs.js`).

use anyhow::Result;
use serde_json::json;
use twilight_model::guild::Permissions;

use crate::discord::{self, Ix, panel};
use crate::modules::fulllogs::{
    LogsState, MAX_RETENTION_DAYS, MIN_RETENTION_DAYS, category_keys, category_meta,
    is_valid_day_key, normalize_disabled_categories, normalize_retention, resolve_logs_state,
};
use crate::util::text::inline;

fn enabled_categories(state: &LogsState) -> Vec<&'static str> {
    category_keys()
        .filter(|k| !state.disabled_categories.iter().any(|d| d == k))
        .collect()
}

fn format_category_list(keys: &[&str], empty: &str) -> String {
    if keys.is_empty() {
        return empty.to_owned();
    }
    keys.iter()
        .map(|key| match category_meta(key) {
            Some((label, icon)) => format!("{icon} {label}"),
            None => format!("• {key}"),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn disabled_list(state: &LogsState) -> Vec<&str> {
    state
        .disabled_categories
        .iter()
        .map(String::as_str)
        .collect()
}

fn total_categories() -> usize {
    category_keys().count()
}

fn status_lines(state: &LogsState, channel_status: Option<&str>) -> Vec<String> {
    let enabled = enabled_categories(state);
    let mut lines = vec![
        format!("Enabled: **{}**", if state.enabled { "yes" } else { "no" }),
        format!(
            "Channel: {}",
            state
                .channel_id
                .as_deref()
                .map_or("not set".to_owned(), |c| format!("<#{c}> (`{c}`)"))
        ),
    ];
    if let Some(status) = channel_status {
        lines.push(format!("Channel status: {status}"));
    }
    lines.extend([
        format!("Retention: **{} days**", state.retention_days),
        format!(
            "Include bots: **{}**",
            if state.include_bots { "yes" } else { "no" }
        ),
        format!(
            "Forward mode: **{}**",
            if state.forward_mode.is_empty() {
                "all"
            } else {
                &state.forward_mode
            }
        ),
        format!(
            "Enabled categories ({}/{}): {}",
            enabled.len(),
            total_categories(),
            format_category_list(&enabled, "none")
        ),
        format!(
            "Disabled categories: {}",
            format_category_list(&disabled_list(state), "none")
        ),
        String::new(),
        "Modes:".to_owned(),
    ]);
    lines.extend(
        [
            "`/logs setup channel:#channel`",
            "`/logs status`",
            "`/logs enable`",
            "`/logs disable`",
            "`/logs channel channel:#channel`",
            "`/logs retention retention_days:30`",
            "`/logs include_bots enabled:true|false`",
            "`/logs categories`",
            "`/logs categories category:message enabled:false`",
            "`/logs preview`",
            "`/logs export day:2026-06-02`",
            "`/logs purge day:2026-06-02 confirm:true`",
            "`/logs purge older_than_days:30 confirm:true`",
            "`/logs test`",
        ]
        .map(str::to_owned),
    );
    lines
}

fn categories_lines(state: &LogsState) -> Vec<String> {
    let enabled = enabled_categories(state);
    vec![
        format!(
            "Enabled ({}/{}): {}",
            enabled.len(),
            total_categories(),
            format_category_list(&enabled, "none")
        ),
        format!(
            "Disabled: {}",
            format_category_list(&disabled_list(state), "none")
        ),
        String::new(),
        "Examples:".to_owned(),
        "`/logs categories category:message enabled:false`".to_owned(),
        "`/logs categories category:voice enabled:true`".to_owned(),
        "`/logs categories category:all enabled:true`".to_owned(),
    ]
}

fn lines(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

pub async fn execute(ix: &Ix<'_>) -> Result<()> {
    let Some(guild_id) = ix.guild_id() else {
        return ix
            .reply(panel(
                "Logs",
                &lines(&["This command only works in a server."]),
            ))
            .await;
    };
    if !ix.member_has(Permissions::ADMINISTRATOR) {
        return ix
            .reply(panel(
                "Logs",
                &lines(&["Administrator permission required."]),
            ))
            .await;
    }
    let app = ix.app;
    let guild = guild_id.to_string();
    let opts = ix.opts();
    let mode = match opts.subcommand.unwrap_or_default() {
        "export" => "export_day",
        other => other,
    }
    .to_owned();
    let current = resolve_logs_state(app, &guild);
    let picked_channel = || {
        opts.channel("channel")
            .or_else(|| ix.interaction.channel.as_ref().map(|c| (c.id, c.kind)))
            .filter(|(_, kind)| discord::is_text_based(*kind))
    };

    match mode.as_str() {
        "setup" => {
            let Some((channel, _)) = picked_channel() else {
                return ix
                    .reply(panel(
                        "Logs Setup",
                        &lines(&["The selected channel does not support text messages."]),
                    ))
                    .await;
            };
            let retention = normalize_retention(
                opts.integer("retention_days").map(|v| v as f64),
                current.retention_days,
            );
            app.state.save_logs(
                &guild,
                json!({
                    "enabled": true,
                    "channelId": channel.to_string(),
                    "retentionDays": retention,
                    "includeBots": opts.boolean("include_bots").unwrap_or(current.include_bots),
                    "forwardMode": if current.forward_mode.is_empty() { "all" } else { &current.forward_mode },
                    "disabledCategories": current.disabled_categories,
                }),
            );
            let next = resolve_logs_state(app, &guild);
            let categories = if next.disabled_categories.is_empty() {
                "all enabled".to_owned()
            } else {
                format!(
                    "{} enabled / {} disabled",
                    enabled_categories(&next).len(),
                    next.disabled_categories.len()
                )
            };
            ix.reply(panel(
                "Logs Setup",
                &[
                    format!(
                        "Logs enabled on {}.",
                        next.channel_id
                            .as_deref()
                            .map_or("unknown channel".to_owned(), |c| format!("<#{c}>"))
                    ),
                    format!("Retention: {} days", next.retention_days),
                    format!(
                        "Include bots: {}",
                        if next.include_bots { "yes" } else { "no" }
                    ),
                    format!("Categories: {categories}"),
                    "Run `/logs preview` or `/logs test` to verify output.".to_owned(),
                ],
            ))
            .await
        }
        "status" => {
            let status = current.channel_id.as_deref().map(|id| {
                match discord::parse_id(id).and_then(|cid| app.cache.channel(cid).map(|c| c.kind)) {
                    None => "missing",
                    Some(kind) if !discord::is_text_based(kind) => {
                        "channel exists, but not text based"
                    }
                    Some(_) => "ok",
                }
            });
            ix.reply(panel("Logs Status", &status_lines(&current, status)))
                .await
        }
        "enable" => {
            app.state.save_logs(&guild, json!({ "enabled": true }));
            let next = resolve_logs_state(app, &guild);
            ix.reply(panel(
                "Logs Enabled",
                &[
                    "Enabled: **yes**".to_owned(),
                    format!(
                        "Channel: {}",
                        next.channel_id.as_deref().map_or(
                            "not set (set with `/logs channel`)".to_owned(),
                            |c| format!("<#{c}>")
                        )
                    ),
                ],
            ))
            .await
        }
        "disable" => {
            app.state.save_logs(&guild, json!({ "enabled": false }));
            ix.reply(panel(
                "Logs Disabled",
                &lines(&[
                    "Enabled: **no**",
                    "Event tracking to channel has been paused.",
                ]),
            ))
            .await
        }
        "channel" => {
            let Some((channel, _)) = picked_channel() else {
                return ix
                    .reply(panel(
                        "Logs Channel",
                        &lines(&["The selected channel does not support text messages."]),
                    ))
                    .await;
            };
            app.state
                .save_logs(&guild, json!({ "channelId": channel.to_string() }));
            ix.reply(panel(
                "Logs Channel",
                &[format!("Log channel set to <#{channel}>.")],
            ))
            .await
        }
        "retention" => {
            let Some(picked) = opts.integer("retention_days") else {
                return ix
                    .reply(panel(
                        "Logs Retention",
                        &[format!(
                            "Provide `retention_days` ({MIN_RETENTION_DAYS}-{MAX_RETENTION_DAYS})."
                        )],
                    ))
                    .await;
            };
            let retention = normalize_retention(Some(picked as f64), current.retention_days);
            app.state
                .save_logs(&guild, json!({ "retentionDays": retention }));
            ix.reply(panel(
                "Logs Retention",
                &[format!("Retention set to **{retention}** days.")],
            ))
            .await
        }
        "include_bots" => {
            let Some(value) = opts.boolean("enabled") else {
                return ix
                    .reply(panel(
                        "Logs Include Bots",
                        &lines(&["Provide `enabled:true` or `enabled:false`."]),
                    ))
                    .await;
            };
            app.state.save_logs(&guild, json!({ "includeBots": value }));
            ix.reply(panel(
                "Logs Include Bots",
                &[format!(
                    "Include bots: **{}**",
                    if value { "yes" } else { "no" }
                )],
            ))
            .await
        }
        "categories" => {
            let category = opts.lower("category").trim().to_owned();
            let value = opts.boolean("enabled");
            if category.is_empty() && value.is_none() {
                return ix
                    .reply(panel("Logs Categories", &categories_lines(&current)))
                    .await;
            }
            let (false, Some(value)) = (category.is_empty(), value) else {
                return ix
                    .reply(panel(
                        "Logs Categories",
                        &lines(&[
                            "To change a category, provide both `category` and `enabled`.",
                            "`enabled:true` turns the category on, `enabled:false` turns it off.",
                        ]),
                    ))
                    .await;
            };
            if category == "all" {
                if !value {
                    return ix
                        .reply(panel(
                            "Logs Categories",
                            &lines(&[
                                "For `category:all`, use only `enabled:true` to turn all categories on.",
                                "To turn everything off, use `/logs disable`.",
                            ]),
                        ))
                        .await;
                }
                app.state
                    .save_logs(&guild, json!({ "disabledCategories": [] }));
                let next = resolve_logs_state(app, &guild);
                let mut out = vec!["Enabled all log categories.".to_owned()];
                out.extend(categories_lines(&next));
                return ix.reply(panel("Logs Categories", &out)).await;
            }
            if !category_keys().any(|k| k == category) {
                return ix
                    .reply(panel(
                        "Logs Categories",
                        &[
                            format!("Unknown category: `{category}`."),
                            format!(
                                "Available: {}",
                                category_keys().collect::<Vec<_>>().join(", ")
                            ),
                        ],
                    ))
                    .await;
            }
            let mut disabled = current.disabled_categories.clone();
            if value {
                disabled.retain(|c| *c != category);
            } else {
                disabled.push(category.clone());
            }
            let disabled = normalize_disabled_categories(&disabled);
            let next_state = LogsState {
                disabled_categories: disabled.clone(),
                ..current.clone()
            };
            if enabled_categories(&next_state).is_empty() {
                return ix
                    .reply(panel(
                        "Logs Categories",
                        &lines(&[
                            "You cannot disable all categories at once.",
                            "Keep at least one enabled or use `/logs disable`.",
                        ]),
                    ))
                    .await;
            }
            app.state
                .save_logs(&guild, json!({ "disabledCategories": disabled }));
            let next = resolve_logs_state(app, &guild);
            let label = category_meta(&category).map_or(category.as_str(), |(label, _)| label);
            let mut out = vec![format!(
                "{} category **{label}**.",
                if value { "Enabled" } else { "Disabled" }
            )];
            out.extend(categories_lines(&next));
            ix.reply(panel("Logs Categories", &out)).await
        }
        "preview" => {
            let payload = app.fulllogs.preview_payload(guild_id).await;
            ix.reply(payload).await
        }
        "export_day" => {
            let day = opts.string("day").unwrap_or_default().trim().to_owned();
            if !is_valid_day_key(&day) {
                return ix
                    .reply(panel(
                        "Logs Export",
                        &lines(&["Provide `day` as `YYYY-MM-DD`."]),
                    ))
                    .await;
            }
            let exported = app.fulllogs.export_day(guild_id, &day).await;
            match exported {
                Ok(None) => {
                    ix.reply(panel("Logs Export", &[format!("No log for `{day}`.")]))
                        .await
                }
                Ok(Some((path, name))) => {
                    // A day of logs can be many MB: acknowledge first, then upload.
                    ix.defer(true).await?;
                    match tokio::fs::read(&path).await {
                        Ok(bytes) => {
                            ix.edit_with_file(&format!("Export of `{day}` is ready."), &name, bytes)
                                .await
                        }
                        Err(error) => {
                            ix.edit_content(format!(
                                "Could not export the log: {}",
                                inline(&error.to_string(), 220)
                            ))
                            .await
                        }
                    }
                }
                Err(error) => {
                    ix.reply(panel(
                        "Logs Export",
                        &[format!(
                            "Could not export the log: {}",
                            inline(&discord::err_text(&error), 220)
                        )],
                    ))
                    .await
                }
            }
        }
        "purge" => {
            let day = opts.string("day").unwrap_or_default().trim().to_owned();
            let older = opts.integer("older_than_days");
            if opts.boolean("confirm") != Some(true) {
                return ix
                    .reply(panel(
                        "Logs Purge",
                        &lines(&[
                            "Purge requires explicit confirmation `confirm:true`.",
                            "Examples:",
                            "`/logs purge day:2026-06-02 confirm:true`",
                            "`/logs purge older_than_days:30 confirm:true`",
                        ]),
                    ))
                    .await;
            }
            if day.is_empty() && older.is_none() {
                return ix
                    .reply(panel(
                        "Logs Purge",
                        &lines(&["Provide `day` or `older_than_days`."]),
                    ))
                    .await;
            }
            if !day.is_empty() && older.is_some() {
                return ix
                    .reply(panel(
                        "Logs Purge",
                        &lines(&["Choose only one range per purge: `day` or `older_than_days`."]),
                    ))
                    .await;
            }
            let day_opt = Some(day.as_str()).filter(|d| !d.is_empty());
            match app.fulllogs.purge(guild_id, day_opt, older).await {
                Ok(removed) => {
                    let preview = if removed.is_empty() {
                        "none".to_owned()
                    } else {
                        removed
                            .iter()
                            .take(10)
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    let mut out = vec![
                        if day_opt.is_some() {
                            if removed.is_empty() {
                                format!("No log found for: `{day}`.")
                            } else {
                                format!("Deleted the log for: `{day}`.")
                            }
                        } else {
                            format!(
                                "Deleted logs older than **{}** days.",
                                older.unwrap_or(1).max(1)
                            )
                        },
                        format!("Deleted files: **{}**", removed.len()),
                        format!("Deleted days: {preview}"),
                    ];
                    if removed.len() > 10 {
                        out.push(format!("... and {} more days.", removed.len() - 10));
                    }
                    ix.reply(panel("Logs Purge", &out)).await
                }
                Err(error) => {
                    ix.reply(panel(
                        "Logs Purge",
                        &[format!(
                            "Purge failed: {}",
                            inline(&discord::err_text(&error), 220)
                        )],
                    ))
                    .await
                }
            }
        }
        "test" => {
            let latest = resolve_logs_state(app, &guild);
            let Some(channel) = latest.channel_id.as_deref().filter(|_| latest.enabled) else {
                return ix
                    .reply(panel(
                        "Logs Test",
                        &lines(&[
                            "First set a channel and enable logs:",
                            "`/logs setup channel:#channel`",
                        ]),
                    ))
                    .await;
            };
            app.fulllogs.send_test_log(app, guild_id, ix.user());
            ix.reply(panel(
                "Logs Test",
                &[
                    format!("Sent a test entry to <#{channel}>."),
                    "Check that the Components v2 card is readable.".to_owned(),
                ],
            ))
            .await
        }
        _ => {
            ix.reply(panel("Logs", &[format!("Unknown mode: `{mode}`.")]))
                .await
        }
    }
}
