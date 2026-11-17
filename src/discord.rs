//! Thin helpers over twilight: JSON message payloads (ported 1:1 from the JS objects),
//! interaction replies with discord.js-like `reply / deferReply / editReply / update / followUp`
//! semantics, slash-command option access and permission checks.

use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use serde_json::{Value, json};
use twilight_model::application::interaction::application_command::{
    CommandData, CommandDataOption, CommandOptionValue,
};
use twilight_model::application::interaction::{Interaction, InteractionData};
use twilight_model::channel::message::component::ComponentType;
use twilight_model::channel::{Attachment, ChannelType};
use twilight_model::guild::{Permissions, Role};
use twilight_model::http::attachment::Attachment as HttpAttachment;
use twilight_model::http::interaction::{
    InteractionResponse, InteractionResponseData, InteractionResponseType,
};
use twilight_model::id::Id;
use twilight_model::id::marker::{ChannelMarker, GuildMarker, RoleMarker, UserMarker};
use twilight_model::user::User;

use crate::app::App;

pub const FLAG_EPHEMERAL: u64 = 1 << 6;
pub const FLAG_COMPONENTS_V2: u64 = 1 << 15;

// ---------------------------------------------------------------------------------------------
// Payload builders
// ---------------------------------------------------------------------------------------------

/// `{ content, ephemeral: true }`
pub fn ephemeral(content: impl Into<String>) -> Value {
    json!({ "content": content.into(), "flags": FLAG_EPHEMERAL })
}

/// Components V2 text display.
pub fn text(content: impl Into<String>) -> Value {
    json!({ "type": 10, "content": content.into() })
}

/// Components V2 container.
pub fn container(components: Vec<Value>) -> Value {
    json!({ "type": 17, "components": components })
}

/// Command replies echo user input and remote data (track titles, yt-dlp/Lavalink errors), so
/// they never ping unless a payload sets `allowed_mentions` itself.
pub fn no_pings(mut payload: Value) -> Value {
    if let Some(object) = payload.as_object_mut() {
        object
            .entry("allowed_mentions")
            .or_insert_with(|| json!({ "parse": [] }));
    }
    payload
}

/// `buildPanel(title, lines)` used by /logs: ephemeral V2 card without mentions.
pub fn panel(title: &str, lines: &[String]) -> Value {
    json!({
        "flags": FLAG_COMPONENTS_V2 | FLAG_EPHEMERAL,
        "allowed_mentions": { "parse": [] },
        "components": [container(vec![text(format!("## {title}")), text(lines.join("\n"))])]
    })
}

pub fn embed_timestamp_now() -> String {
    crate::common::time::now_iso()
}

// ---------------------------------------------------------------------------------------------
// Interaction context
// ---------------------------------------------------------------------------------------------

pub struct Ix<'a> {
    pub app: &'a crate::app::SharedApp,
    pub interaction: &'a Interaction,
    replied: AtomicBool,
    deferred: AtomicBool,
    /// When the gateway delivered the interaction (diagnostics).
    pub received: std::time::Instant,
    /// Milliseconds from `received` to the first answer (`u64::MAX` = none yet).
    first_response_ms: std::sync::atomic::AtomicU64,
    first_response_kind: std::sync::Mutex<&'static str>,
}

impl<'a> Ix<'a> {
    pub fn new(app: &'a crate::app::SharedApp, interaction: &'a Interaction) -> Self {
        Self {
            app,
            interaction,
            replied: AtomicBool::new(false),
            deferred: AtomicBool::new(false),
            received: std::time::Instant::now(),
            first_response_ms: std::sync::atomic::AtomicU64::new(u64::MAX),
            first_response_kind: std::sync::Mutex::new(""),
        }
    }

    /// First answer: time since the gateway delivered the interaction, and its kind.
    pub fn first_response(&self) -> Option<(u64, &'static str)> {
        let ms = self.first_response_ms.load(Ordering::SeqCst);
        (ms != u64::MAX).then(|| {
            let kind = *self
                .first_response_kind
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            (ms, kind)
        })
    }

