#!/usr/bin/env bash
# Updates the youtube-source plugin used by Lavalink, safely. Two modes:
#
#  - default: the newest snapshot (CI build of the latest commit) from the Lavalink maven
#    repository, pinned as a `dependency:` line in application.yml;
#  - SOURCE=1 (used by `/update target:lavalink`): clones lavalink-devs/youtube-source, builds
#    the plugin jar from the newest commit (`./gradlew :plugin:build`, as upstream CI does) and
#    drops it into plugins/; the `dependency:` line is commented out so only the built jar loads.
#
#  1. finds the newest version (snapshot version or git commit),
#  2. exits when it is already installed (or when it was rolled back before),
#  3. waits for an idle node: GET /v4/stats -> players == 0, paused sessions included
#     (otherwise exit 0, next timer run retries),
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
#   SOURCE=1                              build from source instead of using the snapshot
#   SOURCE_REPO / SOURCE_BRANCH           default lavalink-devs/youtube-source, main
#   BUILD_JAVA_HOME                       JDK used for the build; default: the newest
#                                         /usr/lib/jvm/java-{21,17}-openjdk-* found.
#                                         Not Java 25: upstream pins Gradle 8.10, which fails
#                                         on 25 ("What went wrong: 25.0.5", checked 2026-11-15).
#                                         The jar targets Java 8 and runs on Lavalink's Java 25.
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

[[ -f $CONFIG ]] || die "missing $CONFIG"
command -v curl >/dev/null || die "curl is not installed"
mkdir -p "$STATE_DIR"

# One run at a time (timer + manual run).
exec 9>"$STATE_DIR/lock"
flock -n 9 || { log "another run is in progress, exiting"; exit 0; }

if [[ -z ${LAVALINK_PASSWORD:-} ]]; then
    # First `password:` in the file is lavalink.server.password in application.yml.example.
    LAVALINK_PASSWORD=$(sed -n 's/^[[:space:]]*password:[[:space:]]*"\{0,1\}\([^"]*\)"\{0,1\}[[:space:]]*$/\1/p' "$CONFIG" | head -n1)
fi
[[ -n $LAVALINK_PASSWORD ]] || die "LAVALINK_PASSWORD is not set"

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

