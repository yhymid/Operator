#!/usr/bin/env bash
# Posts "unit X failed" with its last log lines to a Discord webhook. Called by
# operator-notify@<unit>.service, which the bot units start through OnFailure=.
# NOTIFY_WEBHOOK_URL comes from /etc/operator/notify.env (chmod 600).
set -euo pipefail
unit=${1:?unit}
[[ -n ${NOTIFY_WEBHOOK_URL:-} ]] || { echo "brak NOTIFY_WEBHOOK_URL" >&2; exit 0; }

# Last lines only, trimmed: enough to see the crash without dumping the whole log to a channel.
tail_text=$(journalctl -u "$unit" -n 8 --no-pager -o cat 2>/dev/null | cut -c1-180 | tail -c 1500)
host=$(hostname)
payload=$(python3 -c 'import json,sys; print(json.dumps({
    "content": f"**{sys.argv[1]}** padl na `{sys.argv[2]}`. systemd probuje go podniesc.\n```\n{sys.argv[3]}\n```",
    "allowed_mentions": {"parse": []}}))' "$unit" "$host" "$tail_text")
curl -fsS --max-time 15 -H 'Content-Type: application/json' -d "$payload" "$NOTIFY_WEBHOOK_URL" >/dev/null
