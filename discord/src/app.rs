//! Shared application context.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use twilight_cache_inmemory::DefaultInMemoryCache;
use twilight_gateway::MessageSender;
use twilight_http::Client as HttpClient;
use twilight_model::id::Id;
use twilight_model::id::marker::{ApplicationMarker, GuildMarker, UserMarker};

use crate::modules::autoclean::AutoClean;
use crate::modules::automod::AutoMod;
use crate::modules::fulllogs::FullLogs;
use crate::modules::music::MusicManager;
use crate::paths::OperatorPaths;
use crate::rest::RawRest;
use crate::state::StateStore;

pub struct App {
    pub http: Arc<HttpClient>,
    pub raw: RawRest,
    pub cache: DefaultInMemoryCache,
    pub application_id: Id<ApplicationMarker>,
    pub bot_id: Id<UserMarker>,
    pub bot_tag: String,
    pub paths: OperatorPaths,
    pub state: StateStore,
    pub sender: MessageSender,
    pub startup_verbose: bool,
    pub autoclean: AutoClean,
    pub automod: AutoMod,
    pub fulllogs: FullLogs,
    pub music: MusicManager,
    pub shutting_down: AtomicBool,
}

impl App {
    pub fn guild_ids(&self) -> Vec<Id<GuildMarker>> {
        self.cache.iter().guilds().map(|g| *g.key()).collect()
    }

    pub fn guild_count(&self) -> usize {
        self.cache.stats().guilds()
    }

    pub fn guild_name(&self, guild_id: Id<GuildMarker>) -> Option<String> {
        self.cache.guild(guild_id).map(|g| g.name().to_owned())
    }

    pub fn is_owner(&self, user_id: Id<UserMarker>) -> bool {
        crate::owners::is_owner(&self.paths.config_path, user_id)
    }
}

pub type SharedApp = Arc<App>;
