#!/usr/bin/env bash
# One update started from Discord (`/update target:...`) through operator-update@<target>.service:
#
#   ytdlp     yt-dlp --update-to $YTDLP_CHANNEL (default nightly)
#   lavalink  youtube-source plugin built from source (youtube-plugin-update.sh with SOURCE=1)
#   server    the Discord bot: newest discord-v* GitHub release through operator-deploy.sh
#             (waits until idle, health check, automatic rollback)
#
# Output goes to the journal and to /var/lib/operator-update/<target>.log, which the bot shows.
# Config (optional): /etc/operator/update.env
#   YTDLP_PATH=/usr/local/bin/yt-dlp   YTDLP_CHANNEL=nightly|stable
#   PLUGIN_UPDATE_SCRIPT=/usr/local/bin/youtube-plugin-update.sh
#   DEPLOY_SCRIPT=/usr/local/bin/operator-deploy.sh   BOT_USER=ubuntu   REPO=yhymid/Operator
#   BOT_DIR / UNIT                      passed to the deploy script (its defaults match the VPS)
set -euo pipefail

target=${1:-}
LOG_DIR=${STATE_DIRECTORY:-/var/lib/operator-update}
case $target in
    ytdlp | lavalink | server) ;;
    *) echo "usage: $0 ytdlp|lavalink|server" >&2; exit 2 ;;
esac
mkdir -p "$LOG_DIR"
exec > >(tee "$LOG_DIR/$target.log") 2>&1

case $target in
    ytdlp)
        ytdlp=${YTDLP_PATH:-$(command -v yt-dlp)}
        before=$("$ytdlp" --version)
        "$ytdlp" --update-to "${YTDLP_CHANNEL:-nightly}"
        after=$("$ytdlp" --version)
        if [[ $before == "$after" ]]; then
            echo "yt-dlp is up to date ($after)"
        else
            echo "yt-dlp $before -> $after"
        fi
        ;;
    lavalink)
        SOURCE=1 exec "${PLUGIN_UPDATE_SCRIPT:-/usr/local/bin/youtube-plugin-update.sh}"
        ;;
    server)
        repo=${REPO:-yhymid/Operator}
        tag=$(curl -fsS --max-time 20 "https://api.github.com/repos/$repo/releases?per_page=30" |
            grep -o '"tag_name": *"discord-v[^"]*"' | head -n1 | cut -d'"' -f4) || true
        if [[ -z $tag ]]; then
            echo "no discord-v* release found in $repo"
            exit 1
        fi
        echo "newest release: $tag"
        exec runuser -u "${BOT_USER:-ubuntu}" -- \
            env ${BOT_DIR:+BOT_DIR="$BOT_DIR"} ${UNIT:+UNIT="$UNIT"} REPO="$repo" \
            bash "${DEPLOY_SCRIPT:-/usr/local/bin/operator-deploy.sh}" discord "$tag"
        ;;
esac
