//! Runtime paths (`OPERATOR_CONFIG_PATH`, `OPERATOR_DATA_DIR`, `OPERATOR_LOG_DIR`).
//!
//! Relative values are resolved against the working directory, which plays the role of the
//! JS project root (run the binary from the project directory or set `WorkingDirectory=`).

use std::path::{Path, PathBuf};

use crate::common::env;

#[derive(Debug, Clone)]
pub struct OperatorPaths {
    pub config_path: PathBuf,
    pub data_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub server_cache_dir: PathBuf,
    pub music_queue_dir: PathBuf,
    pub server_logs_dir: PathBuf,
    pub sync_commands_on_startup: bool,
}

impl OperatorPaths {
    pub fn from_env() -> Self {
        let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let config_path = resolve(
            &project_root,
            env::var("OPERATOR_CONFIG_PATH"),
            "config.json",
        );
        let data_dir = resolve(&project_root, env::var("OPERATOR_DATA_DIR"), "cache");
        let logs_dir = resolve(&project_root, env::var("OPERATOR_LOG_DIR"), "logs");

        Self {
            server_cache_dir: data_dir.join("server"),
            music_queue_dir: data_dir.join("music-queue"),
            server_logs_dir: logs_dir.join("server"),
            sync_commands_on_startup: env::bool("OPERATOR_SYNC_COMMANDS_ON_STARTUP", true),
            config_path,
            data_dir,
            logs_dir,
        }
    }
}

fn resolve(root: &Path, raw: Option<String>, fallback: &str) -> PathBuf {
    match raw {
        Some(value) if Path::new(&value).is_absolute() => PathBuf::from(value),
        Some(value) => root.join(value),
        None => root.join(fallback),
    }
}
