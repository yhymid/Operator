//! `/announce` (port of `src/cmds/announce.js`).

use std::collections::HashSet;
use std::sync::OnceLock;

use anyhow::{Result, bail};
use regex::Regex;
use serde_json::{Map, Value, json};

use crate::discord::{self, FLAG_EPHEMERAL, Ix};
use crate::modules::announcements::{
    self as ann, Opt, ensure_shape, generate_code, generate_id, is_falsy, matches_reference,
    normalize_color, normalize_mode, normalize_optional_text, normalize_profile_name,
    normalize_url, parse_datetime, reference, stored_message_ids,
};
use operator_common::time::now_iso;

fn s<'v>(value: &'v Value, key: &str) -> Option<&'v str> {
    value[key].as_str().filter(|v| !v.is_empty())
}

fn run_at_ms(item: &Value) -> i64 {
    s(item, "runAt")
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .map_or(i64::MAX, |d| d.timestamp_millis())
}

fn utf16_prefix(text: &str, max: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().take(max).collect();
    String::from_utf16_lossy(&units)
}

fn is_valid_code(code: &str) -> bool {
    code.len() == 4
        && code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// `ensureAnnouncementCodes`
fn ensure_codes(items: &mut [Value]) -> bool {
    let mut used = HashSet::new();
    let mut mutated = false;
    for item in items.iter_mut() {
        let raw = s(item, "code")
            .or_else(|| s(item, "id"))
            .unwrap_or_default();
        let mut code = raw.trim().to_uppercase();
        if !is_valid_code(&code) || used.contains(&code) {
            loop {
                code = generate_code();
                if !used.contains(&code) {
                    break;
                }
            }
            item["code"] = json!(code);
            mutated = true;
        } else if item["code"].as_str() != Some(code.as_str()) {
            item["code"] = json!(code);
            mutated = true;
        }
        used.insert(code);
    }
    mutated
}

fn unused_code(items: &[Value]) -> String {
    let used: HashSet<String> = items
        .iter()
        .filter_map(|i| s(i, "code"))
        .map(|c| c.trim().to_uppercase())
        .filter(|c| is_valid_code(c))
        .collect();
    loop {
        let code = generate_code();
        if !used.contains(&code) {
            return code;
        }
    }
}

struct ContentRegexes {
    leading: [(Regex, &'static str); 6],
    line: [(Regex, &'static str); 5],
    br: Regex,
    blank: Regex,
}

fn content_regexes() -> &'static ContentRegexes {
    static RE: OnceLock<ContentRegexes> = OnceLock::new();
    RE.get_or_init(|| {
        let r = |p: &str| Regex::new(p).expect("static regex");
        ContentRegexes {
            leading: [
                (r(r"\s+(#{1,6}\s)"), "\n\n${1}"),
                (r(r"\s+(>{1,3}\s)"), "\n${1}"),
                (r(r"\s+(-#\s)"), "\n${1}"),
                (r(r"\s+-\s+"), "\n- "),
                (r(r"\s+\*\s+"), "\n* "),
                (r(r"\s+(\d+\.\s)"), "\n${1}"),
            ],
            line: [
                (r(r"^(\s*)(#{1,6})([^\s#])"), "${1}${2} ${3}"),
                (r(r"^(\s*)-#(\S)"), "${1}-# ${2}"),
                (r(r"^(\s*)(>{1,3})([^\s>])"), "${1}${2} ${3}"),
                (r(r"^(\s*)([-*])([^\s\-*#])"), "${1}${2} ${3}"),
                (r(r"^(\s*)(\d+\.)([^\s\d])"), "${1}${2} ${3}"),
            ],
            br: r(r"(?i)<br\s*/?>"),
            blank: r(r"\n{3,}"),
        }
    })
}

/// `applyLineBreakFormatting` from `normalizeAnnouncementContent`.
fn format_line_breaks(input: &str) -> String {
    let re = content_regexes();
    let text = input
        .replace("\r\n", "\n")
        .replace("\\n", "\n")
        .replace("\\t", "  ");
    let mut text = re.br.replace_all(&text, "\n").into_owned();
    if !text.contains('\n') {
        for (pattern, replacement) in &re.leading {
            text = pattern.replace_all(&text, *replacement).into_owned();
        }
    }
    let mut in_code = false;
    let lines: Vec<String> = text
        .split('\n')
        .map(|line| {
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                in_code = !in_code;
                return line.to_owned();
            }
            if in_code || trimmed.is_empty() {
                return line.to_owned();
            }
            let mut out = line.to_owned();
            for (pattern, replacement) in &re.line {
                out = pattern.replace(&out, *replacement).into_owned();
            }
            out
        })
        .collect();
    re.blank
        .replace_all(&lines.join("\n"), "\n\n")
        .trim()
        .to_owned()
}

fn normalize_required_content(value: &str) -> Result<String> {
    let raw = value.trim();
    if raw.is_empty() {
        bail!("Announcement content cannot be empty.");
    }
    Ok(format_line_breaks(raw))
}

fn normalize_content(value: Option<String>) -> Opt {
    match normalize_optional_text(value) {
        Opt::Set(v) => Opt::Set(format_line_breaks(&v)),
        other => other,
    }
}

fn list_embed(items: &[Value]) -> Value {
    let mut embed = json!({
        "color": 0x2f9e44,
        "title": "Announcements",
        "timestamp": now_iso(),
    });
    if items.is_empty() {
        embed["description"] = json!("No announcements.");
        return embed;
    }
    let fields: Vec<Value> = items
        .iter()
        .take(25)
        .map(|item| {
            let status = item["status"].as_str().unwrap_or("undefined");
            let ids = item["messageIds"].as_array().map_or(0, Vec::len);
            let mut lines = vec![
                format!(
                    "channel: <#{}>",
                    s(item, "channelId").unwrap_or("undefined")
                ),
                format!("time: <t:{}:F>", run_at_ms(item).div_euclid(1000)),
            ];
            if status != "sent"
                && let Some(code) = s(item, "code")
            {
                lines.push(format!("kod: `{code}`"));
            }
            if let Some(id) = s(item, "messageId") {
                lines.push(format!("messageId: `{id}`"));
            }
            if ids > 1 {
                lines.push(format!("parts: {ids}"));
            }
            let mode = s(item, "sendMode").unwrap_or(
                if s(item, "title").is_some() || s(item, "color").is_some() {
                    "embed"
                } else {
                    "message"
                },
            );
            lines.push(format!("mode: {mode}"));
            for (key, label) in [
                ("profileName", "profile"),
                ("title", "title"),
                ("buttonLabel", "button"),
                ("lastError", "error"),
            ] {
                if let Some(v) = s(item, key) {
                    lines.push(if key == "profileName" {
                        format!("{label}: `{v}`")
                    } else {
                        format!("{label}: {v}")
                    });
                }
            }
            json!({
                "name": format!("{} • {status}", reference(item)),
                "value": lines.join("\n"),
                "inline": false,
            })
        })
        .collect();
    embed["fields"] = json!(fields);
    embed
}

fn sorted_profile_names(profiles: &Map<String, Value>) -> Vec<String> {
    let mut names: Vec<String> = profiles.keys().cloned().collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

fn profile_list_embed(profiles: &Map<String, Value>) -> Value {
    let mut embed = json!({
        "color": 0x1864ab,
        "title": "Announcement Profiles",
        "timestamp": now_iso(),
    });
    if profiles.is_empty() {
        embed["description"] = json!("No profiles.");
        return embed;
    }
    let fields: Vec<Value> = sorted_profile_names(profiles)
        .into_iter()
        .take(25)
        .map(|name| {
            let profile = &profiles[&name];
            let mut lines = vec![format!(
                "channel: <#{}>",
                s(profile, "channelId").unwrap_or("undefined")
            )];
            for (key, label) in [
                ("title", "title"),
                ("color", "color"),
                ("username", "username"),
                ("avatarURL", "avatar"),
            ] {
                if let Some(v) = s(profile, key) {
                    lines.push(format!("{label}: {v}"));
                }
            }
            json!({ "name": name, "value": lines.join("\n"), "inline": false })
        })
        .collect();
    embed["fields"] = json!(fields);
    embed
}

fn ephemeral_embed(embed: Value) -> Value {
    json!({ "embeds": [embed], "flags": FLAG_EPHEMERAL })
}

/// `opt || profile?.field || null` resolution used by create/now.
fn pick(opt: &Opt, profile: Option<&Value>, key: &str) -> Value {
    if opt.is_unset() {
        profile
            .and_then(|p| s(p, key))
            .map_or(Value::Null, |v| json!(v))
    } else {
        opt.to_json()
    }
}

pub async fn autocomplete(ix: &Ix<'_>) -> Result<()> {
    let app = ix.app;
    let Some(guild_id) = ix.guild_id().filter(|_| app.is_owner(ix.user_id())) else {
        return ix.autocomplete(Vec::new()).await;
    };
    let mut current = ensure_shape(&app.state.announcements(&guild_id.to_string()));
    let Some((name, value)) = ix.opts().focused() else {
        return ix.autocomplete(Vec::new()).await;
    };
    let query = value.trim().to_lowercase();
    if name == "profile" || name == "name" {
        let choices = sorted_profile_names(&current.profiles)
            .into_iter()
            .filter(|n| query.is_empty() || n.to_lowercase().contains(&query))
            .take(25)
            .map(|n| (n.clone(), n))
            .collect();
        return ix.autocomplete(choices).await;
    }
    if name == "id" {
        ensure_codes(&mut current.items);
        current.items.sort_by_key(run_at_ms);
        let choices = current
            .items
            .iter()
            .map(|item| {
                let r = reference(item);
                let summary = s(item, "title")
                    .map(str::to_owned)
                    .or_else(|| s(item, "content").map(|c| utf16_prefix(c, 32)));
                let parts: Vec<String> = [
                    Some(r.clone()),
                    s(item, "status").map(str::to_owned),
                    s(item, "profileName").map(str::to_owned),
                    summary,
                ]
                .into_iter()
                .flatten()
                .filter(|p| !p.is_empty())
                .collect();
                (utf16_prefix(&parts.join(" • "), 100), r)
            })
            .filter(|(label, value)| {
                query.is_empty()
                    || label.to_lowercase().contains(&query)
                    || value.to_lowercase().contains(&query)
            })
            .take(25)
            .collect();
        return ix.autocomplete(choices).await;
    }
    ix.autocomplete(Vec::new()).await
}

pub async fn execute(ix: &Ix<'_>) -> Result<()> {
    let Some(guild_id) = ix.guild_id() else {
        return ix
            .reply_ephemeral("This command only works in a server.")
            .await;
    };
    let app = ix.app;
    if !app.is_owner(ix.user_id()) {
        return ix.reply_ephemeral("This command is owner-only.").await;
    }
    let guild = guild_id.to_string();
    // The sweep may be sending right now; wait briefly, but answer before Discord's 3 s limit.
    let Ok(_edit) = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        app.announcements.edit_lock.lock(),
    )
    .await
    else {
        return ix
            .reply_ephemeral("Scheduled announcements are being sent. Try again in a moment.")
            .await;
    };
    let opts = ix.opts();
    let mode = opts.lower("mode");
    let mut current = ensure_shape(&app.state.announcements(&guild));
    if ensure_codes(&mut current.items) {
        app.state
            .save_announcements(&guild, json!({ "items": current.items }));
    }
    let user = ix.user_id().to_string();
    let save_profiles = |profiles: &Map<String, Value>| {
        app.state
            .save_announcements(&guild, json!({ "profiles": profiles }));
    };

    match mode.as_str() {
        "profile_create" => {
            let Some(raw) = opts.string("name") else {
                return ix
                    .reply_ephemeral("For mode=profile_create, provide `name`.")
                    .await;
            };
            let name = match normalize_profile_name(Some(raw))? {
                Opt::Set(n) => n,
                _ => String::from("null"),
            };
            let Some((channel, _)) = opts.channel("channel") else {
                return ix
                    .reply_ephemeral("For mode=profile_create, provide `channel`.")
                    .await;
            };
            let title = normalize_optional_text(opts.string("title"));
            let color = normalize_color(opts.string("color"))?;
            let username = normalize_optional_text(opts.string("username"));
            let avatar = normalize_optional_text(opts.string("avatar_url"));
            if current.profiles.contains_key(&name) {
                return ix
                    .reply_ephemeral("A profile with this name already exists.")
                    .await;
            }
            let now = now_iso();
            current.profiles.insert(
                name.clone(),
                json!({
                    "channelId": channel.to_string(),
                    "title": title.to_json(),
                    "color": color.to_json(),
                    "username": username.to_json(),
                    "avatarURL": avatar.to_json(),
                    "createdBy": user,
                    "createdAt": now,
                    "updatedAt": now,
                }),
            );
            save_profiles(&current.profiles);
            ix.reply_ephemeral(format!("Added profile `{name}` for <#{channel}>."))
                .await
        }
        "profile_list" => {
            ix.reply(ephemeral_embed(profile_list_embed(&current.profiles)))
                .await
        }
        "profile_edit" => {
            let Some(raw) = opts.string("name") else {
                return ix
                    .reply_ephemeral("For mode=profile_edit, provide `name`.")
                    .await;
            };
            let name = match normalize_profile_name(Some(raw))? {
                Opt::Set(n) => n,
                _ => String::new(),
            };
            let Some(profile) = current.profiles.get_mut(&name) else {
                return ix
                    .reply_ephemeral("No profile with this name was found.")
                    .await;
            };
            let next_title = normalize_optional_text(opts.string("title"));
            let next_color = normalize_color(opts.string("color"))?;
            let next_username = normalize_optional_text(opts.string("username"));
            let next_avatar = normalize_optional_text(opts.string("avatar_url"));
            let mut changed = Vec::new();
            if let Some((channel, _)) = opts.channel("channel") {
                profile["channelId"] = json!(channel.to_string());
                changed.push("channel");
            }
            for (opt, key, label) in [
                (&next_title, "title", "title"),
                (&next_color, "color", "color"),
                (&next_username, "username", "username"),
                (&next_avatar, "avatarURL", "avatar_url"),
            ] {
                if !opt.is_unset() {
                    profile[key] = opt.to_json();
                    changed.push(label);
                }
            }
            if changed.is_empty() {
                return ix.reply_ephemeral("No changes were given.").await;
            }
            profile["updatedAt"] = json!(now_iso());
            save_profiles(&current.profiles);
            ix.reply_ephemeral(format!("Updated profile `{name}`: {}.", changed.join(", ")))
                .await
        }
        "profile_delete" => {
            let Some(raw) = opts.string("name") else {
                return ix
                    .reply_ephemeral("For mode=profile_delete, provide `name`.")
                    .await;
            };
            let name = match normalize_profile_name(Some(raw))? {
                Opt::Set(n) => n,
                _ => String::new(),
            };
            if current.profiles.remove(&name).is_none() {
                return ix
                    .reply_ephemeral("No profile with this name was found.")
                    .await;
            }
            save_profiles(&current.profiles);
            ix.reply_ephemeral(format!("Deleted profile `{name}`."))
                .await
        }
        "create" | "now" => {
            let raw_profile = opts.string("profile");
            let profile_name = match normalize_profile_name(raw_profile.clone())? {
                Opt::Set(n) => Some(n),
                _ => None,
            };
            let profile = profile_name
                .as_ref()
                .and_then(|n| current.profiles.get(n))
                .cloned();
            if raw_profile.is_some() && profile.is_none() {
                return ix
                    .reply_ephemeral("No profile with this name was found.")
                    .await;
            }
            let profile_channel = profile
                .as_ref()
                .and_then(|p| s(p, "channelId"))
                .and_then(discord::parse_id)
                .filter(|c| {
                    app.cache
                        .channel(*c)
                        .is_some_and(|ch| ch.guild_id == Some(guild_id))
                });
            let Some(channel) = opts.channel("channel").map(|c| c.0).or(profile_channel) else {
                return ix
                    .reply_ephemeral("Provide `channel` or use a profile that has a channel set.")
                    .await;
            };
            let run_at = if mode == "create" {
                let Some(input) = opts.string("datetime") else {
                    return ix
                        .reply_ephemeral("For mode=create, provide `datetime`.")
                        .await;
                };
                parse_datetime(&input)?
            } else {
                now_iso()
            };
            let Some(content_input) = opts.string("content") else {
                return ix
                    .reply_ephemeral(format!("For mode={mode}, provide `content`."))
                    .await;
            };
            let content = normalize_required_content(&content_input)?;
            let mode_opt = normalize_mode(opts.string("send_mode"))?;
            let title = normalize_optional_text(opts.string("title"));
            let color = normalize_color(opts.string("color"))?;
            let username = normalize_optional_text(opts.string("username"));
            let avatar = normalize_optional_text(opts.string("avatar_url"));
            let button_label = normalize_optional_text(opts.string("button_label"));
            let button_url = normalize_url(opts.string("button_url"))?;
            let send_mode = match &mode_opt {
                Opt::Set(m) => m.clone(),
                _ if button_label.is_set() || button_url.is_set() => "components".to_owned(),
                _ if title.is_set() || color.is_set() => "embed".to_owned(),
                _ => "message".to_owned(),
            };
            if send_mode == "components" && (!button_label.is_set() || !button_url.is_set()) {
                return ix
                    .reply_ephemeral("The `components` mode needs `button_label` and `button_url`.")
                    .await;
            }
            let now = now_iso();
            let mut item = json!({
                "id": generate_id(),
                "code": unused_code(&current.items),
                "channelId": channel.to_string(),
                "runAt": run_at,
                "content": content,
                "sendMode": send_mode,
                "profileName": profile_name,
                "title": pick(&title, profile.as_ref(), "title"),
                "color": pick(&color, profile.as_ref(), "color"),
                "username": pick(&username, profile.as_ref(), "username"),
                "avatarURL": pick(&avatar, profile.as_ref(), "avatarURL"),
                "buttonLabel": button_label.to_json(),
                "buttonURL": button_url.to_json(),
                "status": "scheduled",
                "createdBy": user,
                "createdAt": now,
                "updatedAt": now,
                "sentAt": null,
                "messageId": null,
                "messageIds": [],
                "webhookId": null,
                "deliveryChannelId": null,
                "lastError": null,
            });
            let from_profile = profile_name
                .as_deref()
                .map(|p| format!(" from profile `{p}`"))
                .unwrap_or_default();
            let code = item["code"].as_str().unwrap_or_default().to_owned();
            if mode == "create" {
                current.items.push(item);
                app.state
                    .save_announcements(&guild, json!({ "items": current.items }));
                return ix
                    .reply_ephemeral(format!(
                        "Scheduled announcement `{code}` for <t:{}:F> in <#{channel}>{from_profile}.",
                        chrono::DateTime::parse_from_rfc3339(&run_at).map_or(0, |d| d.timestamp())
                    ))
                    .await;
            }
            ix.defer(true).await?;
            match ann::send_now(app, guild_id, &item).await {
                Ok(sent) => {
                    ann::apply_delivery(&mut item, &sent, &now_iso(), false);
                    let parts = item["messageIds"].as_array().map_or(1, Vec::len);
                    let message_id = item["messageId"].as_str().unwrap_or("null").to_owned();
                    current.items.push(item);
                    app.state
                        .save_announcements(&guild, json!({ "items": current.items }));
                    ix.edit_content(format!(
                        "Sent announcement `{code}` now in <#{channel}> (parts: {parts}, messageId: `{message_id}`){from_profile}."
                    ))
                    .await
                }
                Err(error) => {
                    let message = discord::err_text(&error);
                    item["status"] = json!("failed");
                    item["lastError"] = json!(message);
                    item["updatedAt"] = json!(now_iso());
                    let id = item["id"].as_str().unwrap_or_default().to_owned();
                    current.items.push(item);
                    app.state
                        .save_announcements(&guild, json!({ "items": current.items }));
                    tracing::error!("Announcement now error ({guild}/{id}): {message}");
                    ix.edit_content(format!("Could not send the announcement now: {message}"))
                        .await
                }
            }
        }
        "list" => {
            current.items.sort_by_key(run_at_ms);
            ix.reply(ephemeral_embed(list_embed(&current.items))).await
        }
        "edit" => {
            let id = opts.string("id").unwrap_or_default().trim().to_owned();
            if id.is_empty() {
                return ix.reply_ephemeral("For mode=edit, provide `id`.").await;
            }
            let Some(index) = current.items.iter().position(|i| matches_reference(i, &id)) else {
                return ix
                    .reply_ephemeral("No announcement with this ID was found.")
                    .await;
            };
            let next_datetime = opts.string("datetime").filter(|d| !d.is_empty());
            let next_content = normalize_content(opts.string("content"));
            let next_title = normalize_optional_text(opts.string("title"));
            let next_color = normalize_color(opts.string("color"))?;
            let next_username = normalize_optional_text(opts.string("username"));
            let next_avatar = normalize_optional_text(opts.string("avatar_url"));
            let next_mode = normalize_mode(opts.string("send_mode"))?;
            let next_label = normalize_optional_text(opts.string("button_label"));
            let next_url = normalize_url(opts.string("button_url"))?;
            let previous = current.items[index].clone();
            let item = &mut current.items[index];
            let mut changed = Vec::new();
            if let Some((channel, _)) = opts.channel("channel") {
                item["channelId"] = json!(channel.to_string());
                changed.push("channel");
            }
            if let Some(raw) = next_datetime {
                item["runAt"] = json!(parse_datetime(&raw)?);
                changed.push("datetime");
            }
            if !next_content.is_unset() {
                item["content"] = match &next_content {
                    Opt::Set(v) => json!(v),
                    _ => json!(""),
                };
                changed.push("content");
            }
            for (opt, key, label) in [
                (&next_title, "title", "title"),
                (&next_color, "color", "color"),
                (&next_username, "username", "username"),
                (&next_avatar, "avatarURL", "avatar_url"),
            ] {
                if !opt.is_unset() {
                    item[key] = opt.to_json();
                    changed.push(label);
                }
            }
            if !next_mode.is_unset() {
                item["sendMode"] = match &next_mode {
                    Opt::Set(m) => json!(m),
                    _ => json!("message"),
                };
                changed.push("mode");
            }
            if !next_label.is_unset() {
                item["buttonLabel"] = next_label.to_json();
                changed.push("button_label");
            }
            if !next_url.is_unset() {
                item["buttonURL"] = next_url.to_json();
                changed.push("button_url");
            }
            if changed.is_empty() {
                return ix.reply_ephemeral("No changes were given.").await;
            }
            if item["sendMode"].as_str() == Some("components")
                && (item.get("buttonLabel").is_none_or(is_falsy)
                    || item.get("buttonURL").is_none_or(is_falsy))
            {
                return ix
                    .reply_ephemeral("The `components` mode needs `button_label` and `button_url`.")
                    .await;
            }
            item["updatedAt"] = json!(now_iso());
            item["lastError"] = Value::Null;
            let mut note = None;
            let mut live_error = None;
            if item["status"].as_str() == Some("sent") && !stored_message_ids(item).is_empty() {
                let snapshot = item.clone();
                match ann::edit_message(app, guild_id, &snapshot, &previous).await {
                    Ok(edited) => {
                        if let Some(message_id) = &edited.message_id {
                            item["messageId"] = json!(message_id);
                            item["messageIds"] = if edited.message_ids.is_empty() {
                                json!([message_id])
                            } else {
                                json!(edited.message_ids)
                            };
                            if !edited.webhook_id.is_empty() {
                                item["webhookId"] = json!(edited.webhook_id);
                            }
                            let delivery = edited
                                .delivery_channel_id
                                .clone()
                                .or_else(|| s(item, "deliveryChannelId").map(str::to_owned))
                                .or_else(|| s(item, "channelId").map(str::to_owned));
                            item["deliveryChannelId"] = json!(delivery);
                        }
                        note = match edited.mode {
                            "resent" => Some("live update: republished to the new channel"),
                            "edited_in_place" => Some("live update: edited in place"),
                            _ => None,
                        };
                    }
                    Err(error) => {
                        let message = discord::err_text(&error);
                        item["lastError"] = json!(message);
                        tracing::error!(
                            "Announcement edit live error ({guild}/{}): {message}",
                            s(item, "id").unwrap_or_default()
                        );
                        live_error = Some(message);
                    }
                }
            } else {
                item["status"] = json!("scheduled");
            }
            let base = format!(
                "Updated announcement `{}`: {}.",
                reference(item),
                changed.join(", ")
            );
            app.state
                .save_announcements(&guild, json!({ "items": current.items }));
            let reply = match (live_error, note) {
                (Some(error), _) => format!(
                    "{base} The changes were saved, but updating the already sent message failed: {error}"
                ),
                (None, Some(note)) => format!("{base} {note}."),
                (None, None) => base,
            };
            ix.reply_ephemeral(reply).await
        }
        "delete" => {
            let id = opts.string("id").unwrap_or_default().trim().to_owned();
            if id.is_empty() {
                return ix.reply_ephemeral("For mode=delete, provide `id`.").await;
            }
            let Some(index) = current.items.iter().position(|i| matches_reference(i, &id)) else {
                return ix
                    .reply_ephemeral("No announcement with this ID was found.")
                    .await;
            };
            let item = current.items.remove(index);
            let mut warning = None;
            if item["status"].as_str() == Some("sent")
                && !stored_message_ids(&item).is_empty()
                && let Err(error) = ann::delete_message(app, &guild, &item).await
            {
                let message = discord::err_text(&error);
                tracing::warn!(
                    "Announcement delete live warn ({guild}/{}): {message}",
                    s(&item, "id").unwrap_or_default()
                );
                warning = Some(message);
            }
            app.state
                .save_announcements(&guild, json!({ "items": current.items }));
            let r = reference(&item);
            ix.reply_ephemeral(match warning {
                Some(w) => format!(
                    "Removed announcement `{r}`, but could not delete the published message: {w}"
                ),
                None => format!("Deleted announcement `{r}`."),
            })
            .await
        }
        _ => {
            ix.reply_ephemeral(format!("Unknown announce mode: {mode}."))
                .await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_break_formatting() {
        assert_eq!(format_line_breaks("Hello\\nWorld"), "Hello\nWorld");
        assert_eq!(
            format_line_breaks("Intro ## Title - a - b"),
            "Intro\n\n## Title\n- a\n- b"
        );
        assert_eq!(
            format_line_breaks("#Title\n-item\n\n\n\nx"),
            "# Title\n- item\n\nx"
        );
        assert_eq!(format_line_breaks("```\n#raw\n```"), "```\n#raw\n```");
        assert_eq!(format_line_breaks("a<br>b"), "a\nb");
        // The JS regexes mangled these (`# # Title`, `* *bold**`, `- # x`, `> > q`, `1. 5`).
        assert_eq!(
            format_line_breaks("## Title\n**bold** t\n-# sub\n>> q\n1.5 kg\n1.item"),
            "## Title\n**bold** t\n-# sub\n>> q\n1.5 kg\n1. item"
        );
    }

    #[test]
    fn codes_are_unique_and_valid() {
        let mut items = vec![json!({"code": "abcd"}), json!({"code": "ABCD"}), json!({})];
        assert!(ensure_codes(&mut items));
        let codes: HashSet<_> = items
            .iter()
            .map(|i| i["code"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(codes.len(), 3);
        assert_eq!(items[0]["code"], "ABCD");
    }
}
