#!/usr/bin/env bash
# Updates the youtube-source plugin snapshot used by Lavalink, safely.
#
#  1. reads the newest snapshot version from the Lavalink snapshot maven-metadata.xml,
#  2. exits when application.yml already pins it (or when it was rolled back before),
#  3. waits for an idle node: GET /v4/stats -> playingPlayers == 0 (otherwise exit 0, next timer run retries),
#  4. backs up application.yml + plugins/, swaps the dependency line, restarts Lavalink,
#  5. health check: /version answers, then a known video loads (/v4/loadtracks) and streams
#     (/youtube/stream/{id}); on failure restores the backup, restarts again and remembers the bad version.
#
# Run as root from youtube-plugin-update.service (needs `systemctl restart lavalink`).
# Config (env, e.g. /etc/operator/youtube-plugin-update.env):
#   LAVALINK_DIR=/srv/operator/lavalink   directory with application.yml and plugins/
#   LAVALINK_URL=http://127.0.0.1:2333
#   LAVALINK_PASSWORD=...                 default: lavalink.server.password from application.yml
#   LAVALINK_UNIT=lavalink
#   HEALTH_VIDEO_ID=jNQXAC9IVRw           any public, non age/region restricted video
#   DRY_RUN=1                             only report what would happen
# Exit codes: 0 = nothing to do / updated / postponed, 1 = update failed and rolled back, 2 = setup error.
set -euo pipefail

