//! Embeds the git commit as `OPERATOR_GIT_COMMIT` (shown by `--version`, /health and the
//! diagnostics). `OPERATOR_GIT_COMMIT` from the environment wins; without git it is `unknown`.

use std::path::Path;
use std::process::Command;

fn main() {
    let commit = std::env::var("OPERATOR_GIT_COMMIT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--short=7", "HEAD"])
                .output()
                .ok()
                .filter(|out| out.status.success())
                .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
                .filter(|v| !v.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=OPERATOR_GIT_COMMIT={commit}");
    println!("cargo:rerun-if-env-changed=OPERATOR_GIT_COMMIT");
    for path in [".git/HEAD", ".git/refs/heads", ".git/packed-refs"] {
        if Path::new(path).exists() {
            println!("cargo:rerun-if-changed={path}");
        }
    }
}
