//! `tracing` setup. `RUST_LOG` overrides the default level; `verbose` lowers it to debug.

use tracing_subscriber::EnvFilter;

pub fn init(verbose: bool) {
    let default = if verbose { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    // systemd/journald already timestamps every line; keep the output compact there.
    let under_journald = std::env::var_os("JOURNAL_STREAM").is_some();

    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(!under_journald);

    if under_journald {
        builder.without_time().init();
    } else {
        builder.init();
    }
}