# All players, not only playing ones: a restart also drops paused sessions and their queues.
active_players() {
    api /v4/stats | sed -n 's/.*"players":[[:space:]]*\([0-9]*\).*/\1/p'
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
    wait_ready || { log "Lavalink does not answer after ${STARTUP_TIMEOUT}s"; return 1; }
    # Plugins load before the REST server starts, but give the YouTube clients a moment.
    sleep 5
    local load
    load=$(api "/v4/loadtracks?identifier=https%3A%2F%2Fwww.youtube.com%2Fwatch%3Fv%3D$HEALTH_VIDEO_ID") || {
        log "loadtracks does not answer"; return 1; }
    if ! grep -q '"loadType":[[:space:]]*"track"' <<<"$load"; then
        log "loadtracks: no track ($(grep -o '"message":[[:space:]]*"[^"]*"' <<<"$load" | head -n1))"
        return 1
    fi
    # Real playback path (deciphering, client fallback): read the first 64 KiB of the stream.
    local bytes
    bytes=$(api "/youtube/stream/$HEALTH_VIDEO_ID" -r 0-65535 -o /dev/null -w '%{size_download}') || {
        log "/youtube/stream does not work"; return 1; }
    ((bytes > 0)) || { log "/youtube/stream returned 0 bytes"; return 1; }
    log "health OK (loadtracks + stream ${bytes} B)"
}

# Writes through the existing inode so owner/mode stay as they were (we run as root).
write_config() { # write_config <source>
    cat "$1" >"$CONFIG"
}

# --- what is installed / what is newest -----------------------------------------------------

SOURCE=${SOURCE:-0}
SOURCE_REPO=${SOURCE_REPO:-https://github.com/lavalink-devs/youtube-source.git}
SOURCE_BRANCH=${SOURCE_BRANCH:-main}
SOURCE_DIR="$STATE_DIR/source"
PLUGINS_DIR="$LAVALINK_DIR/plugins"

find_build_jdk() {
    if [[ -n ${BUILD_JAVA_HOME:-} ]]; then
        printf '%s' "$BUILD_JAVA_HOME"
        return
    fi
    local jdk
    for jdk in /usr/lib/jvm/java-21-openjdk-* /usr/lib/jvm/java-17-openjdk-*; do
        [[ -x $jdk/bin/javac ]] && { printf '%s' "$jdk"; return; }
    done
    return 1
}

if [[ $SOURCE == 1 ]]; then
    command -v git >/dev/null || die "git is not installed"
    build_jdk=$(find_build_jdk) || die "no JDK 17/21 for the build (apt install openjdk-21-jdk-headless) or set BUILD_JAVA_HOME"
    if [[ -d $SOURCE_DIR/.git ]]; then
        git -C "$SOURCE_DIR" fetch --quiet origin "$SOURCE_BRANCH"
    else
        git clone --quiet --filter=blob:none --branch "$SOURCE_BRANCH" "$SOURCE_REPO" "$SOURCE_DIR"
    fi
    latest=$(git -C "$SOURCE_DIR" rev-parse "origin/$SOURCE_BRANCH")
    current=$(cat "$STATE_DIR/source-commit" 2>/dev/null || echo "snapshot:$(current_version)")
else
    if [[ -f $STATE_DIR/source-commit ]]; then
        log "the plugin is built from source ($(cat "$STATE_DIR/source-commit")); snapshot updates are off." \
            "Use SOURCE=1, or delete $STATE_DIR/source-commit and restore the dependency line to switch back."
        exit 0
    fi
    current=$(current_version)
    [[ -n $current ]] || die "$ARTIFACT not found in $CONFIG"
    latest=$(latest_version) || die "could not read $METADATA_URL"
fi

if [[ $latest == "$current" || "snapshot:$latest" == "$current" ]]; then
    log "already up to date ($current)"
    exit 0
fi
if grep -qxF "$latest" "$STATE_DIR/bad-versions" 2>/dev/null; then
    log "version $latest was rolled back before, skipping"
    exit 0
fi

players=$(active_players || true)
if [[ $players != 0 ]]; then
    log "new version $latest, but players=${players:-?} - postponed to the next run"
    exit 0
fi

if [[ $SOURCE == 1 ]]; then
    log "building youtube-source $(git -C "$SOURCE_DIR" log -1 --format='%h %s (%cs)' "$latest") with $build_jdk"
    git -C "$SOURCE_DIR" checkout --quiet --force --detach "$latest"
    if ! (
        cd "$SOURCE_DIR"
        JAVA_HOME=$build_jdk GRADLE_USER_HOME="$STATE_DIR/gradle" \
            ./gradlew :plugin:build -x test --no-daemon --console=plain --quiet
    ) >"$STATE_DIR/build.log" 2>&1; then
        tail -n 15 "$STATE_DIR/build.log"
        die "build failed (full log: $STATE_DIR/build.log), nothing changed"
    fi
    jar=$(find "$SOURCE_DIR/plugin/build/libs" -maxdepth 1 -name 'youtube-plugin-*.jar' \
        ! -name '*-sources.jar' ! -name '*-javadoc.jar' | head -n1)
    [[ -n $jar ]] || die "build produced no plugin jar, nothing changed"
    log "built $(basename "$jar")"
fi

log "update $current -> $latest"
if [[ $DRY_RUN == 1 ]]; then
    log "DRY_RUN=1, stopping here"
    exit 0
fi

backup="$STATE_DIR/backup-$(date +%Y%m%dT%H%M%S)"
mkdir -p "$backup"
cp -p "$CONFIG" "$backup/application.yml"
[[ -d $PLUGINS_DIR ]] && cp -pr "$PLUGINS_DIR" "$backup/plugins"

tmp=$(mktemp)
trap 'rm -f "$tmp"' EXIT
if [[ $SOURCE == 1 ]]; then
    # Only the built jar may provide the plugin: comment out the maven dependency (and its
    # `snapshot:` line) so Lavalink does not download a second copy.
    awk -v artifact="\"$ARTIFACT:" '
        index($0, artifact) && $0 !~ /^[[:space:]]*#/ { print "#source-build# " $0; skip = 1; next }
        skip && /^[[:space:]]*snapshot:/ { print "#source-build# " $0; skip = 0; next }
        { skip = 0; print }
    ' "$CONFIG" >"$tmp"
    write_config "$tmp"
    mkdir -p "$PLUGINS_DIR"
    owner=$(stat -c '%U:%G' "$PLUGINS_DIR")
    find "$PLUGINS_DIR" -maxdepth 1 -name 'youtube-plugin-*.jar' -delete
    installed="$PLUGINS_DIR/youtube-plugin-src-${latest:0:12}.jar"
    cp "$jar" "$installed"
    chown "$owner" "$installed"
else
    sed "s|\"$ARTIFACT:$current\"|\"$ARTIFACT:$latest\"|" "$CONFIG" >"$tmp"
    grep -qF "\"$ARTIFACT:$latest\"" "$tmp" || die "replacing the version in $CONFIG failed"
    write_config "$tmp"
fi

systemctl restart "$LAVALINK_UNIT"
if health_check; then
    [[ $SOURCE == 1 ]] && echo "$latest" >"$STATE_DIR/source-commit"
    log "updated to $latest (backup: $backup)"
    # Keep the 5 newest backups.
    find "$STATE_DIR" -maxdepth 1 -name 'backup-*' -type d | sort | head -n -5 | xargs -r rm -rf
    exit 0
fi

log "health check failed - restoring $current"
echo "$latest" >>"$STATE_DIR/bad-versions"
write_config "$backup/application.yml"
if [[ -d $backup/plugins ]]; then
    rm -rf "$PLUGINS_DIR"
    cp -pr "$backup/plugins" "$PLUGINS_DIR"
fi
systemctl restart "$LAVALINK_UNIT"
if health_check; then
    log "restored $current"
else
    # The old version fails too: YouTube/network, not the new build. Do not blacklist it.
    log "WARNING: after restoring $current the health check fails too (YouTube/network problem?)"
    sed -i "/^$latest\$/d" "$STATE_DIR/bad-versions"
fi
exit 1
