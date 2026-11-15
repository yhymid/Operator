#!/usr/bin/env bash
# Prints the release title and writes the release body for a tag, both taken from CHANGELOG.md.
#
#   release-notes.sh discord-v1.2 notes.md   ->  prints "1.2 - Name Update", writes notes.md
#
# CHANGELOG.md has one section per release, newest first:
#   ## 1.2 - Name Update
#   ### Added
#   - ...
# The release is refused when the version has no section or the section is empty, so a tag can
# never publish with an auto-generated commit list.
set -euo pipefail

tag=${1:?usage: release-notes.sh <tag> <body-file>}
body=${2:?usage: release-notes.sh <tag> <body-file>}
changelog=${CHANGELOG:-CHANGELOG.md}
version=${tag#*-v}

[[ $version =~ ^[0-9] ]] || { echo "the tag $tag has no version number" >&2; exit 1; }
[[ -f $changelog ]] || { echo "missing $changelog" >&2; exit 1; }
title=$(awk -v v="$version" '
    /^## / { h = substr($0, 4); split(h, w, " "); if (w[1] == v) { print h; exit } }
' "$changelog")
[[ -n $title ]] || { echo "$changelog has no \"## $version ...\" section for $tag" >&2; exit 1; }

awk -v v="$version" '
    /^## / { split(substr($0, 4), w, " "); inside = (w[1] == v); next }
    inside { print }
' "$changelog" | sed -e '/./,$!d' >"$body"
grep -q '[^[:space:]]' "$body" || { echo "the $version section of $changelog is empty" >&2; exit 1; }
echo "$title"
