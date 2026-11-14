#!/usr/bin/env bash
# Copies common/ from one main branch to the other and commits it there.
#
#   scripts/sync-common.sh            # from the current branch to the other one
#   scripts/sync-common.sh discord    # from discord to telegram
#
# Both bots keep their own copy of common/; it must stay identical (CI warns when it is not).
# Change common/ in its own commit, then run this instead of hand-editing the other branch.
set -euo pipefail

source_branch=${1:-$(git rev-parse --abbrev-ref HEAD)}
case $source_branch in
    discord) target=telegram ;;
    telegram) target=discord ;;
    *) echo "uzycie: $0 [discord|telegram]" >&2; exit 2 ;;
esac

if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "Masz niezacommitowane zmiany - najpierw je zapisz." >&2
    exit 1
fi
if git diff --quiet "$source_branch" "$target" -- common/; then
    echo "common/ jest juz identyczny na $source_branch i $target."
    exit 0
fi

start=$(git rev-parse --abbrev-ref HEAD)
git checkout -q "$target"
git restore --source="$source_branch" --staged --worktree -- common/
# Cargo.lock may need the new/removed dependencies of common/.
cargo metadata --format-version 1 >/dev/null
git add common/ Cargo.lock
git commit -q -m "common: sync from $source_branch ($(git rev-parse --short "$source_branch"))"
echo "Zsynchronizowano common/ z $source_branch na $target:"
git log --oneline -1
git checkout -q "$start"
echo "Sprawdz build na $target (cargo clippy/test) przed pushem."
