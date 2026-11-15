#!/usr/bin/env bash
# Copies src/common/ from one branch to the other and commits it there.
#
#   scripts/sync-common.sh            # from the current branch to the other one
#   scripts/sync-common.sh discord    # from discord to telegram
#
# Both bots keep their own copy of src/common/; it must stay identical (CI warns when it is
# not). Change src/common/ in its own commit, then run this instead of hand-editing the other
# branch. A new dependency of src/common/ must be added to the other branch's Cargo.toml by hand.
set -euo pipefail

source_branch=${1:-$(git rev-parse --abbrev-ref HEAD)}
case $source_branch in
    discord) target=telegram ;;
    telegram) target=discord ;;
    *) echo "usage: $0 [discord|telegram]" >&2; exit 2 ;;
esac

if git diff --quiet "$source_branch" "$target" -- src/common/; then
    echo "src/common/ is already identical on $source_branch and $target."
    exit 0
fi

# The target branch is usually checked out in its own folder (git worktree); work there.
target_dir=$(git worktree list --porcelain |
    awk -v ref="refs/heads/$target" '/^worktree /{dir=substr($0, 10)} $0 == "branch " ref {print dir}')
start=""
if [[ -z $target_dir ]]; then
    if ! git diff --quiet || ! git diff --cached --quiet; then
        echo "Uncommitted changes here - commit them first." >&2
        exit 1
    fi
    start=$(git rev-parse --abbrev-ref HEAD)
    git checkout -q "$target"
    target_dir=$(git rev-parse --show-toplevel)
elif ! git -C "$target_dir" diff --quiet || ! git -C "$target_dir" diff --cached --quiet; then
    echo "Uncommitted changes in $target_dir - commit them first." >&2
    exit 1
fi

git -C "$target_dir" restore --source="$source_branch" --staged --worktree -- src/common/
git -C "$target_dir" commit -q -m "common: sync from $source_branch ($(git rev-parse --short "$source_branch"))"
echo "Synced src/common/ from $source_branch to $target:"
git -C "$target_dir" log --oneline -1
[[ -n $start ]] && git checkout -q "$start"
echo "Check the build on $target (cargo clippy / cargo test) before pushing."
