//! `tracing` setup. `RUST_LOG` overrides the default level; `verbose` lowers it to debug.
//! Every line also goes to the diagnostics recorder when it is enabled.

use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

pub fn init(verbose: bool) {
    let default = if verbose { "debug" } else { "info" };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));
    // systemd/journald already timestamps every line; keep the output compact there.
    let under_journald = std::env::var_os("JOURNAL_STREAM").is_some();
    let with_time = (!under_journald).then(|| {
        tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_ansi(true)
    });
    let without_time = under_journald.then(|| {
        tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_ansi(false)
            .without_time()
    });
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(with_time)
        .with(without_time)
        .with(super::diagnostics::TracingLayer)
        .try_init();
}