LAVALINK_DIR=${LAVALINK_DIR:-/srv/operator/lavalink}
LAVALINK_URL=${LAVALINK_URL:-http://127.0.0.1:2333}
LAVALINK_UNIT=${LAVALINK_UNIT:-lavalink}
HEALTH_VIDEO_ID=${HEALTH_VIDEO_ID:-jNQXAC9IVRw}
METADATA_URL=${METADATA_URL:-https://maven.lavalink.dev/snapshots/dev/lavalink/youtube/youtube-plugin/maven-metadata.xml}
STARTUP_TIMEOUT=${STARTUP_TIMEOUT:-180}
DRY_RUN=${DRY_RUN:-0}

CONFIG="$LAVALINK_DIR/application.yml"
STATE_DIR="$LAVALINK_DIR/.youtube-plugin-update"
ARTIFACT="dev.lavalink.youtube:youtube-plugin"

log() { echo "[yt-plugin-update] $*"; }
die() { log "ERROR: $*"; exit 2; }

[[ -f $CONFIG ]] || die "brak $CONFIG"
command -v curl >/dev/null || die "brak curl"
mkdir -p "$STATE_DIR"

# One run at a time (timer + manual run).
exec 9>"$STATE_DIR/lock"
flock -n 9 || { log "inna instancja dziala, koniec"; exit 0; }

if [[ -z ${LAVALINK_PASSWORD:-} ]]; then
    # First `password:` in the file is lavalink.server.password in application.yml.example.
    LAVALINK_PASSWORD=$(sed -n 's/^[[:space:]]*password:[[:space:]]*"\{0,1\}\([^"]*\)"\{0,1\}[[:space:]]*$/\1/p' "$CONFIG" | head -n1)
fi
[[ -n $LAVALINK_PASSWORD ]] || die "brak LAVALINK_PASSWORD"

# Password goes through a curl config on stdin, never on the command line (visible in ps).
api() { # api <path> [extra curl args...]
    local path=$1; shift
    printf 'header = "Authorization: %s"\n' "$LAVALINK_PASSWORD" |
        curl -fsS --max-time 20 -K - "$@" "$LAVALINK_URL$path"
}

current_version() {
    sed -n "s/.*\"$ARTIFACT:\([^\"]*\)\".*/\1/p" "$CONFIG" | head -n1
}

latest_version() {
    local xml
    xml=$(curl -fsS --max-time 20 "$METADATA_URL") || return 1
    # Prefer <latest>; fall back to the last <version> entry.
    local v
    v=$(printf '%s' "$xml" | sed -n 's:.*<latest>\([^<]*\)</latest>.*:\1:p' | head -n1)
    [[ -n $v ]] || v=$(printf '%s' "$xml" | sed -n 's:.*<version>\([^<]*\)</version>.*:\1:p' | tail -n1)
    [[ $v =~ ^[A-Za-z0-9._-]+$ ]] || return 1
    printf '%s' "$v"
}

playing_players() {
    api /v4/stats | sed -n 's/.*"playingPlayers":[[:space:]]*\([0-9]*\).*/\1/p'
}

wait_ready() {
    local deadline=$((SECONDS + STARTUP_TIMEOUT))
    while ((SECONDS < deadline)); do
        api /version >/dev/null 2>&1 && return 0
        sleep 5
    done
    return 1
}

health_check() {
    wait_ready || { log "Lavalink nie odpowiada po ${STARTUP_TIMEOUT}s"; return 1; }
    # Plugins load before the REST server starts, but give the YouTube clients a moment.
    sleep 5
    local load
    load=$(api "/v4/loadtracks?identifier=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3D$HEALTH_VIDEO_ID") || {
        log "loadtracks nie odpowiada"; return 1; }
    if ! grep -q '"loadType":[[:space:]]*"track"' <<<"$load"; then
        log "loadtracks: brak utworu ($(grep -o '"message":[[:space:]]*"[^"]*"' <<<"$load" | head -n1))"
        return 1
    fi
    # Real playback path (deciphering, client fallback): read the first 64 KiB of the stream.
    local bytes
    bytes=$(api "/youtube/stream/$HEALTH_VIDEO_ID" -r 0-65535 -o /dev/null -w '%{size_download}') || {
        log "/youtube/stream nie dziala"; return 1; }
    ((bytes > 0)) || { log "/youtube/stream zwrocil 0 bajtow"; return 1; }
    log "health OK (loadtracks + stream ${bytes} B)"
}

# Writes through the existing inode so owner/mode stay as they were (we run as root).
write_config() { # write_config <source>
    cat "$1" >"$CONFIG"
}

current=$(current_version)
[[ -n $current ]] || die "nie znaleziono $ARTIFACT w $CONFIG"
latest=$(latest_version) || die "nie udalo sie odczytac $METADATA_URL"

if [[ $latest == "$current" ]]; then
    log "aktualna wersja $current, nic do zrobienia"
    exit 0
fi
if grep -qxF "$latest" "$STATE_DIR/bad-versions" 2>/dev/null; then
    log "wersja $latest zostala wczesniej wycofana, pomijam"
    exit 0
fi

playing=$(playing_players || true)
if [[ $playing != 0 ]]; then
    log "nowa wersja $latest, ale playingPlayers=${playing:-?} - przekladam na nastepne uruchomienie"
    exit 0
fi

log "aktualizacja $current -> $latest"
if [[ $DRY_RUN == 1 ]]; then
    log "DRY_RUN=1, koniec"
    exit 0
fi

backup="$STATE_DIR/backup-$(date +%Y%m%dT%H%M%S)"
mkdir -p "$backup"
cp -p "$CONFIG" "$backup/application.yml"
[[ -d $LAVALINK_DIR/plugins ]] && cp -pr "$LAVALINK_DIR/plugins" "$backup/plugins"

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
sed "s|\"$ARTIFACT:$current\"|\"$ARTIFACT:$latest\"|" "$CONFIG" >"$tmp"
grep -qF "\"$ARTIFACT:$latest\"" "$tmp" || die "podmiana wersji w $CONFIG nie powiodla sie"
write_config "$tmp"

systemctl restart "$LAVALINK_UNIT"
if health_check; then
    log "zaktualizowano do $latest (kopia: $backup)"
    # Keep the 5 newest backups.
    find "$STATE_DIR" -maxdepth 1 -name 'backup-*' -type d | sort | head -n -5 | xargs -r rm -rf
    exit 0
fi

log "health check nie przeszedl - przywracam $current"
echo "$latest" >>"$STATE_DIR/bad-versions"
write_config "$backup/application.yml"
if [[ -d $backup/plugins ]]; then
    rm -rf "$LAVALINK_DIR/plugins"
    cp -pr "$backup/plugins" "$LAVALINK_DIR/plugins"
fi
systemctl restart "$LAVALINK_UNIT"
if health_check; then
    log "przywrocono $current"
else
    log "UWAGA: po przywroceniu $current health check tez nie przechodzi (problem po stronie YouTube/sieci?)"
fi
exit 1
