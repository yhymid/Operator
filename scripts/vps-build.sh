#!/usr/bin/env bash
# Builds both bots from GitHub on the VPS (the binary must match the server's glibc).
# Lives in ~/operator-src/build.sh on the server; clones/updates ~/operator-src/{discord,telegram}
# and builds them with `nice -n 15 -j 3`, so the other services keep their CPU.
# Next step: scripts/vps-deploy.sh. Installs rustup into ~/.cargo when cargo is missing.
set -euo pipefail
cd ~/operator-src
if ! command -v ~/.cargo/bin/cargo >/dev/null; then
    curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable --no-modify-path
fi
export PATH="$HOME/.cargo/bin:$PATH"
rustc --version
for bot in telegram discord; do
    if [ -d $bot/.git ]; then git -C $bot fetch -q origin $bot && git -C $bot reset -q --hard origin/$bot
    else git clone -q -b $bot --single-branch https://github.com/yhymid/Operator.git $bot; fi
    echo "== $bot $(git -C $bot log -1 --format="%h %s")"
    (cd $bot && nice -n 15 cargo build --release --locked -j 3 2>&1 | tail -3)
    ls -l $bot/target/release/operator-$bot
done
echo BUILD_DONE
