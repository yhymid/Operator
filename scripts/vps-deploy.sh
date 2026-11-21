#!/usr/bin/env bash
# Lives in ~/operator-src/deploy.sh on the server.
# Usage: TARGETS="discord telegram" bash ~/operator-src/deploy.sh   (after vps-build.sh)
# FORCE=1 restarts the Discord bot even while music plays (never during a /ytdlp child process).
# Backs up to ~/backups, swaps binaries (old kept as *.prev) only when idle, restarts, verifies.
TARGETS="${TARGETS:-discord telegram}"
set -euo pipefail
R="/home/ubuntu/Main Files/Bots"
SRC=~/operator-src
YML="$R/operator/.local-lavalink/application.yml"
PW=$(sed -n 's/^[[:space:]]*password:[[:space:]]*"\{0,1\}\([^"]*\)"\{0,1\}[[:space:]]*$/\1/p' "$YML" | head -n1)
api() { printf 'header = "Authorization: %s"\n' "$PW" | curl -fsS --max-time 15 -K - "http://127.0.0.1:2333$1"; }

for bot in discord telegram; do
    test -x "$SRC/$bot/target/release/operator-$bot"
    echo "built: $(git -C "$SRC/$bot" log -1 --format="$bot %h %s")"
done

players=$(api /v4/stats | sed -n 's/.*"players":[[:space:]]*\([0-9]*\).*/\1/p')
d=$(wc -l < /sys/fs/cgroup/system.slice/operator.service/cgroup.procs)
t=$(wc -l < /sys/fs/cgroup/system.slice/operator-tg.service/cgroup.procs)
echo "idle check: lavalink players=$players, discord procs=$d, telegram procs=$t"
TARGETS=${TARGETS:-discord telegram}
for target in $TARGETS; do
    case $target in
        discord) [[ $d == 1 && ( ${FORCE:-0} == 1 || $players == 0 ) ]] || { echo "BUSY (discord) - nothing changed"; exit 3; } ;;
        telegram) [[ $t == 1 ]] || { echo "BUSY (telegram) - nothing changed"; exit 3; } ;;
    esac
done

echo "== backup"
TS=$(date -u +%Y%m%d-%H%M%S)
mkdir -p ~/backups
STAGE=$(mktemp -d)
mkdir -p "$STAGE/systemd"
cp -p /etc/systemd/system/operator.service /etc/systemd/system/operator-tg.service /etc/systemd/system/lavalink.service "$STAGE/systemd/"
cp -rp /etc/systemd/system/operator.service.d "$STAGE/systemd/operator.service.d"
cp -rp /etc/systemd/system/operator-tg.service.d "$STAGE/systemd/operator-tg.service.d"
OUT=~/backups/operator-$TS.tar.gz
( umask 077; tar -czf "$OUT" \
    -C "$R" operator/operator-discord operator/.env operator/config.json operator/cookies.txt \
        operator/cache operator/.local-lavalink/application.yml \
        operator-tg/operator-telegram operator-tg/.env \
    -C "$STAGE" systemd )
rm -rf "$STAGE"
tar -tzf "$OUT" >/dev/null && echo "backup ok: $OUT ($(du -h "$OUT" | cut -f1))"
# Keep only the newest backup; the previous binaries stay next to the bots as *.prev.
ls -1t ~/backups/operator-*.tar.gz | tail -n +2 | xargs -r rm -f

echo "== swap binaries: $TARGETS"
swap() { # dir binary
    install -m 755 "$SRC/$3/target/release/$2" "$R/$1/$2.new"
    cp -p "$R/$1/$2" "$R/$1/$2.prev"
    mv -f "$R/$1/$2.new" "$R/$1/$2"
}
UNITS=""; ENVS=()
for target in $TARGETS; do
    case $target in
        discord) swap operator operator-discord discord; UNITS="$UNITS operator"; ENVS+=("$R/operator/.env") ;;
        telegram) swap operator-tg operator-telegram telegram; UNITS="operator-tg $UNITS"; ENVS+=("$R/operator-tg/.env") ;;
    esac
done

echo "== enable diagnostics in .env"
for f in "${ENVS[@]}"; do
    if grep -q '^OPERATOR_DIAGNOSTICS=' "$f"; then
        sed -i 's/^OPERATOR_DIAGNOSTICS=.*/OPERATOR_DIAGNOSTICS=1/' "$f"
    else
        [[ -z $(tail -c1 "$f") ]] || printf '\n' >> "$f"
        printf '# Diagnostyka 24/7 (diagnostics/events-*.txt, diagnostics/summary-*.md)\nOPERATOR_DIAGNOSTICS=1\n' >> "$f"
    fi
    echo "$f: $(grep -c '^OPERATOR_DIAGNOSTICS=1' "$f") line(s), mode $(stat -c %a "$f")"
done

for u in $UNITS; do
    sudo systemctl restart $u
done
sleep 75
for u in operator-tg operator lavalink; do
    echo "== $u: $(systemctl is-active $u), restarts=$(systemctl show -p NRestarts --value $u), since $(systemctl show -p ActiveEnterTimestamp --value $u)"
done
for d in operator operator-tg; do
    [[ -d "$R/$d/diagnostics" ]] && { echo "== $d/diagnostics"; ls -la "$R/$d/diagnostics"; }
done
true
