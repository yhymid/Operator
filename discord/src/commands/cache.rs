//! `/cache` (port of `src/cmds/cache.js`).

use anyhow::Result;
use serde_json::{Value, json};

use crate::discord::{self, Ix};

fn is_snowflake_like(value: &str) -> bool {
    (16..=22).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit())
}

pub async fn execute(ix: &Ix<'_>) -> Result<()> {
    if !ix.in_guild() {
        return ix
            .reply_ephemeral("Ta komenda dziala tylko na serwerze.")
            .await;
    }
    if !ix.app.is_owner(ix.user_id()) {
        return ix
            .reply_ephemeral("Ta komenda jest tylko dla ownera.")
            .await;
    }
    let opts = ix.opts();
    let mode = opts.lower("mode");
    if mode != "serverinfo" {
        return ix
            .reply_ephemeral(format!("Nieznany mode cache: {mode}."))
            .await;
    }
    let id = opts.string("id").unwrap_or_default().trim().to_owned();
    if id.is_empty() {
        return ix.reply_ephemeral("Dla mode=serverinfo podaj `id`.").await;
    }
    if !is_snowflake_like(&id) {
        return ix
            .reply_ephemeral("Niepoprawne ID. Podaj poprawny Discord ID (cyfry).")
            .await;
    }
    let path = ix.app.paths.server_cache_dir.join(format!("{id}.json"));
    if !path.exists() {
        return ix
            .reply_ephemeral(format!("Brak cache dla ID `{id}` ({}).", path.display()))
            .await;
    }
    let pretty = std::fs::read_to_string(&path)
        .map_err(anyhow::Error::from)
        .and_then(|raw| Ok(serde_json::from_str::<Value>(&raw)?))
        .and_then(|mut parsed| {
            redact_secrets(&mut parsed);
            Ok(serde_json::to_string_pretty(&parsed)?)
        });
    let pretty = match pretty {
        Ok(pretty) => pretty,
        Err(error) => {
            tracing::error!(
                "Cache serverinfo error ({id}): {}",
                discord::err_text(&error)
            );
            return ix
                .reply_ephemeral(format!("Nie udało się odczytać cache dla ID `{id}`."))
                .await;
        }
    };
    let wrapped = format!("```json\n{pretty}\n```");
    if discord::js_len(&wrapped) <= 1900 {
        return ix.reply_ephemeral(wrapped).await;
    }
    ix.reply_ephemeral_file(
        &format!("Cache jest za długi na wiadomość. Wysyłam plik `server-{id}.json`."),
        &format!("server-{id}.json"),
        pretty.into_bytes(),
    )
    .await
}

/// Webhook tokens let anyone post as the webhook; they never leave the server's state file.
fn redact_secrets(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                if key.to_ascii_lowercase().contains("token") && item.is_string() {
                    *item = json!("[ukryte]");
                } else {
                    redact_secrets(item);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact_secrets),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webhook_tokens_are_redacted() {
        let mut state = json!({
            "announcements": { "items": [{ "webhookId": "1", "webhookToken": "secret" }] },
            "name": "x"
        });
        redact_secrets(&mut state);
        assert_eq!(
            state["announcements"]["items"][0]["webhookToken"],
            "[ukryte]"
        );
        assert_eq!(state["announcements"]["items"][0]["webhookId"], "1");
        assert_eq!(state["name"], "x");
    }
}
