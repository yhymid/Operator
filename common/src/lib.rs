//! Shared runtime helpers for the Operator bots.
//!
//! Everything here mirrors behaviour of the original JavaScript implementation so the
//! Rust bots can read and write the same `.env` files and on-disk JSON state.

pub mod env;
pub mod fs;
pub mod logging;
pub mod shutdown;
pub mod time;
