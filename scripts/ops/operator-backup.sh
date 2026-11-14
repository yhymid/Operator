#!/usr/bin/env bash
# Daily backup of the bots' state: .env files, config.json, guild state cache, full logs and
# the Lavalink application.yml. Secrets inside, so the archive is chmod 600 and stays on the
# server unless BACKUP_COPY_CMD copies it elsewhere (e.g. `rclone copy {} remote:operator`).
#
# Environment: BOTS_DIR, BACKUP_DIR, KEEP (days), BACKUP_COPY_CMD (optional, {} = archive path).
set -euo pipefail

BOTS_DIR=${BOTS_DIR:-"/home/ubuntu/Main Files/Bots"}
BACKUP_DIR=${BACKUP_DIR:-/home/ubuntu/backups/operator}
KEEP=${KEEP:-14}

cd "$BOTS_DIR"
paths=()
for candidate in \
    operator/.env operator/config.json operator/cache operator/logs \
    operator/.local-lavalink/application.yml \
    operator-tg/.env; do
    [[ -e $candidate ]] && paths+=("$candidate")
done
((${#paths[@]})) || { echo "brak plikow do kopii w $BOTS_DIR" >&2; exit 1; }

umask 077
mkdir -p "$BACKUP_DIR"
archive="$BACKUP_DIR/operator-$(date +%Y%m%d-%H%M%S).tar.zst"
tar --zstd -cf "$archive.tmp" "${paths[@]}"
mv "$archive.tmp" "$archive"
echo "kopia: $archive ($(du -h "$archive" | cut -f1))"

if [[ -n ${BACKUP_COPY_CMD:-} ]]; then
    eval "${BACKUP_COPY_CMD//\{\}/\"$archive\"}"
fi
find "$BACKUP_DIR" -maxdepth 1 -name 'operator-*.tar.zst' -mtime +"$KEEP" -delete