    fn mark_first_response(&self, kind: &'static str) {
        let ms = self.received.elapsed().as_millis() as u64;
        if self
            .first_response_ms
            .compare_exchange(u64::MAX, ms, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            *self
                .first_response_kind
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = kind;
        }
    }

    pub fn replied(&self) -> bool {
        self.replied.load(Ordering::SeqCst)
    }

    pub fn deferred(&self) -> bool {
        self.deferred.load(Ordering::SeqCst)
    }

    pub fn guild_id(&self) -> Option<Id<GuildMarker>> {
        self.interaction.guild_id
    }

    pub fn in_guild(&self) -> bool {
        self.interaction.guild_id.is_some()
    }

    pub fn user(&self) -> Option<&User> {
        self.interaction
            .member
            .as_ref()
            .and_then(|m| m.user.as_ref())
            .or(self.interaction.user.as_ref())
    }

    pub fn user_id(&self) -> Id<UserMarker> {
        self.user().map_or(Id::new(1), |u| u.id)
    }

    /// `interaction.memberPermissions.has(perm)` (admin implies everything).
    pub fn member_has(&self, permission: Permissions) -> bool {
        self.interaction
            .member
            .as_ref()
            .and_then(|m| m.permissions)
            .is_some_and(|p| p.contains(Permissions::ADMINISTRATOR) || p.contains(permission))
    }

    pub fn command(&self) -> Option<&CommandData> {
        match &self.interaction.data {
            Some(InteractionData::ApplicationCommand(data)) => Some(data),
            _ => None,
        }
    }

