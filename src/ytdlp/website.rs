#[allow(clippy::wildcard_imports)]
use super::*;

// ---------------------------------------------------------------------------------------------
// Website publishing (files above the Telegram upload limit)
// ---------------------------------------------------------------------------------------------

pub(super) fn player_html(
    id: &str,
    display_name: &str,
    video_name: &str,
    extension: &str,
    delete_token: &str,
) -> String {
    let video_url = format!("./{video_name}");
    let name = escape_html(display_name);
    let id_json = serde_json::to_string(id).unwrap_or_default();
    let token_json = serde_json::to_string(delete_token).unwrap_or_default();
    format!(
        r#"<!doctype html>
<html lang="pl">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{name} — Bladephoria</title>
  <style>
    :root {{ color-scheme: dark; }}
    body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; background: #101218; color: #f4f5f7; font: 16px system-ui, sans-serif; }}
    main {{ width: min(960px, calc(100% - 32px)); }}
    h1 {{ font-size: clamp(1.1rem, 2vw, 1.6rem); overflow-wrap: anywhere; }}
    video {{ width: 100%; max-height: 75vh; background: #000; border-radius: 10px; }}
    .actions {{ display: flex; gap: 12px; flex-wrap: wrap; margin-top: 16px; }}
    a, button {{ border: 0; border-radius: 7px; padding: 10px 14px; color: #fff; background: #5865f2; text-decoration: none; cursor: pointer; font: inherit; }}
    button {{ background: #b42318; }}
    #message {{ min-height: 1.4em; color: #ffb4ab; }}
  </style>
</head>
<body>
  <main>
    <h1>{name}</h1>
    <video controls preload="metadata">
      <source src="{video_url}" type="{mime}">
      Twoja przeglądarka nie obsługuje odtwarzania tego filmu.
    </video>
    <div class="actions">
      <a href="{video_url}" download="{video_attr}">Pobierz plik</a>
      <button id="delete" type="button">Usuń plik</button>
    </div>
    <p id="message"></p>
  </main>
  <script>
    const mediaId = {id_json};
    const deleteToken = {token_json};
    const button = document.getElementById('delete');
    const message = document.getElementById('message');
    button.addEventListener('click', async () => {{
      if (!window.confirm('Usunąć ten plik bezpowrotnie?')) return;
      button.disabled = true;
      try {{
        const response = await fetch('/auth/media-delete', {{
          method: 'POST',
          headers: {{ 'Content-Type': 'application/json' }},
          body: JSON.stringify({{ id: mediaId, token: deleteToken }}),
        }});
        if (!response.ok) throw new Error('Nie udało się usunąć pliku.');
        document.querySelector('main').innerHTML = '<h1>Plik został usunięty.</h1>';
      }} catch (error) {{
        message.textContent = error.message;
        button.disabled = false;
      }}
    }});
  </script>
</body>
</html>"#,
        mime = media_mime_type(extension),
        video_attr = escape_html(video_name),
    )
}

pub(super) fn write_with_mode(path: &Path, contents: &[u8], mode: u32) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(mode)
        .open(path)?;
    file.write_all(contents)
}

/// Deletes player pages this bot published (`<32 hex>/` with `.delete-token`, or a leftover
/// `<32 hex>.tmp/` staging dir) older than `ttl`. Anything else in the media dir is left alone:
/// it is a public website directory shared with other content.
pub async fn cleanup_expired_pages(media_root: &Path, ttl: Duration) {
    let Some(cutoff) = std::time::SystemTime::now().checked_sub(ttl) else {
        return;
    };
    let Ok(mut entries) = tokio::fs::read_dir(media_root).await else {
        return;
    };
    let mut removed = 0usize;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let (id, staging) = match name.strip_suffix(".tmp") {
            Some(id) => (id, true),
            None => (name.as_str(), false),
        };
        if id.len() != 32
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            continue;
        }
        let path = entry.path();
        // symlink_metadata: never follow a link out of the media dir.
        let Ok(meta) = tokio::fs::symlink_metadata(&path).await else {
            continue;
        };
        if !meta.is_dir() || meta.modified().map_or(true, |m| m > cutoff) {
            continue;
        }
        if !staging
            && !tokio::fs::try_exists(path.join(".delete-token"))
                .await
                .unwrap_or(false)
        {
            continue;
        }
        match tokio::fs::remove_dir_all(&path).await {
            Ok(()) => removed += 1,
            Err(error) => log::warn(
                "Nie udało się usunąć wygasłej strony",
                Some(json!({ "id": id, "error": error.to_string() })),
            ),
        }
    }
    if removed > 0 {
        log::info(
            "Usunięto wygasłe strony z plikami",
            Some(json!({ "count": removed })),
        );
    }
}

/// `publishToWebsite(filePath)` → public player page URL.
pub(super) async fn publish_to_website(bot: &Bot, file_path: &Path) -> Result<String> {
    let media_root = bot.settings.website_root.join("media");
    cleanup_expired_pages(
        &media_root,
        Duration::from_secs(bot.settings.website_ttl_hours.saturating_mul(3600)),
    )
    .await;
    let id = hex(&rand::random::<[u8; 16]>());
    let staging = media_root.join(format!("{id}.tmp"));
    let target = media_root.join(&id);
    let filtered: String = extname(file_path)
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '.')
        .collect();
    let extension = if filtered.is_empty() {
        ".mp4".to_owned()
    } else {
        filtered
    };
    let video_name = format!("video{extension}");
    let display_name = file_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let delete_token = hex(&rand::random::<[u8; 32]>());
    let page_url = format!("{}/media/{id}/", bot.settings.public_base_url);

    let result: Result<()> = async {
        tokio::fs::create_dir_all(&staging).await?;
        tokio::fs::copy(file_path, staging.join(&video_name)).await?;
        let html = player_html(&id, &display_name, &video_name, &extension, &delete_token);
        let (staging_c, token_line) = (staging.clone(), format!("{delete_token}\n"));
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            write_with_mode(
                &staging_c.join(".delete-token"),
                token_line.as_bytes(),
                0o600,
            )?;
            write_with_mode(&staging_c.join("index.html"), html.as_bytes(), 0o640)
        })
        .await??;
        tokio::fs::rename(&staging, &target).await?;
        Ok(())
    }
    .await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(error);
    }
    let size = tokio::fs::metadata(file_path).await.map_or(0, |m| m.len());
    log::info(
        "Opublikowano plik na stronie",
        Some(json!({ "id": id, "sizeBytes": size, "pageUrl": page_url })),
    );
    Ok(page_url)
}
