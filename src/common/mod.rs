//! Shared runtime helpers for the Operator bots.
//!
//! Everything here mirrors behaviour of the original JavaScript implementation so the
//! Rust bots can read and write the same `.env` files and on-disk JSON state.

// Shared by the Discord and Telegram bots (kept identical on both branches); each bot
// uses only part of it.
#![allow(dead_code)]

pub mod env;
pub mod fs;
pub mod logging;
pub mod media;
pub mod net;
pub mod shutdown;
pub mod time;
