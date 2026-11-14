//! `/help` and its button panel (port of `src/cmds/help.js` + `help-command.js`).

use anyhow::Result;
use serde_json::{Value, json};
use twilight_model::id::Id;
use twilight_model::id::marker::UserMarker;

use crate::app::App;
use crate::discord::{FLAG_COMPONENTS_V2, FLAG_EPHEMERAL, Ix};

const HELP_BUTTON_PREFIX: &str = "help_panel";

fn section_content(page: &str, can_use_owner: bool) -> String {
    let lines: &[&str] = match page.to_lowercase().as_str() {
        "music" => &[
            "### Muzyka",
            "`/music mode:play query:<fraza lub link>` albo `audio:<plik z Discorda>`",
            "`/music mode:queue|nowplaying|skip|stop`",
            "`/music mode:pause|resume`",
            "`/music mode:volume value:<+10|-10|80>`",
            "`/music mode:remove position:<numer>`",
            "`/music mode:clear|reconnect`",
            "Panel z przyciskami: loop, autoplay, shuffle, EQ/FX.",
            "`/ytdlp url:<link>` - pobiera video i wysyła je na kanał",
        ],
        "moderation" => &[
            "### Moderacja",
            "`/automod status|test`",
            "`/automod rule list|add|remove`",
            "`/verify mode:setup|status|resend_panel|exception_add|exception_remove|expire_user|cleanup_state`",
            "`/logs mode:setup|status|enable|disable|channel|retention|include_bots|preview|export_day|categories|purge|test`",
            "`/dm user:<user>|role:<role> message:<text> confirm:true`",
        ],
        "settings" => &[
            "### Ustawienia",
            "`/settings mode:show`",
            "`/settings mode:show section:<autoclean|automod|verify>`",
            "`/settings mode:set section:<autoclean|automod|verify> option:<opcja> string_value|integer_value|boolean_value|channel_value|role_value|user_value:<wartość>`",
            "`/health`",
            "`/debug mode:<music|permissions|logs>`",
        ],
        "owner" if !can_use_owner => &[
            "### Owner",
            "Ta sekcja jest dostępna tylko dla ownera bota.",
        ],
        "owner" => &[
            "### Owner",
            "`/clean mode:now|set|reset|status`",
            "`/cache mode:serverinfo id:<guild_id>`",
            "`/announce mode:create|now|list|edit|delete`",
            "`/announce mode:profile_create|profile_list|profile_edit|profile_delete`",
        ],
        _ => &[
            "### Start",
            "`/help section:<sekcja>` - otwiera od razu wybrany obszar",
            "`/music mode:play ...` - odtwarzanie, kolejka i recovery",
            "`/ytdlp url:<link>` - pobieranie video na kanał",
            "`/automod ...`, `/verify ...`, `/logs ...`, `/dm ...` - moderacja, verify i full logs",
            "`/settings ...`, `/health`, `/debug ...` - konfiguracja i diagnostyka",
            "`/announce ...` - planowane i natychmiastowe announcementy",
            "`/clean ...`, `/cache ...` - narzędzia ownera",
        ],
    };
    lines.join("\n")
}

/// `buildHelpPanelPayload(userId, page)`
pub fn panel_payload(app: &App, user_id: Id<UserMarker>, page: &str) -> Value {
    let selected = page.to_lowercase();
    let can_use_owner = app.is_owner(user_id);
    let pages = [
        ("overview", "Start", false),
        ("music", "Muzyka", false),
        ("moderation", "Moderacja", false),
        ("settings", "Ustawienia", false),
        ("owner", "Owner", true),
    ];
    let buttons: Vec<Value> = pages
        .iter()
        .map(|(id, label, owner_only)| {
            json!({
                "type": 2,
                "custom_id": format!("{HELP_BUTTON_PREFIX}:{id}:{user_id}"),
                "label": label,
                "style": if selected == *id { 3 } else { 2 },
                "disabled": *owner_only && !can_use_owner,
            })
        })
        .collect();
    let content = [
        "## Operator Help".to_owned(),
        "Interaktywny panel komend. Wybierz sekcję poniżej albo uruchom `/help section:<sekcja>`."
            .to_owned(),
        String::new(),
        section_content(&selected, can_use_owner),
    ]
    .join("\n");
    json!({
        "flags": FLAG_COMPONENTS_V2 | FLAG_EPHEMERAL,
        "components": [{
            "type": 17,
            "components": [
                {
                    "type": 10,
                    "content": content,
                },
                { "type": 1, "components": buttons },
            ],
        }],
    })
}

pub async fn execute(ix: &Ix<'_>) -> Result<()> {
    let section = ix
        .opts()
        .string("section")
        .unwrap_or_else(|| "overview".to_owned());
    ix.reply(panel_payload(ix.app, ix.user_id(), &section))
        .await
}

/// Returns `true` when the component belonged to the help panel.
pub async fn handle_component(ix: &Ix<'_>) -> Result<bool> {
    if ix.component_kind()
        != Some(twilight_model::channel::message::component::ComponentType::Button)
    {
        return Ok(false);
    }
    let Some(custom_id) = ix.custom_id() else {
        return Ok(false);
    };
    let mut parts = custom_id.split(':');
    let (Some(HELP_BUTTON_PREFIX), Some(page), Some(owner)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return Ok(false);
    };
    if page.is_empty() || owner.is_empty() {
        return Ok(false);
    }
    if ix.user_id().to_string() != owner {
        ix.reply_ephemeral("Ten panel pomocy należy do innego użytkownika. Użyj /help.")
            .await?;
        return Ok(true);
    }
    ix.update(panel_payload(ix.app, ix.user_id(), page)).await?;
    Ok(true)
}
