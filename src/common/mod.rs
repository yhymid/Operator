//! Shared runtime helpers for the Operator bots.
//!
//! Everything here mirrors behaviour of the original JavaScript implementation so the
//! Rust bots can read and write the same `.env` files and on-disk JSON state.

// Shared by the Discord and Telegram bots (kept identical on both branches); each bot
// uses only part of it.
#![allow(dead_code)]

/// Git commit the binary was built from (`build.rs`), `unknown` without git.
pub const GIT_COMMIT: &str = env!("OPERATOR_GIT_COMMIT");
/// `2.0.0 (abc1234)`
pub const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("OPERATOR_GIT_COMMIT"),
    ")"
);

pub mod diagnostics;
pub mod env;
pub mod fs;
pub mod logging;
pub mod media;
pub mod net;
pub mod shutdown;
pub mod time;
