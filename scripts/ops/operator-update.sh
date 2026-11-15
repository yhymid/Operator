#!/usr/bin/env bash
# One update started from Discord (`/update target:...`) through operator-update@<target>.service:
#
#   ytdlp     yt-dlp --update-to $YTDLP_CHANNEL (default nightly)
#   lavalink  youtube-source plugin built from source (youtube-plugin-update.sh with SOURCE=1)
#   server    the whole server: apt-get update + upgrade (non-interactive, config files kept,
#             services are NOT restarted by needrestart); reports when a reboot is needed
#   operator  both bots: newest telegram-v* and discord-v* GitHub releases through
#             operator-deploy.sh (each waits until idle, health check, automatic rollback)
#
# Output goes to the journal and to /var/lib/operator-update/<target>.log, which the bot shows.
# Config (optional): /etc/operator/update.env
#   YTDLP_PATH=/usr/local/bin/yt-dlp   YTDLP_CHANNEL=nightly|stable
#   PLUGIN_UPDATE_SCRIPT=/usr/local/bin/youtube-plugin-update.sh
#   DEPLOY_SCRIPT=/usr/local/bin/operator-deploy.sh   BOT_USER=ubuntu   REPO=yhymid/Operator
#   (BOT_DIR / UNIT defaults of the deploy script match the VPS layout)
set -euo pipefail

target=${1:-}
LOG_DIR=${STATE_DIRECTORY:-/var/lib/operator-update}
case $target in
    ytdlp | lavalink | server | operator) ;;
    *) echo "usage: $0 ytdlp|lavalink|server|operator" >&2; exit 2 ;;
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
        export DEBIAN_FRONTEND=noninteractive
        # List only: needrestart must not restart the bots or Lavalink in the middle of music.
        export NEEDRESTART_MODE=l
        apt-get update -q
        apt-get -y -q -o Dpkg::Options::=--force-confold -o Dpkg::Options::=--force-confdef \
            upgrade
        apt-get -y -q autoremove --purge
        if [[ -f /var/run/reboot-required ]]; then
            echo "REBOOT REQUIRED: $(tr '\n' ' ' </var/run/reboot-required.pkgs 2>/dev/null)"
        else
            echo "no reboot needed"
        fi
        ;;
    operator)
        repo=${REPO:-yhymid/Operator}
        releases=$(curl -fsS --max-time 20 "https://api.github.com/repos/$repo/releases?per_page=50") ||
            { echo "cannot read the releases of $repo"; exit 1; }
        status=0
        # Telegram first: the Discord deploy restarts the bot that started this update.
        for bot in telegram discord; do
            tag=$(grep -o "\"tag_name\": *\"$bot-v[^\"]*\"" <<<"$releases" | head -n1 | cut -d'"' -f4)
            if [[ -z $tag ]]; then
                echo "[$bot] no $bot-v* release in $repo, skipped"
                continue
            fi
            echo "[$bot] newest release: $tag"
            runuser -u "${BOT_USER:-ubuntu}" -- env REPO="$repo" \
                bash "${DEPLOY_SCRIPT:-/usr/local/bin/operator-deploy.sh}" "$bot" "$tag" || status=1
        done
        exit $status
        ;;
esac
