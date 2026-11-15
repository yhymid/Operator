//! The bot's own status ("Playing ...", custom status), set with `/presence` and kept in
//! `<dataDir>/presence.json` so it survives restarts.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use twilight_model::gateway::OpCode;
use twilight_model::gateway::payload::outgoing::UpdatePresence;
use twilight_model::gateway::payload::outgoing::update_presence::UpdatePresencePayload;
use twilight_model::gateway::presence::{ActivityType, MinimalActivity, Status};

use crate::app::App;

pub const KINDS: [&str; 5] = ["playing", "listening", "watching", "competing", "custom"];
/// Discord's limit for an activity name / custom status.
pub const MAX_TEXT: usize = 128;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    /// One of [`KINDS`]; empty = no activity.
    pub kind: String,
    pub text: String,
}

fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("presence.json")
}

pub fn load(data_dir: &Path) -> Presence {
    std::fs::read_to_string(path(data_dir))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save(data_dir: &Path, presence: &Presence) -> std::io::Result<()> {
    let json = serde_json::to_string_pretty(presence).map_err(std::io::Error::other)?;
    std::fs::write(path(data_dir), json)
}

fn command(presence: &Presence) -> UpdatePresence {
    let text: String = presence.text.trim().chars().take(MAX_TEXT).collect();
    let activities = if presence.kind.is_empty() || text.is_empty() {
        Vec::new()
    } else {
        let kind = match presence.kind.as_str() {
            "listening" => ActivityType::Listening,
            "watching" => ActivityType::Watching,
            "competing" => ActivityType::Competing,
            "custom" => ActivityType::Custom,
            _ => ActivityType::Playing,
        };
        let custom = kind == ActivityType::Custom;
        let mut activity: twilight_model::gateway::presence::Activity = MinimalActivity {
            kind,
            // Custom statuses show `state`; Discord requires a name anyway.
            name: if custom {
                "Custom Status".to_owned()
            } else {
                text.clone()
            },
            url: None,
        }
        .into();
        if custom {
            activity.state = Some(text);
        }
        vec![activity]
    };
    // Built directly: `UpdatePresence::new` refuses an empty list, which is how a status is
    // cleared.
    UpdatePresence {
        d: UpdatePresencePayload {
            activities,
            afk: false,
            since: None,
            status: Status::Online,
        },
        op: OpCode::PresenceUpdate,
    }
}

/// Sends the saved presence (or clears it) on the gateway.
pub fn apply(app: &App) {
    let presence = load(&app.paths.data_dir);
    if let Err(error) = app.sender.command(&command(&presence)) {
        tracing::warn!("Presence update failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_activities() {
        let playing = command(&Presence {
            kind: "playing".into(),
            text: "music".into(),
        });
        assert_eq!(playing.d.activities[0].kind, ActivityType::Playing);
        assert_eq!(playing.d.activities[0].name, "music");

        let custom = command(&Presence {
            kind: "custom".into(),
            text: "hello".into(),
        });
        assert_eq!(custom.d.activities[0].state.as_deref(), Some("hello"));

        let cleared = command(&Presence::default());
        assert!(cleared.d.activities.is_empty());
    }
}