    pub fn opts(&self) -> Opts<'_> {
        Opts::new(self.command())
    }

    pub fn custom_id(&self) -> Option<&str> {
        match &self.interaction.data {
            Some(InteractionData::MessageComponent(data)) => Some(&data.custom_id),
            Some(InteractionData::ModalSubmit(data)) => Some(&data.custom_id),
            _ => None,
        }
    }

    pub fn component_kind(&self) -> Option<ComponentType> {
        match &self.interaction.data {
            Some(InteractionData::MessageComponent(data)) => Some(data.component_type),
            _ => None,
        }
    }

    pub fn component_values(&self) -> Vec<String> {
        match &self.interaction.data {
            Some(InteractionData::MessageComponent(data)) => data.values.clone(),
            _ => Vec::new(),
        }
    }

    /// `interaction.fields.getTextInputValue(id)` for modal submits.
    pub fn modal_text(&self, custom_id: &str) -> Option<String> {
        use twilight_model::application::interaction::modal::ModalInteractionComponent as C;
        fn find(components: &[C], id: &str) -> Option<String> {
            components.iter().find_map(|component| match component {
                C::TextInput(input) if input.custom_id == id => Some(input.value.clone()),
                C::ActionRow(row) => find(&row.components, id),
                C::Label(label) => find(std::slice::from_ref(label.component.as_ref()), id),
                _ => None,
            })
        }
        match &self.interaction.data {
            Some(InteractionData::ModalSubmit(data)) => find(&data.components, custom_id),
            _ => None,
        }
    }

    /// Message the component belongs to.
    pub fn message_id(&self) -> Option<Id<twilight_model::id::marker::MessageMarker>> {
        self.interaction.message.as_ref().map(|m| m.id)
    }

    async fn respond(&self, kind: InteractionResponseType, data: Option<Value>) -> Result<()> {
        let carries_message = matches!(
            kind,
            InteractionResponseType::ChannelMessageWithSource
                | InteractionResponseType::UpdateMessage
        );
        let data = match data {
            Some(value) => {
                let value = if carries_message {
                    no_pings(value)
                } else {
                    value
                };
                let (value, files) = split_files(value);
                let mut parsed: InteractionResponseData =
                    serde_json::from_value(value).context("invalid interaction payload")?;
                if !files.is_empty() {
                    parsed.attachments = Some(files);
                }
                Some(parsed)
            }
            None => None,
        };
        let label = response_label(kind);
        let response = InteractionResponse { kind, data };
        timed(
            "interaction.callback",
            label,
            self.app
                .http
                .interaction(self.app.application_id)
                .create_response(self.interaction.id, &self.interaction.token, &response),
        )
        .await
        .context("create interaction response")?;
        self.mark_first_response(label);
        Ok(())
    }

    /// `interaction.reply(payload)`
    pub async fn reply(&self, payload: Value) -> Result<()> {
        self.respond(
            InteractionResponseType::ChannelMessageWithSource,
            Some(payload),
        )
        .await?;
        self.replied.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Ephemeral reply with one attachment, sent as raw multipart bytes (no JSON byte array).
    pub async fn reply_ephemeral_file(
        &self,
        content: &str,
        name: &str,
        bytes: Vec<u8>,
    ) -> Result<()> {
        let payload = no_pings(json!({ "flags": FLAG_EPHEMERAL, "content": content }));
        let mut data: InteractionResponseData =
            serde_json::from_value(payload).context("invalid interaction payload")?;
        data.attachments = Some(vec![HttpAttachment::from_bytes(name.to_owned(), bytes, 0)]);
        let response = InteractionResponse {
            kind: InteractionResponseType::ChannelMessageWithSource,
            data: Some(data),
        };
        timed(
            "interaction.callback",
            "reply_file",
            self.app
                .http
                .interaction(self.app.application_id)
                .create_response(self.interaction.id, &self.interaction.token, &response),
        )
        .await
        .context("create interaction response")?;
        self.mark_first_response("reply_file");
        self.replied.store(true, Ordering::SeqCst);
        Ok(())
    }

    pub async fn reply_ephemeral(&self, content: impl Into<String>) -> Result<()> {
        self.reply(ephemeral(content)).await
    }

    /// `interaction.deferReply({ ephemeral })`
    pub async fn defer(&self, ephemeral: bool) -> Result<()> {
        let data = ephemeral.then(|| json!({ "flags": FLAG_EPHEMERAL }));
        self.respond(
            InteractionResponseType::DeferredChannelMessageWithSource,
            data,
        )
        .await?;
        self.deferred.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// `interaction.deferUpdate()`
    pub async fn defer_update(&self) -> Result<()> {
        self.respond(InteractionResponseType::DeferredUpdateMessage, None)
            .await?;
        self.deferred.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// `interaction.update(payload)` for component interactions.
    pub async fn update(&self, payload: Value) -> Result<()> {
        self.respond(InteractionResponseType::UpdateMessage, Some(payload))
            .await?;
        self.replied.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// `interaction.showModal(modal)`
    pub async fn show_modal(&self, modal: Value) -> Result<()> {
        self.respond(InteractionResponseType::Modal, Some(modal))
            .await?;
        self.replied.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// `interaction.respond(choices)` for autocomplete.
    pub async fn autocomplete(&self, choices: Vec<(String, String)>) -> Result<()> {
        let choices: Vec<Value> = choices
            .into_iter()
            .map(|(name, value)| json!({ "name": name, "value": value }))
            .collect();
        self.respond(
            InteractionResponseType::ApplicationCommandAutocompleteResult,
            Some(json!({ "choices": choices })),
        )
        .await
    }

    /// `interaction.editReply(payload)`
    pub async fn edit(&self, payload: Value) -> Result<()> {
        let (value, files) = split_files(no_pings(payload));
        let body = serde_json::to_vec(&value)?;
        let client = self.app.http.interaction(self.app.application_id);
        let mut request = client
            .update_response(&self.interaction.token)
            .payload_json(&body);
        if !files.is_empty() {
            request = request.attachments(&files);
        }
        timed("interaction.edit", "edit", request)
            .await
            .context("edit interaction reply")?;
        Ok(())
    }

    /// Edit the reply with a single (possibly large) attachment without JSON-encoding the bytes.
    pub async fn edit_with_file(&self, content: &str, name: &str, bytes: Vec<u8>) -> Result<()> {
        let body = serde_json::to_vec(&no_pings(json!({ "content": content })))?;
        let size = bytes.len();
        let files = [HttpAttachment::from_bytes(name.to_owned(), bytes, 0)];
        let started = std::time::Instant::now();
        let result = timed(
            "interaction.edit",
            "edit_file",
            self.app
                .http
                .interaction(self.app.application_id)
                .update_response(&self.interaction.token)
                .payload_json(&body)
                .attachments(&files),
        )
        .await;
        crate::common::diagnostics::record(
            "discord.upload",
            json!({ "op": "interaction file", "bytes": size, "ok": result.is_ok(),
                    "uploadMs": crate::common::diagnostics::elapsed_ms(started),
                    "mbPerS": size as f64 / 1_048_576.0 / started.elapsed().as_secs_f64().max(0.001) }),
        );
        result.context("edit interaction reply")?;
        Ok(())
    }

    pub async fn edit_content(&self, content: impl Into<String>) -> Result<()> {
        self.edit(json!({ "content": content.into() })).await
    }

    /// `interaction.followUp(payload)`
    pub async fn follow_up(&self, payload: Value) -> Result<()> {
        let body = serde_json::to_vec(&no_pings(payload))?;
        timed(
            "interaction.followup",
            "follow_up",
            self.app
                .http
                .interaction(self.app.application_id)
                .create_followup(&self.interaction.token)
                .payload_json(&body),
        )
        .await
        .context("interaction follow-up")?;
        Ok(())
    }
}

/// Small files may be passed as `"files": [{ "name": .., "bytes": [..] }]` inside JSON payloads
/// and are converted to multipart attachments here. Large files go through
/// `Ix::reply_ephemeral_file` / `Ix::edit_with_file`, which skip the JSON byte array.
fn split_files(mut value: Value) -> (Value, Vec<HttpAttachment>) {
    let mut out = Vec::new();
    if let Some(files) = value.as_object_mut().and_then(|m| m.remove("files"))
        && let Some(files) = files.as_array()
    {
        for (index, file) in files.iter().enumerate() {
            let name = file["name"].as_str().unwrap_or("file").to_owned();
            let bytes: Vec<u8> = file["bytes"]
                .as_array()
                .map(|b| {
                    b.iter()
                        .filter_map(|x| x.as_u64().map(|x| x as u8))
                        .collect()
                })
                .unwrap_or_default();
            out.push(HttpAttachment::from_bytes(name, bytes, index as u64));
        }
    }
    (value, out)
}

// ---------------------------------------------------------------------------------------------
// Slash command options
// ---------------------------------------------------------------------------------------------

pub struct Opts<'a> {
    data: Option<&'a CommandData>,
    options: &'a [CommandDataOption],
    pub group: Option<&'a str>,
    pub subcommand: Option<&'a str>,
}

impl<'a> Opts<'a> {
    fn new(data: Option<&'a CommandData>) -> Self {
        let mut options: &[CommandDataOption] = data.map_or(&[], |d| &d.options);
        let mut group = None;
        let mut subcommand = None;
        if let Some(first) = options.first()
            && let CommandOptionValue::SubCommandGroup(inner) = &first.value
        {
            group = Some(first.name.as_str());
            options = inner;
        }
        if let Some(first) = options.first()
            && let CommandOptionValue::SubCommand(inner) = &first.value
        {
            subcommand = Some(first.name.as_str());
            options = inner;
        }
        Self {
            data,
            options,
            group,
            subcommand,
        }
    }

    fn value(&self, name: &str) -> Option<&'a CommandOptionValue> {
        self.options
            .iter()
            .find(|o| o.name == name)
            .map(|o| &o.value)
    }

    pub fn string(&self, name: &str) -> Option<String> {
        match self.value(name)? {
            CommandOptionValue::String(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// Lowercased string option (most `mode` handling in JS does `String(..).toLowerCase()`).
    pub fn lower(&self, name: &str) -> String {
        self.string(name).unwrap_or_default().to_lowercase()
    }

    pub fn integer(&self, name: &str) -> Option<i64> {
        match self.value(name)? {
            CommandOptionValue::Integer(n) => Some(*n),
            _ => None,
        }
    }

    pub fn boolean(&self, name: &str) -> Option<bool> {
        match self.value(name)? {
            CommandOptionValue::Boolean(b) => Some(*b),
            _ => None,
        }
    }

    pub fn user(&self, name: &str) -> Option<User> {
        let CommandOptionValue::User(id) = self.value(name)? else {
            return None;
        };
        self.data?.resolved.as_ref()?.users.get(id).cloned()
    }

    pub fn role(&self, name: &str) -> Option<Role> {
        let CommandOptionValue::Role(id) = self.value(name)? else {
            return None;
        };
        self.data?.resolved.as_ref()?.roles.get(id).cloned()
    }

    pub fn channel(&self, name: &str) -> Option<(Id<ChannelMarker>, ChannelType)> {
        let CommandOptionValue::Channel(id) = self.value(name)? else {
            return None;
        };
        let kind = self
            .data?
            .resolved
            .as_ref()
            .and_then(|r| r.channels.get(id))
            .map_or(ChannelType::GuildText, |c| c.kind);
        Some((*id, kind))
    }

    pub fn attachment(&self, name: &str) -> Option<Attachment> {
        let CommandOptionValue::Attachment(id) = self.value(name)? else {
            return None;
        };
        self.data?.resolved.as_ref()?.attachments.get(id).cloned()
    }
}

// ---------------------------------------------------------------------------------------------
// Misc helpers
// ---------------------------------------------------------------------------------------------

/// discord.js `user.tag`.
pub fn user_tag(user: &User) -> String {
    if user.discriminator == 0 {
        user.name.clone()
    } else {
        format!("{}#{:04}", user.name, user.discriminator)
    }
}

/// discord.js `channel.isTextBased()`.
pub fn is_text_based(kind: ChannelType) -> bool {
    matches!(
        kind,
        ChannelType::GuildText
            | ChannelType::GuildAnnouncement
            | ChannelType::GuildVoice
            | ChannelType::GuildStageVoice
            | ChannelType::AnnouncementThread
            | ChannelType::PublicThread
            | ChannelType::PrivateThread
            | ChannelType::Private
            | ChannelType::Group
    )
}

/// discord.js `channel.isVoiceBased()`.
pub fn is_voice_based(kind: ChannelType) -> bool {
    matches!(kind, ChannelType::GuildVoice | ChannelType::GuildStageVoice)
}

/// Bot member guild-wide permissions (`guild.members.me.permissions`).
pub fn bot_root_permissions(app: &App, guild_id: Id<GuildMarker>) -> Option<Permissions> {
    app.cache.permissions().root(app.bot_id, guild_id).ok()
}

/// Bot permissions in a channel (`channel.permissionsFor(me)`).
pub fn bot_channel_permissions(app: &App, channel_id: Id<ChannelMarker>) -> Option<Permissions> {
    app.cache
        .permissions()
        .in_channel(app.bot_id, channel_id)
        .ok()
}

pub fn has(perms: Permissions, wanted: Permissions) -> bool {
    perms.contains(Permissions::ADMINISTRATOR) || perms.contains(wanted)
}

/// Position of the highest role of `user_id` (`member.roles.highest.position`).
pub fn highest_role_position(app: &App, guild_id: Id<GuildMarker>, user_id: Id<UserMarker>) -> i64 {
    let Some(member) = app.cache.member(guild_id, user_id) else {
        return 0;
    };
    member
        .roles()
        .iter()
        .filter_map(|role_id| app.cache.role(*role_id).map(|r| r.position))
        .max()
        .unwrap_or(0)
}

pub fn role_position(app: &App, role_id: Id<RoleMarker>) -> Option<i64> {
    app.cache.role(role_id).map(|r| r.position)
}

/// Role ids of a member from cache.
pub fn member_roles(
    app: &App,
    guild_id: Id<GuildMarker>,
    user_id: Id<UserMarker>,
) -> Option<Vec<Id<RoleMarker>>> {
    app.cache
        .member(guild_id, user_id)
        .map(|m| m.roles().to_vec())
}

pub fn parse_id<T>(raw: &str) -> Option<Id<T>> {
    raw.trim().parse::<u64>().ok().and_then(Id::new_checked)
}

/// Sends a message from a JSON payload (`client.rest.post(Routes.channelMessages(id), { body })`).
pub async fn send_message(
    app: &App,
    channel_id: Id<ChannelMarker>,
    payload: &Value,
) -> Result<twilight_model::channel::Message> {
    let (value, files) = split_files(payload.clone());
    let body = serde_json::to_vec(&value)?;
    let mut request = app.http.create_message(channel_id).payload_json(&body);
    if !files.is_empty() {
        request = request.attachments(&files);
    }
    let message = timed("channel.message", "create", request)
        .await?
        .model()
        .await?;
    Ok(message)
}

fn response_label(kind: InteractionResponseType) -> &'static str {
    match kind {
        InteractionResponseType::ChannelMessageWithSource => "reply",
        InteractionResponseType::DeferredChannelMessageWithSource => "defer",
        InteractionResponseType::DeferredUpdateMessage => "defer_update",
        InteractionResponseType::UpdateMessage => "update",
        InteractionResponseType::Modal => "modal",
        InteractionResponseType::ApplicationCommandAutocompleteResult => "autocomplete",
        _ => "other",
    }
}

/// Awaits a twilight-http request and records its duration, status and rate limits
/// (`discord.rest` / `discord.rate_limited`).
pub async fn timed<T>(
    route: &str,
    label: &str,
    request: impl std::future::IntoFuture<Output = Result<T, twilight_http::Error>>,
) -> Result<T, twilight_http::Error> {
    let started = std::time::Instant::now();
    let result = request.into_future().await;
    if crate::common::diagnostics::enabled() {
        let total = crate::common::diagnostics::elapsed_ms(started);
        let status = match &result {
            Ok(_) => Some(200),
            Err(error) => match error.kind() {
                twilight_http::error::ErrorType::Response { status, .. } => Some(status.get()),
                _ => None,
            },
        };
        let error = result.as_ref().err().map(api_error_message);
        crate::common::diagnostics::record(
            "discord.rest",
            json!({ "op": format!("{route} {label}"), "totalMs": total, "status": status,
                    "ok": result.is_ok(), "error": error }),
        );
        if status == Some(429) {
            crate::common::diagnostics::record(
                "discord.rate_limited",
                json!({ "op": route, "error": error }),
            );
        }
    }
    result
}

/// Readable error text for logs/replies (`error.message`). Discord API errors are reduced to
/// the API message, like discord.js `DiscordAPIError.message`.
pub fn err_text(error: &anyhow::Error) -> String {
    if let Some(http) = error.downcast_ref::<twilight_http::Error>() {
        return api_error_message(http);
    }
    let mut text = error.to_string();
    if let Some(source) = error.source() {
        let source = source.to_string();
        if !text.contains(&source) {
            text = format!("{text}: {source}");
        }
    }
    text
}

pub fn api_error_message(error: &twilight_http::Error) -> String {
    use twilight_http::api_error::ApiError;
    use twilight_http::error::ErrorType;
    match error.kind() {
        ErrorType::Response {
            error: ApiError::General(general),
            ..
        } => general.message.clone(),
        _ => error.to_string(),
    }
}

/// All guild members (`guild.members.fetch()`), paginated by 1000.
pub async fn fetch_all_members(
    app: &App,
    guild_id: Id<GuildMarker>,
) -> Option<Vec<twilight_model::guild::Member>> {
    let mut out = Vec::new();
    let mut after: Option<Id<UserMarker>> = None;
    loop {
        let mut request = app.http.guild_members(guild_id).limit(1000);
        if let Some(after) = after {
            request = request.after(after);
        }
        let page = request.await.ok()?.models().await.ok()?;
        let len = page.len();
        after = page.last().map(|m| m.user.id);
        out.extend(page);
        if len < 1000 {
            return Some(out);
        }
    }
}

/// Sends a DM (`user.send({ content })`).
pub async fn send_dm(app: &App, user_id: Id<UserMarker>, content: &str) -> Result<()> {
    let channel = app
        .http
        .create_private_channel(user_id)
        .await?
        .model()
        .await?;
    app.http.create_message(channel.id).content(content).await?;
    Ok(())
}

/// JS `string.length` (UTF-16 code units).
pub fn js_len(text: &str) -> usize {
    text.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_do_not_ping_by_default() {
        assert_eq!(
            no_pings(json!({ "content": "@everyone" }))["allowed_mentions"],
            json!({ "parse": [] })
        );
        let explicit = json!({ "content": "<@1>", "allowed_mentions": { "users": ["1"] } });
        assert_eq!(no_pings(explicit.clone()), explicit);
    }
}
