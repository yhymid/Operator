#[allow(clippy::wildcard_imports)]
use super::super::*;

impl FullLogs {
    pub(in super::super) async fn on_interaction_create(
        &self,
        app: &SharedApp,
        event: &Event,
        _before: Before,
    ) -> anyhow::Result<()> {
        let Event::InteractionCreate(interaction) = event else {
            return Ok(());
        };
        let Some(guild_id) = interaction.guild_id else {
            return Ok(());
        };
        let Some(twilight_model::application::interaction::InteractionData::ApplicationCommand(
            data,
        )) = &interaction.data
        else {
            return Ok(());
        };
        let user = interaction
            .member
            .as_ref()
            .and_then(|m| m.user.as_ref())
            .or(interaction.user.as_ref());
        let Some(user) = user else { return Ok(()) };
        let channel_id = interaction.channel.as_ref().map(|c| c.id);
        let name = interaction.channel.as_ref().and_then(|c| c.name.clone());
        self.log(
            app,
            guild_id,
            json!({
                "category": "command",
                "action": "slash_execute",
                "actor": actor_payload(Some(user)),
                "target": { "kind": "command", "name": data.name },
                "location": { "channelId": channel_id.map(|c| c.to_string()), "channelName": name },
                "summary": format!("/{} executed", data.name),
                "details": [
                    format!("command=/{}", data.name),
                    format!("channel=#{}", name.as_deref().unwrap_or("unknown")),
                    format!("interaction_id={}", interaction.id),
                ],
                "metadata": {
                    "commandName": data.name,
                    "channelId": channel_id.map(|c| c.to_string()),
                    "interactionId": interaction.id.to_string()
                }
            }),
            false,
        );
        Ok(())
    }
}
