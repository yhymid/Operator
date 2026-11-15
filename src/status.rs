//! `createStatusUpdater(ctx, status)`: status-message edits for one job are applied strictly
//! in submission order, and an edit with unchanged text is skipped (`status._lastText`).

use std::sync::Arc;

use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use crate::api::Api;
use crate::log;

type Edit = (String, Option<oneshot::Sender<()>>);

#[derive(Clone)]
pub struct StatusUpdater {
    tx: mpsc::UnboundedSender<Edit>,
}

impl StatusUpdater {
    /// `message_id = None` (status message could not be read) turns every update into a no-op.
    pub fn new(api: Arc<Api>, chat_id: Option<i64>, message_id: Option<i64>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Edit>();
        tokio::spawn(async move {
            let mut last_text: Option<String> = None;
            while let Some((text, ack)) = rx.recv().await {
                if let (Some(chat_id), Some(message_id)) = (chat_id, message_id)
                    && last_text.as_deref() != Some(text.as_str())
                {
                    last_text = Some(text.clone());
                    let payload =
                        json!({ "chat_id": chat_id, "message_id": message_id, "text": text });
                    if let Err(error) = api.call("editMessageText", payload).await {
                        log::debug(
                            "Nie udało się zaktualizować statusu /ytdlp",
                            Some(json!({
                                "chatId": chat_id,
                                "messageId": message_id,
                                "details": error.message,
                            })),
                        );
                    }
                }
                if let Some(ack) = ack {
                    let _ = ack.send(());
                }
            }
        });
        Self { tx }
    }

    /// Updater that accepts and acknowledges every edit without talking to Telegram.
    #[cfg(test)]
    pub fn noop() -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<Edit>();
        tokio::spawn(async move {
            while let Some((_, ack)) = rx.recv().await {
                if let Some(ack) = ack {
                    let _ = ack.send(());
                }
            }
        });
        Self { tx }
    }

    /// Queue an edit and wait until it (and everything before it) was applied.
    pub async fn set(&self, text: impl Into<String>) {
        let (ack, done) = oneshot::channel();
        if self.tx.send((text.into(), Some(ack))).is_ok() {
            let _ = done.await;
        }
    }

    /// Queue an edit without waiting (queue position callbacks).
    pub fn set_detached(&self, text: impl Into<String>) {
        let _ = self.tx.send((text.into(), None));
    }
}
