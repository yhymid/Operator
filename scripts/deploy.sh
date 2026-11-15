#!/usr/bin/env bash
# Installs a released bot binary on the server, with a health check and automatic rollback.
#
#   deploy.sh discord discord-v1.2.0
#   deploy.sh telegram telegram-v1.2.0
#
# Steps: download the release asset + .sha256 from GitHub, verify, wait until the bot is idle,
# keep the current binary as <name>.prev, swap, restart the unit, and roll back if the unit is not
# active after HEALTH_SECONDS. Run as the bot user with sudo rights for `systemctl restart`.
#
# Environment (defaults match the current VPS layout):
#   BOT_DIR        directory with the binary and .env
#   UNIT           systemd unit of the bot
#   REPO           GitHub repository with the releases (yhymid/Operator)
#   HEALTH_SECONDS how long the unit must stay active after the restart (30)
#   IDLE_TIMEOUT   how long to wait for an idle bot before giving up (1800 s)
#   FORCE=1        do not wait for idle, reinstall the same tag
set -euo pipefail

bot=${1:-}
tag=${2:-}
case $bot in
    discord)
        binary=operator-discord
        BOT_DIR=${BOT_DIR:-"/home/ubuntu/Main Files/Bots/operator"}
        UNIT=${UNIT:-operator}
        ;;
    telegram)
        binary=operator-telegram
        BOT_DIR=${BOT_DIR:-"/home/ubuntu/Main Files/Bots/operator-tg"}
        UNIT=${UNIT:-operator-tg}
        ;;
    *) echo "usage: $0 discord|telegram <tag>" >&2; exit 2 ;;
esac
[[ $tag == "$bot"-v* ]] || { echo "the tag must start with $bot-v" >&2; exit 2; }
REPO=${REPO:-yhymid/Operator}
HEALTH_SECONDS=${HEALTH_SECONDS:-30}
IDLE_TIMEOUT=${IDLE_TIMEOUT:-1800}

log() { echo "[deploy $bot] $*"; }

target="$BOT_DIR/$binary"
installed_tag="$BOT_DIR/.release-tag"
if [[ ${FORCE:-0} != 1 && -f $installed_tag && $(cat "$installed_tag") == "$tag" ]]; then
    log "already on $tag, nothing to do"
    exit 0
fi

asset="$binary-$tag-x86_64-linux-gnu"
base="https://github.com/$REPO/releases/download/$tag"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

log "downloading $asset"
curl -fsSL --retry 3 -o "$work/$asset" "$base/$asset"
curl -fsSL --retry 3 -o "$work/$asset.sha256" "$base/$asset.sha256"
(cd "$work" && sha256sum -c "$asset.sha256" >/dev/null) || { log "SHA256 mismatch"; exit 1; }
chmod 755 "$work/$asset"

# Busy = yt-dlp/ffmpeg children in the unit's cgroup, or (discord) a Lavalink player.
busy() {
    local procs="/sys/fs/cgroup/system.slice/$UNIT.service/cgroup.procs"
    if [[ -r $procs ]] && (($(wc -l <"$procs") > 1)); then
        echo "child processes in $UNIT"
        return 0
    fi
    if [[ $bot == discord ]]; then
        local players
        players=$(curl -fsS --max-time 5 -H "Authorization: ${LAVALINK_PASSWORD:-youshallnotpass}" \
            http://127.0.0.1:2333/v4/stats 2>/dev/null |
            sed -n 's/.*"players":[[:space:]]*\([0-9]*\).*/\1/p')
        if [[ -n $players && $players != 0 ]]; then
            echo "Lavalink players=$players"
            return 0
        fi
    fi
    return 1
}

if [[ ${FORCE:-0} != 1 ]]; then
    deadline=$((SECONDS + IDLE_TIMEOUT))
    while reason=$(busy); do
        ((SECONDS < deadline)) || { log "the bot is still busy ($reason) - giving up, nothing changed"; exit 1; }
        log "waiting: $reason"
        sleep 30
    done
fi

[[ -f $target ]] && cp -p "$target" "$target.prev"
# New inode: the running process keeps its old file until the restart.
cp "$work/$asset" "$target.new"
mv -f "$target.new" "$target"
log "binary replaced, restarting $UNIT"
sudo systemctl restart "$UNIT"
restarts=$(systemctl show -p NRestarts --value "$UNIT")

sleep "$HEALTH_SECONDS"
# Restart=always would hide a crash loop from is-active alone: compare the restart counter too.
if systemctl is-active --quiet "$UNIT" &&
    [[ $(systemctl show -p NRestarts --value "$UNIT") == "$restarts" ]]; then
    echo "$tag" >"$installed_tag"
    log "OK: $UNIT runs $tag"
    exit 0
fi

log "$UNIT does not run correctly after the restart - restoring the previous binary"
journalctl -u "$UNIT" -n 20 --no-pager || true
if [[ -f $target.prev ]]; then
    mv -f "$target.prev" "$target"
    sudo systemctl restart "$UNIT"
    log "previous version restored"
fi
exit 1
